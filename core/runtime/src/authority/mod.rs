//! Authority, the read half (M2-1): who may use which agent, and with what settings, answered from core.db.
//!
//! The rules are `kith_kernel::secrets` (the Rust port of `kernel/bin/kith-auth`'s `may`, `resolve`, `resolve_bot` and
//! `cut_off`, which `kernel/tests/difftest` holds equal to the Python). This module does not carry a second copy: each
//! call reads core.db in ONE read transaction into the same shape auth.json has (`snapshot`), then asks those rules.
//! So the answers can differ from the file's only where the tables differ; `tests.rs` loads generated auth.json data
//! into core.db and checks that.
//!
//! The write half (M2-2) is `write.rs` (the verbs of `kith-auth`, over the auth.json shape), `store.rs` (one claim
//! transaction each, the difference stored row by row) and `call.rs` (the `people.*` rows the daemon routes here).
//! The person-level writes (M2-4: add, rename, remove, suspend/undo a person, the ids a removal retires) are
//! `revoke.rs`.
//!
//! Nothing is switched. `kith-auth` and the host still decide from auth.json; only the daemon's `people.*` rows call this
//! module, and no caller sends them yet (the CLI switch is M2-18, the import of auth.json M2-8).

use kith_kernel::secrets::{self, Ctx, Rules};
use rusqlite::{Connection, Row};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub(crate) mod call;
#[cfg(test)]
mod load;
mod people_json;
mod requests;
mod revoke;
mod store;
#[cfg(test)]
mod tests;
mod write;

/// The decision over one core.db. `ctx` says where agents live and what time it is, as it does for `kith-auth`.
pub(crate) struct Authority<'a> {
    conn: &'a Connection,
    ctx: &'a Ctx,
}

impl<'a> Authority<'a> {
    pub(crate) fn new(conn: &'a Connection, ctx: &'a Ctx) -> Self {
        Self { conn, ctx }
    }

    /// `may`: (allowed, why). Unknown person, revoked person, a bot whose owner may not, and an agent with no row are all
    /// "no", with the same words `kith-auth` gives.
    pub(crate) fn may(
        &self,
        person: &str,
        agent: &str,
        via: Option<&str>,
        once: bool,
    ) -> Result<(bool, String)> {
        let auth = snapshot(self.conn)?;
        Rules::new(self.ctx, &auth)
            .may(person, agent, via, once)
            .map_err(|d| refused(&d))
    }

    /// `resolve`: what a run of `agent` for `person` gets (a bot's too: see [`Self::resolve_bot`]).
    pub(crate) fn resolve(
        &self,
        agent: &str,
        person: &str,
        via: Option<&str>,
        once: bool,
    ) -> Result<Map<String, Value>> {
        let auth = snapshot(self.conn)?;
        Rules::new(self.ctx, &auth)
            .resolve(agent, person, via, once)
            .map_err(|d| refused(&d))
    }

    /// `resolve_bot`: `resolve`, for a bot only: `None` when `person` is not a bot. The bot rules are not a separate
    /// entry point in `kith-auth` either: `resolve` hands a bot to them after `may` allowed it, and they cap the bot by
    /// its owner's own `resolve`.
    pub(crate) fn resolve_bot(
        &self,
        agent: &str,
        bot: &str,
        via: Option<&str>,
        once: bool,
    ) -> Result<Option<Map<String, Value>>> {
        let auth = snapshot(self.conn)?;
        let id = secrets::canonical(&auth, bot);
        if secrets::parent_of(&auth, &id).is_none() {
            return Ok(None);
        }
        Rules::new(self.ctx, &auth)
            .resolve(agent, &id, via, once)
            .map(Some)
            .map_err(|d| refused(&d))
    }

    /// `cut_off`: whether the person, or the person a bot of theirs answers to, is revoked.
    pub(crate) fn cut_off(&self, person: &str) -> Result<bool> {
        let auth = snapshot(self.conn)?;
        Ok(secrets::cut_off(&auth, person))
    }
}

