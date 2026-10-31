//! A turn whose account ran out of usage goes on with the next account of the agent's backup chain (see
//! `agent::backups`). Only an hq agent has a chain; a turn is never moved while it runs, only started again from the
//! top when it ended on a usage limit, and at most once per entry.

use super::{Actor, Done, Seg};
use crate::agent::acp::{self, Depleted};
use crate::agent::backups::{MAX_MOVES, Pick, RATE_LIMIT};
use crate::store::{Entry, EntryKind};
use tokio::sync::mpsc;

/// How many of the latest messages a backup's new session is told.
const PREFACE_MESSAGES: usize = 6;
const PREFACE_QUOTE: usize = 800;

impl Actor {
    /// Whether `done` is the end of a turn that ran out of usage, and when the limit lifts if the agent said. From the
    /// adapter's error category or its `rejected` rate-limit update, never from words in a message.
    fn depletion(&self, done: &Done) -> Option<Depleted> {
        let seen = self.limit_seen.as_ref();
        match done {
            Err(e) => acp::depleted(e)
                .map(|d| Depleted { resets_at: d.resets_at.or(seen.and_then(|s| s.resets_at)) })
                .or(seen.copied()),
            // A rejected limit that still ended the turn with an answer is not one that stopped it.
            Ok(_) if self.last_text.is_none() && self.sent.is_empty() => seen.copied(),
            Ok(_) => None,
        }
    }

    /// Takes a turn that ended out of usage to the next entry. Whether it did (the turn then runs on, and the caller
    /// leaves it alone); otherwise the turn finishes as it always did.
    pub(super) async fn failover(&mut self, done: &Done, done_tx: &mpsc::UnboundedSender<Done>) -> bool {
        if !self.is_hq() || self.stop_requested || self.compacting.is_some() || self.turn.is_none() {
            return false;
        }
        let Some(Depleted { resets_at }) = self.depletion(done) else { return false };
        let Some(agent) = self.cfg.hq_agent.clone() else { return false };
        // No chain, no change: the turn fails with the agent's own error, as it does without backups.
        let chain = match self.hub.backups.known_chain(&agent) {
            Some(chain) => chain,
            // The chain is only fetched on this path when nothing was out before: the turn that just failed is the
            // first to need it.
            None => super::hq::fetch_chain(&self.hub, &agent).await,
        };
        if chain.len() < 2 {
            return false;
        }
        let failed = self.serving.clone().unwrap_or_else(|| chain[0].id.clone());
        self.hub.backups.mark_out(&failed, resets_at, RATE_LIMIT);
        self.turn_tried.insert(failed.clone());
        // What the attempt already did can't be done twice: the entry is out for the next turn, this one ends here.
        if self.attempt_effects || !self.sent.is_empty() {
            tracing::info!(bot = %self.cfg.id, entry = %failed, "turn ran out of usage after acting; not repeating it");
            return false;
        }
        let Some((text, hidden)) = self.turn_prompt.clone() else { return false };
        // A turn that resumes after a restart has nothing of its own to send again.
        if hidden || self.turn_tried.len() > MAX_MOVES {
            return false;
        }
        match self.hub.backups.pick(&agent, &chain, &self.turn_tried) {
            Pick::AllOut(outs) => {
                self.exhausted = Some(self.hub.backups.all_out_text(&outs));
                return false;
            }
            Pick::Primary | Pick::Backup(_) => {}
        }
        tracing::info!(bot = %self.cfg.id, entry = %failed, "turn ran out of usage; moving to the next account");
        self.drop_attempt();
        self.reuse_prompt = true;
        self.limit_seen = None;
        self.attempt_effects = false;
        self.hub.set_runtime(&self.rt, |r| r.activity = "Switching account…".into());
        let lane = self.lane.clone();
        if let Err(e) = self.begin_prompt(&lane, &text, false, done_tx).await {
            self.start_failed(&e);
        }
        true
    }

