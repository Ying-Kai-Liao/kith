//! The owner socket: how the CLI reaches the daemon (docs/plans/runtime-design.md section 1, the principals table).
//!
//! Who may connect: the same OS user, and only with the local owner token.
//! - Unix: a socket (0600) inside the core folder (0700), and a peer-credentials check on every accept.
//! - Windows: a named pipe whose access list names this account and SYSTEM only.
//!
//! Both come from `kith-holder`'s platform layer, which already does exactly that for the holder's socket.
//!
//! Messages are JSON-RPC lines. A connection says hello first and proves the token before anything runs:
//! 1. client: `hello` with the protocol number, its version, a nonce, and the rows it may call (`rows`: name -> hash
//!    of that row, see [`row_hash`]). A client that sends no `rows` (an older one) is held to the whole table's hash
//!    (`schema`, `kith_api::schema_hash`) instead;
//! 2. daemon: its own protocol, schema, version, generation, pid, a nonce and HMAC(token, "daemon", both nonces), and
//!    `compatible`. The client checks the proof (it is talking to the daemon that made the token). Compatible means
//!    the same protocol and a table that has every row the client named, with the same shape: a daemon with extra
//!    rows still serves, one missing a row (or with another shape of it, listed in `unfit`) does not. Otherwise this
//!    is where it ends: the client falls back and says so, nothing is run;
//! 3. client: `auth` with HMAC(token, "cli", both nonces); the token itself never crosses the socket;
//! 4. any number of calls: a method of the table with params. The caller is the owner, always.
//!
//! The token is rewritten at each daemon start, so a client that read it just before a restart fails the proof and
//! retries once with the new one.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kith_holder::auth;
use kith_holder::platform::{self, Stream};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::perms;

/// Bumped for a change in the hello or the line format that an older peer cannot read.
pub(crate) const PROTOCOL: u32 = 1;
/// This build of the core, shown by `kith doctor` and in the mismatch note.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_LINE: u64 = 1 << 20;
const MAX_CONNECTIONS: usize = 64;
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the daemon of this layout listens.
#[cfg(unix)]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    layout.dir.join("owner.sock")
}

/// Where the daemon of this layout listens: pipe names are global, so the core folder is hashed into it.
#[cfg(not(unix))]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in layout.dir.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    PathBuf::from(format!(r"\\.\pipe\kith-owner-{hash:016x}"))
}

/// 64 random hex digits: the owner token, the holder secret.
pub(crate) fn random_secret() -> Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw)
        .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
    Ok(raw.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// A new token, owner-only, in place through a rename (a reader sees the old one or the new one, never half of it).
fn write_token(layout: &Layout) -> Result<String> {
    let token = random_secret()?;
    let tmp = layout.token.with_extension("token.tmp");
    perms::create_private_file(&tmp)?;
    std::fs::write(&tmp, &token)?;
    std::fs::rename(&tmp, &layout.token)?;
    perms::make_private(&layout.token)?;
    Ok(token)
}

fn read_token(layout: &Layout) -> std::io::Result<String> {
    let text = std::fs::read_to_string(&layout.token)?;
    let t = text.trim();
    if t.is_empty() {
        return Err(std::io::Error::other("the owner token is empty"));
    }
    Ok(t.to_owned())
}

fn proof(token: &str, role: &str, client_nonce: &str, server_nonce: &str) -> String {
    auth::proof(token.as_bytes(), role, client_nonce, server_nonce, 0)
}

// --- the daemon's side ------------------------------------------------------------------------------------------

/// What the daemon answers a call with: the method and its params in, a result or an error out.
pub(crate) type Handler = dyn Fn(&str, &Value) -> Result<Value> + Send + Sync;

pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    endpoint: PathBuf,
    accept: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Closed (its sender dropped) when the accept thread ends: what `stop` waits on, for a bounded time.
    ended: Mutex<Receiver<()>>,
}

/// How long `stop` waits for the accept thread before it lets go of it.
const STOP_WAIT: Duration = Duration::from_secs(2);

struct Ctx {
    token: String,
    generation: i64,
    handler: Arc<Handler>,
    stop: Arc<AtomicBool>,
    active: AtomicUsize,
}

