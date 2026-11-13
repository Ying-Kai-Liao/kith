//! The secret writers `kith-auth` routes here on the authority tombstone (S-b): `set` (and `set --stdin-json [--grant]`),
//! `grant`, `revoke` / `disconnect`, and the `env` writer. Each is a port of its verb over the profiles, grants and env
//! tables, in the ONE locked step that writes, so the checks and the write see the same tables.
//!
//! The core never sees a value. `kith-auth` writes the Keychain item first (and reads it back), then tells the core the
//! NAMES of the variables it stored; the core records the reference `keychain:kith/<profile>#VAR` for each. A crash
//! between the two leaves an orphan Keychain item (or a rotated value under the same reference), never a grant without
//! its item. A key set after the step is never written as a `keychain:` envelope record either: the rollback keeps a
//! Keychain value that differs from the sealed one, so what the Keychain holds survives as it stands.
//!
//! The rules ported: a profile is `<service>/<account>`; a variable name matches the pattern and is one the service
//! declares; an `agent@person` grant needs that person now (checked in this step, so a grant can not land for an id that
//! was just removed); a grant's profiles start with `<service>/`; a host connector's `<id>/host` profile is marked
//! `{"store": "host"}`; a NEW profile's owner is stored as `kith-auth`'s `owner_of` answers it (the owner's id for a
//! personal service, NULL for a shared one), while an existing profile keeps its `person`, `*` and `with` as they are.
//! There is no lent check at write time: `granted()` checks at use, and the secrets view keeps that. One thing the file
//! never had: a caller whose environment names a non-owner `KITH_PERSON` is refused (the file's permissions stood in).

use kith_kernel::secrets::{self, Ctx, Die, Rules};
use serde_json::{Map, Value, json};

use super::revoke::Life;
use super::write::{self, R, die, entry, need_person, truthy, usage};

/// The keys of a profile that are not variables.
const PROFILE_KEYS: [&str; 3] = ["person", "with", "browser"];

/// `HOST_STORE`: the mark of a host connector's profile, whose secrets (if any) the host keeps.
fn host_mark() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("store".to_owned(), json!("host"));
    m
}

/// A profile `disconnect` may remove: only the host-store mark (`HOST_STORE`), no variable of its own, nothing lent, no
/// browser. Its `person` is ignored: the core fills it for a personal service, the file never had it. A keyed profile
/// is the owner's to keep, so this is the only kind a write ever drops.
pub(super) fn is_host_store(p: &Value) -> bool {
    p.as_object().is_some_and(|o| {
        o.get("store") == Some(&json!("host"))
            && o.keys().all(|k| k == "store" || k == "person")
            && !o.contains_key("browser")
    })
}

fn plain(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control)
}

/// `^[a-z0-9][a-z0-9._-]{0,39}$`: `SERVICE_RE` and `ACCOUNT_RE` are the same.
fn is_label(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.len() <= 40
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// `VAR_RE`: `^[A-Za-z_][A-Za-z0-9_]{0,63}$`.
fn is_var(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.len() <= 64
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `AGENT_RE`: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
fn is_agent(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A top-level table of auth.json (`profiles`, `grants`, `env`): `load_auth` makes them, and the view leaves them out
/// when empty.
fn top<'a>(auth: &'a mut Value, name: &str) -> &'a mut Map<String, Value> {
    write::tables(auth);
    let all = auth.as_object_mut().expect("tables() made an object");
    entry(all, name)
}

fn peek<'a>(auth: &'a Value, name: &str) -> Option<&'a Map<String, Value>> {
    auth.get(name).and_then(Value::as_object)
}

