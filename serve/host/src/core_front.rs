//! The host's side of the daemon front (docs/plans/runtime-design.md section 1; M0c): the CLI asks the daemon first, the
//! daemon starts inside `kith serve`, and `kith kernel core-daemon` / `core-status` are the two small verbs around it.
//!
//! The CLI never depends on the daemon. A verb goes to the owner socket only when the table says the core serves it and
//! a daemon answers; otherwise (no daemon, a daemon that disagrees about the method table, a connection that broke, a
//! row the core does not answer yet, a command line the table does not spell) it runs exactly as it did before.
//! `kernel::route` has already sent the command line here; clap still parses everything this module declines.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use crate::kernel;

/// The table rows the daemon answers today (`Core::call`). Any other row is `NotServed` there, and asking would only
/// cost a round trip.
const DAEMON_ROWS: &[&str] = &["paths", "kernel.version"];

/// Reads a command line against the table and, if the daemon serves it and answers, prints its answer.
/// `Some(exit code)` when the command was handled; `None` when the caller carries on as before. `args[0]` is the
/// program label the router put in front.
pub fn try_cli(args: &[OsString]) -> Option<i32> {
    let words: Vec<&str> = args.iter().skip(1).map(|a| a.to_str()).collect::<Option<_>>()?;
    // A usage error is clap's to report (and, for a row Python still serves, never reaches here at all).
    let parsed = kith_api::cli::parse_argv(&words).ok()?;
    if !DAEMON_ROWS.contains(&parsed.method.name) {
        return None;
    }
    let home = kith_kernel::paths::process_home();
    let folders = kernel::folders(&home);
    let params = caller_view(parsed.method.name, parsed.params, &home);
    match kith_core::reach(&folders, parsed.method.name, &params, true) {
        kith_core::Reached::Answered(answer) => print_answer(parsed.method.name, &answer, parsed.format.as_deref()),
        kith_core::Reached::Mismatch(note) => {
            eprintln!("kith: {note}");
            None
        }
        _ => None,
    }
}

/// The caller's view of the environment, for the methods that read it: the daemon's own environment is not the one
/// this process has (a hook or an agent run has its own KITH_HOME, HOME, KITH_STATE ...).
fn caller_view(method: &str, mut params: Value, home: &std::path::Path) -> Value {
    if method == "paths" {
        let env: serde_json::Map<String, Value> = std::env::vars()
            .filter(|(k, _)| k.starts_with("KITH_") || k.starts_with("HQ_"))
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let object = params.as_object_mut().expect("parsed params are an object");
        if !object.contains_key("kernel") {
            let kernel: PathBuf = kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
            object.insert("kernel".into(), json!(kernel.to_string_lossy()));
        }
        object.insert("home".into(), json!(home.to_string_lossy()));
        object.insert("env".into(), Value::Object(env));
    }
    params
}

/// Prints what the embedded verb prints, from the daemon's answer.
fn print_answer(method: &str, answer: &Value, format: Option<&str>) -> Option<i32> {
    let out = match method {
        "kernel.version" => format!("{answer}\n"),
        _ => {
            let folders = folders_from(answer)?;
            match format {
                Some("--json") => format!("{}\n", folders.json()),
                Some("--sh") => folders.sh(),
                _ => folders.text(),
            }
        }
    };
    let mut stdout = std::io::stdout().lock();
    (stdout.write_all(out.as_bytes()).is_ok() && stdout.flush().is_ok()).then_some(0)
}

fn folders_from(answer: &Value) -> Option<Paths> {
    let get = |k: &str| answer.get(k)?.as_str().map(PathBuf::from);
    Some(Paths {
        home: get("KITH_HOME")?,
        state: get("KITH_STATE")?,
        agents: get("KITH_AGENTS")?,
        home_real: get("KITH_HOME_REAL")?,
        state_real: get("KITH_STATE_REAL")?,
        agents_real: get("KITH_AGENTS_REAL")?,
        machine: get("KITH_MACHINE")?,
        secrets: get("KITH_SECRETS")?,
        logs: get("KITH_LOGS")?,
        cache: get("KITH_CACHE")?,
        store: get("KITH_STORE")?,
    })
}

