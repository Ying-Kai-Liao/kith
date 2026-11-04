//! The core's side of the holder (docs/plans/runtime-design.md section 4, steps 5 and 7; M0c).
//!
//! The daemon starts `kith-holder` detached when none answers, says hello with the secret and its generation, and
//! records the holder's `instance` and the processes it listed in core.db. When a hello meets an instance that is not
//! the one recorded, the old holder is gone and on Unix nothing ended its processes: every process group it reported
//! is ended, matched by pid and start time so a reused pid is never hit, and confirmed gone before the new instance is
//! recorded. There are no claims yet (M3), so there is nothing to release.
//!
//! Until M1 nothing uses the holder, so none of this may cost anything: a missing binary, a holder that will not
//! start, one that crashes or one that answers garbage is a line in the core log and a retry later, never an error
//! for `kith serve` or the CLI. Every step here runs on its own thread; startup never waits for it. A `refused`
//! frame is only ever a failed connection: generation authority stays in core.db, so a fake holder cannot make a
//! core step down.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kith_holder::client::{Client, ConnectError};
use kith_holder::{launch, proto};

use crate::attach::Relay;
use crate::daemon::Shared;
use crate::db::{HolderRecord, RecordedProc};
use crate::error::Result;
use crate::layout::Layout;
use crate::terminal::{Placed, Req, STUB_BUSY, SpawnReq};
use crate::{lock, log, perms};

pub(crate) struct HolderConfig {
    /// The holder program. `None`: look for `$KITH_HOLDER_BIN`, then `kith-holder` beside this program.
    pub exe: Option<PathBuf>,
    /// How long a holder with no connection and no process lives before it exits by itself.
    pub idle: Duration,
    /// How often a connected core asks the holder for its list.
    pub poll: Duration,
    pub retry_min: Duration,
    pub retry_max: Duration,
    /// How long ending an old instance's process groups may take before the core tries again later.
    pub kill_wait: Duration,
    /// Reach the holder through this command's stdio instead of the local socket: `ssh <host> kith-holder --stdio`
    /// (program first, then its arguments). The command carries bytes to the holder of the machine it runs on and
    /// starts one there if none answers, so this core starts nothing itself. Tests put `kith-holder --stdio` here.
    pub command: Option<Vec<String>>,
    /// How long a command link may take to answer its hello before it is ended and tried again later.
    pub handshake: Duration,
    /// The lease the core grants the holder: it renews at a third of this, and a holder that hears nothing for this
    /// long kills the processes the core tagged `kill_on_lease_loss`. Nothing is tagged yet.
    pub lease: Duration,
    /// The start stub a held terminal runs its engine under (`terminal.open`). `None`: `$KITH_STUB_BIN`, then
    /// `kith-stub` beside this program.
    pub stub: Option<PathBuf>,
}

impl HolderConfig {
    /// The stub program for a terminal, or `None` when there is none to run.
    pub(crate) fn find_stub(configured: Option<PathBuf>) -> Option<PathBuf> {
        if let Some(e) = configured {
            return e.is_file().then_some(e);
        }
        if let Some(e) = std::env::var_os("KITH_STUB_BIN").filter(|v| !v.is_empty()) {
            let e = PathBuf::from(e);
            return e.is_file().then_some(e);
        }
        let name = if cfg!(windows) {
            "kith-stub.exe"
        } else {
            "kith-stub"
        };
        let beside = std::env::current_exe().ok()?.parent()?.join(name);
        beside.is_file().then_some(beside)
    }
}

impl Default for HolderConfig {
    fn default() -> Self {
        Self {
            exe: None,
            idle: Duration::from_secs(120),
            poll: Duration::from_secs(5),
            retry_min: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            kill_wait: Duration::from_secs(10),
            command: None,
            handshake: Duration::from_secs(15),
            stub: None,
            lease: Duration::from_secs(30),
        }
    }
}

/// What `kith doctor` shows of the holder.
#[derive(Clone, Debug, Default)]
pub(crate) struct HolderState {
    /// `starting`, `connected`, `absent` (no program to start), `unreachable` (retrying), `stopped`.
    pub state: &'static str,
    pub instance: Option<String>,
    pub pid: Option<u32>,
    /// Windows: the holder is in the starter's job, so ending the daemon's job ends it too.
    pub tied_to_starter: bool,
    pub error: Option<String>,
    pub binary: Option<PathBuf>,
    pub procs: usize,
}

