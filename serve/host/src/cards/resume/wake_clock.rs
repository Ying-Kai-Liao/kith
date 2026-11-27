//! The wake queue's clock and the run door by turn id (design 2026-11-21-wake-s4, package `WK4b`). Once the `wake` module is
//! switched the queue is rows of the core, and the host's taker no longer asks for a turn to run: it drives the core's clock
//! and begins the turns the core made.
//!
//! - **R1 the clock**: each pass of the taker (every 5 s, and 300 ms after a kick, the host's own enqueue of a request) calls
//!   `wake.tick`, in Writer mode only, like the schedule runner's pull. A tick the core refuses, answers `Busy` or does not
//!   answer is not made: the next pass is its retry. After a tick the host starts the housekeeping that used to ride on
//!   `kith-wake drain`: the lifecycle sweep (`kith kernel lifecycle sweep`) and, until asks are rows of the core, the ask
//!   bridge (`kith-ask --sweep`, whose `enqueue` goes to the core).
//! - **R3 the pull**: `hostclaim.queued` with the `wake` source lists the turns the tick made. A turn of a bot chat or a channel
//!   thread still goes to its door as before (`handle`, `WK4c` moves those to the turn id). Every other turn is a run door turn,
//!   the owner's session or a person's: its door checks answer the core (`wake.done` withdrawn when nothing is wanted or the
//!   person's access ended, `wake.back` when access is unknown), and a turn that passes is claimed and started by the shared
//!   detached-run module (`routines/run_door.rs`): `kith AGENT --resume SESSION NOTE` with `KITH_TURN_CLAIM` (and the person's
//!   `KITH_PERSON`), whose launch attaches to the claim. Nothing here says the turn is answered: the core's tick settles a run
//!   by its turn (R4), so the host only reports how the process ended.
//! - **A terminal holds the session**: the claim is refused as held and the turn stays where it is (the tick parks it, and
//!   unparks it when the terminal lets go): nothing starts beside the terminal.
//! - **Until sched-turn S1** a detached turn runs only a Claude session: any other engine's turn is cancelled up front (the
//!   core gives its requests back as a failed try; after five they end `failed`) and never started without its claim, because a
//!   host restart would requeue it and its prompt would go out twice ([`engine_gap`]).
//!
//! The run's output goes where `kith-ask` reads a follow-up's words: `<logs>/wake/<run>.log` and `.err`.

use super::channel_wake::Pull;
use super::{
    Card, TAKEN, clear_held, give_up, handle, lock, rebuilt, set_held, spawn_with, state_of, unstarted, wake, wanted,
};
use crate::hub::Hub;
use crate::routines::run_door::{self, Launch, Run, Started};
use crate::terminal::core_holders::{Reader, Reading};
use crate::terminal::core_writer::{CoreWriter, Mode};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Said when a turn cannot run detached yet; the reason a cancelled turn carries.
pub(super) const ENGINE_GAP: &str = "this agent's engine cannot run detached turns yet";
/// What a cancelled interrupted run says: a restart cut off a run whose prompt may have gone out.
const INTERRUPTED: &str = "interrupted: a restart cut this run off";
/// How long a run handed to a door of the old kind (a bot's actor, a channel run) is not handed again: its turn stays listed
/// until the door says it started.
const HANDED_FOR: Duration = Duration::from_secs(120);
/// The lifecycle sweep makes 54-minute decisions: once a minute is the old tick's pace.
const LIFECYCLE_EVERY: Duration = Duration::from_secs(60);

/// The runs given to a door of the old kind, and when.
static HANDED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);
/// How long a turn the core refused as held waits before the door claims it again.
const LATER_FOR: Duration = Duration::from_secs(15);
/// The runs whose claim was refused as held, and when.
static LATER: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);
/// A housekeeping process is running: the next tick starts none beside it.
static SWEEPING: AtomicBool = AtomicBool::new(false);
static ASKING: AtomicBool = AtomicBool::new(false);
static LAST_LIFECYCLE: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(Mutex::default);

/// The turn is begun again after a pause (a wake of the thread holds the key, a writer holds the session, the connection is down).
pub(super) fn later(run: &str) {
    lock(&HANDED).remove(run);
    lock(&LATER).insert(run.to_owned(), Instant::now());
}