/// `kith serve` has the daemon front start beside the host. It never waits for it and never fails on it: a core that
/// will not start is a line in the core log and a status for `kith doctor`, and the host goes on.
pub fn start_beside_host() -> kith_core::Daemon {
    kith_core::Daemon::spawn(&kernel::folders(&kith_kernel::paths::process_home()))
}

/// `kith kernel core-daemon`: the daemon front alone, in the foreground. Prints `ready` once it serves and
/// `served=N` when stdin closes, which is how it is stopped. The difftest and the end-to-end tests run it.
pub fn run_foreground() -> i32 {
    let daemon = kith_core::Daemon::spawn(&kernel::folders(&kith_kernel::paths::process_home()));
    if let Err(e) = daemon.ready(std::time::Duration::from_secs(120)) {
        eprintln!("{e}\nrecovery: {}", e.recovery());
        return 1;
    }
    println!("ready");
    let _ = std::io::stdout().flush();
    let mut sink = Vec::new();
    let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
    let served = daemon.served();
    daemon.stop();
    println!("served={served}");
    0
}

/// `kith kernel core-status [--json]`: the daemon, its socket, the holder, and when there is no core the verb that
/// recovers it. Reads no database and changes nothing.
pub fn print_status(json: bool) {
    let status = kith_core::status(&kernel::folders(&kith_kernel::paths::process_home()));
    if json {
        println!("{status}");
        return;
    }
    let text = |v: &Value| v.as_str().unwrap_or("-").to_owned();
    println!("core:   {} ({})", text(&status["core"]["state"]), text(&status["core"]["note"]));
    if status["daemon"]["running"] == true {
        println!(
            "daemon: pid {}, generation {}, socket {}",
            status["daemon"]["pid"], status["daemon"]["generation"], text(&status["daemon"]["socket"])
        );
    }
    if status["holder"].is_object() {
        println!(
            "holder: {} (instance {}, tied to starter: {})",
            text(&status["holder"]["state"]),
            text(&status["holder"]["instance"]),
            status["holder"]["tied_to_starter"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<OsString> {
        std::iter::once("kith serve").chain(line.split(' ')).map(OsString::from).collect()
    }

    #[test]
    fn only_rows_the_daemon_answers_are_asked() {
        // Nothing listens in a test, so a verb it would ask for comes back declined, like any other; the point is that
        // these lines parse to the rows the daemon is asked for and the rest never leave the process.
        for (line, row) in [("paths --json", "paths"), ("kernel paths --sh", "paths"), ("kernel version", "kernel.version")] {
            let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
            assert_eq!(kith_api::cli::parse_argv(&words).expect("parses").method.name, row);
        }
        for line in ["kernel engines --json", "kernel launch prompt echo", "ls --json", "frobnicate", "paths --bogus"] {
            assert_eq!(try_cli(&args(line)), None, "{line}");
        }
    }

    #[test]
    fn the_caller_view_carries_home_kernel_and_only_kith_variables() {
        let home = PathBuf::from("/h");
        let v = caller_view("paths", json!({}), &home);
        assert_eq!(v["home"], "/h");
        assert!(v["kernel"].is_string());
        for k in v["env"].as_object().expect("env").keys() {
            assert!(k.starts_with("KITH_") || k.starts_with("HQ_"), "{k}");
        }
        // The kernel the caller named stays; other methods get nothing added.
        assert_eq!(caller_view("paths", json!({"kernel": "/k"}), &home)["kernel"], "/k");
        assert_eq!(caller_view("kernel.version", json!({}), &home), json!({}));
    }

    #[test]
    fn an_answer_prints_as_the_embedded_verb_does() {
        let p = Paths {
            home: "/h".into(),
            state: "/s".into(),
            agents: "/a".into(),
            home_real: "/h".into(),
            state_real: "/s".into(),
            agents_real: "/a".into(),
            machine: "/m".into(),
            secrets: "/x".into(),
            logs: "/l".into(),
            cache: "/c".into(),
            store: "/t".into(),
        };
        let answer: Value = serde_json::from_str(&p.json()).expect("json");
        assert_eq!(folders_from(&answer), Some(p));
        assert_eq!(folders_from(&json!({"KITH_HOME": "/h"})), None, "a partial answer is not printed");
    }
}
