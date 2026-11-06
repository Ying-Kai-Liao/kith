//! The owner's writes to authority, as functions over the auth.json-shaped value (M2-2). Each is a port of the verb of
//! `kernel/bin/kith-auth` it is named for, step by step, refusals and the words of what it prints included: Python
//! is the reference. They change the value and say what `kith-auth` would print; `store.rs` writes the difference to
//! core.db, in one transaction, and the tests hold the two shapes equal.
//!
//! Not here, with who owns it: adding, renaming and removing a person, `revoke`/`unrevoke` of a person and the ids a
//! removal retires (`revoke.rs`, M2-4), what `forget` also drops from `grants` and `profiles` (lane B, secrets), and
//! `env.VAR` settings through the table (they work in [`member`]; the row has no param for them yet).

use std::collections::BTreeMap;

use kith_kernel::secrets::{self, Ctx, Die, Rules, pyjson};
use serde_json::{Map, Value, json};

pub(super) type R<T> = Result<T, Die>;

/// Modes that run any command without asking (`FREE_SHELL`).
const FREE_SHELL: [&str; 2] = ["bypassPermissions", "auto"];
/// The membership row for every person kith knows.
const EVERYONE: &str = "*";

pub(super) fn die<T>(msg: impl Into<String>) -> R<T> {
    Err(Die {
        msg: msg.into(),
        code: 1,
    })
}

/// A refusal that is usage (`die(..., 2)`): the request is wrong, not the state.
pub(super) fn usage<T>(msg: impl Into<String>) -> R<T> {
    Err(Die {
        msg: msg.into(),
        code: 2,
    })
}

/// `load_auth`'s `setdefault` of every table.
pub(super) fn tables(auth: &mut Value) {
    if !auth.is_object() {
        *auth = Value::Object(Map::new());
    }
    if let Some(o) = auth.as_object_mut() {
        for t in [
            "people",
            "presets",
            "members",
            "invites",
            "retired",
            "agent_access",
        ] {
            if !o.get(t).is_some_and(Value::is_object) {
                o.insert(t.to_owned(), Value::Object(Map::new()));
            }
        }
    }
}

pub(super) fn table<'a>(auth: &'a mut Value, name: &str) -> &'a mut Map<String, Value> {
    tables(auth);
    auth.get_mut(name)
        .and_then(Value::as_object_mut)
        .expect("tables() made it")
}

/// `dict.setdefault(key, {})` and the object behind it.
pub(super) fn entry<'a>(m: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let v = m
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().expect("just made")
}

pub(super) fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Python's truth of a value.
pub(super) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn free_shell(mode: &str) -> bool {
    FREE_SHELL.contains(&mode)
}

/// `float(text)` as Python reads it: surrounding space, `_` between digits, `inf`/`nan`/`infinity` and an exponent.
fn py_float(text: &str) -> Option<f64> {
    let t = text.trim();
    let b = t.as_bytes();
    for (i, c) in b.iter().enumerate() {
        if *c == b'_'
            && !(i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit())
        {
            return None;
        }
    }
    t.replace('_', "").parse::<f64>().ok()
}

