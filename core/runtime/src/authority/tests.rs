//! Equality of the core.db decision with the auth.json one. `kith_kernel::secrets` over an auth.json value is what
//! `kernel/tests/difftest` holds equal to `kernel/bin/kith-auth`; here the same questions are asked of a core.db loaded
//! with the same data, and the answers must be the same, refusal words included. Generated tables (the way
//! `kernel/tests/kith-auth-props.py` generates them) plus fixed cases, plus the access invariants of runtime-design
//! section 9 checked on the core.db answers themselves.
#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use std::path::Path;

use kith_kernel::secrets::{self, Ctx, Rules, Sandbox, ServeDir};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{Authority, snapshot};
use crate::authority::load::{empty, load};

mod card;
mod cards;
mod gaps;
mod guest;
mod requests;
mod revoke;
mod router;
mod runs;
mod writes;

const NOW: f64 = 1_800_000_000_000.0;
const FUTURE: i64 = 4_102_444_800_000;

fn ctx(root: &Path) -> Ctx {
    Ctx {
        auth: root.join("secrets/auth.json"),
        agents: root.join("agents"),
        kernel: root.join("kernel"),
        person: None,
        now_ms: NOW,
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(root.join("serve")),
    }
}

/// A machine whose kernel ships the built-in agent `scratch` and whose user has the others.
fn machine() -> (tempfile::TempDir, Ctx) {
    let t = tempfile::tempdir().unwrap();
    let scratch = t.path().join("kernel/agents/scratch/role");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("agent.md"), "---\nname: scratch\n---\n").unwrap();
    for a in ["house", "garden", "sound"] {
        let d = t.path().join("agents").join(a).join("role");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
    }
    let house = t.path().join("agents/house/role/capabilities.yaml");
    std::fs::write(
        house,
        "helpers: [garden]\njobs:\n  - name: work\n    domains: [a.example]\n    helpers: [garden]\n",
    )
    .unwrap();
    let c = ctx(t.path());
    (t, c)
}

fn db_of(auth: &Value) -> Connection {
    let conn = empty();
    load(&conn, auth);
    conn
}

const AGENTS: [&str; 5] = ["house", "garden", "scratch", "sound", "nope"];

/// Every id a question is asked about: the people, some that are not, the everyone row, the owner's blank, an old id.
fn ids(auth: &Value) -> Vec<String> {
    let mut v: Vec<String> = auth["people"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for extra in ["ghost", "*", "", "old-sam"] {
        if !v.iter().any(|x| x == extra) {
            v.push(extra.to_owned());
        }
    }
    v
}

fn said<T>(r: Result<T, secrets::Die>) -> Result<T, String> {
    r.map_err(|d| d.msg)
}

fn got<T>(r: crate::error::Result<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string().trim_start_matches("authority: ").to_owned())
}

/// Asks everything of both sides; the first difference is the error. On success the access invariants hold too.
fn same(auth: &Value, c: &Ctx) -> Result<(), String> {
    let conn = db_of(auth);
    let core = Authority::new(&conn, c);
    let people = ids(auth);
    for p in &people {
        let (a, b) = (secrets::cut_off(auth, p), core.cut_off(p).unwrap());
        if a != b {
            return Err(format!("cut_off({p:?}): file {a}, core {b}"));
        }
        for agent in AGENTS {
            for via in [None, Some("house"), Some("garden"), Some("sound")] {
                for once in [false, true] {
                    let q = format!("({p:?}, {agent:?}, {via:?}, once {once})");
                    let (a, b) = (
                        said(Rules::new(c, auth).may(p, agent, via, once)),
                        got(core.may(p, agent, via, once)),
                    );
                    if a != b {
                        return Err(format!("may{q}: file {a:?}, core {b:?}"));
                    }
                    let (a, b) = (
                        said(Rules::new(c, auth).resolve(agent, p, via, once)),
                        got(core.resolve(agent, p, via, once)),
                    );
                    if a != b {
                        return Err(format!("resolve{q}: file {a:?}, core {b:?}"));
                    }
                    let id = secrets::canonical(auth, p);
                    let want = if secrets::parent_of(auth, &id).is_some() {
                        Some(said(Rules::new(c, auth).resolve(agent, &id, via, once)))
                    } else {
                        None
                    };
                    let have = match core.resolve_bot(agent, p, via, once) {
                        Ok(None) => None,
                        Ok(Some(m)) => Some(Ok(m)),
                        Err(e) => Some(got::<serde_json::Map<String, Value>>(Err(e))),
                    };
                    if want != have {
                        return Err(format!("resolve_bot{q}: file {want:?}, core {have:?}"));
                    }
                    let (allowed, _) =
                        got(core.may(p, agent, via, once)).unwrap_or((false, String::new()));
                    invariants(auth, &core, p, agent, via, once, allowed)?;
                }
            }
        }
    }
    Ok(())
}

