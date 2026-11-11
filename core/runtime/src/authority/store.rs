//! Writing an authority change to core.db. A write is ONE `BEGIN IMMEDIATE` transaction through [`Core::claim_tx`]
//! (as the gate's are): read the tables as the auth.json-shaped value, let a function of `write.rs` change the value the
//! way `kith-auth` would, then store the difference row by row and commit. A refusal drops the transaction, so nothing
//! of a half-done verb stays (`kith-auth` saves once, at the end, the same).

use kith_kernel::secrets::{Ctx, Die};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use super::{bad, read_tables, secret_writes, write};
use crate::db::Core;
use crate::error::{Error, Result};

/// Runs `f` over the tables in one write transaction. What `f` says (the lines `kith-auth` would print) comes back.
pub(super) fn run<T>(
    core: &mut Core,
    c: &Ctx,
    f: impl FnOnce(&mut Value) -> std::result::Result<T, Die>,
) -> Result<T> {
    run_with(core, c, f, |_, _| Ok(()))
}

/// [`run`], then `follow` in the same transaction with what the write said: what else the change must take with it
/// (the doors of a person who was cut off) commits or fails together with it.
pub(super) fn run_with<T>(
    core: &mut Core,
    c: &Ctx,
    f: impl FnOnce(&mut Value) -> std::result::Result<T, Die>,
    follow: impl FnOnce(&Connection, &mut T) -> Result<()>,
) -> Result<T> {
    let tx = core.claim_tx()?;
    let before = read_tables(&tx, &mut || {})?;
    let mut after = before.clone();
    write::tables(&mut after);
    let mut said = f(&mut after).map_err(|d| refusal(&d))?;
    let mut before = before;
    write::tables(&mut before);
    // A write that changed nothing (a router's refused card write answers as an answer, with the tables put back) saves
    // nothing in kith-auth either, so the codes that ran out are not dropped by it.
    if after != before {
        write::prune_invites(&mut after, c);
    }
    store(&tx, &before, &after)?;
    follow(&tx, &mut said)?;
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

/// The digest of the tables in one snapshot, under the same door as a write: `people.revision`, which the host's cards
/// compare to see authority change on a tombstone (the file's change time never moves there).
pub(super) fn digest(core: &mut Core) -> Result<String> {
    let tx = core.claim_tx()?;
    let tables = read_tables(&tx, &mut || {})?;
    Ok(crate::cutover::authority::tables_digest(&tables))
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
pub(super) fn store(conn: &Connection, before: &Value, after: &Value) -> Result<()> {
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
        let handles = json(p, "handles");
        if old.contains_key(id) {
            conn.execute(
                "UPDATE people SET name = ?2, lang = ?3, tz = ?4, owner = ?5, bot_of = ?6, revoked = ?7, \
                 revoked_at = ?8, was = ?9, provider = ?10, handles = ?11 WHERE id = ?1",
                params![id, name, lang, tz, owner, bot_of, i64::from(revoked), at, was, provider, handles],
            )?;
        } else {
            conn.execute(
                "INSERT INTO people (id, name, lang, tz, owner, bot_of, revoked, revoked_at, was, provider, handles) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![id, name, lang, tz, owner, bot_of, i64::from(revoked), at, was, provider, handles],
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
                 budget, access, jobs, helper_once, access_by, access_since, helper_since) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                    json(r, "helper_since"),
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

    // same row shape as `load` makes from auth.json
    let (old, new) = (table(before, "agent_access"), table(after, "agent_access"));
    for agent in old.keys().filter(|k| !new.contains_key(*k)) {
        conn.execute("DELETE FROM agent_access WHERE agent = ?1", [agent])?;
    }
    for (agent, r) in new {
        if old.get(agent) != Some(r) {
            conn.execute(
                "INSERT OR REPLACE INTO agent_access (agent, domains, since, by) VALUES (?1, ?2, ?3, ?4)",
                params![agent, json(r, "domains"), json(r, "since"), json(r, "by")],
            )?;
        }
    }

    // lane B's tables: only what the core's writes change (`forget` drops grants and `with` entries). A profile's
    // variables and browser stay as they are; a grant list keeps its order in `seq`.
    let (old, new) = (table(before, "grants"), table(after, "grants"));
    for agent in old
        .keys()
        .chain(new.keys().filter(|k| !old.contains_key(*k)))
    {
        if old.get(agent) == new.get(agent) {
            continue;
        }
        conn.execute("DELETE FROM secret_grants WHERE agent = ?1", [agent])?;
        for (service, v) in new.get(agent).map(table_of).into_iter().flatten() {
            let profiles: Vec<&Value> = match v {
                Value::Array(list) => list.iter().collect(),
                one => vec![one],
            };
            for (seq, profile) in profiles.into_iter().filter_map(Value::as_str).enumerate() {
                conn.execute(
                    "INSERT OR REPLACE INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, ?4)",
                    params![agent, service, profile, i64::try_from(seq).unwrap_or(i64::MAX)],
                )?;
            }
        }
    }
    let (old, new) = (table(before, "profiles"), table(after, "profiles"));
    // Only a host connector's mark (`disconnect`) is ever removed; a keyed profile is the owner's to keep, so anything else
    // refuses the whole write rather than half-delete.
    for (name, p) in old.iter().filter(|(k, _)| !new.contains_key(*k)) {
        if !secret_writes::is_host_store(p) {
            return Err(bad(
                "profiles",
                format!("a profile with keys cannot be removed through the core ('{name}')"),
            ));
        }
        conn.execute("DELETE FROM secret_vars WHERE profile = ?1", [name])?;
        conn.execute("DELETE FROM secret_profiles WHERE profile = ?1", [name])?;
    }
    for (name, p) in new {
        let was = old.get(name);
        let same = |o: &Value| o.get("person") == p.get("person") && o.get("with") == p.get("with");
        if !was.is_some_and(same) {
            conn.execute(
                "INSERT INTO secret_profiles (profile, person, lent) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(profile) DO UPDATE SET person = excluded.person, lent = excluded.lent",
                params![name, text(p, "person")?, json(p, "with")],
            )?;
        }
        // the variables: each is the Keychain reference (or `env:`/`env-file:` one) of its value, never the value
        let (a, b) = (vars_of(was), vars_of(Some(p)));
        if a == b {
            continue;
        }
        for var in a.keys().filter(|k| !b.contains_key(*k)) {
            conn.execute(
                "DELETE FROM secret_vars WHERE profile = ?1 AND var = ?2",
                [name, var],
            )?;
        }
        for (var, source) in b.iter().filter(|(k, v)| a.get(*k) != Some(v)) {
            conn.execute(
                "INSERT OR REPLACE INTO secret_vars (profile, var, source) VALUES (?1, ?2, ?3)",
                params![name, var, source],
            )?;
        }
    }

    // `env`: a service's settings keep the order they were written in (`seq`); an agent or service left with none is gone
    let (old, new) = (table(before, "env"), table(after, "env"));
    for agent in old
        .keys()
        .chain(new.keys().filter(|k| !old.contains_key(*k)))
    {
        let (was, now) = (table_of_opt(old.get(agent)), table_of_opt(new.get(agent)));
        for service in was
            .keys()
            .chain(now.keys().filter(|k| !was.contains_key(*k)))
        {
            // text, not value equality: the order of the variables is part of the row
            if was.get(service).map(Value::to_string) == now.get(service).map(Value::to_string) {
                continue;
            }
            conn.execute(
                "DELETE FROM secret_env WHERE agent = ?1 AND service = ?2",
                [agent, service],
            )?;
            for (seq, (var, value)) in table_of_opt(now.get(service)).iter().enumerate() {
                conn.execute(
                    "INSERT INTO secret_env (agent, service, var, value, seq) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![agent, service, var, value.to_string(), i64::try_from(seq).unwrap_or(i64::MAX)],
                )?;
            }
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

/// A profile's variables (every key that is not `person`, `with` or `browser`) and where each one's value lives.
fn vars_of(p: Option<&Value>) -> std::collections::BTreeMap<String, String> {
    p.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, _)| !["person", "with", "browser"].contains(&k.as_str()))
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
        .collect()
}

fn table_of_opt(v: Option<&Value>) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn table_of(v: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}
