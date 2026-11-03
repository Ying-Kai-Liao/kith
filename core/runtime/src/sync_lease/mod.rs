//! The cloud sync lease in the core (M2-10, lane D): whether this Mac is in sync with the Kith cloud.
//!
//! Ported from the host (`serve/host/src/remote/sync_lease.rs`, which still decides until lane H switches its callers),
//! over core.db instead of `hub.cloud_synced` and `<data dir>/cloud-sync.json`.
//!
//! What the lease is:
//! - **Who holds it.** This Mac, as one row. There is no second holder to contend with: the cloud is the other side, and
//!   it never holds anything here. "Held" means: the relay connection is live and a pull of the cloud's state succeeded
//!   on it, recently enough.
//! - **Acquire.** [`pulled`] with the [`mark`] taken before the pull read the cloud's state. It acquires only if the
//!   cloud has not said "changed" since that mark ([`changed`] bumps the count), so a pull that raced a change cannot
//!   end the wait. The lease then runs for [`LEASE_MS`] from the call.
//! - **Renew.** [`renew`], when the relay is heard (a ping). The host renews whenever its in-memory flag is up; the
//!   core renews only a lease that is still held: one that ran out (the Mac slept, the network dropped) is gone until
//!   a pull acquires it again, so a missed ping can never be papered over by a late one.
//! - **Lose.** At once on [`changed`] (until the pull that applies the change), on [`drop_lease`] (the connection
//!   dropped or is being made; any of these also turns an `off` cloud back on, since the cloud just spoke), on [`unlink`], and by itself when `until` passes.
//! - **Clocks and timeouts.** Time is milliseconds from an injected [`Clock`] (wall clock in production). `LEASE_MS` is
//!   120 s: past the relay's silence limit plus one ping, so a live connection renews in time and a dead one runs out.
//!   The row also keeps the latest reading any change saw (`at`). A clock that steps back below it
//!   cannot be trusted: [`pulled`] and [`renew`] refuse, and the lease reads as not held until the clock catches up. A step
//!   back that stays at or above it is invisible and, as on the host, can make a lease last that much longer.
//! - **On loss.** Nothing in the core reacts. What the host does on loss (account devices and bot asks wait for sync,
//!   the chat channels hold everyone but the owner) stays in the host until lane H.
//! - **What a non-owner's `may()` needs.** [`lease_held_for_non_owner`]: true when the cloud is `off` on purpose
//!   (nothing waits on it, as `linked: false` in the host's file) or when it is linked and the lease is held. Fail
//!   closed: a Mac that was never told about the cloud, an expired lease, a clock behind its last reading, or a lease
//!   that cannot be read all answer false. The owner never needs the lease. The claim path (M3, runtime-design.md
//!   section 9) calls it inside its own claim transaction through [`held_in`]; nothing does yet.
//!
//! What differs from the host, on purpose: a restart. The host's flag starts down; the rows here survive a daemon
//! restart, but the relay connection did not, so [`restart`] ends the lease. Every change is one `BEGIN IMMEDIATE`
//! transaction through [`Core::claim_tx`], so two threads of the daemon never see half a change.

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::db::Core;
use crate::error::{Error, Result};

/// How long one pull, or one ping heard while held, keeps the lease: the host's `LEASE`.
pub(crate) const LEASE_MS: i64 = 120_000;
/// The longest a caller may ask for. A lease is minutes; an hour is already far past any sane relay silence limit.
const MAX_LEASE_MS: i64 = 3_600_000;

