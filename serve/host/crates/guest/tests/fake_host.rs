//! `kith` end to end against an in-process fake cloud + relay + host: one listener answers the accept call over HTTP and
//! the relay upgrade over WebSocket, and speaks the host's side of the handshake and frames with `kith-wire`.
//! The real-host run (kith-serve + wrangler dev) is a separate, later package.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::result_large_err,
    clippy::format_collect,
    clippy::semicolon_if_nothing_returned
)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use kith_guest::cli::Cli;
use kith_guest::output::Out;
use kith_guest::session::Tuning;
use kith_wire::check::Mode;
use kith_wire::crypto::{
    Labels, Opener, Sealer, b64, channel_keys, computer_id, computer_id_raw, hs1_input, random, request_sig_input,
    sha256, sign, transcript_hash, unb64_n, verify, x25519, x25519_pub,
};
use kith_wire::rpc::SCHEMA;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use x25519_dalek::StaticSecret;

const TOKEN: &str = "tok_secret_value";
const INVITE: &str = "bi_test";

/// What the fake host is told to do.
struct Script {
    /// The state `proveBotInvite` leaves the bot in.
    bot_state: &'static str,
    /// Presence `online:false` on connect.
    offline: bool,
    /// What the cloud answers to the accept (status, body); `None` = success.
    accept_override: Option<(u16, Value)>,
    /// `err` to answer every call with (status, code).
    fail_calls_with: Option<(u16, &'static str)>,
    /// Close the socket right after the first partial event of the first connection.
    drop_after_first_event: bool,
    /// Sign the welcome with this key instead of the host key.
    impostor: bool,
    /// Refuse the relay upgrade with 403, as the cloud does once a bot's device is removed.
    refuse_upgrade: bool,
    // ---- what happened ----
    accepts: Vec<Value>,
    calls: Vec<(String, Value)>,
    connections: usize,
    upgrade_headers: Vec<String>,
    final_sent: bool,
}

struct Fake {
    host_key: SigningKey,
    script: Mutex<Script>,
}

struct Server {
    url: String,
    fake: Arc<Fake>,
}

impl Server {
    async fn start(configure: impl FnOnce(&mut Script)) -> Self {
        let mut script = Script {
            bot_state: "active",
            offline: false,
            accept_override: None,
            fail_calls_with: None,
            drop_after_first_event: false,
            impostor: false,
            refuse_upgrade: false,
            accepts: vec![],
            calls: vec![],
            connections: 0,
            upgrade_headers: vec![],
            final_sent: false,
        };
        configure(&mut script);
        let fake = Arc::new(Fake { host_key: SigningKey::from_bytes(&[42; 32]), script: Mutex::new(script) });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shared = fake.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                tokio::spawn(handle(stream, shared.clone()));
            }
        });
        Self { url, fake }
    }

    fn link(&self) -> String {
        let hk = self.fake.host_key.verifying_key().to_bytes();
        format!("{}/b/{}/{INVITE}#t={TOKEN}&k={}", self.url, computer_id(&hk), b64(&hk))
    }

    fn with<R>(&self, f: impl FnOnce(&mut Script) -> R) -> R {
        f(&mut self.fake.script.lock().unwrap())
    }
}

/// Re-checks a `Kith-Sig` the way the cloud does, from the header alone.
fn signature_verifies(header: &str, method: &str, host: &str, path_and_query: &str, body: &[u8]) -> bool {
    let field = |name: &str| header.split(',').find_map(|p| p.strip_prefix(&format!("{name}=")));
    let (Some(kid), Some(ts), Some(nonce), Some(sig)) = (field("kid"), field("ts"), field("nonce"), field("sig"))
    else {
        return false;
    };
    let (Ok(kid), Ok(sig)) = (unb64_n::<32>(kid), unb64_n::<64>(sig)) else { return false };
    let input = request_sig_input(Labels::Kith, method, host, path_and_query, ts.parse().unwrap(), nonce, body);
    verify(&kid, input.as_bytes(), &sig).is_ok()
}

