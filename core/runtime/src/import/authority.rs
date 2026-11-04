//! auth.json -> the authority tables of core.db (m0010, m0011), key by key. The mapping is the contract; the same-answer
//! check (`check.rs`) is what proves it holds for the file at hand.
//!
//! | auth.json                                   | core.db                                              |
//! |---------------------------------------------|------------------------------------------------------|
//! | `people.<id>` (in file order)               | `people` row, `seq` in that order                    |
//! |   `name` `lang` `tz` `bot_of` `provider`   |   same-named text columns                            |
//! |   `owner` (truthy: true, or a non-empty obj)|   `owner` 0/1                                        |
//! |   `revoked` (truthy), `revoked.at`          |   `revoked` 0/1, `revoked_at`                        |
//! |   `was`                                     |   `was` (JSON text)                                  |
//! | `members.<agent>.<person>`                  | `memberships (agent, person)`; a `null` row is REFUSED (it means no row and no `*` fallback) |
//! |   `preset` `mode` `model` `budget`          |   text columns                                       |
//! |   `trusted`                                 |   `trusted` 0/1                                      |
//! |   `deny` `env` `helpers` `access` `jobs` `helper_once` `access_by` `access_since` | JSON text columns, order kept |
//! | `presets.<name>` `mode` `trusted` `deny`    | `presets`                                            |
//! | `invites.<code>` `person` `expires`         | `invites` (every code, expired ones too: the owner's next write prunes them) |
//! | `retired.<id>` `at`                         | `retired`                                            |
//! | `owner_bots_budget`                         | `authority_settings ('owner_bots_budget')` as JSON   |
//!
//! A key that is absent stays NULL (the reader leaves the key out, so the rules' default applies); a key that is JSON
//! `null` is read as absent. A key of the wrong type refuses the whole file (`Error::Refused`) rather than guess.
//!
//! Left behind, on purpose (the report lists which of these the file at hand has):
//! - `profiles`, `grants`, `env`: the secrets side. A `keychain:` reference in a profile has its Keychain item sealed into
//!   the envelope (`items.rs`); the profile table, the grants and the plaintext values `env` keeps in auth.json move with
//!   kith-auth itself (M2-18). The decision reads none of them.
//! - `once`: one-time secrets that live at most an hour; they are not worth a copy.
//! - `agent_access`: the rules (`kith_kernel::secrets`) do not add its domains to a run's access yet where `kith-auth`
//!   does, so importing it would store a different answer. A file that keeps any domain there is REFUSED (Later: the
//!   rules learn `agent_domains`, then the import learns the table).
//! - a person's `handles`, `root`, `kith`, `agents`: channel and cloud links the host and `kith-people` keep; the decision
//!   reads none of them.
//! - any other key, at any level: not read by the decision, so not copied.
//!
//! Not imported here at all: cards (PR #711) and access requests (M2-3).

use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// Keys of a person that are imported; the rest are listed as left behind.
const PERSON_KEYS: &[&str] = &[
    "name", "lang", "tz", "owner", "bot_of", "revoked", "was", "provider",
];
const MEMBER_KEYS: &[&str] = &[
    "preset",
    "mode",
    "trusted",
    "deny",
    "model",
    "env",
    "helpers",
    "budget",
    "access",
    "jobs",
    "helper_once",
    "access_by",
    "access_since",
];
const TOP_KEYS: &[&str] = &[
    "people",
    "members",
    "presets",
    "invites",
    "retired",
    "owner_bots_budget",
    "agent_access",
];
/// Top-level tables left behind by design.
const LEFT_TOP: &[&str] = &["profiles", "grants", "env", "once"];

fn bad(what: &str, why: &str) -> Error {
    Error::Refused(format!("auth.json: {what} {why}; nothing was imported"))
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| bad(what, "is not an object"))
}

/// The table `key` of the file: absent or null is empty.
fn table<'a>(auth: &'a Value, key: &str) -> Result<Option<&'a Map<String, Value>>> {
    match auth.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => object(v, key).map(Some),
    }
}

fn text(row: &Value, key: &str, what: &str) -> Result<Option<String>> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad(&format!("{what}.{key}"), "is not text")),
    }
}

