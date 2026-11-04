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
//! For failover (9b, not here): a running claim never moves to another account (q216); see the header of m0105.

use rusqlite::{Connection, params, types::Value as Sql};
use serde_json::Value;

use kith_kernel::accounts::StateSource;

use crate::error::Result;

pub(crate) mod call;
#[cfg(all(test, unix))]
mod tests;

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
