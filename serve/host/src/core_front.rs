//! The host's side of the daemon front (docs/plans/runtime-design.md section 1; M0c): the CLI asks the daemon first, the
//! daemon starts inside `kith serve`, and `kith kernel core-daemon` / `core-status` are the two small verbs around it.
//!
//! The CLI never depends on the daemon. A verb goes to the owner socket only when the table says the core serves it and
//! a daemon answers; otherwise (no daemon, a daemon that lacks a row this verb calls or reads it differently, a connection that broke, a
//! row the core does not answer yet, a command line the table does not spell) it runs exactly as it did before.
//! `kernel::route` has already sent the command line here; clap still parses everything this module declines.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use crate::kernel;

/// The table rows the daemon answers today (`Core::call`). Any other row is `NotServed` there, and asking would only
/// cost a round trip. `kernel.version` is not one: the bash router asks `kith kernel version` of ONE binary (which
/// engines it has) before it execs that binary, and a daemon from another build would answer for itself.
const DAEMON_ROWS: &[&str] = &["paths"];

/// The variables `paths` reads (kith-kernel `paths::getenv`: `KITH_<name>`, else `HQ_<name>`). The caller sends these
/// and nothing else, so no token or other secret in its environment reaches the daemon.
const PATHS_VARS: &[&str] = &["KITH_STATE", "HQ_STATE", "KITH_HOME", "HQ_HOME", "KITH_AGENTS", "HQ_AGENTS"];

/// The holder's folder and secret file for this machine (made if missing), for the service job that makes the holder the
/// supervised root. The service layer asks here so `kith_core` is named only in this file and `main.rs`.
pub fn holder_files(folders: &Paths) -> anyhow::Result<(PathBuf, PathBuf)> {
    Ok(kith_core::holder_files(folders)?)
}

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
    // None: a path that is not UTF-8 can't be sent as JSON without changing it, so the verb runs embedded.
    let params = caller_view(parsed.method.name, parsed.params, &home)?;
    match kith_core::reach(&folders, parsed.method.name, &params, true) {
        kith_core::Reached::Answered(answer) => print_answer(parsed.method.name, &answer, parsed.format.as_deref()),
        // Said once, in the core log: a stderr line on every `kith paths` would end up in scripts' output.
        kith_core::Reached::Mismatch(note) => {
            kith_core::note_once(&folders, &note);
            None
        }
        _ => None,
    }
}

/// The variables `paths` reads, as this process has them. `var_os`: a variable that is not UTF-8 must not stop
/// `kith paths`; one paths reads is skipped if it is.
pub fn caller_env() -> serde_json::Map<String, Value> {
    PATHS_VARS
        .iter()
        .filter_map(|k| Some(((*k).to_owned(), Value::String(std::env::var_os(k)?.into_string().ok()?))))
        .collect()
}

/// One read row asked of the daemon by the host itself (the event filter's authority questions, M2-15). `Some(answer)`
/// only when a daemon answered; no daemon, a refusal, a mismatch or a lost connection are all `None`, which the asker
/// must read as "no" for anything that grants.
pub fn ask(folders: &Paths, method: &str, params: &Value) -> Option<Value> {
    match kith_core::reach(folders, method, params, true) {
        kith_core::Reached::Answered(answer) => Some(answer),
        _ => None,
    }
}

/// A daemon started for a test of a caller that asks it (`ask`).
#[cfg(test)]
pub struct TestDaemon(kith_core::Daemon);

#[cfg(test)]
impl TestDaemon {
    pub fn stop(self) {
        self.0.stop();
    }
}

/// Starts a daemon on `folders`, ready before it returns.
#[cfg(test)]
pub fn serve_for_test(folders: &Paths) -> TestDaemon {
    let d = kith_core::Daemon::spawn(folders);
    d.ready(std::time::Duration::from_secs(30)).expect("the daemon serves");
    TestDaemon(d)
}