async fn handle(stream: TcpStream, fake: Arc<Fake>) {
    let mut first = [0u8; 4];
    let n = stream.peek(&mut first).await.unwrap_or(0);
    if n >= 4 && &first == b"POST" {
        http(stream, &fake).await;
    } else {
        ws(stream, &fake).await;
    }
}

async fn http(mut stream: TcpStream, fake: &Fake) {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, len) = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        data.extend_from_slice(&chunk[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..i]).to_lowercase();
            let len =
                head.lines().find_map(|l| l.strip_prefix("content-length:")).map_or(0, |v| v.trim().parse().unwrap());
            break (i + 4, len);
        }
    };
    while data.len() < head_end + len {
        let n = stream.read(&mut chunk).await.unwrap();
        data.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let body = &data[head_end..head_end + len];
    let header = |name: &str| {
        head.lines().find_map(|l| {
            l.to_lowercase().starts_with(&format!("{name}:")).then(|| l[name.len() + 1..].trim().to_string())
        })
    };
    let path = head.split_whitespace().nth(1).unwrap().to_string();
    let signed =
        signature_verifies(&header("kith-sig").unwrap_or_default(), "POST", &header("host").unwrap(), &path, body);
    let json: Value = serde_json::from_slice(body).unwrap();
    let (status, answer) = {
        let mut s = fake.script.lock().unwrap();
        s.accepts.push(json!({"path": path, "body": json, "signed": signed}));
        match (&s.accept_override, signed) {
            (_, false) => (401, json!({"error": {"code": "badSignature", "message": "bad signature"}})),
            (Some((status, body)), _) => (*status, body.clone()),
            (None, true) => {
                let hk = fake.host_key.verifying_key().to_bytes();
                (200, json!({"computerId": computer_id(&hk), "botDeviceId": "bd_test"}))
            }
        }
    };
    let text = answer.to_string();
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    let _ = stream.shutdown().await;
}

/// The fake host's answer, held to the host contract as the real host's are (producer check): what `kith` is tested
/// against is what a host may send.
fn answer(fake: &Fake, method: &str, body: &Value) -> Result<Value, (u16, &'static str, &'static str)> {
    let answered = answer_unchecked(fake, method, body);
    if let Ok(ok) = &answered {
        let m = SCHEMA.method(method).unwrap_or_else(|| panic!("{method} isn't in the host contract"));
        assert!(m.who.contains(&"bot"), "{method} isn't a bot's");
        assert_eq!(SCHEMA.check(m.result, ok, Mode::Producer), Vec::<String>::new(), "{method}: {ok:#}");
    }
    if let Err((_, code, _)) = &answered
        && !code.is_empty()
    {
        assert!(SCHEMA.enum_schema("BotErrorCode").unwrap().values.contains(code), "{code}");
    }
    answered
}

fn answer_unchecked(fake: &Fake, method: &str, body: &Value) -> Result<Value, (u16, &'static str, &'static str)> {
    let mut s = fake.script.lock().unwrap();
    s.calls.push((method.to_string(), body.clone()));
    if let Some((status, code)) = s.fail_calls_with {
        return Err((status, code, ""));
    }
    let status = |state: &str| {
        json!({"botId": "bg_1", "name": "Rex", "state": state, "owner": {"name": "Sam"},
            "agents": [{"agent": "house", "name": "House", "role": "member"}],
            "limits": {"ratePerHour": 60, "concurrent": 2}, "version": "0.1.0", "minClient": "0.1.0"})
    };
    match method {
        "proveBotInvite" => {
            let ok = body["token"] == TOKEN && body["inviteId"] == INVITE;
            let first = json!({"state": s.bot_state, "botId": "bg_1", "owner": "Sam", "agents": ["house"]});
            if ok { Ok(first) } else { Err((403, "", "wrong token")) }
        }
        "botStatus" => Ok(status(s.bot_state)),
        "botAsk" => {
            if s.bot_state != "active" {
                return Err((403, "pending", ""));
            }
            Ok(json!({"conversation": "cv_1", "entryId": "u1", "turn": 1, "seq": 10, "state": "running", "queued": 0}))
        }
        "botThread" => {
            let entries = if s.final_sent {
                json!([{"id": "u1", "seq": 10, "turn": 1, "at": 1, "conversation": "cv_1", "kind": "user", "text": "q"},
                       {"id": "a1", "seq": 11, "turn": 1, "at": 2, "conversation": "cv_1", "kind": "agent", "text": "Hello there", "final": true}])
            } else {
                json!([])
            };
            Ok(
                json!({"conversation": "cv_1", "entries": entries, "done": s.final_sent, "turn": 1, "state": if s.final_sent { "idle" } else { "running" }}),
            )
        }
        "botConversations" => Ok(
            json!({"conversations": [{"conversation": "cv_1", "agent": "house", "ref": null, "originLabel": "from Grok · Sam's Rex", "lastAt": 1_700_000_000_000_i64, "state": "idle"}]}),
        ),
        "botLeave" | "botStop" => Ok(json!({})),
        _ => Err((404, "", "no such method")),
    }
}

