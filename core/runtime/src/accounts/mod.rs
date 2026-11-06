//! Account state in core.db, the core side of account choice (M3-9a). The rules (`pick`, the statuses, the listing) are
//! `kith_kernel::accounts`; this module only supplies where they read the state from: [`CoreState`] is a
//! `StateSource` over the `account_state` table (m0105) that answers the same shape `state.json`'s `accounts` has, so
//! `pick` over core.db is the file's `pick` over the same state, by construction. `tests.rs` holds it to that on the
//! golden corpora and on random state.
//!
//! UNSWITCHED, and nothing here writes account state in production. Python's `kith account out|clear|usage`, a launch's
//! `last_started` (Python and Rust), `--extend` and `own` keep writing `state.json`; the file stays the source of truth
//! until every writer switches together. The writes in this module ([`replace_all`]) are for the import
//! (`import/accounts.rs`) and the tests. The rows `account.state` and `account.pick` read core.db; nothing runs on its own. `account.pick` starts the harness status commands, but only on an owner's call.
//!
//! Failover (9b, `failover.rs`; the writer here is [`mark_out`], gated by [`core_writes`]): a running claim never moves to another account (q216); see the header of m0105.

use rusqlite::{Connection, params, types::Value as Sql};
use serde_json::Value;

use kith_kernel::accounts::StateSource;

use crate::error::Result;

pub(crate) mod call;
pub(crate) mod failover;
#[cfg(all(test, unix))]
mod failover_tests;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, unix))]
mod write_tests;

type Map = serde_json::Map<String, Value>;

/// The numeric keys that have a column of their own, in column order.
const NUMBERS: [&str; 5] = [
    "out_until",
    "since",
    "cleared_at",
    "cleared_host_until",
    "last_started",
];
const COLUMNS: &str = "id, out_until, reason, since, cleared_at, cleared_host_until, last_started, usage_json, extra_json";

/// A `StateSource` over the `account_state` rows of an open core.db connection.
pub struct CoreState<'a>(pub &'a Connection);

impl StateSource for CoreState<'_> {
    fn accounts(&self, _machine: &std::path::Path) -> Map {
        // A table that cannot be read is no state, as an unreadable file is (the caller still gets an answer).
        read_all(self.0).unwrap_or_default()
    }
}

fn number(v: &Value) -> Option<Sql> {
    let n = v.as_number()?;
    if let Some(i) = n.as_i64() {
        return Some(Sql::Integer(i));
    }
    // Above i64 stays in `extra_json`, where it keeps its digits.
    if n.is_u64() {
        return None;
    }
    n.as_f64().map(Sql::Real)
}

/// One entry as the columns hold it.
struct Parts {
    /// `NUMBERS`, in order.
    numbers: Vec<Sql>,
    reason: Sql,
    usage: Option<String>,
    extra: String,
}

/// The plain-typed known keys go to their columns, everything else to `extra_json`.
fn split(entry: &Map) -> Parts {
    let numbers: Vec<Sql> = NUMBERS
        .iter()
        .map(|k| entry.get(*k).and_then(number).unwrap_or(Sql::Null))
        .collect();
    let reason = match entry.get("reason") {
        Some(Value::String(s)) => Sql::Text(s.clone()),
        _ => Sql::Null,
    };
    let usage = match entry.get("usage") {
        Some(u @ Value::Object(_)) => Some(u.to_string()),
        _ => None,
    };
    let extra: Map = entry
        .iter()
        .filter(|(k, v)| match k.as_str() {
            "reason" => matches!(reason, Sql::Null),
            "usage" => usage.is_none(),
            k if NUMBERS.contains(&k) => number(v).is_none(),
            _ => true,
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Parts {
        numbers,
        reason,
        usage,
        extra: Value::Object(extra).to_string(),
    }
}

/// Replaces every row with `accounts` (state.json's `accounts` object). Import and tests only.
pub(crate) fn replace_all(conn: &Connection, accounts: &Map) -> Result<()> {
    conn.execute("DELETE FROM account_state", [])?;
    let mut put = conn.prepare(&format!(
        "INSERT INTO account_state ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
    ))?;
    for (id, entry) in accounts {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let p = split(entry);
        put.execute(params![
            id,
            p.numbers[0],
            p.reason,
            p.numbers[1],
            p.numbers[2],
            p.numbers[3],
            p.numbers[4],
            p.usage,
            p.extra
        ])?;
    }
    Ok(())
}

fn sql_json(v: Sql) -> Option<Value> {
    match v {
        Sql::Integer(i) => Some(Value::from(i)),
        Sql::Real(f) => Some(Value::from(f)),
        Sql::Text(s) => Some(Value::String(s)),
        _ => None,
    }
}

fn entry_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Value)> {
    let id: String = r.get(0)?;
    let mut m = Map::new();
    let keys = [
        "out_until",
        "reason",
        "since",
        "cleared_at",
        "cleared_host_until",
        "last_started",
    ];
    for (i, k) in keys.iter().enumerate() {
        if let Some(v) = sql_json(r.get::<_, Sql>(i + 1)?) {
            m.insert((*k).to_owned(), v);
        }
    }
    if let Some(u) = r.get::<_, Option<String>>(7)?
        && let Ok(v) = serde_json::from_str::<Value>(&u)
    {
        m.insert("usage".to_owned(), v);
    }
    if let Ok(Value::Object(x)) = serde_json::from_str::<Value>(&r.get::<_, String>(8)?) {
        m.extend(x);
    }
    Ok((id, Value::Object(m)))
}

/// Every entry, as `state.json`'s `accounts` object.
pub(crate) fn read_all(conn: &Connection) -> Result<Map> {
    let mut q = conn.prepare(&format!("SELECT {COLUMNS} FROM account_state ORDER BY id"))?;
    let rows = q.query_map([], entry_of)?;
    let mut out = Map::new();
    for r in rows {
        let (id, v) = r?;
        out.insert(id, v);
    }
    Ok(out)
}

/// How long an account stays out when the provider did not say when its limit lifts (`OUT_FOR` of `kith_accounts.py`,
/// and the host's `UNKNOWN_RESET_SECS`).
pub(crate) const OUT_FOR: i64 = 3600;

/// `meta` key: `1` once the core is the writer of account state (every writer switched together, see m0105). Until then
/// the failover in the claims report path (`failover.rs`) records why a turn moved but writes no account row, so live
/// Python stays the only writer. Nothing sets it in production.
pub(crate) const CORE_WRITES: &str = "accounts_core_writes";

/// Whether the core may write account state: the `accounts` cutover module is switched ([`switched`]), or the flag
/// [`CORE_WRITES`] says so (the lane's own tests and a hand-set dev core; nothing sets it in production).
pub(crate) fn core_writes(conn: &Connection) -> Result<bool> {
    use rusqlite::OptionalExtension as _;
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [CORE_WRITES],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.as_deref() == Some("1") || switched(conn))
}