/// Whether a run may exist for `person` (M2-13): they are a known person or bot and neither they nor the owner of
/// the bot are revoked. A removed or never-added id is "no", the same as a revoked one. Read from core.db alone (the
/// run tokens have no `Ctx` to give).
pub(crate) fn run_allowed_in(conn: &Connection, person: &str) -> Result<bool> {
    let auth = snapshot(conn)?;
    let id = secrets::canonical(&auth, person);
    let known = auth
        .get("people")
        .and_then(Value::as_object)
        .is_some_and(|p| p.contains_key(&id));
    Ok(known && !secrets::cut_off(&auth, &id))
}

/// A rule that stopped with a refusal `kith-auth` would `die` with (a membership naming a preset nobody defined, which
/// `kith-auth preset X --rm` leaves behind): the rules have no answer, which the caller must read as "no". It is not a
/// damaged database, so it is never `Corrupt` (whose recovery is `kith restore`).
fn refused(d: &secrets::Die) -> Error {
    Error::Refused(format!("authority: {}", d.msg))
}

fn bad(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Refused(format!("core.db: authority {what}: {e}"))
}

fn text(r: &Row<'_>, i: usize) -> Result<Option<String>> {
    Ok(r.get::<_, Option<String>>(i)?)
}

fn json(r: &Row<'_>, i: usize, what: &str) -> Result<Option<Value>> {
    match text(r, i)? {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t).map(Some).map_err(|e| bad(what, e)),
    }
}

fn set(o: &mut Map<String, Value>, key: &str, v: Option<Value>) {
    if let Some(v) = v {
        o.insert(key.to_owned(), v);
    }
}

fn flag(r: &Row<'_>, i: usize) -> Result<Option<Value>> {
    Ok(r.get::<_, Option<i64>>(i)?.map(|n| Value::Bool(n != 0)))
}

/// core.db's authority tables as the auth.json value the rules read, from one read transaction (a snapshot: a writer
/// that commits halfway through cannot show this call a person revoked in one table and not yet in the next).
/// A NULL column is a missing key.
pub(crate) fn snapshot(conn: &Connection) -> Result<Value> {
    snapshot_with(conn, &mut || {})
}

/// The tables as the rules read them, inside a transaction the caller holds and writes in (the doors decide in the
/// transaction that writes the row, so the answer and the write cannot be split by a revoke).
pub(crate) fn tables_within(tx: &Connection) -> Result<Value> {
    read_tables(tx, &mut || {})
}

/// `may` over tables already read (see [`tables_within`]): (allowed, why), a rule with no answer being an error.
pub(crate) fn may_in(ctx: &Ctx, auth: &Value, person: &str, agent: &str) -> Result<(bool, String)> {
    Rules::new(ctx, auth)
        .may(person, agent, None, false)
        .map_err(|d| refused(&d))
}

/// `resolve` over tables already read: what a run of `agent` for `person` gets (a bot's already cut to its owner's).
pub(crate) fn resolve_in(
    ctx: &Ctx,
    auth: &Value,
    agent: &str,
    person: &str,
) -> Result<Map<String, Value>> {
    Rules::new(ctx, auth)
        .resolve(agent, person, None, false)
        .map_err(|d| refused(&d))
}

/// [`snapshot`], calling `between` after each table is read: a test writes there to prove the later tables are read
/// from the same snapshot.
fn snapshot_with(conn: &Connection, between: &mut dyn FnMut()) -> Result<Value> {
    let tx = conn.unchecked_transaction()?;
    read_tables(&tx, between)
}

