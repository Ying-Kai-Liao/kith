//! One turn in Writer mode on the actor: the claim (`begin`, then the transcript sync under the lock) and the end (the lock back,
//! then the report). The mode, the process and the accept are in `writer.rs`; the waits a begin can end in are in `writer_wait.rs`.

use super::Actor;
use super::writer::{
    Accept, Armed, OwnedTurn, Proc, Rel, Run, Seen, Stub, Watch, Why, WriterWait, core, now_ms, orphaned, read_epochs, turn_of,
};
use crate::terminal::core_writer::{Begin, CoreWriter, Dropped, End, Finished, Mode, Running, WarmStub, new_turn_key};
use crate::terminal::{pid_runs, proc_start};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::oneshot;

/// An end running on a blocking thread: the lock released, then the report.
pub(super) struct InFlight {
    rx: oneshot::Receiver<(Finished, Option<Stub>)>,
    run: Running,
    end: End,
}

/// The actor's Writer-mode state (one field of the actor).
#[derive(Default)]
pub(super) struct State {
    /// The turn that holds its claim and the lock now.
    pub run: Option<Run>,
    /// What the item being started carries from its accept; kept through parks and a requeue after a usage limit.
    pub acc: Option<Accept>,
    pub end: Option<Rel>,
    pub ending: Option<InFlight>,
    /// An end the core did not take: sent again on the slow tick.
    pub lost: Option<(Running, End)>,
    pub parked: Option<super::writer_wait::Parked>,
    /// The arguments of the start in progress, kept so a park can start it again.
    pub start: Option<super::writer_wait::StartArgs>,
    /// `loaded` moved (a session forgotten): the next tick brings `warm_procs` in step.
    pub dirty: bool,
    pub tick: Option<std::time::Instant>,
}

impl State {
    /// Nothing of Writer mode is pending: the actor may rest (stop its agent, retire).
    pub(super) fn idle(&self) -> bool {
        self.parked.is_none() && self.ending.is_none() && self.lost.is_none() && self.run.is_none()
    }
}

