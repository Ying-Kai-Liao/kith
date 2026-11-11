//! The card writes `kith-auth` routes here on the authority tombstone (C-a, C-b): the escape card's "always"
//! (`access`), the edge card's `helper` and `job --decline`, and the removals of the three. Each is a port of its
//! `kith-auth` verb, refusals and the words of what it prints included, and runs in the ONE locked step that writes: the
//! checks and the write see the same tables, so a removal or a demotion can not slip between them (C2, M3).
//!
//! A grant here never widens past what the owner would give by hand: the owner and the `*` row are refused, a secret or
//! rule W place is never granted, a bot never holds more than its owner, and `--by` names only the person or the bot's
//! owner, who must be a member now. A removal takes the entry as written, with no checks (like `remove_grant`).

use kith_kernel::secrets::{self, Ctx, Rules};
use serde_json::{Map, Value, json};

use super::revoke::Life;
use super::write::{self, R, die, entry, need_person, usage};

/// `ACCESS_KINDS`: the kind of a card's always, and the field of the row's `access` it lands in.
fn field_of(kind: &str) -> Option<&'static str> {
    match kind {
        "read" => Some("read"),
        "write" => Some("write"),
        "domain" => Some("domains"),
        _ => None,
    }
}

/// `HELPER_ONCE_MS`: a "just this once" waits this long for its launch.
const HELPER_ONCE_MS: f64 = 300_000.0;

#[allow(clippy::cast_possible_truncation)]
fn stamp(c: &Ctx) -> i64 {
    c.now_ms as i64
}

fn is_agent_name(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn row_of<'a>(auth: &'a Value, agent: &str, pid: &str) -> Option<&'a Value> {
    auth.get("members")
        .and_then(|m| m.get(agent))
        .and_then(|r| r.get(pid))
}

fn row_mut<'a>(auth: &'a mut Value, agent: &str, pid: &str) -> Option<&'a mut Map<String, Value>> {
    auth.get_mut("members")
        .and_then(|m| m.get_mut(agent))
        .and_then(|r| r.get_mut(pid))
        .and_then(Value::as_object_mut)
}

/// `own_row`: the C2 checks of a grant on a person's own row, in the locked step that writes it: a person, not the owner,
/// with a row of their own (never the `*` row) and access to agent now. Returns the canonical id.
fn own_row(c: &Ctx, auth: &Value, agent: &str, pid: &str) -> R<String> {
    need_person(auth, pid)?;
    let pid = secrets::canonical(auth, pid);
    if secrets::is_owner(auth, &pid) {
        return die(format!("{pid} is the owner: nothing is boxed for them"));
    }
    if !row_of(auth, agent, &pid).is_some_and(Value::is_object) {
        return die(format!("{pid} has no membership of their own for {agent}"));
    }
    if !Rules::new(c, auth).may(&pid, agent, None, false)?.0 {
        return die(format!("{pid} may not use {agent} now"));
    }
    Ok(pid)
}

/// `approver_of`: who approved an always, as `access_by` stores it: None for the owner (no `--by`, or the owner named),
/// else the person, who must be pid or the bot pid's owner and a member of agent now.
fn approver_of(
    c: &Ctx,
    auth: &Value,
    agent: &str,
    pid: &str,
    by: Option<&str>,
) -> R<Option<String>> {
    let Some(by) = by else { return Ok(None) };
    let by = secrets::canonical(auth, by);
    if secrets::is_owner(auth, &by) {
        return Ok(None);
    }
    if by != pid && secrets::parent_of(auth, pid).as_deref() != Some(by.as_str()) {
        return die(format!(
            "{by} approves only for themselves or their own bot, not for {pid}"
        ));
    }
    let r = Rules::new(c, auth).resolve(agent, &by, None, false)?;
    if r.get("allowed") != Some(&Value::Bool(true))
        || r.get("preset").and_then(Value::as_str) != Some("member")
    {
        return die(format!("{by} is not a member of {agent} now"));
    }
    Ok(Some(by))
}