fn find_exe(cfg: &HolderConfig) -> Option<PathBuf> {
    if let Some(e) = &cfg.exe {
        return e.is_file().then(|| e.clone());
    }
    if let Some(e) = std::env::var_os("KITH_HOLDER_BIN").filter(|v| !v.is_empty()) {
        let e = PathBuf::from(e);
        return e.is_file().then_some(e);
    }
    let name = if cfg!(windows) {
        "kith-holder.exe"
    } else {
        "kith-holder"
    };
    let beside = std::env::current_exe().ok()?.parent()?.join(name);
    beside.is_file().then_some(beside)
}

/// The holder secret: made once, owner-only, in the core folder.
pub(crate) fn secret(layout: &Layout) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(&layout.holder_secret) {
        let t = text.trim();
        if !t.is_empty() {
            return Ok(t.to_owned());
        }
    }
    let secret = crate::owner::random_secret()?;
    perms::create_private_file(&layout.holder_secret)?;
    std::fs::write(&layout.holder_secret, &secret)?;
    perms::make_private(&layout.holder_secret)?;
    Ok(secret)
}

fn sleep_unless_stopped(shared: &Shared, total: Duration) {
    let end = Instant::now() + total;
    while !shared.stopping() && Instant::now() < end {
        std::thread::sleep(
            Duration::from_millis(40).min(end.saturating_duration_since(Instant::now())),
        );
    }
}

/// Runs until the daemon stops. Never returns an error and never panics the daemon: every failure is a state and a
/// log line.
pub(crate) fn supervise(shared: &Arc<Shared>, layout: &Layout, cfg: &HolderConfig) {
    let mut delay = cfg.retry_min;
    let mut last_error = String::new();
    shared.terminals.set_stub(cfg.stub.clone());
    shared.terminals.set_remote(cfg.command.is_some());
    while !shared.stopping() {
        match connect(shared, layout, cfg) {
            Ok(client) => {
                delay = cfg.retry_min;
                last_error.clear();
                let ended = connected(shared, client, cfg);
                // Attached clients are told the link is gone; terminals asked for while it was down are answered, not
                // left to run late.
                shared.terminals.relay.link_lost();
                shared.terminals.set_link(false);
                match ended {
                    Ok(()) => continue,
                    Err(why) => fail(shared, &mut last_error, "unreachable", &why),
                }
            }
            Err((state, why)) => {
                fail(shared, &mut last_error, state, &why);
                delay = if state == "absent" {
                    cfg.retry_max
                } else {
                    (delay * 2).min(cfg.retry_max)
                };
            }
        }
        sleep_unless_stopped(shared, delay);
    }
    shared.terminals.relay.link_lost();
    shared.terminals.set_link(false);
    shared.set_holder(|h| h.state = "stopped");
}

/// Notes a failure once per distinct message in the core log, so a holder that stays down is one line, not one per try.
fn fail(shared: &Shared, last: &mut String, state: &'static str, why: &str) {
    if *last != why {
        log::note(
            &shared.logs,
            &format!("holder: {why} (will retry; nothing uses the holder before M1)"),
        );
        why.clone_into(last);
    }
    shared.set_holder(|h| {
        h.state = state;
        h.error = Some(why.to_owned());
        h.procs = 0;
    });
}

fn connect(
    shared: &Arc<Shared>,
    layout: &Layout,
    cfg: &HolderConfig,
) -> std::result::Result<Client, (&'static str, String)> {
    let unreachable = |why: String| ("unreachable", why);
    if let Some(argv) = cfg.command.as_deref().filter(|a| !a.is_empty()) {
        return connect_command(shared, layout, argv, cfg.handshake);
    }
    let Some(exe) = find_exe(cfg) else {
        shared.set_holder(|h| h.binary = None);
        return Err((
            "absent",
            "no kith-holder program to start (looked for $KITH_HOLDER_BIN and beside kith)"
                .to_owned(),
        ));
    };
    shared.set_holder(|h| h.binary = Some(exe.clone()));
    let secret = secret(layout).map_err(|e| unreachable(e.to_string()))?;
    let generation = shared
        .with_core(|c| Ok(c.generation()))
        .map_err(|e| unreachable(e.to_string()))?;
    let generation = u64::try_from(generation).unwrap_or(0);
    let dir = &layout.holder_dir;
    let first = match Client::connect(dir, &secret, generation) {
        Err(ConnectError::Io(e)) if nothing_listens(&e) => {
            // Nothing answers: start one detached (its own session; out of our job on Windows), then say hello.
            launch::ensure(&exe, dir, &secret, cfg.idle)
                .map_err(|e| unreachable(format!("can't start {}: {e}", exe.display())))?;
            Client::connect(dir, &secret, generation)
        }
        other => other,
    };
    first.map_err(|e| link_error(e, "at the holder's socket"))
}

