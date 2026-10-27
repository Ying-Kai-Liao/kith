//! The device side of the relay (docs/reference/remote-relay.md, section 5.10 of the bot-guests design): the signed
//! WebSocket upgrade, the hello/welcome handshake pinned to the computer's key, and the encrypted frame stream that
//! carries inner requests `{id, m, b}` and answers `{id, ok}` / `{id, err}` / `{id, ev}`.
//! Reference implementation: apps/desktop/src/main/relay/{crypto,link}.ts. The cryptography is `kith-wire`.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use kith_wire::crypto::{
    Keys, Labels, Opener, Sealer, b64, channel_keys, computer_id_raw, hs1_input, random, request_sig_header,
    request_sig_input, sign, transcript_hash, unb64_n, verify, x25519, x25519_pub,
};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config};
use url::Url;
use x25519_dalek::StaticSecret;

use crate::exit::{CliError, HostErr};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Largest message the host may send us (the desktop client allows the same).
const MAX_INBOUND: usize = 16 * 1024 * 1024;
const PING: &str = r#"{"t":"ping"}"#;
/// The relay closes a socket that is silent for 75 s and answers a text ping with a pong.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENT_LIMIT: Duration = Duration::from_secs(75);
const HANDSHAKE_STEP: Duration = Duration::from_secs(10);

/// Why a channel could not be opened or broke. The command decides which of these to retry.
#[derive(Clone, Debug)]
pub enum Fail {
    /// The relay says the computer is not connected (`presence online:false`).
    Offline,
    /// The cloud refused the upgrade with 403: the key is not (yet, or any longer) in the computer's ACL.
    Refused,
    /// The socket ended or misbehaved after it was open. A new connection can pick up.
    Dropped(String),
    /// The cloud could not be reached.
    Network(String),
    /// The host answered a request with an error.
    Host(HostErr),
    /// Nothing more to try: the command ends with this.
    Final(CliError),
}

impl Fail {
    pub fn into_cli(self) -> CliError {
        match self {
            Fail::Offline => CliError::offline(),
            Fail::Refused => CliError::removed(),
            Fail::Dropped(why) => CliError::network(format!("The connection to the computer dropped ({why}).")),
            Fail::Network(why) => CliError::network(format!("Can't reach the Kith cloud: {why}")),
            Fail::Host(e) => e.into_cli(),
            Fail::Final(e) => e,
        }
    }

    /// A broken connection that a reconnect can fix.
    pub fn is_transient(&self) -> bool {
        matches!(self, Fail::Dropped(_) | Fail::Network(_))
    }
}

/// Where a channel goes: the cloud, the computer's id, and the key its welcome must be signed by.
#[derive(Clone, Debug)]
pub struct Target {
    pub cloud: String,
    pub computer_id: String,
    pub host_sign_key: [u8; 32],
}

/// `host[:port]` as the cloud sees it in the `Host` header: what `Kith-Sig` signs as the authority.
pub fn authority(u: &Url) -> String {
    let host = u.host_str().unwrap_or_default();
    match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// A `Kith-Sig` header for one request (`kith-sig-v1`).
pub fn signature_header(key: &SigningKey, method: &str, authority: &str, path_and_query: &str, body: &[u8]) -> String {
    let (ts, nonce) = (now_ms(), b64(&random::<16>()));
    let input = request_sig_input(Labels::Kith, method, authority, path_and_query, ts, &nonce, body);
    request_sig_header(key, ts, &nonce, &input)
}

// ---- handshake (section 6.2 of remote-relay.md) ----

/// The device half: `hello` out, then `finish(welcome)` verifies the pinned host and derives the keys.
pub struct Handshake {
    cid: [u8; 16],
    dk: [u8; 32],
    host_pub: [u8; 32],
    eph: StaticSecret,
    ek_d: [u8; 32],
    n: [u8; 32],
    hello: Value,
}

impl Handshake {
    pub fn new(device: &SigningKey, host_pub: [u8; 32]) -> Self {
        Self::with(device, host_pub, StaticSecret::from(random::<32>()), random::<32>())
    }

    /// Fixed ephemeral key and nonce: for the test vectors.
    pub fn with(device: &SigningKey, host_pub: [u8; 32], eph: StaticSecret, n: [u8; 32]) -> Self {
        let cid = computer_id_raw(&host_pub);
        let dk = device.verifying_key().to_bytes();
        let ek_d = x25519_pub(&eph);
        let sig = sign(device, &hs1_input(Labels::Kith, &cid, &dk, &ek_d, &n));
        let hello = json!({"t": "hello", "v": 1, "dk": b64(&dk), "ek": b64(&ek_d), "n": b64(&n), "sig": b64(&sig)});
        Self { cid, dk, host_pub, eph, ek_d, n, hello }
    }

    pub fn hello(&self) -> &Value {
        &self.hello
    }

    /// Fails unless the pinned host key signed the transcript: never fall back to trusting whatever answered.
    pub fn finish(&self, ek_h: &[u8; 32], sig: &[u8; 64]) -> anyhow::Result<Keys> {
        let th = transcript_hash(Labels::Kith, &self.cid, &self.dk, &self.ek_d, &self.n, ek_h);
        verify(&self.host_pub, &th, sig)?;
        let shared = x25519(&self.eph, ek_h)?;
        Ok(channel_keys(Labels::Kith, &shared, &th))
    }
}

// ---- TLS ----

/// rustls on the ring provider with the webpki roots compiled in, so a bare VM without a CA bundle still verifies.
/// Built explicitly (not `ClientConfig::builder()`): in a workspace build another crate may enable aws-lc too, and
/// then rustls refuses to guess a default provider.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map(|b| b.with_root_certificates(roots).with_no_client_auth());
            // Both the provider and the versions are fixed at compile time; there is no input that makes this fail.
            Arc::new(
                config.unwrap_or_else(|e| unreachable!("ring provider rejected the default protocol versions: {e}")),
            )
        })
        .clone()
}

