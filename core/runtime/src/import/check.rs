//! The same-answer check: for every person (and bot) x agent x `via` x once, `kith_kernel::secrets::Rules` over the
//! original auth.json value and `authority::Authority` over core.db must say the same thing (`may`, `resolve`,
//! `resolve_bot`, `cut_off`), refusal words included. Any difference fails the import.

use std::collections::BTreeSet;

use kith_kernel::secrets::{self, Ctx, Rules, Sandbox};
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::authority::Authority;
use crate::error::Result;

/// How many differences are collected before the check stops looking: the first few say what is wrong.
const MAX_DIFFS: usize = 20;

fn said<T>(r: std::result::Result<T, secrets::Die>) -> std::result::Result<T, String> {
    r.map_err(|d| d.msg)
}

fn got<T>(r: Result<T>) -> std::result::Result<T, String> {
    r.map_err(|e| e.to_string().trim_start_matches("authority: ").to_owned())
}

/// Folder names under `root` (agents live in `<root>/<name>`).
fn dirs(root: &std::path::Path, out: &mut BTreeSet<String>) {
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        if e.path().is_dir()
            && let Some(n) = e.file_name().to_str()
        {
            out.insert(n.to_owned());
        }
    }
}

/// The ids a question is asked about: everyone the file names anywhere, plus ones it does not.
fn people(auth: &Value) -> Vec<String> {
    let mut v = BTreeSet::new();
    let keys = |t: &str| {
        auth.get(t)
            .and_then(Value::as_object)
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    v.extend(keys("people"));
    v.extend(keys("retired"));
    for (_, rows) in auth
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        v.extend(rows.as_object().into_iter().flat_map(Map::keys).cloned());
    }
    for (_, p) in auth
        .get("people")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if let Some(b) = p.get("bot_of").and_then(Value::as_str) {
            v.insert(b.to_owned());
        }
        // Old ids the person answers to (`canonical` follows them).
        strings(p.get("was"), &mut v);
    }
    for (_, i) in auth
        .get("invites")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        strings(i.get("person"), &mut v);
    }
    for extra in ["ghost", "*", ""] {
        v.insert(extra.to_owned());
    }
    v.into_iter().collect()
}

/// Every text in `v` (a string, or the strings of an array / the keys and strings of an object, any depth).
fn strings(v: Option<&Value>, out: &mut BTreeSet<String>) {
    match v {
        Some(Value::String(s)) => {
            out.insert(s.clone());
        }
        Some(Value::Array(a)) => a.iter().for_each(|x| strings(Some(x), out)),
        Some(Value::Object(o)) => {
            for (k, x) in o {
                out.insert(k.clone());
                strings(Some(x), out);
            }
        }
        _ => {}
    }
}

/// Names the rows give agents (helpers, jobs, helpers a job lists) even where no folder exists for them.
fn named_by_rows(auth: &Value) -> BTreeSet<String> {
    let mut v = BTreeSet::new();
    for (_, rows) in auth
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (_, r) in rows.as_object().into_iter().flatten() {
            for k in ["helpers", "jobs", "helper_once"] {
                strings(r.get(k), &mut v);
            }
        }
    }
    v
}

/// Whether an agent's package declares helpers or jobs (the only agents a `via` can matter for).
fn has_helpers_or_jobs(ctx: &Ctx, agent: &str) -> bool {
    [
        ctx.agents.join(agent),
        ctx.kernel.join("agents").join(agent),
    ]
    .iter()
    .flat_map(|d| {
        [
            d.join("role/capabilities.yaml"),
            d.join("capabilities.yaml"),
        ]
    })
    .any(|f| std::fs::read_to_string(f).is_ok_and(|t| t.contains("helpers") || t.contains("jobs")))
}

fn agents(auth: &Value, ctx: &Ctx) -> Vec<String> {
    let mut v = named_by_rows(auth);
    v.extend(
        auth.get("members")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(Map::keys)
            .cloned(),
    );
    v.extend(
        auth.get("agent_access")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(Map::keys)
            .cloned(),
    );
    dirs(&ctx.agents, &mut v);
    dirs(&ctx.kernel.join("agents"), &mut v);
    v.insert("scratch".to_owned());
    v.insert("nope".to_owned());
    v.into_iter().collect()
}

/// The differences between the file's answers and core.db's, at most [`MAX_DIFFS`]; empty means the same.
pub(super) fn diffs(auth: &Value, conn: &Connection, ctx: &Ctx) -> Result<Vec<String>> {
    // One clock and no ambient person or sandbox probe, so both sides are asked the same question.
    let mut ctx = ctx.clone();
    ctx.person = None;
    ctx.sandbox = Sandbox::Fixed(false);
    let core = Authority::new(conn, &ctx);
    let rules = || Rules::new(&ctx, auth);
    let agents = agents(auth, &ctx);
    // A `via` only changes an answer for an agent that can call helpers; every such agent is tried, and every agent a row
    // names as a helper (their folders may be missing).
    let named = named_by_rows(auth);
    let mut vias: Vec<Option<&str>> = vec![None];
    vias.extend(
        agents
            .iter()
            .filter(|a| named.contains(*a) || has_helpers_or_jobs(&ctx, a))
            .map(|a| Some(a.as_str())),
    );
    let mut out = Vec::new();
    for p in people(auth) {
        let (a, b) = (secrets::cut_off(auth, &p), core.cut_off(&p)?);
        if a != b {
            out.push(format!("cut_off({p:?}): file {a}, core {b}"));
        }
        for agent in &agents {
            for via in &vias {
                for once in [false, true] {
                    let q = format!("({p:?}, {agent:?}, {via:?}, once {once})");
                    let (a, b) = (
                        said(rules().may(&p, agent, *via, once)),
                        got(core.may(&p, agent, *via, once)),
                    );
                    if a != b {
                        out.push(format!("may{q}: file {a:?}, core {b:?}"));
                    }
                    let (a, b) = (
                        said(rules().resolve(agent, &p, *via, once)),
                        got(core.resolve(agent, &p, *via, once)),
                    );
                    if a != b {
                        out.push(format!("resolve{q}: file {a:?}, core {b:?}"));
                    }
                    let id = secrets::canonical(auth, &p);
                    let want = secrets::parent_of(auth, &id)
                        .is_some()
                        .then(|| said(rules().resolve(agent, &id, *via, once)));
                    let have = match core.resolve_bot(agent, &p, *via, once) {
                        Ok(None) => None,
                        Ok(Some(m)) => Some(Ok(m)),
                        Err(e) => Some(got::<Map<String, Value>>(Err(e))),
                    };
                    if want != have {
                        out.push(format!("resolve_bot{q}: file {want:?}, core {have:?}"));
                    }
                    if out.len() >= MAX_DIFFS {
                        return Ok(out);
                    }
                }
            }
        }
    }
    Ok(out)
}