/// Runtime-design section 9, the part about access, on what the core says.
fn invariants(
    auth: &Value,
    core: &Authority<'_>,
    p: &str,
    agent: &str,
    via: Option<&str>,
    once: bool,
    allowed: bool,
) -> Result<(), String> {
    let id = secrets::canonical(auth, p);
    let owner = secrets::is_owner(auth, &id);
    let known = auth["people"].get(&id).is_some();
    if !allowed {
        return Ok(());
    }
    if owner {
        return Ok(());
    }
    // unknown = no
    if !known {
        return Err(format!("{p:?} is no person but may use {agent}"));
    }
    // a revoked person, or a bot of one, reaches nothing, whatever rows they hold
    if core.cut_off(&id).unwrap() {
        return Err(format!("{p:?} is cut off but may use {agent}"));
    }
    // the kernel's own agents are the owner's alone
    if agent == "scratch" {
        return Err(format!("{p:?} may use the built-in scratch"));
    }
    // a bot never exceeds its owner: it may use an agent only while its owner may, right now
    if let Some(parent) = secrets::parent_of(auth, &id) {
        let (ok, why) = core.may(&parent, agent, via, false).unwrap();
        if !ok {
            return Err(format!(
                "bot {p:?} may use {agent}, its owner {parent} may not: {why}"
            ));
        }
        if !secrets::is_owner(auth, &parent) {
            // `allowed` was asked with `once`, so the bot is asked with it too; a refused answer has no helpers, deny or
            // budget to compare (a bot let in by a "just this once" is not let in without it)
            let (Ok(mine), Ok(theirs)) = (
                core.resolve(agent, &id, via, once),
                core.resolve(agent, &parent, via, false),
            ) else {
                return Ok(());
            };
            // `may` allowed this bot (with `once`) and its owner, so `resolve` must too
            if mine["allowed"] != Value::Bool(true) || theirs["allowed"] != Value::Bool(true) {
                return Err(format!(
                    "may/resolve disagree for {p:?} on {agent}: {:?} / {:?}",
                    mine["allowed"], theirs["allowed"]
                ));
            }
            let sub = |k: &str| {
                let have = mine[k].as_array().cloned().unwrap_or_default();
                let cap = theirs[k].as_array().cloned().unwrap_or_default();
                have.iter().all(|x| cap.contains(x))
            };
            if !sub("helpers") {
                return Err(format!("bot {p:?} has helpers its owner lacks"));
            }
            let deny = theirs["deny"].as_array().cloned().unwrap_or_default();
            if !deny
                .iter()
                .all(|x| mine["deny"].as_array().is_some_and(|d| d.contains(x)))
            {
                return Err(format!("bot {p:?} lost a deny of its owner"));
            }
            if mine["budget"] != theirs["budget"] {
                return Err(format!(
                    "bot {p:?} spends from another budget than its owner"
                ));
            }
        }
    }
    Ok(())
}

