//! Commands from clients and the queue of turns waiting for the bot.

use super::{Actor, CANCEL_GRACE, Cmd, Done, NoticeStyle, Queued, RESUME_PROMPT, RoutineCompletion, inflight_key};
use crate::agent::{folder, own_memory};
use crate::chat::memory;
use crate::chat::router::{self, Route};
use crate::chat::team::RequestStatus;
use crate::hub::BotStatus;
use crate::market::requests;
use crate::store::{EntryKind, Lane, now_ms};
use anyhow::{Result, anyhow};
use serde_json::json;
use std::time::Instant;
use tokio::sync::mpsc;

impl Actor {
    pub(super) async fn on_cmd(&mut self, cmd: Cmd, done_tx: &mpsc::UnboundedSender<Done>) -> bool {
        match cmd {
            Cmd::RefreshTools => {
                self.tools_changed = true;
            }
            Cmd::Routine(id) => {
                self.queue.push_back(Queued::Routine(id));
                if self.turn.is_none() {
                    self.next_in_queue(done_tx).await;
                }
            }
            Cmd::Send { lane, entry_id, text, route } => {
                if route {
                    self.route_later(&lane, &entry_id);
                }
                self.queue.push_back(Queued::User { lane, entry_id, text, alone: false });
                if self.turn.is_none() {
                    self.next_in_queue(done_tx).await;
                }
            }
            Cmd::WakeBatch { lane, items } => {
                // All of them before the first start, so an idle actor takes them as one turn.
                for (entry_id, text) in items {
                    self.queue.push_back(Queued::User { lane: lane.clone(), entry_id, text, alone: false });
                }
                if self.turn.is_none() {
                    self.next_in_queue(done_tx).await;
                }
            }
            Cmd::Routed { entry_id, route } => self.apply_route(&entry_id, route).await,
            Cmd::Unsend { entry_id, reply } => {
                let _ = reply.send(self.unsend(&entry_id));
            }
            Cmd::Group(turn) => {
                self.queue.push_back(Queued::Group(turn));
                if self.turn.is_none() {
                    self.next_in_queue(done_tx).await;
                }
            }
            Cmd::CancelGroup { chat } => {
                // Dropping a queued turn's reply tells the room it won't come.
                self.queue.retain(|q| !matches!(q, Queued::Group(g) if g.lane.chat == chat));
                if self.active_group.as_ref().is_some_and(|g| g.lane.chat == chat) {
                    self.cancel_turn().await;
                }
            }
            Cmd::BotRequest(ask) => {
                self.queue.push_back(Queued::BotRequest(ask));
                if self.turn.is_none() {
                    self.next_in_queue(done_tx).await;
                }
            }
            Cmd::CancelAsk { id } => {
                self.queue.retain(|q| !matches!(q, Queued::BotRequest(a) if a.expects_reply() && a.id == id));
                if self.active_request.as_ref().is_some_and(|a| a.expects_reply() && a.id == id) {
                    self.hub.team.cancel_from(&self.rt);
                    self.stop_requested = true;
                    // The request has expired. Kill only its process so a harness
                    // ignoring cooperative cancellation cannot block the queue.
                    if let Some(c) = self.conn.take() {
                        c.acp.kill().await;
                    }
                }
            }
            Cmd::SendToUser { text, reply } => {
                // The outbox gave up waiting and told the agent the send failed; posting now would show a bubble
                // the agent will send again.
                if reply.is_closed() {
                    return true;
                }
                let _ = reply.send(self.send_to_user(text));
            }
            Cmd::SendFile { paths, name, cancelled, reply } => self.begin_file(paths, name, cancelled, reply),
            Cmd::Stop => self.stop().await,
            Cmd::IdleFor(reply) => {
                let _ = reply.send(self.idle_for());
            }
            Cmd::StopIdle(reply) => {
                let _ = reply.send(self.stop_idle().await);
            }
            Cmd::Permission { entry_id, option_id } => self.answer_permission(&entry_id, option_id).await,
            // A group lane's (see `Hub::send_cmd`): a room turn that runs finishes; the next one starts afresh.
            Cmd::NewSession if !self.is_main() => self.forget_session(),
            Cmd::NewSession => {
                if self.turn.is_some() {
                    self.stop().await;
                }
                self.forget_session();
                // Retain and summarize completed exchanges even when the user starts afresh. The chat shows no line
                // for it: the user just confirmed it.
                let _ = self.tidier.send(memory::TidyEvent::Flush);
            }
            Cmd::Reconfigure(cfg) => {
                let cfg = *cfg;
                // The harness reads its own memory's switch when it starts: an agent that can resume its session
                // restarts into it, one that can't starts afresh.
                let memory_changed = own_memory::kept(&cfg) != own_memory::kept(&self.cfg);
                let resumes = self.conn.as_ref().is_none_or(|c| c.sessions.load);
                let restart = cfg.backend != self.cfg.backend
                    || cfg.command != self.cfg.command
                    || cfg.model != self.cfg.model
                    || cfg.model_options != self.cfg.model_options
                    || (memory_changed && !resumes);
                self.tools_changed |=
                    cfg.connectors != self.cfg.connectors || cfg.computer != self.cfg.computer || memory_changed;
                // Places with a folder of their own keep working in it; the rest follow the bot's default.
                let default_changed = folder::default_of(&cfg) != folder::default_of(&self.cfg);
                // Live sessions switch before their next turn (see `fast_mode`).
                let fast_changed = cfg.fast_mode != self.cfg.fast_mode;
                // Name, description and skill changes reach the agent as a profile update
                // on the next message (see `context`).
                self.cfg = cfg;
                if fast_changed {
                    self.fast_mode_changed();
                }
                if memory_changed {
                    // Claude Code's notes come or go on the memory screen.
                    memory::screen_changed(&self.hub, &self.cfg.id);
                }
                if restart {
                    self.files.cancel();
                    // A deliberate restart, not a failure: the cut-off turn ends as stopped (as a cancelled ask's does).
                    if self.turn.is_some() {
                        self.stop_requested = true;
                    }
                    if let Some(c) = self.conn.take() {
                        c.acp.kill().await;
                    }
                    for root in self.own_thread_roots() {
                        self.set_session(Some(&root), None);
                    }
                    self.thread_sessions.clear();
                    // Its own chat's or group lane's session; a thread's went with the loop above.
                    if self.session_id.is_some() {
                        self.forget_session();
                        self.notice(
                            "The agent, model, command, folder or memory changed — the agent starts with a fresh context.",
                            NoticeStyle::Divider,
                        );
                    }
                    // A running prompt now fails; finish_turn reports it.
                } else if default_changed {
                    self.reset_inheriting().await;
                }
            }
            Cmd::ResetPlace { lane } => self.reset_place(&lane).await,
            Cmd::ForgetLoaded { lane } => self.forget_loaded(&lane),
            Cmd::Shutdown => {
                // A card's wake queued here never starts: the wake queue gets it back.
                let mut gone: Vec<String> = self.start_entries.clone();
                gone.extend(self.queue.iter().filter_map(|q| match q {
                    Queued::User { entry_id, .. } => Some(entry_id.clone()),
                    _ => None,
                }));
                crate::cards::resume::actor_gone(&self.hub, &gone);
                return false;
            }
        }
        true
    }

