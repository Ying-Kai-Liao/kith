//! What a launch reads of this Mac's choices: the reading half of `kernel/lib/kith_settings.py` (`<machine>/settings.json`)
//! and of `kernel/lib/kith_allowed.py` (an agent's `allowed.json`), plus the permission-rule helpers of the first. Writing
//! them stays in Python until the commands that write move.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

/// Modes kith no longer offers, and what a stored one reads as.
pub fn current(mode: &str) -> &str {
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

fn valid_limit(v: &Value) -> bool {
    v.as_f64().is_some_and(|f| 0.0 < f && f < 1e7)
}

/// `kith_settings.read()`: what the file sets (an empty object when there is no file or it can't be used).
pub fn read(machine: &Path) -> Map<String, Value> {
    let Ok(text) = fs::read_to_string(machine.join("settings.json")) else {
        return Map::new();
    };
    let Ok(Value::Object(mut s)) = serde_json::from_str::<Value>(&text) else {
        return Map::new();
    };
    // Old files still read back: a retired mode as the mode that replaced it.
    let retire = |d: &mut Map<String, Value>| {
        if let Some(Value::String(m)) = d.get("permission_mode") {
            let now = current(m).to_owned();
            d.insert("permission_mode".to_owned(), Value::String(now));
        }
    };
    retire(&mut s);
    if let Some(Value::Object(agents)) = s.get_mut("agents") {
        for a in agents.values_mut() {
            if let Value::Object(a) = a {
                retire(a);
            }
        }
    }
    if let Some(agents) = s.get("agents")
        && !agents
            .as_object()
            .is_some_and(|a| a.values().all(Value::is_object))
    {
        return Map::new();
    }
    let plugins_ok = match s.get("plugins") {
        None => true,
        Some(Value::Array(p)) => p.iter().all(Value::is_string),
        Some(_) => false,
    };
    if !plugins_ok {
        return Map::new();
    }
    let mut places: Vec<&Map<String, Value>> = vec![&s];
    if let Some(Value::Object(agents)) = s.get("agents") {
        places.extend(agents.values().filter_map(Value::as_object));
    }
    for d in places {
        match d.get("backups") {
            None => {}
            Some(Value::Array(b)) if b.iter().all(Value::is_string) => {}
            Some(_) => return Map::new(),
        }
        if let Some(a) = d.get("account")
            && a.as_str().is_none_or(str::is_empty)
        {
            return Map::new();
        }
        for (k, choices) in [
            ("permission_mode", &PERMISSION_MODES[..]),
            ("at_limit", &AT_LIMITS[..]),
        ] {
            if let Some(v) = d.get(k)
                && !v.as_str().is_some_and(|v| choices.contains(&v))
            {
                return Map::new();
            }
        }
        if d.get("limit").is_some_and(|l| !valid_limit(l)) {
            return Map::new();
        }
    }
    let cap_ok = match s.get("delegation") {
        None => true,
        Some(Value::Object(d)) => d
            .get("chain_cost_cap_usd")
            .is_none_or(|v| v == "none" || valid_limit(v)),
        Some(_) => false,
    };
    if !cap_ok {
        return Map::new();
    }
    s
}

/// `get('permission_mode', agent)`: this Mac's choice for the agent, `""` when there is none.
pub fn agent_permission_mode(machine: &Path, agent: &str) -> String {
    read(machine)
        .get("agents")
        .and_then(|a| a.get(agent))
        .and_then(|a| a.get("permission_mode"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `get('permission_mode')`: kith's setting for every agent, else the default.
pub fn global_permission_mode(machine: &Path) -> Value {
    read(machine)
        .remove("permission_mode")
        .unwrap_or_else(|| Value::String("auto".to_owned()))
}

/// `get('plugins')`: the plugin ids every agent's run turns on.
pub fn plugins(machine: &Path) -> Vec<String> {
    read(machine)
        .get("plugins")
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `reads_others(caps_file)`: true unless capabilities.yaml says `reads_others: false`.
pub fn reads_others(caps_file: &Path) -> bool {
    let Ok(text) = fs::read(caps_file) else {
        return true;
    };
    let text = String::from_utf8_lossy(&text);
    !text.lines().any(|l| {
        l.strip_prefix("reads_others:").is_some_and(|rest| {
            let rest = rest.trim_start_matches(' ');
            rest.strip_prefix("false")
                .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
    })
}

/// Whether an allow rule lets a run execute arbitrary code: bare Bash, a wildcard, or a shell/interpreter prefix.
fn is_broad_bash(rule: &str) -> bool {
    const BROAD_SHELLS: [&str; 14] = [
        "sh",
        "bash",
        "zsh",
        "python",
        "python3",
        "node",
        "env",
        "perl",
        "ruby",
        "osascript",
        "uv run",
        "npx",
        "deno",
        "bun",
    ];
    let r = rule.trim();
    if r == "Bash" {
        return true;
    }
    if !(r.starts_with("Bash(") && r.ends_with(')')) {
        return false;
    }
    let body = r[5..r.len() - 1]
        .trim()
        .trim_matches(|c| c == '"' || c == '\'');
    if matches!(body, "" | "*" | ":*" | "**") {
        return true;
    }
    let prog = body.split([':', '*']).next().unwrap_or_default().trim();
    let mut words = prog.split(' ');
    let first = words
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let rest = std::iter::once(first)
        .chain(words)
        .collect::<Vec<_>>()
        .join(" ");
    let rest = rest.trim();
    prog.is_empty()
        || BROAD_SHELLS
            .iter()
            .any(|b| rest == *b || rest.starts_with(&format!("{b} ")))
}

/// The allow rules a non-owner run keeps: a trusted membership keeps all, anyone else loses every broad Bash rule.
pub fn strip_broad_bash(rules: Vec<Value>, trusted: bool) -> Vec<Value> {
    if trusted {
        return rules;
    }
    rules
        .into_iter()
        .filter(|r| !r.as_str().is_some_and(is_broad_bash))
        .collect()
}

/// `kith_allowed.for_settings(f)`: `(permission rules, prose entries, problem)` an owner's run takes from an allowed.json.
pub fn allowed_for_settings(f: &Path) -> (Vec<String>, Vec<String>, Option<String>) {
    let text = match fs::read_to_string(f) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], vec![], None),
        Err(e) => {
            return (
                vec![],
                vec![],
                Some(format!("cannot read {} ({e})", f.display())),
            );
        }
    };
    let items = match serde_json::from_str::<Value>(&text) {
        Ok(doc) => match doc.get("allowed") {
            Some(Value::Array(a)) => a.clone(),
            _ => {
                return (
                    vec![],
                    vec![],
                    Some(format!(
                        "{} is not usable (allowed is not a list); skipped",
                        f.display()
                    )),
                );
            }
        },
        Err(e) => {
            return (
                vec![],
                vec![],
                Some(format!("{} is not usable ({e}); skipped", f.display())),
            );
        }
    };
    let (mut rules, mut prose) = (vec![], vec![]);
    for e in items {
        let Some(rule) = e.get("rule").and_then(Value::as_str) else {
            continue;
        };
        if rule.trim().is_empty() || rule.chars().count() > 2000 {
            continue;
        }
        match e.get("kind").and_then(Value::as_str) {
            Some("rule") => rules.push(rule.to_owned()),
            Some("prose") => prose.push(rule.to_owned()),
            _ => {}
        }
    }
    (rules, prose, None)
}