async fn ws(stream: TcpStream, fake: &Fake) {
    let mut seen = String::new();
    let refuse = fake.script.lock().unwrap().refuse_upgrade;
    let accepted = tokio_tungstenite::accept_hdr_async(
        stream,
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
            if refuse {
                return Err(tokio_tungstenite::tungstenite::http::Response::builder().status(403).body(None).unwrap());
            }
            let host = req.headers().get("host").unwrap().to_str().unwrap().to_string();
            let sig = req.headers().get("kith-sig").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let pq = req.uri().path_and_query().unwrap().to_string();
            seen = format!("{}|{}", signature_verifies(&sig, "GET", &host, &pq, b""), pq);
            Ok(resp)
        },
    )
    .await;
    let Ok(socket) = accepted else { return };
    let (offline, connection, impostor) = {
        let mut s = fake.script.lock().unwrap();
        s.connections += 1;
        s.upgrade_headers.push(seen);
        (s.offline, s.connections, s.impostor)
    };
    let (mut tx, mut rx) = socket.split();
    let send = |v: Value| Message::text(v.to_string());
    if offline {
        tx.send(send(json!({"t": "presence", "online": false}))).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        return;
    }
    tx.send(send(json!({"t": "presence", "online": true}))).await.unwrap();

    let host_pub = fake.host_key.verifying_key().to_bytes();
    let cid = computer_id_raw(&host_pub);
    let (mut sealer, mut opener) = (None::<Sealer>, None::<Opener>);
    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(text) = msg else { continue };
        let wire: Value = serde_json::from_str(text.as_str()).unwrap();
        match wire["t"].as_str().unwrap() {
            "hello" => {
                let dk: [u8; 32] = unb64_n(wire["dk"].as_str().unwrap()).unwrap();
                let ek: [u8; 32] = unb64_n(wire["ek"].as_str().unwrap()).unwrap();
                let n: [u8; 32] = unb64_n(wire["n"].as_str().unwrap()).unwrap();
                let sig: [u8; 64] = unb64_n(wire["sig"].as_str().unwrap()).unwrap();
                verify(&dk, &hs1_input(Labels::Kith, &cid, &dk, &ek, &n), &sig).expect("hello signature");
                let eph = StaticSecret::from(random::<32>());
                let ek_h = x25519_pub(&eph);
                let th = transcript_hash(Labels::Kith, &cid, &dk, &ek, &n, &ek_h);
                let signer = if impostor { SigningKey::from_bytes(&[7; 32]) } else { fake.host_key.clone() };
                let keys = channel_keys(Labels::Kith, &x25519(&eph, &ek).unwrap(), &th);
                sealer = Some(Sealer::new(Labels::Kith, keys.h2d));
                opener = Some(Opener::new(Labels::Kith, keys.d2h, 1 << 20));
                tx.send(send(json!({"t": "welcome", "ek": b64(&ek_h), "sig": b64(&sign(&signer, &th))})))
                    .await
                    .unwrap();
            }
            "f" => {
                let Some(bytes) = opener.as_mut().unwrap().open(&wire).unwrap() else { continue };
                let inner: Value = serde_json::from_slice(&bytes).unwrap();
                let id = inner["id"].as_u64().unwrap();
                let mut out = vec![];
                if let Some(method) = inner["m"].as_str() {
                    out.push(match answer(fake, method, &inner["b"]) {
                        Ok(ok) => json!({"id": id, "ok": ok}),
                        Err((status, code, message)) => {
                            let mut err = json!({"status": status, "message": message});
                            if !code.is_empty() {
                                err["code"] = json!(code);
                            }
                            json!({"id": id, "err": err})
                        }
                    });
                } else if inner["sub"] == "events" {
                    // The turn finishes at the computer as soon as it is watched. On the scripted connection the client
                    // is cut off after the first partial answer and has to catch up from the thread.
                    let cut = {
                        let mut s = fake.script.lock().unwrap();
                        s.calls.push(("subscribe:events".into(), inner["b"].clone()));
                        s.final_sent = true;
                        connection == 1 && s.drop_after_first_event
                    };
                    let entry = |seq: u64, text: &str, fin: bool| {
                        json!({"id": id, "ev": {"type": "entry", "conversation": "cv_1",
                            "entry": {"id": "a1", "seq": seq, "turn": 1, "at": 2, "conversation": "cv_1", "kind": "agent", "text": text, "final": fin}}})
                    };
                    let mut events = vec![entry(11, "Hello", false)];
                    if !cut {
                        events.push(entry(11, "Hello there", true));
                    }
                    for event in events {
                        for frame in sealer.as_mut().unwrap().seal(event.to_string().as_bytes()).unwrap() {
                            tx.send(send(frame)).await.unwrap();
                        }
                    }
                    if cut {
                        return;
                    }
                    continue;
                }
                for m in out {
                    for frame in sealer.as_mut().unwrap().seal(m.to_string().as_bytes()).unwrap() {
                        tx.send(send(frame)).await.unwrap();
                    }
                }
            }
            _ => {}
        }
    }
}

