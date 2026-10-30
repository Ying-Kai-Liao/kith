//! Starting a turn and wrapping it up: final reply, notices, replies to waiters, alerts and memory.

use super::{Actor, Cmd, Conn, Done, Instructions, NoticeStyle, Queued, Seg};
use crate::agent::acp::{self, Incoming};
use crate::analytics::{self, TurnOutcome, TurnSource};
use crate::channels::text::used_pct;
use crate::chat::context::{self, Snapshot};
use crate::chat::memory;
use crate::chat::tasks::TaskState;
use crate::hub::BotStatus;
use crate::remote::push::{self, AlertKind};
use crate::store::{EntryKind, Lane, now_ms};
use anyhow::{Context, Result, anyhow};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Leads a `<bot-profile>` sent again after a compaction, so the agent drops an older copy it may still hold.
const PROFILE_AGAIN: &str = "Your context was summarized. This is your full, current profile and memory; it replaces any earlier bot profile in this conversation.";

impl Actor {
    /// The group room the current turn is in: memory loads for that room (a bot's own chat and threads are none).
    pub(super) fn memory_room(&self) -> Option<String> {
        self.active_group.as_ref().map(|g| g.lane.chat.clone())
    }

    /// This session's frozen instructions (reads memory files when re-rendering).
    pub(super) async fn snapshot(&self, session: &str) -> Result<Snapshot> {
        let (hub, cfg, session, room) = (self.hub.clone(), self.cfg.clone(), session.to_owned(), self.memory_room());
        crate::watchdog::blocking("bot-context", move || context::resolve(&hub.store, &cfg, &session, room.as_deref()))
            .await?
    }

    /// The prompt for `text` in session `sid`: what the agent should hear first (its profile, a handoff, a thread's
    /// start, a memory change) and after (a profile update, how to share files).
    async fn compose(&mut self, sid: &str, text: &str) -> Result<String> {
        if self.is_hq() {
            let recalled = self.first_message_recall(text).await;
            let moved = self.project_context().await;
            let recalled = match (moved, recalled) {
                (Some(m), Some(r)) => Some(format!("{m}\n\n{r}")),
                (m, r) => m.or(r),
            };
            return Ok(self.compose_hq(text, recalled));
        }
        let instructions = self.conn.as_ref().map_or(Instructions::FirstMessage, Conn::instructions);
        let mut snapshot = self.snapshot(sid).await?;
        let remind_files = !snapshot.system.contains("send_file");
        let mut prompt = text.to_owned();
        if let Some(handoff) = self.handoff.take() {
            prompt = format!("{handoff}\n\n{prompt}");
        }
        // A compaction may have dropped the profile the session's first message carried.
        let resend = instructions == Instructions::FirstMessage && self.profile_resend.remove(sid);
        if self.session_fresh || resend {
            // The instructions reaching the agent now hold memory as it was when they were rendered.
            memory::opened(&self.hub.store, &self.cfg.id, sid, &snapshot.memory)?;
        }
        if let Some((notice, revision)) =
            memory::change_notice(&self.hub.store, &self.cfg, sid, self.memory_room().as_deref())
        {
            prompt = format!("{notice}\n\n{prompt}");
            self.turn_memory_revision = revision;
        }
        if let Some(intro) = self.thread_intro.take() {
            prompt = format!("{intro}\n\n{prompt}");
        }
        if std::mem::take(&mut self.session_fresh) && instructions == Instructions::FirstMessage {
            prompt = format!("<bot-profile>\n{}\n</bot-profile>\n\n{prompt}", snapshot.system);
        } else if resend {
            context::mark_announced(&self.hub.store, &self.cfg.id, &snapshot, snapshot.system_identity.clone())?;
            snapshot.announced = snapshot.system_identity.clone();
            prompt = format!("<bot-profile>\n{PROFILE_AGAIN}\n\n{}\n</bot-profile>\n\n{prompt}", snapshot.system);
        }
        if let Some((update, identity)) = context::profile_update(&self.hub.store, &snapshot, &self.cfg) {
            prompt = format!("{prompt}\n\n{update}");
            self.announce = Some((snapshot, identity));
        }
        if remind_files && self.active_group.is_none() && self.active_request.is_none() && self.active_routine.is_none()
        {
            prompt.push_str("\n\n[Kith: send_file(path, name?) shares any regular file up to 100 MiB as a downloadable card. Use it instead of sending a local path. To share several files at once, pass them in one call with send_file(paths) (up to 10): they show as one group.]");
        }
        Ok(prompt)
    }