fn fixture() -> Value {
    json!({
        "people": {
            "kai": {"owner": true, "name": "Kai"},
            "sam": {"name": "Sam", "lang": "en", "tz": "Australia/Sydney", "was": ["samuel", "old-sam"]},
            "ola": {"name": "Ola", "revoked": {"at": 7}},
            "bot-s": {"bot_of": "sam", "was": ["bot-old"]},
            "bot-o": {"bot_of": "ola"},
            "bot-k": {"bot_of": "kai"},
            "bot-x": {"bot_of": "gone"},
            "lone": {},
        },
        "presets": {"pro": {"mode": "acceptEdits", "deny": ["Bash(rm *)"]}, "free": {"mode": "auto"},
                     "trusting": {"mode": "auto", "trusted": true}},
        "members": {
            "house": {
                "sam": {"preset": "pro", "model": "haiku", "deny": ["Edit"], "helpers": ["garden"], "budget": "20",
                        "env": {"A": "1"}, "access": {"read": ["/a"], "write": ["/w"], "domains": ["x.com"]},
                        "jobs": {"work": {"snapshot": {"domains": ["a.example"], "helpers": ["garden"]}}},
                        "helper_once": {"sound": FUTURE, "gone": 5}},
                "ola": {"preset": "pro"},
                "bot-s": {"env": {"B": "2"}, "access": {"read": ["/a/b", "/zzz"], "write": [], "domains": ["x.com", "y.com"]}},
                "bot-o": {},
                "bot-k": {"model": "opus"},
                "bot-x": {},
                "kai": {"preset": "pro"},
            },
            "garden": {"*": {"preset": "member"}, "lone": {"mode": "bypassPermissions"},
                         "sam": {"preset": "free"}, "bot-s": {}},
            "sound": {"sam": {"preset": "trusting"}},
            "scratch": {"*": {}, "sam": {}},
        },
        "owner_bots_budget": "50",
    })
}

#[test]
fn core_db_answers_as_the_file_does_on_a_fixed_table() {
    let (_t, c) = machine();
    same(&fixture(), &c).unwrap();
}

#[test]
fn an_empty_core_db_answers_as_an_empty_auth_json() {
    let (_t, c) = machine();
    same(&json!({}), &c).unwrap();
    let conn = empty();
    let core = Authority::new(&conn, &c);
    // no owner yet: nobody is a person, only the blank id (the owner's) gets in
    assert!(core.may("", "house", None, false).unwrap().0);
    assert!(!core.may("sam", "house", None, false).unwrap().0);
    assert!(!core.cut_off("sam").unwrap());
}

#[test]
fn the_named_cases() {
    let (_t, c) = machine();
    let conn = db_of(&fixture());
    let core = Authority::new(&conn, &c);
    let may = |p, a| core.may(p, a, None, false).unwrap();
    // owner, member row, everyone row, no row
    assert_eq!(may("kai", "house"), (true, "owner".into()));
    assert_eq!(may("sam", "house"), (true, "member".into()));
    assert_eq!(may("lone", "garden").1, "member");
    assert_eq!(may("sam", "sound").1, "member");
    // revoked: refused before any row, the `*` row included; their bot is dormant with them
    assert!(core.cut_off("ola").unwrap() && core.cut_off("bot-o").unwrap());
    assert_eq!(
        may("ola", "house"),
        (false, "ola's access was removed".into())
    );
    assert!(!may("bot-o", "house").0);
    // unknown
    assert!(!may("ghost", "garden").0);
    // a bot whose owner is gone, and a bot of the owner
    assert_eq!(
        may("bot-x", "house"),
        (false, "bot-x's owner 'gone' is gone".into())
    );
    assert_eq!(may("bot-k", "house"), (true, "bot of kai".into()));
    // an old id answers as the person it became
    assert_eq!(may("old-sam", "house").1, "member");
    // the built-in agent is the owner's alone, whatever `*` row it holds
    assert!(!may("sam", "scratch").0 && may("kai", "scratch").0);
    // a bot never gets the `*` row: bot-k has no row of its own on garden
    assert!(!may("bot-k", "garden").0);
    // a free-shell mode without trusted runs as default, and says what was asked
    let r = core.resolve("garden", "lone", None, false).unwrap();
    assert_eq!(r["mode"], "default");
    assert_eq!(r["downgraded"], "bypassPermissions");
    let r = core.resolve("sound", "sam", None, false).unwrap();
    assert_eq!(r["mode"], "auto");
    assert_eq!(r["trusted"], true);
    // a bot: its owner's budget, a guest preset, its grants cut to its owner's
    let r = core
        .resolve_bot("house", "bot-s", None, false)
        .unwrap()
        .unwrap();
    assert_eq!(r["preset"], "guest");
    assert_eq!(r["budget"], "20");
    assert_eq!(r["access"]["read"], json!(["/a/b"]));
    assert_eq!(r["access"]["domains"], json!(["x.com"]));
    assert!(
        core.resolve_bot("house", "sam", None, false)
            .unwrap()
            .is_none()
    );
    // the owner's bots are capped together by owner_bots_budget
    let r = core
        .resolve_bot("house", "bot-k", None, false)
        .unwrap()
        .unwrap();
    assert_eq!(r["budget"], "50");
}

