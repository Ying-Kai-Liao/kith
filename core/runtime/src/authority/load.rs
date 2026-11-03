//! Test-only: auth.json -> core.db, for the equality checks. The real import is `kith migrate` (M2-8); it will decide what
//! to do with an auth.json this loader refuses (a field of the wrong type), where this one stops the test.

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::db::{self, MIGRATIONS};

/// A fresh in-memory database at the latest schema.
pub(super) fn empty() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open");
    db::apply(&mut conn, MIGRATIONS, Some("test-db")).expect("apply");
    conn
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).map(|x| x.as_str().expect("a string field").to_owned())
}

fn j(v: &Value, k: &str) -> Option<String> {
    v.get(k).map(Value::to_string)
}

fn b(v: &Value, k: &str) -> Option<i64> {
    v.get(k).map(|x| i64::from(x.as_bool().expect("a bool field")))
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => panic!("loader: unexpected shape for a flag"),
    }
}

/// Loads the tables the access decision reads. Fields of auth.json it does not read (profiles, grants, env, invites,
/// retired, once; a person's handles, root, provider, kith, agents) are not loaded.
pub(super) fn load(conn: &Connection, auth: &Value) {
    if let Some(people) = auth.get("people").and_then(Value::as_object) {
        for (id, p) in people {
            let revoked = truthy(p.get("revoked"));
            let at = p
                .get("revoked")
                .and_then(|r| r.get("at"))
                .and_then(Value::as_i64);
            conn.execute(
                "INSERT INTO people (id, name, lang, tz, owner, bot_of, revoked, revoked_at, was) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    s(p, "name"),
                    s(p, "lang"),
                    s(p, "tz"),
                    i64::from(truthy(p.get("owner"))),
                    s(p, "bot_of"),
                    i64::from(revoked),
                    at,
                    j(p, "was"),
                ],
            )
            .expect("insert person");
        }
    }
    if let Some(members) = auth.get("members").and_then(Value::as_object) {
        for (agent, rows) in members {
            for (person, r) in rows.as_object().expect("rows") {
                conn.execute(
                    "INSERT INTO memberships (agent, person, preset, mode, trusted, deny, model, env, helpers, budget, \
                     access, jobs, helper_once) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        agent,
                        person,
                        s(r, "preset"),
                        s(r, "mode"),
                        b(r, "trusted"),
                        j(r, "deny"),
                        s(r, "model"),
                        j(r, "env"),
                        j(r, "helpers"),
                        s(r, "budget"),
                        j(r, "access"),
                        j(r, "jobs"),
                        j(r, "helper_once"),
                    ],
                )
                .expect("insert membership");
            }
        }
    }
    if let Some(presets) = auth.get("presets").and_then(Value::as_object) {
        for (name, p) in presets {
            conn.execute(
                "INSERT INTO presets (name, mode, trusted, deny) VALUES (?1, ?2, ?3, ?4)",
                params![name, s(p, "mode"), b(p, "trusted"), j(p, "deny")],
            )
            .expect("insert preset");
        }
    }
    if let Some(cap) = auth.get("owner_bots_budget") {
        conn.execute(
            "INSERT INTO authority_settings (key, value) VALUES ('owner_bots_budget', ?1)",
            [cap.to_string()],
        )
        .expect("insert setting");
    }
}