/// The caller's view of the environment, for the methods that read it: the daemon's own environment is not the one
/// this process has (a hook or an agent run has its own `KITH_HOME`, `HOME`, `KITH_STATE` ...). `None` when the home or
/// the kernel folder is not UTF-8: the daemon would be told another path than the caller has.
fn caller_view(method: &str, mut params: Value, home: &std::path::Path) -> Option<Value> {
    if method == "paths" {
        let env = caller_env();
        let object = params.as_object_mut().expect("parsed params are an object");
        if !object.contains_key("kernel") {
            let kernel: PathBuf = kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
            object.insert("kernel".into(), json!(kernel.to_str()?));
        }
        object.insert("home".into(), json!(home.to_str()?));
        object.insert("env".into(), Value::Object(env));
    }
    Some(params)
}

/// Prints what the embedded verb prints, from the daemon's answer.
fn print_answer(method: &str, answer: &Value, format: Option<&str>) -> Option<i32> {
    let out = if method == "kernel.version" {
        format!("{answer}\n")
    } else {
        let folders = folders_from(answer)?;
        match format {
            Some("--json") => format!("{}\n", folders.json()),
            Some("--sh") => folders.sh(),
            _ => folders.text(),
        }
    };
    // A failed write is the end of it (exit 1): running the embedded verb after a partial print would print twice.
    let mut stdout = std::io::stdout().lock();
    Some(i32::from(!(stdout.write_all(out.as_bytes()).is_ok() && stdout.flush().is_ok())))
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

/// A `secrets.*` row on the owner socket, for the host's vault (M2-7): the host may not name the core elsewhere, so
/// this is its one door to the envelope. Waits up to `wait` for a daemon that is still starting; every other outcome
/// (no daemon, a daemon of another version, a lost connection, a refusal such as a locked key store) is an error whose
/// text says why. The rows are idempotent, so a lost connection is retried once inside `reach`.
#[cfg_attr(test, allow(dead_code, reason = "tests use the in-memory link"))]
pub fn secrets_call(method: &str, params: &Value, wait: std::time::Duration) -> anyhow::Result<Value> {
    secrets_call_at(&kernel::folders(&kith_kernel::paths::process_home()), method, params, wait)
}

#[cfg_attr(test, allow(dead_code, reason = "tests use the in-memory link"))]
fn secrets_call_at(folders: &Paths, method: &str, params: &Value, wait: std::time::Duration) -> anyhow::Result<Value> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match kith_core::reach(folders, method, params, true) {
            kith_core::Reached::Answered(answer) => return Ok(answer),
            kith_core::Reached::Refused(e) => anyhow::bail!("{e}"),
            kith_core::Reached::NoDaemon if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            kith_core::Reached::NoDaemon => {
                let status = kith_core::status(folders);
                let why = |k: &str| status["core"][k].as_str().unwrap_or("-").to_owned();
                anyhow::bail!(
                    "the core is not running ({}: {}; kith doctor says more; secrets need it)",
                    why("state"),
                    why("note")
                )
            }
            kith_core::Reached::Mismatch(note) => anyhow::bail!("the running core does not fit this host: {note}"),
            kith_core::Reached::Lost(why) => anyhow::bail!("the connection to the core ended: {why}"),
        }
    }
}

/// Whether the envelope holds `name`, for the paths that decide whether a secret must be unlocked (a bot starting).
/// Unlike [`secrets_call`] it never reads "cannot ask" as "no": it waits up to `wait` while the daemon is not yet
/// answering (starting, or a command holding the core) and then fails, with the reason. The one honest "no" without an
/// answer is a computer with no core database at all: nothing was ever sealed there.
#[cfg_attr(test, allow(dead_code, reason = "tests use the in-memory link"))]
pub fn secrets_has_waiting(name: &str, wait: std::time::Duration) -> anyhow::Result<bool> {
    secrets_has_at(&kernel::folders(&kith_kernel::paths::process_home()), name, wait)
}