impl Server {
    /// Writes a fresh token, binds the socket (this daemon holds the lock, so a socket file left by a dead one is
    /// not in use and is replaced) and starts accepting.
    pub(crate) fn bind(layout: &Layout, generation: i64, handler: Arc<Handler>) -> Result<Server> {
        let token = write_token(layout)?;
        let ep = endpoint(layout);
        platform::remove_endpoint(&ep);
        let listener = platform::listen(&ep).map_err(|e| {
            Error::Io(format!(
                "kith-core: can't listen on the owner socket {}: {e}",
                ep.display()
            ))
        })?;
        // Polled, not blocked in: `stop` then only sets a flag, and nothing it does can hang (a wake-up connection to a
        // socket with a full backlog could). Windows pipes have no such mode; `stop` wakes those with a connection.
        #[cfg(unix)]
        listener
            .set_nonblocking(true)
            .map_err(|e| Error::Io(format!("kith-core: can't poll the owner socket: {e}")))?;
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx {
            token,
            generation,
            handler,
            stop: stop.clone(),
            active: AtomicUsize::new(0),
        });
        let (end_tx, ended) = mpsc::channel::<()>();
        let accept = std::thread::Builder::new()
            .name("kith-owner-accept".into())
            .spawn(move || {
                accept_loop(&listener, &ctx);
                drop(end_tx);
            })
            .map_err(|e| Error::Io(format!("kith-core: can't start the owner socket: {e}")))?;
        Ok(Server {
            stop,
            endpoint: ep,
            accept: Mutex::new(Some(accept)),
            ended: Mutex::new(ended),
        })
    }

    /// Stops accepting, removes the socket and lets the accept thread end. Connections in flight finish their line.
    /// Never waits for longer than [`STOP_WAIT`] for the accept thread, and may be called from several threads: each
    /// returns with the socket gone.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        #[cfg(not(unix))]
        {
            // The thread sits in a blocking accept: one connection of our own wakes it.
            let _ = platform::connect(&self.endpoint);
        }
        let ended = self
            .ended
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Nothing is ever sent: this returns when the thread has dropped its end, or after the wait.
        let done = !matches!(
            ended.recv_timeout(STOP_WAIT),
            Err(RecvTimeoutError::Timeout)
        );
        drop(ended);
        let handle = self.accept.lock().ok().and_then(|mut a| a.take());
        if let Some(handle) = handle {
            if done {
                let _ = handle.join();
            }
            // Otherwise it is left to end on its own; the flag is set, so it will not serve another connection.
        }
        platform::remove_endpoint(&self.endpoint);
    }
}

