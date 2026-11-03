//! What `<machine>/settings.json` says about accounts: the reading half of `kernel/lib/kith_settings.py` (`read`,
//! `migrate_accounts`, `accounts`, `pin`, `cross_harness`, `account_policy`) and `migrate_file`, the one write `kith account
//! list` makes. A file that cannot be used sets nothing, as there.

use super::{Ctx, Map, family, pyjson, split};
use fs2::FileExt as _;
use serde_json::Value;
use std::io::Write as _;
use std::path::Path;

/// Modes kith no longer offers, and what a stored one reads as (`RETIRED`).
fn current(mode: &str) -> &str {
    match mode {
        "manual" => "default",
        "dontAsk" => "auto",
        m => m,
    }
}

const PERMISSION_MODES: [&str; 5] = [
    "default",
    "acceptEdits",
    "plan",
    "auto",
    "bypassPermissions",
];
const AT_LIMITS: [&str; 3] = ["ask", "tell", "hold"];
const POLICIES: [&str; 2] = ["fill", "spread"];

/// A monthly limit or a chain cap: dollars above 0 (a bool is no number).
pub fn valid_limit(v: &Value) -> bool {
    v.is_number() && v.as_f64().is_some_and(|n| n > 0.0 && n < 1e7)
}

fn list_of_strings(v: Option<&Value>) -> bool {
    match v {
        None => true,
        Some(Value::Array(a)) => a.iter().all(Value::is_string),
        Some(_) => false,
    }
}

/// Whether one level of the file (the file itself, or one agent: `top` says which) is usable.
fn level_ok(d: &Map, top: bool) -> bool {
    if !list_of_strings(d.get("backups")) || !list_of_strings(d.get("accounts")) {
        return false;
    }
    if d.get("account")
        .is_some_and(|a| a.as_str().is_none_or(str::is_empty))
    {
        return false;
    }
    if d.get("account_cross_harness")
        .is_some_and(|c| !c.is_boolean())
    {
        return false;
    }
    if let Some(pin) = d.get("pin") {
        let ok = !top
            && pin.as_object().is_some_and(|p| {
                p.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty())
                    && p.get("only").is_none_or(Value::is_boolean)
            });
        if !ok {
            return false;
        }
    }
    for (key, choices) in [
        ("permission_mode", &PERMISSION_MODES[..]),
        ("at_limit", &AT_LIMITS[..]),
        ("account_policy", &POLICIES[..]),
    ] {
        if d.get(key)
            .is_some_and(|v| !v.as_str().is_some_and(|v| choices.contains(&v)))
        {
            return false;
        }
    }
    d.get("limit").is_none_or(valid_limit)
}

/// `kith_settings.read()` without the account migration: `None` when the file is there and cannot be used, an empty map
/// when it is not there. A retired permission mode reads as the mode that replaced it.
pub fn read_valid(machine: &Path) -> Option<Map> {
    let Ok(text) = std::fs::read_to_string(machine.join("settings.json")) else {
        return Some(Map::new());
    };
    let Ok(Value::Object(mut s)) = serde_json::from_str::<Value>(&text) else {
        return None;
    };
    let retire = |d: &mut Map| {
        if let Some(Value::String(m)) = d.get("permission_mode") {
            let now = current(m).to_owned();
            d.insert("permission_mode".to_owned(), Value::String(now));
        }
    };
    retire(&mut s);
    match s.get_mut("agents") {
        None => {}
        Some(Value::Object(agents)) => {
            for a in agents.values_mut().filter_map(Value::as_object_mut) {
                retire(a);
            }
            if !agents.values().all(Value::is_object) {
                return None;
            }
        }
        Some(_) => return None,
    }
    if !list_of_strings(s.get("plugins")) {
        return None;
    }
    if !level_ok(&s, true)
        || !s
            .get("agents")
            .and_then(Value::as_object)
            .is_none_or(|a| a.values().all(|a| a.as_object().is_some_and(|a| level_ok(a, false))))
    {
        return None;
    }
    match s.get("delegation") {
        None => {}
        Some(Value::Object(d)) => {
            if d.get("chain_cost_cap_usd")
                .is_some_and(|c| c.as_str() != Some("none") && !valid_limit(c))
            {
                return None;
            }
        }
        Some(_) => return None,
    }
    Some(s)
}