    /// A start the terminal's hold refused: the owner's own messages go to the wake queue, quietly (no notice, not
    /// failed), and the queue runs them as the thread's next turn. Whether they all did.
    async fn wait_for_terminal(&self, held: &crate::terminal::HeldByWriter, entry_ids: &[String]) -> bool {
        let owners_own = self.cfg.person.is_none()
            && held.code == "liveInTerminal"
            && self.active_request.is_none()
            && self.active_group.is_none()
            && self.active_routine.is_none()
            && !entry_ids.is_empty();
        let Some(agent) = self.cfg.hq_agent.as_deref().filter(|_| owners_own) else { return false };
        let users = |id: &String| self.hub.store.entry(id).is_some_and(|e| e.kind == EntryKind::User.as_str());
        if !entry_ids.iter().all(users) {
            return false;
        }
        for id in entry_ids {
            if let Err(error) = crate::cards::resume::wait_for_terminal(&self.hub, agent, id).await {
                tracing::warn!(error = format!("{error:#}"), "couldn't queue a message for a terminal thread");
                return false;
            }
        }
        true
    }

    /// Starts a turn in `lane` for the given user entries (none for a hidden turn). Stop and Shutdown still work
    /// while it starts (an install or a slow session load takes minutes): they end the start, and the agent process
    /// with it. Other commands wait until it is over, in order.
    pub(super) async fn start_turn(
        &mut self,
        lane: Lane,
        entry_ids: &[String],
        text: &str,
        hidden: bool,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) -> Result<()> {
        self.start_entries = entry_ids.to_vec();
        let stopping = |c: &Cmd| matches!(c, Cmd::Stop | Cmd::Shutdown);
        let cancelled = self.deferred.iter().any(stopping);
        if !cancelled && let Some(mut rx) = self.cmd_rx.take() {
            let mut open = true;
            let mut arrived = Vec::new();
            let mut finished = None;
            {
                let start = self.start_turn_inner(lane, entry_ids, text, hidden, done_tx);
                tokio::pin!(start);
                loop {
                    tokio::select! {
                        result = &mut start => {
                            finished = Some(result);
                            break;
                        }
                        cmd = rx.recv(), if open => match cmd {
                            Some(cmd) => {
                                let stop = stopping(&cmd);
                                arrived.push(cmd);
                                if stop {
                                    break;
                                }
                            }
                            None => open = false,
                        },
                    }
                }
            }
            self.cmd_rx = Some(rx);
            self.deferred.extend(arrived);
            if let Some(result) = finished {
                return result;
            }
        } else if !cancelled {
            return self.start_turn_inner(lane, entry_ids, text, hidden, done_tx).await;
        }
        // The half-started session may be anywhere (loading, installing): the process goes, the next turn starts a
        // fresh one.
        if let Some(c) = self.conn.take() {
            c.acp.kill().await;
        }
        for id in entry_ids {
            if self.hub.store.entry(id).is_some_and(|e| e.kind == EntryKind::User.as_str()) {
                self.set_user_status(id, crate::chat::team::RequestStatus::Cancelled);
            }
        }
        Err(StartCancelled.into())
    }

