//! A turn the agent started by itself. Claude Code wakes an agent when a background task finishes, with no host turn
//! running: the agent calls `send_file` or `send_message` and writes its reply, and all of it used to vanish ("no turn
//! is running"). The host now opens an off-turn turn in the lane of the session's last turn, so the work lands where
//! the person was talking, and ends it after a quiet period through the normal finish path.

use super::{Actor, NoticeStyle, Seg};
use crate::chat::tasks::TaskState;
use crate::hub::BotStatus;
use crate::store::{EntryKind, now_ms};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long an off-turn turn waits with no update from the agent before it ends. A test host shortens it with
/// `KITH_SERVE_OFF_TURN_QUIET_MS`.
fn quiet() -> Duration {
    static QUIET: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *QUIET.get_or_init(|| {
        std::env::var("KITH_SERVE_OFF_TURN_QUIET_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(Duration::from_secs(30), Duration::from_millis)
    })
}

/// A tool call still running holds the turn open this long: a long command says nothing while it runs.
const TOOL_QUIET: Duration = Duration::from_secs(10 * 60);

/// Whether an update is the agent doing something now, for a turn nobody asked for. Usage and option updates are
/// bookkeeping and never open one; a late update for a tool call that already closed doesn't either.
pub(super) fn wakes(actor: &Actor, u: &Value) -> bool {
    match u["sessionUpdate"].as_str() {
        Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "plan") => true,
        Some("tool_call_update") => u["toolCallId"]
            .as_str()
            .and_then(|id| actor.tools.get(id))
            .and_then(|entry| actor.hub.store.entry(entry))
            .is_some_and(|e| matches!(e.data["status"].as_str(), Some("pending" | "in_progress"))),
        _ => false,
    }
}

impl Actor {
    /// Makes sure a turn is running for `send_file` and `send_message`: the one that is, or an off-turn turn in the
    /// lane of the session's last turn. Why not, when it can't.
    pub(super) async fn ensure_turn(&mut self) -> Result<()> {
        if self.turn.is_some() {
            if self.off_turn.is_some() {
                self.off_turn = Some(Instant::now());
            }
            return Ok(());
        }
        self.adopt_off_turn().await
    }

    /// Opens the off-turn turn: only in the bot's own chat and threads (`off_lane` is none after a room, an ask or a
    /// routine), never after a Stop, and only where no terminal holds the thread.
    pub(super) async fn adopt_off_turn(&mut self) -> Result<()> {
        ensure!(!self.stop_requested, "turn is stopping");
        let (Some(lane), Some(sid)) = (self.off_lane.clone(), self.turn_session.clone()) else {
            anyhow::bail!("no turn is running");
        };
        ensure!(self.conn.is_some(), "no turn is running");
        ensure!(self.hub.store.bot(&self.cfg.id)?.is_some_and(|bot| !bot.deleted), "bot was deleted");
        // The same check a turn start makes, under the thread's lock, which is also where the lane is marked working.
        let (rt, begin_lane) = (self.rt.clone(), lane.clone());
        crate::terminal::start_gate(&self.hub, &lane, move |hub| {
            hub.set_runtime(&rt, |r| {
                r.status = BotStatus::Working;
                r.lane = Some(begin_lane);
            });
        })
        .await?;
        if let Err(error) = self.claim_for_turn(&sid, &lane).await {
            self.hub.set_runtime(&self.rt, |r| {
                r.status = BotStatus::Idle;
                r.lane = None;
            });
            return Err(error);
        }
        self.lane = lane.clone();
        self.turn = Some(self.hub.store.max_turn(&lane.chat));
        self.off_turn = Some(Instant::now());
        self.hub.set_task_status(&lane, TaskState::Working);
        self.turn_text = None;
        self.turn_entries.clear();
        self.secrets_turn = false;
        self.cancel_deadline = None;
        self.attempt_effects = false;
        self.exit_tail = None;
        self.seg = Seg::None;
        self.plan_entry = None;
        self.plan_card = None;
        self.last_text = None;
        self.last_agent = None;
        self.sent.clear();
        self.files.summary = None;
        self.hub.team.start_turn(&self.rt, None);
        self.hub.set_runtime(&self.rt, |r| {
            r.activity = "Working…".into();
            r.started_at = Some(now_ms());
            r.session = Some(sid);
        });
        tracing::info!(bot = %self.cfg.id, lane = %lane.key(), "the agent woke itself: opened an off-turn turn");
        Ok(())
    }

    /// Ends the off-turn turn the normal way: status back to idle, the last text saved as the reply.
    pub(super) fn end_off_turn(&mut self) {
        if self.off_turn.take().is_none() {
            return;
        }
        self.cancel_deadline = None;
        self.finish_turn(&Ok(json!({"stopReason": "end_turn"})));
    }

    /// Whether the off-turn turn has been quiet long enough to end: nothing from the agent, no question open, no
    /// file being copied.
    pub(super) fn off_turn_quiet(&self) -> bool {
        let Some(since) = self.off_turn else { return false };
        if !self.perms.is_empty() || !self.files.is_idle() {
            return false;
        }
        if since.elapsed() < quiet() {
            return false;
        }
        let running = self.tools.values().any(|id| {
            self.hub
                .store
                .entry(id)
                .is_some_and(|e| matches!(e.data["status"].as_str(), Some("pending" | "in_progress")))
        });
        !running || since.elapsed() >= TOOL_QUIET
    }

    /// Whether a send that fails now is the agent's own doing outside any turn the person started: it gets a notice.
    pub(super) fn off_turn_context(&self) -> bool {
        self.turn.is_none() || self.off_turn.is_some()
    }

    /// An off-turn send that couldn't be made: a notice in the lane the person is reading, and the log. Never silent.
    pub(super) fn off_turn_failed(&self, what: &str, error: &anyhow::Error) {
        tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "off-turn send failed: {what}");
        // After a room, an ask or a routine there is no lane of the person's to tell.
        let Some(lane) = self.off_lane.as_ref() else { return };
        let text = format!("{} tried to {what} but couldn't: {error}", self.cfg.name);
        let turn = self.hub.store.max_turn(&lane.chat);
        let data = json!({"text": text, "style": NoticeStyle::Error, "author": self.cfg.id});
        self.hub.add_entry(lane, EntryKind::Notice, turn, &data);
    }
}

/// What `send_file` was asked to send, for a notice: the given name, else the files' names.
pub(super) fn file_label(sources: &[String], name: Option<&str>) -> String {
    let names: Vec<&str> = sources
        .iter()
        .map(|s| std::path::Path::new(s).file_name().and_then(|n| n.to_str()).unwrap_or(s.as_str()))
        .collect();
    match (name, names.as_slice()) {
        (Some(name), _) => format!("send {name}"),
        (None, [one]) => format!("send {one}"),
        (None, many) => format!("send {}", many.join(", ")),
    }
}
