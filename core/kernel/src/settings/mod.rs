//! `kith kernel settings ...`: the command line of `kernel/lib/kith_settings.py` in Rust. The reads (`KEY`, `KEY AGENT`,
//! `call_model`, `accounts`, `pin`, `limit`, `delegation.chain_cost_cap_usd`, `cap`, `reads-others`) and the writes
//! (`set permission_mode|call_model|at_limit`, `set limit`, `set delegation.chain_cost_cap_usd`, with `--json` for the
//! per-agent ones) print what the Python prints and exit with its codes; the difftest golden `settings.json` holds its
//! answers. `kith_settings.py` stays the one the launch calls until `settings` is in `lib/core-switched.txt`.
//!
//! The file is read by `accounts::settings` (the one reader of `<machine>/settings.json`: the old account keys read as the
//! new ones). The old-file migration is the same as Python's: every write goes through that reader, so a write persists
//! the new shape, and `accounts::settings::migrate_file` is `kith account list`'s own. Writes take `settings.json.lock`
//! (flock, as Python) and replace the file atomically.
//!
//! Where the Python would crash with a traceback (a settings value that is a list of numbers printed with `KEY AGENT`, a
//! caps file that is not UTF-8) this answers a short line and exit 1; a caps file that is not UTF-8 counts as a reader
//! (`reads_others` true), never as `reads_others: false`.
#![allow(clippy::cast_possible_truncation, clippy::float_cmp)]

mod pyval;
#[cfg(test)]
mod tests;

use crate::accounts::{Ctx, settings};
use crate::cost::pyfmt;
use crate::secrets::pyjson;
use fs2::FileExt as _;
use serde_json::{Map as JsonMap, Value, json};
use std::path::PathBuf;

type Map = JsonMap<String, Value>;

/// `sys.exit(__doc__.strip())`.
const USAGE: &str = include_str!("usage.txt");

const PERMISSION_MODES: [&str; 5] = [
    "default",
    "acceptEdits",
    "plan",
    "auto",
    "bypassPermissions",
];
const AT_LIMITS: [&str; 3] = ["ask", "tell", "hold"];
const POLICIES: [&str; 2] = ["fill", "spread"];
/// The default cost cap of a delegation chain, in dollars.
const CHAIN_CAP_USD: i64 = 5;
/// The keys `KEY` and `KEY AGENT` print (`DEFAULTS`), besides `pin`.
const KEYS: [&str; 5] = [
    "permission_mode",
    "plugins",
    "call_model",
    "account_policy",
    "accounts",
];

/// What a run prints and exits with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Out {
    fn line(text: &str) -> Out {
        Out {
            stdout: format!("{text}\n"),
            ..Out::default()
        }
    }

    fn fail(text: &str) -> Out {
        Out {
            stderr: format!("{text}\n"),
            code: 1,
            ..Out::default()
        }
    }

    fn ok() -> Out {
        Out::default()
    }
}

/// The environment a run reads besides the command line.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env {
    /// One of `KITH_ASK_ID`, `KITH_AGENT`, `CLAUDE_CODE_SESSION_ID` is set: this is an agent's run, not the owner.
    pub agent_run: bool,
}

impl Env {
    pub fn from_process() -> Env {
        Env {
            agent_run: ["KITH_ASK_ID", "KITH_AGENT", "CLAUDE_CODE_SESSION_ID"]
                .iter()
                .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty())),
        }
    }
}

fn file(ctx: &Ctx<'_>) -> PathBuf {
    ctx.machine.join("settings.json")
}

// --- reading ------------------------------------------------------------------------------------------------------------

/// `kith_settings.read()[0]`: what the file sets in the account-set shape, nothing when it cannot be used.
fn read(ctx: &Ctx<'_>) -> Map {
    settings::read(ctx)
}

fn agent_of<'a>(s: &'a Map, agent: &str) -> Option<&'a Map> {
    s.get("agents")?.get(agent)?.as_object()
}

