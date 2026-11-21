//! What the agent reports about its context and its own compactions. An agent's context is its own: Claude Code
//! compacts it, and the host drafts no handoff; the host only asks for Kith's own summary of the thread (tier 2 of
//! recall). The rules are `kith_mem::compaction`.

use super::{Actor, Done};
use kith_mem::summary::Reason as SummaryReason;

/// Context use below which a fall isn't read as a compaction: a short session has little to summarize.
const COMPACTED_FROM: u64 = 20_000;

impl Actor {
    /// The agent reported its context use.
    pub(super) fn track_usage(&mut self, used: u64, size: u64) {
        let Some(sid) = self.turn_session.clone() else { return };
        let Some(conn) = self.conn.as_mut() else { return };
        let before = conn.usage.insert(sid, (used, size)).map(|(used, _)| used);
        if !conn.reports_compaction && compacted_by_usage(before, used) {
            self.compacted();
        }
    }

    /// The agent summarized its context: the thread gets Kith's own summary, whoever's harness compacted. A compaction
    /// reported twice finds the summary already covering.
    pub(super) fn compacted(&mut self) {
        crate::hqmem::summary::request(&self.hub, &self.cfg.id, self.lane.thread.as_deref(), SummaryReason::Compaction);
    }

    /// After a turn: the prompt cache's use, the first thing to look at when a turn costs more than it should.
    pub(super) fn after_turn(&mut self, done: &Done) {
        if let Ok(answer) = done
            && answer["usage"].is_object()
        {
            let usage = &answer["usage"];
            tracing::info!(
                bot = %self.cfg.id,
                session = self.turn_session.as_deref().unwrap_or_default(),
                cached_read = usage["cachedReadTokens"].as_u64().unwrap_or(0),
                cached_write = usage["cachedWriteTokens"].as_u64().unwrap_or(0),
                input = usage["inputTokens"].as_u64().unwrap_or(0),
                "turn usage"
            );
        }
    }
}

/// Whether a session's context use fell the way it does when the agent summarizes it: by more than half.
fn compacted_by_usage(before: Option<u64>, used: u64) -> bool {
    before.is_some_and(|before| before >= COMPACTED_FROM && used < before / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_halved_context_reads_as_a_compaction() {
        assert!(compacted_by_usage(Some(90_000), 12_000));
        assert!(!compacted_by_usage(Some(90_000), 60_000), "ordinary churn");
        assert!(!compacted_by_usage(Some(8_000), 1_000), "too little to summarize");
        assert!(!compacted_by_usage(None, 1_000), "the session's first report");
    }
}
