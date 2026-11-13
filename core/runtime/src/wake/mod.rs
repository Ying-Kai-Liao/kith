//! The wake queue and the waiters in core.db (M3-7; docs/plans/one-bot-model.md M3, runtime-design section 2). A port of
//! `kernel/bin/kith-wake`, which stays the writer of the queue files until the switch (M3-10): nothing here runs on its
//! own, the daemon runs no drain and fires no waiter, and `import/wake.rs` is how its files get in.
//!
//! - `model.rs`: the queue as values, and the Python's file formats.
//! - `queue.rs`: enqueue, merge, dedupe (rule 13), withdraw, take, done, back, the `[kith wake RUN]` recovery rule, status,
//!   ls: the rules W2 (a request leaves the queue only at turn commit) and W3 (a turn starts only on a session nobody holds).
//! - `waiters.rs`: `ci:`, `pid:` and `at:` waiters and their evaluation.
//! - `outside.rs`: what they ask of the world (the clock, the process table, `gh`, the transcripts), as traits.
//! - `store.rs`: the tables; `call.rs`: the `wake.*` rows.
//! - `turns.rs`: a run's turn in the conversations lane, written by `take` in the same transaction (M3-10c).
//!
//! One call is one claim transaction over the whole state (a few rows), which replaces the Python's three locks (the ask
//! box's, `waits/.lock`, `queue/.lock`). `take` marks what is owed as carried, returns the
//! turn as `kith-wake take` prints it, and queues the run's turn through the claims API (parked as `terminal_live` when a
//! terminal holds the session). Nothing starts that turn before the switch.

mod call;
mod model;
mod outside;
#[cfg(all(test, unix))]
mod parity_tests;
mod queue;
mod store;
#[cfg(test)]
mod tests;
mod turns;
mod waiters;

pub(crate) use call::{call, serves};
// The import (Unix only, like the rest of `import`) is the one outside user of the model and the counts.
pub(crate) use model::State;
#[cfg(unix)]
pub(crate) use model::{Answered, Item, Waiter};
#[cfg(unix)]
pub(crate) use store::counts;
pub(crate) use store::{load, save};

use crate::db::Core;
use crate::error::{Error, Result};

/// Runs `f` over the state in one write transaction. Answered ids older than a week are forgotten on the way.
pub(crate) fn write<T>(
    core: &mut Core,
    now: f64,
    f: impl FnOnce(&mut State) -> Result<T>,
) -> Result<T> {
    write_tx(core, now, |_, st| f(st))
}

/// [`write`] for a step that also writes the conversations lane (`take` makes the run's turn): `f` gets the transaction.
/// Whatever runs left the queue in the step has its waiting turn closed in the same transaction.
pub(crate) fn write_tx<T>(
    core: &mut Core,
    now: f64,
    f: impl FnOnce(&rusqlite::Transaction<'_>, &mut State) -> Result<T>,
) -> Result<T> {
    let tx = core.claim_tx()?;
    let before = load(&tx)?;
    let mut after = before.clone();
    after.prune(now);
    let said = f(&tx, &mut after)?;
    turns::close_gone_in(&tx, &before, &after, now)?;
    save(&tx, &before, &after)?;
    tx.commit()?;
    Ok(said)
}

/// Reads the state in a plain (deferred) read transaction: a consistent snapshot that takes no write lock, so `ls` and
/// `status` do not queue behind writers. A read changes nothing, so it needs no generation check.
pub(crate) fn read<T>(core: &mut Core, now: f64, f: impl FnOnce(&State) -> T) -> Result<T> {
    let tx = core.conn().unchecked_transaction()?;
    let mut st = load(&tx)?;
    st.prune(now);
    Ok(f(&st))
}

/// How often [`write_after`] starts over when the rows changed between its read and its write.
const CAS_TRIES: usize = 5;

/// Evaluates outside the write lock, then writes only if nothing changed in between. `eval` gets a copy of the state read
/// in a plain transaction and may do slow things (`gh`, `ps`, `kill`); the result is written in one claim transaction
/// that first checks the rows are still what `eval` saw (compare and swap). When they moved, `eval` runs again on the new
/// state; after [`CAS_TRIES`] tries the call fails rather than hold the write lock over a slow thing.
pub(crate) fn write_after<T>(
    core: &mut Core,
    now: f64,
    mut eval: impl FnMut(&mut State) -> T,
) -> Result<T> {
    for _ in 0..CAS_TRIES {
        let seen = {
            let tx = core.conn().unchecked_transaction()?;
            load(&tx)?
        };
        let mut after = seen.clone();
        after.prune(now);
        let said = eval(&mut after);
        let tx = core.claim_tx()?;
        let current = load(&tx)?;
        if current != seen {
            continue;
        }
        turns::close_gone_in(&tx, &current, &after, now)?;
        save(&tx, &current, &after)?;
        tx.commit()?;
        return Ok(said);
    }
    Err(Error::Refused(
        "wake: the queue kept changing under the evaluation".into(),
    ))
}

/// What a stop owes the callers (`kith ask --end`): the requests and waiters of each session are withdrawn, and then of
/// everything the persons are owed, inside the transaction the caller holds (the stop's, so the records, the claims and the
/// wakes change in one step). Answers how many went. Withdrawing nothing is fine.
pub(crate) fn withdraw_owed_in(
    tx: &rusqlite::Connection,
    now_ms: i64,
    sessions: &[String],
    persons: &[String],
) -> Result<usize> {
    #[allow(clippy::cast_precision_loss)]
    let now = now_ms as f64 / 1000.0;
    let before = load(tx)?;
    let mut after = before.clone();
    after.prune(now);
    let mut n: usize = sessions
        .iter()
        .map(|s| queue::withdraw(&mut after, now, s, None))
        .sum();
    if !persons.is_empty() {
        n += queue::withdraw_person(&mut after, now, persons);
    }
    turns::close_gone_in(tx, &before, &after, now)?;
    save(tx, &before, &after)?;
    Ok(n)
}