fn accept_loop(listener: &platform::Listener, ctx: &Arc<Ctx>) {
    loop {
        let stream = match platform::accept(listener) {
            Ok(s) => s,
            Err(e)
                if platform::accept_error_is_temporary(&e) && !ctx.stop.load(Ordering::SeqCst) =>
            {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => break,
        };
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        // BSD-derived systems hand the listener's non-blocking mode on to the connection.
        #[cfg(unix)]
        if stream.set_nonblocking(false).is_err() {
            continue;
        }
        if ctx.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("kith-owner-conn".into())
            .spawn({
                let ctx = ctx.clone();
                move || {
                    serve_connection(stream, &ctx);
                    ctx.active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

enum Phase {
    Hello,
    Auth { client_nonce: String, nonce: String },
    Ready,
}

fn serve_connection(stream: Stream, ctx: &Ctx) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut phase = Phase::Hello;
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !line.ends_with('\n') {
            break; // a line over the limit, or the peer went away in the middle of one
        }
        let Ok(req) = serde_json::from_str::<Value>(line.trim()) else {
            let _ = write_line(
                &mut writer,
                &failure(&Value::Null, -32700, "parse_error", "not JSON"),
            );
            break;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        let current = std::mem::replace(&mut phase, Phase::Hello);
        let (reply, keep) = match current {
            Phase::Hello if method == "hello" => {
                let (reply, next) = hello(ctx, &id, &params);
                match next {
                    Some(next) => {
                        phase = next;
                        (reply, true)
                    }
                    None => (reply, false),
                }
            }
            Phase::Auth {
                client_nonce,
                nonce,
            } if method == "auth" => {
                let expect = proof(&ctx.token, "cli", &client_nonce, &nonce);
                let given = params.get("proof").and_then(Value::as_str).unwrap_or("");
                if auth::same(expect.as_bytes(), given.as_bytes()) {
                    phase = Phase::Ready;
                    (success(&id, &json!({"ok": true})), true)
                } else {
                    (
                        failure(
                            &id,
                            -32003,
                            "unauthorized",
                            "the owner token does not match",
                        ),
                        false,
                    )
                }
            }
            Phase::Ready if method != "hello" && method != "auth" => {
                phase = Phase::Ready;
                let reply = match (ctx.handler)(method, &params) {
                    Ok(v) => success(&id, &v),
                    Err(e) => error_reply(&id, &e),
                };
                (reply, true)
            }
            _ => (
                failure(&id, -32002, "protocol", "hello and auth come first"),
                false,
            ),
        };
        if write_line(&mut writer, &reply).is_err() || !keep {
            break;
        }
    }
    let _ = writer.shutdown(std::net::Shutdown::Both);
}

/// A short fingerprint of one row of the table (FNV-1a over its OpenRPC entry: name, params, result, who, how it is
/// served), or `None` for a name the table does not have.
pub(crate) fn row_hash(name: &str) -> Option<&'static str> {
    static ROWS: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        let doc = kith_api::openrpc::generate();
        doc["methods"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for b in m.to_string().bytes() {
                    h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                }
                Some((m["name"].as_str()?.to_owned(), format!("{h:016x}")))
            })
            .collect()
    })
    .get(name)
    .map(String::as_str)
}

/// The rows a client that calls `method` sends: just that one, if the table has it.
fn rows_for(method: &str) -> serde_json::Map<String, Value> {
    row_hash(method)
        .map(|h| serde_json::Map::from_iter([(method.to_owned(), json!(h))]))
        .unwrap_or_default()
}

/// The rows of `wanted` this daemon lacks or shapes differently.
fn unfit(wanted: &serde_json::Map<String, Value>) -> Vec<String> {
    wanted
        .iter()
        .filter(|(name, hash)| row_hash(name) != hash.as_str())
        .map(|(name, _)| name.clone())
        .collect()
}

fn hello(ctx: &Ctx, id: &Value, params: &Value) -> (Value, Option<Phase>) {
    let client_nonce = params.get("nonce").and_then(Value::as_str).unwrap_or("");
    let nonce = platform::random_hex();
    let protocol = params.get("protocol").and_then(Value::as_u64);
    let (fits, unfit) = match params.get("rows").and_then(Value::as_object) {
        Some(rows) => {
            let unfit = unfit(rows);
            (unfit.is_empty(), unfit)
        }
        // An older client names no rows: it is held to the whole table.
        None => (
            params.get("schema").and_then(Value::as_str) == Some(kith_api::schema_hash()),
            Vec::new(),
        ),
    };
    let compatible = protocol == Some(u64::from(PROTOCOL)) && fits;
    let reply = success(
        id,
        &json!({
            "protocol": PROTOCOL,
            "version": VERSION,
            "schema": kith_api::schema_hash(),
            "generation": ctx.generation,
            "pid": std::process::id(),
            "nonce": nonce,
            "proof": proof(&ctx.token, "daemon", client_nonce, &nonce),
            "compatible": compatible,
            "unfit": unfit,
        }),
    );
    let next = (compatible && !client_nonce.is_empty()).then(|| Phase::Auth {
        client_nonce: client_nonce.to_owned(),
        nonce,
    });
    (reply, next)
}

fn success(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn failure(id: &Value, code: i64, kind: &str, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message, "data": {"kind": kind}}})
}

