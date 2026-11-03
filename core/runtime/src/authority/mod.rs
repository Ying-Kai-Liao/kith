//! Authority, the read half (M2-1): who may use which agent, and with what settings, answered from core.db.
//!
//! The rules are `kith_kernel::secrets` (the Rust port of `kernel/bin/kith-auth`'s `may`, `resolve`, `resolve_bot` and
//! `cut_off`, which `kernel/tests/difftest` holds equal to the Python). This module does not carry a second copy: each
//! call reads core.db in ONE read transaction into the same shape auth.json has (`snapshot`), then asks those rules.
//! So the answers can differ from the file's only where the tables differ; `tests.rs` loads generated auth.json data
//! into core.db and checks that.
//!
//! Nothing is switched. `kith-auth` and the host still decide from auth.json; no production code calls this module and
//! nothing writes its tables yet (M2-2 writes them, M2-8 imports auth.json). The `people.*` rows stay `NotServed`.

use kith_kernel::secrets::{self, Ctx, Rules};
use rusqlite::{Connection, Row};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

#[cfg(test)]
mod load;
#[cfg(test)]
mod tests;

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
            .map_err(refused)
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
            .map_err(refused)
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
            .map_err(refused)
    }

    /// `cut_off`: whether the person, or the person a bot of theirs answers to, is revoked.
    pub(crate) fn cut_off(&self, person: &str) -> Result<bool> {
        let auth = snapshot(self.conn)?;
        Ok(secrets::cut_off(&auth, person))
    }
}

/// A rule that stopped with a refusal `kith-auth` would `die` with (a membership naming a preset nobody defined): the
/// tables are not in a state the rules can answer from, so the caller gets no answer, which it must read as "no".
fn refused(d: secrets::Die) -> Error {
    Error::Corrupt(format!("authority: {}", d.msg))
}

fn bad(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("core.db: authority {what}: {e}"))
}

fn text(r: &Row<'_>, i: usize) -> Result<Option<String>> {
    Ok(r.get::<_, Option<String>>(i)?)
}

fn json(r: &Row<'_>, i: usize, what: &str) -> Result<Option<Value>> {
    match text(r, i)? {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| bad(what, e)),
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
    let tx = conn.unchecked_transaction()?;
    let mut people = Map::new();
    {
        let mut q = tx.prepare(
            "SELECT id, name, lang, tz, owner, bot_of, revoked, revoked_at, was FROM people ORDER BY seq",
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
            people.insert(r.get::<_, String>(0)?, Value::Object(p));
        }
    }
    let mut members: Map<String, Value> = Map::new();
    {
        let mut q = tx.prepare(
            "SELECT agent, person, preset, mode, trusted, deny, model, env, helpers, budget, access, jobs, helper_once \
             FROM memberships ORDER BY agent, person",
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
            set(&mut row, "helper_once", json(r, 12, "memberships.helper_once")?);
            let agent: String = r.get(0)?;
            let rows = members
                .entry(agent)
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(rows) = rows {
                rows.insert(r.get::<_, String>(1)?, Value::Object(row));
            }
        }
    }
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
    let mut auth = Map::new();
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
    if let Some(cap) = cap {
        auth.insert(
            "owner_bots_budget".to_owned(),
            serde_json::from_str(&cap).map_err(|e| bad("owner_bots_budget", e))?,
        );
    }
    Ok(Value::Object(auth))
}