#[test]
fn a_missing_field_reads_as_kith_auth_defaults() {
    let (_t, c) = machine();
    // a row with nothing in it, a person with nothing in it: the older-version shapes
    let auth =
        json!({"people": {"kai": {"owner": true}, "sam": {}}, "members": {"house": {"sam": {}}}});
    same(&auth, &c).unwrap();
    let conn = db_of(&auth);
    let r = Authority::new(&conn, &c)
        .resolve("house", "sam", None, false)
        .unwrap();
    assert_eq!(r["preset"], "guest");
    assert_eq!(r["mode"], "default");
    assert_eq!(r["model"], "");
    assert_eq!(r["budget"], "");
    assert_eq!(r["env"], json!({}));
    assert_eq!(r["was"], json!([]));
    assert!(r.get("name").is_none());
}

#[test]
fn an_undefined_preset_is_the_same_refusal() {
    let (_t, c) = machine();
    let auth = json!({"people": {"kai": {"owner": true}, "sam": {}},
                      "members": {"house": {"sam": {"preset": "nope"}}}});
    same(&auth, &c).unwrap();
    let conn = db_of(&auth);
    let e = Authority::new(&conn, &c)
        .resolve("house", "sam", None, false)
        .unwrap_err();
    assert!(e.to_string().contains("no preset 'nope'"), "{e}");
}

#[test]
fn a_loop_of_bots_is_refused_alike() {
    let (_t, c) = machine();
    let auth = json!({"people": {"kai": {"owner": true}, "a": {"bot_of": "b"}, "b": {"bot_of": "a"}},
                      "members": {"house": {"a": {}, "b": {}}}});
    same(&auth, &c).unwrap();
}

#[test]
fn people_keep_the_order_of_auth_json() {
    // `owner` and `canonical` take the first match, so the order is part of the answer
    let (_t, c) = machine();
    let auth = json!({"people": {"z": {"owner": true}, "a": {"owner": true}, "m": {"was": ["x"]}, "b": {"was": ["x"]}}});
    let conn = db_of(&auth);
    assert_eq!(snapshot(&conn).unwrap()["people"], auth["people"]);
    same(&auth, &c).unwrap();
}

#[test]
fn a_call_reads_one_snapshot() {
    // A write that lands after the call has started reading is not seen by it: the call sees the tables as they were
    // when its transaction began, never a person revoked in one table and not yet in the next.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("core.db");
    let mut w = Connection::open(&path).unwrap();
    w.pragma_update(None, "journal_mode", "WAL").unwrap();
    crate::db::apply(&mut w, crate::db::MIGRATIONS, Some("t")).unwrap();
    load(
        &w,
        &json!({"people": {"kai": {"owner": true}, "sam": {}}, "members": {"house": {"sam": {}}}}),
    );
    let r = Connection::open(&path).unwrap();
    let tx = r.unchecked_transaction().unwrap();
    let before: i64 = tx
        .query_row("SELECT count(*) FROM people WHERE revoked = 1", [], |x| {
            x.get(0)
        })
        .unwrap();
    w.execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
        .unwrap();
    let still: i64 = tx
        .query_row("SELECT count(*) FROM people WHERE revoked = 1", [], |x| {
            x.get(0)
        })
        .unwrap();
    assert_eq!(
        (before, still),
        (0, 0),
        "the open read transaction keeps its snapshot"
    );
    drop(tx);
    // a call that starts after the commit sees it
    assert!(
        snapshot(&r).unwrap()["people"]["sam"]
            .get("revoked")
            .is_some()
    );
}

