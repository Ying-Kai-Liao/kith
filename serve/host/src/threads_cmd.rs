//! `kith kernel threads <verb>`: the writer claim and the session registry (`kernel/bin/kith-sessions`) from the
//! `kith-kernel` crate. This is the one place that asks a running host (`resolveThread` over its local endpoint, the way
//! `kith-sessions host_call` does) and the one that asks the core (`claim.*`, `session.record`, through
//! `kith kernel core-call`, the way `kith_claims.call` does), because the crate never calls either itself. `kernel/bin/kith`
//! routes its `kith-sessions` calls here while `threads` is in `lib/core-switched.txt`; the host's own claims
//! (`terminal/claim.rs`) do not go through the crate: the host never switches ahead of the CLI.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kith_kernel::hostlocal;
use kith_kernel::paths::{process_env, process_home};
use kith_kernel::threads::core::{Core, CoreErr};
use kith_kernel::threads::{self, Ctx, Host, cli};
use serde_json::{Value, json};

use crate::core_front::{self, CallEnd};
use crate::kernel;

/// The host on this Mac, if one answers on its local endpoint with the owner token.
pub(crate) struct Loopback {
    pub(crate) kernel: PathBuf,
    pub(crate) home: PathBuf,
}

impl Loopback {
    /// `(data dir, port)` of this install's host: the env wins, else `kith-serve endpoint`, else the defaults.
    fn endpoint(&self) -> (PathBuf, String) {
        let (data, port) = (process_env("KITH_SERVE_DIR"), process_env("KITH_SERVE_PORT"));
        if let (Some(d), Some(p)) = (&data, &port) {
            return (PathBuf::from(d), p.clone());
        }
        let said = std::process::Command::new(self.kernel.join("bin").join("kith-serve"))
            .arg("endpoint")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let mut words = said.as_deref().unwrap_or("").split_whitespace();
        let (d, p) = match (words.next(), words.next()) {
            (Some(d), Some(p)) => (d.to_owned(), p.to_owned()),
            _ => (self.home.join(".kith").join("serve").to_string_lossy().into_owned(), "19224".to_owned()),
        };
        (PathBuf::from(data.unwrap_or(d)), port.unwrap_or(p))
    }
}

/// One claim call: the Python gave eight seconds to find the program and eight to the call.
const CALL_SECS: u64 = 8;

impl Core for Loopback {
    /// The call `kith kernel core-call` makes, in this process and within a deadline ([`core_front::core_call_within`]): an
    /// answer, a rejection (exit 4 there) or no answer (3 and 5), as `kith_claims.call` reads them.
    fn call(&self, method: &str, params: &Value, timeout: Option<Duration>) -> Result<Value, CoreErr> {
        match core_front::core_call_within(method, params, timeout.unwrap_or(Duration::from_secs(CALL_SECS))) {
            CallEnd::Answered(v) if v.is_object() => Ok(v),
            CallEnd::Answered(_) => Err(CoreErr::NoAnswer("the answer is not a JSON object".to_owned())),
            CallEnd::Refused(why) => Err(CoreErr::Refused(if why.is_empty() { "refused".to_owned() } else { why })),
            CallEnd::NoAnswer(why, code) => {
                Err(CoreErr::NoAnswer(if why.is_empty() { format!("core-call exited {code}") } else { why }))
            }
        }
    }
}

impl Host for Loopback {
    fn resolve_thread(&self, thread: &str) -> Option<Value> {
        let (data, _) = self.endpoint();
        let token = std::fs::read_to_string(data.join("token")).ok()?.trim().to_owned();
        let (status, body) = post(&data, "resolveThread", &token, &json!({ "thread": thread }).to_string())?;
        // A refusal with a body is an answer ("unknown thread"); a 404 is an older host.
        if status == 404 {
            return Some(json!({"error": "unknown method", "unknownMethod": true}));
        }
        serde_json::from_slice(&body).ok()
    }
}

/// One POST to `/api/<method>` on the host's local endpoint (never loopback TCP), with the owner token; `(status, body)`. Four
/// seconds for each step of it. `None` when the host is down, is not this account's or is older than this command.
fn post(data: &Path, method: &str, token: &str, body: &str) -> Option<(u16, Vec<u8>)> {
    let reply = hostlocal::post(data, &format!("/api/{method}"), token, body, Duration::from_secs(4)).ok()?;
    Some((reply.status, reply.body))
}

/// `kith kernel threads <verb> ...`; returns the exit code.
pub fn run(kernel_dir: Option<PathBuf>, args: &[String]) -> i32 {
    let home = process_home();
    let kernel_dir = kernel_dir.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    // `KITH_MACHINE` first, as every `kith-sessions` lookup of the Mac's folder reads it (the switch files, the cost ledger, the
    // extra logins), else where the paths put it.
    let machine = process_env("KITH_MACHINE")
        .filter(|m| !m.is_empty())
        .map_or_else(|| kith_kernel::paths::paths(&home, &kernel_dir, &process_env).machine, PathBuf::from);
    let ctx = Ctx {
        state: process_env("KITH_STATE").map(PathBuf::from),
        machine: Some(machine),
        home: home.clone(),
        env: &process_env,
    };
    // `run AGENT -- CMD...` is the one verb that is a process and not an answer: it keeps stdio and the terminal, and ends with
    // its child's code. Anything else after `run` is `kith-sessions`' usage text, which `cli` prints.
    if let [verb, agent, dash, cmd @ ..] = args
        && verb == "run"
        && dash == "--"
        && !cmd.is_empty()
    {
        let host = Loopback { kernel: kernel_dir.clone(), home };
        return threads::run::run(&ctx, &host, Some(&host), &kernel_dir, agent, cmd, &threads::run::say);
    }
    let host = Loopback { kernel: kernel_dir, home };
    let stdin = || {
        let mut text = String::new();
        let _ = std::io::stdin().read_to_string(&mut text);
        text
    };
    // `pick` shows its table and prompt at once and reads one line (a terminal's stdin never ends); `record` reads the whole
    // of the hook's.
    let ask = |prompt: &str| {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut text = String::new();
        let _ = std::io::stdin().read_line(&mut text);
        text
    };
    let mut out = cli::run_with(&ctx, &host, Some(&host), args, &stdin, &ask);
    // `run` without a command ends in the Python's `sys.exit(__doc__)`, which writes the text and a newline of its own.
    if args.first().is_some_and(|a| a == "run") {
        out.stderr.push('\n');
    }
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}