    async fn start_turn_inner(
        &mut self,
        lane: Lane,
        entry_ids: &[String],
        text: &str,
        hidden: bool,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) -> Result<()> {
        // A terminal may have taken the thread since the message was accepted. Checked under the thread's lock, which
        // is also where the turn is marked running: `resolveThread` then either sees this turn or the terminal's
        // claim stops it. A refused message fails with a notice and the queue goes on.
        let (rt, begin_lane) = (self.rt.clone(), lane.clone());
        let begin = move |hub: &crate::hub::Hub| {
            hub.set_runtime(&rt, |r| {
                r.status = BotStatus::Working;
                r.lane = Some(begin_lane);
            });
        };
        if let Err(held) = crate::terminal::start_gate(&self.hub, &lane, begin).await {
            // The owner's own messages wait for the terminal as wakes instead of failing: the queue starts their turn
            // once it lets go. Anything else (a bot guest's thread, a person's chat, a hidden or room turn) is refused.
            if self.wait_for_terminal(&held, entry_ids).await {
                return Err(held.into());
            }
            for id in entry_ids {
                if let Some(mut e) = self.hub.store.entry(id).filter(|e| e.kind == EntryKind::User.as_str()) {
                    e.data["status"] = json!(crate::chat::team::RequestStatus::Failed);
                    self.hub.set_entry(id, &e.data);
                }
            }
            let turn = self.hub.store.max_turn(&lane.chat);
            self.hub.add_entry(&lane, EntryKind::Notice, turn, &json!({"text": held.to_string(), "style": "error"}));
            // `start_failed` closes whatever this turn was for (a request, a room turn, a routine) and clears inflight.
            return Err(held.into());
        }
        let turn = entry_ids.last().and_then(|id| self.hub.store.entry(id)).map_or_else(
            || {
                self.hub.store.max_turn(&lane.chat)
                    + i64::from(self.active_group.is_some() || self.active_routine.is_some())
            },
            |e| e.turn,
        );
        // Only a person's own words to an hq agent say what the chat is about.
        let from_person = !hidden
            && self.is_hq()
            && self.active_request.is_none()
            && self.active_group.is_none()
            && self.active_routine.is_none()
            && !entry_ids.is_empty();
        self.project_turn = if from_person { self.infer_project(&lane, entry_ids, text).await } else { None };
        for id in entry_ids {
            // Bot-message notices belong to `chat::team`; writing them here would race its final status.
            if let Some(mut e) = self.hub.store.entry(id).filter(|e| e.kind == EntryKind::User.as_str()) {
                e.data["status"] = json!(crate::chat::team::RequestStatus::Sent);
                // It no longer waits for a terminal (it did, if a wake brought it here).
                if let Some(data) = e.data.as_object_mut() {
                    data.remove("waitsFor");
                }
                if let Some(turn) = &self.project_turn {
                    e.data["projects"] = json!(turn.projects);
                }
                self.hub.set_entry(id, &e.data);
            }
        }
        self.lane = lane.clone();
        self.hub.set_task_status(&lane, TaskState::Working);
        if !hidden {
            memory::summary::resume(&self.hub, &lane);
        }
        self.turn = Some(turn);
        self.project_divider();
        // Only the user's own words feed memory: not another bot's, a room's or a routine's.
        // The opening turn of a call the agent placed (`calls::greet`) is the host's, not the person's words.
        let greeting = !entry_ids.is_empty()
            && entry_ids.iter().all(|id| self.hub.store.entry(id).is_some_and(|e| e.data["greeting"] == true));
        let from_user = !hidden
            && !greeting
            && self.active_request.is_none()
            && self.active_group.is_none()
            && self.active_routine.is_none();
        self.turn_call = (from_user || greeting)
            && !entry_ids.is_empty()
            && entry_ids.iter().all(|id| self.hub.store.entry(id).is_some_and(|e| e.data["via"] == "call"));
        self.turn_text = from_user.then(|| text.to_owned());
        self.turn_entries = if from_user { entry_ids.to_vec() } else { Vec::new() };
        self.turn_memory_revision.clear();
        self.announce = None;
        self.stop_requested = false;
        self.cancel_deadline = None;
        self.hub.team.start_turn(&self.rt, self.active_request.as_ref());
        self.exit_tail = None;
        self.seg = Seg::None;
        self.tools.clear();
        self.plan_entry = None;
        self.last_text = None;
        self.last_agent = None;
        self.sent.clear();
        self.files.summary = None;
        self.files.cancel();
        // A delegated or group turn has no live waiter after a host restart. Its persisted
        // notices are marked interrupted instead of silently repeating work.
        self.set_inflight(
            self.active_request.is_none() && self.active_group.is_none() && self.active_routine.is_none(),
        );
        self.hub.set_runtime(&self.rt, |r| {
            r.status = BotStatus::Working;
            r.activity = match (self.compacting.is_some(), hidden) {
                (true, _) => "Summarizing the earlier conversation…",
                (false, true) => "Picking up where it left off…",
                (false, false) => "Starting…",
            }
            .into();
            r.started_at = Some(now_ms());
            r.lane = Some(lane.clone());
            r.message = if from_user { entry_ids.last().cloned() } else { None };
        });

        // Memory files changed since (in an editor, say) reach the opening or the next notice.
        if !self.is_hq() {
            let (hub, cfg) = (self.hub.clone(), self.cfg.clone());
            let _ = tokio::task::spawn_blocking(move || memory::files::scan(&hub, &cfg)).await;
        }
        self.turn_prompt = Some((text.to_owned(), hidden));
        self.turn_tried.clear();
        self.limit_seen = None;
        self.attempt_effects = false;
        self.exhausted = None;
        self.reuse_prompt = false;
        self.attempt_prompt = None;
        self.preface = None;
        self.begin_prompt(&lane, text, hidden, done_tx).await
    }