/// The file's settings as a caller that cannot name an agent reads them: nothing when the file cannot be used.
pub fn read_settings(machine: &Path) -> Map {
    read_valid(machine).unwrap_or_default()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn dedup(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in ids {
        if !i.is_empty() && !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

fn array(ids: Vec<String>) -> Value {
    Value::Array(ids.into_iter().map(Value::String).collect())
}

/// Whether `entry` is of harness `own`'s family (`_family_of`).
fn family_of(entry: &str, own: &str) -> bool {
    let h = split(entry).0.split(':').next().unwrap_or_default();
    family(own).contains(&h)
}

/// `migrate_accounts`, in place: the old main account and backups chain (`account`, `backups`, `agents.<a>.account`,
/// `agents.<a>.backups`) as the account set, a pin and per-agent subsets. Idempotent. `harness_of` is the harness an
/// agent's `agent.md` names.
fn migrate(s: &mut Map, harness_of: &dyn Fn(&str) -> String) {
    let old_main = s
        .get("account")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let old_backups = strings(s.get("backups"));
    let agent_has_old = s
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|a| {
            a.values().filter_map(Value::as_object).any(|a| a.contains_key("account") || a.contains_key("backups"))
        });
    if !(s.contains_key("account") || s.contains_key("backups") || agent_has_old) {
        return;
    }
    let harness_of_id = |i: &str| {
        i.split(':')
            .next()
            .unwrap_or_default()
            .replace("claude-api", "claude")
    };
    let main_family = if old_main.is_empty() {
        "claude".to_owned()
    } else {
        harness_of_id(&old_main)
    };
    let mixed = old_backups.iter().any(|i| harness_of_id(i) != main_family);
    if mixed && !s.contains_key("account_cross_harness") {
        s.insert("account_cross_harness".to_owned(), Value::Bool(true));
    }
    if !s.contains_key("accounts") && (!old_main.is_empty() || !old_backups.is_empty()) {
        let mut head = vec![if old_main.is_empty() { "claude".to_owned() } else { old_main.clone() }];
        if old_main.starts_with("codex") {
            head.push("claude".to_owned());
        }
        s.insert(
            "accounts".to_owned(),
            array(dedup(head.into_iter().chain(old_backups.iter().cloned()))),
        );
    }
    if let Some(Value::Object(agents)) = s.get_mut("agents") {
        for (name, a) in agents.iter_mut() {
            let Some(a) = a.as_object_mut() else { continue };
            let own_main = a.get("account").and_then(Value::as_str).filter(|m| !m.is_empty()).map(str::to_owned);
            let own_backups = a.get("backups").and_then(Value::as_array).map(|_| strings(a.get("backups")));
            if own_main.is_some() || own_backups.is_some() {
                let h = harness_of(name);
                let main = [own_main.as_deref(), Some(old_main.as_str())]
                    .into_iter()
                    .flatten()
                    .find(|m| !m.is_empty() && family_of(m, &h))
                    .map_or(h.clone(), str::to_owned);
                let chain = own_backups.unwrap_or_else(|| {
                    old_backups
                        .iter()
                        .filter(|i| mixed || family_of(i, &h))
                        .cloned()
                        .collect()
                });
                if !a.contains_key("accounts") {
                    a.insert(
                        "accounts".to_owned(),
                        array(dedup(std::iter::once(main).chain(chain))),
                    );
                }
            }
            if let Some(m) = own_main
                && !a.contains_key("pin")
            {
                let mut pin = Map::new();
                pin.insert("id".to_owned(), Value::String(m));
                pin.insert("only".to_owned(), Value::Bool(false));
                a.insert("pin".to_owned(), Value::Object(pin));
            }
            a.shift_remove("account");
            a.shift_remove("backups");
        }
    }
    s.shift_remove("account");
    s.shift_remove("backups");
}

/// `kith_settings.read()[0]`: what the file sets, in the account-set shape, or nothing when it cannot be used.
pub fn read(ctx: &Ctx<'_>) -> Map {
    let Some(mut s) = read_valid(&ctx.machine) else {
        return Map::new();
    };
    migrate(&mut s, &|agent| {
        super::agent_harness(super::agent_md_of(ctx, agent).as_deref())
    });
    s
}

fn agent_entry<'a>(s: &'a Map, agent: &str) -> Option<&'a Map> {
    if agent.is_empty() {
        return None;
    }
    s.get("agents")?.get(agent)?.as_object()
}

/// `(ids or None, where)`: the set that applies to the agent: its own (`"agent"`, even when empty), else the global one
/// (`"global"`); `None` when the file has no set at all (every agent on its harness's default login).
pub fn accounts(s: &Map, agent: &str) -> (Option<Vec<String>>, &'static str) {
    if let Some(own) = agent_entry(s, agent).and_then(|a| a.get("accounts")) {
        return (Some(strings(Some(own))), "agent");
    }
    (s.get("accounts").map(|a| strings(Some(a))), "global")
}

/// An agent's pin: the account its runs start on, and whether it is the only one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub id: String,
    pub only: bool,
}

pub fn pin(s: &Map, agent: &str) -> Option<Pin> {
    let p = agent_entry(s, agent)?.get("pin")?.as_object()?;
    Some(Pin {
        id: p.get("id")?.as_str()?.to_owned(),
        only: p.get("only").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Whether accounts of another harness serve as fallbacks for the agent: its own switch, else the global one.
pub fn cross_harness(s: &Map, agent: &str) -> bool {
    agent_entry(s, agent)
        .and_then(|a| a.get("account_cross_harness"))
        .or_else(|| s.get("account_cross_harness"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn policy(s: &Map) -> String {
    s.get("account_policy")
        .and_then(Value::as_str)
        .unwrap_or("fill")
        .to_owned()
}

/// `kith_settings.migrate_file()`: rewrite the file in the account-set shape when it is still in the older one (or names a
/// retired mode). Takes the same lock as the Python writers (`settings.json.lock`, made even when there is nothing to
/// write). Whether it wrote.
pub fn migrate_file(ctx: &Ctx<'_>) -> std::io::Result<bool> {
    std::fs::create_dir_all(&ctx.machine)?;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(ctx.machine.join("settings.json.lock"))?;
    lock.lock_exclusive()?;
    let file = ctx.machine.join("settings.json");
    let Ok(Ok(Value::Object(raw))) = std::fs::read_to_string(&file).map(|t| serde_json::from_str::<Value>(&t)) else {
        return Ok(false);
    };
    let Some(mut s) = read_valid(&ctx.machine) else {
        return Ok(false);
    };
    migrate(&mut s, &|agent| {
        super::agent_harness(super::agent_md_of(ctx, agent).as_deref())
    });
    if raw == s {
        return Ok(false);
    }
    write(&file, &s)?;
    Ok(true)
}

/// The whole file, atomically (a reader never sees half of it), with the permissions a plain write would give it.
fn write(file: &Path, s: &Map) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let tmp = super::temp_in(dir, "settings.json.", ".tmp");
    let result = (|| {
        let mut f = super::create_new(&tmp, 0o666)?;
        f.write_all((pyjson::dumps_pretty(&Value::Object(s.clone())) + "\n").as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
