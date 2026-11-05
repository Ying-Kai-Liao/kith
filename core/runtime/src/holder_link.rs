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

use std::collections::HashMap;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kith_holder::client::{Client, ConnectError};
use kith_holder::{launch, proto};
use kith_kernel::procstart;

use crate::attach::Relay;
use crate::conversations::claims::{self, Redeemed};
use crate::daemon::Shared;
use crate::db::{Core, HolderRecord, RecordedProc};
use crate::error::Result;
use crate::layout::Layout;
use crate::terminal::{
    END_STUCK_ROUNDS, Placed, Req, STUB_BUSY, SpawnReq, TurnDone, TurnOp, TurnReq,
};
use crate::{log, perms};

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
    /// `starting`, `connected`, `absent` (no program to start), `unreachable` (retrying), `blocked` (the old instance's processes
    /// cannot be ended or told apart; retrying slowly, nothing signalled), `stopped`.
    pub state: &'static str,
    pub instance: Option<String>,
    pub pid: Option<u32>,
    /// Windows: the holder is in the starter's job, so ending the daemon's job ends it too.
    pub tied_to_starter: bool,
    pub error: Option<String>,
    pub binary: Option<PathBuf>,
    pub procs: usize,
    /// The pids of the processes the connected holder lists (this machine's; empty for a command link).
    pub pids: Vec<u32>,
    /// Ends that have not finished: kills the core keeps sending (`kith doctor` says so past a few rounds).
    pub ending: usize,
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
                let ended = connected(shared, client, cfg);
                // Attached clients are told the link is gone; terminals asked for while it was down are answered, not
                // left to run late.
                shared.terminals.relay.link_lost();
                shared.terminals.set_link(false);
                match ended {
                    Ok(()) => {
                        delay = cfg.retry_min;
                        last_error.clear();
                        continue;
                    }
                    // A wedged reconcile connects fine every time: resetting here would turn it into a reconnect a
                    // second, two log lines each. Keep what failed and back off as for an absent program.
                    Err(Lost::Blocked(why)) => {
                        fail(shared, &mut last_error, "blocked", &why);
                        delay = cfg.retry_max;
                    }
                    Err(Lost::Link(why)) => {
                        delay = cfg.retry_min;
                        last_error.clear();
                        fail(shared, &mut last_error, "unreachable", &why);
                    }
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

/// Why a connected link ended.
#[derive(Debug)]
pub(crate) enum Lost {
    /// The holder stopped answering, or the core could not keep its record.
    Link(String),
    /// Processes of the old holder instance are still there and cannot be ended or told apart from strangers: the
    /// new instance is not recorded, nothing is signalled, and the same hello meets the same answer until they go.
    Blocked(String),
}

impl From<String> for Lost {
    fn from(why: String) -> Self {
        Lost::Link(why)
    }
}

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Lost::Link(why) | Lost::Blocked(why)) = self;
        f.write_str(why)
    }
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
        h.pids.clear();
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
/// most the core's clock at the PONG receipt plus `lease`
/// (the hand's own deadline is anchored at its clock on reading the PING, so it lapses no later). Nothing releases or requeues a guest run on that belief yet (no run is tagged
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
) -> std::result::Result<(), Lost> {
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
    // What the core keeps of each held process (its claim, pid and conversation), so an exit the holder reports can be
    // told to the claim it ended, including one that happened while this core was down (the welcome lists it).
    let mut seen = Known::default();
    seen.note_all(&client.welcome.procs);
    for h in client.welcome.procs.iter().filter(|h| h.exit.is_some()) {
        if let Some(exit) = &h.exit {
            report_exit(shared, h.claim, h.pid, exit);
        }
    }
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
        h.pids = procs.iter().map(|p| p.pid).collect();
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
            Some(Req::Spawn(req)) => {
                serve_spawn(&mut client, relay, &req, &mut seen)?;
                for held in seen.unfinished.drain(..) {
                    shared.terminals.requeue_end(held, 0);
                }
            }
            Some(Req::End(held)) => {
                // Until the holder says the tree is gone the kill goes again (Claims4 finding 4): the request goes
                // back to the queue, and a link that broke under it keeps it for the next connection.
                if !end_held(&mut client, relay, &held) {
                    shared.terminals.requeue_end(held, 0);
                    shared.set_holder(|h| h.ending = shared.terminals.ending());
                }
            }
            Some(Req::Turn(req)) => serve_turn(shared, &mut client, &seen, &req),
            None => {}
        }
        resend_ends(shared, &mut client, relay);
        let pause = if busy { PUMP_WAIT } else { Duration::ZERO };
        pump(&mut client, relay, shared, &mut seen, pause)?;
        if shared.stopping() {
            break;
        }
        if Instant::now() < next_list {
            continue;
        }
        next_list = Instant::now() + cfg.poll;
        shared.replay_open_removals();
        let held = client
            .list()
            .map_err(|e| format!("the holder stopped answering: {e}"))?;
        if remote {
            let live = held.iter().filter(|h| h.exit.is_none()).count();
            shared.set_holder(|h| h.procs = live);
            continue;
        }
        seen.note_all(&held);
        let now = record_procs(&held);
        if now != known {
            reconcile(shared, &instance, &now, cfg)?;
            shared.set_holder(|h| {
                h.procs = now.len();
                h.pids = now.iter().map(|p| p.pid).collect();
            });
            known = now;
        }
    }
    Ok(())
}