    /// Everything of a turn from getting its session on: a failed-over turn comes back here on the next entry (see
    /// `failover`).
    pub(super) async fn begin_prompt(
        &mut self,
        lane: &Lane,
        text: &str,
        hidden: bool,
        done_tx: &mpsc::UnboundedSender<Done>,
    ) -> Result<()> {
        let slot = self.slot(lane);
        if let Some(deadline) = self.routine_deadline {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), self.ensure_session(slot.as_deref()))
                .await
                .context("routine session setup timed out")??;
        } else {
            self.ensure_session(slot.as_deref()).await?;
        }
        let session = self.turn_session.clone();
        self.hub.set_runtime(&self.rt, |r| r.session.clone_from(&session));
        if let Some(sid) = session {
            self.sync_fast_mode(&sid).await;
            self.sync_call_model(&sid).await;
        }
        if hidden && self.session_fresh {
            // A new session must never silently repeat an interrupted routine, and has nothing to compact.
            self.compacting = None;
            self.raw_prompt = false;
            self.finish_routine(
                crate::routines::Status::Interrupted,
                Some("The agent could not reload this run's saved session; inspect before retrying".into()),
                None,
            );
            self.release_turn_claim();
            self.turn = None;
            if self.routine_completion.is_none() {
                self.set_inflight(false);
            }
            self.hub.team.cancel_from(&self.rt);
            self.hub.set_runtime(&self.rt, |r| {
                r.status = BotStatus::Idle;
                r.activity = String::new();
                r.started_at = None;
                r.lane = None;
                r.message = None;
                r.session = None;
            });
            return Ok(());
        }
        // Session-start chatter (banners, command lists) isn't part of the reply.
        // Some adapters (pi-acp) send it on a timer right after session/new.
        if self.session_fresh {
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        if let Some(conn) = self.conn.as_mut() {
            while let Ok(inc) = conn.rx.try_recv() {
                if let Incoming::Request { id, .. } = inc {
                    let _ = conn.acp.respond_error(id, -32601, "not supported").await;
                }
            }
        }
        let sid = self.turn_session.clone().ok_or_else(|| anyhow!("no session"))?;
        self.claim_for_turn(&sid, lane).await?;
        let prompt =
            if std::mem::take(&mut self.raw_prompt) { text.to_owned() } else { self.compose(&sid, text).await? };
        self.hub.set_runtime(&self.rt, |r| r.activity = "Thinking…".into());
        let acp = self.conn.as_ref().ok_or_else(|| anyhow!("agent not running"))?.acp.clone();
        let params = json!({
            "sessionId": sid,
            "prompt": [{"type": "text", "text": prompt}],
        });
        // A turn moved to a backup account starts again but is the same run.
        if let Some(id) = self.active_routine.as_ref().filter(|_| self.turn_tried.is_empty()) {
            self.hub.routines.mark_started(&self.hub.store, id)?;
            self.set_inflight(true);
        }
        // Written before this returns, so a Stop right after is sent after the prompt.
        let answer = acp.send("session/prompt", params).await?;
        crate::cards::resume::turn_started(&self.hub, &self.start_entries);
        if !self.turn_memory_revision.is_empty()
            && let Err(error) = memory::mark_announced(&self.hub.store, &self.cfg.id, &sid, &self.turn_memory_revision)
        {
            tracing::warn!(%error, "could not acknowledge memory update notice");
        }
        let done_tx = done_tx.clone();
        tokio::spawn(async move {
            let _ = done_tx.send(answer.response().await);
        });
        Ok(())
    }

    /// While a turn runs (in a thread or the main chat), the agent process holds the session: a writer claim for its pid keeps a terminal
    /// out, also after a host crash leaves the process behind (the claim is live as long as the process is).
    ///
    /// An app chat's session takes it before it loads (`ensure_hq_session`): that claim is the turn's.
    pub(super) async fn claim_for_turn(&mut self, sid: &str, lane: &Lane) -> Result<()> {
        let pid = self.conn.as_ref().and_then(|c| c.acp.pid());
        if pid.is_some() && self.turn_claim.as_ref().is_some_and(|(held, by)| held == sid && Some(*by) == pid) {
            return Ok(());
        }
        self.release_turn_claim();
        let Some(pid) = pid else {
            return Ok(());
        };
        let state = self.hub.terminal.env().state;
        // A bot guest's conversation is held as a bot, so an app send and a terminal see who writes.
        let bot =
            lane.thread.as_deref().is_some_and(|r| self.hub.store.bot_conversation_of_thread(&lane.chat, r).is_some());
        let kind = if bot { crate::terminal::claim::Kind::Bot } else { crate::terminal::claim::Kind::App };
        // flock and `ps` block: not on an async worker thread.
        // The claim names the run's person (its bot's `person`, set by the host), so revoking them ends this process too,
        // also when a restarted host no longer knows it. The owner's runs carry none and are never swept.
        let (st, id, person, agent) = (state, sid.to_owned(), self.cfg.person.clone(), self.cfg.hq_agent.clone());
        let taken = tokio::task::spawn_blocking(move || {
            if let Some(p) = &person {
                // The warm process outlives the turn claim (and can outlive a crashed host): it stays findable.
                if let Err(error) = crate::terminal::claim::take_proc(&st, pid, p, agent.as_deref()) {
                    tracing::warn!(%error, "couldn't record a person's agent process");
                }
            }
            crate::terminal::claim::take_as(&st, &id, kind, pid, false, person.as_deref(), agent.as_deref())
        })
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e)));
        match taken {
            Ok(Ok(())) => self.turn_claim = Some((sid.to_owned(), pid)),
            // A terminal got in between the start check and here: it keeps the session.
            Ok(Err(held)) if held.0.pid == pid => self.turn_claim = Some((sid.to_owned(), pid)),
            // A main-lane claim only keeps a terminal out; it never stops the turn (bots of one room or team may
            // hold the same session id, and a terminal can't attach to the main chat). Run without the claim.
            Ok(Err(held)) if lane.thread.is_none() => {
                tracing::warn!(holder = held.0.pid, "main-lane turn runs without its writer claim: another holds it");
            }
            Ok(Err(_)) => {
                let held =
                    crate::terminal::HeldByWriter { code: if bot { "heldByOtherWriter" } else { "liveInTerminal" } };
                self.notice(&held.to_string(), NoticeStyle::Error);
                return Err(held.into());
            }
            Err(error) => tracing::warn!(%error, "couldn't claim a session for a turn"),
        }
        Ok(())
    }

    pub(super) fn release_turn_claim(&mut self) {
        if let Some((sid, pid)) = self.turn_claim.take() {
            let state = self.hub.terminal.env().state;
            crate::terminal::claim::release(&state, &sid, pid, false);
        }
    }

    pub(super) fn finish_turn(&mut self, done: &Done) {
        if self.turn.is_none() {
            return;
        }
        let compaction = self.compacting.is_some();
        // A terminal thread's transcript now holds this turn: it isn't imported again.
        crate::terminal::host_turn_ended(&self.hub, &self.lane);
        self.after_turn(done);
        // The bot's own edits to its memory files, checked and shown in the chat it made them from.
        if !self.is_hq() {
            memory::files::scan_after_turn(self.hub.clone(), self.cfg.clone());
        }
        if let (Some(conn), Some(sid)) = (self.conn.as_mut(), &self.turn_session)
            && let Some(used) = conn.loaded.get_mut(sid)
        {
            *used = Instant::now();
        }
        self.files.cancel();
        self.flush(true);
        self.seg = Seg::None;
        // Unanswered permission cards can't be answered after the turn.
        let ids: Vec<String> = self.perms.drain().map(|(k, _)| k).collect();
        // Cards nobody answered: an owner's gave up the turn's slot, which comes back with the dropped guard.
        self.parked_prompts.clear();
        for id in ids {
            // A question with a card parks with its asker; an old one can't be answered and shows as expired.
            if crate::cards::park_entry(&self.hub, &id) {
                continue;
            }
            if let Some(mut e) = self.hub.store.entry(&id)
                && e.data["status"] == "pending"
            {
                e.data["status"] = "expired".into();
                self.hub.set_entry(&id, &e.data);
            }
        }
        let stopped = self.stop_requested || matches!(&done, Ok(v) if v["stopReason"] == "cancelled");
        // Tool calls the agent never closed out would spin forever in the trace.
        for entry_id in self.tools.values() {
            if let Some(mut e) = self.hub.store.entry(entry_id)
                && matches!(e.data["status"].as_str(), Some("pending" | "in_progress"))
            {
                e.data["status"] = if stopped || done.is_err() { "failed" } else { "completed" }.into();
                self.hub.set_entry(entry_id, &e.data);
            }
        }
        let mut final_text = None;
        let routine = self.active_routine.is_some();
        let grouped = self.active_group.is_some() || routine;
        let delegated = self.active_request.as_ref().is_some_and(crate::chat::team::BotRequest::expects_reply);
        let sent = std::mem::take(&mut self.sent);
        if !sent.is_empty() {
            // The bot talked through send_message: what it wrote as its reply stays in the trace.
            self.last_text = None;
            final_text = Some(sent.join("\n\n"));
        }
        if let Some(id) = self.last_text.take().filter(|_| !stopped)
            && let Some(mut e) = self.hub.store.entry(&id)
        {
            let text = e.data["text"].as_str().map(str::to_owned);
            // A pass in a room stays in the trace; the room doesn't see it.
            if !(grouped && text.as_deref().is_none_or(crate::chat::group::is_pass)) {
                // A room turn that a newer message or Stop moved on from: the reply is kept, marked, and wakes nobody.
                if let Some(g) = &self.active_group {
                    // Saved with the `final` mark just below (a room turn is never a delegated ask).
                    if let Some(conversation) = &g.conversation {
                        e.data["conversation"] = conversation.clone().into();
                    }
                    if !crate::chat::group::current(&self.hub.store, &g.lane, g.epoch) {
                        e.data["late"] = true.into();
                        let stopped = g
                            .conversation
                            .as_deref()
                            .and_then(|c| self.hub.store.conversation(c))
                            .is_some_and(|c| c.state == crate::store::ConvState::Stopped);
                        e.data["lateWhy"] = if stopped { "stopped" } else { "newer" }.into();
                    }
                }
                // An ask's answer belongs to its requesting bot and exchange notice.
                // Independent messages still report to the user in this chat.
                if !delegated {
                    e.data["final"] = true.into();
                    self.hub.set_entry(&id, &e.data);
                }
                final_text = text;
            }
        }
        // After the `final` marking above: `flush(true)` rewrote the open segment's data wholesale.
        if !compaction
            && let (Some(id), Some(sid)) = (self.last_agent.take(), &self.turn_session)
            && let Some(pct) =
                context_percent(self.conn.as_ref().and_then(|c| c.usage.get(sid)).copied(), self.context_show)
        {
            tag_context(&self.hub, &id, pct);
        }
        if final_text.is_none() {
            final_text = self.files.summary.take();
        }
        let stop_reason = match &done {
            Ok(v) => v["stopReason"].as_str().unwrap_or("end_turn").to_owned(),
            Err(_) => "error".into(),
        };
        match (&done, stop_reason.as_str()) {
            (Err(e), _) if !self.stop_requested => {
                let tail = self.exit_tail.take().filter(|t| !t.is_empty());
                if let Some(text) = self.exhausted.take() {
                    // Every account for this agent is out of usage: say who and until when, not the adapter's error.
                    self.notice(&text, NoticeStyle::Info);
                } else if !self.invite_sign_in(e) {
                    let detail = tail.map(|t| format!("\n\n{}", acp::truncate(&t, 1200))).unwrap_or_default();
                    self.notice(&format!("The agent failed: {e}{detail}"), NoticeStyle::Error);
                }
            }
            (_, "cancelled") | (Err(_), _) => self.notice("Stopped.", NoticeStyle::Info),
            (_, "max_tokens") => self.notice("The agent hit its output limit.", NoticeStyle::Info),
            (_, "max_turn_requests") => self.notice("The agent hit its step limit for this turn.", NoticeStyle::Info),
            (_, "refusal") => self.notice("The agent declined to continue.", NoticeStyle::Info),
            _ => {}
        }
        let failure = done.as_ref().err().filter(|_| !self.stop_requested);
        let failed = failure.is_some();
        self.report_turn(stopped, failure, &stop_reason);
        let reply = if stopped {
            Err(anyhow!("recipient was stopped; partial work may have happened"))
        } else if stop_reason != "end_turn" {
            Err(anyhow!("recipient did not finish ({stop_reason}); partial work may have happened"))
        } else {
            final_text
                .clone()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("recipient finished without a text reply"))
        };
        if routine {
            let (status, detail) = if self.routine_timed_out {
                (
                    crate::routines::Status::Failed,
                    Some("Routine exceeded its execution time limit; its agent process was stopped".into()),
                )
            } else if stopped {
                (crate::routines::Status::Cancelled, Some("Stopped by user".into()))
            } else if failed || stop_reason != "end_turn" {
                (
                    crate::routines::Status::Failed,
                    Some(format!("Routine did not finish ({stop_reason}); inspect the run conversation")),
                )
            } else {
                (crate::routines::Status::Succeeded, None)
            };
            let text = (status == crate::routines::Status::Succeeded).then(|| final_text.clone()).flatten();
            self.finish_routine(status, detail, text);
        }
        let message_failed =
            !stopped && reply.is_err() && self.active_request.as_ref().is_some_and(|r| !r.expects_reply());
        self.complete_request(reply, stopped);
        self.complete_group(if stopped || failed || stop_reason != "end_turn" {
            Err(anyhow!("turn did not complete ({stop_reason})"))
        } else {
            Ok(final_text.clone())
        });
        self.hub.set_task_status(&self.lane, TaskState::after_turn(stopped, failed, &stop_reason));
        self.release_turn_claim();
        self.turn = None;
        if self.routine_completion.is_none() {
            self.set_inflight(false);
        }
        // The message reached the agent: the profile update it carried is now known to it.
        if let (Ok(_), Some((snapshot, identity))) = (&done, self.announce.take())
            && let Err(error) = context::mark_announced(&self.hub.store, &self.cfg.id, &snapshot, identity)
        {
            tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't record the profile update");
        }
        if let (Some(user), Some(agent), "end_turn") = (self.turn_text.take(), &final_text, stop_reason.as_str()) {
            let turn = crate::chat::naming::Turn { user, agent: agent.clone() };
            let _ = self.tidier.send(memory::TidyEvent::Turn(turn));
        }
        self.hub.set_runtime(&self.rt, |r| {
            r.status = if failed { BotStatus::Error } else { BotStatus::Idle };
            r.activity = String::new();
            r.started_at = None;
            r.lane = None;
            r.message = None;
            r.session = None;
        });
        self.hub.team.cancel_from(&self.rt);
        // A room turn's news comes from the group once the room is done (see `group`).
        let more = self.queue.iter().any(|q| matches!(q, Queued::User { .. }));
        if !self.stop_requested
            && !delegated
            && !grouped
            && let Some(kind) = turn_alert(failed || message_failed, &stop_reason, more)
        {
            let body = final_text.unwrap_or_else(|| match kind {
                AlertKind::Done => "Finished.".into(),
                _ => "The agent didn't finish.".into(),
            });
            push::notify(&self.hub, &self.cfg, None, &self.cfg.name, &body, kind);
        }
    }
}