/// `read()` with the reason a file cannot be used, for a write to refuse with (`ValueError(why)`).
fn read_checked(ctx: &Ctx<'_>) -> Result<Map, String> {
    let path = file(ctx);
    let name = path.display().to_string();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => {
            let os = e.to_string();
            let os = os.split(" (os error").next().unwrap_or(&os);
            let errno = e
                .raw_os_error()
                .map_or(String::new(), |n| format!("[Errno {n}] "));
            return Err(format!("{name} is unreadable ({errno}{os}: '{name}')"));
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            let at = e.utf8_error().valid_up_to();
            let byte = e.as_bytes()[at];
            let why = if e.utf8_error().error_len().is_none() {
                "unexpected end of data"
            } else if byte & 0xc0 == 0x80 || matches!(byte, 0xc0 | 0xc1 | 0xf5..=0xff) {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            return Err(format!(
                "{name} is unreadable ('utf-8' codec can't decode byte 0x{byte:02x} in position {at}: {why})"
            ));
        }
    };
    if let Some(e) = pyjson::error_of(&text) {
        return Err(format!("{name} is unreadable ({e})"));
    }
    if settings::read_valid(&ctx.machine).is_some() {
        return Ok(read(ctx));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(s)) => Err(diagnose(&name, s)),
        Ok(_) => Err(format!("{name} is not a JSON object")),
        // a text only Python reads (NaN, Infinity)
        Err(e) => Err(format!("{name} is unreadable ({e})")),
    }
}

/// The reason `kith_settings.read()` gives for a file that parses and is not usable, in its order.
fn diagnose(name: &str, mut s: Map) -> String {
    let current = |d: &mut Map| {
        if let Some(Value::String(m)) = d.get("permission_mode") {
            let now = match m.as_str() {
                "manual" => "default",
                "dontAsk" => "auto",
                m => m,
            }
            .to_owned();
            d.insert("permission_mode".to_owned(), Value::String(now));
        }
    };
    current(&mut s);
    if let Some(Value::Object(agents)) = s.get_mut("agents") {
        for a in agents.values_mut().filter_map(Value::as_object_mut) {
            current(a);
        }
    }
    let agents_ok = match s.get("agents") {
        None => true,
        Some(Value::Object(a)) => a.values().all(Value::is_object),
        Some(_) => false,
    };
    if !agents_ok {
        return format!("{name}: agents is not {{\"<agent>\": {{...}}}}");
    }
    let plugins_ok = match s.get("plugins") {
        None => true,
        Some(Value::Array(p)) => p.iter().all(Value::is_string),
        Some(_) => false,
    };
    if !plugins_ok {
        return format!("{name}: plugins is not a list of plugin ids");
    }
    let mut levels: Vec<(String, &Map)> = vec![(String::new(), &s)];
    if let Some(Value::Object(agents)) = s.get("agents") {
        levels.extend(
            agents
                .iter()
                .filter_map(|(n, a)| a.as_object().map(|a| (format!("agents.{n}."), a))),
        );
    }
    for (at, d) in levels {
        for key in ["backups", "accounts"] {
            let ok = match d.get(key) {
                None => true,
                Some(Value::Array(b)) => b.iter().all(Value::is_string),
                Some(_) => false,
            };
            if !ok {
                return format!("{name}: {at}{key} is not a list of account ids");
            }
        }
        if d.get("account")
            .is_some_and(|a| a.as_str().is_none_or(str::is_empty))
        {
            return format!("{name}: {at}account is not an account id");
        }
        if d.get("account_cross_harness")
            .is_some_and(|c| !c.is_boolean())
        {
            return format!("{name}: {at}account_cross_harness is not true or false");
        }
        if let Some(pin) = d.get("pin") {
            let ok = !at.is_empty()
                && pin.as_object().is_some_and(|p| {
                    p.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty())
                        && p.get("only").is_none_or(Value::is_boolean)
                });
            if !ok {
                return format!("{name}: {at}pin is not {{\"id\": ID, \"only\": true|false}}");
            }
        }
        for (k, v) in d {
            let choices: &[&str] = match k.as_str() {
                "permission_mode" => &PERMISSION_MODES,
                "at_limit" => &AT_LIMITS,
                "account_policy" => &POLICIES,
                _ => continue,
            };
            if !v.as_str().is_some_and(|v| choices.contains(&v)) {
                return format!(
                    "{name}: {at}{k} {} is not one of {}",
                    pyval::repr(v),
                    choices.join(", ")
                );
            }
        }
        if let Some(l) = d.get("limit")
            && !settings::valid_limit(l)
        {
            return format!(
                "{name}: {at}limit {} is not a number of dollars above 0",
                pyval::repr(l)
            );
        }
    }
    format!(
        "{name}: delegation.chain_cost_cap_usd is not a number of dollars above 0 (or \"none\")"
    )
}

// --- writing ------------------------------------------------------------------------------------------------------------

/// `locked()` + read + change + `write()`: one writer at a time, the rest of the file kept. A refused read or change
/// leaves the file as it is.
fn edit(ctx: &Ctx<'_>, change: impl FnOnce(&mut Map) -> Result<(), String>) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", file(ctx).display());
    std::fs::create_dir_all(&ctx.machine).map_err(io)?;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(ctx.machine.join("settings.json.lock"))
        .map_err(io)?;
    lock.lock_exclusive().map_err(io)?;
    let mut s = read_checked(ctx)?;
    change(&mut s)?;
    settings::write(&file(ctx), &s).map_err(io)
}