#[cfg_attr(test, allow(dead_code, reason = "tests use the in-memory link"))]
fn secrets_has_at(folders: &Paths, name: &str, wait: std::time::Duration) -> anyhow::Result<bool> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match kith_core::reach(folders, "secrets.has", &json!({"name": name}), true) {
            kith_core::Reached::Answered(answer) => return Ok(answer["has"] == true),
            kith_core::Reached::NoDaemon => {
                let status = kith_core::status(folders);
                let state = status["core"]["state"].as_str().unwrap_or("-");
                if state == "no-core" {
                    return Ok(false);
                }
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "the core is not running ({state}: {}; kith doctor says more; secrets need it)",
                        status["core"]["note"].as_str().unwrap_or("-")
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            kith_core::Reached::Refused(e) => anyhow::bail!("{e}"),
            kith_core::Reached::Mismatch(note) => anyhow::bail!("the running core does not fit this host: {note}"),
            kith_core::Reached::Lost(why) => anyhow::bail!("the connection to the core ended: {why}"),
        }
    }
}

// --- run tokens (M2-13) -----------------------------------------------------------------------------------------------

/// Set to `1` (or `true`) to have the host get a guest run's token from the core daemon (`run.mint` / `run.bind` /
/// `run.end` on the owner socket) instead of its own table. Off by default: a token the core minted works on the core's
/// run.sock, which a guest's sandbox does not allow yet, so a guest run holding one reads nothing. It is for the step
/// that switches host.sock, and for trying the path on a machine where that does not matter.
pub const RUN_TOKENS_VIA_CORE: &str = "KITH_RUN_TOKENS_VIA_CORE";

/// Whether [`RUN_TOKENS_VIA_CORE`] is on.
pub fn run_tokens_via_core() -> bool {
    switch_is_on(std::env::var(RUN_TOKENS_VIA_CORE).ok().as_deref())
}

fn switch_is_on(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true"))
}

/// What a run needs to be minted. An empty room or project is left out.
pub struct RunWho<'a> {
    pub person: &'a str,
    pub agent: &'a str,
    pub conversation: &'a str,
    pub room: Option<&'a str>,
    pub project: Option<&'a str>,
}

/// A run token the core minted. The token is shown once, here, and goes into the run's environment; Debug leaves it out.
pub struct CoreRun {
    pub run_id: String,
    pub token: String,
    /// The core's run socket, as the daemon says it.
    pub socket: PathBuf,
    paths: Paths,
}

impl std::fmt::Debug for CoreRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreRun").field("run_id", &self.run_id).finish_non_exhaustive()
    }
}

/// What asking the core to mint came to.
#[derive(Debug)]
pub enum Minted {
    Run(Box<CoreRun>),
    /// The core answered and said no (a revoked person): the run gets no token, and the host's own table is NOT asked
    /// in its place.
    Refused,
    /// No daemon answers, or it cannot mint (an older one, a connection that broke). With the switch on the caller
    /// gives the run no token (the core is the authority); only with it off does the host mint in its own table.
    Unavailable,
}

/// The folders of this process's view, for the run-token calls.
pub fn process_folders() -> Paths {
    kernel::folders(&kith_kernel::paths::process_home())
}

pub fn mint_run(who: &RunWho<'_>) -> Minted {
    mint_run_in(&process_folders(), who)
}

pub fn mint_run_in(paths: &Paths, who: &RunWho<'_>) -> Minted {
    let mut params = json!({"agent": who.agent, "person": who.person, "conversation": who.conversation});
    for (k, v) in [("room", who.room), ("project", who.project)] {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            params[k] = json!(v);
        }
    }
    // A mint is a write: never sent twice.
    match kith_core::reach(paths, "run.mint", &params, false) {
        kith_core::Reached::Answered(a) => {
            let text = |k: &str| a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned);
            match (text("runId"), text("token"), text("socket")) {
                (Some(run_id), Some(token), Some(socket)) => Minted::Run(Box::new(CoreRun {
                    run_id,
                    token,
                    socket: PathBuf::from(socket),
                    paths: paths.clone(),
                })),
                _ => Minted::Unavailable,
            }
        }
        kith_core::Reached::Refused(kith_core::Error::NotAllowed(_)) => Minted::Refused,
        _ => Minted::Unavailable,
    }
}

impl CoreRun {
    /// Binds the run to the process it was started as. False when the core did not bind it (the token then resolves
    /// for nobody).
    pub fn bind(&self, pid: u32) -> bool {
        let answer = kith_core::reach(&self.paths, "run.bind", &json!({"runId": self.run_id, "pid": pid}), false);
        matches!(answer, kith_core::Reached::Answered(a) if a["bound"] == true)
    }