/// The time source. A parameter so tests cover expiry and a clock stepping back without sleeping.
pub(crate) trait Clock {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// The real wall clock. An unreadable clock reads as 0, which is before every reading and so fails closed.
pub(crate) struct System;

impl Clock for System {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cloud {
    Unknown,
    Linked,
    Off,
}

impl Cloud {
    fn text(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Linked => "linked",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Row {
    cloud: Cloud,
    synced: bool,
    until: i64,
    changes: i64,
    at: i64,
}

const EMPTY: Row = Row {
    cloud: Cloud::Unknown,
    synced: false,
    until: 0,
    changes: 0,
    at: 0,
};

fn load(tx: &Transaction<'_>) -> Result<Row> {
    let found = tx
        .query_row(
            "SELECT cloud, synced, until, changes, at FROM sync_lease WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((cloud, synced, until, changes, at)) = found else {
        return Ok(EMPTY);
    };
    let cloud = match cloud.as_str() {
        "linked" => Cloud::Linked,
        "off" => Cloud::Off,
        _ => Cloud::Unknown,
    };
    Ok(Row {
        cloud,
        synced: synced == 1,
        until,
        changes,
        at,
    })
}

fn store(tx: &Transaction<'_>, row: &Row) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO sync_lease (id, cloud, synced, until, changes, at) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![row.cloud.text(), i64::from(row.synced), row.until, row.changes, row.at],
    )?;
    Ok(())
}

/// Held at `now` by this row. The one rule every reader shares.
fn held_row(row: &Row, now: i64) -> bool {
    match row.cloud {
        Cloud::Unknown => false,
        Cloud::Off => true,
        // `now < at`: the clock stepped back, so how long ago the pull was cannot be told.
        Cloud::Linked => row.synced && now >= row.at && now < row.until,
    }
}

/// Whether the lease is held at `now`, read inside a transaction the caller already holds (the claim transaction).
pub(crate) fn held_in(tx: &Transaction<'_>, now: i64) -> Result<bool> {
    Ok(held_row(&load(tx)?, now))
}

/// What a non-owner's claim needs: the lease is held, or the cloud is off on purpose. Fail closed: anything not known
/// to be held is not held, including a lease that cannot be read.
pub(crate) fn lease_held_for_non_owner(core: &mut Core, clock: &dyn Clock) -> bool {
    let now = clock.now_ms();
    core.claim_tx()
        .and_then(|tx| held_in(&tx, now))
        .unwrap_or(false)
}

fn check_ms(ms: i64) -> Result<i64> {
    if (1..=MAX_LEASE_MS).contains(&ms) {
        Ok(ms)
    } else {
        Err(Error::BadParams(format!(
            "kith-core: sync_lease: ms is 1 to {MAX_LEASE_MS}"
        )))
    }
}

/// Runs one state change as a claim transaction and writes the row it returns.
fn change<T>(core: &mut Core, clock: &dyn Clock, f: impl FnOnce(&mut Row, i64) -> T) -> Result<T> {
    let now = clock.now_ms();
    let tx = core.claim_tx()?;
    let mut row = load(&tx)?;
    let out = f(&mut row, now);
    row.at = row.at.max(now);
    store(&tx, &row)?;
    tx.commit()?;
    Ok(out)
}

/// The cloud is on (`linked`): from now on the lease decides. Not synced until a pull says so.
pub(crate) fn link(core: &mut Core, clock: &dyn Clock) -> Result<()> {
    change(core, clock, |row, _| {
        if row.cloud != Cloud::Linked {
            *row = Row {
                cloud: Cloud::Linked,
                synced: false,
                until: 0,
                ..*row
            };
        }
    })
}

/// The cloud is off on purpose: nothing waits on it, and an old lease is gone.
pub(crate) fn unlink(core: &mut Core, clock: &dyn Clock) -> Result<()> {
    change(core, clock, |row, _| {
        *row = Row {
            cloud: Cloud::Off,
            synced: false,
            until: 0,
            ..*row
        };
    })
}

/// Where the cloud's changes stand; taken before a pull reads the state.
pub(crate) fn mark(core: &mut Core) -> Result<i64> {
    let tx = core.claim_tx()?;
    Ok(load(&tx)?.changes)
}

/// The cloud said its state changed: out of sync until a pull that starts after this one succeeds. Answers the new mark.
pub(crate) fn changed(core: &mut Core, clock: &dyn Clock) -> Result<i64> {
    change(core, clock, |row, _| {
        row.changes = row.changes.saturating_add(1);
        saw_cloud(row);
        row.changes
    })
}

/// The connection dropped or is being made: out of sync. The cloud's change count stays. A cloud that was off is on.
pub(crate) fn drop_lease(core: &mut Core, clock: &dyn Clock) -> Result<()> {
    change(core, clock, |row, _| saw_cloud(row))
}

/// Any word from the cloud means it is on, whatever this Mac was told (unknown stays unknown): out of sync, and no longer "off" (an
/// off that kept reading as held would pass the stale view a revoke must not pass).
fn saw_cloud(row: &mut Row) {
    if row.cloud == Cloud::Off {
        row.cloud = Cloud::Linked;
    }
    row.synced = false;
    row.until = 0;
}

/// A pull that started at `mark` succeeded. True when the lease is now held for `ms`; false when it is not: the cloud
/// changed since the mark, the cloud was off (a pull turns it on, unsynced), or the clock is behind its last reading.
pub(crate) fn pulled(core: &mut Core, clock: &dyn Clock, mark: i64, ms: i64) -> Result<bool> {
    let ms = check_ms(ms)?;
    change(core, clock, |row, now| {
        if row.cloud == Cloud::Off {
            // A pull is the cloud answering: it is on. Not held on this call; the next pull acquires.
            saw_cloud(row);
            return false;
        }
        if row.cloud != Cloud::Linked || row.changes != mark || now < row.at {
            return false;
        }
        row.synced = true;
        row.until = now.saturating_add(ms);
        true
    })
}

/// The relay was heard: runs a held lease out `ms` from now. True when renewed; false when there was nothing held to
/// renew (never acquired, expired, lost, or the clock is behind its last reading). It never acquires.
pub(crate) fn renew(core: &mut Core, clock: &dyn Clock, ms: i64) -> Result<bool> {
    let ms = check_ms(ms)?;
    change(core, clock, |row, now| {
        if row.cloud != Cloud::Linked || !held_row(row, now) {
            return false;
        }
        row.until = row.until.max(now.saturating_add(ms));
        true
    })
}

/// A daemon (re)start: the relay connection of the last run is gone, so a lease held then is not held now. The cloud
/// setting and the change count stay.
pub(crate) fn restart(core: &mut Core, clock: &dyn Clock) -> Result<()> {
    change(core, clock, |row, _| {
        row.synced = false;
        row.until = 0;
    })
}

/// The lease as the rows show it.
pub(crate) fn status(core: &mut Core, clock: &dyn Clock) -> Result<Value> {
    let now = clock.now_ms();
    let tx = core.claim_tx()?;
    let row = load(&tx)?;
    Ok(json!({
        "cloud": row.cloud.text(),
        "synced": row.synced,
        "held": held_row(&row, now),
        "until": row.until,
        "mark": row.changes,
    }))
}

fn ms_param(params: &Value) -> Result<i64> {
    match params.get("ms") {
        None => Ok(LEASE_MS),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| Error::BadParams("kith-core: sync_lease: ms is a whole number".into()))
            .and_then(check_ms),
    }
}

/// The `syncLease.*` rows of the table. The params were checked against the row already.
pub(crate) fn call(
    core: &mut Core,
    method: &str,
    params: &Value,
    clock: &dyn Clock,
) -> Result<Value> {
    match method {
        "syncLease.status" => status(core, clock),
        "syncLease.link" => link(core, clock).map(|()| json!({"cloud": "linked"})),
        "syncLease.unlink" => unlink(core, clock).map(|()| json!({"cloud": "off"})),
        "syncLease.mark" => Ok(json!({"mark": mark(core)?})),
        "syncLease.changed" => Ok(json!({"mark": changed(core, clock)?})),
        "syncLease.drop" => drop_lease(core, clock).map(|()| json!({"held": false})),
        "syncLease.pulled" => {
            let m = params
                .get("mark")
                .and_then(Value::as_i64)
                .filter(|m| *m >= 0)
                .ok_or_else(|| {
                    Error::BadParams(
                        "kith-core: syncLease.pulled needs mark, taken before the pull".into(),
                    )
                })?;
            Ok(json!({"held": pulled(core, clock, m, ms_param(params)?)?}))
        }
        "syncLease.renew" => Ok(json!({"renewed": renew(core, clock, ms_param(params)?)?})),
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

#[cfg(test)]
mod tests;