/// [`later`] with a longer pause: the turn waits `extra` more than [`LATER_FOR`] (a launch that keeps refusing the claim).
pub(super) fn later_extra(run: &str, extra: Duration) {
    lock(&HANDED).remove(run);
    lock(&LATER).insert(run.to_owned(), Instant::now() + extra);
}

/// A door of the old kind said what became of the turn it was handed: it is not held back any more.
pub(super) fn released(run: &str) {
    lock(&HANDED).remove(run);
}

/// One pass of the clock. `false`: the module is not switched, and the caller's old path (`drive`) is the one that runs the
/// queue. `true`: the core's queue is this pass's, whether or not this host may tick it.
pub(super) async fn step(hub: &Arc<Hub>) -> bool {
    let state = state_of(hub);
    if !wake::module_switched(&state) {
        return false;
    }
    let writer = {
        let hub = hub.clone();
        wake::blocking(move || crate::agent::bot::writer_core(&hub).filter(|c| c.mode() == Mode::Writer))
            .await
            .flatten()
    };
    // Not the Writer: the core's claims are another host's or the files', and nothing here ticks or begins a turn.
    let Some(core) = writer else { return true };
    super::reap(hub);
    // A turn an actor was handed by its id stays handed until the actor says it began or did not.
    lock(&HANDED).retain(|run, at| at.elapsed() < HANDED_FOR || lock(&super::PULLED).contains_key(run));
    // A run that ended some other way than starting (withdrawn, cancelled, already ran) leaves its wait behind: forgotten once old.
    lock(&LATER).retain(|_, at| at.elapsed() < LATER_FOR * 4);
    let st = state.clone();
    match wake::blocking(move || wake::tick(&st)).await {
        Some(Ok(_)) => housekeeping(),
        Some(Err(error)) => {
            tracing::debug!(error = format!("{error:#}"), "wake: a tick was not made; the next one tries again");
        }
        None => {}
    }
    pull(hub, &core, &state).await;
    true
}

/// Lists the wake turns the core made and begins each.
async fn pull(hub: &Arc<Hub>, core: &Arc<CoreWriter>, state: &Path) {
    let listed = {
        let core = core.clone();
        wake::blocking(move || core.queued_from(&["wake"])).await
    };
    let turns = match listed {
        Some(Ok(turns)) => turns,
        Some(Err(why)) => return tracing::debug!(why, "wake: the core's queued turns can't be read"),
        None => return,
    };
    for t in &turns {
        begin(hub, core, state, t).await;
    }
}

/// The run id a wake turn's note starts with: `[kith wake RUN] ...`, the marker the transcript proves delivery by.
fn run_of(note: &str) -> Option<&str> {
    note.strip_prefix("[kith wake ")?.split_once(']').map(|(run, _)| run).filter(|r| !r.is_empty())
}

/// The door a session is for: a bot chat's or a channel thread's turn keeps its old door (`WK4c`), every other session is a
/// Claude session the run door resumes.
fn old_door(session: &str) -> bool {
    session.starts_with("acp:") || session.contains('#')
}

/// Whether a turn may be started detached today. Until sched-turn S1 (`kernel launch AGENT --exec --kernel K --turn N --claim C`)
/// lands, the host's launch is `kith AGENT --resume SESSION`: a Claude session. One place, so S1's follow-up switches it.
///
/// The agent's harness decides (the rule the launch uses: `harness:` of agent.md, unset is claude); the conversation's engine
/// column is only the answer for an agent whose file cannot be read, since the core defaults a session it has not seen to claude.
pub(super) fn engine_gap(t: &Value, harness: Option<&str>) -> Option<&'static str> {
    let engine = harness.or_else(|| t["engine"].as_str());
    (!engine.is_some_and(|e| e.eq_ignore_ascii_case("claude") || e.eq_ignore_ascii_case("claude-code")))
        .then_some(ENGINE_GAP)
}

