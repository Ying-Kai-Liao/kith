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
//!
//! One call is one claim transaction over the whole state (a few rows), which replaces the Python's three locks (the ask
//! box's, `waits/.lock`, `queue/.lock`). A turn is not started here: `take` only marks what is owed as carried and returns
//! the turn as `kith-wake take` prints it; creating the turn in the conversations lane is the switch's step.

mod call;
mod model;
mod outside;
#[cfg(all(test, unix))]
mod parity_tests;
mod queue;
mod store;
#[cfg(test)]
mod tests;
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
use crate::error::Result;

/// Runs `f` over the state in one write transaction. Answered ids older than a week are forgotten on the way.
pub(crate) fn write<T>(
    core: &mut Core,
    now: f64,
    f: impl FnOnce(&mut State) -> Result<T>,
) -> Result<T> {
    let tx = core.claim_tx()?;
    let before = load(&tx)?;
    let mut after = before.clone();
    after.prune(now);
    let said = f(&mut after)?;
    save(&tx, &before, &after)?;
    tx.commit()?;
    Ok(said)
}

/// Reads the state under the same door as a write (the generation must still be this core's) and changes nothing.
pub(crate) fn read<T>(core: &mut Core, now: f64, f: impl FnOnce(&State) -> T) -> Result<T> {
    let tx = core.claim_tx()?;
    let mut st = load(&tx)?;
    st.prune(now);
    Ok(f(&st))
}