// ---- the channel ----

pub struct Channel {
    ws: Ws,
    sealer: Sealer,
    opener: Opener,
    next_id: u64,
    /// Inner messages that arrived while waiting for something else (events during a call).
    parked: VecDeque<Value>,
    last_ping: Instant,
    last_heard: Instant,
}

/// Something that came off a subscription.
#[derive(Debug, PartialEq)]
pub enum Event {
    Ev(Value),
    End,
}

fn close_fail(code: u16) -> Fail {
    match code {
        // revoked / unauthorized
        4001 | 4003 => Fail::Final(CliError::removed()),
        // protocol version the relay or host no longer speaks
        4400 => Fail::Final(CliError::version()),
        // 4100 relay restart, 4011 rekey: a fresh channel carries on.
        4011 | 4100 => Fail::Dropped(format!("closed {code}, reconnecting")),
        _ => Fail::Dropped(format!("closed {code}")),
    }
}

fn upgrade_fail(status: u16) -> Fail {
    match status {
        403 => Fail::Refused,
        // The cloud doesn't know this computer.
        404 => Fail::Offline,
        401 => Fail::Final(CliError::network("The cloud refused this device's signature. Is this VM's clock right?")),
        426 => Fail::Final(CliError::version()),
        429 => Fail::Final(CliError::new(
            crate::exit::Exit::Busy,
            "rateLimited",
            "The cloud is rate limiting this device. Try again soon.",
        )),
        s => Fail::Network(format!("the cloud answered {s}")),
    }
}

/// `GET {cloud}/v1/relay/device/{computerId}?v=1`, signed with the device key, then the handshake.
pub async fn connect(target: &Target, key: &SigningKey, dial_timeout: Duration) -> Result<Channel, Fail> {
    let base =
        Url::parse(&target.cloud).map_err(|e| Fail::Final(CliError::unexpected(format!("bad cloud address: {e}"))))?;
    let path_and_query = kith_wire::cloud::path::relay_device(&target.computer_id);
    let mut ws_url = base.join(&path_and_query).map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?;
    ws_url
        .set_scheme(if base.scheme() == "http" { "ws" } else { "wss" })
        .map_err(|()| Fail::Final(CliError::unexpected("bad cloud address")))?;

    let mut request =
        ws_url.as_str().into_client_request().map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?;
    let sig = signature_header(key, "GET", &authority(&base), &path_and_query, b"");
    let headers = request.headers_mut();
    headers
        .insert("Kith-Sig", HeaderValue::from_str(&sig).map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?);
    headers.insert("User-Agent", HeaderValue::from_static(concat!("kith-guest/", env!("CARGO_PKG_VERSION"))));

    let dial = connect_async_tls_with_config(request, None, true, Some(Connector::Rustls(tls_config())));
    let ws = match tokio::time::timeout(dial_timeout, dial).await {
        Err(_) => return Err(Fail::Network("connecting timed out".into())),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) => return Err(upgrade_fail(resp.status().as_u16())),
        Ok(Err(e)) => return Err(Fail::Network(e.to_string())),
        Ok(Ok((ws, _))) => ws,
    };
    let now = Instant::now();
    let mut pending = Channel {
        ws,
        // Replaced by the real keys once the welcome verifies; never used before.
        sealer: Sealer::new(Labels::Kith, [0; 32]),
        opener: Opener::new(Labels::Kith, [0; 32], MAX_INBOUND),
        next_id: 0,
        parked: VecDeque::new(),
        last_ping: now,
        last_heard: now,
    };
    pending.handshake(key, target.host_sign_key).await?;
    Ok(pending)
}

