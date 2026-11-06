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
//! |   `handles` (an object, `kith` in it)      |   `handles` (JSON text, the file's order)             |
//! | `members.<agent>.<person>`                  | `memberships (agent, person)`; a `null` row is REFUSED (it means no row and no `*` fallback) |
//! |   `preset` `mode` `model` `budget`          |   text columns                                       |
//! |   `trusted`                                 |   `trusted` 0/1                                      |
//! |   `deny` `env` `helpers` `access` `jobs` `helper_once` `access_by` `access_since` | JSON text columns, order kept |
//! | `presets.<name>` `mode` `trusted` `deny`    | `presets`                                            |
//! | `invites.<code>` `person` `expires`         | `invites` (every code, expired ones too: the owner's next write prunes them) |
//! | `retired.<id>` `at`                         | `retired`                                            |
//! | `agent_access.<agent>` `domains` `since` `by` | `agent_access` (JSON text columns; a `null` row is skipped, it keeps nothing) |
//! | `owner_bots_budget`                         | `authority_settings ('owner_bots_budget')` as JSON   |
//! | `profiles.<name>` (every one, vars or not)  | `secret_profiles` row                                |
//! |   `person`, `with`, `browser`               |   `person` (as `owner_of` answers it, below), `lent` (JSON list), `browser` |
//! |   every other key (VAR -> source)           |   `secret_vars (profile, var, source)`: the reference (`keychain:..`, `env:..`, `env-file:..`), never a value |
//! | `grants.<agent>.<service>` (`coder`, `coder@<person>`) | `secret_grants` rows, `seq` = the place in a list of profiles |
//!
//! A profile's `person` is what `kith-auth`'s `owner_of` answers: the named person; `*` (shared on purpose) is NULL; with
//! none named, the owner's id (the text `the owner` while the file has no owner) when the service is personal (its
//! `"personal"` or else a `login`/`browser` `kind`, read from the kernel's `services/` then this Mac's), else NULL. The
//! core has no service specs of its own, so this is worked out here, once; a file whose service spec cannot be read is
//! refused. A variable's source must be a reference (a plain value could be a secret and `core.db` keeps no plaintext)
//! or the host-store mark `store: host`; anything else refuses the file, naming the profile and variable, never the value.
//!
//! A key that is absent stays NULL (the reader leaves the key out, so the rules' default applies); a key that is JSON
//! `null` is read as absent. A key of the wrong type refuses the whole file (`Error::Refused`) rather than guess.
//!
//! Left behind, on purpose (the report lists which of these the file at hand has):
//! - `env`: the plaintext values agents' env keeps in auth.json. No table holds them (m0021 has profiles, variables and
//!   grants only) and a plaintext value does not belong in core.db; they move with kith-auth itself (M2-18).
//!   A `keychain:` reference in a profile has its Keychain item sealed into the envelope (`items.rs`).
//! - `once`: one-time secrets that live at most an hour; they are not worth a copy.
//! - a person's `root`, `kith` (it is in `handles`), `agents`: derived by `kith-auth people --json`; the decision reads
//!   none of them. (`handles` is kept since m0015: `people.listPeople`, `retired` and `--rm --unused` read it.)
//! - any other key, at any level: not read by the decision, so not copied.
//!
//! Not imported here at all: cards (PR #711) and access requests (M2-3).

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// Keys of a person that are imported; the rest are listed as left behind.
const PERSON_KEYS: &[&str] = &[
    "name", "lang", "tz", "owner", "bot_of", "revoked", "was", "provider", "handles",
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
    "profiles",
    "grants",
];
/// Top-level tables left behind by design.
const LEFT_TOP: &[&str] = &["env", "once"];
/// The keys of a profile that are not variables.
const PROFILE_KEYS: &[&str] = &["person", "with", "browser"];

/// Where the service specs are, in `kith-auth`'s order: the kernel's `services/`, then this Mac's.
pub(crate) struct Services(Vec<std::path::PathBuf>);

impl Services {
    pub(crate) fn new(kernel: &std::path::Path, machine: &std::path::Path) -> Self {
        Self(vec![kernel.join("services"), machine.join("services")])
    }

