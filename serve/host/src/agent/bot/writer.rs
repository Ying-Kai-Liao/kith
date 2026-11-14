//! Writer mode on the host's actors (design 2026-11-14-m3-cutover 1.1 to 1.5; the wiring of P7b): a turn runs on a core claim and
//! the per-turn writer lock a resident `kith-stub` holds, through [`CoreWriter`] (`terminal/core_writer.rs`, the machinery) and
//! [`ResidentStub`] (`terminal/warm_stub.rs`, the process). This file is what the actor adds around them; hq.rs, turn.rs, queue.rs,
//! mod.rs, offturn.rs and failover.rs only call it.
//!
//! **The mode** is read at every begin and at the slow tick (`CoreWriter::begin`, [`Actor::writer_tick`]), never once. Only a gate
//! that says `ok: true` is Writer. `Held` (the gate cannot be read, a switch file is there): the turn waits with "core not
//! answering" and runs nothing. `Files` and `Shadow` take today's path untouched: [`Actor::writer_claim`] answers `false`, and a
//! process that was started without a stub is never given one (a warm one is stopped when the mode turns Writer, see
//! [`Actor::writer_align`]).
//!
//! **One turn**: accept ([`Actor::accept`]: the key and the revoke epochs, read before any may check) -> the actor's start (`start_gate`
//! parks a held thread instead of refusing it) -> the process under a resident stub ([`Actor::start_resident`], a `warm_procs` row
//! per process and per session it loads) -> [`Actor::writer_claim`] (`begin`, then the transcript sync under the lock, step 7) ->
//! the prompt -> [`Actor::writer_release`] (release the lock, then report). A begin that cannot run now parks the turn
//! ([`WriterWait`], `writer_wait.rs`).
//!
//! **Revoke is not called here.** The owner's revoke reaches the core through `cut_off` -> `sweep_claims` -> `claim.sweepPerson`
//! and kills the person's processes by their `warm_procs` rows and their claimed turns by the basis; this side registers the
//! rows (person, agent, every loaded session) and reports ends, nothing else.
//!
//! The one begin per conversation at a time rule is the actor's own: it runs one turn at a time, a begin is awaited inside the start
//! phase and the previous turn's end is awaited before the next begin ([`Actor::writer_settle_end`]).

use super::{Actor, Conn};
use crate::terminal::core_writer::{Begin, CoreWriter, Mode, Running, Turn, WarmStub, new_turn_key};
use crate::terminal::{pid_runs, proc_start};
#[cfg(unix)]
use crate::terminal::warm_stub::{Launch, ResidentStub};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The warm process under a resident stub. Windows has none until the named-pipe spawn lands (S2 refuses there).
#[cfg(unix)]
pub(crate) type Stub = ResidentStub;
#[cfg(not(unix))]
pub(crate) enum Stub {}

#[cfg(not(unix))]
impl WarmStub for Stub {
    fn pid(&self) -> u32 {
        match *self {}
    }
    fn account(&self) -> Option<String> {
        match *self {}
    }
    fn take(&self, _: &str, _: i64) -> std::io::Result<kith_acp::Take> {
        match *self {}
    }
    fn release(&self, _: &str, _: i64) -> std::io::Result<kith_acp::Release> {
        match *self {}
    }
    fn restart(&mut self, _: Option<&str>) -> std::io::Result<()> {
        match *self {}
    }
    fn kill_group(&self) {
        match *self {}
    }
}

/// How long a begin refused by a process that is being stopped waits before it tries again.
pub(super) const RETRY_AFTER: Duration = Duration::from_secs(1);

/// How the next release ends the turn, when it is not a normal end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rel {
    /// The turn ran out of usage and failover will move it: `End::OutOfUsage`, the core requeues it.
    OutOfUsage,
    /// The 15 s kill of a cancelled turn: `End::Killed` is the only end that reports `exit`.
    Killed,
    /// The turn never prompted (its start failed after the claim): the claim is given up and the turn cancelled.
    Abandon,
    /// The prompt failed. If the process is gone (the core killed it: a revoke that reached a sibling session, an idle-kill; or it
    /// crashed) the turn did not finish: `exit` puts it back in the core instead of `done` losing it. A process that is still there
    /// ended an ordinary failed turn, which the host does not retry: `done`.
    Died,
}