/// `grant_list`: the profiles of a grant, which is one name or a list of them.
fn grant_list(g: Option<&Value>) -> Vec<String> {
    match g {
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        Some(Value::Array(l)) => l
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// One profile stays the plain string, as `kith-auth` writes it.
fn grant_value(list: &[String]) -> Value {
    if list.len() > 1 {
        json!(list)
    } else {
        json!(list[0])
    }
}

/// `once_owner_only`'s cousin: the secret writers are the owner's, and a guest's MCP server runs outside the box.
fn owner_only(c: &Ctx, auth: &Value) -> R<()> {
    let me = Rules::new(c, auth).acting();
    if me == "-" || !secrets::is_owner(auth, &me) {
        return usage("refused: not available to this run");
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Kernel,
    Mac,
    Agent,
}

/// `service_file`: `kernel/services/<name>.json`, else this Mac's, else the one the agent brings in its own role.
fn spec_of(c: &Ctx, life: &Life, svc: &str, agent: Option<&str>) -> R<Option<(Origin, Value)>> {
    if svc.is_empty() || svc.contains(['/', '\\']) || svc == "." || svc == ".." {
        return Ok(None);
    }
    let file = format!("{svc}.json");
    let mut dirs = vec![
        (Origin::Kernel, life.kernel.join("services")),
        (Origin::Mac, life.machine.join("services")),
    ];
    if let Some(a) = agent.filter(|a| c.agent_exists(a)) {
        dirs.push((Origin::Agent, c.package_dir(a).join("services")));
    }
    for (origin, dir) in dirs {
        let path = dir.join(&file);
        if !path.is_file() {
            continue;
        }
        let spec = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(Value::is_object)
            .ok_or_else(|| Die {
                msg: format!("the service spec of {svc} cannot be read"),
                code: 1,
            })?;
        return Ok(Some((origin, spec)));
    }
    Ok(None)
}

fn need_service(c: &Ctx, life: &Life, svc: &str, agent: Option<&str>) -> R<(Origin, Value)> {
    spec_of(c, life, svc, agent)?.map_or_else(
        || {
            die(format!(
                "no service '{svc}' (kernel/services/, or an agent's services/)"
            ))
        },
        Ok,
    )
}

/// `personal(svc)`: a service's profiles belong to someone unless marked shared (logins and browser profiles are).
fn personal(c: &Ctx, life: &Life, svc: &str) -> R<bool> {
    Ok(match spec_of(c, life, svc, None)? {
        None => false,
        Some((_, spec)) => match spec.get("personal") {
            Some(v) => truthy(Some(v)),
            None => matches!(
                spec.get("kind").and_then(Value::as_str),
                Some("login" | "browser")
            ),
        },
    })
}

/// `ensure_profile`: the profile's entry, a host connector's `<id>/host` marked as the host's own. A profile that is new
/// gets its owner as `owner_of` would answer it (m0021): the owner's id (the text `the owner` while none is named) for a
/// personal service, none for a shared one.
fn ensure_profile(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    svc: &str,
    profile: &str,
    agent: Option<&str>,
) -> R<()> {
    let fresh = !peek(auth, "profiles").is_some_and(|p| p.contains_key(profile));
    let owner = if fresh && personal(c, life, svc)? {
        Some(secrets::owner(auth).unwrap_or_else(|| "the owner".to_owned()))
    } else {
        None
    };
    let host = profile == format!("{svc}/host")
        && matches!(spec_of(c, life, svc, agent)?, Some((Origin::Mac, s)) if !truthy(s.get("secrets")));
    let prof = entry(top(auth, "profiles"), profile);
    if prof.is_empty() && host {
        *prof = host_mark();
    }
    if let Some(o) = owner {
        prof.insert("person".to_owned(), Value::String(o));
    }
    Ok(())
}

/// `cmd_set_stdin` / `cmd_set`, after the Keychain took the values: records `keychain:kith/<profile>#VAR` for each name
/// (a variable already there keeps its place), and with `grant` adds the profile to that agent's grants for the service.
pub(crate) fn secret_set(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    profile: &str,
    vars: &[&str],
    grant: Option<&str>,
) -> R<Value> {
    owner_only(c, auth)?;
    let (svc, account) = profile.split_once('/').unwrap_or((profile, ""));
    if !(is_label(svc) && is_label(account)) {
        return usage(
            "profiles are named <service>/<account> (account: a-z 0-9 . _ -, at most 40), e.g. github/work",
        );
    }
    if grant.is_some_and(|g| !is_agent(g)) {
        return usage("refused: not an agent name; nothing stored");
    }
    if vars.is_empty() {
        return usage("refused: stdin must be one JSON object of VAR: value; nothing stored");
    }
    let (_, spec) = need_service(c, life, svc, grant)?;
    let declared: Vec<&str> = spec
        .get("secrets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if vars.iter().any(|v| !(is_var(v) && declared.contains(v))) {
        return usage(format!(
            "refused: a name is not one {svc} declares; nothing stored"
        ));
    }
    let kc = format!("kith/{profile}");
    ensure_owned(auth, c, life, svc, profile)?;
    let refs = entry(top(auth, "profiles"), profile);
    for var in vars {
        refs.insert((*var).to_owned(), json!(format!("keychain:{kc}#{var}")));
    }
    if let Some(agent) = grant {
        let by_service = entry(top(auth, "grants"), agent);
        let mut have = grant_list(by_service.get(svc));
        if !have.iter().any(|p| p == profile) {
            have.push(profile.to_owned());
        }
        by_service.insert(svc.to_owned(), grant_value(&have));
    }
    Ok(json!({ "ok": true }))
}

/// A new profile gets its owner (see [`ensure_profile`]) without the host-store mark, which only a grant gives.
fn ensure_owned(auth: &mut Value, c: &Ctx, life: &Life, svc: &str, profile: &str) -> R<()> {
    if peek(auth, "profiles").is_some_and(|p| p.contains_key(profile)) {
        return Ok(());
    }
    let owner = if personal(c, life, svc)? {
        Some(secrets::owner(auth).unwrap_or_else(|| "the owner".to_owned()))
    } else {
        None
    };
    let prof = entry(top(auth, "profiles"), profile);
    if let Some(o) = owner {
        prof.insert("person".to_owned(), Value::String(o));
    }
    Ok(())
}

/// What `do_grant` is asked: agents (`agent` or `agent@person`), the service, the profiles, and (`connect`) the grants the
/// caller saw, so a concurrent change is refused rather than overwritten.
pub(crate) struct GrantArgs<'a> {
    pub agents: &'a [&'a str],
    pub service: &'a str,
    pub profiles: &'a [&'a str],
    pub known_only: bool,
    pub if_have: Option<&'a Map<String, Value>>,
}

