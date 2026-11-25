//! The host's part of running a `--bg` ask's callee: the third kind of the run door (`run_door.rs`, beside the cron job's
//! `core_cron.rs` and the wake turn's `cards/resume/wake_clock.rs`; design 2026-11-24-ask-ledger R2, R4, R6, A2).
//!
//! `kith ask --bg AGENT task` opens a row in the core (`ask.open`, mode `bg`), which makes a turn of source `ask` for the callee.
//! The pull of `core_runs.rs` lists those turns (`hostclaim.queued`, sources `ask`) and each goes through the door: claim it with no
//! process yet, open the out file the core named as the callee's stdout (a file, never a pipe: a host restart must not cut the
//! callee's output), start the callee in a group of its own with the claim in `KITH_TURN_CLAIM`, wait for it and report its end.
//! What this kind adds to the door:
//!
//! - **The engine is read before the claim.** A Claude callee starts as today's fresh one-shot (`kith AGENT -- task`, which attaches
//!   the claim before the engine starts). Any other engine (codex, qwen, ...) starts through the kernel's launch
//!   (`kith kernel launch AGENT --exec --kernel K`, sched-turn S1), which attaches a claim handed in `KITH_TURN_CLAIM` the same way
//!   for every engine. A callee that cannot start is cancelled up front, so the tick settles the ask `lost`; an engine is never
//!   started without its claim attached (a host restart would requeue the turn and the prompt would go out twice: `PromptOnce`).
//! - **What the caller asked for** (`-m -e -C -p`, `ask.open`'s `model`, `effort`, `folder`, `project`) goes on to the run. A row
//!   from before those fields has none, and the run takes the agent's own, as before.
//! - **The end is told to the ask** (`ask.finish`, with the out file's text and the turn), asked again until the core answers. The
//!   core settles each ask once; a host that was restarted meanwhile has no waiter, and the core's tick settles the ask from the
//!   same out file (`NoStuck`).
//! - **Stop wins** (R6): `ask.end` ends the run's claim; the waiter that holds the child asks the core about it and stops the child.
//!   The finish that follows settles nothing (the ask is stopped) but books the run's cost.

use super::run_door::{Launch, Pace, Run, Started, Watch, free, start_with, until_told};
use crate::terminal::core_writer::CoreWriter;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// How often the waiter asks the core whether the run's claim is still there.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// What the pull needs of the system outside the core, so the rule below is tested without it.
pub(super) struct World<'a> {
    /// The `kith` program (`agent::hq::bin()`).
    pub bin: Option<PathBuf>,
    /// The kernel folder.
    pub kernel: Option<PathBuf>,
    /// The `harness:` an agent's snapshot names, `claude` when it names none, `None` when it cannot be read.
    pub harness: &'a dyn Fn(&str) -> Option<String>,
    /// The host's runtime, to bind a run token to the callee's process.
    pub runtime: Option<tokio::runtime::Handle>,
}

/// The ask turns one pull listed: begins each. A turn the host cannot start right now stays queued and is pulled again.
pub(super) fn take(writer: &Arc<CoreWriter>, turns: &[Value], world: &World<'_>) {
    // One read of an agent's harness per pull, however many of its turns wait.
    let mut harness: HashMap<String, Option<String>> = HashMap::new();
    for t in turns {
        if t["source"] != "ask" || t["parkedReason"].is_string() {
            continue;
        }
        let agent = t["agent"].as_str().unwrap_or_default().to_owned();
        let said = harness.entry(agent.clone()).or_insert_with(|| (world.harness)(&agent)).clone();
        match begin(writer, t, said.as_deref(), world) {
            Started::Ran | Started::Later | Started::Gone => {}
            // The door cancelled a run that could not start; the core refusing the claim is cancelled here. Either way the tick settles
            // the ask `lost`, and the asker is told.
            Started::Cancelled(why) => {
                tracing::warn!(turn = ?t["turn"], why, "routines: a background ask's run could not start; its turn is cancelled");
            }
            Started::Failed(why) => {
                tracing::warn!(turn = ?t["turn"], why, "routines: a background ask's turn was refused; it is cancelled");
                if let Some(turn) = t["turn"].as_i64() {
                    free(writer, turn, &why);
                }
            }
        }
    }
}

/// Whether the engine runs as the one-shot `kith AGENT` does for Claude, else through the kernel's launch. The agent's own harness
/// decides (the rule the launch uses); the turn's `engine` is what `-H` asked for, or the conversation's, which the core defaults to
/// `claude`, so it speaks only when it names another engine, and is the answer for an agent whose file cannot be read.
fn engine_of(t: &Value, harness: Option<&str>) -> String {
    let turn = t["engine"].as_str().filter(|e| !e.is_empty()).unwrap_or("claude");
    if !is_claude(turn) {
        return turn.to_owned();
    }
    harness.unwrap_or(turn).to_owned()
}

fn is_claude(engine: &str) -> bool {
    engine.eq_ignore_ascii_case("claude") || engine.eq_ignore_ascii_case("claude-code")
}

