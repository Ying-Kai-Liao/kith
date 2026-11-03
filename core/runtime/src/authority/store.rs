//! Writing an authority change to core.db. A write is ONE `BEGIN IMMEDIATE` transaction through [`Core::claim_tx`]
//! (as the gate's are): read the tables as the auth.json-shaped value, let a function of `write.rs` change the value the
//! way `kith-auth` would, then store the difference row by row and commit. A refusal drops the transaction, so nothing
//! of a half-done verb stays (`kith-auth` saves once, at the end, the same).

use kith_kernel::secrets::{Ctx, Die};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use super::{bad, read_tables, write};
use crate::db::Core;
use crate::error::{Error, Result};

/// Runs `f` over the tables in one write transaction. What `f` says (the lines `kith-auth` would print) comes back.
pub(super) fn run<T>(
    core: &mut Core,
    c: &Ctx,
    f: impl FnOnce(&mut Value) -> std::result::Result<T, Die>,
) -> Result<T> {
    let tx = core.claim_tx()?;
    let before = read_tables(&tx, &mut || {})?;
    let mut after = before.clone();
    write::tables(&mut after);
    let said = f(&mut after).map_err(|d| refusal(&d))?;
    write::prune_invites(&mut after, c);
    let mut before = before;
    write::tables(&mut before);
    store(&tx, &before, &after)?;
    tx.commit()?;
    Ok(said)
}

/// Reads the tables as one snapshot, under the same door as a write (the generation must still be this core's), and
/// changes nothing: no codes that ran out are dropped, as `kith-auth` only does that when it saves.
pub(super) fn read<T>(core: &mut Core, f: impl FnOnce(&Value) -> T) -> Result<T> {
    let tx = core.claim_tx()?;
    let mut now = read_tables(&tx, &mut || {})?;
    write::tables(&mut now);
    Ok(f(&now))
}

/// A refusal of the rules, as the readers give theirs: `Refused`, or `BadParams` for the ones `kith-auth` gives exit
/// code 2 (the request is wrong, not the state).
pub(super) fn refusal(d: &Die) -> Error {
    if d.code == 2 {
        Error::BadParams(format!("authority: {}", d.msg))
    } else {
        Error::Refused(format!("authority: {}", d.msg))
    }
}

fn table<'a>(v: &'a Value, name: &str) -> &'a Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.get(name)
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// A text column: a string, or NULL.
fn text(o: &Value, k: &str) -> Result<Option<String>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(bad(k, format!("expected text, found {other}"))),
    }
}

/// A JSON column.
fn json(o: &Value, k: &str) -> Option<String> {
    o.get(k).map(Value::to_string)
}