/// `do_grant` and `connect`'s write.
pub(crate) fn secret_grant(auth: &mut Value, c: &Ctx, life: &Life, a: &GrantArgs) -> R<Value> {
    owner_only(c, auth)?;
    let svc = a.service;
    let base = |n: &str| n.split_once('@').map_or(n, |(b, _)| b).to_owned();
    if a.agents.is_empty() {
        return usage("no agent named");
    }
    if a.agents.iter().any(|n| !plain(n)) || !plain(svc) || a.profiles.iter().any(|p| !plain(p)) {
        return usage("a name may not be empty or hold a control character");
    }
    need_service(c, life, svc, a.agents.first().map(|n| base(n)).as_deref())?;
    if a.known_only {
        for n in a.agents {
            if !c.agent_exists(&base(n)) && !matches!(*n, "bridge" | "serve") {
                return die(format!("no agent '{n}' (kith ls)"));
            }
        }
    }
    for n in a.agents {
        if let Some((_, person)) = n.split_once('@') {
            need_person(auth, person)?;
        }
    }
    if a.profiles.is_empty()
        || a.profiles
            .iter()
            .any(|p| !p.starts_with(&format!("{svc}/")))
    {
        return usage(format!("a {svc} grant takes {svc}/<account> profiles"));
    }
    if let Some(seen) = a.if_have {
        for (agent, want) in seen {
            let now = grant_list(
                peek(auth, "grants")
                    .and_then(|g| g.get(agent))
                    .and_then(|g| g.get(svc)),
            );
            let want: Vec<String> = want
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if now != want {
                return die(format!(
                    "{agent}'s {svc} grant changed while this ran; look again and retry"
                ));
            }
        }
    }
    let first = a.agents.first().map(|n| base(n));
    for p in a.profiles {
        ensure_profile(auth, c, life, svc, p, first.as_deref())?;
    }
    let list: Vec<String> = a.profiles.iter().map(|p| (*p).to_owned()).collect();
    for agent in a.agents {
        entry(top(auth, "grants"), agent).insert(svc.to_owned(), grant_value(&list));
    }
    Ok(json!({ "ok": true }))
}

