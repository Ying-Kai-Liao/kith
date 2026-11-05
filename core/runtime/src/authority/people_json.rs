//! `people.listPeople`: `kith-auth people --json` (`cmd_people`) over the tables, the same array, field by field.
//!
//! One object per person, the owner first and then everyone else by id, each person's bots right after them
//! (`ordered`; a loop in `bot_of` is still listed, last). The fields: `id`, `name`, `owner`, `bot_of` (the parent as
//! their id is now, `""` for no one), `provider`, `root` (the person at the top of `bot_of`), `revoked` (the person
//! or the person above a bot is revoked: `cut_off`), `revoked_at`, `kith` (`handles.kith`), `agents` and `handles`
//! (every platform but `kith`). `agents` is `agent_rows`: the person's own rows or the `*` row (never a bot's), as
//! they stand, a removed person's too, and none for the owner.
//!
//! No person is left out (bots, revoked and temp people appear), and no people is `[]`. A database imported before
//! m0015 has no handles until it is imported again: `kith` is `""` and `handles` is `{}` for those.

use std::collections::BTreeMap;

use kith_kernel::secrets::{self, Ctx};
use serde_json::{Map, Value, json};

use super::write::{people, str_of, truthy};

/// `add` of `ordered`: the person, then each of their bots, once.
fn add(auth: &Value, pid: &str, out: &mut Vec<String>) {
    if out.iter().any(|x| x == pid) {
        return;
    }
    out.push(pid.to_owned());
    for b in bots_of(auth, pid) {
        add(auth, &b, out);
    }
}

/// The ids of `people` in `ordered` order.
fn ordered(auth: &Value) -> Vec<String> {
    let all = people(auth);
    let mut top: Vec<&String> = all
        .keys()
        .filter(|p| secrets::parent_of(auth, p).is_none_or(|parent| !all.contains_key(&parent)))
        .collect();
    top.sort_by(|a, b| {
        let key = |p: &String| (!truthy(all.get(p).and_then(|v| v.get("owner"))), p.clone());
        key(a).cmp(&key(b))
    });
    let mut out: Vec<String> = Vec::new();
    for p in top {
        add(auth, p, &mut out);
    }
    // a loop in bot_of: still listed
    let mut rest: Vec<&String> = all.keys().filter(|p| !out.contains(p)).collect();
    rest.sort();
    out.extend(rest.into_iter().cloned());
    out
}

/// `bots_of`: the bots a person owns, sorted.
fn bots_of(auth: &Value, person: &str) -> Vec<String> {
    let mut out: Vec<String> = people(auth)
        .iter()
        .filter(|(_, v)| {
            str_of(v, "bot_of")
                .is_some_and(|b| !b.is_empty() && secrets::canonical(auth, b) == person)
        })
        .map(|(b, _)| b.clone())
        .collect();
    out.sort();
    out
}

/// `root_of`: follow `bot_of` to the person at the top; a loop stops at the repeat.
fn root_of(auth: &Value, person: &str) -> String {
    let mut person = person.to_owned();
    let mut seen = vec![person.clone()];
    while let Some(parent) = secrets::parent_of(auth, &person) {
        if seen.contains(&parent) {
            break;
        }
        seen.push(parent.clone());
        person = parent;
    }
    person
}

/// `agent_rows`: `[{agent, role, via}]` from the person's membership rows as they stand.
fn agent_rows(auth: &Value, c: &Ctx, person: &str) -> Vec<Value> {
    if secrets::is_owner(auth, person) {
        return Vec::new();
    }
    let bot = truthy(people(auth).get(person).and_then(|p| p.get("bot_of")));
    let members: BTreeMap<&String, &Value> = auth
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .collect();
    let mut out = Vec::new();
    for (agent, rows) in members {
        if c.is_builtin(agent) {
            continue;
        }
        let (row, via) = match rows.get(person) {
            Some(r) => (Some(r), "own"),
            None => (rows.get("*"), "everyone"),
        };
        let Some(row) = row else { continue };
        if bot && via == "everyone" {
            continue;
        }
        out.push(json!({
            "agent": agent,
            "role": str_of(row, "preset").unwrap_or("guest"),
            "via": via,
        }));
    }
    out
}

/// The array `kith-auth people --json` prints.
pub(super) fn people_json(auth: &Value, c: &Ctx) -> Value {
    let all = people(auth);
    let mut out = Vec::new();
    for pid in ordered(auth) {
        let p = &all[&pid];
        let handles = p
            .get("handles")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let others: Map<String, Value> = handles
            .iter()
            .filter(|(k, _)| k.as_str() != "kith")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.push(json!({
            "id": pid,
            "name": str_of(p, "name").unwrap_or(""),
            "owner": truthy(p.get("owner")),
            "bot_of": secrets::parent_of(auth, &pid).unwrap_or_default(),
            "provider": str_of(p, "provider").unwrap_or(""),
            "root": root_of(auth, &pid),
            "revoked": secrets::cut_off(auth, &pid),
            "revoked_at": p.get("revoked").and_then(|r| r.get("at")).cloned().unwrap_or(Value::Null),
            "kith": handles.get("kith").cloned().unwrap_or_else(|| json!("")),
            "agents": agent_rows(auth, c, &pid),
            "handles": others,
        }));
    }
    Value::Array(out)
}