/// `cmd_access`, the grant: a card's always, for pid on agent. `home` is the caller's.
pub(crate) fn access_grant(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    home: &str,
    args: (&str, &str, &str, &str),
    by: Option<&str>,
) -> R<Vec<String>> {
    let (agent, pid, kind, value) = args;
    let Some(field) = field_of(kind) else {
        return usage("access <agent> <id> read|write|domain <path|host> [--by <id>] [--rm]");
    };
    let value = if kind == "domain" {
        if !kith_kernel::launch::guard::valid_domain(value) {
            return usage(format!(
                "'{value}' is not a plain host name (no wildcard, address or port)"
            ));
        }
        value.to_owned()
    } else {
        if !value.starts_with('/') {
            return usage("a path must be absolute");
        }
        let value = kith_kernel::paths::realpath(std::path::Path::new(value))
            .to_string_lossy()
            .into_owned();
        let secrets_dir = c.auth.parent().unwrap_or_else(|| std::path::Path::new(""));
        if let Some(why) = kith_kernel::permit::never_granted(
            home,
            &life.kernel.to_string_lossy(),
            &life.state.to_string_lossy(),
            &life.machine.to_string_lossy(),
            &secrets_dir.to_string_lossy(),
            &value,
        ) {
            return usage(why);
        }
        value
    };
    write::tables(auth);
    need_person(auth, pid)?;
    let pid = secrets::canonical(auth, pid);
    if secrets::is_owner(auth, &pid) {
        return die(format!("{pid} is the owner: nothing is boxed for them"));
    }
    if row_of(auth, agent, &pid).is_none_or(Value::is_null) {
        return die(format!("{pid} has no membership of their own for {agent}"));
    }
    if !Rules::new(c, auth).may(&pid, agent, None, false)?.0 {
        return die(format!("{pid} may not use {agent} now"));
    }
    if let Some(parent) = secrets::parent_of(auth, &pid)
        && !secrets::is_owner(auth, &parent)
    {
        let theirs = Rules::new(c, auth).resolve(agent, &parent, None, false)?;
        let have = theirs.get("access").cloned().unwrap_or(Value::Null);
        if !secrets::held(&have, field, &json!(value)) {
            return die(format!(
                "{pid} is a bot: it can't be given more than its owner {parent} holds"
            ));
        }
    }
    let by = approver_of(c, auth, agent, &pid, by)?;
    let now = stamp(c);
    let Some(row) = row_mut(auth, agent, &pid) else {
        return die(format!("{pid} has no membership of their own for {agent}"));
    };
    let have = entry(row, "access")
        .entry(field)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !have.is_array() {
        *have = Value::Array(Vec::new());
    }
    let list = have.as_array_mut().expect("just made");
    let new = !list.iter().any(|x| x.as_str() == Some(value.as_str()));
    if new {
        list.push(json!(value));
    }
    if !row.get("access_by").is_some_and(Value::is_object) {
        row.insert("access_by".to_owned(), json!({}));
    }
    let key = format!("{kind}:{value}");
    let by_value = by.map_or(Value::Null, Value::String);
    if new {
        entry(row, "access_since").insert(key.clone(), json!(now));
        entry(row, "access_by").insert(key, by_value);
    } else if by_value.is_null() {
        // the owner's yes outlasts a person's; a person's never replaces the owner's
        entry(row, "access_by").insert(key, Value::Null);
    }
    Ok(vec![format!(
        "{pid} may {kind} {value} beyond the box for {agent}"
    )])
}

/// `remove_access`: taking an always away works on anyone, and on the entry as it is written: no check stands in the way
/// (a path that moved since, or that a newer rule would refuse, must still go). A path is also tried by its real path.
pub(crate) fn access_remove(
    auth: &mut Value,
    agent: &str,
    pid: &str,
    kind: &str,
    value: &str,
) -> R<Vec<String>> {
    let Some(field) = field_of(kind) else {
        return usage("access <agent> <id> read|write|domain <path|host> [--by <id>] [--rm]");
    };
    let mut names = vec![value.to_owned()];
    if kind != "domain" && value.starts_with('/') {
        let real = kith_kernel::paths::realpath(std::path::Path::new(value))
            .to_string_lossy()
            .into_owned();
        if !names.contains(&real) {
            names.push(real);
        }
    }
    write::tables(auth);
    if let Some(row) = row_mut(auth, agent, pid) {
        for v in names {
            let Some(list) = row
                .get_mut("access")
                .and_then(|a| a.get_mut(field))
                .and_then(Value::as_array_mut)
            else {
                break;
            };
            let Some(i) = list.iter().position(|x| x.as_str() == Some(v.as_str())) else {
                continue;
            };
            list.remove(i);
            for meta in ["access_since", "access_by"] {
                if let Some(m) = row.get_mut(meta).and_then(Value::as_object_mut) {
                    m.remove(&format!("{kind}:{v}"));
                }
            }
        }
    }
    Ok(vec![format!(
        "{pid} no longer has {kind} {value} beyond the box for {agent}"
    )])
}

/// How a `helper` write changes the row.
pub(crate) enum HelperMode {
    Always,
    Once,
    Remove,
}

