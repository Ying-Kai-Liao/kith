//! `kith kernel ask ARGS...`: `kith ask` as a client of the core's ask rows (`kith-kernel`'s `ask` module), with no Python. Like
//! `wake_cmd`, this is the one place that reaches the core, because the crate never does: each call is `core-call`'s own, in this
//! process (`core_front::core_call_within`). `kernel/bin/kith-ask` forwards here only when this program lists `ask-ledger` and the
//! `asks` module is switched on the Mac; before that kith-ask answers itself from its files.
//!
//! What needs an OS call the kernel crate does not make (it denies `unsafe`) is here: the SIGTERM a foreground ask turns into the end
//! of its row, and the settings write behind `--cap N`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kith_kernel::ask::{Host, run};
use kith_kernel::paths::process_env;
use kith_kernel::wake::{Call, Fail};
use serde_json::Value;

use crate::core_front::{self, CallEnd};
use crate::kernel;

/// How long one call may take: the Python had no deadline, but `ask.open` and `ask.finish` are one transaction each.
const CALL_SECS: u64 = 60;

static TERM: AtomicBool = AtomicBool::new(false);

fn call(method: &str, params: &Value) -> Result<Value, Fail> {
    match core_front::core_call_within(method, params, Duration::from_secs(CALL_SECS)) {
        CallEnd::Answered(v) if v.is_object() => Ok(v),
        CallEnd::Answered(_) => Err(Fail::NoAnswer("the answer is not a JSON object".to_owned())),
        CallEnd::Refused(why) => Err(Fail::Refused(if why.is_empty() { "refused".to_owned() } else { why })),
        CallEnd::NoAnswer(why, code) => {
            Err(Fail::NoAnswer(if why.is_empty() { format!("core-call exited {code}") } else { why }))
        }
    }
}

#[cfg(unix)]
extern "C" fn on_term(_signal: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

/// SIGTERM (the helper wait's, or a stop) and SIGINT end a foreground ask: the loop that waits for the callee sees the flag, kills the
/// callee and ends the row. Windows has no signal to take here (its console handler comes with its launch).
fn watch_term() {
    #[cfg(unix)]
    {
        let handler = on_term as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler only stores into an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGINT, handler);
        }
    }
}

/// `kith kernel settings ARGS...`, in this process.
#[cfg(unix)]
fn settings(kernel: &std::path::Path, home: &std::path::Path, args: &[String]) -> (i32, String, String) {
    use kith_kernel::accounts::{Ctx, System};
    let p = kith_kernel::paths::paths(home, kernel, &process_env);
    let system = System { kernel: kernel.to_path_buf() };
    let ctx = Ctx {
        machine: p.machine.clone(),
        agents: p.agents.clone(),
        kernel: kernel.to_path_buf(),
        home: home.to_path_buf(),
        claude_config_dir: std::env::var("CLAUDE_CONFIG_DIR").ok(),
        out: &system,
    };
    let out = kith_kernel::settings::run(&ctx, kith_kernel::settings::Env::from_process(), args);
    (out.code, out.stdout, out.stderr)
}

#[cfg(not(unix))]
fn settings(_: &std::path::Path, _: &std::path::Path, _: &[String]) -> (i32, String, String) {
    (1, String::new(), "kith: setting the cap is not on Windows yet (windows-owner W5a)\n".to_owned())
}

/// `kith kernel ask [--kernel DIR] ARGS...`; returns the exit code.
pub fn main(kernel_dir: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    let home = kith_kernel::paths::process_home();
    let kernel_dir = kernel_dir.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let p = kith_kernel::paths::paths(&home, &kernel_dir, &process_env);
    let c: &Call = &call;
    let terminated = || TERM.load(Ordering::SeqCst);
    let settings_of = |a: &[String]| settings(&kernel_dir, &home, a);
    let host = Host {
        call: c,
        kernel: kernel_dir.clone(),
        home: home.clone(),
        state: p.state.clone(),
        machine: p.machine.clone(),
        env: std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect(),
        terminated: &terminated,
        watch_term: &watch_term,
        settings: &settings_of,
    };
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
    run(&host, args, &mut out, &mut err)
}
