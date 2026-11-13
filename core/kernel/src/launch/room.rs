//! C0 of rooms with people: the grant of a "room person" (`room-<id>`), the MEET of every current person's
//! `kith-auth resolve` answer for the agent. While a room has any person, the agent's turns in it run on this grant, so
//! nobody in the room can be answered with more than they could get alone (design rule R5).
//!
//! `room_grant` is a pure function of the answers. `resolve_basis` runs `kith-auth resolve` through the router (the same
//! call `assemble` makes, so it works with the authority switched or not) and feeds it. Nothing calls either in the live
//! launch path yet (PR C does).
//!
//! The rules, field by field (a person's answer is the widest the meet may be on that field):
//! - mode: the strictest. `default` < `acceptEdits`; a free-shell mode (`bypassPermissions`, `auto`) counts as
//!   `acceptEdits` and is never the result; any other mode is not understood, so the launch is refused.
//! - deny: the union. helpers: the intersection. trusted: AND. access (read, write, domains): the intersection, where a
//!   folder survives as the narrower of two nested ones (the cover `held` in kith-auth gives: write covers read).
//! - model: the agent's own (empty here). jobs, doors, env: empty in C0 (PR D fills them).
//! - the owner's answer (no mode, no helpers: the owner holds everything) limits nothing, except that the room person
//!   is never a free shell.
//! - missing optional fields: kith-auth gives a non-owner `deny`, `helpers` and `access` always, so a missing one means
//!   the answer is not a real one; it is read as the narrowest value (no helpers, no access, no deny), never wider.
//! - fail closed: no basis, an answer that is not allowed, is held (out of sync) or does not parse, is a refusal.

use super::settings::Member;
use crate::secrets;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Why a room person can not launch. Never a partial grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A room person's basis is its people; with none there is nothing to meet.
    NoBasis,
    /// One basis person's answer says no (revoked, suspended, not on the agent, out of sync) or could not be read.
    Person { person: String, why: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoBasis => f.write_str("a room person needs at least one person in the room"),
            Refusal::Person { person, why } => {
                write!(f, "{person} can't use this agent here: {why}")
            }
        }
    }
}

/// What a room person's turn may have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomGrant {
    pub agent: String,
    pub mode: String,
    pub deny: Vec<String>,
    pub helpers: Vec<String>,
    pub trusted: bool,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub domains: Vec<String>,
    /// The agent's own model: left empty so the launch picks the agent's.
    pub model: String,
    /// Empty in C0.
    pub jobs: Vec<String>,
    pub doors: Vec<String>,
    pub env: Map<String, Value>,
}

impl RoomGrant {
    /// The shape the settings read (`Member`): a guest whose mode, deny and trust are the meet's.
    pub fn member(&self, id: &str) -> Member {
        Member {
            owner: false,
            id: id.to_owned(),
            trusted: self.trusted,
            mode: self.mode.clone(),
            deny: self.deny.clone(),
        }
    }
}

const FREE_SHELL: [&str; 2] = ["bypassPermissions", "auto"];

/// 0 for default, 1 for acceptEdits; a free shell is capped to acceptEdits. `None`: not a mode we know.
fn rank(mode: &str) -> Option<u8> {
    match mode {
        "default" => Some(0),
        "acceptEdits" => Some(1),
        m if FREE_SHELL.contains(&m) => Some(1),
        _ => None,
    }
}

fn mode_name(rank: u8) -> &'static str {
    if rank == 0 { "default" } else { "acceptEdits" }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn within(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{}/", root.trim_end_matches('/')))
}

/// The folders both lists cover: for each pair, the narrower when one holds the other.
fn narrow(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for x in a {
        for y in b {
            if within(x, y) {
                out.insert(x.clone());
            } else if within(y, x) {
                out.insert(y.clone());
            }
        }
    }
    out
}

/// One person's answer, read for the meet.
struct Basis {
    owner: bool,
    rank: u8,
    deny: BTreeSet<String>,
    helpers: BTreeSet<String>,
    trusted: bool,
    read: BTreeSet<String>,
    write: BTreeSet<String>,
    domains: BTreeSet<String>,
}

