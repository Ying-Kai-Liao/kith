//! The marker `step` leaves in core.db (a row of the `meta` table, key `cutover:<module>`): "this core holds the import the
//! switch file `at` stands for". It is what makes a forged or stale switch file only able to deny: `may_serve` serves a
//! switched module only when the file's `at` equals the marker's, so a `.switch` written by hand, or one left over after
//! core.db was restored from before the step, finds no matching marker and serves nothing.
//!
//! Value: `at=<unix seconds of the switch file>;digest=<the module's import digest, may be empty>`. Written by `step` after
//! the import and the same-answer check, in one claim transaction of its own, and before `switched` is recorded. Removed
//! by a finished rollback. (The module's own import runs in the module's transactions: the framework cannot make the two
//! one, so the marker follows the last of them and a crash in between just means `step` runs again.)

use rusqlite::Connection;

use crate::db::Core;
use crate::error::Result;

fn key(module: &str) -> String {
    format!("cutover:{module}")
}

fn seed_key(module: &str) -> String {
    format!("cutover:{module}:seed")
}

/// `seed`: what the module keeps beside the marker (see `Module::seed`); a re-step with none removes an older one.
pub(crate) fn write(
    core: &mut Core,
    module: &str,
    at: i64,
    digest: &str,
    seed: Option<&str>,
) -> Result<()> {
    let tx = core.claim_tx()?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        [key(module), format!("at={at};digest={digest}")],
    )?;
    match seed {
        Some(v) => tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            [seed_key(module), v.to_owned()],
        )?,
        None => tx.execute("DELETE FROM meta WHERE key = ?1", [seed_key(module)])?,
    };
    tx.commit()?;
    Ok(())
}

/// The `at` the marker holds, and the digest.
pub(crate) fn read(db: &Connection, module: &str) -> Option<(i64, String)> {
    let v: String = db
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [key(module)],
            |r| r.get(0),
        )
        .ok()?;
    let (at, digest) = v.strip_prefix("at=")?.split_once(";digest=")?;
    Some((at.parse().ok()?, digest.to_owned()))
}

/// The seed written with the marker; `None` for a marker from before seeds, which is the safe answer.
pub(crate) fn seed(db: &Connection, module: &str) -> Option<String> {
    db.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [seed_key(module)],
        |r| r.get(0),
    )
    .ok()
}

pub(crate) fn clear(core: &mut Core, module: &str) -> Result<()> {
    let tx = core.claim_tx()?;
    tx.execute("DELETE FROM meta WHERE key = ?1", [key(module)])?;
    tx.execute("DELETE FROM meta WHERE key = ?1", [seed_key(module)])?;
    tx.commit()?;
    Ok(())
}