fn error_reply(id: &Value, e: &Error) -> Value {
    let (code, kind) = match e {
        Error::NoSuchMethod(_) => (-32601, "no_such_method"),
        Error::BadParams(_) => (-32602, "bad_params"),
        Error::NotAllowed(_) => (-32001, "not_allowed"),
        Error::NotServed(_) => (-32004, "not_served"),
        _ => (-32000, "failed"),
    };
    failure(id, code, kind, &e.to_string())
}

fn from_error(message: &str, kind: &str) -> Error {
    let m = message.to_owned();
    match kind {
        "no_such_method" => Error::NoSuchMethod(m),
        "bad_params" => Error::BadParams(m),
        "not_allowed" => Error::NotAllowed(m),
        "not_served" => Error::NotServed(m),
        _ => Error::Io(m),
    }
}

fn write_line(w: &mut Stream, v: &Value) -> std::io::Result<()> {
    let mut text = v.to_string();
    text.push('\n');
    w.write_all(text.as_bytes())?;
    w.flush()
}

// --- the CLI's side ---------------------------------------------------------------------------------------------

/// What reaching the daemon came to.
#[derive(Debug)]
pub enum Reached {
    /// The daemon ran the method.
    Answered(Value),
    /// The daemon answered with a refusal about the call itself (no such method, params that do not fit, not served
    /// by the core yet): nothing ran, and the caller does what it did before.
    Refused(Error),
    /// Nothing answers on the owner socket (no daemon, a dead socket, or no token to prove with): run embedded.
    NoDaemon,
    /// A daemon answers, but it and this program disagree on the protocol or the method table. Nothing was run; the
    /// note says so, and the caller falls back to embedded or Python.
    Mismatch(String),
    /// The call went out and the connection ended before an answer, twice for a call that may be repeated, once for
    /// one that may not. It may or may not have run: never repeated for a write.
    Lost(String),
}

enum Attempt {
    Done(Reached),
    /// Failed; `sent` says whether the call itself had gone out.
    Failed {
        sent: bool,
        why: String,
    },
}

/// Calls `method` on the daemon of `layout`. `repeatable` says the call may run twice without harm (a read): it is
/// then retried once when the connection breaks (the daemon restarting). A write is never sent a second time.
pub(crate) fn reach(layout: &Layout, method: &str, params: &Value, repeatable: bool) -> Reached {
    reach_with(
        layout,
        method,
        params,
        repeatable,
        (PROTOCOL, kith_api::schema_hash()),
        Some(rows_for(method)),
    )
}

/// [`reach`] as a client of another protocol number and method table (what a mismatch test needs to be).
pub(crate) fn reach_as(
    layout: &Layout,
    method: &str,
    params: &Value,
    repeatable: bool,
    identity: (u32, &str),
) -> Reached {
    reach_with(layout, method, params, repeatable, identity, None)
}