/// The `harness:` of the agent's pinned snapshot (`kith-pkg resolve`, what the launch reads: `$pkg/agent.md`), `claude` when it
/// names none; `None` when the snapshot cannot be read. Blocking (a process and the agents folder): call it off the async workers.
fn harness_of_agent(agent: &str) -> Option<String> {
    let kernel = crate::kernel::current_kernel()?;
    let out = Command::new(kernel.join("bin/kith-pkg")).args(["resolve", agent]).stdin(Stdio::null()).output().ok()?;
    let dir = out.status.success().then(|| std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))?;
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

/// Cancels a turn at the core and asks again until it answers (the turn would wait for ever otherwise).
async fn cancel(core: &Arc<CoreWriter>, turn: i64, why: &'static str) {
    let core = core.clone();
    wake::blocking(move || run_door::free(&core, turn, why)).await;
}

/// One listed wake turn.
async fn begin(hub: &Arc<Hub>, core: &Arc<CoreWriter>, state: &Path, t: &Value) {
    let (Some(turn), Some(session)) = (t["turn"].as_i64(), t["conversation"].as_str()) else { return };
    let note = t["note"].as_str().unwrap_or_default();
    let Some(run) = run_of(note) else {
        tracing::warn!(turn, "wake: a wake turn whose note names no run is left as it is");
        return;
    };
    // Parked behind a terminal (or another writer): the tick unparks it when nothing holds the session. A turn this host
    // handed to an old door waits there for its answer.
    if t["parkedReason"].is_string() || lock(&TAKEN).contains_key(run) || lock(&HANDED).contains_key(run) {
        return;
    }
    if lock(&LATER).get(run).is_some_and(|at| at.elapsed() < LATER_FOR) {
        return;
    }
    // Its earlier claim ran (a restart cut it off): its prompt may have gone out, so it is never begun again. R4 settles it.
    if t["redeemedBefore"] == true {
        return cancel(core, turn, INTERRUPTED).await;
    }
    let st = (state.to_path_buf(), run.to_owned(), note.to_owned());
    let Some(Some(carried)) = wake::blocking(move || wake::turn_of(&st.0, &st.1, &st.2)).await else {
        // A Stop took the run between the list and now: R5 cancels its turn.
        return;
    };
    if old_door(session) {
        lock(&HANDED).insert(run.to_owned(), Instant::now());
        let pull = Pull { core, turn, person: t["person"].as_str().unwrap_or_default().to_owned() };
        return handle(hub, carried, Some(pull)).await;
    }
    door(hub, core, t, turn, &carried).await;
}