fn flag(o: &Value, k: &str) -> Result<Option<i64>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(i64::from(*b))),
        Some(other) => Err(bad(k, format!("expected true or false, found {other}"))),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Stores what differs between the two values. Rows are matched by their key, so a changed person keeps its `seq` (the
/// order `canonical` and `owner` read), and a new one goes last, where `kith-auth` appends it.
fn store(conn: &Connection, before: &Value, after: &Value) -> Result<()> {
    let (old, new) = (table(before, "people"), table(after, "people"));
    for id in old.keys().filter(|k| !new.contains_key(*k)) {
        conn.execute("DELETE FROM people WHERE id = ?1", [id])?;
    }
    for (id, p) in new {
        if old.get(id) == Some(p) {
            continue;
        }
        let revoked = truthy(p.get("revoked"));
        let at = p
            .get("revoked")
            .and_then(|r| r.get("at"))
            .and_then(Value::as_i64);
        let (name, lang, tz) = (text(p, "name")?, text(p, "lang")?, text(p, "tz")?);
        let (bot_of, provider, was) = (text(p, "bot_of")?, text(p, "provider")?, json(p, "was"));
        let owner = i64::from(truthy(p.get("owner")));
        if old.contains_key(id) {
            conn.execute(
                "UPDATE people SET name = ?2, lang = ?3, tz = ?4, owner = ?5, bot_of = ?6, revoked = ?7, \
                 revoked_at = ?8, was = ?9, provider = ?10 WHERE id = ?1",
                params![id, name, lang, tz, owner, bot_of, i64::from(revoked), at, was, provider],
            )?;
        } else {
            conn.execute(
                "INSERT INTO people (id, name, lang, tz, owner, bot_of, revoked, revoked_at, was, provider) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![id, name, lang, tz, owner, bot_of, i64::from(revoked), at, was, provider],
            )?;
        }
    }

    let (old, new) = (table(before, "members"), table(after, "members"));
    for (agent, rows) in old {
        for person in table_of(rows).keys() {
            let kept = new
                .get(agent)
                .map(table_of)
                .is_some_and(|r| r.contains_key(person));
            if !kept {
                conn.execute(
                    "DELETE FROM memberships WHERE agent = ?1 AND person = ?2",
                    [agent, person],
                )?;
            }
        }
    }
    for (agent, rows) in new {
        for (person, r) in table_of(rows) {
            if old.get(agent).map(table_of).and_then(|o| o.get(person)) == Some(r) {
                continue;
            }
            conn.execute(
                "INSERT OR REPLACE INTO memberships (agent, person, preset, mode, trusted, deny, model, env, helpers, \
                 budget, access, jobs, helper_once, access_by, access_since) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    agent,
                    person,
                    text(r, "preset")?,
                    text(r, "mode")?,
                    flag(r, "trusted")?,
                    json(r, "deny"),
                    text(r, "model")?,
                    json(r, "env"),
                    json(r, "helpers"),
                    text(r, "budget")?,
                    json(r, "access"),
                    json(r, "jobs"),
                    json(r, "helper_once"),
                    json(r, "access_by"),
                    json(r, "access_since"),
                ],
            )?;
        }
    }

    let (old, new) = (table(before, "presets"), table(after, "presets"));
    for name in old.keys().filter(|k| !new.contains_key(*k)) {
        conn.execute("DELETE FROM presets WHERE name = ?1", [name])?;
    }
    for (name, p) in new {
        if old.get(name) != Some(p) {
            conn.execute(
                "INSERT OR REPLACE INTO presets (name, mode, trusted, deny) VALUES (?1, ?2, ?3, ?4)",
                params![name, text(p, "mode")?, flag(p, "trusted")?, json(p, "deny")],
            )?;
        }
    }

    let (old, new) = (table(before, "invites"), table(after, "invites"));
    for code in old.keys().filter(|k| !new.contains_key(*k)) {
        conn.execute("DELETE FROM invites WHERE code = ?1", [code])?;
    }
    for (code, i) in new {
        if old.get(code) != Some(i) {
            conn.execute(
                "INSERT OR REPLACE INTO invites (code, person, expires) VALUES (?1, ?2, ?3)",
                params![
                    code,
                    text(i, "person")?,
                    i.get("expires").and_then(Value::as_f64)
                ],
            )?;
        }
    }

    let (old, new) = (table(before, "retired"), table(after, "retired"));
    for id in old.keys().filter(|k| !new.contains_key(*k)) {
        conn.execute("DELETE FROM retired WHERE id = ?1", [id])?;
    }
    for (id, r) in new {
        if old.get(id) != Some(r) {
            conn.execute(
                "INSERT OR REPLACE INTO retired (id, at) VALUES (?1, ?2)",
                params![id, r.get("at").and_then(Value::as_i64)],
            )?;
        }
    }

    match (
        before.get("owner_bots_budget"),
        after.get("owner_bots_budget"),
    ) {
        (a, b) if a == b => {}
        (_, Some(cap)) => {
            conn.execute(
                "INSERT OR REPLACE INTO authority_settings (key, value) VALUES ('owner_bots_budget', ?1)",
                [cap.to_string()],
            )?;
        }
        (_, None) => {
            conn.execute(
                "DELETE FROM authority_settings WHERE key = 'owner_bots_budget'",
                [],
            )?;
        }
    }
    Ok(())
}

fn table_of(v: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