/// The `accounts` cutover module is switched for THIS core: the switch file, the marker in this very database and the
/// tombstone all agree (`cutover::same_answer::may_serve`), so a core.db from another machine, a forged switch file or a
/// rolled-back module answers no. The machine folder is found from the connection's own path; a database that is not at
/// `<machine>/core/core.db` (memory, a test's scratch file) is never switched. Unix only, like the framework.
#[cfg(unix)]
fn switched(conn: &Connection) -> bool {
    use crate::cutover::{Env, modules::accounts::Accounts, same_answer::may_serve};
    static WARNED: std::sync::Once = std::sync::Once::new();
    let Some(db) = conn.path().map(std::path::Path::new) else {
        return false;
    };
    let Some(machine) = db.parent().and_then(std::path::Path::parent) else {
        return false;
    };
    let env = Env::new(machine);
    if env.layout.db != db {
        return false;
    }
    let s = may_serve(&Accounts, &env, conn, false);
    // The one place the core acts although the old file is no tombstone any more (the framework serves on, loudly): say so
    // in the core log, once, not on every write.
    if let Some(w) = s.warning.as_ref().filter(|_| s.ok) {
        WARNED
            .call_once(|| kith_kernel::log::note(&format!("accounts: core writes continue: {w}")));
    }
    s.ok
}

#[cfg(not(unix))]
fn switched(_: &Connection) -> bool {
    false
}

/// `mark_out` of `kith_accounts.py`: the entry is out until `until` (default an hour from `now`), epoch seconds. With
/// `extend_only` an entry already out for longer stays so. `since` is kept while the entry was already out. The write is
/// the caller's to gate ([`core_writes`]); this is the writer the failover and, at the switch, `kith account out` use.
#[allow(
    clippy::cast_precision_loss,
    reason = "epoch seconds fit an f64's mantissa; `pick` compares them as floats the same way"
)]
pub(crate) fn mark_out(
    conn: &Connection,
    id: &str,
    until: Option<i64>,
    reason: &str,
    now: i64,
    extend_only: bool,
) -> Result<()> {
    let until = until.filter(|u| *u != 0).unwrap_or(now + OUT_FOR);
    let mut entry = read_all(conn)?
        .remove(id)
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    let cur = entry.get("out_until").and_then(Value::as_f64);
    if extend_only && cur.is_some_and(|c| c > until.max(now) as f64) {
        return Ok(());
    }
    let since = match (cur, entry.get("since")) {
        (Some(c), Some(s)) if c > now as f64 => s.clone(),
        _ => Value::from(now),
    };
    entry.insert("since".into(), since);
    entry.insert("out_until".into(), Value::from(until));
    entry.insert("reason".into(), Value::from(reason));
    put(conn, id, &entry)
}

/// `INSERT OR REPLACE` of one entry as the columns hold it.
fn put(conn: &Connection, id: &str, entry: &Map) -> Result<()> {
    let p = split(entry);
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO account_state ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        ),
        params![
            id,
            p.numbers[0],
            p.reason,
            p.numbers[1],
            p.numbers[2],
            p.numbers[3],
            p.numbers[4],
            p.usage,
            p.extra
        ],
    )?;
    Ok(())
}

/// One entry, as `state.json` keeps it.
fn read_entry(conn: &Connection, id: &str) -> Result<Option<Map>> {
    use rusqlite::OptionalExtension as _;
    let row = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM account_state WHERE id = ?1"),
            [id],
            entry_of,
        )
        .optional()?;
    Ok(row.and_then(|(_, v)| match v {
        Value::Object(m) => Some(m),
        _ => None,
    }))
}

/// One change of the writers (`kith account out|clear|usage`, a launch's start), in one transaction: the same edit the
/// file's writers make (`kith_kernel::accounts::apply_change`) on the entry's row. The caller gates it ([`core_writes`]).
pub(crate) fn make(
    conn: &Connection,
    change: &kith_kernel::accounts::Change,
    now: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let id = change.id();
    let mut one = Map::new();
    if let Some(e) = read_entry(&tx, id)? {
        one.insert(id.to_owned(), Value::Object(e));
    }
    kith_kernel::accounts::apply_change(&mut one, change, now);
    if let Some(Value::Object(e)) = one.get(id) {
        put(&tx, id, e)?;
    }
    tx.commit()?;
    Ok(())
}