/// How many kills a re-sent end may send in one round.
const RESEND_TRIES: u32 = 3;

/// Sends again the kills whose wait is over. One that is still not gone goes back with one more round; past
/// [`END_STUCK_ROUNDS`] the core log says so once, and `holder.ending` (in `kith doctor`) counts it.
fn resend_ends(shared: &Shared, client: &mut Client, relay: &Relay) {
    for (claim, proc) in shared.terminals.take_owed_claim_ends() {
        if !try_end_stuck_claim(shared, claim, proc.as_ref()) {
            shared.terminals.owe_claim_end(claim, proc);
            log::note(
                &shared.logs,
                &format!("holder: could not end claim {claim} of a stuck lock yet; asking again"),
            );
        }
    }
    let due = shared.terminals.due_ends();
    if due.is_empty() {
        return;
    }
    for (held, rounds) in due {
        // A few kills a round, not forty: a tree that will not die must not keep the supervisor out of its pump.
        if end_held_within(client, relay, &held, RESEND_TRIES) {
            continue;
        }
        let rounds = shared.terminals.requeue_end(held.clone(), rounds);
        if rounds == END_STUCK_ROUNDS {
            log::note(
                &shared.logs,
                &format!(
                    "holder: process {} of instance {} is still not gone after {rounds} rounds of kills; the core keeps sending them",
                    held.id, held.instance
                ),
            );
        }
    }
    shared.set_holder(|h| h.ending = shared.terminals.ending());
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
    shared: &Shared,
    seen: &mut Known,
    first_wait: Duration,
) -> std::result::Result<(), String> {
    let (instance, logs) = (client.welcome.instance.clone(), shared.logs.as_path());
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
                    Ok(exit) => {
                        // The claim hears of it before the relay does: a claim never outlives the process it names.
                        if let Some((claim, pid, _)) = seen.of(id) {
                            report_exit(shared, claim, pid, &exit);
                        }
                        relay.exited(id, &exit)
                    }
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
                        instance: instance.clone(),
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
/// it). False when it is not done: the holder still said `killing` after the tries, or the link broke under it. The kill
/// is then sent again (the caller puts the end back in the queue), never given up on and never acked while a process of
/// the tree may be alive (Claims4 finding 4, row `c4-nokillresend`).
fn end_held(client: &mut Client, relay: &Relay, held: &proto::HeldId) -> bool {
    end_held_within(client, relay, held, 40)
}

/// [`end_held`] with the number of kills it may send before it gives the end back to its caller.
fn end_held_within(client: &mut Client, relay: &Relay, held: &proto::HeldId, tries: u32) -> bool {
    relay.forget(held);
    for _ in 0..tries {
        match client.kill(held) {
            Ok(proto::KillState::Killing) => std::thread::sleep(Duration::from_millis(50)),
            Ok(_) => {
                let _ = client.ack(held);
                return true;
            }
            Err(_) => return false,
        }
    }
    false
}

/// Puts one spawn to the holder and tells the caller what came of it. `Err` is a broken link (the caller was told).
fn serve_spawn(
    client: &mut Client,
    relay: &Relay,
    req: &SpawnReq,
    seen: &mut Known,
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
                    seen.procs
                        .insert(held.id, (spec.claim, *pid, spec.conv.clone()));
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
        if !end_held(client, relay, &held) {
            seen.unfinished.push(held);
        }
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

/// The listed processes with the start stamp (`kith_kernel::procstart`) the system gives their pid now, noted while the
/// holder is alive so a later kill can tell them from a stranger that was handed the pid. One that has gone since the
/// holder listed it is dropped (a pid recorded with no start could be a stranger's by the next poll); one that is there
/// but whose start cannot be read is recorded with an empty one, which a later kill treats as unknown.
pub(crate) fn record_procs(held: &[proto::Held]) -> Vec<RecordedProc> {
    held.iter()
        .filter(|h| h.exit.is_none())
        .filter_map(|h| {
            let started = match procstart::probe(h.pid) {
                procstart::Probe::Gone => return None,
                procstart::Probe::Alive(start) => start.unwrap_or_default(),
                procstart::Probe::Unknown => String::new(),
            };
            Some(RecordedProc {
                pid: h.pid,
                started,
                conv: h.conv.clone(),
            })
        })
        .collect()
}

/// The serve host's instance name on its turn claims; no holder may say hello under it (see [`reconcile`]).
pub(crate) const HOST_INSTANCE: &str = "kith-serve-host";

/// Step 7: a hello that meets an instance other than the recorded one ends what the old one held, confirms it gone,
/// and only then records the new instance. A record from M0a has no instance: nothing is ended on its account.
///
/// Instance ids and the host: the serve host names its own turn claims `kith-serve-host` (`HOST_INSTANCE` in
/// `serve/host/src/terminal/core_claims.rs`) and, after a restart, ends any claim carrying that name as its own stale
/// one. A holder that said hello under the same id would have its live claims ended by the host, so the hello is
/// refused and nothing is recorded. Open: `claims::different_holder` (step 7 of the claims) is not run from this hello
/// yet; the M3 holder owns that wiring, and until it lands only the process groups below are ended here. A record
/// stored before this rule is not re-read as a refusal: only the hello is checked.
pub(crate) fn reconcile(
    shared: &Shared,
    instance: &str,
    procs: &[RecordedProc],
    cfg: &HolderConfig,
) -> std::result::Result<(), Lost> {
    if instance == HOST_INSTANCE {
        return Err(Lost::Blocked(format!(
            "the holder's instance id {instance:?} is the serve host's own claim name; not recorded"
        )));
    }
    let recorded = shared
        .with_core(|c| c.holder_record())
        .map_err(|e| Lost::Link(e.to_string()))?;
    if let Some(old) = recorded.instance.as_deref().filter(|old| *old != instance) {
        // Never a process the new instance lists: it is alive and its own by that holder's account.
        let stale: Vec<RecordedProc> = recorded
            .procs
            .iter()
            .filter(|p| !procs.iter().any(|n| n.pid == p.pid))
            .cloned()
            .collect();
        // The notes are logged only when the pass gets through: a blocked one comes back on every hello, and its
        // reason goes out once, through the supervisor's one-line-per-message failure note.
        let mut notes = Vec::new();
        if let Err(stuck) = end_groups(&mut notes, &stale, cfg.kill_wait) {
            // What was ended on the way is logged all the same: the groups signalled before one would not end are
            // gone, and the log is where anyone looks for what the core did to processes.
            for n in notes {
                log::note(&shared.logs, &n);
            }
            return Err(Lost::Blocked(format!(
                "processes of the old holder instance {old} are still running and not ended: {stuck}; not recording the new instance yet"
            )));
        }
        log::note(
            &shared.logs,
            &format!(
                "holder: instance {old} is gone, now {instance}; ended {} process group(s) it held",
                stale.len()
            ),
        );
        for n in notes {
            log::note(&shared.logs, &n);
        }
    }
    if recorded.instance.as_deref() != Some(instance) || recorded.procs != procs {
        let rec = HolderRecord {
            instance: Some(instance.to_owned()),
            procs: procs.to_vec(),
        };
        shared
            .with_core(|c| c.save_holder_record(&rec))
            .map_err(|e| Lost::Link(e.to_string()))?;
    }
    Ok(())
}

/// Ends the process group of each recorded process whose pid still has the recorded start stamp, and waits for the
/// groups to be empty. A pid that is gone, or has another start in the same form, is not ours: left alone. So is a
/// live pid that is not a session leader: every holder spawn calls `setsid`, so the recorded process leads its session
/// and anything else at that pid is a stranger, whatever the record says. A start that cannot be told otherwise (the
/// pid's own cannot be read, or the record is empty, old `ps` text, or of another form) is neither: nothing is
/// signalled for it and it comes back in the `Err`, as does a group that will not end, so the caller keeps the old
/// record and the next hello looks again. `notes` gets what happened to the ones that were not in doubt.
#[cfg(unix)]
fn end_groups(
    notes: &mut Vec<String>,
    procs: &[RecordedProc],
    wait: Duration,
) -> std::result::Result<(), String> {
    let mut mine = Vec::new();
    let mut stuck: Vec<String> = Vec::new();
    for p in procs {
        match procstart::probe(p.pid) {
            procstart::Probe::Gone => {
                notes.push(format!("holder: pid {} is gone; nothing to end", p.pid));
            }
            procstart::Probe::Alive(Some(now)) if procstart::comparable(&p.started, &now) => {
                if procstart::same_process(&p.started, &now) {
                    mine.push(p.pid);
                } else {
                    notes.push(format!(
                        "holder: pid {} is not the process that was recorded (another start); left alone",
                        p.pid
                    ));
                }
            }
            procstart::Probe::Alive(_) | procstart::Probe::Unknown => {
                if procstart::session_id(p.pid).is_some_and(|sid| sid != p.pid) {
                    notes.push(format!(
                        "holder: pid {} is not a session leader, so not the process that was recorded; left alone",
                        p.pid
                    ));
                } else {
                    stuck.push(format!(
                        "pid {} is running but its start cannot be told from the record ({:?})",
                        p.pid, p.started
                    ));
                }
            }
        }
    }
    if !mine.is_empty() {
        let signalled = mine.clone();
        for pid in &mine {
            signal_group(*pid, "TERM");
        }
        let begun = Instant::now();
        let mut forced = false;
        loop {
            mine.retain(|pid| group_alive(*pid));
            if mine.is_empty() {
                break;
            }
            if begun.elapsed() >= wait {
                stuck.extend(mine.iter().map(|pid| {
                    format!(
                        "the group of pid {pid} did not end within {}s",
                        wait.as_secs()
                    )
                }));
                break;
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
        for pid in signalled.into_iter().filter(|pid| !mine.contains(pid)) {
            notes.push(format!("holder: ended the process group of pid {pid}"));
        }
    }
    if stuck.is_empty() {
        Ok(())
    } else {
        Err(stuck.join("; "))
    }
}

/// Windows needs no extra step: each spawn's job object ends with the holder (runtime-design section 4, step 7).
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // the same shape as the Unix one
fn end_groups(
    _: &mut Vec<String>,
    _: &[RecordedProc],
    _: Duration,
) -> std::result::Result<(), String> {
    Ok(())
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

// --- A warm turn's writer lock, through the holder (M3-4) ----------------------------------------------------------

/// What the holder answered to a take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnLock {
    /// The conversation's lock is held by the process's resident stub for this claim (a take repeated is `Locked` again).
    Locked,
    /// Another writer holds the conversation (a terminal's stub, a `--direct` run, another claim's turn).
    Busy,
    /// Nothing was locked: a stale or already-released claim, a bad key, or the stub or process is gone (the reason).
    Error(String),
}

/// What the holder answered to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnRelease {
    Released,
    /// That claim's lock was not held (never taken, already released, another claim's, or the stub is gone). The lock,
    /// if any other claim holds it, stays.
    NotHeld,
    Error(String),
}

/// An answer and the claim id it is for. The claim id rides on everything (runtime-design section 4): a caller acts
/// only on an answer for its current claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answered<T> {
    pub claim: u64,
    pub result: T,
}

/// Asks the holder to take conversation lock `key` for `claim` through the resident stub of the warm process `held`.
///
/// This only sends the request and returns the typed answer. It does not read or write claims, turns or reports:
/// [`begin_turn`] calls this from the `claimed -> running` step (a `Busy` there ends the claim and parks the turn) and
/// [`finish_turn`] calls `release_turn_lock` from the turn's end. An `Err` is a
/// broken or silent link; the take may still have happened, and asking again is safe (the same claim is `Locked`
/// again).
pub(crate) fn take_turn_lock(
    client: &mut Client,
    held: &proto::HeldId,
    key: &str,
    claim: u64,
) -> std::io::Result<Answered<TurnLock>> {
    let a = client.take_lock(held, key, claim)?;
    let result = match a.word {
        proto::LockWord::Locked => TurnLock::Locked,
        proto::LockWord::Busy => TurnLock::Busy,
        proto::LockWord::Error => TurnLock::Error(a.reason.unwrap_or_default()),
        other => TurnLock::Error(format!("unexpected answer {other:?} to a take")),
    };
    Ok(Answered {
        claim: a.claim,
        result,
    })
}

/// Releases the lock `take_turn_lock` took, at the turn's end. Like the take, it touches no claim, turn or report
/// row. A release asked again is `NotHeld`, which is fine; a lost answer only delays.
pub(crate) fn release_turn_lock(
    client: &mut Client,
    held: &proto::HeldId,
    key: &str,
    claim: u64,
) -> std::io::Result<Answered<TurnRelease>> {
    let a = client.release_lock(held, key, claim)?;
    let result = match a.word {
        proto::LockWord::Released => TurnRelease::Released,
        proto::LockWord::NotHeld => TurnRelease::NotHeld,
        proto::LockWord::Error => TurnRelease::Error(a.reason.unwrap_or_default()),
        other => TurnRelease::Error(format!("unexpected answer {other:?} to a release")),
    };
    Ok(Answered {
        claim: a.claim,
        result,
    })
}

// --- A warm turn's claim: lock, redeem, report (M3-10b3) -------------------------------------------------------------

/// Where the turn steps reach core.db. The holder round trips (up to seconds) must not run under the core's lock, so the
/// steps ask for the core only around their own transactions.
pub(crate) trait CoreAccess {
    fn with<R>(&self, f: impl FnOnce(&mut Core) -> Result<R>) -> Result<R>;
}

impl CoreAccess for Shared {
    fn with<R>(&self, f: impl FnOnce(&mut Core) -> Result<R>) -> Result<R> {
        self.with_core(f)
    }
}

#[cfg(test)]
impl CoreAccess for std::sync::Mutex<Core> {
    fn with<R>(&self, f: impl FnOnce(&mut Core) -> Result<R>) -> Result<R> {
        f(&mut self
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

/// Milliseconds since the epoch, the clock of the claim rows.
pub(crate) fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
    )
    .unwrap_or(i64::MAX)
}

/// What the core keeps of each process the holder lists, by the holder's id for it: the claim it was spawned for, its
/// pid and its conversation.
#[derive(Default)]
pub(crate) struct Known {
    procs: HashMap<u32, (u64, u32, String)>,
    /// Ends that did not finish in a spawn's late path; the supervisor puts them back in the queue.
    unfinished: Vec<proto::HeldId>,
}

impl Known {
    fn note_all(&mut self, held: &[proto::Held]) {
        for h in held {
            self.procs.insert(h.id, (h.claim, h.pid, h.conv.clone()));
        }
    }

    fn of(&self, id: u32) -> Option<(u64, u32, String)> {
        self.procs.get(&id).cloned()
    }
}

/// The body of an exit report: the process it names (the claim's rule is by pid: one that names another pid changes
/// nothing, one that names none moves the claim to `ending`) and how it ended. `out_of_usage` is the stub's or the
/// engine's word that the account ran out (`accounts::failover`): the holder's own exit never says it.
pub(crate) fn exit_body(
    pid: Option<u32>,
    exit: Option<&proto::Exit>,
    out_of_usage: bool,
    why: Option<&str>,
) -> String {
    let mut body = serde_json::json!({"out_of_usage": out_of_usage});
    if let Some(pid) = pid {
        body["pid"] = pid.into();
    }
    if let Some(e) = exit {
        body["code"] = e.code.into();
        body["signal"] = e.signal.into();
        body["lapsed"] = e.lapsed.into();
        if let Some(err) = &e.error {
            body["error"] = err.clone().into();
        }
    }
    if let Some(why) = why {
        body["why"] = why.into();
    }
    body.to_string()
}

/// A holder exit for the claim the process was spawned for. A claim id the core does not know (a terminal started
/// before claims, a process of another core) is stored nowhere and changes nothing. A failure is logged, never raised: the
/// exit stays in the holder until acked and the next listing offers it again.
fn report_exit(shared: &Shared, claim: u64, pid: u32, exit: &proto::Exit) {
    let Ok(claim) = i64::try_from(claim) else {
        return;
    };
    let body = exit_body(Some(pid), Some(exit), false, None);
    if let Err(e) = shared.with_core(|c| claims::report(c, claim, "exit", &body, now_ms())) {
        log::note(
            &shared.logs,
            &format!("holder: could not report the exit of pid {pid} for claim {claim}: {e}"),
        );
    }
}

/// How the process `pid` started, as the system says now (empty when it cannot be read: a later kill then treats it as
/// unknown).
pub(crate) fn start_of(pid: u32) -> String {
    match procstart::probe(pid) {
        procstart::Probe::Alive(Some(start)) => start,
        _ => String::new(),
    }
}

/// What came of starting a warm turn or finishing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnStep {
    /// The lock is held and the claim is `running` on the process (`again`: it already was, a retried begin).
    Running { again: bool },
    /// The turn is over and the lock is not held (released, or it went with the stub).
    Finished,
    /// Another writer holds the conversation: the claim ended and its turn is parked as `lock_busy`.
    Busy,
    /// The process or its stub is gone: its exit was reported, which ends the claim.
    Gone(String),
    /// Nothing was changed for this reason (a claim that is not `claimed`, a stale or released claim, an unknown process).
    Refused(String),
    /// No answer, or the core.db write failed: asking again is safe (a take is `Locked` again for the same claim).
    Lost(String),
    /// The lock was taken, the claim could not be run, and giving the lock back failed too: the stub may still hold the
    /// conversation. The supervisor writes it to the core log; the warm turn driver of M3-10c ends the claim (a stop of a
    /// `claimed` claim), which ends the process and so frees the lock. Nothing in this module does that.
    LockStuck(String),
}

/// `claimed -> running` for a warm process (runtime-design section 4, step 3): the resident stub takes the conversation's
/// lock for `claim_id`, then the redeem CAS runs in core.db. A redeem that fails after the lock was taken gives the lock
/// back; a `Busy` ends the claim and parks the turn; a take that never answers is asked again, never given up on with
/// the lock possibly held. `pid` is the process whose stub holds the lock.
pub(crate) fn begin_turn(
    client: &mut Client,
    core: &impl CoreAccess,
    held: &proto::HeldId,
    key: &str,
    claim_id: i64,
    pid: u32,
) -> TurnStep {
    let Ok(claim) = u64::try_from(claim_id) else {
        return TurnStep::Refused("not a claim id".into());
    };
    let took = match take_turn_lock(client, held, key, claim) {
        Ok(a) if a.claim == claim => a.result,
        Ok(a) => return TurnStep::Lost(format!("the answer was for claim {}", a.claim)),
        Err(e) => return TurnStep::Lost(e.to_string()),
    };
    match took {
        TurnLock::Locked => {
            let started = start_of(pid);
            match core
                .with(|c| claims::redeem(c, claim_id, pid, &started, &held.instance, now_ms()))
            {
                Ok(Redeemed::Running { again }) => TurnStep::Running { again },
                Ok(Redeemed::Refused(state)) => {
                    if give_lock_back(client, held, key, claim) {
                        TurnStep::Refused(state)
                    } else {
                        TurnStep::LockStuck(format!("redeem refused ({state})"))
                    }
                }
                // The write failed. The lock is given back all the same: nothing guarantees a retry (the claim may be
                // stopped or revoked meanwhile), and a lock held for a claim that is not running locks the conversation
                // until the stub dies. A retried begin is a new take of the same claim, which the holder answers
                // `Locked` again, unless the release went through; then it is refused, and the caller ends the claim.
                Err(e) => {
                    if give_lock_back(client, held, key, claim) {
                        TurnStep::Lost(e.to_string())
                    } else {
                        TurnStep::LockStuck(format!("redeem failed ({e})"))
                    }
                }
            }
        }
        TurnLock::Busy => {
            let body = exit_body(Some(pid), None, false, None);
            match core.with(|c| claims::report(c, claim_id, "busy", &body, now_ms())) {
                Ok(_) => TurnStep::Busy,
                Err(e) => TurnStep::Lost(e.to_string()),
            }
        }
        TurnLock::Error(why) if lock_went_with_stub(&why) => {
            let body = exit_body(Some(pid), None, false, Some(&why));
            match core.with(|c| claims::report(c, claim_id, "exit", &body, now_ms())) {
                Ok(_) => TurnStep::Gone(why),
                Err(e) => TurnStep::Lost(e.to_string()),
            }
        }
        TurnLock::Error(why) => TurnStep::Refused(why),
    }
}

/// The turn's end: the lock is released (never taken again for this claim; the holder refuses a take after it).
/// Touches no claim row: the turn's `done` is the exit or done report's, not the lock's.
pub(crate) fn finish_turn(
    client: &mut Client,
    held: &proto::HeldId,
    key: &str,
    claim_id: i64,
) -> TurnStep {
    let Ok(claim) = u64::try_from(claim_id) else {
        return TurnStep::Refused("not a claim id".into());
    };
    match release_turn_lock(client, held, key, claim) {
        Ok(a) if a.claim != claim => {
            TurnStep::Lost(format!("the answer was for claim {}", a.claim))
        }
        Ok(a) => match a.result {
            TurnRelease::Released | TurnRelease::NotHeld => TurnStep::Finished,
            TurnRelease::Error(why) if lock_went_with_stub(&why) => TurnStep::Finished,
            TurnRelease::Error(why) => TurnStep::Lost(why),
        },
        Err(e) => TurnStep::Lost(e.to_string()),
    }
}

/// The holder's reasons that mean the process or its stub is not there to hold a lock any more.
fn lock_went_with_stub(why: &str) -> bool {
    matches!(why, "stub-gone" | "gone" | "exited")
}

/// Releases a lock whose claim could not be redeemed. A lost answer asks again (a repeated release is `NotHeld`).
fn give_lock_back(client: &mut Client, held: &proto::HeldId, key: &str, claim: u64) -> bool {
    (0..3).any(|_| release_turn_lock(client, held, key, claim).is_ok())
}

/// The process and conversation a turn step runs against. They come from what the core recorded when the holder listed
/// the process, never from the caller: a request for another conversation's key, or for a claim older than the one the
/// process was spawned for, is refused (a warm process serves later claims of its conversation, so a newer claim is fine).
fn turn_target(
    recorded: Option<(u64, u32, String)>,
    key: &str,
    claim: i64,
) -> std::result::Result<(u32, String), TurnStep> {
    let Some((spawned, pid, conv)) = recorded else {
        return Err(TurnStep::Refused(
            "the holder does not list that process".into(),
        ));
    };
    if conv != key {
        return Err(TurnStep::Refused(format!(
            "that process is conversation {conv}'s, not {key}'s"
        )));
    }
    if u64::try_from(claim).is_ok_and(|c| c < spawned) {
        return Err(TurnStep::Refused(
            "that claim is older than the process's own".into(),
        ));
    }
    Ok((pid, conv))
}

/// A `LockStuck` turn's end: the claim is stopped (its turn goes back to the queue), so the claim row never stays
/// `claimed` while its process may hold the conversation. The process the stub is known by is recorded on the claim
/// first (a `spawned` report; a claim that already names a process keeps it): a `claimed` claim with no pid would end
/// at once with nothing to kill while the stub still holds the lock. With the pid the claim goes to `ending` and
/// answers the kill, which the caller keeps sending until the process is gone. Repeating it changes nothing: a claim
/// that is not live is left alone.
pub(crate) fn end_stuck_claim(
    core: &impl CoreAccess,
    claim_id: i64,
    proc: Option<(u32, &str)>,
) -> Result<Option<claims::Kill>> {
    // `proc` is the stub's pid with the start time read when the lock was found stuck (the stub was alive then): a retry
    // never reads it again, because by then the pid may be a stranger's (the stub is being killed, pids get reused).
    if let Some((pid, started)) = proc {
        let body = serde_json::json!({"pid": pid, "started": started}).to_string();
        core.with(|c| claims::report(c, claim_id, "spawned", &body, now_ms()))?;
    }
    core.with(|c| claims::stop(c, claim_id, "lock_stuck", true, now_ms()))
}

/// [`end_stuck_claim`] a few times with a short pause between the tries (a busy core.db is what usually fails the write).
/// False when none went through; the caller keeps the end owed.
fn try_end_stuck_claim(
    core: &impl CoreAccess,
    claim_id: i64,
    proc: Option<&(u32, String)>,
) -> bool {
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        if end_stuck_claim(core, claim_id, proc.map(|(p, s)| (*p, s.as_str()))).is_ok() {
            return true;
        }
    }
    false
}

/// Runs one turn step the caller queued.
fn serve_turn(shared: &Shared, client: &mut Client, seen: &Known, req: &TurnReq) {
    let mut stuck_pid = 0;
    let step = match turn_target(seen.of(req.held.id), &req.key, req.claim) {
        Err(refused) => refused,
        Ok((pid, conv)) => {
            stuck_pid = pid;
            match req.op {
                TurnOp::Begin => begin_turn(client, shared, &req.held, &conv, req.claim, pid),
                TurnOp::Finish => finish_turn(client, &req.held, &conv, req.claim),
            }
        }
    };
    if let TurnStep::LockStuck(why) = &step {
        log::note(
            &shared.logs,
            &format!(
                "holder: the lock of {} may still be held by process {} ({why}); ending its claim",
                req.key, req.held.id
            ),
        );
        // The process is killed whatever the core says: the lock is held until it dies. A core write that failed is
        // tried again, a few times at once and then by the supervisor on every round (`resend_ends`) until it goes
        // through: nothing else ends a `claimed` turn claim before the next core restart.
        let proc = Some((stuck_pid, start_of(stuck_pid)));
        if !try_end_stuck_claim(shared, req.claim, proc.as_ref()) {
            shared.terminals.owe_claim_end(req.claim, proc);
        }
        // The claim's end is the process's end: the holder is asked to kill it, and asked again until it is gone.
        shared.terminals.requeue_end(req.held.clone(), 0);
    }
    let _ = req.reply.send(TurnDone(step));
}

/// `holder.forget <pid>` (the owner's): drops one process from the holder record in core.db, for a `blocked` holder whose
/// old process the core cannot tell from a stranger (a start in the old `ps` form, or one that cannot be read). Nothing is
/// signalled. The next hello no longer waits for that pid. Asked again, or for a pid that is not recorded, it changes
/// nothing and answers `forgot: false`.
pub(crate) fn forget_proc(
    core: &mut Core,
    params: &serde_json::Value,
    blocked: bool,
    live: &[u32],
) -> Result<serde_json::Value> {
    let pid = params
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| {
            crate::error::Error::BadParams("kith-core: holder.forget needs a pid".into())
        })?;
    // A process the connected holder still lists is its own: forgetting it would leave the next reconcile unable to end
    // it. Only a holder the core calls `blocked` (it cannot tell its old processes from strangers) may have any dropped.
    if live.contains(&pid) && !blocked {
        return Err(crate::error::Error::Refused(format!(
            "kith-core: pid {pid} is a process the holder lists now; holder.forget is for a blocked holder's old processes"
        )));
    }
    let mut record = core.holder_record()?;
    let before = record.procs.len();
    record.procs.retain(|p| p.pid != pid);
    let forgot = record.procs.len() != before;
    if forgot {
        core.save_holder_record(&record)?;
    }
    Ok(serde_json::json!({"forgot": forgot}))
}

#[cfg(all(test, unix))]
#[path = "holder_link_tests.rs"]
mod tests;
