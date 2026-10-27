//! The Slack adapter against a fake Slack (axum: Web API, file URLs and a Socket Mode websocket), the real core and
//! the stub hq (`tests/fixtures/channels_stub.py`). Ported from the retired Python bridge's checks.

#![cfg(unix)]

use super::api::Api;
use super::{Slack, text};
use crate::LockExt;
use crate::agent::gate::{Gate, Settings as GateSettings, memory};
use crate::channels::adapter::{Adapter, Incoming, Reaction};
use crate::channels::hq::Paths;
use crate::channels::judge::Judge;
use crate::channels::manager::{Backoff, supervise};
use crate::channels::settings::Settings;
use crate::channels::{Core, core};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const STUB: &str = include_str!("../../../tests/fixtures/channels_stub.py");
const BOT_TOKEN: &str = "xoxb-TESTBOTTOKENVALUE";
const APP_TOKEN: &str = "xapp-TESTAPPTOKENVALUE";
const TICKET: &str = "SOCKETTICKETVALUE";
const ME: &str = "U_HQ";
const KAI: &str = "U_KAI";
const GUEST: &str = "U_GUEST";
const DM: &str = "D100";
const ROOM: &str = "C200";

fn stub_script() -> &'static std::path::Path {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kith-slack-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("channels_stub.py");
        std::fs::write(&path, STUB).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    })
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---- the fake Slack ----

#[derive(Default)]
struct SlackState {
    /// Envelopes waiting for a socket to take them.
    pending: VecDeque<Value>,
    /// The envelope ids the app acknowledged, in order.
    acks: Vec<String>,
    calls: Vec<(String, Value)>,
    next: u64,
    /// Sockets opened (apps.connections.open) and websockets accepted.
    opened: usize,
    connected: usize,
    /// The websocket stops reading and writing.
    hang: bool,
    /// How many chat.postMessage still answer 429.
    rate_limit: usize,
    /// Markdown blocks are refused (an older workspace).
    reject_blocks: bool,
    /// Plain-text posts are refused too.
    reject_plain: bool,
    files: HashMap<String, Vec<u8>>,
    uploads: HashMap<String, Vec<u8>>,
    /// Bearer tokens the file downloads came with.
    download_auth: Vec<String>,
    envelope: u64,
    /// How long a file download takes.
    file_delay: Duration,
}

struct Fake {
    url: String,
    state: Mutex<SlackState>,
}

type Shared = State<Arc<Fake>>;

fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned()
}

fn decode(text: &str) -> String {
    let bytes = text.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push(u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// A request body, JSON or form-encoded, as one JSON object.
fn params(headers: &HeaderMap, body: &[u8]) -> Value {
    let form = headers.get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.contains("urlencoded"));
    if !form {
        return serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    }
    let mut out = json!({});
    for pair in String::from_utf8_lossy(body).split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out[decode(k)] = json!(decode(v));
    }
    out
}

async fn api_call(
    State(fake): Shared,
    Path(method): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let token = bearer(&headers);
    let wanted = if method == "apps.connections.open" { APP_TOKEN } else { BOT_TOKEN };
    let ok = |body: Value| (StatusCode::OK, HeaderMap::new(), Json(body));
    if token != wanted {
        return ok(json!({"ok": false, "error": "invalid_auth"}));
    }
    let body = params(&headers, &body);
    let mut st = fake.state.locked();
    st.calls.push((method.clone(), body.clone()));
    match method.as_str() {
        "auth.test" => ok(json!({"ok": true, "user_id": ME, "team": "Test Team"})),
        "apps.connections.open" => {
            st.opened += 1;
            ok(json!({"ok": true, "url": format!("{}/ws?ticket={TICKET}", fake.url.replace("http://", "ws://"))}))
        }
        "chat.postMessage" => {
            let reject = |st: &mut SlackState, why: &str| {
                if let Some((_, call)) = st.calls.last_mut() {
                    call["rejected"] = json!(why);
                }
            };
            if st.rate_limit > 0 {
                st.rate_limit -= 1;
                reject(&mut st, "429");
                let mut h = HeaderMap::new();
                h.insert("retry-after", "1".parse().unwrap());
                return (StatusCode::TOO_MANY_REQUESTS, h, Json(json!({"ok": false, "error": "ratelimited"})));
            }
            if st.reject_plain && body["blocks"].is_null() {
                reject(&mut st, "channel_not_found");
                return ok(json!({"ok": false, "error": "channel_not_found"}));
            }
            if st.reject_blocks && body["blocks"][0]["type"] == "markdown" {
                reject(&mut st, "invalid_blocks");
                return ok(json!({"ok": false, "error": "invalid_blocks"}));
            }
            st.next += 1;
            let ts = format!("1760000000.{:06}", st.next);
            if let Some((_, call)) = st.calls.last_mut() {
                call["sent_ts"] = json!(ts); // so a test can press the button of the line it was sent as
            }
            ok(json!({"ok": true, "channel": body["channel"], "ts": ts}))
        }
        "files.getUploadURLExternal" => {
            st.next += 1;
            let id = format!("F{}", st.next);
            ok(json!({"ok": true, "upload_url": format!("{}/upload/{id}", fake.url), "file_id": id}))
        }
        _ => ok(json!({"ok": true})),
    }
}

async fn upload(State(fake): Shared, Path(id): Path<String>, body: Bytes) -> StatusCode {
    fake.state.locked().uploads.insert(id, body.to_vec());
    StatusCode::OK
}