/// A run door turn: the checks the host keeps at the door, then the claim and the start.
async fn door(hub: &Arc<Hub>, core: &Arc<CoreWriter>, t: &Value, core_turn: i64, turn: &wake::Turn) {
    let carried: Vec<Card> =
        turn.ids.iter().filter_map(|i| i.strip_prefix("card:")).filter_map(|id| hub.store.card(id)).collect();
    let others = turn.ids.iter().filter(|i| !i.starts_with("card:") && !i.starts_with("msg:")).count();
    let person = turn.person.clone().filter(|p| !p.is_empty());
    // A7 again, at the door: a person's access can end between the yes and the turn.
    if let Some(person) = &person {
        match crate::api::roles::may_use_checked(person, &turn.agent).await {
            Some(false) => {
                super::super::withdraw_person(hub, person, Some(&turn.agent));
                return give_up(hub, &turn.run);
            }
            None => return unstarted(hub, &turn.run, true, true),
            Some(true) => {}
        }
    }
    // Looked at after the awaits: a Stop that raced the take has closed its cards by now (R8).
    let live: Vec<Card> = carried.iter().filter_map(|c| hub.store.card(&c.id)).filter(wanted).collect();
    if live.is_empty() && others == 0 {
        return give_up(hub, &turn.run);
    }
    let session = turn.session.as_str();
    if !crate::terminal::claim::safe_session(session) {
        super::end_claimed(hub, &live);
        return give_up(hub, &turn.run);
    }
    let note = if live.len() + others == turn.ids.len() { turn.note.clone() } else { rebuilt(turn, &live) };
    for c in &live {
        let _ = hub.store.card_resume_claim(&c.id, crate::store::now_ms()); // a restart gave the claim back: held again
    }
    let harness = {
        let agent = turn.agent.clone();
        wake::blocking(move || harness_of_agent(&agent)).await.flatten()
    };
    if let Some(why) = engine_gap(t, harness.as_deref()) {
        tracing::warn!(run = %turn.run, why, "wake: a detached turn that cannot run is cancelled before its claim");
        return cancel(core, core_turn, why).await;
    }
    let Some(bin) = crate::agent::hq::bin() else { return unstarted(hub, &turn.run, true, false) };
    let reply = live.iter().find(|c| c.reply_to.is_some());
    let spawn = spawn_with(
        &turn.agent,
        person.as_deref().unwrap_or_default(),
        reply.and_then(|c| c.reply_to.as_deref()),
        session,
        &note,
    );
    let asked = reply.and_then(|c| crate::runauth::AskReply::parse(c.reply_to.as_deref()?, c.asked_by.as_deref()));
    let plan = Plan {
        bin,
        args: spawn.args,
        env: spawn.env,
        asked,
        agent: turn.agent.clone(),
        state: state_of(hub),
        run: turn.run.clone(),
        claim_person: t["person"].as_str().unwrap_or_default().to_owned(),
        conversation: session.to_owned(),
        turn: core_turn,
    };
    let (writer, handle) = (core.clone(), tokio::runtime::Handle::current());
    let started = wake::blocking(move || plan.start(&writer, &handle)).await;
    // A turn that waits behind a writer is not asked again at once: every pass would claim and be refused.
    if matches!(started, Some(Started::Later)) {
        lock(&LATER).insert(turn.run.clone(), Instant::now());
    } else {
        lock(&LATER).remove(&turn.run);
    }
    match started {
        Some(Started::Ran) => clear_held(hub, &live),
        Some(Started::Later) => show_holder(hub, &live, session).await,
        // The module cancelled the turn itself (a launch or spawn error), or the turn is gone.
        Some(Started::Gone | Started::Cancelled(_)) | None => {}
        Some(Started::Failed(why)) => {
            tracing::warn!(run = %turn.run, why, "wake: a run door turn could not start; its turn is cancelled");
            let core = core.clone();
            wake::blocking(move || run_door::free(&core, core_turn, &why)).await;
        }
    }
}

/// Who holds the session, on the cards' entries, while a turn waits behind it (what the app's Stop shows).
async fn show_holder(hub: &Hub, live: &[Card], session: &str) {
    let (state, sid) = (state_of(hub), session.to_owned());
    let held = wake::blocking(move || Reader::new(&state).read(&sid)).await;
    if let Some(Reading::By(h, _)) = held {
        for c in live {
            set_held(hub, c, Some((session, &h)));
        }
    }
}

/// The files a run's words go to.
struct Logs {
    out: std::fs::File,
    err: std::fs::File,
    err_path: std::path::PathBuf,
}

/// `<logs>/wake/<run>.log` and `.err`: where `kith-ask` reads what a follow-up's turn said (`follow_ups`). `None` when the folder
/// cannot be made (the run's words go nowhere; the run still runs).
fn log_files(state: &Path, run: &str) -> Option<Logs> {
    let dir = crate::terminal::core_holders::folders_of(state)?.logs.join("wake");
    std::fs::create_dir_all(&dir).ok()?;
    let open = |path: &Path| std::fs::OpenOptions::new().create(true).append(true).open(path).ok();
    let err_path = dir.join(format!("{run}.err"));
    Some(Logs { out: open(&dir.join(format!("{run}.log")))?, err: open(&err_path)?, err_path })
}

/// What the wake kind gives the detached-run module.
struct Plan {
    bin: std::path::PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// The ask a card's reply answers (its run token carries it).
    asked: Option<crate::runauth::AskReply>,
    agent: String,
    state: std::path::PathBuf,
    run: String,
    claim_person: String,
    conversation: String,
    turn: i64,
}

