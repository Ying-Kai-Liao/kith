//! A launch under a host-claimed turn whose conversation is not the launch's own session (retire-python P2, C3L; rule C3 of the
//! wake-s4 design's amendment of 2026-11-26): the real core (the claims stepped), the built `kith` as the launch, and only the
//! engine a stand-in. The host claims a turn on its conversation and starts the launch with `KITH_TURN_CLAIM` and
//! `KITH_TURN_CONVERSATION`; the launch takes its writer claim on its own session first (Claude), attaches the claim naming the
//! turn's conversation, and the engine sees neither variable.
//!
//! - the Rust launch, Claude: the session is the launch's own, the turn is on another conversation
//! - the Rust launch, another engine: the claim is attached naming the conversation (no session of its own)
//! - `kernel/bin/kith`, Claude: the same, through the script's claim step
//! - a refused attach starts no engine (exit 10); the variable without a claim is refused
//!
//! The program is `target/<profile>/kith` beside the test binary: a `cargo test` of the package builds it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::terminal::core_writer::{CoreWriter, RunClaim};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One world at a time (the stub program is a process-wide variable).
static SERIAL: Mutex<()> = Mutex::new(());

pub(super) fn repo_kernel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel").canonicalize().unwrap()
}

/// The built `kith`, beside the folder the test binary runs from (`target/debug/deps/..`).
pub(super) fn built_kith() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let kith = exe.parent().unwrap().parent().unwrap().join("kith");
    assert!(kith.is_file(), "the built kith is not at {}: build the package's binaries first", kith.display());
    kith
}

/// Both engines record, once per real run, what they were started with; `HOLD` keeps them running until it is gone.
const ENGINE: &str = r#"#!/bin/sh
case "$1 $2" in
  "--version "*) echo "2.1.309 (Claude Code)"; exit 0 ;;
  "plugin list") echo '[]'; exit 0 ;;
  "auth status") echo '{"loggedIn":true}'; exit 0 ;;
esac
while [ -e "$HOLD" ]; do sleep 0.1; done
{
  echo "argv=$*"
  env | sort
} > "$REC/run.$$"
# What a run that answers says on stdout (the ask tests set it; the callee's out file is its answer).
[ -z "$ANSWER" ] || echo "$ANSWER"
exit 0
"#;

pub(super) struct World {
    pub(super) dir: PathBuf,
    pub(super) folders: Paths,
    pub(super) env: Vec<(String, String)>,
    daemon: Option<crate::core_front::TestDaemon>,
    _held: std::sync::MutexGuard<'static, ()>,
}