/// `cmd_revoke` (all of a service's grants, or one `profile`) and `cmd_disconnect` (`drop_host`): a host profile nobody
/// else holds goes with its last grant; a keyed profile is the owner's to keep.
pub(crate) fn secret_revoke(
    auth: &mut Value,
    c: &Ctx,
    agent: &str,
    svc: &str,
    profile: Option<&str>,
    drop_host: bool,
) -> R<Value> {
    owner_only(c, auth)?;
    if !plain(agent) || !plain(svc) || profile.is_some_and(|p| !plain(p)) {
        return usage("a name may not be empty or hold a control character");
    }
    let grants = top(auth, "grants");
    if let Some(p) = profile {
        let by_service = entry(grants, agent);
        let left: Vec<String> = grant_list(by_service.get(svc))
            .into_iter()
            .filter(|x| x != p)
            .collect();
        if left.is_empty() {
            by_service.shift_remove(svc);
        } else {
            by_service.insert(svc.to_owned(), grant_value(&left));
        }
        if by_service.is_empty() {
            grants.shift_remove(agent);
        }
        return Ok(json!({ "ok": true }));
    }
    if !drop_host {
        // `revoke <agent> <service>`: the file keeps the agent's (now empty) row, which the tables do not tell apart
        if let Some(by_service) = grants.get_mut(agent).and_then(Value::as_object_mut) {
            by_service.shift_remove(svc);
        }
        return Ok(json!({ "ok": true }));
    }
    let held = grant_list(grants.get(agent).and_then(|g| g.get(svc)));
    if held.is_empty() {
        return Ok(json!({ "ok": true }));
    }
    if let Some(by_service) = grants.get_mut(agent).and_then(Value::as_object_mut) {
        by_service.shift_remove(svc);
        if by_service.is_empty() {
            grants.shift_remove(agent);
        }
    }
    for p in held {
        let held_by_another = peek(auth, "grants").is_some_and(|g| {
            g.values().any(|by| {
                by.as_object()
                    .is_some_and(|by| by.values().any(|v| grant_list(Some(v)).contains(&p)))
            })
        });
        let host = peek(auth, "profiles")
            .and_then(|ps| ps.get(&p))
            .is_some_and(is_host_store);
        if host && !held_by_another {
            top(auth, "profiles").shift_remove(&p);
        }
    }
    Ok(json!({ "ok": true }))
}

/// The `env` writer: `VAR=value` sets (a new variable goes last, a changed one keeps its place), `VAR=` clears. A
/// service left with no variable, and an agent left with no service, go.
pub(crate) fn secret_env(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    agent: &str,
    svc: &str,
    pairs: &[(&str, &str)],
) -> R<Value> {
    owner_only(c, auth)?;
    if !plain(agent) || !plain(svc) {
        return usage("a name may not be empty or hold a control character");
    }
    need_service(c, life, svc, Some(agent))?;
    if pairs.is_empty() {
        return Ok(json!({ "ok": true }));
    }
    if pairs.iter().any(|(var, _)| !plain(var)) {
        return usage("expected VAR=value, got ''");
    }
    let env = top(auth, "env");
    let mine = entry(entry(env, agent), svc);
    for (var, value) in pairs {
        if value.is_empty() {
            mine.shift_remove(*var);
        } else {
            mine.insert((*var).to_owned(), json!(value));
        }
    }
    if mine.is_empty() {
        entry(env, agent).shift_remove(svc);
    }
    if env
        .get(agent)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        env.shift_remove(agent);
    }
    Ok(json!({ "ok": true }))
}