/// The holder through a command's stdio (M0f). A dropped link is the same retry as a dropped socket: the supervisor
/// connects again with the same generation, `connected` lists what the holder holds, and exits wait there until acked.
fn connect_command(
    shared: &Arc<Shared>,
    layout: &Layout,
    argv: &[String],
    handshake: Duration,
) -> std::result::Result<Client, (&'static str, String)> {
    let unreachable = |why: String| ("unreachable", why);
    shared.set_holder(|h| h.binary = Some(PathBuf::from(&argv[0])));
    let secret = secret(layout).map_err(|e| unreachable(e.to_string()))?;
    let generation = shared
        .with_core(|c| Ok(c.generation()))
        .map_err(|e| unreachable(e.to_string()))?;
    let generation = u64::try_from(generation).unwrap_or(0);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    Client::over_command(cmd, &secret, generation, handshake)
        .map_err(|e| link_error(e, &format!("through `{}`", argv.join(" "))))
}

fn link_error(e: ConnectError, via: &str) -> (&'static str, String) {
    let why = match e {
        ConnectError::Refused(r) => format!(
            "the holder refused the hello ({:?}); that settles nothing, the generation in core.db is the authority",
            r.why
        ),
        ConnectError::Impostor => {
            format!("what answers {via} does not know the holder secret")
        }
        ConnectError::Io(e) => format!("the holder did not answer properly: {e}"),
    };
    ("unreachable", why)
}

fn nothing_listens(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    matches!(e.kind(), NotFound | ConnectionRefused)
}

/// Ends the renewal thread when the link's loop returns, however it returns.
struct RenewalGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for RenewalGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Keeps the lease alive: a ping at once, then one from a thread of its own every third of the lease, never behind a
/// listing or a reconcile, which can take long enough on a loaded machine for a leftover deadline to lapse. A renewal
/// is a round (verification/lease): the hand answers PONG(seq) and remembers its clock, and the client's reader thread
/// answers that PONG with RENEW(seq, lease), so the hand's deadline is its own clock when it read the PING plus the
/// lease (so at or after the core's send plus the lease). That is safe because the core's belief starts later, at the
/// PONG receipt. The same reader pings again at once on a `LEASE_LOST` (at most one such ping outstanding), so a
/// refused tagged spawn can be retried as soon as the lease is armed.
///
/// What the core may assume: every RENEW it sent says "lease from now", so it stands behind a hand's guests for at
/// most the send time plus `lease`. Nothing releases or requeues a guest run on that belief yet (no run is tagged
/// `kill_on_lease_loss` today). When one does, it must wait for belief + eps*lease (eps: how slow the hand clock may
/// run) + the hand's 10 ms watcher + kill latency, or for the hand's report of the kill (`Exit::lapsed`, or
/// `Welcome::lease_lost` on reconnect), whichever comes first (skill `holder-lease`, row `belief`).
fn start_renewing(
    shared: &Arc<Shared>,
    client: &Client,
    cfg: &HolderConfig,
) -> std::result::Result<RenewalGuard, String> {
    let renewer = client.renewer();
    let lease = cfg.lease;
    client.answer_pongs(lease);
    renewer
        .ping()
        .map_err(|e| format!("the holder stopped answering: {e}"))?;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stop, shared) = (done.clone(), shared.clone());
    let every = (lease / 3).max(Duration::from_millis(1));
    std::thread::spawn(move || {
        let stopped = || stop.load(std::sync::atomic::Ordering::Relaxed) || shared.stopping();
        let mut due = Instant::now() + every;
        while !stopped() {
            std::thread::sleep(Duration::from_millis(10).min(every));
            if Instant::now() >= due {
                if renewer.ping().is_err() {
                    return; // the link is gone; its loop notices on its next listing
                }
                due = Instant::now() + every;
            }
        }
    });
    Ok(RenewalGuard(done))
}