    /// Stops this place: its running turn and everything queued here. The bot's other places carry on.
    pub(super) async fn stop(&mut self) {
        self.hub.team.cancel_from(&self.rt);
        // Routines run in the bot's own chat.
        if self.is_main() {
            if let Err(error) = self.hub.routines.cancel_queued(&self.hub.store, &self.cfg.id) {
                tracing::error!(error = format!("{error:#}"), "couldn't cancel queued routines");
            }
            if self.active_routine.is_none() {
                self.hub.routines.release(&self.cfg.id);
            }
        }
        // Queued messages are dropped too: Stop means "stop everything here".
        let mut dropped = Vec::new();
        for queued in std::mem::take(&mut self.queue) {
            let Queued::User { entry_id, .. } = queued else { continue };
            self.set_user_status(&entry_id, RequestStatus::Cancelled);
            dropped.push(entry_id);
        }
        // A card's wake among them is not delivered later: the wake queue is told it is dropped (R8). Also when the
        // router's Stop ended this place, which no `stop_place` saw.
        crate::cards::resume::stopped_lane(&self.hub, &self.home);
        crate::cards::resume::not_started(&self.hub, &dropped, false);
        self.cancel_turn().await;
    }

    /// Where a message of the user's waits in the queue.
    fn queued_at(&self, entry_id: &str) -> Option<usize> {
        self.queue.iter().position(|q| matches!(q, Queued::User { entry_id: id, .. } if id == entry_id))
    }