/// How long a process that was let go is waited for before its `warm_procs` rows are forgotten (it is still the core's to kill
/// after that, `kill_resend`).
const GONE_WITHIN: Duration = Duration::from_secs(30);

/// The turn budget a room turn opens its chain with: the host's own cap (`chat::group`). The host's count decides; the core's
/// copy can only refuse more.
pub(super) fn budget() -> i64 {
    crate::chat::group::budget_of(None)
}

// MARK: the core and the mode

static CORES: OnceLock<Mutex<HashMap<PathBuf, Arc<CoreWriter>>>> = OnceLock::new();

/// The writer for this host's core, one per state folder (a host instance is the process's). `None` where Writer mode cannot be:
/// no core folders for the state, or a platform with no resident stub.
pub(crate) fn core(hub: &crate::hub::Hub) -> Option<Arc<CoreWriter>> {
    if !cfg!(unix) {
        return None;
    }
    let state = hub.terminal.env().state;
    let mut cores = CORES.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = cores.get(&state) {
        return Some(c.clone());
    }
    #[cfg(all(test, unix))]
    let folders = tests::folders_for(&state).or_else(|| crate::terminal::core_holders::folders_of(&state))?;
    #[cfg(not(all(test, unix)))]
    let folders = crate::terminal::core_holders::folders_of(&state)?;
    let made = Arc::new(CoreWriter::new(folders));
    cores.insert(state, made.clone());
    Some(made)
}

/// The mode right now, with the core to use when it is Writer. A blocking read (the gate is a core row).
pub(super) async fn mode_of(hub: &Arc<crate::hub::Hub>) -> (Mode, Option<Arc<CoreWriter>>) {
    let Some(core) = core(hub) else { return (Mode::Files, None) };
    let c = core.clone();
    let mode = tokio::task::spawn_blocking(move || c.mode()).await.unwrap_or(Mode::Held);
    if mode == Mode::Writer {
        adopt_once(hub, &core).await;
    }
    (mode, Some(core))
}

/// The first Writer-mode begin of this host start ends what an earlier host instance left (design 1.3, "Host restart").
async fn adopt_once(hub: &crate::hub::Hub, core: &Arc<CoreWriter>) {
    static DONE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let state = hub.terminal.env().state;
    let first = DONE.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(state);
    if !first {
        return;
    }
    let c = core.clone();
    match tokio::task::spawn_blocking(move || c.adopt()).await {
        Ok(Ok(adopted)) => tracing::info!(ended = adopted.ended.len(), kill = adopted.kill.len(), "writer mode: adopted"),
        Ok(Err(error)) => tracing::warn!(error, "writer mode: couldn't adopt an earlier host instance's claims"),
        Err(_) => {}
    }
}

// MARK: accept

/// What an item carries from the moment it is accepted: its key (a retried enqueue is the same turn) and the revoke epochs read
/// before any may check (closes B7). `None` epochs: accepted outside Writer mode; they are read at the begin.
#[derive(Clone, Debug, Default)]
pub(super) struct Accept {
    pub key: String,
    pub seen: Option<Seen>,
    /// The core's id of the turn, once it was enqueued: a repeated begin (a park, a requeue after a usage limit) never enqueues it twice.
    pub turn: Option<i64>,
}

/// The principal's epoch (`revokeSeen`) and every basis person's. A room's `room-<id>` is swept by `cut_off` itself, so it has its own.
#[derive(Clone, Debug, Default)]
pub(super) struct Seen {
    pub principal: i64,
    pub basis: Vec<(String, i64)>,
}

impl Accept {
    /// An item accepted with nothing read (Files or Shadow, or an internal turn: a compaction, a resumed turn).
    pub(super) fn unread() -> Self {
        Self { key: new_turn_key(), seen: None, turn: None }
    }
}