#[test]
fn the_schema_holds_the_tables_and_nothing_foreign() {
    let conn = empty();
    for t in ["people", "memberships", "presets", "authority_settings"] {
        conn.prepare(&format!("SELECT * FROM {t}")).unwrap();
    }
    // no SQL foreign key: a bot whose owner is gone, a row for an id that is no person, must load
    for t in ["people", "memberships", "presets", "authority_settings"] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM pragma_foreign_key_list('{t}')"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{t}");
    }
}

// --- generated tables ---------------------------------------------------------------------------------------------

const WHO: [&str; 7] = ["kai", "sam", "amy", "bot-a", "bot-b", "ghost", "*"];
const MEMBERS: [&str; 4] = ["house", "garden", "scratch", "sound"];

fn pick<T: Clone + std::fmt::Debug + 'static>(xs: &'static [T]) -> impl Strategy<Value = T> {
    prop::sample::select(xs)
}

fn some<T: std::fmt::Debug>(s: impl Strategy<Value = T>) -> impl Strategy<Value = Option<T>> {
    prop::option::weighted(0.4, s)
}

fn strs(xs: &'static [&'static str]) -> impl Strategy<Value = Vec<&'static str>> {
    prop::collection::vec(pick(xs), 0..3)
}

type Row = (
    Option<&'static str>,
    Option<&'static str>,
    Option<bool>,
    Option<Vec<&'static str>>,
    Option<&'static str>,
    Option<bool>,
    Option<Vec<&'static str>>,
    Option<&'static str>,
    Option<(Vec<&'static str>, Vec<&'static str>, Vec<&'static str>)>,
    Option<bool>,
    Option<(bool, bool)>,
);

fn row() -> impl Strategy<Value = Row> {
    (
        some(pick(&["guest", "member", "pro", "empty", "nope"])),
        some(pick(&[
            "default",
            "acceptEdits",
            "auto",
            "bypassPermissions",
        ])),
        some(any::<bool>()),
        some(strs(&["Bash", "Edit"])),
        some(pick(&["haiku", "opus"])),
        some(any::<bool>()),
        some(strs(&["house", "garden", "sound"])),
        some(pick(&["7", "20"])),
        some((
            strs(&["/a", "/a/b"]),
            strs(&["/w", "/a"]),
            strs(&["x.com", "y.com"]),
        )),
        some(any::<bool>()),
        some((any::<bool>(), any::<bool>())),
    )
}

fn row_value(r: &Row) -> Value {
    let mut o = serde_json::Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            o.insert(k.to_owned(), v);
        }
    };
    put("preset", r.0.map(Value::from));
    put("mode", r.1.map(Value::from));
    put("trusted", r.2.map(Value::from));
    put("deny", r.3.as_ref().map(|v| json!(v)));
    put("model", r.4.map(Value::from));
    put(
        "env",
        r.5.map(|b| if b { json!({"FOO": "1"}) } else { json!({}) }),
    );
    put("helpers", r.6.as_ref().map(|v| json!(v)));
    put("budget", r.7.map(Value::from));
    put(
        "access",
        r.8.as_ref()
            .map(|(a, b, c)| json!({"read": a, "write": b, "domains": c})),
    );
    put(
        "jobs",
        r.9.map(|b| {
            if b {
                json!({"work": {"snapshot": {"domains": ["a.example"], "helpers": ["garden"]}}})
            } else {
                json!({})
            }
        }),
    );
    put("helper_once", r.10.map(|(a, b)| json!({"garden": if a { FUTURE } else { 5 }, "sound": if b { FUTURE } else { 5 }})));
    Value::Object(o)
}