/// `cmd_owner`: whose a profile is. `-` makes it shared (stored `*`), `default` stores what `owner_of` answers for its
/// service (the owner's id for a personal one, none for a shared one: the table has no "unset"), a person's id needs
/// that person now, in this same step.
pub(crate) fn secret_owner(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    profile: &str,
    person: &str,
) -> R<Value> {
    owner_only(c, auth)?;
    if !peek(auth, "profiles").is_some_and(|p| p.contains_key(profile)) {
        return die(format!("no profile '{profile}'"));
    }
    let set = match person {
        "-" => Some("*".to_owned()),
        "default" => {
            let svc = profile.split_once('/').map_or(profile, |(s, _)| s);
            if personal(c, life, svc)? {
                Some(secrets::owner(auth).unwrap_or_else(|| "the owner".to_owned()))
            } else {
                None
            }
        }
        id => {
            need_person(auth, id)?;
            Some(id.to_owned())
        }
    };
    let prof = entry(top(auth, "profiles"), profile);
    match set {
        Some(p) => {
            prof.insert("person".to_owned(), Value::String(p));
        }
        None => {
            prof.shift_remove("person");
        }
    }
    Ok(json!({ "ok": true }))
}

/// `cmd_share`: lends a profile to people (`remove`: takes it back). Every id must be a person now, in this step; the
/// list keeps its order, drops repeats, and an empty one is none.
pub(crate) fn secret_share(
    auth: &mut Value,
    c: &Ctx,
    profile: &str,
    ids: &[&str],
    remove: bool,
) -> R<Value> {
    owner_only(c, auth)?;
    if !peek(auth, "profiles").is_some_and(|p| p.contains_key(profile)) {
        return die(format!("no profile '{profile}'"));
    }
    for id in ids {
        need_person(auth, id)?;
    }
    let prof = entry(top(auth, "profiles"), profile);
    let have = grant_list(prof.get("with"));
    let lent: Vec<String> = if remove {
        have.into_iter()
            .filter(|x| !ids.contains(&x.as_str()))
            .collect()
    } else {
        let mut all = have;
        for id in ids {
            if !all.iter().any(|x| x == id) {
                all.push((*id).to_owned());
            }
        }
        all
    };
    if lent.is_empty() {
        prof.shift_remove("with");
    } else {
        prof.insert("with".to_owned(), json!(lent));
    }
    Ok(json!({ "ok": true }))
}

/// `browser <profile> [<browser>]`: a missing profile is made with the owner `owner_of` gives (a browser profile is
/// personal), and the browser is recorded when named. The note and the grantee list are the caller's to print.
pub(crate) fn secret_browser(
    auth: &mut Value,
    c: &Ctx,
    life: &Life,
    profile: &str,
    browser: Option<&str>,
) -> R<Value> {
    owner_only(c, auth)?;
    let name = if profile.starts_with("browser/") {
        profile.to_owned()
    } else {
        format!("browser/{profile}")
    };
    // no name rule beyond what the file has: `kith-auth browser` accepts any name
    if !plain(&name) || browser.is_some_and(|b| !plain(b)) {
        return usage("a name may not be empty or hold a control character");
    }
    ensure_owned(auth, c, life, "browser", &name)?;
    if let Some(b) = browser {
        entry(top(auth, "profiles"), &name).insert("browser".to_owned(), json!(b));
    }
    Ok(json!({ "ok": true }))
}
