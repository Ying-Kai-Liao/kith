//! The detached runs: a turn whose engine runs as a process of its own, started by the host and outliving it (a cron job's
//! `kith-sched run-turn`, a wake's run). Both go through the same two claim steps (`sched_run::claim_run`, then
//! `sched_run::attach`) and both end by the same rule (design 2026-11-21-wake-s4, R3 and R3a).
//!
//! **R3a: a report never ends a detached run's claim.** The host tells the core `done` or `exit` when it sees its launcher
//! (the process it started) exit. The launcher may be all that died: its engine runs on in the same process group and
//! keeps writing the conversation. So for an `external` claim that has a detached turn, [`holds_after_report`] is true:
//! `apply` sets the claim `ending` (with the report as `end_reason`) and leaves the turn `running`, and only `end_gone` ends
//! it, once the recorded process AND its group are gone (`recorded_gone`). It then sets the turn `done` when the report was
//! `done`, else `failed` ([`END_STATE`]). Until then the conversation stays held, so neither a terminal nor the next wake
//! can take it (model counterexample `broken-ReportToEnding`).

use rusqlite::Transaction;

use crate::error::Result;

/// The sources of a turn the host runs as a detached process. A new kind of detached run (the ask) is added here.
pub(crate) const SOURCES: &[&str] = &["schedule", "wake"];

/// Whether a turn of `source` is a detached run's.
pub(crate) fn is_source(source: &str) -> bool {
    SOURCES.contains(&source)
}

/// [`SOURCES`] as an SQL list, `('schedule', 'wake')`.
pub(crate) fn sources_sql() -> String {
    quoted(SOURCES)
}

/// `names` as an SQL list. Only names [`is_source`] accepts are put in (the rest are dropped), so no caller's text reaches the
/// statement.
pub(crate) fn quoted(names: &[&str]) -> String {
    let quoted: Vec<String> = names
        .iter()
        .filter(|s| is_source(s))
        .map(|s| format!("'{s}'"))
        .collect();
    format!("({})", quoted.join(", "))
}

/// Whether the claim is an `external` one that runs a detached turn: a report on it must not end it (R3a).
pub(super) fn holds_after_report(tx: &Transaction<'_>, claim_id: i64) -> Result<bool> {
    Ok(tx.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM claims c JOIN turns t ON t.claim_id = c.claim_id
                            WHERE c.claim_id = ?1 AND c.kind = 'external' AND t.source IN {})",
            sources_sql()
        ),
        [claim_id],
        |r| r.get(0),
    )?)
}

/// The SQL that gives the turn of an `external` claim `?1` its state once `end_gone` has ended the claim: `done` when the
/// run said `done`, else `failed` (its prompt went out, so it is never put back).
pub(super) const END_STATE: &str = "CASE WHEN (SELECT end_reason FROM claims WHERE claim_id = ?1) = 'done' THEN 'done' ELSE 'failed' END";