type Person = (u8, Option<&'static str>, Option<Vec<&'static str>>, bool);

fn person() -> impl Strategy<Value = Option<Person>> {
    prop::option::weighted(
        0.8,
        (
            0u8..3,
            some(pick(&[
                "kai", "sam", "amy", "bot-a", "bot-b", "gone", "old-sam",
            ])),
            some(strs(&["old-sam", "old-amy", "old-b"])),
            any::<bool>(),
        ),
    )
}

fn auth_value(
    people: &[Option<Person>],
    rows: &[Option<Row>],
    presets: &(
        Option<&'static str>,
        Option<bool>,
        Option<Vec<&'static str>>,
    ),
    cap: Option<&'static str>,
) -> Value {
    let mut p = serde_json::Map::new();
    p.insert("kai".to_owned(), json!({"owner": true}));
    for (id, spec) in WHO[1..5].iter().zip(people) {
        let Some((revoked, bot_of, was, named)) = spec else {
            continue;
        };
        let mut o = serde_json::Map::new();
        if *named {
            o.insert("name".to_owned(), (*id).into());
        }
        // a person's id as their bot's owner, or any other id: hand edits and removed owners happen
        if id.starts_with("bot")
            && let Some(b) = bot_of
        {
            o.insert("bot_of".to_owned(), (*b).into());
        }
        if let Some(w) = was {
            o.insert("was".to_owned(), json!(w));
        }
        match revoked {
            1 => {
                o.insert("revoked".to_owned(), json!({"at": 7}));
            }
            2 => {
                o.insert("revoked".to_owned(), json!(true));
            }
            _ => {}
        }
        p.insert((*id).to_owned(), Value::Object(o));
    }
    let mut members = serde_json::Map::new();
    let mut it = rows.iter();
    for agent in MEMBERS {
        let mut m = serde_json::Map::new();
        for who in WHO {
            if let Some(Some(r)) = it.next() {
                m.insert(who.to_owned(), row_value(r));
            }
        }
        members.insert(agent.to_owned(), Value::Object(m));
    }
    let mut pre = serde_json::Map::new();
    pre.insert("empty".to_owned(), json!({}));
    let mut pro = serde_json::Map::new();
    if let Some(m) = presets.0 {
        pro.insert("mode".to_owned(), m.into());
    }
    if let Some(t) = presets.1 {
        pro.insert("trusted".to_owned(), t.into());
    }
    if let Some(d) = &presets.2 {
        pro.insert("deny".to_owned(), json!(d));
    }
    pre.insert("pro".to_owned(), Value::Object(pro));
    let mut out = json!({"people": p, "members": members, "presets": pre});
    if let Some(c) = cap {
        out["owner_bots_budget"] = c.into();
    }
    out
}

fn auth_strategy() -> impl Strategy<Value = Value> {
    (
        prop::collection::vec(person(), 4),
        prop::collection::vec(
            prop::option::weighted(0.3, row()),
            MEMBERS.len() * WHO.len(),
        ),
        (
            some(pick(&["default", "acceptEdits", "auto"])),
            some(any::<bool>()),
            some(strs(&["Bash", "Edit"])),
        ),
        some(pick(&["50", "0"])),
        // kept hosts, so `people.agentAccess` has rows to add to and to take away from
        prop::collection::vec(
            prop::option::weighted(0.4, strs(&["a.example.com", "b.example.com"])),
            MEMBERS.len(),
        ),
    )
        .prop_map(|(people, rows, presets, cap, kept)| {
            let mut auth = auth_value(&people, &rows, &presets, cap);
            let mut access = serde_json::Map::new();
            for (agent, domains) in MEMBERS.iter().zip(kept) {
                if let Some(mut d) = domains.filter(|d| !d.is_empty()) {
                    d.sort_unstable();
                    d.dedup();
                    let since: serde_json::Map<_, _> =
                        d.iter().map(|h| ((*h).to_owned(), json!(5))).collect();
                    let by: serde_json::Map<_, _> =
                        d.iter().map(|h| ((*h).to_owned(), Value::Null)).collect();
                    access.insert(
                        (*agent).to_owned(),
                        json!({"domains": d, "since": since, "by": by}),
                    );
                }
            }
            auth["agent_access"] = Value::Object(access);
            auth
        })
}

