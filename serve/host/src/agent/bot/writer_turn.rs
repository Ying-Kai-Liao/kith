//! One turn in Writer mode on the actor: the claim (`begin`, then the transcript sync under the lock) and the end (the lock back,
//! then the report). The mode, the process and the accept are in `writer.rs`; the waits a begin can end in are in `writer_wait.rs`.

use super::Actor;
use super::writer::{
    Accept, Armed, OwnedTurn, Proc, Rel, Run, Seen, Stub, Watch, Why, WriterWait, chain, chain_opened, core, now_ms,
    orphaned, read_epochs, turn_of,
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
    pub lost: Vec<(Running, End)>,
    pub parked: Option<super::writer_wait::Parked>,
    /// The wait a resumed start came from (why, tries, since), while that start runs: a park that follows continues it.
    pub waited: Option<(Why, u32, std::time::Instant)>,
    /// The arguments of the start in progress, kept so a park can start it again.
    pub start: Option<super::writer_wait::StartArgs>,
    /// `loaded` moved (a session forgotten): the next tick brings `warm_procs` in step.
    pub dirty: bool,
    pub tick: Option<std::time::Instant>,
}

impl State {
    /// Nothing of Writer mode is pending: the actor may rest (stop its agent, retire).
    pub(super) fn idle(&self) -> bool {
        self.parked.is_none() && self.ending.is_none() && self.lost.is_empty() && self.run.is_none()
    }
}

/// How a release ends the turn at the core: `how` is what the actor noted, `gone` whether the claim's process is gone, `room` whether
/// the actor is a room person's. The only ends that report `exit` are the 15 s kill, a process that died under its turn and a move to
/// the next account; a process that is still there ended an ordinary failed turn, which is `done`.
pub(super) fn end_for(how: Option<Rel>, gone: bool, room: bool) -> End {
    match how {
        Some(Rel::Killed) => End::Killed,
        Some(Rel::Died) if gone => End::Died,
        Some(Rel::OutOfUsage) => End::OutOfUsage,
        Some(Rel::Left) => End::Left,
        Some(Rel::Abandon) => End::Abandoned,
        _ if room => End::Room,
        _ => End::Done,
    }
}

/// `exit` puts the turn back in the core's queue, and nothing in the host runs a requeued core turn: an end that reports it cancels the
/// turn after. A stop is the user's; a process that died ends the item as a failed turn does in Files mode (the item has already
/// failed by now, and what it did can't be repeated); a move goes on as a new turn on the backup's session.
pub(super) fn cancel_reason(end: End) -> Option<&'static str> {
    match end {
        End::Killed => Some("stopped"),
        End::Died => Some("process died"),
        End::OutOfUsage => Some("moved to the next account"),
        End::Left => Some("the session was forked"),
        // The stop of an abandon cancels the turn itself.
        End::Done | End::Room | End::Abandoned => None,
    }
}

/// The turn `exit` put back is not wanted again ([`cancel_reason`]): a cancel the core did not answer turns a taken end into a lost one,
/// so the slow tick sends both again (the report on an ended claim changes nothing).
pub(super) fn cancelled(core: &CoreWriter, run: &Running, end: End, finished: Finished) -> Finished {
    match (finished, cancel_reason(end).filter(|_| !run.keep)) {
        (Finished::Reported, Some(why)) if !core.cancel_turn(run.turn, why) => {
            Finished::ReportLost("the cancel of the turn did not reach the core".into())
        }
        (finished, _) => finished,
    }
}

/// The end report again (a lost one, or a stub that is gone), and the cancel that follows it.
pub(super) fn end_report(core: &CoreWriter, run: &Running, end: End) -> Finished {
    cancelled(core, run, end, core.retry_end(run, end))
}