    /// Ends the run's token. Nothing to do when the core is gone: a restart ended it already.
    pub fn end(&self) {
        let _ = kith_core::reach(&self.paths, "run.end", &json!({"runId": self.run_id}), false);
    }
}

/// The methods `kith kernel core-call` may reach: the held-terminal ones the bash `kith` needs (M1-7).
const CORE_CALL_METHODS: &[&str] = &["terminal.open", "terminal.attach", "terminal.list"];

/// `kith kernel core-call METHOD --params-stdin`: one owner-socket call, the JSON answer on stdout. 0 answered, 3 no daemon
/// answers (nothing ran, the caller falls back), 4 refused, 5 lost after sending, 2 usage.
pub fn core_call(method: &str, params_stdin: bool) -> i32 {
    if !CORE_CALL_METHODS.contains(&method) {
        eprintln!("core-call: {method} is not one of {}", CORE_CALL_METHODS.join(", "));
        return 2;
    }
    if !params_stdin {
        eprintln!("core-call: the params are read from stdin: pass --params-stdin");
        return 2;
    }
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).is_err() {
        eprintln!("core-call: params on stdin are not text");
        return 2;
    }
    let params: Value =
        if text.trim().is_empty() { json!({}) } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
    if !params.is_object() {
        eprintln!("core-call: params must be a JSON object");
        return 2;
    }
    let folders = kernel::folders(&kith_kernel::paths::process_home());
    // Only a list is repeated when the connection breaks: an open or a ticket may have run.
    match kith_core::reach(&folders, method, &params, method == "terminal.list") {
        kith_core::Reached::Answered(answer) => {
            println!("{answer}");
            0
        }
        kith_core::Reached::Refused(e) => {
            eprintln!("{e}");
            4
        }
        kith_core::Reached::NoDaemon => {
            eprintln!("no core daemon answers");
            3
        }
        kith_core::Reached::Mismatch(note) => {
            eprintln!("{note}");
            3
        }
        kith_core::Reached::Lost(note) => {
            eprintln!("{note}");
            5
        }
    }
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
/// The running core daemon's pid, from its status (no database read). None when there is no core.
pub fn daemon_pid() -> Option<u32> {
    let status = kith_core::status(&kernel::folders(&kith_kernel::paths::process_home()));
    status["daemon"]["pid"].as_u64().and_then(|p| u32::try_from(p).ok())
}

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
            status["daemon"]["pid"],
            status["daemon"]["generation"],
            text(&status["daemon"]["socket"])
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
    use std::path::Path;

    fn args(line: &str) -> Vec<OsString> {
        std::iter::once("kith serve").chain(line.split(' ')).map(OsString::from).collect()
    }

    #[test]
    fn the_run_token_switch_is_off_unless_asked() {
        for off in [None, Some(""), Some("0"), Some("yes"), Some("false")] {
            assert!(!switch_is_on(off), "{off:?}");
        }
        assert!(switch_is_on(Some("1")) && switch_is_on(Some("true")));
    }

    /// A short folder: a Unix socket path holds about a hundred bytes.
    #[cfg(unix)]
    fn machine_paths(tag: &str) -> Paths {
        let home = PathBuf::from("/tmp").join(format!("kcf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("kith")).expect("home");
        let home = home.canonicalize().expect("canonical");
        let kith_home = home.join("kith");
        let lookup = move |k: &str| (k == "KITH_HOME").then(|| kith_home.to_string_lossy().into_owned());
        kith_kernel::paths::paths(&home, &home.join("kith/kernel"), &lookup)
    }

    #[cfg(unix)]
    fn who(person: &str) -> RunWho<'_> {
        RunWho { person, agent: "echo", conversation: "run:g:3:sam:t1", room: Some("g1"), project: None }
    }

    /// The host mints through a real core daemon, binds and ends there, is refused for a revoked person, and where no
    /// daemon answers it says so (the caller then gives the run no token).
    #[cfg(unix)]
    #[test]
    fn the_host_mints_through_a_real_daemon_and_falls_back_when_there_is_none() {
        let paths = machine_paths("mint");
        // Nothing listens yet.
        assert!(matches!(mint_run_in(&paths, &who("sam")), Minted::Unavailable));

        let daemon = kith_core::Daemon::spawn(&paths);
        daemon.ready(std::time::Duration::from_secs(20)).expect("serving");
        let owner = |method: &str, params: Value| match kith_core::reach(&paths, method, &params, false) {
            kith_core::Reached::Answered(v) => v,
            other => panic!("{method}: {other:?}"),
        };
        owner("people.addPerson", json!({"id": "sam"}));

        let Minted::Run(run) = mint_run_in(&paths, &who("sam")) else { panic!("the daemon mints") };
        assert!(run.socket.ends_with("run.sock"), "{:?}", run.socket);
        assert!(run.socket.exists(), "the core's run socket is there");
        assert_eq!(run.token.len(), 64);
        assert!(!format!("{run:?}").contains(&run.token), "Debug keeps the token out");
        let run_id = run.run_id.clone();
        // A grant over it: the run's environment names the core's socket, bind goes to the core, drop ends the token.
        let socket = run.socket.clone();
        let grant = crate::runauth::RunGrant::from_core(run);
        let env = grant.env(Path::new("/not/used"));
        assert_eq!(env[1], (crate::runauth::SOCKET_ENV.to_owned(), socket.to_string_lossy().into_owned()));
        grant.bind_pid(std::process::id());
        drop(grant);
        assert_eq!(owner("run.end", json!({"runId": run_id}))["ended"], false, "the grant ended it on drop");

        // A revoked person gets no token, and that is not "no daemon".
        owner("people.suspend", json!({"id": "sam"}));
        assert!(matches!(mint_run_in(&paths, &who("sam")), Minted::Refused));
        owner("people.undo", json!({"id": "sam"}));
        assert!(matches!(mint_run_in(&paths, &who("sam")), Minted::Run(_)));

        daemon.stop();
        assert!(matches!(mint_run_in(&paths, &who("sam")), Minted::Unavailable), "a stopped daemon is unavailable");
        let _ = std::fs::remove_dir_all(paths.home.parent().unwrap_or(&paths.home));
    }

    #[test]
    fn only_rows_the_daemon_answers_are_asked() {
        // Nothing listens in a test, so a verb it would ask for comes back declined, like any other; the point is that
        // these lines parse to the rows the daemon is asked for and the rest never leave the process.
        for (line, row) in
            [("paths --json", "paths"), ("kernel paths --sh", "paths"), ("kernel version", "kernel.version")]
        {
            let words: Vec<String> = line.split(' ').map(str::to_owned).collect();
            assert_eq!(kith_api::cli::parse_argv(&words).expect("parses").method.name, row);
        }
        // `kernel version` is the router's per-binary question: never the daemon's to answer.
        for line in [
            "kernel version",
            "kernel engines --json",
            "kernel launch prompt echo",
            "ls --json",
            "frobnicate",
            "paths --bogus",
        ] {
            assert_eq!(try_cli(&args(line)), None, "{line}");
        }
    }

    fn short_folders(tag: &str) -> Paths {
        // A Unix socket path holds about a hundred bytes, so not the long per-user temp folder.
        let p = PathBuf::from("/tmp").join(format!("kfs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("folder");
        Paths {
            home: p.clone(),
            state: p.clone(),
            agents: p.clone(),
            home_real: p.clone(),
            state_real: p.clone(),
            agents_real: p.clone(),
            machine: p.clone(),
            secrets: p.clone(),
            logs: p.clone(),
            cache: p.clone(),
            store: p,
        }
    }

    /// The vault's own door to a real daemon: the rows that need no key answer, a refusal arrives as an error, and no
    /// daemon is one fast error rather than a wait. (Rows that need the key are tested in the core with a stand-in key
    /// store, as the host may not pick one.)
    #[cfg(unix)]
    #[test]
    fn secrets_call_reaches_a_real_daemon_and_fails_fast_without_one() {
        let folders = short_folders("sc");
        let none = std::time::Instant::now();
        let err = secrets_call_at(&folders, "secrets.has", &json!({"name": "a"}), std::time::Duration::ZERO)
            .expect_err("no daemon yet");
        assert!(format!("{err:#}").contains("core is not running"), "{err:#}");
        assert!(none.elapsed() < std::time::Duration::from_secs(5));

        let daemon = serve_for_test(&folders);
        let wait = std::time::Duration::from_secs(5);
        assert_eq!(secrets_call_at(&folders, "secrets.has", &json!({"name": "a"}), wait).expect("has")["has"], false);
        assert_eq!(
            secrets_call_at(&folders, "secrets.delete", &json!({"name": "a"}), wait).expect("delete")["deleted"],
            false
        );
        let bad = secrets_call_at(&folders, "secrets.has", &json!({}), wait);
        assert!(bad.is_err(), "missing params are refused: {bad:?}");
        daemon.stop();
        let _ = std::fs::remove_dir_all(&folders.home);
    }

    /// The bot-start question: a core that is starting is waited for, a core that is down is an error (never "no"),
    /// and a computer with no core database has nothing sealed.
    #[cfg(unix)]
    #[test]
    fn secrets_has_waits_for_a_starting_core_and_fails_loudly_for_a_down_one() {
        let folders = short_folders("sh");
        let none = std::time::Duration::from_millis(300);
        // No core database yet: nothing was ever sealed, and nobody waits.
        let t = std::time::Instant::now();
        assert!(!secrets_has_at(&folders, "a", std::time::Duration::from_secs(10)).expect("no core"));
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        // A core database but no daemon: down, and a loud error.
        serve_for_test(&folders).stop();
        let err = secrets_has_at(&folders, "a", none).expect_err("down is not 'no'");
        assert!(format!("{err:#}").contains("core is not running"), "{err:#}");
        // A daemon that comes up while the caller waits.
        let starting = folders.clone();
        let late = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            serve_for_test(&starting)
        });
        let t = std::time::Instant::now();
        assert!(!secrets_has_at(&folders, "a", std::time::Duration::from_secs(10)).expect("waits"));
        assert!(t.elapsed() >= std::time::Duration::from_millis(500));
        late.join().expect("daemon").stop();
        let _ = std::fs::remove_dir_all(&folders.home);
    }

    #[test]
    fn the_caller_view_carries_home_kernel_and_only_the_variables_paths_reads() {
        let home = PathBuf::from("/h");
        let v = caller_view("paths", json!({}), &home).expect("view");
        assert_eq!(v["home"], "/h");
        assert!(v["kernel"].is_string());
        for k in v["env"].as_object().expect("env").keys() {
            assert!(PATHS_VARS.contains(&k.as_str()), "{k}");
        }
        // The kernel the caller named stays; other methods get nothing added.
        assert_eq!(caller_view("paths", json!({"kernel": "/k"}), &home).expect("view")["kernel"], "/k");
        assert_eq!(caller_view("kernel.version", json!({}), &home), Some(json!({})));
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_that_is_not_utf8_does_not_stop_the_caller_view() {
        use std::os::unix::ffi::OsStringExt as _;
        // The old std::env::vars() panicked on any such variable, whatever its name. The variable is the child
        // process's environment, so this process never calls the unsafe `set_var`.
        if std::env::var_os("KITH_TEST_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().expect("test exe"))
                .args(["--exact", "core_front::tests::a_variable_that_is_not_utf8_does_not_stop_the_caller_view"])
                .env("KITH_TEST_CHILD", "1")
                .env("KITH_STATE", OsString::from_vec(vec![0xff, 0xfe]))
                .output()
                .expect("child runs");
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let v = caller_view("paths", json!({}), &PathBuf::from("/h")).expect("view");
        // The one paths reads is skipped, not sent mangled.
        assert!(v["env"].as_object().expect("env").get("KITH_STATE").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_home_that_is_not_utf8_is_not_sent_so_the_verb_runs_embedded() {
        use std::os::unix::ffi::OsStringExt as _;
        let home = PathBuf::from(OsString::from_vec(vec![b'/', 0xff, 0xfe]));
        assert_eq!(caller_view("paths", json!({}), &home), None);
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