/// Reconciles the record with this hello, then keeps the record current until the connection ends. `Ok` means the
/// supervisor was told to stop; `Err` carries why the link was lost.
fn connected(
    shared: &Arc<Shared>,
    mut client: Client,
    cfg: &HolderConfig,
) -> std::result::Result<(), String> {
    // First thing after the welcome: a deadline left over from before this link must not lapse while we reconcile.
    let _renewing = start_renewing(shared, &client, cfg)?;
    // A holder reached through a command runs on another machine: its pids mean nothing here, so no start time is
    // read and no process group is ended on this machine's account, and the local holder's record is left alone.
    let remote = cfg.command.is_some();
    let procs = if remote {
        Vec::new()
    } else {
        record_procs(&client.welcome.procs)
    };
    let instance = client.welcome.instance.clone();
    let (pid, tied) = (client.welcome.pid, client.welcome.tied_to_starter);
    let count = client
        .welcome
        .procs
        .iter()
        .filter(|h| h.exit.is_none())
        .count();
    if remote {
        log::note(
            &shared.logs,
            "holder: linked through a command; its pids are not this machine's, so nothing is recorded or ended here",
        );
    } else {
        reconcile(shared, &instance, &procs, cfg)?;
    }
    // Before the state says connected: whoever waits for that state may call terminal.open at once.
    shared
        .terminals
        .relay
        .link_up(client.sender(), &client.welcome.procs);
    shared.terminals.set_link(true);
    shared.set_holder(|h| {
        h.state = "connected";
        h.instance = Some(instance.clone());
        h.pid = Some(pid);
        h.tied_to_starter = tied;
        h.error = None;
        h.procs = count;
    });
    log::note(
        &shared.logs,
        &format!("holder: connected (instance {instance}, pid {pid}, {count} process(es))"),
    );
    let mut known = procs;
    let mut next_list = Instant::now() + cfg.poll;
    while !shared.stopping() {
        // A terminal to start is served at once; the listing keeps its own pace. With viewers attached the wait is
        // short: it is also how long their output waits.
        let relay = &shared.terminals.relay;
        let busy = relay.busy();
        let wait = if busy {
            Duration::ZERO
        } else {
            cfg.poll.min(Duration::from_millis(100))
        };
        match shared.terminals.next(wait) {
            Some(Req::Spawn(req)) => serve_spawn(&mut client, relay, &req)?,
            Some(Req::End(held)) => end_held(&mut client, relay, &held),
            None => {}
        }
        let pause = if busy { PUMP_WAIT } else { Duration::ZERO };
        pump(&mut client, relay, &instance, &shared.logs, pause)?;
        if shared.stopping() {
            break;
        }
        if Instant::now() < next_list {
            continue;
        }
        next_list = Instant::now() + cfg.poll;
        let held = client
            .list()
            .map_err(|e| format!("the holder stopped answering: {e}"))?;
        if remote {
            let live = held.iter().filter(|h| h.exit.is_none()).count();
            shared.set_holder(|h| h.procs = live);
            continue;
        }
        let now = record_procs(&held);
        if now != known {
            reconcile(shared, &instance, &now, cfg)?;
            shared.set_holder(|h| h.procs = now.len());
            known = now;
        }
    }
    Ok(())
}

/// How long the supervisor waits for the holder's next frame while viewers are attached.
const PUMP_WAIT: Duration = Duration::from_millis(5);
/// The most frames one turn of the supervisor reads before it looks at its other work.
const PUMP_BATCH: usize = 512;

/// Reads the holder's output, exits and tails in the order they came and hands them to the attach relay (which only
/// copies them into the viewers' buffers, so this never waits on a viewer). A tail that answers the end of a terminal
/// is acked here, once the relay has sent EXIT to the viewers and kept the tail.
fn pump(
    client: &mut Client,
    relay: &Relay,
    instance: &str,
    logs: &Path,
    first_wait: Duration,
) -> std::result::Result<(), String> {
    let mut wait = first_wait;
    for _ in 0..PUMP_BATCH {
        let Some((kind, id, payload)) = client.take(wait, |f| {
            matches!(
                f.0,
                proto::OUTPUT | proto::STDERR | proto::EXITED | proto::TAILED
            )
        }) else {
            return Ok(());
        };
        wait = Duration::ZERO;
        match kind {
            proto::OUTPUT => relay.output(id, &payload),
            proto::EXITED => {
                // An exit the relay does not know (or cannot read) is acked all the same, so the holder forgets it.
                let known = match serde_json::from_slice::<proto::Exit>(&payload) {
                    Ok(exit) => relay.exited(id, &exit),
                    Err(e) => {
                        log::note(
                            logs,
                            &format!("holder: unreadable exit of process {id} ({e}); acked"),
                        );
                        false
                    }
                };
                if !known {
                    let held = proto::HeldId {
                        instance: instance.to_owned(),
                        id,
                    };
                    client
                        .ack(&held)
                        .map_err(|e| format!("the holder stopped answering: {e}"))?;
                }
            }
            proto::TAILED => {
                if let Some(held) = relay.tailed(id, &payload) {
                    client
                        .ack(&held)
                        .map_err(|e| format!("the holder stopped answering: {e}"))?;
                }
            }
            _ => {} // a pipes process's stderr: nobody attaches to those
        }
    }
    Ok(())
}