fn sub<'a>(s: &'a mut Map, key: &str) -> &'a mut Map {
    let v = s
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().unwrap_or_else(|| unreachable!())
}

/// `set_limit`: the agent's monthly limit; `none`, `off`, `0` or nothing forgets it.
fn set_limit(ctx: &Ctx<'_>, agent: &str, value: &str) -> Result<(), String> {
    let v = value.trim().trim_start_matches('$');
    let amount: Option<Value> = if ["", "none", "off", "0"].contains(&v.to_lowercase().as_str()) {
        None
    } else {
        let n = pyval::float(v).ok_or_else(|| {
            format!(
                "limit {} is not a number of dollars (or none)",
                pyval::str_repr(value)
            )
        })?;
        if !(n > 0.0 && n < 1e7) {
            return Err(format!(
                "limit {} is not a number of dollars above 0",
                pyval::str_repr(value)
            ));
        }
        Some(dollars(n, 2))
    };
    edit(ctx, |s| {
        let agents = sub(s, "agents");
        let a = sub(agents, agent);
        match amount {
            None => {
                a.shift_remove("limit");
            }
            Some(n) => {
                a.insert("limit".to_owned(), n);
            }
        }
        if a.is_empty() {
            agents.shift_remove(agent);
        }
        Ok(())
    })
}

/// `int(n)` when whole, else `round(n, places)`.
fn dollars(n: f64, places: usize) -> Value {
    if n == n.trunc() {
        json!(n as i64)
    } else {
        json!(pyfmt::round(n, places))
    }
}

/// `set_chain_cost_cap`: dollars, `none`/`off` for no cap, `default` (or nothing) to forget the choice.
fn set_chain_cost_cap(ctx: &Ctx<'_>, value: &str) -> Result<(), String> {
    let v = value.trim().trim_start_matches('$');
    let amount: Option<Value> = match v.to_lowercase().as_str() {
        "" | "default" => None,
        "none" | "off" => Some(json!("none")),
        _ => {
            let n = pyval::float(v).ok_or_else(|| {
                format!(
                    "cap {} is not a number of dollars (or none)",
                    pyval::str_repr(value)
                )
            })?;
            if !(n > 0.0 && n < 1e7) {
                return Err(format!(
                    "cap {} is not a number of dollars above 0",
                    pyval::str_repr(value)
                ));
            }
            Some(dollars(n, 4))
        }
    };
    edit(ctx, |s| {
        let d = sub(s, "delegation");
        match amount {
            None => {
                d.shift_remove("chain_cost_cap_usd");
            }
            Some(n) => {
                d.insert("chain_cost_cap_usd".to_owned(), n);
            }
        }
        if d.is_empty() {
            s.shift_remove("delegation");
        }
        Ok(())
    })
}

/// A model Claude Code takes: an alias (optionally with its 1M window), a full `claude-*` id, or kith's own words.
fn valid_call_model(v: &str) -> bool {
    if v == "default" || v == "own" {
        return true;
    }
    let base = v.strip_suffix("[1m]").unwrap_or(v);
    if ["sonnet", "haiku", "opus"].contains(&base) {
        return true;
    }
    base.strip_prefix("claude-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'-')
    })
}

fn bad_call_model(value: &str) -> String {
    format!(
        "call model {} is not sonnet, haiku, opus, a claude-* model id, own or default",
        pyval::str_repr(value)
    )
}

/// `choose_global`: this Mac's choice for every agent; `default` or nothing forgets it.
fn choose_global(ctx: &Ctx<'_>, key: &str, value: &str) -> Result<(), String> {
    edit(ctx, |s| {
        if key == "call_model" && !value.is_empty() && !valid_call_model(value) {
            return Err(bad_call_model(value));
        }
        if value.is_empty() || value == "default" {
            s.shift_remove(key);
        } else {
            s.insert(key.to_owned(), json!(value));
        }
        Ok(())
    })
}