    /// Takes a message of the user's back out of the queue, if it still waits here. Whether it did.
    fn unsend(&mut self, entry_id: &str) -> bool {
        let Some(p) = self.queued_at(entry_id) else { return false };
        self.queue.remove(p);
        // A wake carried it: the queue is told it is not coming.
        crate::cards::resume::unsent(&self.hub, entry_id);
        self.set_user_status(entry_id, RequestStatus::Cancelled);
        true
    }

    /// A message the user sent while this bot answers them in the same lane: asks the router, in the
    /// background, what it means for the running work. The message waits in the queue meanwhile.
    fn route_later(&self, lane: &Lane, entry_id: &str) {
        let (Some(task), true) = (self.turn_text.clone(), self.turn.is_some() && self.lane == *lane) else {
            return;
        };
        let typed = |id: &str| {
            self.hub.store.entry(id).and_then(|e| e.data["text"].as_str().map(str::to_owned)).unwrap_or_default()
        };
        let recent: Vec<String> = self
            .queue
            .iter()
            .filter_map(|q| match q {
                Queued::User { lane: l, entry_id, .. } if l == lane => Some(typed(entry_id)),
                _ => None,
            })
            .collect();
        let latest = typed(entry_id);
        let (hub, actor, entry_id) = (self.hub.clone(), self.rt.clone(), entry_id.to_owned());
        tokio::spawn(async move {
            let route = router::route(&hub, &task, &recent, &latest).await;
            if route != Route::Merge
                && let Err(error) = hub.send_to_actor(&actor, Cmd::Routed { entry_id, route })
            {
                tracing::warn!(actor, error = format!("{error:#}"), "couldn't apply a routed message");
            }
        });
    }

    /// Acts on the router's answer, if the message still waits (or, for Stop, if its turn runs).
    async fn apply_route(&mut self, entry_id: &str, route: Route) {
        let pos = self.queued_at(entry_id);
        match route {
            Route::Stop => {
                if let Some(p) = pos {
                    // It did its job by stopping; it isn't a message for the agent.
                    self.queue.remove(p);
                    self.set_user_status(entry_id, RequestStatus::Sent);
                } else if !self.turn_entries.iter().any(|id| id == entry_id) {
                    return;
                }
                self.stop().await;
            }
            Route::Interrupt => {
                // Only while the work it corrects still runs; otherwise it waits like any message.
                let (Some(p), Some(task)) = (pos, self.turn_text.clone()) else { return };
                let running_here = matches!(self.queue.get(p), Some(Queued::User { lane, .. }) if *lane == self.lane);
                if self.turn.is_none() || !running_here {
                    return;
                }
                let Some(Queued::User { lane, entry_id, text, .. }) = self.queue.remove(p) else {
                    unreachable!("position found a user message")
                };
                // The same work again, corrected, before anything else that waits.
                let text = router::redo(&task, &text);
                self.queue.push_front(Queued::User { lane, entry_id, text, alone: false });
                self.notice("Stopping to start again with your correction.", NoticeStyle::Info);
                self.cancel_turn().await;
            }
            Route::Separate => {
                if let Some(Queued::User { alone, .. }) = pos.and_then(|p| self.queue.get_mut(p)) {
                    *alone = true;
                }
            }
            Route::Merge => {}
        }
    }

