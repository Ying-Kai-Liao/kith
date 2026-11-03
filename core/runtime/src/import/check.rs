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
/// How many agents are tried as `via` besides none (the helper chain's caller).
const MAX_VIA: usize = 8;

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
    }
    for extra in ["ghost", "*", ""] {
        v.insert(extra.to_owned());
    }
    v.into_iter().collect()
}

fn agents(auth: &Value, ctx: &Ctx) -> Vec<String> {
    let mut v = BTreeSet::new();
    v.extend(
        auth.get("members")
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
    let mut vias: Vec<Option<&str>> = vec![None];
    vias.extend(agents.iter().take(MAX_VIA).map(|a| Some(a.as_str())));
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