async fn download(
    State(fake): Shared,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, [(&'static str, &'static str); 1], Vec<u8>) {
    let delay = {
        let mut st = fake.state.locked();
        st.download_auth.push(bearer(&headers));
        st.file_delay
    };
    tokio::time::sleep(delay).await;
    let found = fake.state.locked().files.get(&name).cloned();
    // Slack answers a link it won't serve with its sign-in page, status 200.
    let kind = if name.starts_with("HTML") { "text/html; charset=utf-8" } else { "application/octet-stream" };
    found.map_or((StatusCode::NOT_FOUND, [("content-type", kind)], Vec::new()), |b| {
        (StatusCode::OK, [("content-type", kind)], b)
    })
}

async fn socket(State(fake): Shared, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| serve_socket(fake, socket))
}

async fn serve_socket(fake: Arc<Fake>, mut socket: WebSocket) {
    fake.state.locked().connected += 1;
    let _ = socket.send(Message::Text(json!({"type": "hello"}).to_string().into())).await;
    loop {
        if fake.state.locked().hang {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let ack: Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                    if let Some(id) = ack["envelope_id"].as_str() {
                        fake.state.locked().acks.push(id.to_owned());
                    }
                }
                Some(Ok(_)) => {}
                _ => return,
            },
            () = tokio::time::sleep(Duration::from_millis(15)) => {
                let next = fake.state.locked().pending.pop_front();
                if next.as_ref().is_some_and(|e| e["type"] == "close-without-saying-so") {
                    return;
                }
                if let Some(envelope) = next
                    && socket.send(Message::Text(envelope.to_string().into())).await.is_err()
                {
                    return;
                }
            }
        }
    }
}

impl Fake {
    async fn start() -> Arc<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Arc::new(Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            state: Mutex::new(SlackState { next: 1000, ..SlackState::default() }),
        });
        let app = Router::new()
            .route("/api/{method}", post(api_call))
            .route("/upload/{id}", any(upload))
            .route("/files/{name}", get(download))
            .route("/ws", get(socket))
            .with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        fake
    }

    fn envelope(&self, kind: &str, payload: &Value) -> String {
        let mut st = self.state.locked();
        st.envelope += 1;
        let id = format!("env-{}", st.envelope);
        st.pending.push_back(json!({"envelope_id": id, "type": kind, "payload": payload,
                                    "accepts_response_payload": false}));
        id
    }

    /// An event as Slack delivers it -> (envelope id).
    fn deliver(&self, event: &Value, event_id: &str) -> String {
        self.envelope("events_api", &json!({"type": "event_callback", "event_id": event_id, "event": event}))
    }

    /// A user's message, sent `ago` seconds ago -> its ts.
    fn say(&self, user: &str, channel: &str, text: &str, ago: u64, extra: &Value) -> String {
        let ts = {
            let mut st = self.state.locked();
            st.next += 1;
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - ago;
            format!("{secs}.{:06}", st.next)
        };
        let kind = if channel.starts_with('D') { "im" } else { "channel" };
        let mut event = json!({"type": "message", "user": user, "channel": channel, "channel_type": kind,
                               "text": text, "ts": ts});
        for (k, v) in extra.as_object().into_iter().flatten() {
            event[k] = v.clone();
        }
        self.deliver(&event, &format!("Ev{ts}"));
        ts
    }

    fn press(&self, user: &str, channel: &str, ts: &str, action_id: &str, value: &str) {
        self.envelope(
            "interactive",
            &json!({"type": "block_actions", "user": {"id": user}, "channel": {"id": channel},
                    "container": {"message_ts": ts},
                    "actions": [{"action_id": action_id, "value": value}]}),
        );
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.state.locked().calls.iter().filter(|(m, _)| m == method).map(|(_, b)| b.clone()).collect()
    }

    /// The chat.postMessage calls Slack took.
    fn posted(&self) -> Vec<Value> {
        self.calls("chat.postMessage").into_iter().filter(|c| c["rejected"].is_null()).collect()
    }

    /// The text of every answer (not the working line).
    fn texts(&self) -> Vec<String> {
        self.posted()
            .iter()
            .filter_map(|c| c["text"].as_str())
            .filter(|t| *t != "working…")
            .map(str::to_owned)
            .collect()
    }

    fn answers(&self, token: &str) -> usize {
        self.texts().iter().filter(|t| t.contains("echo:") && t.contains(token)).count()
    }

    fn opened(&self) -> usize {
        self.state.locked().opened
    }

    fn acked(&self, id: &str) -> bool {
        self.state.locked().acks.iter().any(|a| a == id)
    }
}

// ---- the rig ----