    pub(super) fn set_user_status(&self, entry_id: &str, status: RequestStatus) {
        if let Some(mut e) = self.hub.store.entry(entry_id) {
            e.data["status"] = json!(status);
            self.hub.set_entry(entry_id, &e.data);
        }
    }

    /// Stops the running turn only (queued ones stay).
    pub(super) async fn cancel_turn(&mut self) {
        if self.turn.is_none() {
            return;
        }
        self.stop_requested = true;
        self.cancel_deadline.get_or_insert_with(|| Instant::now() + CANCEL_GRACE);
        self.files.cancel();
        let ids: Vec<String> = self.perms.keys().cloned().collect();
        for id in ids {
            self.answer_permission(&id, None).await;
        }
        if let (Some(c), Some(sid)) = (&self.conn, &self.turn_session) {
            let _ = c.acp.notify("session/cancel", json!({"sessionId": sid})).await;
        }
    }

    pub(super) async fn next_in_queue(&mut self, done_tx: &mpsc::UnboundedSender<Done>) {
        let main = Lane::main(&self.cfg.id);
        while self.turn.is_none() && self.routine_completion.is_none() && !self.queue.is_empty() {
            // Every turn takes a slot from the gate first; without one this resumes when it is granted.
            if !self.have_room() {
                return;
            }
            match self.queue.front() {
                Some(Queued::Routine(_)) => {
                    let Some(Queued::Routine(id)) = self.queue.pop_front() else { unreachable!("front is a routine") };
                    self.active_routine = Some(id.clone());
                    self.routine_timed_out = false;
                    match self.hub.routines.begin(&self.hub, &self.cfg.id, &id) {
                        Ok(Some(prepared)) => {
                            self.routine_deadline = Some(Instant::now() + prepared.timeout);
                            let result = self.start_turn(prepared.lane, &[], &prepared.prompt, false, done_tx).await;
                            if let Err(error) = result {
                                self.start_failed(&error);
                            }
                        }
                        Ok(None) => {
                            self.active_routine = None;
                            self.hub.routines.release(&self.cfg.id);
                        }
                        Err(error) => {
                            self.finish_routine(
                                crate::routines::Status::Failed,
                                Some(format!("Could not start routine: {error:#}")),
                                None,
                            );
                        }
                    }
                    continue;
                }
                Some(Queued::BotRequest(_)) => {
                    let Some(Queued::BotRequest(mut ask)) = self.queue.pop_front() else {
                        unreachable!("front is an ask")
                    };
                    if ask.reply_closed() {
                        continue;
                    }
                    let ids = [ask.entry_id.clone()];
                    let prompt = ask.prompt.clone();
                    ask.mark_started();
                    self.active_request = Some(ask);
                    let result = self.start_turn(main.clone(), &ids, &prompt, false, done_tx).await;
                    if let Err(e) = result {
                        self.start_failed(&e);
                    }
                    continue;
                }
                Some(Queued::Group(_)) => {
                    let Some(Queued::Group(turn)) = self.queue.pop_front() else {
                        unreachable!("front is a group turn")
                    };
                    // A newer message in the room (or Stop) ended the room turn that queued this.
                    if turn.reply.is_closed() || !crate::chat::group::current(&self.hub.store, &turn.lane, turn.epoch) {
                        continue;
                    }
                    let (lane, prompt) = (turn.lane.clone(), turn.prompt.clone());
                    self.active_group = Some(turn);
                    let result = self.start_turn(lane, &[], &prompt, false, done_tx).await;
                    if let Err(e) = result {
                        self.start_failed(&e);
                    }
                    continue;
                }
                Some(Queued::Compact { .. }) => {
                    let Some(Queued::Compact { idle }) = self.queue.pop_front() else {
                        unreachable!("front is a compaction")
                    };
                    self.start_compaction(idle, done_tx).await;
                    continue;
                }
                _ => {}
            }
            // Messages sent while the agent worked go out together as its next turn
            // without crossing a bot request or another lane: each gets its own reply.
            let Some((lane, mut batch)) = take_batch(&mut self.queue) else { continue };
            // A message taken back or stopped while it waited is not part of the turn.
            batch.retain(|(id, _)| {
                let wanted = self
                    .hub
                    .store
                    .entry(id)
                    .is_none_or(|e| e.kind != EntryKind::User.as_str() || e.data["status"] != "cancelled");
                if !wanted {
                    // A wake carried it: the queue is told it is not coming.
                    crate::cards::resume::unsent(&self.hub, id);
                }
                wanted
            });
            if batch.is_empty() {
                continue;
            }
            let ids: Vec<String> = batch.iter().map(|(id, _)| id.clone()).collect();
            let text = batch.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n\n");
            // The owner's messages to an agent over its monthly limit wait under a card (ask, hold) or go on with a
            // line (tell); never refused.
            if self.limit_parks(&lane, &ids).await {
                continue;
            }
            // A handoff waiting in the chat starts a new conversation with this message.
            if lane == main {
                self.handoff = self.take_handoff();
            }
            if let Err(e) = self.start_turn(lane, &ids, &text, false, done_tx).await {
                self.start_failed(&e);
            }
        }
    }