/// [`snapshot_with`] inside a transaction the caller holds (a write reads what it changes in its own transaction).
fn read_tables(tx: &Connection, between: &mut dyn FnMut()) -> Result<Value> {
    let mut people = Map::new();
    {
        let mut q = tx.prepare(
            "SELECT id, name, lang, tz, owner, bot_of, revoked, revoked_at, was, provider, handles FROM people ORDER BY seq",
        )?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let mut p = Map::new();
            for (k, i) in [("name", 1), ("lang", 2), ("tz", 3)] {
                set(&mut p, k, text(r, i)?.map(Value::String));
            }
            if r.get::<_, i64>(4)? != 0 {
                p.insert("owner".to_owned(), true.into());
            }
            set(&mut p, "bot_of", text(r, 5)?.map(Value::String));
            if r.get::<_, i64>(6)? != 0 {
                let at = r.get::<_, Option<i64>>(7)?;
                p.insert(
                    "revoked".to_owned(),
                    at.map_or(Value::Bool(true), |at| serde_json::json!({ "at": at })),
                );
            }
            set(&mut p, "was", json(r, 8, "people.was")?);
            set(&mut p, "provider", text(r, 9)?.map(Value::String));
            set(&mut p, "handles", json(r, 10, "people.handles")?);
            people.insert(r.get::<_, String>(0)?, Value::Object(p));
        }
    }
    between();
    let mut members: Map<String, Value> = Map::new();
    {
        let mut q = tx.prepare(
            "SELECT agent, person, preset, mode, trusted, deny, model, env, helpers, budget, access, jobs, helper_once, \
             access_by, access_since FROM memberships ORDER BY agent, person",
        )?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let mut row = Map::new();
            set(&mut row, "preset", text(r, 2)?.map(Value::String));
            set(&mut row, "mode", text(r, 3)?.map(Value::String));
            set(&mut row, "trusted", flag(r, 4)?);
            set(&mut row, "deny", json(r, 5, "memberships.deny")?);
            set(&mut row, "model", text(r, 6)?.map(Value::String));
            set(&mut row, "env", json(r, 7, "memberships.env")?);
            set(&mut row, "helpers", json(r, 8, "memberships.helpers")?);
            set(&mut row, "budget", text(r, 9)?.map(Value::String));
            set(&mut row, "access", json(r, 10, "memberships.access")?);
            set(&mut row, "jobs", json(r, 11, "memberships.jobs")?);
            set(
                &mut row,
                "helper_once",
                json(r, 12, "memberships.helper_once")?,
            );
            set(&mut row, "access_by", json(r, 13, "memberships.access_by")?);
            set(
                &mut row,
                "access_since",
                json(r, 14, "memberships.access_since")?,
            );
            let agent: String = r.get(0)?;
            let rows = members
                .entry(agent)
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(rows) = rows {
                rows.insert(r.get::<_, String>(1)?, Value::Object(row));
            }
        }
    }
    between();
    let mut presets = Map::new();
    {
        let mut q = tx.prepare("SELECT name, mode, trusted, deny FROM presets ORDER BY name")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let mut p = Map::new();
            set(&mut p, "mode", text(r, 1)?.map(Value::String));
            set(&mut p, "trusted", flag(r, 2)?);
            set(&mut p, "deny", json(r, 3, "presets.deny")?);
            presets.insert(r.get::<_, String>(0)?, Value::Object(p));
        }
    }
    between();
    let mut invites = Map::new();
    {
        let mut q = tx.prepare("SELECT code, person, expires FROM invites ORDER BY code")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            invites.insert(
                r.get::<_, String>(0)?,
                serde_json::json!({ "person": r.get::<_, String>(1)?, "expires": r.get::<_, f64>(2)? }),
            );
        }
    }
    let mut retired = Map::new();
    {
        let mut q = tx.prepare("SELECT id, at FROM retired ORDER BY id")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let at = r.get::<_, Option<i64>>(1)?;
            retired.insert(
                r.get::<_, String>(0)?,
                at.map_or_else(
                    || Value::Object(Map::new()),
                    |at| serde_json::json!({ "at": at }),
                ),
            );
        }
    }
    let mut agent_access = Map::new();
    {
        let mut q =
            tx.prepare("SELECT agent, domains, since, by FROM agent_access ORDER BY agent")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            let mut row = Map::new();
            set(&mut row, "domains", json(r, 1, "agent_access.domains")?);
            set(&mut row, "since", json(r, 2, "agent_access.since")?);
            set(&mut row, "by", json(r, 3, "agent_access.by")?);
            agent_access.insert(r.get::<_, String>(0)?, Value::Object(row));
        }
    }
    // store.rs writes people, memberships, presets, invites, retired, agent_access, the budget, and the snapshot's
    // `grants` and `profiles` (person and `with` only, for `forget`); it does not write `handles_stale`.
    let mut auth = Map::new();
    // Lane B's tables, in auth.json's shape, only for the checks that read who owns or is lent a login or holds a grant
    // (`retired`, `person --rm --unused`). Left out when empty: nothing writes them until the secrets switch, and the
    // rules read neither.
    let profiles = read_profiles(tx)?;
    if !profiles.is_empty() {
        auth.insert("profiles".to_owned(), Value::Object(profiles));
    }
    let grants = read_grants(tx)?;
    if !grants.is_empty() {
        auth.insert("grants".to_owned(), Value::Object(grants));
    }
    auth.insert("agent_access".to_owned(), Value::Object(agent_access));
    auth.insert("invites".to_owned(), Value::Object(invites));
    auth.insert("retired".to_owned(), Value::Object(retired));
    auth.insert("people".to_owned(), Value::Object(people));
    auth.insert("members".to_owned(), Value::Object(members));
    auth.insert("presets".to_owned(), Value::Object(presets));
    let cap: Option<String> = tx
        .query_row(
            "SELECT value FROM authority_settings WHERE key = 'owner_bots_budget'",
            [],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    // Set by m0015 on a database imported before `people.handles` existed; a re-import clears it. Only `person --rm
    // --unused` reads it (a NULL handles may then hide a person's handles).
    let stale = tx
        .query_row(
            "SELECT 1 FROM authority_settings WHERE key = 'handles_stale'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if stale {
        auth.insert("handles_stale".to_owned(), Value::Bool(true));
    }
    if let Some(cap) = cap {
        auth.insert(
            "owner_bots_budget".to_owned(),
            serde_json::from_str(&cap).map_err(|e| bad("owner_bots_budget", e))?,
        );
    }
    Ok(Value::Object(auth))
}