impl Channel {
    async fn send_text(&mut self, text: String) -> Result<(), Fail> {
        self.ws.send(Message::text(text)).await.map_err(|e| Fail::Dropped(e.to_string()))
    }

    /// The next JSON wire message (`{"t": ...}`), pinging while idle. `None` at `until`.
    async fn next_wire(&mut self, until: Instant) -> Result<Option<Value>, Fail> {
        loop {
            let now = Instant::now();
            if now >= until {
                return Ok(None);
            }
            if now.duration_since(self.last_heard) > SILENT_LIMIT {
                return Err(Fail::Dropped("the relay went silent".into()));
            }
            let ping_at = self.last_ping + PING_EVERY;
            let wake = until.min(ping_at);
            match timeout_at(wake, self.ws.next()).await {
                Err(_) => {
                    if Instant::now() >= ping_at {
                        self.last_ping = Instant::now();
                        self.send_text(PING.to_string()).await?;
                    }
                }
                Ok(None) => return Err(Fail::Dropped("the connection closed".into())),
                Ok(Some(Err(e))) => return Err(Fail::Dropped(e.to_string())),
                Ok(Some(Ok(msg))) => {
                    self.last_heard = Instant::now();
                    match msg {
                        Message::Text(t) => {
                            if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                return Ok(Some(v));
                            }
                        }
                        Message::Close(frame) => {
                            return Err(
                                frame.map_or_else(|| Fail::Dropped("closed".into()), |f| close_fail(u16::from(f.code)))
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn handshake(&mut self, key: &SigningKey, host_pub: [u8; 32]) -> Result<(), Fail> {
        let hs = Handshake::new(key, host_pub);
        let deadline = Instant::now() + HANDSHAKE_STEP;
        let mut sent_hello = false;
        loop {
            let Some(wire) = self.next_wire(deadline).await? else {
                return Err(Fail::Dropped("the computer didn't answer the handshake".into()));
            };
            match wire["t"].as_str() {
                Some("presence") if wire["online"] == true => {
                    if !sent_hello {
                        self.send_text(hs.hello().to_string()).await?;
                        sent_hello = true;
                    }
                }
                Some("presence") => return Err(Fail::Offline),
                Some("reject") => return Err(reject_fail(&wire)),
                Some("welcome") if sent_hello => {
                    let ek: Option<[u8; 32]> = wire["ek"].as_str().and_then(|s| unb64_n(s).ok());
                    let sig: Option<[u8; 64]> = wire["sig"].as_str().and_then(|s| unb64_n(s).ok());
                    let (Some(ek), Some(sig)) = (ek, sig) else {
                        return Err(Fail::Dropped("malformed welcome".into()));
                    };
                    // A welcome the pinned key didn't sign is a different computer behind that id: stop, never retry.
                    let keys = hs.finish(&ek, &sig).map_err(|_| Fail::Final(CliError::identity()))?;
                    self.sealer = Sealer::new(Labels::Kith, keys.d2h);
                    self.opener = Opener::new(Labels::Kith, keys.h2d, MAX_INBOUND);
                    self.last_heard = Instant::now();
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    /// The next inner message (decrypted, parsed). `None` at `until`. A presence change ends the channel: `offline`
    /// means the computer left, `online` means it came back and our keys are void.
    async fn next_inner(&mut self, until: Instant) -> Result<Option<Value>, Fail> {
        if let Some(v) = self.parked.pop_front() {
            return Ok(Some(v));
        }
        loop {
            let Some(wire) = self.next_wire(until).await? else { return Ok(None) };
            match wire["t"].as_str() {
                Some("f") => match self.opener.open(&wire) {
                    Ok(Some(bytes)) => {
                        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                            return Ok(Some(v));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => return Err(Fail::Dropped(format!("frame error {e:?}"))),
                },
                Some("presence") if wire["online"] == true => {
                    return Err(Fail::Dropped("the computer reconnected".into()));
                }
                Some("presence") => return Err(Fail::Offline),
                Some("reject") => return Err(reject_fail(&wire)),
                _ => {}
            }
        }
    }

    async fn send_inner(&mut self, message: &Value) -> Result<(), Fail> {
        let bytes = serde_json::to_vec(message).map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?;
        // None: the counter is about to wrap. Replace the channel.
        let frames = self.sealer.seal(&bytes).ok_or_else(|| Fail::Dropped("rekeying".into()))?;
        for frame in frames {
            self.send_text(frame.to_string()).await?;
        }
        Ok(())
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// One request and its answer. Events of a running subscription that arrive meanwhile are kept for `next_event`.
    pub async fn call(&mut self, method: &str, body: Value, limit: Duration) -> Result<Value, Fail> {
        let id = self.new_id();
        self.send_inner(&json!({"id": id, "m": method, "b": body})).await?;
        let deadline = Instant::now() + limit;
        let mut held = Vec::new();
        let outcome = loop {
            let Some(inner) = self.next_inner(deadline).await? else {
                break Err(Fail::Final(CliError::timeout("The computer didn't answer in time.")));
            };
            if inner["id"].as_u64() != Some(id) {
                held.push(inner);
                continue;
            }
            if let Some(err) = inner.get("err") {
                break Err(Fail::Host(host_err(err)));
            }
            break Ok(inner.get("ok").cloned().unwrap_or(Value::Null));
        };
        // Back to the front of the queue, in arrival order.
        for v in held.into_iter().rev() {
            self.parked.push_front(v);
        }
        outcome
    }

    /// Opens a subscription (`sub: "events"`); returns its id.
    pub async fn subscribe(&mut self, sub: &str, body: Value) -> Result<u64, Fail> {
        let id = self.new_id();
        self.send_inner(&json!({"id": id, "sub": sub, "b": body})).await?;
        Ok(id)
    }

    /// The next event of subscription `sub_id`; `None` at `until`.
    pub async fn next_event(&mut self, sub_id: u64, until: Instant) -> Result<Option<Event>, Fail> {
        loop {
            let Some(inner) = self.next_inner(until).await? else { return Ok(None) };
            if inner["id"].as_u64() != Some(sub_id) {
                continue;
            }
            if let Some(ev) = inner.get("ev") {
                return Ok(Some(Event::Ev(ev.clone())));
            }
            if inner["end"] == true {
                return Ok(Some(Event::End));
            }
            if let Some(err) = inner.get("err") {
                return Err(Fail::Host(host_err(err)));
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

fn host_err(err: &Value) -> HostErr {
    HostErr {
        status: err["status"].as_u64().and_then(|s| u16::try_from(s).ok()).unwrap_or(500),
        code: err["code"].as_str().map(str::to_string),
        message: err["message"].as_str().unwrap_or_default().to_string(),
        retry_after: err["retryAfter"].as_u64(),
    }
}

/// A `{"t":"reject"}` from the relay or the host (section 7.5 of remote-relay.md).
fn reject_fail(wire: &Value) -> Fail {
    let message = wire["message"].as_str().unwrap_or_default();
    match wire["code"].as_str() {
        Some("unauthorized" | "revoked") => Fail::Final(CliError::removed()),
        Some("unsupportedVersion") => Fail::Final(CliError::version()),
        // The computer couldn't confirm access lately, or rate limits, or skewed clocks: a later attempt can work.
        _ => Fail::Dropped(if message.is_empty() {
            "the computer refused the connection".into()
        } else {
            message.to_string()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../../../docs/reference/fixtures/remote-relay-vectors.json")).unwrap()
    }

    fn b32(v: &Value) -> [u8; 32] {
        unb64_n(v.as_str().unwrap()).unwrap()
    }

    fn signing(seed: &Value) -> SigningKey {
        SigningKey::from_bytes(&b32(seed))
    }

    /// The device's hello and the keys it derives are the fixture's, byte for byte.
    #[test]
    fn handshake_matches_the_vectors() {
        let v = vectors();
        let (k, h) = (&v["keys"], &v["handshake"]);
        let device = signing(&k["deviceSignSeed"]);
        let host = signing(&k["hostSignSeed"]);
        let host_pub = host.verifying_key().to_bytes();
        let hs = Handshake::with(&device, host_pub, StaticSecret::from(b32(&h["deviceEphPriv"])), b32(&h["n"]));
        assert_eq!(hs.hello()["sig"], h["hs1Sig"]);
        assert_eq!(hs.hello()["ek"], h["deviceEphPub"]);
        assert_eq!(hs.hello()["dk"], k["deviceSignPub"]);

        let keys = hs.finish(&b32(&h["hostEphPub"]), &unb64_n(h["hs2Sig"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(b64(&keys.d2h), h["kD2H"]);
        assert_eq!(b64(&keys.h2d), h["kH2D"]);
    }

    #[test]
    fn a_welcome_signed_by_another_key_is_refused() {
        let v = vectors();
        let (k, h) = (&v["keys"], &v["handshake"]);
        let device = signing(&k["deviceSignSeed"]);
        let other = SigningKey::from_bytes(&[9; 32]);
        let host_pub = signing(&k["hostSignSeed"]).verifying_key().to_bytes();
        let hs = Handshake::with(&device, host_pub, StaticSecret::from(b32(&h["deviceEphPriv"])), b32(&h["n"]));
        let th: [u8; 32] = b32(&h["transcriptHash"]);
        let forged = sign(&other, &th);
        assert!(hs.finish(&b32(&h["hostEphPub"]), &forged).is_err());
        // A host key that isn't the pinned one signing the right transcript fails the same way.
        let mut tampered: [u8; 64] = unb64_n(h["hs2Sig"].as_str().unwrap()).unwrap();
        tampered[0] ^= 1;
        assert!(hs.finish(&b32(&h["hostEphPub"]), &tampered).is_err());
    }

    /// Frames the host's fixture sealed open with our Opener, and ours seal to the same bytes.
    #[test]
    fn frames_round_trip_against_the_vectors() {
        let v = vectors();
        let h = &v["handshake"];
        let (d2h, h2d) = (b32(&h["kD2H"]), b32(&h["kH2D"]));
        let mut sealer = Sealer::new(Labels::Kith, d2h);
        let mut opener = Opener::new(Labels::Kith, h2d, MAX_INBOUND);
        for f in v["frames"].as_array().unwrap() {
            let msg = f["message"].as_str().unwrap().as_bytes();
            let (c, last) = (f["c"].as_u64().unwrap(), f["final"].as_bool().unwrap());
            if f["dir"] == "d2h" {
                // Only the single-frame messages are whole messages we can seal in one go.
                if c == 0 && last {
                    let frames = sealer.seal(msg).unwrap();
                    assert_eq!(frames[0]["d"], f["d"]);
                    assert_eq!(frames[0]["c"], c);
                }
            } else {
                let frame = json!({"t": "f", "c": c, "d": f["d"]});
                assert_eq!(opener.open(&frame).unwrap().unwrap(), msg);
            }
        }
    }

    /// The signed upgrade is the fixture's `Kith-Sig`, byte for byte.
    #[test]
    fn signature_header_matches_the_vector() {
        let v = vectors();
        let r = &v["requestSig"];
        let key = signing(&v["keys"]["deviceSignSeed"]);
        let (ts, nonce) = (r["ts"].as_i64().unwrap(), r["nonce"].as_str().unwrap());
        let input = request_sig_input(
            Labels::Kith,
            "GET",
            r["authority"].as_str().unwrap(),
            r["pathAndQuery"].as_str().unwrap(),
            ts,
            nonce,
            b"",
        );
        assert_eq!(input, r["canonical"].as_str().unwrap());
        assert_eq!(format!("Kith-Sig: {}", request_sig_header(&key, ts, nonce, &input)), r["header"].as_str().unwrap());
        let live = signature_header(&key, "GET", "cloud.example.com", "/v1/relay/device/x?v=1", b"");
        for part in ["v=1,kid=", ",ts=", ",nonce=", ",sig="] {
            assert!(live.contains(part), "{live}");
        }
    }

    #[test]
    fn authority_keeps_a_non_default_port() {
        assert_eq!(authority(&Url::parse("https://cloud.example.com").unwrap()), "cloud.example.com");
        assert_eq!(authority(&Url::parse("http://127.0.0.1:8787").unwrap()), "127.0.0.1:8787");
        assert_eq!(authority(&Url::parse("https://cloud.example.com:443").unwrap()), "cloud.example.com");
    }

    #[test]
    fn close_and_upgrade_codes_map_as_documented() {
        assert!(matches!(close_fail(4003), Fail::Final(e) if e.exit == crate::exit::Exit::Removed));
        assert!(matches!(close_fail(4001), Fail::Final(e) if e.exit == crate::exit::Exit::Removed));
        assert!(matches!(close_fail(4400), Fail::Final(e) if e.exit == crate::exit::Exit::Version));
        assert!(matches!(close_fail(4011), Fail::Dropped(_)));
        assert!(matches!(close_fail(1006), Fail::Dropped(_)));
        assert!(matches!(upgrade_fail(403), Fail::Refused));
        assert!(matches!(upgrade_fail(502), Fail::Network(_)));
    }
}