    /// Whether the owner's messages `ids` wait under a limit card instead of running now: only for an owner's kith
    /// agent over its monthly limit with ask or hold (`limits`). Tell writes its line once a month and runs.
    async fn limit_parks(&mut self, lane: &Lane, ids: &[String]) -> bool {
        if !self.is_hq() || self.cfg.person.is_some() || ids.is_empty() {
            return false;
        }
        // Allowed on the card: this run goes on.
        if ids.iter().all(|id| self.hub.store.entry(id).is_some_and(|e| e.data["limitAllowed"] == true)) {
            return false;
        }
        let Some(agent) = self.cfg.hq_agent.clone() else { return false };
        let Some(state) = crate::limits::over(&agent).await else { return false };
        if state["at"] == "tell" {
            if crate::limits::tell_due(&agent, state["month"].as_str().unwrap_or_default()) {
                let text = format!("{}; running anyway.", state["text"].as_str().unwrap_or_default());
                let turn = self.hub.store.max_turn(&lane.chat);
                self.hub.add_entry(lane, EntryKind::Notice, turn, &json!({"text": text, "style": NoticeStyle::Info}));
            }
            return false;
        }
        crate::limits::park(&self.hub, &self.cfg, lane, ids, &state);
        true
    }

    pub(super) fn start_failed(&mut self, e: &anyhow::Error) {
        let stopped = e.is::<super::turn::StartCancelled>();
        if self.turn.is_some() {
            self.report_turn(stopped, (!stopped).then_some(e), "start_failed");
        }
        let status = if stopped { crate::routines::Status::Cancelled } else { crate::routines::Status::Failed };
        self.finish_routine(status, Some(format!("Could not start routine: {e:#}")), None);
        self.complete_request(Err(anyhow!("couldn't start recipient: {e:#}")), false);
        self.complete_group(Err(anyhow!("couldn't start: {e:#}")));
        if stopped {
            self.notice("Stopped.", NoticeStyle::Info);
        } else if let Some(none) = e.downcast_ref::<crate::agent::backups::NoAccount>() {
            self.notice(&none.0, NoticeStyle::Info);
        } else if let Some(refused) = e.downcast_ref::<crate::agent::hq::Refused>() {
            // The kernel said no for this person (not a member any more, or over their budget): it says why.
            self.notice(&format!("Couldn't run this agent for you: {refused}"), NoticeStyle::Error);
            // At their budget: the owner gets a card in this chat to add to it.
            crate::limits::guest_refused(
                self.hub.clone(),
                self.cfg.clone(),
                self.lane.clone(),
                self.turn_entries.clone(),
                &refused.0,
            );
        } else if e.is::<crate::terminal::HeldByWriter>() {
            // `start_turn` already put the notice in the thread. A card's resume that was behind this turn is owed
            // again (the claim was refused, so its marker goes back) and shows who holds the session.
            crate::cards::resume::refused(&self.hub, &self.start_entries, self.turn_session.as_deref());
        } else if !self.invite_sign_in(e) {
            // `{:#}` keeps the reason ("starting `npx …`: EACCES"), not just "agent process exited".
            self.notice(&format!("Couldn't start the agent: {e:#}"), NoticeStyle::Error);
        }
        // Any other start that failed gives its wake back too (M5); one the person stopped isn't offered again now.
        crate::cards::resume::not_started(&self.hub, &self.start_entries, !stopped);
        self.release_turn_claim();
        self.turn = None;
        if self.routine_completion.is_none() {
            self.set_inflight(false);
        }
        // A terminal holding the thread is not an error of this bot: it goes on with its other places.
        let held = e.is::<crate::terminal::HeldByWriter>() || stopped;
        self.hub.set_runtime(&self.rt, |r| {
            r.status = if held { BotStatus::Idle } else { BotStatus::Error };
            r.activity = if held { String::new() } else { "Couldn't start".into() };
            r.started_at = None;
            r.lane = None;
            r.message = None;
            r.session = None;
        });
        self.hub.team.cancel_from(&self.rt);
    }