fn tuning() -> Tuning {
    Tuning { dial_timeout: Duration::from_secs(3), backoff: Duration::from_millis(10), poll: Duration::from_millis(50) }
}

/// Runs `kith <args>` in-process against the fake. Returns (exit code, stdout, stderr).
async fn kith(server: &Server, home: &Path, args: &[&str]) -> (u8, String, String) {
    let mut argv = vec!["kith", "--home", home.to_str().unwrap(), "--cloud", &server.url];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(argv).unwrap();
    let (out, captured) = Out::capture(cli.json);
    let code = kith_guest::execute(cli, &out, tuning()).await;
    (code, captured.out(), captured.err())
}

fn guest_json(home: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(home.join("guest.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn join_then_ask_prints_the_reply_once() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    let (code, out, err) =
        kith(&server, home.path(), &["join", &server.link(), "--name", "Rex", "--provider", "grok", "--wait", "5"])
            .await;
    assert_eq!((code, out.as_str()), (0, "Joined Sam's computer as \"Rex\". Agents: house.\n"), "{err}");

    // The cloud was shown the hash of the token, never the token, in a signature it can check.
    let accepts = server.with(|s| s.accepts.clone());
    assert_eq!(accepts.len(), 1);
    assert_eq!(accepts[0]["path"], format!("/v1/bot-invites/{INVITE}/accept"));
    assert_eq!(accepts[0]["signed"], true);
    assert_eq!(accepts[0]["body"]["token"], hex(&sha256(&[TOKEN.as_bytes()])));
    assert_eq!(accepts[0]["body"]["provider"], "grok");
    assert!(!accepts[0].to_string().contains(TOKEN));
    // The relay upgrade carried a verifiable Kith-Sig.
    assert!(server.with(|s| s.upgrade_headers[0].starts_with("true|/v1/relay/device/")));

    let g = guest_json(home.path());
    assert_eq!(
        (g["state"].as_str(), g["botId"].as_str(), g["owner"].as_str()),
        (Some("active"), Some("bg_1"), Some("Sam"))
    );
    assert!(g.get("token").is_none(), "the token is deleted once proved");

    let (code, out, err) = kith(&server, home.path(), &["ask", "@house", "what's on the list?", "--wait"]).await;
    assert_eq!((code, out.as_str()), (0, "Hello there\n"), "{err}");
    assert!(err.contains("conversation: cv_1"), "{err}");
    let ask = server.with(|s| s.calls.iter().find(|c| c.0 == "botAsk").cloned()).unwrap();
    assert_eq!((ask.1["agent"].as_str(), ask.1["text"].as_str()), (Some("house"), Some("what's on the list?")));
    assert!(ask.1["clientNonce"].as_str().is_some_and(|n| !n.is_empty()));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
async fn stream_and_json_modes() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);

    let (code, out, err) = kith(&server, home.path(), &["ask", "house", "hi", "--stream"]).await;
    assert_eq!((code, out.as_str()), (0, "Hello there\n"), "{err}");

    let (code, out, _) = kith(&server, home.path(), &["--json", "ask", "house", "hi"]).await;
    assert_eq!(code, 0);
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines[0]["type"], "conversation");
    assert_eq!(lines[0]["conversation"], "cv_1");
    let done = lines.last().unwrap();
    assert_eq!(
        (done["type"].as_str(), done["text"].as_str(), done["turn"].as_u64()),
        (Some("done"), Some("Hello there"), Some(1))
    );
}

