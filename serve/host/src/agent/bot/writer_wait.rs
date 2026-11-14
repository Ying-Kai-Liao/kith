//! A start the core cannot take yet waits on the actor (design 1.3, "Unpark"): the item stays where it was (its turn id, its
//! key and its epochs are kept, the actor's queue stays behind it), a slow tick and a kick read the mode and ask the core to wake
//! it, and the start runs again with the same turn id. A routine with a deadline expires instead. A lost end report is sent again on
//! the same tick.

use super::writer::{Why, WriterWait, core};
use super::{Actor, Done, NoticeStyle};
use crate::chat::team::RequestStatus;
use crate::hub::BotStatus;
use crate::store::{EntryKind, Lane, now_ms};
use crate::terminal::core_writer::Mode;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How often a parked start asks the core again (the mode, the unpark). A kick skips the wait.
const SLOW: Duration = Duration::from_secs(2);

/// What a start is called with, kept so a park can start it again.
#[derive(Clone, Debug)]
pub(super) struct StartArgs {
    pub lane: Lane,
    pub entry_ids: Vec<String>,
    pub text: String,
    pub hidden: bool,
}

/// A start waiting for the core. The actor's `active_*` fields keep what the item is (a routine, an ask, a room turn).
pub(super) struct Parked {
    args: StartArgs,
    why: Why,
    /// The Claude session the turn claims: where the core's park is.
    conversation: String,
    /// The core's turn, when it was enqueued.
    turn: Option<i64>,
    since: Instant,
    /// A retry that waits out a moment (a process being stopped) is not due before it.
    not_before: Option<Instant>,
    due: bool,
}

impl Actor {
    /// Records what the start in progress was called with, so [`Self::writer_park`] can start it again.
    pub(super) fn writer_note_start(&mut self, lane: &Lane, entry_ids: &[String], text: &str, hidden: bool) {
        self.writer.start =
            Some(StartArgs { lane: lane.clone(), entry_ids: entry_ids.to_vec(), text: text.to_owned(), hidden });
    }

    /// Whether a thread a terminal holds parks the start (Writer mode) instead of refusing it.
    pub(super) async fn writer_parks(&self) -> bool {
        super::writer::mode_of(&self.hub).await.0 == Mode::Writer
    }

    /// Parks the start that ended in `wait` instead of failing it. Whether it did (`false`: no start to park).
    pub(super) fn writer_park(&mut self, wait: &WriterWait) -> bool {
        let Some(args) = self.writer.start.take() else { return false };
        // Back to waiting: the messages show as queued again, nothing runs, no turn is recorded as running.
        for id in &args.entry_ids {
            if !crate::cards::resume::wake_carries(id)
                && self.hub.store.entry(id).is_some_and(|e| e.kind == EntryKind::User.as_str() && e.data["status"] == "sent")
            {
                self.set_user_status(id, RequestStatus::Queued);
            }
        }
        self.turn = None;
        self.set_inflight(false);
        self.hub.team.cancel_from(&self.rt);
        let text = wait.to_string();
        let (lane, session) = (args.lane.clone(), self.turn_session.clone().unwrap_or_default());
        self.hub.set_runtime(&self.rt, |r| {
            r.status = BotStatus::Working;
            r.activity = text;
            r.started_at = Some(now_ms());
            r.lane = Some(lane);
            r.message = None;
        });
        let first = !self.writer.parked.as_ref().is_some_and(|p| p.why == wait.why);
        if wait.why == Why::Core && first {
            // "core not answering": said once per wait, in the chat.
            self.notice("core not answering", NoticeStyle::Info);
        }
        let not_before = (wait.why == Why::Retry).then(|| Instant::now() + super::writer::RETRY_AFTER);
        self.writer.parked = Some(Parked {
            args,
            why: wait.why.clone(),
            conversation: session,
            turn: wait.turn.or(self.writer.acc.as_ref().and_then(|a| a.turn)),
            since: self.writer.parked.as_ref().map_or_else(Instant::now, |p| p.since),
            not_before,
            due: false,
        });
        true
    }

    /// The parked start is given up (Stop, a cancelled ask or room turn, shutdown): its core turn is cancelled and the item ends as a
    /// stopped start does.
    pub(super) fn writer_drop_parked(&mut self) {
        let Some(parked) = self.writer.parked.take() else { return };
        self.writer_cancel_turn(parked.turn, "stopped while waiting");
        self.writer.acc = None;
        self.start_failed(&super::turn::StartCancelled.into());
    }

    fn writer_cancel_turn(&self, turn: Option<i64>, why: &'static str) {
        if let (Some(turn), Some(core)) = (turn, core(&self.hub)) {
            tokio::task::spawn_blocking(move || core.cancel_turn(turn, why));
        }
    }