impl Actor {
    /// `begin` on the process's stub, on a blocking thread. The stub goes out of its [`Proc`] for the call and comes back. `Ok` is
    /// the answer and whether the process was restarted meanwhile (a stub that failed, or an account switch): its connection is
    /// then stale.
    async fn begin_on_stub(&mut self, core: &Arc<CoreWriter>, t: OwnedTurn) -> Result<(Begin, bool)> {
        let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()) else { bail!("the agent process has no stub") };
        let Some(mut stub) = proc.stub.take() else { bail!("the agent process is busy") };
        let gone = Arc::new(AtomicBool::new(false));
        let (tx, rx) = oneshot::channel();
        let (core, flag) = (core.clone(), gone.clone());
        // A task of its own: a start that is dropped (Stop) still has its blocking thread finish, and gives back what it holds.
        tokio::spawn(async move {
            let joined = tokio::task::spawn_blocking(move || {
                let (begin, restarted) = {
                    let mut armed = Armed::new(&mut stub, flag);
                    let begin = core.begin(&t.as_turn(), &mut armed);
                    (begin, armed.restarted())
                };
                (begin, restarted, stub, t, core)
            })
            .await;
            let Ok((begin, restarted, stub, t, core)) = joined else { return };
            if let Err((begin, _, mut stub)) = tx.send((begin, restarted, stub)) {
                let _ = tokio::task::spawn_blocking(move || orphaned(&core, &t.conversation, &begin, &mut stub)).await;
            }
        });
        let mut watch = Watch::new(gone);
        let (begin, restarted, stub) = rx.await.map_err(|_| anyhow!("the begin was lost"))?;
        watch.disarm();
        if let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()) {
            proc.stub = Some(stub);
        }
        Ok((begin, restarted))
    }

    /// The `hostclaim.enqueue` fields of a room turn at room level: its basis (the people it runs for, each with the epoch read at
    /// accept), the room, the gen, the join, the author and the budget the chain opens with. A turn at owner level, or no room turn
    /// at all, sends none: it is a plain turn.
    fn room_fields(&self, seen: &Seen) -> Option<Value> {
        use crate::chat::room_turn::Level;
        let basis = self.turn_basis.as_ref().filter(|b| b.level() == Level::Room)?;
        let group = self.active_group.as_ref()?;
        let epochs: serde_json::Map<String, Value> = seen.basis.iter().map(|(p, e)| (p.clone(), json!(e))).collect();
        Some(json!({"level": "room", "basis": basis.people, "epochs": epochs, "room": basis.room, "gen": basis.room_gen,
                    "joinEpoch": basis.join_gen, "author": group.author, "budget": crate::agent::bot::writer::budget()}))
    }

    /// What the claim names the turn: its source for the core's rows.
    fn turn_source(&self, off: bool) -> &'static str {
        if off {
            "off"
        } else if self.active_routine.is_some() {
            "routine"
        } else if self.active_request.is_some() {
            "bot"
        } else if self.active_group.is_some() {
            "group"
        } else if self.compacting.is_some() {
            "compact"
        } else {
            "app"
        }
    }

    /// The turn to begin, from what its item carried (the epochs are read now when it was accepted outside Writer mode).
    async fn writer_turn(&mut self, core: &Arc<CoreWriter>, sid: &str, off: bool) -> Result<OwnedTurn> {
        let basis: Vec<String> = self
            .turn_basis
            .as_ref()
            .filter(|b| b.level() == crate::chat::room_turn::Level::Room)
            .map(|b| b.people.clone())
            .unwrap_or_default();
        let mut acc = self.writer.acc.take().unwrap_or_else(Accept::unread);
        let covers = |s: &Seen| basis.iter().all(|p| s.basis.iter().any(|(q, _)| q == p));
        if !acc.seen.as_ref().is_some_and(covers) {
            let principal = self.cfg.person.clone().unwrap_or_default();
            acc.seen = read_epochs(core.clone(), principal, basis).await;
        }
        let Some(seen) = acc.seen.clone() else {
            self.writer.acc = Some(acc);
            bail!("the revoke epochs can't be read: the core did not answer");
        };
        let room = self.room_fields(&seen);
        let deadline = self.routine_deadline.map(|d| {
            now_ms() + i64::try_from(d.saturating_duration_since(std::time::Instant::now()).as_millis()).unwrap_or(0)
        });
        let turn = OwnedTurn {
            id: acc.turn,
            conversation: sid.to_owned(),
            agent: self.writer_agent(),
            engine: if self.cfg.backend == "codex" { "codex" } else { "claude" }.into(),
            source: self.turn_source(off),
            person: self.cfg.person.clone().unwrap_or_default(),
            cwd: self.conn_started_as.as_ref().map_or_else(|| self.cfg.cwd.clone(), |s| s.1.clone()),
            revoke_seen: seen.principal,
            deadline,
            key: acc.key.clone(),
            room,
        };
        self.writer.acc = Some(acc);
        Ok(turn)
    }

    /// The claim of a turn about to prompt `sid` (or, `off`, one the agent started by itself) in Writer mode: `begin`, then the
    /// transcript sync under the lock (design 1.3 step 7). `Ok(false)`: the mode is not Writer (the caller takes the files path).
    /// A begin that cannot run now is a [`WriterWait`]; any other answer that is not `Running` is the error the start fails with.
    pub(super) async fn writer_claim(&mut self, sid: &str, lane: &crate::store::Lane, off: bool) -> Result<bool> {
        let Some(core) = self.conn.as_ref().and_then(|c| c.writer.as_ref()).map(|p| p.core.clone()) else {
            return Ok(false);
        };
        self.writer_settle_end().await;
        if self.writer.run.as_ref().is_some_and(|r| r.conversation == sid) {
            return Ok(true);
        }
        // Another session's claim of this turn (the account moved, a session was reloaded): given up first.
        if self.writer.run.is_some() {
            self.release_turn_claim();
            self.writer_settle_end().await;
        }
        let mut sid = sid.to_owned();
        for _ in 0..3 {
            self.sync_warm().await?;
            let t = self.writer_turn(&core, &sid, off).await?;
            let (begin, restarted) = self.begin_on_stub(&core, t).await?;
            if let (Some(id), Some(acc)) = (turn_of(&begin), self.writer.acc.as_mut()) {
                acc.turn = Some(id);
            }
            if restarted {
                self.rebuild_after_restart(&core).await?;
            }
            let running = match begin {
                Begin::Running(r) => r,
                Begin::NotWriter { mode: Mode::Files | Mode::Shadow, .. } => return Ok(false),
                other => {
                    let error = self.writer_refusal(other);
                    // The engine is already writing: a begin that did not give it the session stops the process.
                    if off {
                        self.writer_kill_process().await;
                    }
                    return Err(error);
                }
            };
            self.writer.run = Some(Run { run: running, conversation: sid.clone() });
            if restarted {
                // A new process has loaded nothing: its session loads under the lock, as a reload does.
                self.writer_reload(lane).await?;
            }
            // Step 7: what a terminal wrote since the session loaded.
            match crate::terminal::sync_for_turn(&self.hub, lane, &sid).await? {
                crate::terminal::Synced::Unchanged => return Ok(true),
                crate::terminal::Synced::Moved => {
                    if let Some(c) = self.conn.as_mut() {
                        c.loaded.remove(&sid);
                    }
                    self.writer_reload(lane).await?;
                    return Ok(true);
                }
                crate::terminal::Synced::Forked { .. } => {
                    // The claim is on the session the thread left: given up, and the turn begins again on the lane's session.
                    self.writer.end = Some(Rel::Abandon);
                    self.release_turn_claim();
                    self.writer_settle_end().await;
                    if let Some(root) = lane.thread.as_deref() {
                        self.thread_sessions.remove(root);
                    }
                    if let Some(c) = self.conn.as_mut() {
                        c.loaded.remove(&sid);
                    }
                    self.writer_reload(lane).await?;
                    sid = self.turn_session.clone().ok_or_else(|| anyhow!("no session"))?;
                    if let Some(acc) = self.writer.acc.as_mut() {
                        acc.turn = None;
                        acc.key = new_turn_key();
                    }
                }
            }
        }
        bail!("the session moved under the turn three times")
    }

    /// Loads the lane's session again in the process (it moved, or the process is a new one). Boxed: `ensure_session` reaches
    /// `claim_before_load`, which is not `writer_claim`, but the types see one cycle.
    async fn writer_reload(&mut self, lane: &crate::store::Lane) -> Result<()> {
        let slot = self.slot(lane);
        Box::pin(self.ensure_session(slot.as_deref())).await
    }

    /// What a begin that did not run the turn comes to for the start: a wait, or the error it fails with.
    fn writer_refusal(&mut self, b: Begin) -> anyhow::Error {
        use crate::terminal::core_writer::Parked;
        let wait = |why, turn| -> anyhow::Error { WriterWait { why, turn }.into() };
        match b {
            Begin::Running(_) | Begin::NotWriter { mode: Mode::Files | Mode::Shadow | Mode::Writer, .. } => {
                anyhow!("not a refusal")
            }
            Begin::NotWriter { turn, .. } | Begin::CoreDown { turn, .. } => wait(Why::Core, turn),
            Begin::Queued { turn } => wait(Why::Claim, Some(turn)),
            Begin::Retry { turn, .. } => wait(Why::Retry, Some(turn)),
            Begin::Parked { turn, why } => wait(if why == Parked::TerminalLive { Why::Terminal } else { Why::Lock }, Some(turn)),
            Begin::Dropped { why: Dropped::Revoked, .. } => {
                self.notice("Access was revoked.", super::NoticeStyle::Error);
                anyhow!("revoked: the turn was refused")
            }
            Begin::Dropped { why: Dropped::Budget, said, .. } => {
                self.notice(&format!("This conversation's turn budget is spent. {said}"), super::NoticeStyle::Info);
                anyhow!("the turn budget is spent")
            }
            // Cancelled or expired meanwhile, or the conversation's ask was stopped: the person stopped it.
            Begin::Dropped { why: Dropped::NotWaiting | Dropped::Stopped, .. } => super::turn::StartCancelled.into(),
            Begin::Dropped { why: Dropped::Refused, said, .. } => anyhow!("the core refused the turn: {said}"),
            Begin::Exhausted { text } => crate::agent::backups::NoAccount(text).into(),
            Begin::Failed { why, .. } => anyhow!("{why}"),
            Begin::LockStuck { why } => {
                self.conn = None;
                anyhow!("the agent's session lock could not be released ({why}); the agent was stopped")
            }
        }
    }

    /// A stub that was restarted under a begin has a new process: its connection (and what it had loaded) is the old one's. A new
    /// connection on the new process, registered as the first one was.
    async fn rebuild_after_restart(&mut self, core: &Arc<CoreWriter>) -> Result<()> {
        #[cfg(not(unix))]
        {
            let _ = core;
            bail!("Writer mode needs Unix");
        }
        #[cfg(unix)]
        {
            let Some(old) = self.conn.as_mut() else { bail!("the agent process is gone") };
            let run_grant = old.run_grant.take();
            let Some(proc) = old.writer.as_mut() else { bail!("the agent process has no stub") };
            let Some(mut stub) = proc.stub.take() else { bail!("the agent process is busy") };
            let (pid, acp) = (stub.pid(), stub.acp());
            let Some(rx) = stub.take_incoming() else { bail!("the restarted stub has no incoming channel") };
            let started = proc_start(pid).ok_or_else(|| anyhow!("the stub's start time can't be read"))?;
            let (person, agent, seen) = (proc.person.clone(), proc.agent.clone(), proc.seen);
            // The old row first: the old process is gone (a restart waits for its group).
            let (c, old_pid, old_started) = (core.clone(), proc.pid, proc.started.clone());
            let _ = tokio::task::spawn_blocking(move || c.proc_gone(old_pid, &old_started)).await;
            let (c, p, a, s) = (core.clone(), person.clone(), agent.clone(), started.clone());
            let registered =
                tokio::task::spawn_blocking(move || c.register_proc(pid, &s, &p, &a, None, (!p.is_empty()).then_some(seen)))
                    .await?;
            if let Err(why) = registered {
                stub.kill_group();
                bail!("couldn't register the restarted agent process: {why}");
            }
            let mut conn = super::launch::handshake(acp, rx, Duration::from_secs(180)).await?;
            conn.writer = Some(Proc::new(core.clone(), stub, pid, started, person, agent, seen));
            // The token the process runs with is the process's (a restart on the same launch keeps its grant).
            conn.run_grant = run_grant;
            self.conn = Some(conn);
            Ok(())
        }
    }

    /// The agent is already writing and the begin did not give it the session (an off-turn turn): the process is stopped.
    async fn writer_kill_process(&mut self) {
        if let Some(proc) = self.conn.as_ref().and_then(|c| c.writer.as_ref())
            && let Some(stub) = proc.stub.as_ref()
        {
            stub.kill_group();
        }
        if let Some(c) = self.conn.take() {
            c.acp.kill().await;
        }
    }

    // MARK: the end

    /// Ends the turn that holds a claim (the lock back, then the report): `finish_turn`, a failed start and a failover all come here
    /// through `release_turn_claim`. The end runs on a blocking thread; the next begin, and the slow tick, collect it
    /// ([`Self::writer_settle_end`]).
    pub(super) fn writer_release(&mut self) {
        let Some(run) = self.writer.run.take() else { return };
        let how = self.writer.end.take();
        let gone = !pid_runs(run.run.pid);
        let end = match (how, self.is_room_actor()) {
            (Some(Rel::Killed), _) => End::Killed,
            (Some(Rel::Died), _) if gone => End::Died,
            (Some(Rel::OutOfUsage), _) => End::OutOfUsage,
            (_, true) => End::Room,
            _ => End::Done,
        };
        // A requeue keeps the item's turn (the next begin names it); any other end is the item's last.
        if how != Some(Rel::OutOfUsage) {
            self.writer.acc = None;
        }
        let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()) else {
            // The process is gone: nothing to release (the lock went with it); the end is reported.
            self.writer_report_only(run, end);
            return;
        };
        let core = proc.core.clone();
        let stub = proc.stub.take();
        let (tx, rx) = oneshot::channel();
        let (r, conv, abandon) = (run.run.clone(), run.conversation.clone(), how == Some(Rel::Abandon));
        tokio::task::spawn_blocking(move || {
            let Some(mut stub) = stub else {
                let _ = tx.send((core.retry_end(&r, end), None));
                return;
            };
            let finished = if abandon {
                core.abandon(r.turn, r.claim, &conv, &mut stub);
                Finished::Reported
            } else if !pid_runs(r.pid) {
                // The stub is gone with its lock: nothing to release, only the end to report (`end` is `Died`, or `Killed`).
                core.retry_end(&r, end)
            } else {
                core.finish(&r, &conv, &mut stub, end)
            };
            // The user's stop that had to be killed: the turn put back by `exit` is not wanted again.
            if end == End::Killed {
                core.cancel_turn(r.turn, "stopped");
            }
            let _ = tx.send((finished, Some(stub)));
        });
        self.writer.ending = Some(InFlight { rx, run: run.run, end });
    }

    /// The process is gone: only the end is reported.
    fn writer_report_only(&mut self, run: Run, end: End) {
        let Some(core) = core(&self.hub) else { return };
        let (tx, rx) = oneshot::channel();
        let r = run.run.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send((core.retry_end(&r, end), None));
            if end == End::Killed {
                core.cancel_turn(r.turn, "stopped");
            }
        });
        self.writer.ending = Some(InFlight { rx, run: run.run, end });
    }

    /// Waits for an end in flight and takes what it came to. A report the core did not take is kept for the slow tick.
    pub(super) async fn writer_settle_end(&mut self) {
        let Some(flight) = self.writer.ending.take() else { return };
        let Ok((finished, stub)) = flight.rx.await else { return };
        self.writer_ended(flight.run, flight.end, finished, stub);
    }

    /// The end in flight, if it is done: for the tick, which never waits.
    pub(super) fn writer_collect(&mut self) {
        let Some(mut flight) = self.writer.ending.take() else { return };
        match flight.rx.try_recv() {
            Ok((finished, stub)) => self.writer_ended(flight.run, flight.end, finished, stub),
            Err(oneshot::error::TryRecvError::Empty) => self.writer.ending = Some(flight),
            Err(oneshot::error::TryRecvError::Closed) => {}
        }
    }

    fn writer_ended(&mut self, run: Running, end: End, finished: Finished, stub: Option<Stub>) {
        let keep = !matches!(finished, Finished::RoomEnded { .. } | Finished::LockStuck(_));
        if let (Some(stub), true) = (stub, keep)
            && let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()).filter(|p| p.stub.is_none() && p.pid == run.pid)
        {
            proc.stub = Some(stub);
        }
        match finished {
            Finished::Reported => {}
            Finished::ReportLost(why) => {
                tracing::warn!(bot = %self.cfg.id, why, "the core did not take the end of a turn; sending it again");
                self.writer.lost = Some((run, end));
            }
            // The process is stopped (a room turn, or a lock that could not be given back): its connection goes.
            Finished::RoomEnded { .. } | Finished::LockStuck(_) => self.conn = None,
        }
    }

    /// The prompt failed (`finish_turn`): if the process turns out to be gone, the end is `exit`, not `done` (see [`Rel::Died`]).
    pub(super) fn writer_prompt_failed(&mut self) {
        if self.writer.run.is_some() && self.writer.end.is_none() {
            self.writer.end = Some(Rel::Died);
        }
    }

    /// Notes that a cancelled turn's process is being killed (the 15 s rule): its end is the one that reports `exit`.
    pub(super) fn writer_mark_killed(&mut self) {
        if self.writer.run.is_some() {
            self.writer.end = Some(Rel::Killed);
        }
    }

    /// A session of this process was dropped from `loaded`: `warm_procs` follows on the next tick.
    pub(super) fn writer_forgot(&mut self) {
        self.writer.dirty = true;
    }
}