/// `profiles` of auth.json from `secret_profiles`: `person` and `with` (the people it is lent to); the variables are not read.
fn read_profiles(tx: &Connection) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    let mut q = tx.prepare("SELECT profile, person, lent FROM secret_profiles ORDER BY profile")?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let mut p = Map::new();
        set(&mut p, "person", text(r, 1)?.map(Value::String));
        set(&mut p, "with", json(r, 2, "secret_profiles.lent")?);
        out.insert(r.get::<_, String>(0)?, Value::Object(p));
    }
    Ok(out)
}

/// `grants` of auth.json from `secret_grants`: agent (or `agent@person`) -> service -> profile. A list of profiles
/// keeps its order; only the keys matter to the checks that read it.
fn read_grants(tx: &Connection) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    let mut q = tx.prepare(
        "SELECT agent, service, profile FROM secret_grants ORDER BY agent, service, seq",
    )?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let (agent, service, profile): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let Value::Object(by_service) = out
            .entry(agent)
            .or_insert_with(|| Value::Object(Map::new()))
        else {
            continue;
        };
        match by_service.get_mut(&service) {
            None => {
                by_service.insert(service, Value::String(profile));
            }
            Some(Value::Array(list)) => list.push(Value::String(profile)),
            Some(first) => *first = Value::Array(vec![first.clone(), Value::String(profile)]),
        }
    }
    Ok(out)
}
