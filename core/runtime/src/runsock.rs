//! The run socket (M0e): the one core socket a guest run may reach. A run holds a token (`runs.rs`) and asks for the
//! few things a guest is allowed: `recall`, `memorySearch` and `projectId`, the same subset host.sock serves today.
//! host.sock is NOT switched over to this: both exist, nothing in the host or the kernel uses this one yet, and the
//! guest sandbox still denies the whole `core/` folder (so a guest cannot reach it from inside its sandbox until a
//! later step allows exactly this path).
//!
//! The socket comes from `kith-holder`'s platform layer like the owner socket (a Unix socket, 0600, in the 0700 core
//! folder, with the same-user peer check; on Windows a named pipe whose access list names this account and SYSTEM).
//! Unlike the owner socket there is no hello and no proof: one request per connection, a JSON line
//! `{"token", "method", "args"}`, answered by one JSON line `{"ok": true, ...}` or `{"ok": false, "error"}`. Who is
//! asking is the token's and the connecting process's, never anything in the request.
//!
//! The call goes through the method table with the run-token principal (`kith_api::Principal::RunToken`), as
//! `run.recall`, `run.memorySearch` and `run.projectId`.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use kith_holder::platform::{self, Stream};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::runs::{MAX_REQUEST, RunIdentity, RunTokens, memory_args, project_args, recall_args};

const MAX_CONNECTIONS: usize = 64;
const READ_BUDGET: Duration = Duration::from_secs(5);

/// Where the daemon of this layout listens for runs: next to the owner socket.
#[cfg(unix)]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    layout.dir.join("run.sock")
}

#[cfg(not(unix))]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    crate::owner::pipe_name(layout, "run")
}

/// What answers an authorized request: the run, the table row and its params in, a result out.
pub(crate) type RunHandler = dyn Fn(&RunIdentity, &str, &Value) -> Result<Value> + Send + Sync;

/// A request that passed every check: whose it is, which row it calls and the narrowed arguments.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Authorized {
    pub identity: RunIdentity,
    pub row: &'static str,
    pub args: Vec<String>,
}

/// Checks a request line without running anything. Every token failure reads the same to the caller, so a probe learns
/// nothing about which tokens exist.
pub(crate) fn authorize(
    tokens: &RunTokens,
    line: &[u8],
    peer: Option<u32>,
) -> std::result::Result<Authorized, String> {
    let refused = |why: &str| Err(why.to_owned());
    if line.len() > MAX_REQUEST {
        return refused("request too large");
    }
    let Ok(req) = serde_json::from_slice::<Value>(line) else {
        return refused("malformed request");
    };
    let (Some(token), Some(method)) = (req["token"].as_str(), req["method"].as_str()) else {
        return refused("malformed request");
    };
    let Some(identity) = tokens.resolve(token, peer) else {
        return refused("not authorized");
    };
    let given = req["args"].as_array().map_or(&[][..], Vec::as_slice);
    let (row, args) = match method {
        "recall" => ("run.recall", recall_args(given)),
        "memorySearch" => ("run.memorySearch", memory_args(given)),
        "projectId" => (
            "run.projectId",
            project_args(given, identity.project.as_deref()),
        ),
        _ => return refused("unknown method"),
    };
    let args = args.map_err(str::to_owned)?;
    Ok(Authorized {
        identity,
        row,
        args,
    })
}

/// The answer line for a request.
fn answer(tokens: &RunTokens, handler: &RunHandler, line: &[u8], peer: Option<u32>) -> Value {
    let job = match authorize(tokens, line, peer) {
        Ok(job) => job,
        Err(why) => return json!({"ok": false, "error": why}),
    };
    match handler(&job.identity, job.row, &json!({"args": job.args})) {
        Ok(Value::Object(mut fields)) => {
            fields.insert("ok".into(), Value::Bool(true));
            Value::Object(fields)
        }
        Ok(other) => json!({"ok": true, "result": other}),
        Err(Error::NotServed(_)) => {
            json!({"ok": false, "error": "that isn't answered by the core yet"})
        }
        Err(Error::NotAllowed(_)) => json!({"ok": false, "error": "not authorized"}),
        Err(Error::BadParams(_)) => json!({"ok": false, "error": "bad request"}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    endpoint: PathBuf,
    accept: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Ctx {
    tokens: Arc<RunTokens>,
    handler: Arc<RunHandler>,
    stop: Arc<AtomicBool>,
    active: AtomicUsize,
}

impl Server {
    /// Binds the socket (this daemon holds the lock, so a socket file left by a dead one is not in use and is
    /// replaced) and starts accepting.
    pub(crate) fn bind(
        layout: &Layout,
        tokens: Arc<RunTokens>,
        handler: Arc<RunHandler>,
    ) -> Result<Server> {
        let ep = endpoint(layout);
        platform::remove_endpoint(&ep);
        let listener = platform::listen(&ep).map_err(|e| {
            Error::Io(format!(
                "kith-core: can't listen on the run socket {}: {e}",
                ep.display()
            ))
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx {
            tokens,
            handler,
            stop: stop.clone(),
            active: AtomicUsize::new(0),
        });
        let accept = std::thread::Builder::new()
            .name("kith-run-accept".into())
            .spawn(move || accept_loop(&listener, &ctx))
            .map_err(|e| Error::Io(format!("kith-core: can't start the run socket: {e}")))?;
        Ok(Server {
            stop,
            endpoint: ep,
            accept: Mutex::new(Some(accept)),
        })
    }

    /// Stops accepting and removes the socket. Requests in flight finish their line.
    pub(crate) fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // The thread sits in accept: one connection of our own wakes it.
        let _ = platform::connect(&self.endpoint);
        if let Some(handle) = self.accept.lock().ok().and_then(|mut a| a.take()) {
            let _ = handle.join();
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
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => break,
        };
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        if ctx.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let spawned = std::thread::Builder::new()
            .name("kith-run-conn".into())
            .spawn({
                let ctx = ctx.clone();
                move || {
                    serve_one(stream, &ctx);
                    ctx.active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn serve_one(stream: Stream, ctx: &Ctx) {
    let peer = platform::peer_pid(&stream);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    // The request has a time to arrive in: a peer that sends nothing is cut off.
    let (done, waiting) = mpsc::channel::<()>();
    if let Ok(cutter) = stream.try_clone() {
        let _ = std::thread::Builder::new()
            .name("kith-run-budget".into())
            .spawn(move || {
                if waiting.recv_timeout(READ_BUDGET) == Err(mpsc::RecvTimeoutError::Timeout) {
                    let _ = cutter.shutdown(std::net::Shutdown::Both);
                }
            });
    }
    let mut line = Vec::new();
    // One byte past the cap tells an oversized line from one that is exactly at it.
    let read = BufReader::new(stream.take(MAX_REQUEST as u64 + 1)).read_until(b'\n', &mut line);
    let _ = done.send(());
    let reply = match read {
        Ok(n) if n > 0 => {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            answer(&ctx.tokens, &*ctx.handler, &line, peer)
        }
        _ => json!({"ok": false, "error": "malformed request"}),
    };
    let _ = writer.write_all(format!("{reply}\n").as_bytes());
    let _ = writer.flush();
    let _ = writer.shutdown(std::net::Shutdown::Both);
}