impl Actor {
    /// `turn_finished`, while the turn's start time is still in the runtime.
    pub(super) fn report_turn(&self, stopped: bool, failure: Option<&anyhow::Error>, stop_reason: &str) {
        let outcome = if stopped {
            TurnOutcome::Stopped
        } else if failure.is_some() {
            TurnOutcome::Failed
        } else if stop_reason == "end_turn" {
            TurnOutcome::Done
        } else {
            TurnOutcome::Incomplete
        };
        let source = if self.active_routine.is_some() {
            TurnSource::Routine
        } else if self.active_group.is_some() {
            TurnSource::Group
        } else if self.active_request.is_some() {
            TurnSource::Bot
        } else {
            TurnSource::User
        };
        let started = self.hub.runtime(&self.rt).started_at.unwrap_or_else(now_ms);
        let ms = now_ms() - started;
        analytics::turn_finished(&self.hub, &self.cfg.backend, outcome, stop_reason, failure, source, ms);
    }
}

/// The context fullness to show on a reply: whole percent, once the session is at least `show` full.
/// None below that, with `show` off, or when the session's window size is unknown.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "token counts are far below 2^52 and a percent fits in i64"
)]
fn context_percent(usage: Option<(u64, u64)>, show: Option<f64>) -> Option<i64> {
    let (used, size) = usage?;
    let pct = used_pct(used as f64, size as f64)?;
    (pct >= show?).then(|| pct.round() as i64)
}