impl Plan {
    /// Claims the turn and starts `kith AGENT --resume SESSION NOTE` with the claim in `KITH_TURN_CLAIM`, in its own group.
    fn start(self, writer: &Arc<CoreWriter>, handle: &tokio::runtime::Handle) -> Started {
        let Plan { bin, args, env, asked, agent, state, run: run_id, claim_person, conversation, turn } = self;
        let run = Run {
            turn,
            conversation: &conversation,
            person: &claim_person,
            kind: "wakeRun",
            what: "kith --resume",
            ended: "the wake run",
            on_fail: None,
        };
        let (handle, session) = (handle.clone(), conversation.clone());
        run_door::start(writer, &run, move |claim| {
            // The run token is minted here, after the claim: a turn that waits behind a writer mints none. A file or network
            // card's once grant is consumed here too, and the profile built from the result.
            let mut os_env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
                env.iter().map(|(k, v)| (k.into(), v.into())).collect();
            let grant = crate::runauth::grant_for_env(&mut os_env, &agent, &session, asked);
            let mut cmd = Command::new(bin);
            cmd.args(args)
                .env_clear()
                .envs(os_env.iter().map(|(k, v)| (k, v)))
                .env("KITH_TURN_CLAIM", claim.to_string())
                .stdin(Stdio::null());
            let err_file = if let Some(l) = log_files(&state, &run_id) {
                cmd.stdout(l.out).stderr(l.err);
                Some(l.err_path)
            } else {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
                None
            };
            run_door::detach(&mut cmd);
            let bind = grant.map(|grant| {
                Box::new(move |pid: u32| -> Box<dyn std::any::Any + Send> {
                    handle.block_on(grant.bind_pid(pid));
                    Box::new(grant)
                }) as Box<dyn FnOnce(u32) -> Box<dyn std::any::Any + Send> + Send>
            });
            Ok(Launch { cmd, stdin: None, err_file, bind })
        })
    }
}

// ---- what rode on `kith-wake drain`: housekeeping after a tick

/// Starts the lifecycle sweep (once a minute) and the ask bridge (after every tick), each detached and never beside itself.
fn housekeeping() {
    let due = {
        let mut last = lock(&LAST_LIFECYCLE);
        let due = last.is_none_or(|at| at.elapsed() >= LIFECYCLE_EVERY);
        if due {
            *last = Some(Instant::now());
        }
        due
    };
    if due {
        detached(&SWEEPING, lifecycle_command);
    }
    detached(&ASKING, ask_command);
}

/// Runs the command `make` builds on a thread of its own, one at a time per `busy`; silent when there is none to run.
fn detached(busy: &'static AtomicBool, make: fn() -> Option<Command>) {
    if busy.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        if let Some(mut cmd) = make() {
            cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            run_door::detach(&mut cmd);
            match cmd.spawn() {
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(error) => tracing::debug!(%error, "wake: a housekeeping process did not start"),
            }
        }
        busy.store(false, Ordering::Release);
    });
}

/// `kith kernel lifecycle --kernel K sweep`: the 54-minute decision of sessions nobody runs (F12 rule 5). Skipped with no kith.
fn lifecycle_command() -> Option<Command> {
    let kith = crate::agent::hq::bin()?;
    let mut cmd = Command::new(kith);
    cmd.args(["kernel", "lifecycle"]);
    if let Some(kernel) = crate::kernel::current_kernel() {
        cmd.arg("--kernel").arg(kernel);
    }
    cmd.arg("sweep");
    Some(cmd)
}

// Unix-only until retire-python P1-12: the run door's stand-in `kith` is a shell script (no Windows kernel launcher before it).
#[cfg(all(test, unix))]
#[path = "wake_clock_tests.rs"]
mod tests;

/// `kith-ask --sweep`: answers nobody read whose asker's session ended become its wake, until asks are rows of the core. It is
/// the kernel's Python, so a host with no Python (a Windows owner) has no hand-over until the ask item lands (design section 6).
fn ask_command() -> Option<Command> {
    let program = crate::kernel::current_kernel()?.join("bin/kith-ask");
    (cfg!(unix) && program.is_file()).then(|| {
        let mut cmd = Command::new(program);
        cmd.arg("--sweep");
        cmd
    })
}

#[cfg(all(test, unix))]
#[path = "wake_clock_core_tests.rs"]
mod core_tests;