/// `re.fullmatch(r'[a-z][a-z0-9-]*', id)`.
pub(super) fn is_id(id: &str) -> bool {
    let mut it = id.chars();
    it.next().is_some_and(|c| c.is_ascii_lowercase())
        && it.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(super) fn people(auth: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    auth.get("people")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

pub(super) fn is_bot(auth: &Value, id: &str) -> bool {
    truthy(people(auth).get(id).and_then(|p| p.get("bot_of")))
}

/// `bots_of`: the bots a person owns, sorted.
pub(super) fn bots_of(auth: &Value, person: &str) -> Vec<String> {
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

/// `need_person`: someone removed (or never added) is refused.
pub(super) fn need_person(auth: &Value, pid: &str) -> R<()> {
    if people(auth).contains_key(pid) {
        Ok(())
    } else {
        die(format!(
            "no person '{pid}' (kith-auth person {pid} name=..)"
        ))
    }
}

/// `preset`: a named bundle, from auth.json or the built-in `guest` and `member`.
fn preset(auth: &Value, name: &str) -> R<Value> {
    let own = auth.get("presets").and_then(|p| p.get(name));
    if truthy(own) {
        return Ok(own.cloned().unwrap_or(Value::Null));
    }
    match name {
        "guest" => Ok(json!({"mode": "default"})),
        "member" => Ok(json!({"mode": "acceptEdits"})),
        _ => die(format!(
            "no preset '{name}' (kith-auth preset {name} mode=..)"
        )),
    }
}

/// `mode_of`: the membership's mode; a free shell set without `trusted` falls back to default.
fn mode_of(row: &Value, p: &Value) -> String {
    let own = str_of(row, "mode").filter(|m| !m.is_empty());
    let mode = own.or_else(|| str_of(p, "mode")).unwrap_or("default");
    let trusted = if own.is_some() {
        truthy(row.get("trusted"))
    } else {
        truthy(p.get("trusted"))
    };
    if free_shell(mode) && !trusted {
        "default".to_owned()
    } else {
        mode.to_owned()
    }
}

fn check_free_shell(mode: &str, trusted: bool, what: &str) -> R<()> {
    if free_shell(mode) && !trusted {
        return usage(format!(
            "{what}: mode={mode} runs any command on this Mac as you, unasked. Add --trusted if you mean it"
        ));
    }
    Ok(())
}

/// `check_env_key`: a plain variable name, and none that would change how the process starts or which kith it talks to.
fn check_env_key(name: &str) -> R<()> {
    let mut chars = name.chars();
    let shape = chars
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let reserved = ["PATH", "HOME", "SHELL"].contains(&name)
        || ["DYLD_", "LD_", "KITH_", "HQ_"]
            .iter()
            .any(|p| name.starts_with(p));
    if !shape || reserved {
        return usage(format!(
            "env.{name}: not a variable a membership may set (NAME in capitals, and not PATH, HOME, SHELL, DYLD_*, LD_*, KITH_* or HQ_*)"
        ));
    }
    Ok(())
}

fn list_of(v: &str) -> Value {
    Value::Array(
        v.split(',')
            .filter(|x| !x.is_empty())
            .map(|x| Value::String(x.to_owned()))
            .collect(),
    )
}

pub(super) fn words(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn joined(v: Option<&Value>) -> String {
    words(v).join(",")
}

/// `drop_self_approved`: pid stops being a member of agent: the always entries they approved on agent, on their own row
/// and their bots', go. Entries with no approver recorded stay for the owner to review. Returns how many went.
fn drop_self_approved(auth: &mut Value, agent: &str, pid: &str) -> usize {
    let mut who = vec![pid.to_owned()];
    who.extend(bots_of(auth, pid));
    let mut gone = 0;
    let Some(rows) = auth
        .get_mut("members")
        .and_then(|m| m.get_mut(agent))
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };
    for w in who {
        let Some(row) = rows.get_mut(&w).and_then(Value::as_object_mut) else {
            continue;
        };
        let keys: Vec<String> = row
            .get("access_by")
            .and_then(Value::as_object)
            .map(|by| {
                by.iter()
                    .filter(|(_, v)| v.as_str() == Some(pid))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        for key in keys {
            let (kind, value) = key.split_once(':').unwrap_or((key.as_str(), ""));
            let field = match kind {
                "read" => "read",
                "write" => "write",
                "domain" => "domains",
                _ => "",
            };
            if let Some(list) = row
                .get_mut("access")
                .and_then(|a| a.get_mut(field))
                .and_then(Value::as_array_mut)
                && let Some(i) = list.iter().position(|x| x.as_str() == Some(value))
            {
                list.remove(i);
                gone += 1;
            }
            if let Some(by) = row.get_mut("access_by").and_then(Value::as_object_mut) {
                by.remove(&key);
            }
            if let Some(since) = row.get_mut("access_since").and_then(Value::as_object_mut) {
                since.remove(&key);
            }
        }
    }
    gone
}

/// `forget`: everything that gives these ids access, as Python's: memberships, their `agent@<id>` grants, open
/// invites, and their place in profiles' `with` (an emptied `with` is dropped; profiles the id owns stay). Without
/// the grants, a bot handed over by remove-and-re-add would get its old logins back.
pub(super) fn forget(auth: &mut Value, ids: &[&str]) {
    let members = table(auth, "members");
    for rows in members.values_mut() {
        if let Some(rows) = rows.as_object_mut() {
            for x in ids {
                rows.remove(*x);
            }
        }
    }
    members.retain(|_, r| truthy(Some(&*r)));
    if let Some(grants) = auth.get_mut("grants").and_then(Value::as_object_mut) {
        grants.retain(|k, _| match k.rsplit_once('@') {
            Some((_, id)) => !ids.contains(&id),
            None => true,
        });
    }
    table(auth, "invites").retain(|_, i| !ids.iter().any(|x| str_of(i, "person") == Some(*x)));
    let profiles = auth.get_mut("profiles").and_then(Value::as_object_mut);
    for prof in profiles.into_iter().flat_map(|p| p.values_mut()) {
        let Some(with) = prof.get("with").and_then(Value::as_array) else {
            continue;
        };
        let lent = |w: &Value| w.as_str().is_some_and(|w| ids.contains(&w));
        if !with.iter().any(lent) {
            continue;
        }
        let kept: Vec<Value> = with.iter().filter(|w| !lent(w)).cloned().collect();
        if let Some(o) = prof.as_object_mut() {
            if kept.is_empty() {
                o.remove("with");
            } else {
                o.insert("with".to_owned(), Value::Array(kept));
            }
        }
    }
}

/// The `key=value` settings of a command, the last of a repeated key winning.
pub(crate) type Settings = BTreeMap<String, String>;

fn resolve(c: &Ctx, auth: &Value, agent: &str, pid: &str) -> R<Map<String, Value>> {
    Rules::new(c, auth).resolve(agent, pid, None, false)
}

/// The `drop_self_approved` of a demotion, with its line. True when something went (and `resolve` is asked again).
fn drop_and_say(auth: &mut Value, agent: &str, pid: &str, out: &mut Vec<String>) -> bool {
    let n = drop_self_approved(auth, agent, pid);
    if n == 0 {
        return false;
    }
    out.push(format!(
        "{pid} is no longer a member of {agent}: {n} always {} they approved went",
        if n == 1 { "entry" } else { "entries" }
    ));
    true
}

/// `kith-auth member <agent>[,..] <id> [preset=.. mode=.. deny=.. model=.. helpers=a,b budget=USD env.VAR=..]
/// [--trusted] [--rm]`: what it prints, one line each.
pub(crate) fn member(
    auth: &mut Value,
    c: &Ctx,
    agents: &str,
    pid: &str,
    kv: &Settings,
    trusted: bool,
    rm: bool,
) -> R<Vec<String>> {
    let wanted: Vec<&str> = agents.split(',').filter(|a| !a.is_empty()).collect();
    if let Some(budget) = kv.get("budget").filter(|b| !b.is_empty())
        && !py_float(budget).is_some_and(|f| f.is_nan() || f >= 0.0)
    {
        return usage("budget is dollars a month, e.g. budget=20");
    }
    tables(auth);
    if pid != EVERYONE && !rm {
        // taking access away works on anyone, giving it only on a person
        need_person(auth, pid)?;
    }
    if is_bot(auth, pid) {
        if kv.get("preset").map_or("guest", String::as_str) != "guest" {
            return usage(format!("{pid} is a bot: its preset is guest"));
        }
        if kv.get("budget").is_some_and(|b| !b.is_empty()) {
            return usage(format!(
                "{pid} is a bot: a bot spends from its owner's budget"
            ));
        }
        if kv
            .get("mode")
            .is_some_and(|m| !m.is_empty() && m != "default")
        {
            return usage(format!(
                "{pid} is a bot: it runs in the guest preset's mode"
            ));
        }
    }
    let mut out = Vec::new();
    for agent in wanted {
        // taking access away needs no folder: a row must not wait for one to come back
        if !rm && !c.agent_exists(agent) {
            return die(format!("no agent '{agent}' (kith ls)"));
        }
        if !rm && c.is_builtin(agent) {
            return usage(format!(
                "{agent} is a built-in agent: only the owner runs it, it can't be given to {}",
                if pid == EVERYONE { "everyone" } else { pid }
            ));
        }
        if rm {
            // their bots' rows keep nothing of theirs
            let n = if pid == EVERYONE {
                0
            } else {
                drop_self_approved(auth, agent, pid)
            };
            let members = table(auth, "members");
            if let Some(rows) = members.get_mut(agent).and_then(Value::as_object_mut) {
                rows.remove(pid);
            }
            if !truthy(members.get(agent)) {
                members.remove(agent);
            }
            out.push(format!(
                "{pid} may no longer use {agent}{}",
                if n > 0 {
                    format!(
                        " ({n} always {} they approved went)",
                        if n == 1 { "entry" } else { "entries" }
                    )
                } else {
                    String::new()
                }
            ));
            continue;
        }
        if let Some(mode) = kv.get("mode").filter(|m| !m.is_empty()) {
            check_free_shell(mode, trusted, &format!("{pid} on {agent}"))?;
        }
        let was_member = pid != EVERYONE
            && resolve(c, auth, agent, pid)?
                .get("preset")
                .and_then(Value::as_str)
                == Some("member");
        {
            let row = entry(entry(table(auth, "members"), agent), pid);
            if let Some(mode) = kv.get("mode") {
                row.remove("trusted");
                if free_shell(mode) {
                    row.insert("trusted".to_owned(), Value::Bool(true));
                }
            }
            for (k, v) in kv {
                if let Some(name) = k.strip_prefix("env.") {
                    if !v.is_empty() {
                        check_env_key(name)?;
                    }
                    let env = entry(row, "env");
                    if v.is_empty() {
                        env.remove(name);
                    } else {
                        env.insert(name.to_owned(), Value::String(v.clone()));
                    }
                    if env.is_empty() {
                        row.remove("env");
                    }
                } else if k == "deny" || k == "helpers" {
                    row.insert(k.clone(), list_of(v));
                } else if !v.is_empty() {
                    row.insert(k.clone(), Value::String(v.clone()));
                } else {
                    row.remove(k);
                }
            }
        }
        let row = auth["members"][agent][pid].clone();
        if let Some(p) = str_of(&row, "preset") {
            preset(auth, p)?;
        }
        let r: Map<String, Value> = if pid == EVERYONE {
            let name = str_of(&row, "preset").unwrap_or("guest");
            let p = preset(auth, name)?;
            let mut deny = words(p.get("deny"));
            deny.extend(words(row.get("deny")));
            let mut r = Map::new();
            r.insert("preset".into(), name.into());
            r.insert("mode".into(), mode_of(&row, &p).into());
            r.insert("deny".into(), json!(deny));
            r.insert("helpers".into(), json!(words(row.get("helpers"))));
            r
        } else {
            let mut r = resolve(c, auth, agent, pid)?;
            if was_member
                && r.get("preset").and_then(Value::as_str) != Some("member")
                && drop_and_say(auth, agent, pid, &mut out)
            {
                r = resolve(c, auth, agent, pid)?;
            }
            if r.get("owner") == Some(&Value::Bool(true)) {
                // the owner is always allowed: resolve gives no preset or mode for them
                out.push(format!("{pid} is the owner: always may use {agent}"));
                continue;
            }
            if r.get("allowed") != Some(&Value::Bool(true)) {
                // a bot whose owner does not hold the agent: the row waits, the access is not there
                out.push(format!(
                    "{pid} has a row for {agent}, but may not use it now: {}",
                    r.get("why").and_then(Value::as_str).unwrap_or_default()
                ));
                continue;
            }
            r
        };
        out.push(summary(pid, agent, &r));
    }
    Ok(out)
}

fn summary(pid: &str, agent: &str, r: &Map<String, Value>) -> String {
    let text = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let (deny, helpers) = (joined(r.get("deny")), joined(r.get("helpers")));
    let mode = text("mode");
    let budget = r.get("budget").filter(|b| truthy(Some(b)));
    format!(
        "{} may use {agent}: preset {}, mode {mode}{}{}{}{}",
        if pid == EVERYONE {
            "everyone kith knows"
        } else {
            pid
        },
        text("preset"),
        if deny.is_empty() {
            String::new()
        } else {
            format!(", deny {deny}")
        },
        if helpers.is_empty() {
            String::new()
        } else {
            format!(", helpers {helpers}")
        },
        budget.map_or(String::new(), |b| format!(
            ", budget ${}/month",
            b.as_str().map_or_else(|| b.to_string(), str::to_owned)
        )),
        if free_shell(&mode) {
            " (a free shell on this Mac: trusted)"
        } else {
            ""
        },
    )
}

/// `kith-auth preset <name> [mode=.. deny=a,b] [--rm]`.
pub(crate) fn preset_set(
    auth: &mut Value,
    name: &str,
    kv: &Settings,
    trusted: bool,
    rm: bool,
) -> R<Vec<String>> {
    tables(auth);
    if rm {
        table(auth, "presets").remove(name);
    } else {
        let presets = table(auth, "presets");
        if !presets.contains_key(name) {
            let start = match name {
                "guest" => json!({"mode": "default"}),
                "member" => json!({"mode": "acceptEdits"}),
                _ => json!({}),
            };
            presets.insert(name.to_owned(), start);
        }
        let p = entry(presets, name);
        if let Some(mode) = kv.get("mode") {
            check_free_shell(mode, trusted, &format!("preset {name}"))?;
            p.insert("mode".to_owned(), mode.clone().into());
            p.remove("trusted");
            if free_shell(mode) {
                p.insert("trusted".to_owned(), true.into());
            }
        }
        if let Some(deny) = kv.get("deny") {
            p.insert("deny".to_owned(), list_of(deny));
        }
    }
    let known = auth["presets"].get(name).is_some() || matches!(name, "guest" | "member");
    let shown = if known {
        pyjson::dumps(&preset(auth, name)?)
    } else {
        "removed".to_owned()
    };
    Ok(vec![format!("{name}: {shown}")])
}

/// `kith-auth bot <id> --of <person> [name=.. provider=..]`.
pub(crate) fn bot_add(
    auth: &mut Value,
    bid: &str,
    of: Option<&str>,
    kv: &Settings,
) -> R<Vec<String>> {
    if !is_id(bid) {
        return usage("a bot id is lowercase letters, digits and -, e.g. bot-3f9a1c2e");
    }
    tables(auth);
    let Some(of) = of.filter(|o| !o.is_empty()) else {
        return usage(
            "bot <id> --of <person>: whose bot it is (the owner, or a person kith knows)",
        );
    };
    let parent = secrets::canonical(auth, of);
    if !people(auth).contains_key(&parent) {
        return die(format!("no person '{of}' (kith-auth people)"));
    }
    if is_bot(auth, &parent) {
        return die(format!(
            "{parent} is a bot: a bot belongs to a person, not to another bot"
        ));
    }
    let cur = people(auth).get(bid).cloned();
    if let Some(cur) = &cur {
        if !truthy(cur.get("bot_of")) {
            return die(format!("'{bid}' is already a person, not a bot"));
        }
        let theirs = str_of(cur, "bot_of").unwrap_or_default();
        if secrets::canonical(auth, theirs) != parent {
            return die(format!(
                "{bid} belongs to {theirs}; remove it (--rm) and add it again to hand it over"
            ));
        }
    } else {
        // need_free(left=False): a bot's folders are its own from an earlier join, so only the two id rules
        let why = if auth["retired"].get(bid).is_some() {
            Some(format!("'{bid}' was a person who was removed"))
        } else if secrets::canonical(auth, bid) != bid {
            Some(format!(
                "'{bid}' is an old id of {}",
                secrets::canonical(auth, bid)
            ))
        } else {
            None
        };
        if let Some(why) = why {
            return die(format!(
                "{why}: give the new person another id (e.g. {bid}-2)"
            ));
        }
        forget(auth, &[bid]);
    }
    let p = entry(table(auth, "people"), bid);
    p.insert("bot_of".to_owned(), parent.clone().into());
    for (k, v) in kv {
        if v.is_empty() {
            p.remove(k);
        } else {
            p.insert(k.clone(), v.clone().into());
        }
    }
    let mut line = format!("{bid}: bot of {parent}");
    for k in ["name", "provider"] {
        if let Some(v) = p.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
            line = format!("{line}, {k} {v}");
        }
    }
    Ok(vec![line])
}

/// `kith-auth bot <id> --rm`.
pub(crate) fn bot_remove(auth: &mut Value, bid: &str) -> R<Vec<String>> {
    if !is_id(bid) {
        return usage("a bot id is lowercase letters, digits and -, e.g. bot-3f9a1c2e");
    }
    tables(auth);
    if !is_bot(auth, bid) {
        // A person is not "no such id": kith-auth's mirror treats "no bot" as already gone and stays quiet, which a person's
        // row must not earn.
        if people(auth).contains_key(bid) {
            return die(format!("'{bid}' is a person, not a bot"));
        }
        return die(format!("no bot '{bid}'"));
    }
    // shift_remove: a person leaving must not move the others (the first match of `canonical` and `owner` wins)
    table(auth, "people").shift_remove(bid);
    forget(auth, &[bid]);
    Ok(vec![format!("{bid} removed, with its memberships")])
}

/// `kith-auth bots-cap <amount|off>`: the cap on what all the owner's bots spend together. The line it prints.
pub(crate) fn bots_cap(auth: &mut Value, arg: &str) -> R<Vec<String>> {
    tables(auth);
    if arg == "off" {
        if let Some(o) = auth.as_object_mut() {
            o.remove("owner_bots_budget");
        }
    } else {
        if !py_float(arg).is_some_and(|f| f.is_nan() || f > 0.0) {
            return usage(
                "bots-cap is dollars a month for all your bots together, e.g. bots-cap 50 (or off)",
            );
        }
        auth["owner_bots_budget"] = arg.into();
    }
    Ok(vec![bots_cap_line(auth)])
}

/// `kith-auth bots-cap` with no argument.
pub(crate) fn bots_cap_line(auth: &Value) -> String {
    match auth.get("owner_bots_budget").filter(|c| truthy(Some(c))) {
        Some(cap) => format!(
            "your bots together: at most ${} a month",
            cap.as_str().map_or_else(|| cap.to_string(), str::to_owned)
        ),
        None => "your bots have no monthly cap".to_owned(),
    }
}

/// `kith-auth invite <id> [minutes]`. The code is the caller's: the OS's randomness is not this module's.
pub(crate) fn invite_create(
    auth: &mut Value,
    c: &Ctx,
    person: &str,
    minutes: Option<i64>,
    code: &str,
) -> R<Vec<String>> {
    tables(auth);
    need_person(auth, person)?;
    if secrets::cut_off(auth, &secrets::canonical(auth, person)) {
        return die(format!(
            "{person}'s access was removed: kith-auth person {person} --unrevoke first"
        ));
    }
    let minutes = minutes.unwrap_or(15);
    let Some(seconds) = minutes.checked_mul(60).filter(|s| s.abs() <= (1 << 52)) else {
        return usage("minutes is a number of minutes, e.g. invite sam 30");
    };
    #[allow(clippy::cast_precision_loss)]
    let expires = c.now_ms / 1000.0 + seconds as f64;
    table(auth, "invites").insert(
        code.to_owned(),
        json!({"person": person, "expires": expires}),
    );
    Ok(vec![
        code.to_owned(),
        format!(
            "{person} sends \"/link {code}\" to the kith bot on any platform within {minutes} minutes"
        ),
    ])
}

/// A code as `kith-auth link` reads it.
pub(crate) fn code_of(code: &str) -> String {
    code.trim().to_uppercase()
}

/// Takes an open code back. `kith-auth` only spends one (`link`); the owner taking one back gets the same refusal for a
/// code that is unknown or ran out.
pub(crate) fn invite_remove(auth: &mut Value, c: &Ctx, code: &str) -> R<Vec<String>> {
    tables(auth);
    let code = code_of(code);
    let live = table(auth, "invites")
        .get(&code)
        .and_then(|i| i.get("expires"))
        .and_then(Value::as_f64)
        .is_some_and(|e| e >= c.now_ms / 1000.0);
    if !live {
        return die("that code is unknown or expired; ask for a new one (kith-auth invite <id>)");
    }
    table(auth, "invites").remove(&code);
    Ok(vec![format!("{code} removed")])
}

/// `AGENT_RE`: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (Python's `$` also lets one newline through).
fn is_agent_name(s: &str) -> bool {
    let s = s.strip_suffix('\n').unwrap_or(s);
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `cmd_agent_access` with a host: keeps it for every guest of the agent, or (`remove`) stops keeping it. Adding twice
/// and removing what is not kept change nothing; a removal checks nothing about the host, as in `kith-auth`.
pub(crate) fn agent_access(
    auth: &mut Value,
    c: &Ctx,
    agent: &str,
    host: &str,
    remove: bool,
) -> R<Vec<String>> {
    // `kith-auth` lets a trailing newline through (Python's `$`); a name with one, or any control character, is no name
    // for a table key the owner reads back.
    if agent.chars().chain(host.chars()).any(char::is_control) {
        return usage("an agent or host name may not hold a newline or other control character");
    }
    if !is_agent_name(agent) {
        return usage("agent-access <agent> [--json] | agent-access <agent> domain <host> [--rm]");
    }
    tables(auth);
    if remove {
        let kept = table(auth, "agent_access");
        let was = kept
            .get(agent)
            .and_then(|k| k.get("domains"))
            .and_then(Value::as_array)
            .is_some_and(|d| d.iter().any(|x| x.as_str() == Some(host)));
        if was && let Some(row) = kept.get_mut(agent).and_then(Value::as_object_mut) {
            if let Some(d) = row.get_mut("domains").and_then(Value::as_array_mut) {
                d.retain(|x| x.as_str() != Some(host));
            }
            for meta in ["since", "by"] {
                if let Some(m) = row.get_mut(meta).and_then(Value::as_object_mut) {
                    m.remove(host);
                }
            }
            if !truthy(row.get("domains")) {
                kept.remove(agent);
            }
        }
        return Ok(vec![format!(
            "{host} is no longer kept for everyone {agent} is shared with"
        )]);
    }
    if !kith_kernel::launch::guard::valid_domain(host) {
        return usage(format!(
            "'{host}' is not a plain host name (no wildcard, address or port)"
        ));
    }
    let row = entry(table(auth, "agent_access"), agent);
    let have = row
        .entry("domains")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !have.is_array() {
        *have = Value::Array(Vec::new());
    }
    let list = have.as_array_mut().expect("just made");
    if !list.iter().any(|x| x.as_str() == Some(host)) {
        list.push(json!(host));
        // a clock in milliseconds is far inside i64
        #[allow(clippy::cast_possible_truncation)]
        let at = c.now_ms as i64;
        entry(row, "since").insert(host.to_owned(), json!(at));
        entry(row, "by").insert(host.to_owned(), Value::Null);
    }
    Ok(vec![format!(
        "every guest of {agent} may reach {host} beyond the box"
    )])
}

/// What `save_auth` does on every save: the codes that ran out are gone.
pub(super) fn prune_invites(auth: &mut Value, c: &Ctx) {
    let now = c.now_ms / 1000.0;
    table(auth, "invites")
        .retain(|_, i| i.get("expires").and_then(Value::as_f64).unwrap_or(0.0) > now);
}

/// The open codes, sorted by code.
pub(crate) fn invites(auth: &Value, c: &Ctx) -> Vec<Value> {
    let now = c.now_ms / 1000.0;
    auth.get("invites")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(_, i)| i.get("expires").and_then(Value::as_f64).unwrap_or(0.0) > now)
                .map(|(code, i)| json!({"code": code, "person": i["person"], "expires": i["expires"]}))
                .collect()
        })
        .unwrap_or_default()
}

/// Who may use which agent, one entry per membership row: the person's own rows and the `*` row, as the rules read them.
/// An entry whose rules cannot answer (a preset nobody defined) says so in `why` and is not allowed.
pub(crate) fn list(auth: &Value, c: &Ctx, agent: Option<&str>, person: Option<&str>) -> Vec<Value> {
    let Some(members) = auth.get("members").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (a, rows) in members {
        let Some(rows) = rows.as_object() else {
            continue;
        };
        if agent.is_some_and(|f| f != a) {
            continue;
        }
        for (who, row) in rows {
            if person.is_some_and(|f| f != who) || !row.is_object() {
                continue;
            }
            let mut e = Map::new();
            e.insert("agent".into(), a.clone().into());
            e.insert("person".into(), who.clone().into());
            e.insert(
                "via".into(),
                if who == EVERYONE { "everyone" } else { "own" }.into(),
            );
            let seen: R<Map<String, Value>> = if who == EVERYONE {
                let name = str_of(row, "preset").unwrap_or("guest");
                preset(auth, name).map(|p| {
                    let mut deny = words(p.get("deny"));
                    deny.extend(words(row.get("deny")));
                    let mut r = Map::new();
                    r.insert("allowed".into(), true.into());
                    r.insert("why".into(), "everyone".into());
                    r.insert("preset".into(), name.into());
                    r.insert("mode".into(), mode_of(row, &p).into());
                    r.insert("deny".into(), json!(deny));
                    r.insert(
                        "model".into(),
                        str_of(row, "model").unwrap_or_default().into(),
                    );
                    r.insert("helpers".into(), json!(words(row.get("helpers"))));
                    r.insert(
                        "budget".into(),
                        str_of(row, "budget").unwrap_or_default().into(),
                    );
                    r
                })
            } else {
                resolve(c, auth, a, who)
            };
            match seen {
                Ok(r) => {
                    for k in [
                        "allowed", "why", "preset", "mode", "deny", "model", "helpers", "budget",
                    ] {
                        if let Some(v) = r.get(k) {
                            e.insert(k.into(), v.clone());
                        }
                    }
                }
                Err(d) => {
                    e.insert("allowed".into(), false.into());
                    e.insert("why".into(), d.msg.into());
                }
            }
            out.push(Value::Object(e));
        }
    }
    out
}

/// `kith-auth budgets <agent> --json`: each person's monthly budget on the agent in dollars, null for none; the `*` row
/// is left out. A budget that is no number is an error here as it is a crash there: a spending limit that cannot be read
/// is never "no limit".
pub(crate) fn budgets(auth: &Value, agent: &str) -> R<Value> {
    let mut out = Map::new();
    let rows = auth
        .get("members")
        .and_then(|m| m.get(agent))
        .and_then(Value::as_object);
    for (pid, row) in rows.into_iter().flatten().filter(|(p, _)| *p != EVERYONE) {
        let raw = row.get("budget").filter(|b| truthy(Some(b)));
        let amount = match raw {
            None => Value::Null,
            Some(b) => {
                let f = match b {
                    Value::String(s) => py_float(s),
                    Value::Number(n) => n.as_f64(),
                    _ => None,
                };
                match f
                    .filter(|f| f.is_finite())
                    .and_then(serde_json::Number::from_f64)
                {
                    Some(n) => Value::Number(n),
                    None => {
                        return die(format!(
                            "the budget of {pid} on {agent} is not a number: {b}"
                        ));
                    }
                }
            }
        };
        out.insert(pid.clone(), amount);
    }
    Ok(Value::Object(out))
}

const ACCESS_KINDS: [(&str, &str); 3] =
    [("read", "read"), ("write", "write"), ("domain", "domains")];
const LEDGER_KINDS: [&str; 6] = ["read", "write", "domain", "service", "job", "helper"];

/// `grant_list`: the profiles a grant names, one string or a list of them.
fn grant_list(g: &Value) -> Vec<&str> {
    match g {
        Value::String(s) if !s.is_empty() => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// `access_of(row)` for one field: the strings of the places or websites the row holds.
fn access_field<'a>(row: &'a Value, field: &str) -> Vec<&'a str> {
    row.get("access")
        .and_then(|a| a.get(field))
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// `ledger(auth)`: every grant that outlives one run, for anyone but the owner, sorted by agent, person, kind, target.
/// `person` is canonical already (`ledger --person` names an old id as well).
pub(crate) fn ledger(
    auth: &Value,
    c: &Ctx,
    person: Option<&str>,
    agent: Option<&str>,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut add = |kind: &str,
                   agent: &str,
                   pid: &str,
                   target: &str,
                   since: Option<&Value>,
                   by: Option<Value>| {
        let name = people(auth)
            .get(pid)
            .and_then(|p| p.get("name"))
            .cloned()
            .unwrap_or_else(|| json!(""));
        out.push(json!({
            "key": format!("{kind}:{agent}:{pid}:{target}"), "kind": kind, "agent": agent, "person": pid,
            "name": name, "target": target,
            "since": since.filter(|s| s.is_i64() || s.is_u64()).cloned().unwrap_or(Value::Null),
            "approved_by": by.unwrap_or(Value::Null),
        }));
    };
    let by_of = |by: &Map<String, Value>, k: &str| -> Option<Value> {
        by.get(k).map(|v| {
            if v.is_null() {
                json!("owner")
            } else {
                v.clone()
            }
        })
    };
    let none = Map::new();
    for (a, rows) in auth
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (pid, row) in rows.as_object().into_iter().flatten() {
            if !row.is_object() {
                continue;
            }
            let since = row
                .get("access_since")
                .and_then(Value::as_object)
                .unwrap_or(&none);
            let by = row
                .get("access_by")
                .and_then(Value::as_object)
                .unwrap_or(&none);
            for (kind, field) in ACCESS_KINDS {
                for v in access_field(row, field) {
                    let k = format!("{kind}:{v}");
                    add(kind, a, pid, v, since.get(&k), by_of(by, &k));
                }
            }
            for (name, tick) in row
                .get("jobs")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                add("job", a, pid, name, tick.get("since"), None);
            }
            let hsince = row
                .get("helper_since")
                .and_then(Value::as_object)
                .unwrap_or(&none);
            for h in row
                .get("helpers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                add("helper", a, pid, h, hsince.get(h), None);
            }
        }
    }
    // the hosts every guest of an agent reaches: person `*`; approved_by `package` for the ones its package declares
    let mut agents: Vec<String> = auth
        .get("agent_access")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for n in c.agent_names() {
        if !agents.contains(&n) {
            agents.push(n);
        }
    }
    agents.sort();
    for a in &agents {
        let kept = auth
            .get("agent_access")
            .and_then(|k| k.get(a.as_str()))
            .filter(|k| k.is_object());
        let since = kept
            .and_then(|k| k.get("since"))
            .and_then(Value::as_object)
            .unwrap_or(&none);
        let by = kept
            .and_then(|k| k.get("by"))
            .and_then(Value::as_object)
            .unwrap_or(&none);
        let kept_domains: Vec<&str> = kept
            .and_then(|k| k.get("domains"))
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for d in Rules::new(c, auth).agent_domains(a) {
            if kept_domains.contains(&d.as_str()) {
                add("domain", a, EVERYONE, &d, since.get(&d), by_of(by, &d));
            } else {
                add("domain", a, EVERYONE, &d, None, Some(json!("package")));
            }
        }
    }
    for (k, svcs) in auth
        .get("grants")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some((a, pid)) = k.split_once('@') else {
            continue;
        };
        let Some(svcs) = svcs.as_object() else {
            continue;
        };
        if secrets::is_owner(auth, &secrets::canonical(auth, pid)) {
            continue;
        }
        for g in svcs.values() {
            for profile in grant_list(g) {
                add("service", a, pid, profile, None, None);
            }
        }
    }
    let s = |e: &Value, k: &str| e[k].as_str().unwrap_or_default().to_owned();
    out.sort_by_key(|e| (s(e, "agent"), s(e, "person"), s(e, "kind"), s(e, "target")));
    out.retain(|e| {
        person.is_none_or(|p| e["person"] == p) && agent.is_none_or(|a| e["agent"] == a)
    });
    out
}