/// Merges `contextPercent` into the entry's data, keeping everything else the entry holds.
fn tag_context(hub: &crate::hub::Hub, id: &str, pct: i64) {
    if let Some(mut e) = hub.store.entry(id).filter(|e| e.kind == EntryKind::Agent.as_str()) {
        e.data["contextPercent"] = json!(pct);
        hub.set_entry(id, &e.data);
    }
}

/// A finished turn alerts once the user's work is done: not while their next message
/// is already queued. A turn that ended short of `end_turn` always reports the failure.
fn turn_alert(failed: bool, stop_reason: &str, more: bool) -> Option<AlertKind> {
    if failed || !matches!(stop_reason, "end_turn" | "cancelled") {
        return Some(AlertKind::Failed);
    }
    (stop_reason == "end_turn" && !more).then_some(AlertKind::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_hint_needs_enough_use_and_a_known_size() {
        assert_eq!(context_percent(Some((500, 1000)), Some(30.0)), Some(50));
        assert_eq!(context_percent(Some((300, 1000)), Some(30.0)), Some(30), "at the threshold");
        assert_eq!(context_percent(Some((299, 1000)), Some(30.0)), None, "below");
        assert_eq!(context_percent(Some((500, 1000)), None), None, "off");
        assert_eq!(context_percent(None, Some(30.0)), None, "no usage yet");
        assert_eq!(context_percent(Some((500, 0)), Some(0.0)), None, "unknown size");
        assert_eq!(context_percent(Some((1234, 10000)), Some(0.0)), Some(12), "rounds");
    }

    #[tokio::test]
    async fn the_tag_lands_on_one_entry_and_keeps_its_data() {
        let dir = std::env::temp_dir().join(format!("kith-hint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = crate::hub::Hub::new(store, "hint-test".into(), identity, "test".into(), 0);
        let lane = Lane { chat: "bot".into(), thread: None };
        let first = hub.add_entry(&lane, EntryKind::Agent, 1, &json!({"text": "a", "final": false})).unwrap();
        let last =
            hub.add_entry(&lane, EntryKind::Agent, 1, &json!({"text": "b", "final": true, "files": [1]})).unwrap();
        tag_context(&hub, &last.id, 42);
        tag_context(&hub, &last.id, 43);
        let last = hub.store.entry(&last.id).unwrap();
        assert_eq!(last.data["contextPercent"], json!(43));
        assert_eq!((last.data["text"].as_str(), last.data["final"].as_bool()), (Some("b"), Some(true)));
        assert_eq!(last.data["files"], json!([1]));
        assert!(hub.store.entry(&first.id).unwrap().data.get("contextPercent").is_none());
        tag_context(&hub, "missing", 10);
    }

    #[test]
    fn only_finished_tasks_alert() {
        assert_eq!(turn_alert(false, "end_turn", false), Some(AlertKind::Done));
        assert_eq!(turn_alert(false, "end_turn", true), None, "next message queued");
        assert_eq!(turn_alert(false, "cancelled", false), None);
        assert_eq!(turn_alert(false, "max_turn_requests", false), Some(AlertKind::Failed));
        assert_eq!(turn_alert(true, "error", true), Some(AlertKind::Failed));
    }
}

/// A Stop (or shutdown) ended the turn while it was starting: nothing failed.
#[derive(Debug)]
pub(super) struct StartCancelled;

impl std::fmt::Display for StartCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped before the agent started")
    }
}

impl std::error::Error for StartCancelled {}