impl Actor {
    /// Accepts an item: its key, and in Writer mode the epochs of the principal and of each of `basis` (the people a room turn runs
    /// for), read now, before the start's may checks. Outside Writer mode nothing is asked of the core.
    pub(super) async fn accept(&self, basis: &[String]) -> Accept {
        let mut acc = Accept::unread();
        let (mode, core) = mode_of(&self.hub).await;
        if let (Mode::Writer, Some(core)) = (mode, core) {
            let principal = self.cfg.person.clone().unwrap_or_default();
            acc.seen = read_epochs(core, principal, basis.to_vec()).await;
        }
        acc
    }
}

pub(super) async fn read_epochs(core: Arc<CoreWriter>, principal: String, basis: Vec<String>) -> Option<Seen> {
    tokio::task::spawn_blocking(move || {
        let own = core.epoch(&principal).ok()?;
        let people = basis.iter().map(|p| core.epoch(p).map(|e| (p.clone(), e))).collect::<std::io::Result<Vec<_>>>().ok()?;
        Some(Seen { principal: own, basis: people })
    })
    .await
    .ok()
    .flatten()
}

// MARK: the process

/// A warm process's side of Writer mode: its stub, and the `warm_procs` rows the core has for it.
pub(crate) struct Proc {
    pub(super) core: Arc<CoreWriter>,
    /// Away while a begin or an end runs on a blocking thread.
    pub(super) stub: Option<Stub>,
    pub(super) pid: u32,
    pub(super) started: String,
    pub(super) person: String,
    pub(super) agent: String,
    /// `hostclaim.epoch` of `person`, read before the process started.
    pub(super) seen: i64,
    /// The conversations that have a row.
    rows: HashSet<String>,
}

impl Proc {
    pub(super) fn new(core: Arc<CoreWriter>, stub: Stub, pid: u32, started: String, person: String, agent: String, seen: i64) -> Self {
        Self { core, stub: Some(stub), pid, started, person, agent, seen, rows: HashSet::new() }
    }
}

impl Drop for Proc {
    /// Every way a process goes (a kill, an idle release, the process exiting) drops its connection: the core forgets the rows
    /// once the process is confirmed gone, and a process that is not gone stays the core's to kill.
    fn drop(&mut self) {
        let (core, pid, started) = (self.core.clone(), self.pid, self.started.clone());
        let forget = move || {
            let until = std::time::Instant::now() + GONE_WITHIN;
            while pid_runs(pid) && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            if !pid_runs(pid) {
                let _ = core.proc_gone(pid, &started);
            }
        };
        if let Err(error) = std::thread::Builder::new().name("warm-proc-gone".into()).spawn(forget) {
            tracing::warn!(%error, "couldn't forget a warm process");
        }
    }
}

/// The stub binary and the lock folder of Writer mode: `<machine>/core/locks`, the folder `kith --direct` locks in.
fn stub_paths(core: &CoreWriter) -> Result<(PathBuf, PathBuf)> {
    #[cfg(all(test, unix))]
    if let Some(stub) = tests::stub_bin() {
        return Ok((stub, core.folders().machine.join("core").join("locks")));
    }
    let Some((_, stub)) = crate::kernel::stub_env() else { bail!("Writer mode needs kith-stub and it was not found") };
    Ok((PathBuf::from(stub), core.folders().machine.join("core").join("locks")))
}

impl Actor {
    /// The mode for this ensure, with a warm process of the wrong kind put away: a process started without a stub cannot run a Writer
    /// turn, so once the gate says Writer it is stopped (its sessions load again under a stub). `Ok(true)`: start under a stub.
    /// `Held`: the turn waits ([`WriterWait`]); the core is not asked anything.
    pub(super) async fn writer_align(&mut self) -> Result<bool> {
        let (mode, _) = mode_of(&self.hub).await;
        match mode {
            Mode::Held => Err(WriterWait::mode().into()),
            Mode::Writer => {
                if self.conn.as_ref().is_some_and(|c| c.writer.is_none()) {
                    tracing::info!(bot = %self.cfg.id, "writer mode: stopping a process that has no stub");
                    if let Some(c) = self.conn.take() {
                        c.acp.kill().await;
                    }
                }
                Ok(true)
            }
            Mode::Files | Mode::Shadow => Ok(false),
        }
    }