/// How many started terminals were ended because their caller had given up (a test reads it to see the late path).
#[cfg(test)]
pub(crate) static LATE_ENDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Ends a held process the core no longer wants (kill until the holder says its group is gone, then ack so it forgets
/// it). A broken link is the supervisor's to find on its next turn.
fn end_held(client: &mut Client, relay: &Relay, held: &proto::HeldId) {
    relay.forget(held);
    for _ in 0..40 {
        match client.kill(held) {
            Ok(proto::KillState::Killing) => std::thread::sleep(Duration::from_millis(50)),
            Ok(_) => break,
            Err(_) => return,
        }
    }
    let _ = client.ack(held);
}

/// Puts one spawn to the holder and tells the caller what came of it. `Err` is a broken link (the caller was told).
fn serve_spawn(
    client: &mut Client,
    relay: &Relay,
    req: &SpawnReq,
) -> std::result::Result<(), String> {
    if Instant::now() >= req.deadline {
        let _ = req
            .reply
            .send(Placed::Failed("the holder link was down".into()));
        return Ok(());
    }
    let spec = &req.spec;
    if let Err(e) = client.spawn(spec) {
        let _ = req
            .reply
            .send(Placed::Failed(format!("the holder link broke: {e}")));
        return Err(format!("the holder stopped answering: {e}"));
    }
    let (conv, claim) = (spec.conv.clone(), spec.claim);
    let mine = |f: &kith_holder::wire::Frame| {
        matches!(
            f.0,
            proto::SPAWNED | proto::SPAWN_FAILED | proto::STALE_CLAIM
        ) && serde_json::from_slice::<serde_json::Value>(&f.2)
            .is_ok_and(|v| v["conv"] == conv.as_str() && v["claim"].as_u64() == Some(claim))
    };
    let Some((kind, _, payload)) = client.take(Duration::from_secs(10), mine) else {
        let _ = req
            .reply
            .send(Placed::Failed("the holder did not answer the spawn".into()));
        return Err("the holder did not answer the spawn".into());
    };
    let placed = match kind {
        proto::SPAWNED => match serde_json::from_slice::<proto::Spawned>(&payload) {
            Ok(s) => {
                let placed = watch_start(client, s.held_id(), s.pid, req.busy_wait);
                if let Placed::Started { held, pid } = &placed {
                    // Listed before the caller hears of it, so its ticket never meets an unknown terminal.
                    relay.register(held, &spec.conv, &req.agent, *pid, (spec.cols, spec.rows));
                }
                placed
            }
            Err(e) => Placed::Failed(format!("unreadable answer: {e}")),
        },
        proto::SPAWN_FAILED => Placed::Failed(
            serde_json::from_slice::<proto::SpawnFailed>(&payload)
                .map_or_else(|_| "spawn failed".into(), |f| f.error),
        ),
        _ => Placed::Stale,
    };
    // A caller that gave up (its receiver is gone) must not leave the terminal it asked for running with no claim and
    // no ticket: it is ended here.
    if let Err(mpsc::SendError(Placed::Started { held, .. })) = req.reply.send(placed) {
        #[cfg(test)]
        LATE_ENDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        end_held(client, relay, &held);
    }
    Ok(())
}

/// A new process that ends within `wait` did not start a terminal: the stub's exit 75 is "another writer holds this
/// key", anything else is a failure. Either way it is acked so the holder forgets it.
fn watch_start(client: &mut Client, held: proto::HeldId, pid: u32, wait: Duration) -> Placed {
    let Some((_, _, payload)) = client.expect(proto::EXITED, held.id, wait) else {
        return Placed::Started { held, pid };
    };
    let exit = serde_json::from_slice::<proto::Exit>(&payload).ok();
    let _ = client.ack(&held);
    match exit {
        Some(e) if e.code == Some(STUB_BUSY) => Placed::Busy,
        Some(e) => Placed::Failed(format!(
            "the terminal ended at once (code {:?}, signal {:?}{})",
            e.code,
            e.signal,
            e.error.map_or(String::new(), |m| format!(", {m}"))
        )),
        None => Placed::Failed("the terminal ended at once".into()),
    }
}

/// The listed processes with the start time the system gives their pid now, noted while the holder is alive so a
/// later kill can tell them from a stranger that was handed the pid.
fn record_procs(held: &[proto::Held]) -> Vec<RecordedProc> {
    held.iter()
        .filter(|h| h.exit.is_none())
        .map(|h| RecordedProc {
            pid: h.pid,
            started: lock::process_start(h.pid).unwrap_or_default(),
            conv: h.conv.clone(),
        })
        .collect()
}

/// Step 7: a hello that meets an instance other than the recorded one ends what the old one held, confirms it gone,
/// and only then records the new instance. A record from M0a has no instance: nothing is ended on its account.
pub(crate) fn reconcile(
    shared: &Shared,
    instance: &str,
    procs: &[RecordedProc],
    cfg: &HolderConfig,
) -> std::result::Result<(), String> {
    let recorded = shared
        .with_core(|c| c.holder_record())
        .map_err(|e| e.to_string())?;
    if let Some(old) = recorded.instance.as_deref().filter(|old| *old != instance) {
        // Never a process the new instance lists: it is alive and its own by that holder's account.
        let stale: Vec<RecordedProc> = recorded
            .procs
            .iter()
            .filter(|p| !procs.iter().any(|n| n.pid == p.pid))
            .cloned()
            .collect();
        log::note(
            &shared.logs,
            &format!(
                "holder: instance {old} is gone, now {instance}; ending {} process group(s) it held",
                stale.len()
            ),
        );
        if !end_groups(&shared.logs, &stale, cfg.kill_wait) {
            return Err(format!(
                "processes of the old holder instance {old} are still running; not recording the new one yet"
            ));
        }
    }
    if recorded.instance.as_deref() != Some(instance) || recorded.procs != procs {
        let rec = HolderRecord {
            instance: Some(instance.to_owned()),
            procs: procs.to_vec(),
        };
        shared
            .with_core(|c| c.save_holder_record(&rec))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Ends the process group of each recorded process whose pid still has the recorded start time, and waits for the
/// groups to be empty. A pid with another start time (or none we can read) is not touched. True when nothing it was
/// asked to end is left.
#[cfg(unix)]
fn end_groups(logs: &Path, procs: &[RecordedProc], wait: Duration) -> bool {
    let mut mine = Vec::new();
    for p in procs {
        if p.started.is_empty() {
            log::note(
                logs,
                &format!(
                    "holder: pid {} has no recorded start time; left alone",
                    p.pid
                ),
            );
        } else if lock::process_start(p.pid).as_deref() == Some(p.started.as_str()) {
            mine.push(p.pid);
        } else {
            log::note(
                logs,
                &format!(
                    "holder: pid {} is not the process that was recorded (another start time, or gone); left alone",
                    p.pid
                ),
            );
        }
    }
    if mine.is_empty() {
        return true;
    }
    for pid in &mine {
        signal_group(*pid, "TERM");
    }
    let begun = Instant::now();
    let mut forced = false;
    loop {
        mine.retain(|pid| group_alive(*pid));
        if mine.is_empty() {
            return true;
        }
        if begun.elapsed() >= wait {
            return false;
        }
        // Politely first; whatever ignored it gets the hard signal after a third of the time.
        if !forced && begun.elapsed() >= wait / 3 {
            for pid in &mine {
                signal_group(*pid, "KILL");
            }
            forced = true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Windows needs no extra step: each spawn's job object ends with the holder (runtime-design section 4, step 7).
#[cfg(not(unix))]
fn end_groups(_: &Path, _: &[RecordedProc], _: Duration) -> bool {
    true
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) -> bool {
    // An absolute path, never `kill` from PATH: this ends process groups, and the daemon's PATH is not its own to trust.
    // (unsafe code is forbidden here, so no killpg.)
    let kill = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .unwrap_or("/bin/kill");
    std::process::Command::new(kill)
        .args(["-s", signal, "--"])
        .arg(format!("-{pid}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    signal_group(pid, "0")
}