    /// What the failed attempt left open: its text, its tool calls and its questions. The entries stay in the chat as
    /// the account wrote them.
    fn drop_attempt(&mut self) {
        self.files.cancel();
        self.flush(true);
        self.seg = Seg::None;
        self.parked_prompts.clear();
        for (id, _) in self.perms.drain().collect::<Vec<_>>() {
            crate::cards::withdraw_entry(&self.hub, &id);
            if let Some(mut e) = self.hub.store.entry(&id) {
                e.data["status"] = "expired".into();
                self.hub.set_entry(&id, &e.data);
            }
        }
        for entry_id in std::mem::take(&mut self.tools).into_values() {
            if let Some(mut e) = self.hub.store.entry(&entry_id)
                && matches!(e.data["status"].as_str(), Some("pending" | "in_progress"))
            {
                e.data["status"] = "failed".into();
                self.hub.set_entry(&entry_id, &e.data);
            }
        }
        self.end_plan_card();
        self.plan_entry = None;
        self.plan_card = None;
        self.last_text = None;
        self.exit_tail = None;
    }

    /// The messages of this lane worth telling an account that has not seen them, oldest first, as quoted lines: the
    /// user's and the agent's final replies, leaving out the ones this turn is answering.
    fn lane_lines(&self, entries: impl Fn(&[Entry]) -> Vec<Entry>) -> Vec<String> {
        let all = self.hub.store.messages_after(&self.lane, 0, 40).unwrap_or_default();
        entries(&all)
            .iter()
            .filter(|e| !self.turn_entries.contains(&e.id))
            .map(|e| {
                let who = if e.kind == EntryKind::User.as_str() { "the user" } else { "you" };
                format!("{who}: \"{}\"", acp::truncate(e.data["text"].as_str().unwrap_or_default(), PREFACE_QUOTE))
            })
            .collect()
    }

    /// The latest messages of the lane, for a new session that knows nothing of them, after `why`.
    fn latest_preface(&self, why: &str) -> Option<String> {
        let mut lines = self.lane_lines(<[Entry]>::to_vec);
        let skip = lines.len().saturating_sub(PREFACE_MESSAGES);
        let lines = lines.split_off(skip);
        (!lines.is_empty()).then(|| {
            format!(
                "[Kith] {why} The latest messages, oldest first:\n{}\nContinue from the user's message below.",
                lines.join("\n")
            )
        })
    }

    /// A backup's new session knows nothing of the conversation: the latest messages go ahead of the user's.
    pub(super) fn backup_preface(&self) -> Option<String> {
        self.latest_preface("You are picking this conversation up on a backup account: the usual one is out of usage.")
    }

    /// The agent's main account changed, and a session is only loaded under the account that made it: the new one is
    /// told the latest messages, as a backup's is.
    pub(super) fn account_preface(&self) -> Option<String> {
        self.latest_preface(
            "You are picking this conversation up on another account: the agent's main account changed, and its \
             earlier session belongs to the old one.",
        )
    }

    /// The agent's own account, back after its limit, loads its old session, which missed what the backup answered:
    /// those messages, if there are any.
    pub(super) fn recovery_preface(&self) -> Option<String> {
        let answered_by_backup = |e: &Entry| e.kind == EntryKind::Agent.as_str() && !e.data["answeredBy"].is_null();
        let lines = self.lane_lines(|all| {
            // Everything after the agent's own last reply.
            let own_last = all.iter().rposition(|e| {
                e.kind == EntryKind::Agent.as_str() && e.data["answeredBy"].is_null() && e.data["final"] == true
            });
            let after = own_last.map_or(all, |i| &all[i + 1..]);
            if after.iter().any(answered_by_backup) { after.to_vec() } else { Vec::new() }
        });
        (!lines.is_empty()).then(|| {
            format!(
                "[Kith] While your account was out of usage, a backup account answered here. The messages since your last \
                 reply, oldest first:\n{}\nContinue from the user's message below.",
                lines.join("\n")
            )
        })
    }
}