/// One listed ask turn: the engine read, then the door.
fn begin(writer: &Arc<CoreWriter>, t: &Value, harness: Option<&str>, world: &World<'_>) -> Started {
    let (Some(turn), Some(conversation), Some(ask)) =
        (t["turn"].as_i64(), t["conversation"].as_str(), t["ask"].as_str())
    else {
        return Started::Gone;
    };
    // Its earlier claim ran (a restart cut it off): its prompt may have gone out, so it never begins again. The tick settles the ask.
    if t["redeemedBefore"] == true {
        free(writer, turn, super::core_runs::INTERRUPTED);
        return Started::Gone;
    }
    let person = t["person"].as_str().unwrap_or_default();
    let Some(out) = t["out"].as_str().map(PathBuf::from) else {
        let why = "the ask has no out file for its callee";
        free(writer, turn, why);
        return Started::Cancelled(why.to_owned());
    };
    let engine = engine_of(t, harness);
    let run = Run { turn, conversation, person, kind: "askRun", what: "the callee", ended: "the background ask" };
    let (id, task) = (ask.to_owned(), t["prompt"].as_str().unwrap_or_default().to_owned());
    start_with(writer, &run, |claim| {
        let plan = Plan::of(t, &engine, &task, claim, world)?;
        let (file, err_file) = Plan::files(&out)?;
        let mut cmd = plan.command(file, &err_file)?;
        let grant = grant_for(&mut cmd, &plan, conversation, world);
        super::run_door::detach(&mut cmd);
        let bind = grant.map(|(grant, runtime)| {
            Box::new(move |pid: u32| -> Box<dyn std::any::Any + Send> {
                runtime.block_on(grant.bind_pid(pid));
                Box::new(grant)
            }) as super::run_door::Bind
        });
        let (alive, told) = (writer.clone(), writer.clone());
        let watch = Watch {
            every: WATCH_EVERY,
            // `None`: the core did not answer, and silence is not an end.
            ended: Box::new(move || alive.claim_alive(claim) == Some(false)),
            after: Box::new(move || finish(&told, &id, turn, &out)),
        };
        Ok((Launch { cmd, stdin: None, err_file: Some(err_file), bind }, Some(watch)))
    })
}

/// What the run is, read from the turn.
struct Plan {
    agent: String,
    task: String,
    claim: i64,
    claude: bool,
    /// `-H`, for an engine other than the agent's own.
    harness: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    project: Option<String>,
    folder: Option<PathBuf>,
    /// The ask this run answers (a card the callee parks on answers it) and who asked.
    ask: String,
    asked_by: String,
    /// The callee's session to continue: a `-c` ask's, or the one a card's answer resumes. None: a fresh one.
    resume: Option<String>,
    /// Whose run it is: `None` for the owner.
    person: Option<String>,
    bin: PathBuf,
    kernel: Option<PathBuf>,
}

impl Plan {
    fn of(t: &Value, engine: &str, task: &str, claim: i64, world: &World<'_>) -> Result<Self, String> {
        let opt = |k: &str| t["opts"][k].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
        let claude = is_claude(engine);
        let bin = world.bin.clone().ok_or_else(|| "no kith program here to run the callee".to_owned())?;
        if !claude && world.kernel.is_none() {
            return Err("no kith kernel here to run the callee".to_owned());
        }
        // A session of the kernel's own naming (`ask:ID`, a conversation the core made up) is not one an engine can resume.
        let real = |s: Option<&str>| s.filter(|s| !s.is_empty() && !s.starts_with("ask:")).map(str::to_owned);
        let person = t["person"].as_str().filter(|p| !p.is_empty() && *p != "owner" && t["owner"] != true);
        Ok(Self {
            agent: t["agent"].as_str().unwrap_or_default().to_owned(),
            task: task.to_owned(),
            claim,
            claude,
            harness: (!claude).then(|| engine.to_owned()),
            model: opt("model"),
            effort: opt("effort"),
            project: opt("project"),
            folder: opt("folder").map(PathBuf::from),
            ask: t["ask"].as_str().unwrap_or_default().to_owned(),
            asked_by: t["askedBy"].as_str().unwrap_or_default().to_owned(),
            resume: real(t["askSession"].as_str()).or_else(|| real(t["conversation"].as_str())),
            person: person.map(str::to_owned),
            bin,
            kernel: world.kernel.clone(),
        })
    }

    /// The out file (stdout: never truncated by a second run of the same turn, which only exists after a requeue that nothing
    /// attached to) and the file stderr goes to, beside it.
    fn files(out: &Path) -> Result<(std::fs::File, PathBuf), String> {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make the folder of the out file: {e}"))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
            .map_err(|e| format!("couldn't open the out file {}: {e}", out.display()))?;
        Ok((file, out.with_extension("err")))
    }