    /// The agent name the core records for this actor's claims: the hq agent's, or `bot:<id>` for a native bot (B10).
    pub(super) fn writer_agent(&self) -> String {
        self.cfg.hq_agent.clone().unwrap_or_else(|| format!("bot:{}", self.cfg.id))
    }

    /// Starts `command` under a resident stub, as `kith-serve`'s own `start_agent` starts it bare: the same handshake, plus a
    /// `warm_procs` row for the process (before its first session loads) and the stub kept with the connection.
    /// `account` is the account the process runs as; the core records it and, until accounts are switched, does not pick another.
    pub(super) async fn start_resident(
        &self,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
        budget: Duration,
        account: &str,
    ) -> Result<Conn> {
        #[cfg(not(unix))]
        {
            let _ = (command, cwd, env, budget, account);
            bail!("Writer mode needs Unix until the resident stub has a named-pipe spawn");
        }
        #[cfg(unix)]
        {
            let Some(core) = core(&self.hub) else { bail!("no core to claim through") };
            let (stub_bin, locks) = stub_paths(&core)?;
            let person = self.cfg.person.clone().unwrap_or_default();
            let seen = if person.is_empty() { 0 } else { read_epoch(core.clone(), person.clone()).await? };
            let launch = Launch {
                account: account.to_owned(),
                command: command.to_owned(),
                cwd: cwd.to_owned(),
                env: env.to_vec(),
            };
            // The same launch on a restart. Until accounts are switched the host picks the account, so a restart on another
            // one is not asked for; a different name is answered with the process's own, which `begin` then reads as "not the
            // account the core picked" and refuses rather than runs on the wrong login.
            let launcher: crate::terminal::warm_stub::Launcher = Box::new(move |_| launch.clone());
            let mut stub = ResidentStub::spawn(&stub_bin, &locks, launcher, Some(account))?;
            let (pid, acp) = (stub.pid(), stub.acp());
            let Some(rx) = stub.take_incoming() else { bail!("the stub's incoming channel was taken") };
            let started = proc_start(pid).ok_or_else(|| anyhow!("the stub's start time can't be read"))?;
            let agent = self.writer_agent();
            // Before any session loads (and before the handshake): a revoke that lands from here on finds the process.
            let (c, p, a, s, st) = (core.clone(), person.clone(), agent.clone(), started.clone(), seen);
            let registered = tokio::task::spawn_blocking(move || c.register_proc(pid, &s, &p, &a, None, (!p.is_empty()).then_some(st)))
                .await?;
            if let Err(why) = registered {
                stub.kill_group();
                return Err(if why.starts_with("revoked") || why.contains("revoked") {
                    anyhow!("revoked: the turn was refused")
                } else {
                    anyhow!("couldn't register the agent process: {why}")
                });
            }
            let proc = Proc::new(core.clone(), stub, pid, started, person, agent, seen);
            match super::launch::handshake(acp, rx, budget).await {
                Ok(mut conn) => {
                    conn.writer = Some(proc);
                    Ok(conn)
                }
                // The failed handshake stopped the process; the row goes with `proc`.
                Err(error) => Err(error),
            }
        }
    }