    /// No service specs: nothing is personal.
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self(Vec::new())
    }

    /// `kith-auth`'s `personal(svc)`: the spec's `"personal"`, else whether its `kind` is `login` or `browser`; a service
    /// with no spec is not personal. A spec that cannot be read or is not an object is an error, as it is for kith-auth.
    fn personal(&self, svc: &str) -> Result<bool> {
        let name = format!("{svc}.json");
        if svc.is_empty() || svc.contains(['/', '\\']) || svc == ".." || svc == "." {
            return Ok(false);
        }
        for dir in &self.0 {
            let path = dir.join(&name);
            if !path.is_file() {
                continue;
            }
            let spec: Value = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .filter(Value::is_object)
                .ok_or_else(|| {
                    bad(
                        &format!("the service spec of {svc}"),
                        "cannot be read (kith-auth cannot read it either), so its profiles' owners are unknown",
                    )
                })?;
            return Ok(match spec.get("personal") {
                Some(v) => py_truthy(v),
                None => matches!(
                    spec.get("kind").and_then(Value::as_str),
                    Some("login" | "browser")
                ),
            });
        }
        Ok(false)
    }
}

/// Python's truth of a JSON value.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

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

/// A person's `handles`: an object (JSON text) or absent; any other type refuses the file.
fn handles(row: &Value, what: &str) -> Result<Option<String>> {
    match row.get("handles") {
        None | Some(Value::Null) => Ok(None),
        Some(h @ Value::Object(_)) => Ok(Some(h.to_string())),
        Some(_) => Err(bad(&format!("{what}.handles"), "is not an object")),
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

/// One `secret_profiles` row and its `secret_vars`.
struct ProfileRow {
    name: String,
    person: Option<String>,
    lent: Option<String>,
    browser: Option<String>,
    vars: Vec<(String, String)>,
}

/// A source `kith-auth` can resolve (`resolve_ref`), or the host-store mark; the only values a profile may hold.
fn is_reference(var: &str, source: &str) -> bool {
    ["keychain:", "env:", "env-file:"]
        .iter()
        .any(|k| source.starts_with(k))
        || (var == "store" && source == "host")
}

/// `profiles` as rows. The owner is the first person flagged `owner`, as `kith-auth`'s `owner()` takes it.
fn parse_profiles(auth: &Value, services: &Services) -> Result<Vec<ProfileRow>> {
    let owner = table(auth, "people")?
        .into_iter()
        .flatten()
        .find(|(_, p)| truthy(p.get("owner"), "owner").unwrap_or(false))
        .map(|(id, _)| id.clone());
    let mut out = Vec::new();
    for (name, p) in table(auth, "profiles")?.into_iter().flatten() {
        let what = format!("profiles.{name}");
        let p = object(p, &what)?;
        let row = Value::Object(p.clone());
        let named = text(&row, "person", &what)?.filter(|s| !s.is_empty());
        let person = match named.as_deref() {
            Some("*") => None,
            Some(_) => named,
            None if services.personal(name.split('/').next().unwrap_or(name))? => {
                Some(owner.clone().unwrap_or_else(|| "the owner".to_owned()))
            }
            None => None,
        };
        let lent = match p.get("with") {
            None | Some(Value::Null) => None,
            Some(w @ Value::Array(l)) if l.iter().all(Value::is_string) => Some(w.to_string()),
            Some(_) => return Err(bad(&format!("{what}.with"), "is not a list of people")),
        };
        let mut vars = Vec::new();
        for (var, source) in p {
            if PROFILE_KEYS.contains(&var.as_str()) {
                continue;
            }
            let source = match source {
                // Not set: `kith-auth` reads a null or empty source as no variable.
                Value::Null => continue,
                Value::String(s) if s.is_empty() => continue,
                Value::String(s) => s,
                _ => return Err(bad(&format!("{what}.{var}"), "is not text")),
            };
            if !is_reference(var, source) {
                return Err(bad(
                    &format!("{what}.{var}"),
                    "is not a keychain:, env: or env-file: reference (core.db keeps no plain value; its value is not shown)",
                ));
            }
            vars.push((var.clone(), source.clone()));
        }
        out.push(ProfileRow {
            name: name.clone(),
            person,
            lent,
            browser: text(&row, "browser", &what)?,
            vars,
        });
    }
    Ok(out)
}

/// `grants` as `(agent, service, profile, seq)`: a list of profiles keeps its order in `seq`. A null or empty grant is
/// no grant (`kith-auth` reads it so), and a profile named twice in a list once.
fn parse_grants(auth: &Value) -> Result<Vec<(String, String, String, i64)>> {
    let mut out = Vec::new();
    for (agent, by_service) in table(auth, "grants")?.into_iter().flatten() {
        if by_service.is_null() {
            continue;
        }
        let what = format!("grants.{agent}");
        for (service, g) in object(by_service, &what)? {
            let what = format!("{what}.{service}");
            let list: Vec<&str> = match g {
                Value::Null => continue,
                Value::String(s) => vec![s.as_str()],
                Value::Array(l) => {
                    let list: Vec<&str> = l
                        .iter()
                        .map(|p| {
                            p.as_str()
                                .ok_or_else(|| bad(&what, "is not a profile or a list of profiles"))
                        })
                        .collect::<Result<_>>()?;
                    // `kith-auth` takes the first entry of a list as the grant, so an empty one cannot be dropped
                    // without the core granting what the file does not.
                    if list.iter().any(|p| p.is_empty()) {
                        return Err(bad(&what, "is a list with an empty entry"));
                    }
                    list
                }
                _ => return Err(bad(&what, "is not a profile or a list of profiles")),
            };
            let mut seen = std::collections::BTreeSet::new();
            let mut seq = 0;
            for profile in list.into_iter().filter(|p| !p.is_empty()) {
                if seen.insert(profile) {
                    out.push((agent.clone(), service.clone(), profile.to_owned(), seq));
                    seq += 1;
                }
            }
        }
    }
    Ok(out)
}

/// A file the import can store faithfully, or the reason it cannot. Run before anything is written.
pub(super) fn refuse_what_it_cannot_keep(auth: &Value, services: &Services) -> Result<()> {
    object(auth, "the file")?;
    // The owner is the one person a revoke cannot reach (`kith-auth` and the core both refuse to make a revoked person the
    // owner, #780), so a file that says both has been edited by hand or written by an older version. Which of the two it
    // means is not for the import to guess.
    for (id, p) in table(auth, "people")?.into_iter().flatten() {
        if truthy(p.get("owner"), "owner").unwrap_or(false)
            && truthy(p.get("revoked"), "revoked").unwrap_or(false)
        {
            return Err(Error::Refused(format!(
                "auth.json: {id} is both the owner and revoked; the file cannot be imported as it is. Remove the person \
                 (kith-auth person {id} --rm) and add {id} again as the owner, or give the ownership to someone else, and run it \
                 again (the core's undo cannot clear this for an owner). Nothing was imported."
            )));
        }
    }
    // `agent_access` is kept (m0013). A wrong-typed row is refused like any other table; a null row or a null scalar in it
    // reads as "nothing kept" in `kith-auth`, which is what an absent column gives.
    for (agent, row) in table(auth, "agent_access")?.into_iter().flatten() {
        if row.is_null() {
            continue;
        }
        let what = format!("agent_access.{agent}");
        object(row, &what)?;
        for (key, want_list) in [("domains", true), ("since", false), ("by", false)] {
            match (row.get(key), want_list) {
                (None | Some(Value::Null), _)
                | (Some(Value::Array(_)), true)
                | (Some(Value::Object(_)), false) => {}
                (Some(_), true) => return Err(bad(&format!("{what}.{key}"), "is not a list")),
                (Some(_), false) => return Err(bad(&format!("{what}.{key}"), "is not an object")),
            }
        }
    }
    // `profiles` and `grants` are kept (m0021): a wrong shape, or a variable that could be a plain secret, refuses the file.
    parse_profiles(auth, services)?;
    parse_grants(auth)?;
    Ok(())
}

/// Writes the tables from `auth` into `conn` (which holds none of these rows). Any wrong shape is `Refused`.
pub(super) fn insert_all(conn: &Connection, auth: &Value, services: &Services) -> Result<()> {
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
            "INSERT INTO people (id, name, lang, tz, owner, bot_of, revoked, revoked_at, was, provider, handles) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                handles(p, &what)?,
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
    for (agent, row) in table(auth, "agent_access")?.into_iter().flatten() {
        if row.is_null() {
            continue;
        }
        conn.execute(
            "INSERT INTO agent_access (agent, domains, since, by) VALUES (?1, ?2, ?3, ?4)",
            params![
                agent,
                json(row, "domains"),
                json(row, "since"),
                json(row, "by")
            ],
        )?;
    }
    if let Some(cap) = auth.get("owner_bots_budget") {
        conn.execute(
            "INSERT INTO authority_settings (key, value) VALUES ('owner_bots_budget', ?1)",
            [cap.to_string()],
        )?;
    }
    for p in parse_profiles(auth, services)? {
        conn.execute(
            "INSERT INTO secret_profiles (profile, person, lent, browser) VALUES (?1, ?2, ?3, ?4)",
            params![p.name, p.person, p.lent, p.browser],
        )?;
        for (var, source) in p.vars {
            conn.execute(
                "INSERT INTO secret_vars (profile, var, source) VALUES (?1, ?2, ?3)",
                params![p.name, var, source],
            )?;
        }
    }
    for (agent, service, profile, seq) in parse_grants(auth)? {
        conn.execute(
            "INSERT INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, ?4)",
            params![agent, service, profile, seq],
        )?;
    }
    Ok(())
}

/// The removals a forced re-import of `auth` makes: each person core.db holds that the file no longer has, with the
/// removed bots under them, as `(id, bots)` in id order. Reads only; refuses what a revoke can not carry out.
///
/// `clear()` deletes every `people` row, so without this a person who is gone from the file would leave no record and the
/// revoke order (`revoke_order`) would never end their claims, asks or bots'. A removed id whose owner stays is a root
/// of its own (a bot gone from the file is a removal); a removed id whose owner is removed too belongs to that owner's
/// set. A bot still in the file (handed over, `bot_of` changed) is not in any set and is not descended through: it stays.
pub(super) fn removals(conn: &Connection, auth: &Value) -> Result<Vec<(String, Vec<String>)>> {
    let keep: std::collections::BTreeSet<&str> = table(auth, "people")?
        .into_iter()
        .flatten()
        .map(|(id, _)| id.as_str())
        .collect();
    let mut st = conn.prepare("SELECT id, bot_of, owner FROM people ORDER BY id")?;
    let rows: Vec<(String, Option<String>, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    // An id in a kept person's `was` is a rename (`kith-auth rename`), not a removal: the person lives on under the new id.
    // Moving its claims and asks to the new id is a follow-up; revoking a live person's work is the wrong failure.
    let renamed: std::collections::BTreeSet<&str> = table(auth, "people")?
        .into_iter()
        .flatten()
        .filter_map(|(_, p)| p.get("was").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let gone: std::collections::BTreeMap<&str, Option<&str>> = rows
        .iter()
        .filter(|(id, ..)| !keep.contains(id.as_str()) && !renamed.contains(id.as_str()))
        .map(|(id, bot_of, _)| (id.as_str(), bot_of.as_deref()))
        .collect();
    if let Some((id, ..)) = rows
        .iter()
        .find(|(id, _, owner)| *owner == 1 && gone.contains_key(id.as_str()))
    {
        return Err(Error::Refused(format!(
            "auth.json: the owner {id} is in core.db and not in the file; removing the owner is never right, so nothing was \
             imported. Put {id} back in the file (as the owner) and run it again."
        )));
    }
    // While an open removal names an id, `people.addPerson` and `people.addBot` refuse it; the import must not put it back.
    for id in &keep {
        if let Some(who) = crate::revoke_order::open_removal_naming(conn, id)? {
            return Err(Error::Refused(format!(
                "auth.json: {id} is in the file but the removal of {who} (which names {id}) has not finished in core.db (the import just tried the order and it did not complete); nothing \
                 was imported. Start the core so it runs the order again (it does at start), or take {id} out of the file, and run it again."
            )));
        }
    }
    let mut out = Vec::new();
    for (&id, &owner) in &gone {
        if owner.is_some_and(|o| gone.contains_key(o)) {
            continue;
        }
        let mut bots = Vec::new();
        let mut frontier = vec![id];
        while let Some(o) = frontier.pop() {
            for (&b, _) in gone.iter().filter(|(_, bo)| **bo == Some(o)) {
                if b != id && !bots.iter().any(|x: &String| x == b) {
                    bots.push(b.to_owned());
                    frontier.push(b);
                }
            }
        }
        bots.sort();
        out.push((id.to_owned(), bots));
    }
    // A `bot_of` cycle among the gone ids (A and B each the other's bot, or a bot of itself) has no root, so no row would
    // name it and nothing would be revoked. Never skip: refuse.
    let covered: std::collections::BTreeSet<&str> = out
        .iter()
        .flat_map(|(id, bots)| std::iter::once(id.as_str()).chain(bots.iter().map(String::as_str)))
        .collect();
    let lost: Vec<&str> = gone
        .keys()
        .copied()
        .filter(|id| !covered.contains(id))
        .collect();
    if !lost.is_empty() {
        return Err(Error::Refused(format!(
            "auth.json: core.db's people {} are gone from the file and form a bot_of cycle, so no removal can name them; \
             nothing was imported. Break the cycle in core.db or keep them in the file, and run it again.",
            lost.join(", ")
        )));
    }
    Ok(out)
}

/// Writes the open removal for each of [`removals`]'s ids, in the caller's transaction, before `clear()`. Returns the ids.
pub(super) fn record_removals(conn: &Connection, auth: &Value, now_ms: f64) -> Result<Vec<String>> {
    let found = removals(conn, auth)?;
    for (id, bots) in &found {
        crate::revoke_order::record_removal(conn, id, bots, now_ms)?;
    }
    Ok(found.into_iter().map(|(id, _)| id).collect())
}

/// The people the file has suspended and core.db holds as not suspended (a missed suspend mirror). Reads only. The owner
/// (an old file may still mark one revoked) and bots are skipped: the order refuses the owner, so a row would replay
/// forever. A person new to core.db is not listed: nothing of theirs is in the core to sweep. Only the file's current id
/// is read, not its `was` ids (a rename is the rename follow-up's).
pub(super) fn suspends(conn: &Connection, auth: &Value) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (id, p) in table(auth, "people")?.into_iter().flatten() {
        if !truthy(p.get("revoked"), &format!("people.{id}.revoked"))? {
            continue;
        }
        if truthy(p.get("owner"), &format!("people.{id}.owner"))?
            || p.get("bot_of").is_some_and(|b| !b.is_null())
        {
            continue;
        }
        // core.db's owner is never listed, whatever the file says of them.
        let held: Option<(i64, i64)> = conn
            .query_row(
                "SELECT revoked, owner FROM people WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if held == Some((0, 0)) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Writes the open record for each of [`suspends`]'s ids, in the caller's transaction, before `clear()`. Returns the ids.
pub(super) fn record_suspends(conn: &Connection, auth: &Value, now_ms: f64) -> Result<Vec<String>> {
    let found = suspends(conn, auth)?;
    for id in &found {
        crate::revoke_order::record_suspend(conn, id, now_ms)?;
    }
    Ok(found)
}

/// Empties the tables `insert_all` fills (a forced re-import). `authority_settings` keeps any key it does not own.
pub(super) fn clear(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM people; DELETE FROM memberships; DELETE FROM presets; DELETE FROM invites; DELETE FROM retired; DELETE FROM agent_access; \
         DELETE FROM secret_profiles; DELETE FROM secret_vars; DELETE FROM secret_grants; DELETE FROM secret_stale; \
         DELETE FROM authority_settings WHERE key IN ('owner_bots_budget', 'handles_stale');",
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
        "env" => {
            "agent env values (plaintext in auth.json today) have no table in core.db and move with kith-auth (M2-18); never copied here"
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
        "{} people, {} memberships, {} presets, {} invites, {} retired, {} agent_access, {} profiles, {} grants",
        n("people")?,
        n("memberships")?,
        n("presets")?,
        n("invites")?,
        n("retired")?,
        n("agent_access")?,
        n("secret_profiles")?,
        n("secret_grants")?
    ))
}