    /// When `e` asks for signing in: marks the harness signed out and puts a sign-in card in the
    /// chat, which sends this turn's messages again once the user has signed in. Whether it did.
    pub(super) fn invite_sign_in(&self, e: &anyhow::Error) -> bool {
        let backend = &self.cfg.backend;
        let Some(h) = crate::agent::backends::harness(backend).filter(|_| crate::agent::acp::needs_sign_in(e)) else {
            return false;
        };
        crate::agent::backends::set_signed_in(backend, Some(false));
        let invited =
            requests::invite_agent_sign_in(&self.hub, &self.cfg.id, &self.lane, backend, h.name, &self.turn_entries);
        invited
            .inspect_err(
                |error| tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't ask to sign in"),
            )
            .is_ok()
    }

    pub(super) fn complete_request(&mut self, result: Result<String>, cancelled: bool) {
        if let Some(ask) = self.active_request.take() {
            ask.complete(result, cancelled);
        }
    }

    pub(super) fn complete_group(&mut self, result: Result<Option<String>>) {
        if let Some(turn) = self.active_group.take() {
            let _ = turn.reply.send(result);
        }
    }

    pub(super) fn finish_routine(
        &mut self,
        status: crate::routines::Status,
        detail: Option<String>,
        text: Option<String>,
    ) {
        if let Some(id) = self.active_routine.take() {
            self.routine_completion = Some(RoutineCompletion { id, status, detail, text });
            self.routine_deadline = None;
            self.retry_routine_completion();
        }
    }

    /// If SQLite is temporarily unavailable, keep the result and block this
    /// actor's next turn until the durable completion has been saved.
    pub(super) fn retry_routine_completion(&mut self) -> bool {
        let Some(completion) = &self.routine_completion else {
            return true;
        };
        match self.hub.routines.complete(
            &self.hub,
            &completion.id,
            completion.status,
            completion.detail.clone(),
            completion.text.clone(),
        ) {
            Ok(()) => {
                self.routine_completion = None;
                true
            }
            Err(error) => {
                tracing::error!(
                    error = format!("{error:#}"),
                    run = completion.id,
                    "couldn't save routine result; will retry"
                );
                false
            }
        }
    }