fn read_basis(r: &Value) -> Result<Basis, Refusal> {
    let id = r
        .get("person")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let fail = |why: &str| Refusal::Person {
        person: id.clone(),
        why: why.to_owned(),
    };
    if !r.is_object() {
        return Err(fail("its answer could not be read"));
    }
    if r.get("held")
        .is_some_and(|h| h.as_bool() != Some(false) && !h.is_null())
    {
        return Err(fail("this computer is out of sync with the Kith cloud"));
    }
    if r.get("allowed").and_then(Value::as_bool) != Some(true) {
        let why = r
            .get("why")
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty());
        return Err(fail(why.unwrap_or("not allowed")));
    }
    if r.get("owner").and_then(Value::as_bool) == Some(true) {
        return Ok(Basis {
            owner: true,
            rank: 1,
            deny: BTreeSet::new(),
            helpers: BTreeSet::new(),
            trusted: true,
            read: BTreeSet::new(),
            write: BTreeSet::new(),
            domains: BTreeSet::new(),
        });
    }
    let mode = r.get("mode").and_then(Value::as_str).unwrap_or_default();
    let rank = rank(mode).ok_or_else(|| fail("its permission mode is not one a room can use"))?;
    let access = r.get("access").filter(|a| a.is_object());
    let folders = |k: &str| strings(access.and_then(|a| a.get(k)));
    let write: BTreeSet<String> = folders("write").into_iter().collect();
    // write covers read
    let read: BTreeSet<String> = folders("read")
        .into_iter()
        .chain(write.iter().cloned())
        .collect();
    Ok(Basis {
        owner: false,
        rank,
        deny: strings(r.get("deny")).into_iter().collect(),
        helpers: strings(r.get("helpers")).into_iter().collect(),
        trusted: r.get("trusted").and_then(Value::as_bool) == Some(true),
        read,
        write,
        domains: strings(access.and_then(|a| a.get("domains")))
            .into_iter()
            .collect(),
    })
}

fn meet(
    acc: Option<BTreeSet<String>>,
    next: &BTreeSet<String>,
    both: fn(&BTreeSet<String>, &BTreeSet<String>) -> BTreeSet<String>,
) -> BTreeSet<String> {
    match acc {
        None => next.clone(),
        Some(a) => both(&a, next),
    }
}

fn same(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    a.intersection(b).cloned().collect()
}

/// The meet of the basis people's `kith-auth resolve` answers for `agent`, or why there is none.
pub fn room_grant(agent: &str, basis: &[Value]) -> Result<RoomGrant, Refusal> {
    if basis.is_empty() {
        return Err(Refusal::NoBasis);
    }
    let rows = basis
        .iter()
        .map(read_basis)
        .collect::<Result<Vec<_>, _>>()?;
    let rank = rows.iter().map(|b| b.rank).min().unwrap_or(0);
    let mut deny = BTreeSet::new();
    let mut trusted = true;
    let (mut helpers, mut domains, mut read, mut write) = (None, None, None, None);
    for b in &rows {
        deny.extend(b.deny.iter().cloned());
        trusted &= b.trusted;
        // The owner limits nothing: only the people who are not the owner narrow what is shared.
        if !b.owner {
            helpers = Some(meet(helpers, &b.helpers, same));
            domains = Some(meet(domains, &b.domains, same));
            read = Some(meet(read, &b.read, narrow));
            write = Some(meet(write, &b.write, narrow));
        }
    }
    // A room of owners only has nobody to be wider than; it still gets nothing it was not given by name.
    let list = |s: Option<BTreeSet<String>>| s.unwrap_or_default().into_iter().collect::<Vec<_>>();
    Ok(RoomGrant {
        agent: agent.to_owned(),
        mode: mode_name(rank).to_owned(),
        deny: deny.into_iter().collect(),
        helpers: list(helpers),
        trusted,
        read: list(read),
        write: list(write),
        domains: list(domains),
        model: String::new(),
        jobs: vec![],
        doors: vec![],
        env: Map::new(),
    })
}

/// Runs `kith-auth resolve AGENT` for each person through the router, then `room_grant`. Any call that fails or prints
/// something that is not JSON refuses the whole launch.
pub fn resolve_basis(
    sctx: &secrets::Ctx,
    agent: &str,
    people: &[String],
) -> Result<RoomGrant, Refusal> {
    let mut answers = Vec::with_capacity(people.len());
    for person in people {
        let mut c = sctx.clone();
        c.person = Some(person.clone());
        let o = secrets::run(&c, "resolve", &[agent.to_owned()]);
        let parsed = (o.code == 0)
            .then(|| serde_json::from_str::<Value>(&o.stdout).ok())
            .flatten();
        let Some(r) = parsed else {
            return Err(Refusal::Person {
                person: person.clone(),
                why: "kith-auth resolve failed".to_owned(),
            });
        };
        answers.push(r);
    }
    room_grant(agent, &answers)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "room_tests.rs"]
mod tests;