#[test]
fn generated_tables_answer_the_same_from_core_db() {
    let (_t, c) = machine();
    let mut config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    let mut runner = TestRunner::new(config);
    runner
        .run(&auth_strategy(), |auth| {
            same(&auth, &c).map_err(TestCaseError::fail)
        })
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_bot_let_in_by_a_just_this_once_is_checked_with_the_once() {
    // The flaky case of the generated test (CI run 41696953934, attempt 1): the invariants asked `may` with `once`
    // and then `resolve` without it, which answers "not allowed" and has no `helpers` key to index.
    let (_t, c) = machine();
    let auth = json!({"people": {"kai": {"owner": true}, "sam": {}, "bot-a": {"bot_of": "sam"}},
                      "members": {"house": {"sam": {"helpers": ["garden"]},
                                            "bot-a": {"helper_once": {"garden": FUTURE}}}}});
    assert_eq!(same(&auth, &c), Ok(()));
}

#[test]
fn a_rule_refusal_is_not_a_reason_to_restore_a_backup() {
    // `kith-auth preset X --rm` leaves the rows that name X behind: the rules cannot answer, but nothing is damaged
    let (_t, c) = machine();
    let auth = json!({"people": {"kai": {"owner": true}, "sam": {}},
                      "members": {"house": {"sam": {"preset": "gone"}}}});
    let conn = db_of(&auth);
    let e = Authority::new(&conn, &c)
        .resolve("house", "sam", None, false)
        .unwrap_err();
    assert!(matches!(e, crate::error::Error::Refused(_)), "{e}");
    assert_eq!(e.recovery(), "kith doctor");
    // text in a JSON column that is not JSON is the same kind of refusal
    conn.execute("UPDATE memberships SET deny = 'not json'", [])
        .unwrap();
    let e = Authority::new(&conn, &c)
        .may("sam", "house", None, false)
        .unwrap_err();
    assert!(matches!(e, crate::error::Error::Refused(_)), "{e}");
}

#[test]
fn every_table_is_read_from_one_snapshot() {
    // A writer commits between the reads of two tables: the later read must not see it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("core.db");
    let mut w = Connection::open(&path).unwrap();
    w.pragma_update(None, "journal_mode", "WAL").unwrap();
    crate::db::apply(&mut w, crate::db::MIGRATIONS, Some("t")).unwrap();
    load(
        &w,
        &json!({"people": {"kai": {"owner": true}, "sam": {}},
                "members": {"house": {"sam": {}}}, "presets": {"pro": {"mode": "auto"}}}),
    );
    let r = Connection::open(&path).unwrap();
    let mut stage = 0;
    let seen = super::snapshot_with(&r, &mut || {
        stage += 1;
        match stage {
            1 => {
                w.execute("DELETE FROM memberships", []).unwrap();
            }
            2 => {
                w.execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
                    .unwrap();
                w.execute("DELETE FROM presets", []).unwrap();
            }
            _ => {}
        }
    })
    .unwrap();
    assert_eq!(stage, 3);
    assert!(seen["members"]["house"].get("sam").is_some(), "{seen}");
    assert!(seen["presets"].get("pro").is_some(), "{seen}");
    assert!(seen["people"]["sam"].get("revoked").is_none(), "{seen}");
    // the next call sees all of it
    let after = snapshot(&r).unwrap();
    assert!(after["members"].get("house").is_none());
    assert!(after["people"]["sam"].get("revoked").is_some());
}