#[tokio::test]
async fn a_dropped_connection_is_repaired_and_nothing_prints_twice() {
    let server = Server::start(|s| s.drop_after_first_event = true).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    // join used connection 1, which is when the script drops; use a fresh script state for the ask.
    server.with(|s| {
        s.connections = 0;
        s.final_sent = false;
    });
    let (code, out, err) = kith(&server, home.path(), &["ask", "house", "hi", "--wait"]).await;
    assert_eq!((code, out.as_str()), (0, "Hello there\n"), "{err}");
    assert!(server.with(|s| s.connections) >= 2, "it reconnected");
    // The resubscription resumed from the ask's cursor, and the ask was sent exactly once.
    assert_eq!(server.with(|s| s.calls.iter().filter(|c| c.0 == "botAsk").count()), 1);
}

#[tokio::test]
async fn pending_bot_exits_5_and_proceeds_once_allowed() {
    let server = Server::start(|s| s.bot_state = "pending").await;
    let home = tempfile::tempdir().unwrap();
    let (code, out, err) = kith(&server, home.path(), &["join", &server.link(), "--wait", "1"]).await;
    assert_eq!((code, out.as_str()), (5, ""), "{err}");
    assert!(err.contains("Waiting for"), "{err}");
    assert_eq!(guest_json(home.path())["state"], "pending");

    // Asking while pending says so (without sending the question).
    let (code, _, _) = kith(&server, home.path(), &["ask", "house", "hi", "--wait"]).await;
    assert_eq!(code, 5);
    assert!(server.with(|s| s.calls.iter().all(|c| c.0 != "botAsk")));

    // The owner allows it; the next command notices.
    server.with(|s| s.bot_state = "active");
    let (code, out, err) = kith(&server, home.path(), &["ask", "house", "hi", "--wait"]).await;
    assert_eq!((code, out.as_str()), (0, "Hello there\n"), "{err}");
    assert_eq!(guest_json(home.path())["state"], "active");
}

#[tokio::test]
async fn the_same_link_twice_is_not_an_error_and_another_needs_rekey() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    let (code, out, err) = kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await;
    assert_eq!((code, out.as_str()), (0, "Joined Sam's computer as \"Rex\". Agents: house.\n"), "{err}");
    assert_eq!(server.with(|s| s.accepts.len()), 1, "an active bot isn't accepted again");

    let other = server.link().replace(INVITE, "bi_other");
    let (code, _, err) = kith(&server, home.path(), &["join", &other, "--wait", "5"]).await;
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("Already joined"), "{err}");
}

#[tokio::test]
async fn the_computer_being_offline_fails_fast_with_10() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    server.with(|s| s.offline = true);
    let started = std::time::Instant::now();
    let (code, out, err) = kith(&server, home.path(), &["ask", "house", "hi"]).await;
    assert_eq!((code, out.as_str()), (10, ""), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2), "fail fast");
    let (code, out, _) = kith(&server, home.path(), &["status", "--json"]).await;
    assert_eq!(code, 0, "status reports an offline computer");
    let v: Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert_eq!(
        (v["host"].as_str(), v["state"].as_str(), v["name"].as_str().is_some()),
        (Some("offline"), Some("active"), true)
    );
}