fn flag(row: &Value, key: &str, what: &str) -> Result<Option<i64>> {
    match row.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(i64::from(*b))),
        Some(_) => Err(bad(&format!("{what}.{key}"), "is not true or false")),
    }
}

fn json(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.to_string()),
    }
}

/// Python's truth for the flags kith-auth tests with `if p.get('owner')`: a missing, null, false or empty value is no.
fn truthy(v: Option<&Value>, what: &str) -> Result<bool> {
    match v {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Object(o)) => Ok(!o.is_empty()),
        Some(_) => Err(bad(what, "is not true, false or an object")),
    }
}

/// A file the import can store faithfully, or the reason it cannot. Run before anything is written.
pub(super) fn refuse_what_it_cannot_keep(auth: &Value) -> Result<()> {
    object(auth, "the file")?;
    if let Some(access) = table(auth, "agent_access")? {
        for (agent, row) in access {
            let kept = row
                .get("domains")
                .and_then(Value::as_array)
                .is_some_and(|d| !d.is_empty());
            if kept {
                return Err(Error::Refused(format!(
                    "auth.json: agent_access keeps domains for {agent}; the Rust rules do not add them to a run's access yet, so \
                     importing would store a different answer than kith-auth gives. Nothing was imported. (Later: the rules learn \
                     agent_domains, then this import takes the table.)"
                )));
            }
        }
    }
    Ok(())
}

/// Writes the tables from `auth` into `conn` (which holds none of these rows). Any wrong shape is `Refused`.
pub(super) fn insert_all(conn: &Connection, auth: &Value) -> Result<()> {
    for (id, p) in table(auth, "people")?.into_iter().flatten() {
        let what = format!("people.{id}");
        object(p, &what)?;
        let revoked = truthy(p.get("revoked"), &format!("{what}.revoked"))?;
        let at = match p.get("revoked").and_then(|r| r.get("at")) {
            None | Some(Value::Null) => None,
            Some(a) => Some(
                a.as_i64()
                    .ok_or_else(|| bad(&format!("{what}.revoked.at"), "is not a whole number"))?,
            ),
        };
        conn.execute(
            "INSERT INTO people (id, name, lang, tz, owner, bot_of, revoked, revoked_at, was, provider) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                text(p, "name", &what)?,
                text(p, "lang", &what)?,
                text(p, "tz", &what)?,
                i64::from(truthy(p.get("owner"), &format!("{what}.owner"))?),
                text(p, "bot_of", &what)?,
                i64::from(revoked),
                at,
                json(p, "was"),
                text(p, "provider", &what)?,
            ],
        )?;
    }
    for (agent, rows) in table(auth, "members")?.into_iter().flatten() {
        let rows = object(rows, &format!("members.{agent}"))?;
        for (person, r) in rows {
            let what = format!("members.{agent}.{person}");
            // A null row means "no row, and no `*` fallback" to kith-auth; a NULL-column row would read as an empty row
            // that falls back. The tables cannot say it, so the file is refused rather than imported as another answer.
            if r.is_null() {
                return Err(bad(&what, "is null, which core.db cannot keep faithfully"));
            }
            object(r, &what)?;
            conn.execute(
                "INSERT INTO memberships (agent, person, preset, mode, trusted, deny, model, env, helpers, budget, \
                 access, jobs, helper_once, access_by, access_since) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    agent,
                    person,
                    text(r, "preset", &what)?,
                    text(r, "mode", &what)?,
                    flag(r, "trusted", &what)?,
                    json(r, "deny"),
                    text(r, "model", &what)?,
                    json(r, "env"),
                    json(r, "helpers"),
                    text(r, "budget", &what)?,
                    json(r, "access"),
                    json(r, "jobs"),
                    json(r, "helper_once"),
                    json(r, "access_by"),
                    json(r, "access_since"),
                ],
            )?;
        }
    }
    for (name, p) in table(auth, "presets")?.into_iter().flatten() {
        let what = format!("presets.{name}");
        object(p, &what)?;
        conn.execute(
            "INSERT INTO presets (name, mode, trusted, deny) VALUES (?1, ?2, ?3, ?4)",
            params![
                name,
                text(p, "mode", &what)?,
                flag(p, "trusted", &what)?,
                json(p, "deny")
            ],
        )?;
    }
    for (code, i) in table(auth, "invites")?.into_iter().flatten() {
        let what = format!("invites.{code}");
        object(i, &what)?;
        let person = text(i, "person", &what)?
            .ok_or_else(|| bad(&format!("{what}.person"), "is missing"))?;
        // A code without a time is one kith-auth's save drops as expired (`expires` defaults to 0 there).
        let expires = match i.get("expires") {
            None | Some(Value::Null) => 0.0,
            Some(e) => e
                .as_f64()
                .ok_or_else(|| bad(&format!("{what}.expires"), "is not a number"))?,
        };
        conn.execute(
            "INSERT INTO invites (code, person, expires) VALUES (?1, ?2, ?3)",
            params![code, person, expires],
        )?;
    }
    for (id, r) in table(auth, "retired")?.into_iter().flatten() {
        let at = match r.get("at") {
            None | Some(Value::Null) => None,
            Some(a) => Some(
                a.as_i64()
                    .ok_or_else(|| bad(&format!("retired.{id}.at"), "is not a whole number"))?,
            ),
        };
        conn.execute(
            "INSERT INTO retired (id, at) VALUES (?1, ?2)",
            params![id, at],
        )?;
    }
    if let Some(cap) = auth.get("owner_bots_budget") {
        conn.execute(
            "INSERT INTO authority_settings (key, value) VALUES ('owner_bots_budget', ?1)",
            [cap.to_string()],
        )?;
    }
    Ok(())
}