/// `cmd_helper`: the always (and `--once`) of a helper card: the person's runs of agent may call helper. `--rm` takes the
/// entry as written, with no check.
pub(crate) fn helper_set(
    auth: &mut Value,
    c: &Ctx,
    agent: &str,
    pid: &str,
    helper: &str,
    mode: &HelperMode,
) -> R<Vec<String>> {
    write::tables(auth);
    if matches!(mode, HelperMode::Remove) {
        let pid = secrets::canonical(auth, pid);
        if let Some(row) = row_mut(auth, agent, &pid)
            && let Some(hs) = row.get_mut("helpers").and_then(Value::as_array_mut)
            && let Some(i) = hs.iter().position(|h| h.as_str() == Some(helper))
        {
            hs.remove(i);
            if let Some(since) = row.get_mut("helper_since").and_then(Value::as_object_mut) {
                since.remove(helper);
            }
        }
        return Ok(vec![format!(
            "{pid}'s runs of {agent} may no longer call {helper}"
        )]);
    }
    if !c.agent_exists(helper) || helper == agent {
        return usage(format!("no agent '{helper}' to call from {agent}"));
    }
    let pid = own_row(c, auth, agent, pid)?;
    if let Some(parent) = secrets::parent_of(auth, &pid)
        && !secrets::is_owner(auth, &parent)
    {
        let theirs = Rules::new(c, auth).resolve(agent, &parent, None, false)?;
        let listed = theirs
            .get("helpers")
            .and_then(Value::as_array)
            .is_some_and(|h| h.iter().any(|x| x.as_str() == Some(helper)));
        if !listed {
            return die(format!(
                "{pid} is a bot: it can't be given more than its owner {parent} holds"
            ));
        }
    }
    let now = c.now_ms;
    let at = stamp(c);
    let Some(row) = row_mut(auth, agent, &pid) else {
        return die(format!("{pid} has no membership of their own for {agent}"));
    };
    if matches!(mode, HelperMode::Once) {
        let left: Map<String, Value> = row
            .get("helper_once")
            .and_then(Value::as_object)
            .map(|l| {
                l.iter()
                    .filter(|(_, t)| t.as_f64().is_some_and(|t| t > now))
                    .map(|(h, t)| (h.clone(), t.clone()))
                    .collect()
            })
            .unwrap_or_default();
        row.insert("helper_once".to_owned(), Value::Object(left));
        #[allow(clippy::cast_possible_truncation)]
        let until = (now + HELPER_ONCE_MS) as i64;
        entry(row, "helper_once").insert(helper.to_owned(), json!(until));
        return Ok(vec![format!(
            "{pid}'s run of {agent} may call {helper} once"
        )]);
    }
    let mut helpers: Vec<Value> = row
        .get("helpers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !helpers.iter().any(|h| h.as_str() == Some(helper)) {
        helpers.push(json!(helper));
        row.insert("helpers".to_owned(), Value::Array(helpers));
        entry(row, "helper_since").insert(helper.to_owned(), json!(at));
    }
    Ok(vec![format!("{pid}'s runs of {agent} may call {helper}")])
}

/// `cmd_job` with `--rm`: the tick goes, as written, with no check.
pub(crate) fn job_remove(auth: &mut Value, agent: &str, pid: &str, name: &str) -> Vec<String> {
    write::tables(auth);
    let pid = secrets::canonical(auth, pid);
    if let Some(row) = row_mut(auth, agent, &pid)
        && let Some(ticks) = row.get_mut("jobs").and_then(Value::as_object_mut)
    {
        ticks.shift_remove(name);
    }
    vec![format!("{pid} no longer has the job {name} for {agent}")]
}

/// `cmd_job --decline`: the No of a widening card, so that package update isn't asked again (the pieces it added still ask
/// one by one).
pub(crate) fn job_decline(
    auth: &mut Value,
    c: &Ctx,
    agent: &str,
    pid: &str,
    name: &str,
    hash: &str,
) -> R<Vec<String>> {
    if !is_agent_name(agent) {
        return usage("job <agent> <id> <job> [--shown JSON | --decline HASH | --rm]");
    }
    if !Rules::new(c, auth).declares_job(agent, name) {
        return usage(format!(
            "{agent} declares no job '{name}' (capabilities.yaml jobs:)"
        ));
    }
    write::tables(auth);
    let pid = own_row(c, auth, agent, pid)?;
    let tick = row_mut(auth, agent, &pid)
        .and_then(|r| r.get_mut("jobs"))
        .and_then(Value::as_object_mut)
        .and_then(|t| t.get_mut(name))
        .and_then(Value::as_object_mut);
    let Some(tick) = tick else {
        return die(format!("{pid} has no tick of {name} on {agent}"));
    };
    let mut declined: Vec<Value> = tick
        .get("declined")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !declined.iter().any(|d| d.as_str() == Some(hash)) {
        declined.push(json!(hash));
    }
    tick.insert("declined".to_owned(), Value::Array(declined));
    Ok(vec![format!(
        "{pid}'s {name} on {agent} stays as it was ticked"
    )])
}