    /// Grok Bot's upgrade resume: the previous host process stopped mid-turn, so
    /// the agent is told, in the same session, to finish without redoing steps.
    pub(super) async fn resume_interrupted(&mut self, lane: Lane, done_tx: &mpsc::UnboundedSender<Done>) {
        self.set_inflight(false);
        if self.session_for(lane.thread.as_deref()).is_none() {
            self.finish_routine(
                crate::routines::Status::Interrupted,
                Some("No saved agent session is available; inspect this run before retrying".into()),
                None,
            );
            return;
        }
        self.await_room().await;
        let prompt = if self.active_routine.is_some() {
            format!(
                "{RESUME_PROMPT} This is still a scheduled routine run. If its instruction permits silence and there is nothing to report, return exactly (pass)."
            )
        } else {
            RESUME_PROMPT.to_owned()
        };
        if let Err(e) = self.start_turn(lane, &[], &prompt, true, done_tx).await {
            self.start_failed(&e);
        }
    }

    /// Marks a turn in the bot's own chat or a thread as running across host restarts
    /// (see [`Self::resume_interrupted`]); the thread root rides along.
    pub(super) fn set_inflight(&self, on: bool) {
        let value = match (&self.lane.thread, on) {
            (_, false) => String::new(),
            (Some(root), true) => format!("{}\t{root}", now_ms()),
            (None, true) => now_ms().to_string(),
        };
        if let Err(error) = self.hub.store.kv_set(&inflight_key(&self.rt), &value) {
            tracing::warn!(bot = %self.cfg.id, error = format!("{error:#}"), "couldn't record the running turn");
        }
    }
}

/// The messages at the front of the queue that go out together as the next turn, with their lane: the same lane, up to
/// and including one on a different subject (which goes alone). None when the front is not a user message.
fn take_batch(queue: &mut std::collections::VecDeque<Queued>) -> Option<(Lane, Vec<(String, String)>)> {
    let Some(Queued::User { lane, .. }) = queue.front() else { return None };
    let lane = lane.clone();
    let mut batch = Vec::new();
    while let Some(Queued::User { lane: l, alone, .. }) = queue.front()
        && *l == lane
        && (batch.is_empty() || !*alone)
    {
        let alone = *alone;
        if let Some(Queued::User { entry_id, text, .. }) = queue.pop_front() {
            batch.push((entry_id, text));
        }
        if alone {
            break;
        }
    }
    Some((lane, batch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn user(lane: &Lane, id: &str, alone: bool) -> Queued {
        Queued::User { lane: lane.clone(), entry_id: id.into(), text: format!("t-{id}"), alone }
    }

    #[test]
    fn a_wake_batch_queued_together_starts_as_one_turn() {
        let lane = Lane::in_thread("bot", "root");
        // What `Cmd::WakeBatch` pushes: a card's notice and two messages that waited for a terminal.
        let mut queue: VecDeque<Queued> = ["notice", "m1", "m2"].iter().map(|id| user(&lane, id, false)).collect();
        queue.push_back(user(&Lane::main("bot"), "other", false));
        let (got, batch) = take_batch(&mut queue).unwrap();
        assert_eq!(got, lane);
        assert_eq!(batch.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["notice", "m1", "m2"]);
        assert_eq!(queue.len(), 1, "another lane's message waits for its own turn");
    }

    #[test]
    fn a_message_on_another_subject_goes_alone() {
        let lane = Lane::main("bot");
        let mut queue: VecDeque<Queued> =
            VecDeque::from([user(&lane, "a", false), user(&lane, "b", true), user(&lane, "c", false)]);
        assert_eq!(take_batch(&mut queue).unwrap().1.len(), 1, "b is alone, so a stops before it");
        assert_eq!(take_batch(&mut queue).unwrap().1.len(), 1);
        assert_eq!(take_batch(&mut queue).unwrap().1.len(), 1);
        assert!(take_batch(&mut queue).is_none());
    }
}