struct Rig {
    dir: PathBuf,
    paths: Paths,
    core: Arc<Core>,
    fake: Arc<Fake>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn new_core(paths: &Paths) -> Arc<Core> {
    let settings = Settings {
        debounce: Duration::from_millis(100),
        hold: Duration::from_millis(400),
        poll: Duration::from_millis(40),
        ..Settings::default()
    };
    let gate = Gate::new(GateSettings::unlimited(), Box::new(memory::System));
    Core::new(core::Deps {
        paths: paths.clone(),
        settings,
        gate,
        judge: Judge::fixed(None, paths.clone()),
        backups: Arc::new(crate::agent::backups::Backups::new(paths.data.join("backups.json"))),
    })
}

impl Rig {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-slack-{}", uuid::Uuid::new_v4()));
        let (bin, stub) = (dir.join("bin"), dir.join("stub"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&stub).unwrap();
        for tool in ["kith", "kith-auth"] {
            std::os::unix::fs::symlink(stub_script(), bin.join(tool)).unwrap();
        }
        for (name, card) in [
            ("house", "description: Keeps the house\ndisplay_name: Home\n"),
            ("personal", "description: Personal\n"),
            ("shop", "description: Shopping\n"),
        ] {
            std::fs::create_dir_all(dir.join("agents").join(name)).unwrap();
            std::fs::write(dir.join("agents").join(name).join("agent.md"), card).unwrap();
        }
        let files = [
            (
                "people",
                json!({format!("slack:{KAI}"): {"person": "kai", "owner": true, "was": []},
                       format!("slack:{GUEST}"): {"person": "guest", "owner": false, "was": []}}),
            ),
            ("access", json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": ["shop"]}})),
        ];
        for (name, v) in files {
            std::fs::write(stub.join(format!("{name}.json")), v.to_string()).unwrap();
        }
        let paths = Paths {
            hq_bin: bin.join("kith"),
            auth_bin: bin.join("kith-auth"),
            machine: dir.join("machine"),
            agent_roots: vec![dir.join("agents")],
            data: dir.join("data"),
        };
        let core = new_core(&paths);
        Self { dir, paths, core, fake: Fake::start().await }
    }

    fn slack(&self, core: &Arc<Core>) -> Slack {
        let mut s = Slack::new("slack/hq", BOT_TOKEN, APP_TOKEN, &format!("{}/api", self.fake.url), core.clone());
        s.pace.ping = Duration::from_millis(200);
        s.pace.reconnect = Duration::from_millis(30);
        s
    }

    fn run(&self) -> (Arc<Slack>, JoinHandle<anyhow::Result<()>>) {
        self.run_with(|_| {})
    }

    fn run_with(&self, tune: impl FnOnce(&mut Slack)) -> (Arc<Slack>, JoinHandle<anyhow::Result<()>>) {
        let mut s = self.slack(&self.core);
        tune(&mut s);
        let s = Arc::new(s);
        let task = tokio::spawn(s.clone().start());
        (s, task)
    }

    fn hq_calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("stub/calls.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|c| c["tool"] == "kith").collect()
    }
}

fn dm(rig: &Rig, text: &str) -> String {
    rig.fake.say(KAI, DM, text, 0, &json!({}))
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_gets_one_answer_in_its_thread_and_is_not_late() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let ts = dm(&rig, "@house msg-live");
    until("the answer", || rig.fake.answers("msg-live") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.fake.answers("msg-live"), 1);
    let answer =
        rig.fake.posted().into_iter().find(|c| c["text"].as_str().is_some_and(|t| t.contains("echo:"))).unwrap();
    assert_eq!(answer["thread_ts"], ts);
    assert_eq!(answer["channel"], DM);
    assert_eq!(answer["username"], "Home");
    assert_eq!(answer["blocks"][0]["type"], "markdown");
    assert!(answer["blocks"][0]["text"].as_str().unwrap().starts_with("echo:msg-live"), "{answer}");
    let tasks: Vec<String> = rig.hq_calls().iter().map(|c| c["task"].as_str().unwrap().to_owned()).collect();
    assert!(tasks.iter().all(|t| !t.contains("picked it up late")), "{tasks:?}");
    until("the working line to go", || !rig.fake.calls("chat.delete").is_empty()).await;
    // Nothing is logged for the sockets either: both tokens were used, only on their own calls.
    assert_eq!(rig.fake.calls("auth.test").len(), 1);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn envelopes_are_acknowledged_and_repeats_are_answered_once() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    // Slack retries what it has no answer to; and a mention arrives as message and as app_mention.
    let ts =
        format!("{}.000001", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    let event = json!({"type": "message", "user": KAI, "channel": ROOM, "channel_type": "channel",
                       "text": format!("<@{ME}> @house msg-twice"), "ts": ts});
    let ids = [
        rig.fake.deliver(&event, "EvA"),
        rig.fake.deliver(&event, "EvA"),
        rig.fake.deliver(
            &json!({"type": "app_mention", "user": KAI, "channel": ROOM,
                                 "text": format!("<@{ME}> @house msg-twice"), "ts": ts}),
            "EvB",
        ),
    ];
    until("the answer", || rig.fake.answers("msg-twice") == 1).await;
    until("all acknowledged", || ids.iter().all(|i| rig.fake.acked(i))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.fake.answers("msg-twice"), 1);
    assert_eq!(rig.hq_calls().len(), 1);
    // The app's own mention is taken out of the task.
    assert!(rig.hq_calls()[0]["task"].as_str().unwrap().starts_with("msg-twice"), "{:?}", rig.hq_calls());
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_sent_while_down_are_marked_late() {
    let rig = Rig::new().await;
    rig.fake.say(KAI, DM, "@house msg-down", 600, &json!({}));
    let (_s, task) = rig.run();
    until("the answer", || rig.fake.answers("msg-down") == 1).await;
    assert!(rig.hq_calls()[0]["task"].as_str().unwrap().contains("picked it up late"), "{:?}", rig.hq_calls());
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_continues_the_task_with_the_agents_session() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let root = rig.fake.say(KAI, ROOM, "@house start", 0, &json!({}));
    until("the first answer", || rig.fake.answers("start") == 1).await;
    // A reply in the thread needs no @agent and no mention.
    rig.fake.say(KAI, ROOM, "msg-more", 0, &json!({"thread_ts": root}));
    until("the second answer", || rig.fake.answers("msg-more") == 1).await;
    let calls = rig.hq_calls();
    assert!(calls[0]["resume"].is_null());
    assert_eq!(calls[1]["resume"], "sess-house-1");
    let last =
        rig.fake.posted().into_iter().rfind(|c| c["text"].as_str().is_some_and(|t| t.contains("msg-more"))).unwrap();
    assert_eq!(last["thread_ts"], root);
    // A thread hq didn't open is nobody's business without a mention.
    rig.fake.say(KAI, ROOM, "msg-elsewhere", 0, &json!({"thread_ts": "1760000999.000001"}));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(rig.hq_calls().len(), 2);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_agent_text_and_bang_commands_address_the_core() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    rig.fake.say(KAI, ROOM, "hey @shop msg-word", 0, &json!({}));
    until("the @agent answer", || rig.fake.answers("msg-word") == 1).await;
    assert_eq!(rig.hq_calls()[0]["agent"], "shop");
    // `!whoami` is the core's /whoami, answered by a note in the room (not in a thread, in a DM).
    let ts = dm(&rig, "!whoami");
    until("the note", || rig.fake.texts().iter().any(|t| t.contains("You are kai in kith"))).await;
    let note =
        rig.fake.posted().into_iter().find(|c| c["text"].as_str().is_some_and(|t| t.contains("You are kai"))).unwrap();
    assert!(note["thread_ts"].is_null(), "{note}");
    assert_eq!(note["channel"], DM);
    assert_ne!(ts, "");
    // In a channel the note goes in a thread under the command.
    let ts = rig.fake.say(KAI, ROOM, &format!("<@{ME}> !agents"), 0, &json!({}));
    until("the list", || rig.fake.posted().iter().any(|c| c["thread_ts"] == ts)).await;
    let list = rig.fake.posted().into_iter().find(|c| c["thread_ts"] == ts).unwrap();
    assert!(list["text"].as_str().unwrap().contains("house"), "{list}");
    // Nobody ran: commands are not tasks.
    assert_eq!(rig.hq_calls().len(), 1);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_not_meant_for_the_app_are_ignored() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let f = &rig.fake;
    f.say(KAI, ROOM, "just talking, msg-chatter", 0, &json!({}));
    f.say(KAI, ROOM, "@house msg-bot", 0, &json!({"bot_id": "B1"}));
    f.say(ME, ROOM, "@house msg-self", 0, &json!({}));
    f.say(KAI, ROOM, "@house msg-edit", 0, &json!({"subtype": "message_changed"}));
    f.say(KAI, ROOM, "@house msg-joined", 0, &json!({"subtype": "channel_join"}));
    f.say("U_LOST", ROOM, "email me@house.org msg-email", 0, &json!({}));
    // A message forwarded into the thread is quoted to the agent.
    f.say(
        KAI,
        ROOM,
        &format!("<@{ME}> @house msg-quote"),
        0,
        &json!({"attachments": [{"is_share": true, "text": "what the neighbour said"}]}),
    );
    until("the real one", || f.answers("msg-quote") == 1).await;
    let calls = rig.hq_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0]["task"].as_str().unwrap().starts_with("(Replying to: \"what the neighbour said\")"), "{calls:?}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_who_takes_it_and_pressing_a_button_twice() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let ts = rig.fake.say(KAI, ROOM, &format!("<@{ME}> msg-ask"), 0, &json!({}));
    until("the question", || rig.fake.texts().iter().any(|t| t == "Who should take this?")).await;
    let ask = rig.fake.posted().into_iter().find(|c| c["text"] == "Who should take this?").unwrap();
    assert_eq!(ask["thread_ts"], ts);
    assert_eq!(ask["blocks"][0]["type"], "section");
    let buttons = ask["blocks"][1]["elements"].as_array().unwrap();
    assert_eq!(buttons.len(), 3, "{ask}");
    let first = &buttons[0];
    assert_eq!(first["action_id"], "kith-pick-house");
    let value = first["value"].as_str().unwrap().to_owned();
    let (pid, agent) = value.split_once(':').unwrap();
    assert_eq!(agent, "house");
    // Somebody else's press is refused, privately.
    rig.fake.press(GUEST, ROOM, "1760000001.000001", "kith-pick-house", &value);
    until("the refusal", || rig.fake.calls("chat.postEphemeral").len() == 1).await;
    let refusal = &rig.fake.calls("chat.postEphemeral")[0];
    assert_eq!(
        (refusal["user"].as_str(), refusal["text"].as_str()),
        (Some(GUEST), Some("That question is someone else's."))
    );
    assert_eq!(rig.hq_calls().len(), 0);
    // The asker's press runs the task, once, and the buttons turn into the choice.
    rig.fake.press(KAI, ROOM, "1760000001.000001", "kith-pick-house", &value);
    until("the answer", || rig.fake.answers("msg-ask") == 1).await;
    assert_eq!(rig.hq_calls()[0]["agent"], "house");
    let update = &rig.fake.calls("chat.update")[0];
    assert_eq!((update["text"].as_str(), update["ts"].as_str()), (Some("→ house"), Some("1760000001.000001")));
    // Pressed again: nothing runs.
    rig.fake.press(KAI, ROOM, "1760000001.000001", "kith-pick-house", &format!("{pid}:house"));
    until("the second refusal", || rig.fake.calls("chat.postEphemeral").len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.hq_calls().len(), 1);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_button_stops_the_work() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let ts = dm(&rig, "@house SLOW work");
    until("the working line", || rig.fake.posted().iter().any(|c| c["text"] == "working…")).await;
    let line = rig.fake.posted().into_iter().find(|c| c["text"] == "working…").unwrap();
    assert_eq!(line["thread_ts"], ts);
    assert_eq!(line["username"], "Home");
    let button = &line["blocks"][1]["elements"][0];
    assert_eq!((button["text"]["text"].as_str(), button["action_id"].as_str()), (Some("Stop"), Some("kith-stop")));
    let wid = button["value"].as_str().unwrap().to_owned();
    until("the run", || !rig.hq_calls().is_empty()).await;
    let line_ts = line["sent_ts"].as_str().unwrap().to_owned();
    let line_ts = line_ts.as_str();
    // Another person's press is refused and stops nothing.
    rig.fake.press(GUEST, DM, line_ts, "kith-stop", &wid);
    until("the refusal", || rig.fake.calls("chat.postEphemeral").len() == 1).await;
    assert_eq!(rig.fake.calls("chat.postEphemeral")[0]["text"], "Only the person who started this can stop it.");
    assert_eq!(rig.fake.calls("chat.update").len(), 0);
    // Kai's press stops it: "stopping…", then "stopped" on the working line.
    rig.fake.press(KAI, DM, line_ts, "kith-stop", &wid);
    until("stopped", || rig.fake.calls("chat.update").iter().any(|c| c["text"] == "stopped")).await;
    let edits: Vec<String> =
        rig.fake.calls("chat.update").iter().map(|c| c["text"].as_str().unwrap().to_owned()).collect();
    // The run ending and the press race: "stopping…" may be skipped, but never lands after "stopped".
    assert_eq!(edits.last().map(String::as_str), Some("stopped"), "{edits:?}");
    assert_eq!(edits.iter().filter(|e| *e == "stopped").count(), 1, "{edits:?}");
    // Pressed after it ended: nothing to stop, no complaint.
    rig.fake.press(KAI, DM, line_ts, "hq-stop", &wid); // the old action id still works
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.fake.calls("chat.postEphemeral").len(), 1);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_working_line_changes_and_ends() {
    let rig = Rig::new().await;
    let (s, task) = rig.run();
    until("connected", || rig.fake.opened() == 1).await;
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000001"});
    let handle = s.status(&conv, "house", "w1").await.unwrap();
    assert_eq!((handle["channel"].as_str(), handle["wid"].as_str()), (Some(DM), Some("w1")));
    s.status_text(&handle, "2 queued").await;
    s.status_text(&handle, "restarting…").await;
    let edits = rig.fake.calls("chat.update");
    assert_eq!(edits[0]["blocks"][1]["elements"][0]["value"], "w1", "the Stop button stays: {edits:?}");
    assert_eq!(edits[1]["blocks"].as_array().unwrap().len(), 1, "no Stop button while restarting: {edits:?}");
    s.status_end(&conv, "house", &handle, crate::channels::Ended::Done).await;
    assert_eq!(rig.fake.calls("chat.delete").len(), 1);
    // Stopped with no line shown says so in the thread.
    s.status_end(&conv, "house", &Value::Null, crate::channels::Ended::Stopped).await;
    assert!(rig.fake.texts().contains(&"stopped".to_owned()));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_come_in_and_go_out() {
    let rig = Rig::new().await;
    rig.fake.state.locked().files.insert("F1".into(), b"the notes".to_vec());
    let (s, task) = rig.run();
    let url = |n: &str| format!("{}/files/{n}", rig.fake.url);
    rig.fake.say(
        KAI,
        DM,
        "@house msg-file",
        0,
        &json!({"subtype": "file_share", "files": [{"id": "F1", "name": "../notes.txt", "url_private_download": url("F1"), "size": 9}]}),
    );
    until("the answer", || rig.fake.answers("msg-file") == 1).await;
    let task_text = rig
        .hq_calls()
        .into_iter()
        .map(|c| c["task"].as_str().unwrap().to_owned())
        .find(|t| t.contains("msg-file"))
        .unwrap();
    let paths: Vec<&str> =
        task_text.split("Attached files: ").nth(1).unwrap().split("\n\n(From").next().unwrap().split(", ").collect();
    let folder = rig.core.attachments_dir("slack/hq");
    assert_eq!(paths.len(), 1, "{task_text}");
    assert!(std::path::Path::new(paths[0]).starts_with(&folder), "inside the attachments folder: {}", paths[0]);
    assert!(paths[0].ends_with("/F1-notes.txt"), "{}", paths[0]);
    assert_eq!(std::fs::read(paths[0]).unwrap(), b"the notes");
    assert_eq!(rig.fake.state.locked().download_auth, [BOT_TOKEN], "downloaded with the bot token");

    // Files out: each to its upload URL, then shared together; the caption on the share.
    let outdir = rig.dir.join("out");
    std::fs::create_dir_all(&outdir).unwrap();
    for (n, c) in [("a.png", "image"), ("b.pdf", "doc")] {
        std::fs::write(outdir.join(n), c).unwrap();
    }
    let files: Vec<String> =
        ["a.png", "missing.png", "b.pdf"].iter().map(|n| outdir.join(n).to_string_lossy().into_owned()).collect();
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000007"});
    s.send_files(&conv, "house", &files, "see this").await.unwrap();
    let asked = rig.fake.calls("files.getUploadURLExternal");
    assert_eq!(asked.len(), 2);
    assert_eq!((asked[0]["filename"].as_str(), asked[0]["length"].as_str()), (Some("a.png"), Some("5")));
    let done = rig.fake.calls("files.completeUploadExternal");
    assert_eq!(done.len(), 1);
    assert_eq!((done[0]["channel_id"].as_str(), done[0]["thread_ts"].as_str()), (Some(DM), Some("1760000000.000007")));
    assert_eq!(done[0]["initial_comment"], "*Home* see this");
    let shared: Vec<Value> = serde_json::from_str(done[0]["files"].as_str().unwrap()).unwrap();
    assert_eq!(shared.len(), 2);
    let stored = rig.fake.state.locked().uploads.clone();
    assert_eq!(stored[shared[0]["id"].as_str().unwrap()], b"image");
    assert_eq!(stored[shared[1]["id"].as_str().unwrap()], b"doc");
    assert_eq!(shared[1]["title"], "b.pdf");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_over_the_limit_are_not_downloaded() {
    let rig = Rig::new().await;
    rig.fake.state.locked().files.insert("BIG".into(), vec![7u8; 4096]);
    let (_s, task) = rig.run_with(|s| s.pace.max_file = 1000);
    let url = |n: &str| format!("{}/files/{n}", rig.fake.url);
    // One that says it is big, and one that says it is small and isn't.
    rig.fake.say(
        KAI,
        DM,
        "@house msg-big",
        0,
        &json!({"files": [{"id": "BIG", "name": "huge.bin", "url_private_download": url("BIG"), "size": 5000},
                          {"id": "BIG", "name": "liar.bin", "url_private": url("BIG"), "size": 10}]}),
    );
    until("the answer", || rig.fake.answers("msg-big") == 1).await;
    let task_text = rig.hq_calls()[0]["task"].as_str().unwrap().to_owned();
    assert!(
        task_text.contains("(file too big: huge.bin)") && task_text.contains("(file too big: liar.bin)"),
        "{task_text}"
    );
    assert!(!task_text.contains("Attached files"), "{task_text}");
    assert_eq!(rig.fake.state.locked().download_auth.len(), 1, "the declared size is believed before downloading");
    let left = walk(&rig.core.attachments_dir("slack/hq"));
    assert_eq!(left.len(), 0, "{left:?}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_cannot_be_fetched_does_not_lose_the_message() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    let url = format!("{}/files/GONE", rig.fake.url);
    rig.fake.say(
        KAI,
        DM,
        "@house msg-gone",
        0,
        &json!({"files": [{"id": "GONE", "name": "gone.txt", "url_private": url}]}),
    );
    until("the answer", || rig.fake.answers("msg-gone") == 1).await;
    let task_text = rig.hq_calls()[0]["task"].as_str().unwrap().to_owned();
    assert!(task_text.contains("(file not downloaded: gone.txt)"), "{task_text}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_already_acknowledged_is_answered_after_its_socket_is_gone() {
    let rig = Rig::new().await;
    {
        let mut st = rig.fake.state.locked();
        st.files.insert("F1".into(), b"slow notes".to_vec());
        st.file_delay = Duration::from_millis(600);
    }
    let (_s, task) = rig.run();
    let url = format!("{}/files/F1", rig.fake.url);
    rig.fake.say(KAI, DM, "@house msg-slow", 0, &json!({"files": [{"id": "F1", "name": "n.txt", "url_private": url}]}));
    until("the download to start", || !rig.fake.state.locked().download_auth.is_empty()).await;
    task.abort(); // the connection is replaced while the message is still being taken in
    until("the answer", || rig.fake.answers("msg-slow") == 1).await;
    assert!(rig.hq_calls()[0]["task"].as_str().unwrap().contains("n.txt"));
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disconnect_gets_a_new_socket_and_nothing_is_lost() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    until("the first socket", || rig.fake.opened() == 1 && rig.fake.state.locked().connected == 1).await;
    dm(&rig, "@house msg-before");
    until("the first answer", || rig.fake.answers("msg-before") == 1).await;
    rig.fake.state.locked().pending.push_back(json!({"type": "disconnect", "reason": "refresh_requested"}));
    until("a second socket", || rig.fake.opened() == 2 && rig.fake.state.locked().connected == 2).await;
    dm(&rig, "@house msg-after");
    until("the answer after", || rig.fake.answers("msg-after") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((rig.fake.answers("msg-after"), rig.fake.answers("msg-before")), (1, 1));
    assert_eq!(rig.fake.calls("auth.test").len(), 1, "the same connection goes on, no new login");
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_socket_is_replaced_by_a_new_connection() {
    let rig = Rig::new().await;
    let mut s = rig.slack(&rig.core);
    s.quiet = Duration::from_secs(8); // a message may take a third of this under load
    let backoff =
        Backoff { first: Duration::from_millis(50), max: Duration::from_millis(200), healthy: Duration::from_secs(60) };
    let supervised = tokio::spawn(supervise(Arc::new(s), backoff));
    until("the first login", || rig.fake.calls("auth.test").len() == 1 && rig.fake.state.locked().connected == 1).await;
    dm(&rig, "@house msg-before");
    until("the first answer", || rig.fake.answers("msg-before") == 1).await;
    rig.fake.state.locked().hang = true; // a socket that never answers again
    until("a second login", || rig.fake.calls("auth.test").len() >= 2).await;
    rig.fake.state.locked().hang = false;
    until("a second socket", || rig.fake.state.locked().connected >= 2).await;
    dm(&rig, "@house msg-after");
    until("the answer after", || rig.fake.answers("msg-after") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((rig.fake.answers("msg-after"), rig.fake.answers("msg-before")), (1, 1));
    supervised.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rate_limit_is_waited_out_and_blocks_fall_back_to_text() {
    let rig = Rig::new().await;
    let (s, task) = rig.run();
    until("connected", || rig.fake.opened() == 1).await;
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000001"});
    rig.fake.state.locked().rate_limit = 1;
    let begun = Instant::now();
    s.post(&conv, "house", "after the wait", Some(42.0)).await.unwrap();
    assert!(begun.elapsed() >= Duration::from_millis(900), "Retry-After is respected");
    let calls = rig.fake.calls("chat.postMessage");
    assert_eq!(calls.len(), 2, "sent again after the 429");
    assert_eq!(calls[1]["blocks"][0]["text"], "after the wait\n\n_context 42%_");
    // An older workspace refuses markdown blocks: the same text, plain.
    rig.fake.state.locked().reject_blocks = true;
    s.post(&conv, "house", "plain please", None).await.unwrap();
    let last = rig.fake.posted().pop().unwrap();
    assert_eq!((last["text"].as_str(), last["blocks"].is_null()), (Some("plain please"), true));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_answers_are_cut_into_blocks_that_fit() {
    let rig = Rig::new().await;
    let (s, task) = rig.run();
    until("connected", || rig.fake.opened() == 1).await;
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000001"});
    let line = "x".repeat(99);
    let long = (0..300).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
    s.post(&conv, "house", &long, None).await.unwrap();
    let posted = rig.fake.posted();
    assert!(posted.len() >= 3, "{}", posted.len());
    for p in &posted {
        assert!(p["blocks"][0]["text"].as_str().unwrap().chars().count() <= 11_000);
        assert!(p["text"].as_str().unwrap().chars().count() <= 3000);
    }
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reactions_use_slacks_names() {
    let rig = Rig::new().await;
    let (s, task) = rig.run();
    let m = Incoming { reference: json!({"channel": ROOM, "ts": "1760000000.000003"}), ..Incoming::default() };
    s.react(&m, Reaction::Stop).await;
    s.react(&m, Reaction::Redo).await;
    s.react(&Incoming::default(), Reaction::Stop).await;
    let calls = rig.fake.calls("reactions.add");
    let names: Vec<&str> = calls.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["octagonal_sign", "repeat"]);
    assert_eq!((calls[0]["channel"].as_str(), calls[0]["timestamp"].as_str()), (Some(ROOM), Some("1760000000.000003")));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_token_ends_the_connection_without_showing_it() {
    let rig = Rig::new().await;
    let s = Arc::new(Slack::new(
        "slack/hq",
        "xoxb-WRONGTOKEN",
        APP_TOKEN,
        &format!("{}/api", rig.fake.url),
        rig.core.clone(),
    ));
    let err = tokio::time::timeout(Duration::from_secs(10), s.start()).await.unwrap().unwrap_err();
    let shown = format!("{err:#}");
    assert!(shown.contains("invalid_auth") && !shown.contains("WRONGTOKEN"), "{shown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokens_and_the_socket_ticket_never_show_in_errors() {
    // Nothing listens there: the connection error must not carry the tokens.
    let api = Api::new("http://127.0.0.1:1/api", BOT_TOKEN, APP_TOKEN);
    let err = api.call("auth.test", &json!({}), super::api::As::Bot).await.unwrap_err();
    assert!(!err.to_string().contains(BOT_TOKEN) && !err.to_string().contains("127.0.0.1"), "{err}");
    assert_eq!(api.scrub(&format!("bad {BOT_TOKEN} and {APP_TOKEN}")), "bad <token> and <token>");
    let secrets =
        super::Secrets(std::collections::BTreeMap::from([("SLACK_BOT_TOKEN".to_owned(), BOT_TOKEN.to_owned())]));
    assert!(!format!("{secrets:?}").contains(BOT_TOKEN));
}

#[test]
fn the_apps_mention_and_slacks_escapes_are_taken_out() {
    assert_eq!(text::clean("<@U_HQ> hello", "U_HQ"), "hello");
    assert_eq!(text::clean("hey <@U_HQ>   @shop do it", "U_HQ"), "hey @shop do it");
    assert_eq!(text::clean("<@U_HQ> !link ABCD12", "U_HQ"), "/link ABCD12");
    assert_eq!(text::clean("!stop", "U_HQ"), "/stop");
    assert_eq!(
        text::clean("see <https://x.org/a?b=1&amp;c=2|the page> and <https://y.org>", "U_HQ"),
        "see [the page](https://x.org/a?b=1&c=2) and https://y.org"
    );
    assert_eq!(text::clean("1 &lt; 2 &amp;&amp; 3 &gt; 2", "U_HQ"), "1 < 2 && 3 > 2");
    assert_eq!(
        text::clean("<@U_OTHER> and <#C1|general> and <mailto:a@b.c|a@b.c>", "U_HQ"),
        "<@U_OTHER> and <#C1|general> and <mailto:a@b.c|a@b.c>"
    );
    assert_eq!(text::clean("<http> <https://> done", "U_HQ"), "<http> <https://> done");
}

#[test]
fn only_a_real_at_word_names_an_agent() {
    assert!(text::has_agent_word("@house msg"));
    assert!(text::has_agent_word("hey @my-agent"));
    assert!(!text::has_agent_word("mail me@house.org"));
    assert!(!text::has_agent_word("@@house"));
    assert!(!text::has_agent_word("at @ home"));
    assert!(text::has_agent_word("(@house)"));
}

#[test]
fn a_slack_timestamp_is_unix_seconds() {
    assert_eq!(text::sent("1700000000.000200"), Some(1_700_000_000.000_2));
    assert_eq!(text::sent("x"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_closed_without_a_disconnect_reconnects_in_run_and_keeps_order() {
    let rig = Rig::new().await;
    {
        let mut st = rig.fake.state.locked();
        st.files.insert("F1".into(), b"slow".to_vec());
        st.file_delay = Duration::from_millis(500);
    }
    let (_s, task) = rig.run();
    until("the first socket", || rig.fake.state.locked().connected == 1).await;
    let url = format!("{}/files/F1", rig.fake.url);
    rig.fake.say(
        KAI,
        DM,
        "@house msg-first",
        0,
        &json!({"files": [{"id": "F1", "name": "n.txt", "url_private": url}]}),
    );
    until("the download to start", || !rig.fake.state.locked().download_auth.is_empty()).await;
    rig.fake.state.locked().pending.push_back(json!({"type": "close-without-saying-so"}));
    until("a second socket", || rig.fake.opened() == 2 && rig.fake.state.locked().connected == 2).await;
    rig.fake.state.locked().file_delay = Duration::ZERO;
    dm(&rig, "@house msg-second");
    until("both answers", || rig.fake.answers("msg-first") == 1 && rig.fake.answers("msg-second") == 1).await;
    let order: Vec<String> = rig.fake.texts().into_iter().filter(|t| t.contains("echo:")).collect();
    assert!(order[0].contains("msg-first") && order[1].contains("msg-second"), "{order:?}");
    assert_eq!(rig.fake.calls("auth.test").len(), 1, "no new login: run() itself reconnected");
    assert!(!task.is_finished());
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_event_is_not_handled() {
    let rig = Rig::new().await;
    let (_s, task) = rig.run();
    rig.fake.say(KAI, DM, "@house msg-edited", 0, &json!({"edited": {"user": KAI, "ts": "1760000001.000000"}}));
    dm(&rig, "@house msg-plain");
    until("the plain answer", || rig.fake.answers("msg-plain") == 1).await;
    assert_eq!(rig.fake.answers("msg-edited"), 0);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_fallback_post_is_an_error() {
    let rig = Rig::new().await;
    let s = rig.slack(&rig.core);
    // Blocks refused, and the plain text is refused too.
    rig.fake.state.locked().reject_blocks = true;
    rig.fake.state.locked().reject_plain = true;
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000007"});
    assert!(s.post(&conv, "house", "hello", None).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bot_token_goes_only_to_slack_hosts_and_a_sign_in_page_is_a_failure() {
    let rig = Rig::new().await;
    {
        let mut st = rig.fake.state.locked();
        st.files.insert("F1".into(), b"one".to_vec());
        st.files.insert("HTML1".into(), b"<html>sign in</html>".to_vec());
    }
    let (_s, task) = rig.run();
    // Same server, but another host name: not Slack's.
    let elsewhere = rig.fake.url.replace("127.0.0.1", "localhost");
    rig.fake.say(
        KAI,
        DM,
        "@house msg-hosts",
        0,
        &json!({"files": [{"id": "F1", "name": "ok.txt", "url_private": format!("{}/files/F1", rig.fake.url)},
                          {"id": "F2", "name": "other.txt", "url_private": format!("{elsewhere}/files/F1")},
                          {"id": "H", "name": "login.txt", "url_private": format!("{}/files/HTML1", rig.fake.url)}]}),
    );
    until("the answer", || rig.fake.answers("msg-hosts") == 1).await;
    let task_text = rig.hq_calls()[0]["task"].as_str().unwrap().to_owned();
    assert!(task_text.contains("(file not downloaded: other.txt)"), "{task_text}");
    assert!(task_text.contains("(file not downloaded: login.txt)"), "{task_text}");
    assert!(task_text.contains("ok.txt"), "{task_text}");
    assert_eq!(rig.fake.state.locked().download_auth.len(), 2, "never asked the other host");
    assert!(walk(&rig.core.attachments_dir("slack/hq")).iter().all(|p| !p.ends_with("H-login.txt")));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_with_the_same_name_do_not_overwrite_each_other() {
    let rig = Rig::new().await;
    {
        let mut st = rig.fake.state.locked();
        st.files.insert("A".into(), b"first".to_vec());
        st.files.insert("B".into(), b"second".to_vec());
    }
    let (_s, task) = rig.run();
    let url = |n: &str| format!("{}/files/{n}", rig.fake.url);
    rig.fake.say(
        KAI,
        DM,
        "@house msg-same",
        0,
        &json!({"files": [{"id": "A", "name": "image.png", "url_private": url("A")},
                          {"id": "B", "name": "image.png", "url_private": url("B")}]}),
    );
    until("the answer", || rig.fake.answers("msg-same") == 1).await;
    let mut contents: Vec<Vec<u8>> =
        walk(&rig.core.attachments_dir("slack/hq")).iter().map(|p| std::fs::read(p).unwrap()).collect();
    contents.sort();
    assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_over_the_upload_cap_is_refused_before_reading_it() {
    let rig = Rig::new().await;
    let s = rig.slack(&rig.core);
    let big = rig.dir.join("big.bin");
    std::fs::File::create(&big).unwrap().set_len(super::outbound::MAX_UPLOAD + 1).unwrap(); // sparse
    let conv = json!({"channel": DM, "thread_ts": "1760000000.000007"});
    let err = s.send_files(&conv, "house", &[big.to_string_lossy().into_owned()], "x").await.unwrap_err();
    assert!(err.to_string().contains("too big"), "{err}");
    assert_eq!(rig.fake.calls("files.getUploadURLExternal").len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strangers_files_are_not_downloaded() {
    let rig = Rig::new().await;
    rig.fake.state.locked().files.insert("F1".into(), b"x".to_vec());
    let (_s, task) = rig.run();
    let file = json!({"subtype": "file_share", "files": [{"id": "F1", "name": "a.txt",
        "url_private_download": format!("{}/files/F1", rig.fake.url), "size": 1}]});
    rig.fake.say("U_STRANGER", DM, "hello", 0, &file);
    until("the not-linked reply", || !rig.fake.posted().is_empty()).await;
    assert_eq!(rig.fake.state.locked().download_auth.len(), 0);
    let kept = walk(&rig.core.attachments_dir("slack/hq"));
    assert!(kept.is_empty(), "{kept:?}");
    task.abort();
}