/// `choose`: this Mac's choice for one agent, keeping the rest of the file.
fn choose(ctx: &Ctx<'_>, agent: &str, key: &str, value: &str) -> Result<(), String> {
    edit(ctx, |s| {
        let choices: Option<&[&str]> = match key {
            "permission_mode" => Some(&PERMISSION_MODES),
            "at_limit" => Some(&AT_LIMITS),
            _ => None,
        };
        if let Some(c) = choices
            && !value.is_empty()
            && !c.contains(&value)
        {
            return Err(format!(
                "{key} {} is not one of {}",
                pyval::str_repr(value),
                c.join(", ")
            ));
        }
        let agents = sub(s, "agents");
        let value = if key == "call_model" && value == "default" {
            ""
        } else {
            value
        };
        if key == "call_model" && !value.is_empty() && !valid_call_model(value) {
            return Err(bad_call_model(value));
        }
        if value.is_empty() {
            // forgetting the choice: the agent's own suggestion and kith's default apply again
            if let Some(Value::Object(a)) = agents.get_mut(agent) {
                a.shift_remove(key);
            }
        } else {
            sub(agents, agent).insert(key.to_owned(), json!(value));
        }
        Ok(())
    })
}

// --- the verbs ----------------------------------------------------------------------------------------------------------

/// `stored`: what this Mac has recorded for one agent's setting, `None` when nothing.
fn stored(ctx: &Ctx<'_>, agent: &str, key: &str) -> Value {
    agent_of(&read(ctx), agent)
        .and_then(|a| a.get(key))
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned()
        .unwrap_or(Value::Null)
}

/// `set_with_json`: run the setter, print `{agent, setting, value, previous, changed}`; an error is `{"ok": false, ...}` on
/// stdout, exit 1, nothing on stderr.
fn set_json(
    ctx: &Ctx<'_>,
    key: &str,
    agent: &str,
    setter: impl FnOnce() -> Result<(), String>,
) -> Out {
    let name = match key {
        "permission_mode" => "permission",
        "call_model" => "call-model",
        "at_limit" => "at-limit",
        _ => "limit",
    };
    let before = stored(ctx, agent, key);
    if let Err(e) = setter() {
        return Out {
            stdout: format!(
                "{}\n",
                pyjson::dumps(&json!({"ok": false, "error": format!("kith: {e}")}))
            ),
            code: 1,
            ..Out::default()
        };
    }
    let after = stored(ctx, agent, key);
    let said = json!({"agent": agent, "setting": name, "value": after, "previous": before,
                      "changed": !pyval::same(&before, &after)});
    Out::line(&pyjson::dumps(&said))
}