/// Empties the tables `insert_all` fills (a forced re-import). `authority_settings` keeps any key it does not own.
pub(super) fn clear(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM people; DELETE FROM memberships; DELETE FROM presets; DELETE FROM invites; DELETE FROM retired; \
         DELETE FROM authority_settings WHERE key = 'owner_bots_budget';",
    )?;
    Ok(())
}

/// What the file has that the import does not copy, one line each (counts and names of keys, never values).
pub(super) fn left_behind(auth: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(top) = auth.as_object() else {
        return out;
    };
    for key in LEFT_TOP {
        let n = match top.get(*key) {
            Some(Value::Object(o)) => o.len(),
            _ => 0,
        };
        if n > 0 {
            out.push(format!("left behind: {key} ({n} entries): {}", why(key)));
        }
    }
    let mut unknown: Vec<&str> = top
        .keys()
        .map(String::as_str)
        .filter(|k| !TOP_KEYS.contains(k) && !LEFT_TOP.contains(k))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        out.push(format!(
            "left behind: other top-level keys {unknown:?}: the decision reads none of them"
        ));
    }
    let mut extra_person = std::collections::BTreeSet::new();
    for (_, p) in top
        .get("people")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for k in p.as_object().into_iter().flat_map(Map::keys) {
            if !PERSON_KEYS.contains(&k.as_str()) {
                extra_person.insert(k.clone());
            }
        }
    }
    if !extra_person.is_empty() {
        out.push(format!(
            "left behind: person keys {extra_person:?} (channel and cloud links the host keeps; the decision reads none)"
        ));
    }
    let mut extra_member = std::collections::BTreeSet::new();
    for (_, rows) in top
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (_, r) in rows.as_object().into_iter().flatten() {
            for k in r.as_object().into_iter().flat_map(Map::keys) {
                if !MEMBER_KEYS.contains(&k.as_str()) {
                    extra_member.insert(k.clone());
                }
            }
        }
    }
    if !extra_member.is_empty() {
        out.push(format!(
            "left behind: membership keys {extra_member:?}: not part of the decision's rows"
        ));
    }
    out
}

fn why(key: &str) -> &'static str {
    match key {
        "profiles" => {
            "the profile table moves with kith-auth (M2-18); keychain: references have their items sealed separately"
        }
        "grants" => "which profiles an agent may use moves with kith-auth (M2-18)",
        "env" => {
            "agent env values (plaintext in auth.json today) move with kith-auth (M2-18); never copied here"
        }
        _ => "one-time secrets expire within the hour",
    }
}

/// A short count line for the log and the report.
pub(super) fn counts(conn: &Connection) -> Result<String> {
    let n = |t: &str| -> Result<i64> {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))?)
    };
    Ok(format!(
        "{} people, {} memberships, {} presets, {} invites, {} retired",
        n("people")?,
        n("memberships")?,
        n("presets")?,
        n("invites")?,
        n("retired")?
    ))
}