    /// Brings `warm_procs` in step with the sessions this process has loaded: a row for each (registered before its load, see
    /// [`Self::warm_session`]), none for a session it dropped. A registration the core refuses as revoked ends the process.
    pub(super) async fn sync_warm(&mut self) -> Result<()> {
        let Some(conn) = self.conn.as_mut() else { return Ok(()) };
        let Some(proc) = conn.writer.as_mut() else { return Ok(()) };
        let want: HashSet<String> = conn.loaded.keys().cloned().collect();
        let (add, drop): (Vec<String>, Vec<String>) =
            (want.difference(&proc.rows).cloned().collect(), proc.rows.difference(&want).cloned().collect());
        if add.is_empty() && drop.is_empty() {
            return Ok(());
        }
        let (core, pid, started, person, agent, seen) =
            (proc.core.clone(), proc.pid, proc.started.clone(), proc.person.clone(), proc.agent.clone(), proc.seen);
        let (a, d) = (add.clone(), drop.clone());
        let done = tokio::task::spawn_blocking(move || -> Result<(), String> {
            for sid in &a {
                core.register_proc(pid, &started, &person, &agent, Some(sid), (!person.is_empty()).then_some(seen))?;
            }
            for sid in &d {
                core.proc_forget(pid, &started, sid)?;
            }
            Ok(())
        })
        .await?;
        match done {
            Ok(()) => {
                if let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()) {
                    proc.rows.extend(add);
                    for sid in drop {
                        proc.rows.remove(&sid);
                    }
                }
                Ok(())
            }
            Err(why) if why.contains("revoked") => {
                if let Some(c) = self.conn.take() {
                    c.acp.kill().await;
                }
                Err(anyhow!("revoked: the turn was refused"))
            }
            Err(why) => bail!("couldn't register the session with the core: {why}"),
        }
    }

    /// The row for `sid`, before `session/load` names it (design 1.5, B4): a person's process is findable by a revoke the whole
    /// time the session loads.
    pub(super) async fn warm_session(&mut self, sid: &str) -> Result<()> {
        if let Some(conn) = self.conn.as_mut()
            && conn.writer.is_some()
        {
            // The load is about to put it in `loaded`; `sync_warm` reads that set, so name it there first.
            conn.loaded.entry(sid.to_owned()).or_insert_with(std::time::Instant::now);
            let result = self.sync_warm().await;
            // Not loaded yet: if the load fails it must not stay listed as loaded.
            if let Some(conn) = self.conn.as_mut() {
                conn.loaded.remove(sid);
            }
            return result;
        }
        Ok(())
    }

    /// The claim a session load needs before it (access-arch rule 4): in Writer mode the process's own row (its person, its sessions)
    /// is what a revoke finds, and the turn's claim comes with `begin` after the load, so this is only that row.
    pub(super) async fn claim_before_load(&mut self, sid: &str, lane: &crate::store::Lane) -> Result<()> {
        if self.conn.as_ref().is_some_and(|c| c.writer.is_some()) {
            return self.warm_session(sid).await;
        }
        self.claim_for_turn(sid, lane).await
    }
}

async fn read_epoch(core: Arc<CoreWriter>, person: String) -> Result<i64> {
    tokio::task::spawn_blocking(move || core.epoch(&person)).await?.map_err(|e| anyhow!("the revoke epoch can't be read: {e}"))
}

// MARK: the turn

/// A turn that holds its claim.
pub(super) struct Run {
    pub run: Running,
    pub conversation: String,
}

/// The wait that parks a start: what the retry is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Why {
    /// The gate cannot be read, or the core is not answering: "core not answering".
    Core,
    /// A terminal or external claim writes: unparked when it ends.
    Terminal,
    /// The stub's lock is held by another (`--direct`): the claim ended `busy`.
    Lock,
    /// Another turn's claim holds the conversation, or a reconcile is under way: tried again on the next tick.
    Claim,
    /// A process marked for a kill: tried again after a second.
    Retry,
}

/// A start the core cannot take yet. The error `begin_prompt` returns; [`Actor::start_failed`] parks the turn instead of failing it.
#[derive(Debug)]
pub(super) struct WriterWait {
    pub why: Why,
    pub turn: Option<i64>,
}

impl WriterWait {
    pub(super) fn mode() -> Self {
        Self { why: Why::Core, turn: None }
    }
}

impl std::fmt::Display for WriterWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.why {
            Why::Core => "core not answering",
            Why::Terminal => "waiting for your terminal session",
            Why::Lock | Why::Claim | Why::Retry => "waiting for the agent's session",
        })
    }
}

impl std::error::Error for WriterWait {}