    /// A kick: something the parked start waits for may have ended (a terminal claim). The next tick asks at once.
    pub(super) fn writer_kick(&mut self) {
        self.writer.tick = None;
    }

    /// The slow tick (every flush tick calls it; it asks the core every [`SLOW`] or when kicked): a lost end report is sent again, a
    /// parked start's mode is read and the core is asked to wake it, and a start that is due runs again with its turn's id.
    pub(super) async fn writer_tick(&mut self, done_tx: &mpsc::UnboundedSender<Done>) {
        self.writer_collect();
        if self.writer.tick.is_some_and(|t| t.elapsed() < SLOW) {
            return;
        }
        if self.writer.run.is_none() && self.writer.parked.is_none() && self.writer.lost.is_none() && !self.writer.dirty {
            return;
        }
        self.writer.tick = Some(Instant::now());
        if self.writer.dirty {
            self.writer.dirty = false;
            if let Err(error) = self.sync_warm().await {
                tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't bring the core's process rows in step");
            }
        }
        if let (Some((run, end)), Some(core)) = (self.writer.lost.take(), core(&self.hub)) {
            let (r, e) = (run.clone(), end);
            let sent = tokio::task::spawn_blocking(move || core.retry_end(&r, e)).await;
            if !matches!(sent, Ok(crate::terminal::core_writer::Finished::Reported)) {
                self.writer.lost = Some((run, end));
            }
        }
        self.writer_wake(done_tx).await;
    }

    /// The parked start, if there is one: expires a routine past its deadline, reads the mode and asks the core to wake it.
    async fn writer_wake(&mut self, done_tx: &mpsc::UnboundedSender<Done>) {
        let Some(parked) = self.writer.parked.as_ref() else { return };
        if self.active_routine.is_some() && self.routine_deadline.is_some_and(|d| Instant::now() >= d) {
            let turn = parked.turn;
            self.writer.parked = None;
            self.writer_cancel_turn(turn, "expired");
            self.writer.acc = None;
            self.start_failed(&anyhow::anyhow!("the routine expired while it waited for its session"));
            return;
        }
        // An ask or a room turn that nobody waits for any more is not started.
        let closed = self.active_request.as_ref().is_some_and(crate::chat::team::BotRequest::reply_closed)
            || self.active_group.as_ref().is_some_and(|g| {
                g.reply.is_closed() || !crate::chat::group::current(&self.hub.store, &g.lane, g.epoch)
            });
        if closed {
            self.writer_drop_parked();
            return;
        }
        let (why, conv, turn, not_before) = (parked.why.clone(), parked.conversation.clone(), parked.turn, parked.not_before);
        let Some(core) = core(&self.hub) else { return };
        let c = core.clone();
        let mode = tokio::task::spawn_blocking(move || c.mode()).await.unwrap_or(Mode::Held);
        let due = match mode {
            // The module was stepped back: the item runs on the other path.
            Mode::Files | Mode::Shadow => true,
            Mode::Held => false,
            Mode::Writer => match why {
                Why::Core | Why::Claim => true,
                Why::Retry => not_before.is_none_or(|t| Instant::now() >= t),
                // The retried take is the probe: the core only has to say the claim that parked the turn is gone.
                Why::Terminal | Why::Lock => {
                    let woken = tokio::task::spawn_blocking(move || core.unpark(&conv, false)).await;
                    match (woken, turn) {
                        (Ok(Ok(turns)), Some(t)) => turns.contains(&t),
                        (Ok(Ok(turns)), None) => !turns.is_empty(),
                        _ => false,
                    }
                }
            },
        };
        if let Some(p) = self.writer.parked.as_mut() {
            p.due = due;
        }
        if due {
            self.writer_resume(done_tx).await;
        }
    }

    /// Starts the parked start again (same turn id: the retried take is the probe). No slot from the gate yet: it stays parked.
    async fn writer_resume(&mut self, done_tx: &mpsc::UnboundedSender<Done>) {
        if !self.have_room() {
            return;
        }
        let Some(parked) = self.writer.parked.take() else { return };
        let a = parked.args;
        tracing::info!(bot = %self.cfg.id, waited_ms = u64::try_from(parked.since.elapsed().as_millis()).unwrap_or(0), "a parked start runs again");
        if let Err(error) = self.start_turn(a.lane, &a.entry_ids, &a.text, a.hidden, done_tx).await {
            self.start_failed(&error);
        }
    }

    /// The error a parked turn's session shows while it waits, for the apps: the same words as the notice.
    #[allow(dead_code, reason = "read by the tests")]
    pub(super) fn writer_waiting(&self) -> Option<String> {
        self.writer.parked.as_ref().map(|p| match p.why {
            Why::Core => "core not answering".to_owned(),
            Why::Terminal => "waiting for your terminal session".to_owned(),
            Why::Lock | Why::Claim | Why::Retry => "waiting for the agent's session".to_owned(),
        })
    }
}