#[tokio::test]
async fn a_revoked_bot_hears_exit_6_and_remembers_it() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    // The cloud refuses a removed bot's device at the relay. (The host has no `revoked` error code: a bot it no longer
    // knows is refused at the channel, and a call on a channel opened before answers 403.)
    server.with(|s| s.refuse_upgrade = true);
    let (code, _, err) = kith(&server, home.path(), &["ask", "house", "hi"]).await;
    assert_eq!(code, 6);
    assert!(err.contains("Your access to this computer was removed"), "{err}");
    assert_eq!(guest_json(home.path())["state"], "revoked");
    // The next command doesn't even connect.
    let before = server.with(|s| s.connections);
    assert_eq!(kith(&server, home.path(), &["ask", "house", "hi"]).await.0, 6);
    assert_eq!(server.with(|s| s.connections), before);
}

#[tokio::test]
async fn an_agent_that_is_not_granted_is_exit_6_and_a_busy_one_exit_7() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    server.with(|s| s.fail_calls_with = Some((403, "notGranted")));
    assert_eq!(kith(&server, home.path(), &["ask", "notes", "hi"]).await.0, 6);
    server.with(|s| s.fail_calls_with = Some((409, "heldByOtherWriter")));
    assert_eq!(kith(&server, home.path(), &["ask", "house", "hi", "--thread", "cv_1"]).await.0, 7);
    assert_eq!(guest_json(home.path())["state"], "active", "neither ends the bot");
}

#[tokio::test]
async fn a_welcome_signed_by_another_key_is_exit_12() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    server.with(|s| s.impostor = true);
    let (code, _, err) = kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await;
    assert_eq!(code, 12, "{err}");
}

#[tokio::test]
async fn invite_problems_are_exit_4_and_not_joined_is_exit_3() {
    let server = Server::start(|s| {
        s.accept_override =
            Some((410, json!({"error": {"code": "inviteExpired", "message": "This invite has expired"}})))
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    let (code, _, err) = kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await;
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("expired"), "{err}");

    let empty = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, empty.path(), &["ask", "house", "hi"]).await.0, 3);
    assert_eq!(kith(&server, empty.path(), &["status"]).await.0, 3);
    // A damaged link never reaches the network.
    assert_eq!(kith(&server, empty.path(), &["join", "https://x/b/y/z#t=1&k=2"]).await.0, 4);
}

#[tokio::test]
async fn credentials_from_the_environment_stand_in_for_the_files() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    let (code, line, err) =
        kith(&server, home.path(), &["join", &server.link(), "--wait", "5", "--print-credentials"]).await;
    assert_eq!(code, 0, "{err}");
    let creds = line.trim().to_string();
    assert!(!creds.contains(TOKEN) && !creds.is_empty());

    // A new VM: no files, only the secret.
    let fresh = tempfile::tempdir().unwrap();
    let (code, out, err) =
        kith(&server, fresh.path(), &["--credentials", &creds, "ask", "house", "hi", "--wait"]).await;
    assert_eq!((code, out.as_str()), (0, "Hello there\n"), "{err}");
    assert!(!fresh.path().join("guest.json").exists(), "never written back");
}

#[tokio::test]
async fn leave_removes_the_key_and_threads_and_agents_list() {
    let server = Server::start(|_| {}).await;
    let home = tempfile::tempdir().unwrap();
    assert_eq!(kith(&server, home.path(), &["join", &server.link(), "--wait", "5"]).await.0, 0);
    let (code, out, _) = kith(&server, home.path(), &["agents"]).await;
    assert_eq!((code, out.as_str()), (0, "@house\tHouse\n"));
    let (code, out, _) = kith(&server, home.path(), &["threads"]).await;
    assert_eq!((code, out.as_str()), (0, "cv_1\t@house\tidle\tfrom Grok · Sam's Rex\n"));
    let (code, _, err) = kith(&server, home.path(), &["leave"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(!home.path().join("device.key").exists() && !home.path().join("guest.json").exists());
    assert_eq!(kith(&server, home.path(), &["ask", "house", "hi"]).await.0, 3);
}