impl World {
    /// A home with agent `house` (a job `j`) and the owner `kai`, and a core serving it with the claims stepped.
    pub(super) fn start() -> Self {
        let held = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Short, for the core's socket.
        let dir = PathBuf::from("/tmp").join(format!("klt-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let (home, machine) = (dir.join("home"), dir.join("machine"));
        let kernel = repo_kernel();
        let (bin, rec) = (dir.join("bin"), dir.join("rec"));
        for d in [&home, &machine, &bin, &rec, &home.join("agents/house/role"), &home.join(".claude")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            home.join("agents/house/role/agent.md"),
            "---\nname: house\ndescription: test agent\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            home.join("agents/house/jobs.json"),
            json!([{"id": "j", "cron": "0 8 * * *", "task": "do the job"}]).to_string(),
        )
        .unwrap();
        std::fs::write(home.join(".claude/.fake-login"), "").unwrap();
        for name in ["claude", "codex"] {
            let engine = bin.join(name);
            std::fs::write(&engine, ENGINE).unwrap();
            std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let acp = bin.join("acp-engine");
        // The echo agent of the bot tests, offering the session modes a codex run asks for.
        let echo = dir.join("acp_agent.py");
        let fixture =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo_agent.py"))
                .unwrap();
        let with_modes = fixture.replace(
            r#"{"sessionId": f"s{sessions}"}"#,
            r#"{"sessionId": f"s{sessions}", "modes": {"availableModes": [{"id": "read-only"}, {"id": "auto"}, {"id": "full-access"}]}}"#,
        );
        assert_ne!(with_modes, fixture, "the echo agent's session/new answer changed");
        std::fs::write(&echo, with_modes).unwrap();
        std::fs::write(
            &acp,
            format!(
                "#!/bin/sh\nwhile [ -e \"$HOLD\" ]; do sleep 0.1; done\nenv | sort > \"$REC/run.$$\"\ntee \"$ACPIN\" | python3 {}\n",
                echo.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&acp, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The claims step wants a start stub to exist (it is the writer lock's; nothing here runs it).
        let stub = dir.join("kith-stub");
        std::fs::write(&stub, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: set under SERIAL, before this test starts a thread of its own; every test sets the same name.
        unsafe { std::env::set_var("KITH_STUB_BIN", &stub) };

        let vars = [
            ("KITH_STATE", machine.to_string_lossy().into_owned()),
            ("KITH_HOME", home.to_string_lossy().into_owned()),
            ("KITH_AGENTS", home.join("agents").to_string_lossy().into_owned()),
        ];
        let lookup = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let folders = kith_kernel::paths::paths(&home, &kernel, &lookup);
        std::fs::create_dir_all(&folders.secrets).unwrap();
        let p = |x: &Path| x.to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> = vars.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
        env.extend([
            ("HOME".to_owned(), p(&home)),
            ("PATH".to_owned(), format!("{}:/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin", bin.display())),
            ("REC".to_owned(), p(&rec)),
            ("ACPIN".to_owned(), p(&dir.join("acp-in"))),
            ("HOLD".to_owned(), p(&dir.join("hold"))),
            ("KITH_AUTH".to_owned(), p(&folders.secrets.join("auth.json"))),
            ("KITH_KERNEL".to_owned(), p(&kernel)),
            ("KITH_BIN".to_owned(), p(&built_kith())),
            // No host here: nothing is asked of one.
            ("KITH_SERVE_DIR".to_owned(), p(&dir.join("serve"))),
            ("KITH_SERVE_PORT".to_owned(), "1".to_owned()),
            ("TZ".to_owned(), "UTC".to_owned()),
            // A non-Claude engine runs over ACP; the runner has no adapter, so the run gets this one (an echo agent behind a
            // wrapper that records the environment and what the launch sent it).
            ("KITH_RUN_ONCE_ACP_COMMAND".to_owned(), p(&acp)),
        ]);
        let mut world = Self { dir, folders, env, daemon: None, _held: held };
        let out = world
            .command(Path::new("python3"))
            .arg(kernel.join("bin/kith-auth"))
            .args(["person", "kai", "owner"])
            .output()
            .unwrap();
        assert!(out.status.success(), "kith-auth person: {}", String::from_utf8_lossy(&out.stderr));
        let said = crate::core_front::step_authority_for_test(&world.folders, &kernel).expect("the authority step");
        assert!(said.iter().any(|l| l.contains("switched")), "{said:?}");
        // The step writes the writer lock's tombstone there.
        std::fs::create_dir_all(world.folders.machine.join("live/terminal")).unwrap();
        let said = crate::core_front::step_claims_for_test(&world.folders, &kernel).expect("the claims step");
        assert!(said.iter().any(|l| l.contains("switched")), "{said:?}");
        // The ask part of `wake.tick` and the owner's `wake.*` rows answer a switched queue only.
        kith_core::mark_wake_switched_for_test(&world.folders).expect("the wake switch");
        let daemon = crate::core_front::serve_for_test(&world.folders);
        daemon.reconciled();
        world.daemon = Some(daemon);
        world
    }

    /// The `turns` rows of a conversation: (id, state).
    pub(super) fn turns(&self, conversation: &str) -> Vec<(i64, String)> {
        self.daemon.as_ref().expect("the daemon").turns(conversation)
    }

    pub(super) fn command(&self, program: &Path) -> Command {
        let mut c = Command::new(program);
        c.env_clear().envs(self.env.iter().map(|(k, v)| (k, v)));
        c
    }

    pub(super) fn core(&self, method: &str, params: &Value) -> Value {
        crate::core_front::ask(&self.folders, method, params)
            .unwrap_or_else(|| panic!("{method}: the core did not answer"))
    }

    /// A row only the host may call.
    pub(super) fn core_host(&self, method: &str, params: &Value) -> Value {
        crate::core_front::call_as_host(&self.folders, method, params)
            .unwrap_or_else(|| panic!("{method}: the core did not answer"))
    }

    pub(super) fn show(&self, claim: i64) -> Value {
        crate::core_front::call_as_host(&self.folders, "hostclaim.show", &json!({"claimId": claim}))
            .map_or(Value::Null, |v| v["claim"].clone())
    }

    /// Fires the job and has the host claim its turn: `(turn, conversation, claim)`. The conversation is the turn's, not any
    /// session of a launch.
    fn claimed_turn(&self) -> (i64, String, i64) {
        let fired = self.core("schedule.runNow", &json!({"agent": "house", "id": "j", "key": "c3l"}));
        assert_eq!(fired["outcome"], "fired", "{fired}");
        let host = CoreWriter::with_instance(self.folders.clone(), "host-1".to_owned());
        let turns = host.queued().expect("the core's queue");
        let t = turns.iter().find(|t| t["job"] == "j").unwrap_or_else(|| panic!("no turn: {turns:?}"));
        let (turn, conversation) = (t["turn"].as_i64().unwrap(), t["conversation"].as_str().unwrap().to_owned());
        let RunClaim::Claimed(claim) =
            host.claim_run_as("scheduleRun", &conversation, t["person"].as_str().unwrap_or(""), turn)
        else {
            panic!("the claim");
        };
        (turn, conversation, claim)
    }

    /// The runs the engines recorded.
    pub(super) fn runs(&self) -> Vec<String> {
        std::fs::read_dir(self.dir.join("rec"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect()
    }

    /// Starts `program args` as the launch of the claimed turn (the claim and its conversation in the environment), the engine
    /// held; waits for the claim to be attached and checks it names the launch's pid; releases the engine and waits for the end.
    /// Answers the launch's exit status and the one run the engine recorded.
    fn launch(&self, program: &Path, args: &[&str], claim: i64, conversation: &str, own_session: bool) -> Vec<String> {
        std::fs::write(self.dir.join("hold"), "").unwrap();
        let mut cmd = self.command(program);
        cmd.args(args)
            .env("KITH_TURN_CLAIM", claim.to_string())
            .env("KITH_TURN_CONVERSATION", conversation)
            .current_dir(self.dir.join("home"))
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(self.dir.join("launch.out")).unwrap())
            .stderr(std::fs::File::create(self.dir.join("launch.err")).unwrap());
        let mut child = cmd.spawn().unwrap();
        let diag = || {
            format!(
                "stdout: {}\nstderr: {}",
                std::fs::read_to_string(self.dir.join("launch.out")).unwrap_or_default(),
                std::fs::read_to_string(self.dir.join("launch.err")).unwrap_or_default()
            )
        };
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let c = self.show(claim);
            if c["pid"].is_i64() {
                assert_eq!(c["pid"], i64::from(child.id()), "the claim is the launch's own pid");
                assert_eq!(c["state"], "running", "{c}");
                break;
            }
            assert!(Instant::now() < end, "the launch did not attach the claim\n{}", diag());
            if let Some(status) = child.try_wait().unwrap() {
                panic!("the launch ended ({status}) before it attached\n{}", diag());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // A Claude launch holds the writer claim on its own session while the engine runs; another engine has none.
        let sessions = self.session_claims(conversation);
        assert_eq!(sessions.len(), usize::from(own_session), "the launch's session claims: {sessions:?}");
        for c in &sessions {
            assert_eq!(c["pid"].as_i64(), Some(i64::from(child.id())), "{c}");
            assert!(["claimed", "running"].contains(&c["state"].as_str().unwrap_or_default()), "{c}");
        }
        std::fs::remove_file(self.dir.join("hold")).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "the launch failed: {status}\n{}", diag());
        let runs = self.runs();
        assert_eq!(runs.len(), 1, "the task reached the engine once: {runs:?}\n{}", diag());
        for gone in ["KITH_TURN_CLAIM", "HQ_TURN_CLAIM", "KITH_TURN_CONVERSATION", "HQ_TURN_CONVERSATION"] {
            assert!(!runs[0].lines().any(|l| l.starts_with(&format!("{gone}="))), "{gone} reached the engine");
        }
        runs
    }

    /// The live writer claims on a conversation other than the turn's `conversation`: the sessions launches hold.
    fn session_claims(&self, conversation: &str) -> Vec<Value> {
        let listed = self.core("claim.list", &json!({}));
        listed["live"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["kind"] == "external" && c["conversation"] != conversation)
            .cloned()
            .collect()
    }

    /// A launch that is refused: its exit status and what it said; no engine ran.
    fn refused(&self, program: &Path, args: &[&str], vars: &[(&str, &str)]) -> (Option<i32>, String) {
        let mut cmd = self.command(program);
        cmd.args(args).envs(vars.iter().copied()).current_dir(self.dir.join("home")).stdin(Stdio::null());
        let out = cmd.output().unwrap();
        assert!(self.runs().is_empty(), "an engine ran: {:?}", self.runs());
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn kernel_args<'a>(kernel: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["kernel", "launch-exec", "house", "--kernel", kernel];
    v.extend(extra);
    v
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_rust_launch_of_claude_takes_its_own_session_then_attaches_the_turns_conversation() {
    let w = World::start();
    let (_, conversation, claim) = w.claimed_turn();
    let kernel = repo_kernel().to_string_lossy().into_owned();
    let args = kernel_args(&kernel, &["do the job"]);
    let runs = w.launch(&built_kith(), &args, claim, &conversation, true);
    assert!(runs[0].contains("do the job"), "{}", runs[0]);
    let shown = w.show(claim);
    assert_eq!(shown["conversation"], conversation.as_str(), "the claim stays the turn's: {shown}");
}

#[test]
fn the_rust_launch_of_another_engine_attaches_naming_the_turns_conversation() {
    let w = World::start();
    let (_, conversation, claim) = w.claimed_turn();
    let kernel = repo_kernel().to_string_lossy().into_owned();
    let args = kernel_args(&kernel, &["-H", "codex", "do the job"]);
    w.launch(&built_kith(), &args, claim, &conversation, false);
    let sent = std::fs::read_to_string(w.dir.join("acp-in")).unwrap();
    assert!(sent.contains("do the job"), "the task reached the engine: {sent}");
}

#[test]
fn the_script_launch_of_claude_takes_its_own_session_then_attaches_the_turns_conversation() {
    let w = World::start();
    let (_, conversation, claim) = w.claimed_turn();
    let script = repo_kernel().join("bin/kith");
    let runs = w.launch(&script, &["house", "do the job"], claim, &conversation, true);
    assert!(runs[0].contains("do the job"), "{}", runs[0]);
}

#[test]
fn a_refused_attach_starts_no_engine_and_the_conversation_without_a_claim_is_refused() {
    let w = World::start();
    let (_, conversation, claim) = w.claimed_turn();
    let kernel = repo_kernel().to_string_lossy().into_owned();
    let args = kernel_args(&kernel, &["-H", "codex", "do the job"]);
    // Another conversation than the turn's: the core refuses the attach, the launch exits 10 and nothing runs.
    let (code, said) = w.refused(
        &built_kith(),
        &args,
        &[("KITH_TURN_CLAIM", &claim.to_string()), ("KITH_TURN_CONVERSATION", "ask:not-this-one")],
    );
    assert_eq!(code, Some(10), "{said}");
    assert_eq!(w.show(claim)["state"], "claimed", "nothing attached it");
    // The variable alone names no turn.
    let (code, said) = w.refused(&built_kith(), &args, &[("KITH_TURN_CONVERSATION", &conversation)]);
    assert_eq!(code, Some(10), "{said}");
    // The Claude launch too: the session's claim goes back, the turn stays claimed.
    let (code, said) = w.refused(
        &built_kith(),
        &kernel_args(&kernel, &["do the job"]),
        &[("KITH_TURN_CLAIM", &claim.to_string()), ("KITH_TURN_CONVERSATION", "ask:not-this-one")],
    );
    assert_eq!(code, Some(10), "{said}");
    assert_eq!(w.show(claim)["state"], "claimed");
    assert!(w.session_claims(&conversation).is_empty(), "the refused Claude launch gave its session back");
    // The script's claim step: the variable alone, and a conversation that is not the turn's.
    let script = repo_kernel().join("bin/kith");
    let (code, said) = w.refused(&script, &["house", "do the job"], &[("KITH_TURN_CONVERSATION", &conversation)]);
    assert_eq!(code, Some(10), "{said}");
    let (code, said) = w.refused(
        &script,
        &["house", "do the job"],
        &[("KITH_TURN_CLAIM", &claim.to_string()), ("KITH_TURN_CONVERSATION", "ask:not-this-one")],
    );
    assert_eq!(code, Some(10), "{said}");
    assert_eq!(w.show(claim)["state"], "claimed");
    assert!(w.session_claims(&conversation).is_empty(), "the refused script launch gave its session back");
}