/// [`reach_as`] naming the rows the client may call (`None`: an older client that names none and is held to the whole
/// table's hash).
pub(crate) fn reach_with(
    layout: &Layout,
    method: &str,
    params: &Value,
    repeatable: bool,
    identity: (u32, &str),
    rows: Option<serde_json::Map<String, Value>>,
) -> Reached {
    let mut tried = 0;
    loop {
        match attempt(layout, method, params, identity, rows.as_ref()) {
            Attempt::Done(r) => return r,
            Attempt::Failed { sent, why } => {
                tried += 1;
                if sent && !repeatable {
                    return Reached::Lost(why);
                }
                if tried >= 2 {
                    return if sent {
                        Reached::Lost(why)
                    } else {
                        Reached::NoDaemon
                    };
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

struct Conn {
    writer: Stream,
    lines: Receiver<Option<String>>,
}

impl Conn {
    fn open(stream: Stream) -> std::io::Result<Conn> {
        let writer = stream.try_clone()?;
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new()
            .name("kith-owner-read".into())
            .spawn(move || {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    match (&mut reader).take(MAX_LINE).read_line(&mut line) {
                        Ok(n) if n > 0 && line.ends_with('\n') => {
                            if tx.send(Some(line)).is_err() {
                                break;
                            }
                        }
                        _ => {
                            let _ = tx.send(None);
                            break;
                        }
                    }
                }
            })?;
        Ok(Conn { writer, lines })
    }

    fn send(&mut self, id: u64, method: &str, params: &Value) -> std::io::Result<()> {
        write_line(
            &mut self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
    }

    /// The next reply, or why there is none.
    fn recv(&self, within: Duration) -> std::result::Result<Value, String> {
        match self.lines.recv_timeout(within) {
            Ok(Some(line)) => {
                serde_json::from_str(line.trim()).map_err(|_| "an unreadable reply".to_owned())
            }
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                Err("the connection closed".to_owned())
            }
            Err(RecvTimeoutError::Timeout) => Err("no answer in time".to_owned()),
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

fn attempt(
    layout: &Layout,
    method: &str,
    params: &Value,
    (protocol, schema): (u32, &str),
    rows: Option<&serde_json::Map<String, Value>>,
) -> Attempt {
    let before = |why: &str| Attempt::Failed {
        sent: false,
        why: why.to_owned(),
    };
    let Ok(stream) = platform::connect(&endpoint(layout)) else {
        return Attempt::Done(Reached::NoDaemon);
    };
    let Ok(token) = read_token(layout) else {
        return Attempt::Done(Reached::NoDaemon);
    };
    let Ok(mut conn) = Conn::open(stream) else {
        return before("can't read from the socket");
    };
    let nonce = platform::random_hex();
    let mut hello =
        json!({"protocol": protocol, "version": VERSION, "schema": schema, "nonce": nonce});
    if let Some(rows) = rows {
        hello["rows"] = json!(rows);
    }
    if conn.send(1, "hello", &hello).is_err() {
        return before("can't write to the socket");
    }
    let hello = match conn.recv(STEP_TIMEOUT) {
        Ok(v) => v,
        Err(why) => return before(&why),
    };
    let r = &hello["result"];
    let (Some(server_nonce), Some(given)) = (r["nonce"].as_str(), r["proof"].as_str()) else {
        return before("an unreadable hello");
    };
    if !auth::same(
        proof(&token, "daemon", &nonce, server_nonce).as_bytes(),
        given.as_bytes(),
    ) {
        // Not the daemon that wrote this token: a restart in between, or something else on the socket.
        return before("the daemon did not prove it knows the owner token");
    }
    if r["compatible"] != true {
        let unfit = r["unfit"]
            .as_array()
            .filter(|a| !a.is_empty())
            .map(|a| {
                let names: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                format!(", and it lacks or reads differently: {}", names.join(", "))
            })
            .unwrap_or_default();
        return Attempt::Done(Reached::Mismatch(format!(
            "the running daemon (pid {}, kith-core {}, protocol {}, method table {}) and this kith (kith-core {VERSION}, protocol {protocol}, method table {schema}) disagree{unfit}, so this command runs on its own",
            r["pid"],
            r["version"].as_str().unwrap_or("?"),
            r["protocol"],
            r["schema"].as_str().unwrap_or("?"),
        )));
    }
    if conn
        .send(
            2,
            "auth",
            &json!({"proof": proof(&token, "cli", &nonce, server_nonce)}),
        )
        .is_err()
    {
        return before("can't write to the socket");
    }
    match conn.recv(STEP_TIMEOUT) {
        Ok(v) if v["result"]["ok"] == true => {}
        Ok(_) => return before("the daemon refused the owner token"),
        Err(why) => return before(&why),
    }
    if let Err(e) = conn.send(3, method, params) {
        return Attempt::Failed {
            sent: false,
            why: format!("can't write to the socket: {e}"),
        };
    }
    match conn.recv(CALL_TIMEOUT) {
        Ok(v) => {
            if let Some(err) = v.get("error") {
                let message = err["message"]
                    .as_str()
                    .unwrap_or("the daemon refused the call");
                let kind = err["data"]["kind"].as_str().unwrap_or("");
                return Attempt::Done(Reached::Refused(from_error(message, kind)));
            }
            match v.get("result") {
                Some(result) => Attempt::Done(Reached::Answered(result.clone())),
                None => Attempt::Failed {
                    sent: true,
                    why: "an answer with no result".to_owned(),
                },
            }
        }
        Err(why) => Attempt::Failed { sent: true, why },
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