    /// The command: the agent's one-shot, `kith AGENT [flags] -- TASK`, for Claude; the kernel's launch for another engine. The
    /// environment is the wake door's (the host's own, filtered, and the conversation lock's launcher) plus what `kith ask` gives a
    /// callee: the answer as JSON, who asked, the ask's id (a card the run parks on answers it).
    fn command(&self, out: std::fs::File, err_file: &Path) -> Result<Command, String> {
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(err_file)
            .map_err(|e| format!("couldn't open the log {}: {e}", err_file.display()))?;
        let mut cmd = Command::new(&self.bin);
        if self.claude {
            cmd.arg(&self.agent);
        } else if let Some(kernel) = &self.kernel {
            cmd.args(["kernel", "launch", &self.agent, "--exec", "--kernel"]).arg(kernel);
        }
        for (flag, value) in [("-H", &self.harness), ("-m", &self.model), ("-e", &self.effort), ("-p", &self.project)] {
            if let Some(v) = value {
                cmd.args([flag, v]);
            }
        }
        cmd.args(["--", &self.task]);
        if let Some(dir) = &self.folder {
            if !dir.is_dir() {
                return Err(format!("the folder {} is gone, so the callee has nowhere to work", dir.display()));
            }
            cmd.current_dir(dir);
        }
        cmd.env_clear().envs(self.env()).stdin(Stdio::null()).stdout(out).stderr(err);
        Ok(cmd)
    }

    fn env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> =
            std::env::vars().filter(|(k, _)| crate::channels::run::inherited(k)).collect();
        env.extend(crate::kernel::stub_env());
        let mut set = |name: &str, v: &str| {
            env.retain(|(k, _)| k != name);
            env.push((name.to_owned(), v.to_owned()));
        };
        set("KITH_VIA", "ask");
        set("KITH_OUTPUT", "json");
        set("KITH_LAUNCH_KIND", "ask");
        set("KITH_LAUNCH_LABEL", &self.asked_by);
        set("KITH_LAUNCH_ASK_ID", &self.ask);
        // No host picks the account: the launch walks the candidates (`kith account pick`).
        set("KITH_BACKUP_CHAIN", "on");
        set("KITH_TURN_CLAIM", &self.claim.to_string());
        if let Ok(mode) = std::env::var("KITH_ASK_PERMISSION_MODE") {
            set("KITH_PERMISSION_MODE", &mode);
        }
        if let Some(session) = &self.resume {
            set("KITH_RESUME", session);
        }
        if let Some(person) = &self.person {
            for (k, v) in crate::kernel::both("PERSON", person)
                .into_iter()
                .chain(crate::kernel::both("ROOM", &crate::agent::hq::room_of(&self.agent, Some(person), None)))
            {
                set(&k, &v);
            }
        }
        env
    }
}

/// A run token for the callee (a card it raises is the owner's or its person's), bound to its process once it starts. `None`
/// when the host has no run socket or no runtime to bind with: the run goes without, as a wake's does.
fn grant_for(
    cmd: &mut Command,
    plan: &Plan,
    conversation: &str,
    world: &World<'_>,
) -> Option<(crate::runauth::RunGrant, tokio::runtime::Handle)> {
    let runtime = world.runtime.clone()?;
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        cmd.get_envs().filter_map(|(k, v)| Some((k.to_owned(), v?.to_owned()))).collect();
    let grant = crate::runauth::grant_for_env(&mut env, &plan.agent, conversation, None)?;
    let known: Vec<_> = cmd.get_envs().map(|(k, _)| k.to_owned()).collect();
    for (k, v) in env.into_iter().filter(|(k, _)| !known.contains(k)) {
        cmd.env(k, v);
    }
    Some((grant, runtime))
}

/// Tells the ask its run ended: the out file's text (empty when there is none: the ask is `lost`) and the turn. Asked again until
/// the core answers; a refusal is an answer (the ask is gone or was ended: nothing to settle).
fn finish(writer: &Arc<CoreWriter>, ask: &str, turn: i64, out: &Path) {
    let text = std::fs::read_to_string(out).unwrap_or_default();
    until_told("ask finish", turn, &Pace::LIVE, || writer.ask_finish(ask, turn, &text));
}

/// The `harness:` of the agent's pinned snapshot (`kith-pkg resolve`, what the launch reads: `$pkg/agent.md`), `claude` when it names
/// none; `None` when the snapshot cannot be read. Blocking (a process and the agents folder). The wake door reads it the same way
/// (`cards/resume/wake_clock.rs`).
// TODO(WK4c, #1221): the wake door's copy and this one are the same function; keep one when WK4c lands.
pub(super) fn harness_of_agent(agent: &str) -> Option<String> {
    let kernel = crate::kernel::current_kernel()?;
    let out = Command::new(kernel.join("bin/kith-pkg")).args(["resolve", agent]).stdin(Stdio::null()).output().ok()?;
    let dir = out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))?;
    let text = std::fs::read_to_string(dir.join("agent.md")).ok()?;
    let mut lines = text.lines();
    if lines.next() == Some("---") {
        for l in lines.take_while(|l| *l != "---") {
            if let Some(v) = l.strip_prefix("harness:") {
                return Some(v.trim().to_owned()).filter(|h| !h.is_empty()).or_else(|| Some("claude".to_owned()));
            }
        }
    }
    Some("claude".to_owned())
}

#[cfg(all(test, unix))]
#[path = "core_ask_tests.rs"]
mod tests;