impl Actor {
    /// `begin` on the process's stub, on a blocking thread. The stub goes out of its [`Proc`] for the call and comes back. `Ok` is
    /// the answer and whether the process was restarted meanwhile (a stub that failed, or an account switch): its connection is
    /// then stale.
    async fn begin_on_stub(&mut self, core: &Arc<CoreWriter>, t: OwnedTurn) -> Result<(Begin, bool)> {
        let Some(proc) = self.conn.as_mut().and_then(|c| c.writer.as_mut()) else {
            bail!("the agent process has no stub")
        };
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
                (begin, restarted, stub, core)
            })
            .await;
            let Ok((begin, restarted, stub, core)) = joined else { return };
            if let Err((begin, _, mut stub)) = tx.send((begin, restarted, stub)) {
                let _ = tokio::task::spawn_blocking(move || orphaned(&core, &begin, &mut stub)).await;
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
        let mut fields = json!({"level": "room", "basis": basis.people, "epochs": epochs, "room": basis.room, "gen": basis.room_gen,
                                "joinEpoch": basis.join_gen, "author": group.author});
        let (name, value) = chain(self.chain_root(), self.chain_budget());
        fields[name] = json!(value);
        Some(fields)
    }

    /// The conversation's own budget (what `send` granted it), which the first round opens the chain with.
    fn chain_budget(&self) -> i64 {
        let conversation = self.active_group.as_ref().and_then(|g| g.conversation.as_deref());
        conversation.and_then(|c| self.hub.store.conversation(c)).map_or_else(super::writer::budget, |c| c.budget)
    }

    /// The turn that opened the chain of the room conversation this turn belongs to (a round after the first follows it).
    fn chain_root(&self) -> Option<i64> {
        let conversation = self.active_group.as_ref()?.conversation.as_deref()?;
        super::writer::chain_root(&self.hub.terminal.env().state, conversation)
    }

    /// What the claim names the turn: its source for the core's rows.
    fn turn_source(&self, off: bool) -> &'static str {
        if off {
            "off"
        } else if self.active_routine.is_some() {
            "routine"
        } else if self.active_group.is_some() {
            "group"
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
        // A pulled turn runs as its own person (the core's turn names it), on the core's conversation.
        let principal =
            acc.pulled.as_ref().map_or_else(|| self.cfg.person.clone().unwrap_or_default(), |p| p.person.clone());
        if acc.pulled.is_some() && acc.turn.is_none() {
            self.writer.acc = Some(acc);
            bail!("a scheduled run the core queued has lost its turn id; it is not enqueued again");
        }
        let covers = |s: &Seen| basis.iter().all(|p| s.basis.iter().any(|(q, _)| q == p));
        if !acc.seen.as_ref().is_some_and(covers) {
            acc.seen = read_epochs(core.clone(), principal.clone(), basis).await;
        }
        let Some(seen) = acc.seen.clone() else {
            self.writer.acc = Some(acc);
            bail!("the revoke epochs can't be read: the core did not answer");
        };
        let room = self.room_fields(&seen);
        let deadline = self.routine_deadline.map(|d| {
            now_ms() + i64::try_from(d.saturating_duration_since(std::time::Instant::now()).as_millis()).unwrap_or(0)
        });
        let (conversation, session) = match &acc.pulled {
            Some(p) => (p.conversation.clone(), Some(sid.to_owned())),
            None => (sid.to_owned(), None),
        };
        let turn = OwnedTurn {
            id: acc.turn,
            conversation,
            session,
            agent: self.writer_agent(),
            engine: if self.cfg.backend == "codex" { "codex" } else { "claude" }.into(),
            source: self.turn_source(off),
            person: principal,
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
            self.writer.end = Some(Rel::Left);
            self.release_turn_claim();
            self.writer_settle_end().await;
        }
        let mut sid = sid.to_owned();
        for _ in 0..3 {
            self.sync_warm().await?;
            let t = self.writer_turn(&core, &sid, off).await?;
            let opens = t.room.as_ref().is_some_and(|r| r.get("budget").is_some());
            let (begin, restarted) = self.begin_on_stub(&core, t).await?;
            if let (Some(id), Some(acc)) = (turn_of(&begin), self.writer.acc.as_mut()) {
                acc.turn = Some(id);
            }
            if let (true, Some(id), Some(group)) = (opens, turn_of(&begin), self.active_group.as_ref())
                && let Some(conversation) = group.conversation.as_deref()
            {
                chain_opened(&self.hub.terminal.env().state, conversation, id);
            }
            if restarted {
                self.rebuild_after_restart(&core).await?;
            }
            let running = match begin {
                Begin::Running(r) => r,
                Begin::NotWriter { mode: Mode::Files | Mode::Shadow, .. } => return Ok(false),
                other => {
                    return Err(self.writer_refusal(other));
                }
            };
            self.writer.run = Some(Run { run: running, conversation: sid.clone() });
            match self.writer_step7(lane, &sid, restarted, off).await {
                Ok(None) => return Ok(true),
                Ok(Some(next)) => sid = next,
                // Past the claim and the lock the start cannot go on: they are given up, not left in `run`.
                Err(error) => {
                    self.writer_give_up(Rel::Abandon).await;
                    return Err(error);
                }
            }
        }
        bail!("the session moved under the turn three times")
    }

    /// Step 7, after `Begin::Running`: a new process loads its session under the lock, then the transcript sync. `Ok(None)`: the turn
    /// may prompt `sid`. `Ok(Some(session))`: the thread left `sid` (a fork): the claim is given up and the turn begins again, as a
    /// new turn, on `session`.
    async fn writer_step7(
        &mut self,
        lane: &crate::store::Lane,
        sid: &str,
        restarted: bool,
        off: bool,
    ) -> Result<Option<String>> {
        let claim = self.writer.run.as_ref().map(|r| r.run.claim);
        if restarted {
            // A new process has loaded nothing: its session loads under the lock, as a reload does.
            self.writer_reload(lane).await?;
            self.note_ran(sid, claim);
        }
        let mut synced = crate::terminal::sync_for_turn(&self.hub, lane, sid).await?;
        // A claim of someone else on this conversation above the last this process ran under (a lock-only `--direct` run leaves no
        // transcript line the sync above could see) means the same: the session loads again, on any lane. For an off-turn turn too:
        // its engine is then stopped below, not left on stale context.
        if synced == crate::terminal::Synced::Unchanged
            && !restarted
            && let Some(claim) = claim
            && self.newer_claim(sid, claim).await
        {
            synced = crate::terminal::Synced::Moved;
        }
        // An off-turn turn's engine is already writing `sid`: it cannot be moved to another session or made to load this one again,
        // so anything but `Unchanged` is an error, and the caller stops the process (check-in MUST 6).
        if off && synced != crate::terminal::Synced::Unchanged {
            bail!("the session moved under a turn the agent started by itself");
        }
        match synced {
            crate::terminal::Synced::Unchanged => {
                self.note_ran(sid, claim);
                Ok(None)
            }
            crate::terminal::Synced::Moved => {
                if let Some(c) = self.conn.as_mut() {
                    c.loaded.remove(sid);
                }
                self.writer_reload(lane).await?;
                self.note_ran(sid, claim);
                Ok(None)
            }
            crate::terminal::Synced::Forked { .. } => {
                // The claim is on the session the thread left: ended with the process alive (an abandon would have the core kill it),
                // and the turn begins again, as a new one, on the lane's session.
                self.writer_give_up(Rel::Left).await;
                if let Some(root) = lane.thread.as_deref() {
                    self.thread_sessions.remove(root);
                }
                if let Some(c) = self.conn.as_mut() {
                    c.loaded.remove(sid);
                }
                self.writer_reload(lane).await?;
                let next = self.turn_session.clone().ok_or_else(|| anyhow!("no session"))?;
                if let Some(acc) = self.writer.acc.as_mut() {
                    acc.turn = None;
                    acc.key = new_turn_key();
                }
                Ok(Some(next))
            }
        }
    }

    /// Whether someone else claimed `sid` since this process last ran a turn on it (`CoreWriter::claim_since`). No record: the
    /// session was loaded outside a begin (`claim_before_load` only registers the row) and nobody can say what wrote since, so it
    /// reads as moved, once per session per process. A restart records its own load first.
    async fn newer_claim(&mut self, sid: &str, claim: i64) -> bool {
        let Some(proc) = self.conn.as_ref().and_then(|c| c.writer.as_ref()) else { return false };
        let Some(after) = proc.ran.get(sid).copied() else { return true };
        let (core, sid) = (proc.core.clone(), sid.to_owned());
        tokio::task::spawn_blocking(move || core.claim_since(&sid, after, claim)).await.unwrap_or(true)
    }

    /// The process has loaded `sid` as of this turn's claim (recorded after a load went through).
    fn note_ran(&mut self, sid: &str, claim: Option<i64>) {
        if let (Some(claim), Some(proc)) = (claim, self.conn.as_mut().and_then(|c| c.writer.as_mut())) {
            proc.ran.insert(sid.to_owned(), claim);
        }
    }

    /// Gives up the claim the turn holds without having prompted: the lock back, the claim ended, the turn cancelled (`how`: [`Rel::Abandon`]
    /// stops the claim, which has the core kill its process; [`Rel::Left`] reports `exit` and leaves the process alone).
    async fn writer_give_up(&mut self, how: Rel) {
        if self.writer.run.is_some() {
            self.writer.end = Some(how);
            self.release_turn_claim();
        }
        self.writer_settle_end().await;
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
            Begin::Parked { turn, why } => {
                wait(if why == Parked::TerminalLive { Why::Terminal } else { Why::Lock }, Some(turn))
            }
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
            let registered = tokio::task::spawn_blocking(move || {
                c.register_proc(pid, &s, &p, &a, None, (!p.is_empty()).then_some(seen))
            })
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
    pub(super) async fn writer_kill_process(&mut self) {
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
        let Some(mut run) = self.writer.run.take() else { return };
        let how = self.writer.end.take();
        let end = end_for(how, !pid_runs(run.run.pid), self.is_room_actor());
        // The item's last end drops what it carried. A move to the next account, or to another session (a fork), keeps the epochs and
        // starts a new turn: the old turn's row (on the old session) is cancelled below, so the next begin makes its own. A scheduled
        // run the core queued is on the core's conversation, not the session: the move begins the same turn again by its id (`exit`
        // requeued it), and nothing cancels it. A fork (`Left`) of a pulled run still drops the id: its next begin fails, and the
        // turn is cancelled as any forked turn is; the run reports it could not finish.
        let pulled = self.writer.acc.as_ref().is_some_and(|a| a.pulled.is_some());
        if how == Some(Rel::OutOfUsage) && pulled {
            run.run.keep = true;
        } else if matches!(how, Some(Rel::OutOfUsage | Rel::Left)) {
            if let Some(acc) = self.writer.acc.as_mut() {
                acc.turn = None;
                acc.key = new_turn_key();
            }
        } else {
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
                let _ = tx.send((end_report(&core, &r, end), None));
                return;
            };
            let finished = if abandon {
                // Stops the claim, kills and waits for the process (the lock must not be free while it lives). The next begin
                // restarts the stub. A stop the core did not take is sent again on the slow tick (`End::Abandoned`).
                core.abandon(&r, &stub)
            } else if !pid_runs(r.pid) {
                // The stub is gone with its lock: nothing to release, only the end to report (`end` is `Died`, or `Killed`).
                end_report(&core, &r, end)
            } else {
                core.finish(&r, &conv, &mut stub, end)
            };
            let finished = cancelled(&core, &r, end, finished);
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
            let _ = tx.send((end_report(&core, &r, end), None));
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
            && let Some(proc) =
                self.conn.as_mut().and_then(|c| c.writer.as_mut()).filter(|p| p.stub.is_none() && p.pid == run.pid)
        {
            proc.stub = Some(stub);
        }
        match finished {
            Finished::Reported => {}
            Finished::ReportLost(why) => {
                tracing::warn!(bot = %self.cfg.id, why, "the core did not take the end of a turn; sending it again");
                self.writer.lost.push((run, end));
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