/// An owned [`Turn`], to move onto a blocking thread.
pub(super) struct OwnedTurn {
    pub id: Option<i64>,
    pub conversation: String,
    pub agent: String,
    pub engine: String,
    pub source: &'static str,
    pub person: String,
    pub cwd: String,
    pub revoke_seen: i64,
    pub deadline: Option<i64>,
    pub key: String,
    pub room: Option<Value>,
}

impl OwnedTurn {
    pub(super) fn as_turn(&self) -> Turn<'_> {
        Turn {
            id: self.id,
            conversation: &self.conversation,
            agent: &self.agent,
            engine: &self.engine,
            source: self.source,
            person: &self.person,
            cwd: &self.cwd,
            revoke_seen: self.revoke_seen,
            deadline: self.deadline,
            key: &self.key,
            room: self.room.as_ref(),
        }
    }
}

/// The stub a begin runs on, which refuses what a stopped start must not do: take a lock or start another process.
pub(super) struct Armed<'a> {
    stub: &'a mut Stub,
    gone: Arc<AtomicBool>,
    restarted: bool,
}

impl<'a> Armed<'a> {
    pub(super) fn new(stub: &'a mut Stub, gone: Arc<AtomicBool>) -> Self {
        Self { stub, gone, restarted: false }
    }

    /// Whether the process was started again under the begin.
    pub(super) fn restarted(&self) -> bool {
        self.restarted
    }
}

impl WarmStub for Armed<'_> {
    fn pid(&self) -> u32 {
        self.stub.pid()
    }
    fn account(&self) -> Option<String> {
        self.stub.account()
    }
    fn take(&self, key: &str, claim: i64) -> std::io::Result<kith_acp::Take> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("the start was stopped"));
        }
        self.stub.take(key, claim)
    }
    fn release(&self, key: &str, claim: i64) -> std::io::Result<kith_acp::Release> {
        self.stub.release(key, claim)
    }
    fn restart(&mut self, account: Option<&str>) -> std::io::Result<()> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("the start was stopped"));
        }
        self.restarted = true;
        self.stub.restart(account)
    }
    fn kill_group(&self) {
        self.stub.kill_group();
    }
}

/// Sets `gone` when the start that awaits a begin is dropped (Stop, shutdown): the begin on its blocking thread then stops at its next
/// step and gives back what it holds.
pub(super) struct Watch(Arc<AtomicBool>, bool);

impl Watch {
    pub(super) fn new(gone: Arc<AtomicBool>) -> Self {
        Self(gone, false)
    }

    /// The start got its answer: dropping this is not a cancel any more.
    pub(super) fn disarm(&mut self) {
        self.1 = true;
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

/// What a begin leaves behind when nobody waits for it any more: a claim running is given up, a turn queued is cancelled.
pub(super) fn orphaned(core: &CoreWriter, conversation: &str, begin: &Begin, stub: &mut dyn WarmStub) {
    match begin {
        Begin::Running(r) => {
            core.abandon(r.turn, r.claim, conversation, stub);
            stub.kill_group();
        }
        Begin::Queued { turn }
        | Begin::Retry { turn, .. }
        | Begin::Parked { turn, .. }
        | Begin::Dropped { turn: Some(turn), .. }
        | Begin::Failed { turn: Some(turn), .. } => core.cancel_turn(*turn, "start stopped"),
        _ => {}
    }
}

/// The turn id a begin's answer names, if it does.
pub(super) fn turn_of(b: &Begin) -> Option<i64> {
    match b {
        Begin::Running(r) => Some(r.turn),
        Begin::Queued { turn } | Begin::Retry { turn, .. } | Begin::Parked { turn, .. } => Some(*turn),
        Begin::NotWriter { turn, .. }
        | Begin::Dropped { turn, .. }
        | Begin::CoreDown { turn, .. }
        | Begin::Failed { turn, .. } => *turn,
        Begin::Exhausted { .. } | Begin::LockStuck { .. } => None,
    }
}

pub(super) fn now_ms() -> i64 {
    crate::store::now_ms()
}

#[cfg(all(test, unix))]
#[path = "writer_tests.rs"]
mod tests;