/// `kith-auth ledger --rm <key>`: that one entry goes, as it is written (a path that moved since must still go). Nothing
/// removed is an error, never a success.
pub(crate) fn remove_grant(auth: &mut Value, c: &Ctx, key: &str) -> R<Vec<String>> {
    let mut parts = key.splitn(4, ':');
    let (kind, agent, pid, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if !LEDGER_KINDS.contains(&kind) || agent.is_empty() || pid.is_empty() || target.is_empty() {
        return usage(format!(
            "not a ledger key: {key} (<kind>:<agent>:<person>:<target>)"
        ));
    }
    tables(auth);
    // Service grants are lane B's table, which a database imported before that lane may not hold in full: it would say
    // "removed already" for a grant that is there. Refused until a re-import.
    if kind == "service" && truthy(auth.get("secrets_stale")) {
        return die(
            "the core has not imported the service grants yet (kith kernel core-migrate --authority --force)",
        );
    }
    let mut gone = false;
    if kind == "service" {
        let gkey = format!("{agent}@{pid}");
        let mut emptied = false;
        if let Some(svcs) = auth
            .get_mut("grants")
            .and_then(|g| g.get_mut(&gkey))
            .and_then(Value::as_object_mut)
        {
            for svc in svcs.keys().cloned().collect::<Vec<_>>() {
                let have: Vec<String> = grant_list(&svcs[&svc])
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                if have.iter().any(|p| p == target) {
                    gone = true;
                    let left: Vec<String> = have.into_iter().filter(|p| p != target).collect();
                    match left.len() {
                        0 => {
                            svcs.remove(&svc);
                        }
                        1 => {
                            svcs.insert(svc, json!(left[0]));
                        }
                        _ => {
                            svcs.insert(svc, json!(left));
                        }
                    }
                }
            }
            emptied = gone && svcs.is_empty();
        }
        if emptied && let Some(g) = auth.get_mut("grants").and_then(Value::as_object_mut) {
            g.remove(&gkey);
        }
    } else if kind == "job" || kind == "helper" {
        let row = auth
            .get_mut("members")
            .and_then(|m| m.get_mut(agent))
            .and_then(|r| r.get_mut(pid))
            .and_then(Value::as_object_mut);
        if let Some(row) = row {
            if kind == "job" {
                if let Some(jobs) = row.get_mut("jobs").and_then(Value::as_object_mut)
                    && jobs.remove(target).is_some()
                {
                    gone = true;
                }
            } else if let Some(hs) = row.get_mut("helpers").and_then(Value::as_array_mut)
                && let Some(i) = hs.iter().position(|h| h.as_str() == Some(target))
            {
                gone = true;
                hs.remove(i);
                if let Some(since) = row.get_mut("helper_since").and_then(Value::as_object_mut) {
                    since.remove(target);
                }
            }
        }
    } else {
        if kind == "domain" && pid == EVERYONE {
            gone = drop_agent_domain(auth, agent, target);
            if !gone
                && Rules::new(c, auth)
                    .agent_domains(agent)
                    .iter()
                    .any(|d| d == target)
            {
                return die(format!(
                    "{target} is declared by the package of {agent} (guest_domains in capabilities.yaml), not kept by a card"
                ));
            }
        }
        let field = ACCESS_KINDS
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or("", |(_, f)| *f);
        let row = auth
            .get_mut("members")
            .and_then(|m| m.get_mut(agent))
            .and_then(|r| r.get_mut(pid))
            .and_then(Value::as_object_mut);
        if let Some(row) = row
            && let Some(listed) = row
                .get_mut("access")
                .and_then(|a| a.get_mut(field))
                .and_then(Value::as_array_mut)
            && let Some(i) = listed.iter().position(|x| x.as_str() == Some(target))
        {
            gone = true;
            listed.remove(i);
            for meta in ["access_since", "access_by"] {
                if let Some(m) = row.get_mut(meta).and_then(Value::as_object_mut) {
                    m.remove(&format!("{kind}:{target}"));
                }
            }
        }
    }
    if !gone {
        return die(format!("the ledger has no {key} (removed already?)"));
    }
    Ok(vec![format!(
        "{pid} no longer has {kind} {target} for {agent}"
    )])
}

/// `drop_agent_domain`: takes one host out of the agent's kept guest domains (as written: nothing else is checked). True
/// if it was there.
fn drop_agent_domain(auth: &mut Value, agent: &str, host: &str) -> bool {
    let Some(kept) = auth.get_mut("agent_access").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(row) = kept.get_mut(agent).and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(domains) = row.get_mut("domains").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(i) = domains.iter().position(|d| d.as_str() == Some(host)) else {
        return false;
    };
    domains.remove(i);
    let empty = domains.is_empty();
    for meta in ["since", "by"] {
        if let Some(m) = row.get_mut(meta).and_then(Value::as_object_mut) {
            m.remove(host);
        }
    }
    if empty {
        kept.remove(agent);
    }
    true
}

/// `kith-auth handle <id> <platform> <user-id>` (`uid` Some) and `handle <id> <platform> --rm` (None): binds a person to
/// their account on a chat platform, or lets it go. The line it prints.
pub(crate) fn handle(auth: &mut Value, pid: &str, plat: &str, uid: Option<&str>) -> R<Vec<String>> {
    tables(auth);
    need_person(auth, pid)?;
    let Some(uid) = uid else {
        if let Some(h) = auth["people"][pid]
            .get_mut("handles")
            .and_then(Value::as_object_mut)
        {
            h.remove(plat);
        }
        return Ok(vec![format!("{pid}: no {plat} account")]);
    };
    // a Kith account id is the Clerk user id; the others are numbers (Telegram, Discord) or Slack ids. ASCII only: a
    // lookalike digit is no id
    let ok = if plat == "kith" {
        uid.strip_prefix("user_")
            .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric()))
    } else {
        let digits = uid.strip_prefix('-').unwrap_or(uid);
        (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            || (!uid.is_empty()
                && uid
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
    };
    if !ok {
        return usage(format!(
            "{plat} user ids are numbers (Telegram, Discord) or Slack ids like U0123ABC, not usernames{}",
            if plat == "kith" {
                "; kith ids are Kith account ids (user_...)"
            } else {
                ""
            }
        ));
    }
    for (other, p) in people(auth) {
        if other != pid
            && p.get("handles")
                .and_then(|h| h.get(plat))
                .and_then(Value::as_str)
                == Some(uid)
        {
            return die(format!("{plat} {uid} is already {other}"));
        }
    }
    let p = auth["people"][pid]
        .as_object_mut()
        .expect("need_person found it");
    entry(p, "handles").insert(plat.to_owned(), json!(uid));
    Ok(vec![format!("{pid} is {plat} {uid}")])
}