fn reads_others(caps_file: &str) -> bool {
    let Ok(bytes) = std::fs::read(caps_file) else {
        return true;
    };
    // `^reads_others: *false\b` with re.M
    // A file that is not UTF-8 makes Python crash; here it takes the conservative answer (a reader), so it can never
    // be what lets `reads_others: false` be found.
    let Ok(text) = String::from_utf8(bytes) else {
        return true;
    };
    !text.split('\n').any(|line| {
        line.strip_prefix("reads_others:")
            .map(|r| r.trim_start_matches(' '))
            .and_then(|r| r.strip_prefix("false"))
            .is_some_and(|rest| {
                !rest
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
    })
}

/// `cap_mode`: bypass only for an agent that works on the owner's own files; a reader agent is held to auto.
fn cap_mode(mode: &str, caps_file: &str) -> String {
    if mode == "bypassPermissions" && reads_others(caps_file) {
        "auto".to_owned()
    } else {
        mode.to_owned()
    }
}

/// `retired()`: where this Mac's file names a retired mode: `(key, mode)`, e.g. `("agents.coder.permission_mode",
/// "dontAsk")`. The raw file, so a file that cannot be used says nothing.
pub fn retired(ctx: &Ctx<'_>) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(file(ctx)) else {
        return Vec::new();
    };
    let Ok(Value::Object(s)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut places: Vec<(String, &Map)> = vec![("permission_mode".to_owned(), &s)];
    if let Some(Value::Object(agents)) = s.get("agents") {
        places.extend(agents.iter().filter_map(|(n, a)| {
            a.as_object()
                .map(|a| (format!("agents.{n}.permission_mode"), a))
        }));
    }
    places
        .into_iter()
        .filter_map(|(k, d)| match d.get("permission_mode") {
            Some(Value::String(m)) if matches!(m.as_str(), "manual" | "dontAsk") => {
                Some((k, m.clone()))
            }
            _ => None,
        })
        .collect()
}

/// `call_model(agent)`: the model a voice-call turn runs on, `''` when it keeps its own.
fn call_model(s: &Map, agent: &str) -> String {
    let chosen = if agent.is_empty() {
        None
    } else {
        agent_of(s, agent).and_then(|a| a.get("call_model"))
    };
    let chosen = match chosen {
        Some(v) if pyval::truthy(v) => v.clone(),
        _ => s
            .get("call_model")
            .cloned()
            .unwrap_or_else(|| json!("sonnet")),
    };
    match chosen.as_str() {
        Some("own" | "default") | None => String::new(),
        Some(c) => c.trim().to_owned(),
    }
}

/// `get(key, agent)` printed: a list one item per line, anything else as `print` shows it.
fn get(s: &Map, key: &str, agent: &str) -> Result<String, String> {
    let default = match key {
        "permission_mode" => json!("auto"),
        "plugins" | "accounts" => json!([]),
        _ => json!("fill"),
    };
    let v = if agent.is_empty() {
        s.get(key).cloned().unwrap_or(default)
    } else {
        agent_of(s, agent)
            .and_then(|a| a.get(key))
            .cloned()
            .unwrap_or_else(|| json!(""))
    };
    match &v {
        Value::Array(items) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "TypeError: sequence item: expected str instance".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|l| l.join("\n")),
        v => Ok(pyval::text(v)),
    }
}

/// `kith_settings.py ARGS...`: the verbs above; any other line is the usage, on stderr, exit 1.
pub fn run(ctx: &Ctx<'_>, env: Env, args: &[String]) -> Out {
    let json_flag = args.iter().any(|a| a == "--json");
    let a: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|x| *x != "--json")
        .collect();
    let sorry = |e: String| Out::fail(&format!("kith: {e}"));
    let done = |r: Result<(), String>| r.map_or_else(sorry, |()| Out::ok());
    let usage = || Out {
        stderr: USAGE.to_owned(),
        code: 1,
        ..Out::default()
    };
    match a.as_slice() {
        ["set", "limit", agent, value] => {
            if json_flag {
                return set_json(ctx, "limit", agent, || set_limit(ctx, agent, value));
            }
            done(set_limit(ctx, agent, value))
        }
        ["set", "delegation.chain_cost_cap_usd", value] => {
            if env.agent_run {
                return Out::fail(
                    "kith: the delegation cap is the owner's to change, not an agent's run",
                );
            }
            done(set_chain_cost_cap(ctx, value))
        }
        ["delegation.chain_cost_cap_usd"] => {
            let s = read(ctx);
            let cap = s
                .get("delegation")
                .and_then(|d| d.get("chain_cost_cap_usd"))
                .cloned()
                .unwrap_or_else(|| json!(CHAIN_CAP_USD));
            Out::line(&pyval::text(&cap))
        }
        ["cap", mode, caps_file] => Out::line(&cap_mode(mode, caps_file)),
        ["reads-others", caps_file] => Out::line(if reads_others(caps_file) { "1" } else { "0" }),
        ["limit", agent] => {
            let s = read(ctx);
            let at = agent_of(&s, agent);
            let limit = at
                .and_then(|a| a.get("limit"))
                .cloned()
                .unwrap_or(Value::Null);
            let mode = at
                .and_then(|a| a.get("at_limit"))
                .filter(|v| pyval::truthy(v))
                .cloned()
                .unwrap_or_else(|| json!("ask"));
            Out::line(&pyjson::dumps(&json!({"limit": limit, "at": mode})))
        }
        ["set", key, rest @ ..] if matches!(rest.len(), 1 | 2) => {
            if !["permission_mode", "call_model", "at_limit"].contains(key)
                || (rest.len() == 1 && *key != "call_model")
            {
                return Out::fail(&format!(
                    "kith: {key} is not a setting kith can set per agent"
                ));
            }
            if rest.len() == 2 {
                let (agent, value) = (rest[0], rest[1]);
                if json_flag {
                    return set_json(ctx, key, agent, || choose(ctx, agent, key, value));
                }
                done(choose(ctx, agent, key, value))
            } else {
                done(choose_global(ctx, key, rest[0]))
            }
        }
        [key, rest @ ..] if rest.len() <= 1 && (KEYS.contains(key) || *key == "pin") => {
            let agent = rest.first().copied().unwrap_or("");
            let s = read(ctx);
            match *key {
                "call_model" => Out::line(&call_model(&s, agent)),
                "accounts" => {
                    let ids = settings::accounts(&s, agent).0.unwrap_or_default();
                    Out::line(&ids.join("\n"))
                }
                "pin" => Out::line(&match (rest.len(), settings::pin(&s, agent)) {
                    (1, Some(p)) => pyjson::dumps(&json!({"id": p.id, "only": p.only})),
                    _ => String::new(),
                }),
                _ => get(&s, key, agent).map_or_else(|e| Out::fail(&e), |t| Out::line(&t)),
            }
        }
        _ => usage(),
    }
}
