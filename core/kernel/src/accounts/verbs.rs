//! `kith account list|pick|out|clear|usage` as `kernel/bin/kith-account` runs them (`cmd_list`, `cmd_pick`, `cmd_out`,
//! `cmd_clear`, `cmd_usage`), plus the two verbs the Python readers use once the state is in the core (`state`, `started`).
//! The words, the usage lines and the exit codes are the Python's; the state goes through [`super::change`], so the same
//! code answers over the file and over the core's table.

use super::change::{self, Change, OUT_FOR};
use super::{Ctx, Map, clock, listing, pick_from, pyjson, settings, state};
use serde_json::{Value, json};
use std::fmt::Write as _;

/// The verbs of this module, as they are spelled on the command line.
pub const VERBS: &[&str] = &[
    "list", "ls", "pick", "out", "clear", "usage", "state", "started",
];

/// What a verb printed and how it ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verb {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Verb {
    fn out(text: String) -> Verb {
        Verb {
            stdout: text,
            ..Verb::default()
        }
    }

    /// `die(msg, code)`.
    fn die(msg: &str, code: i32) -> Verb {
        Verb {
            stderr: format!("kith: {msg}\n"),
            code,
            ..Verb::default()
        }
    }
}

/// `flag(args, name)`.
fn flag(args: &mut Vec<String>, name: &str) -> bool {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

/// `option(args, name)`: the value after the first `name`, both taken out of `args`.
fn option(args: &mut Vec<String>, name: &str) -> Result<Option<String>, Verb> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        return Err(Verb::die(&format!("{name} needs a value"), 2));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Ok(Some(v))
}

fn valid_id(entry: &str) -> Result<(), Verb> {
    super::parse(entry)
        .map(|_| ())
        .map_err(|e| Verb::die(&e, 2))
}

fn agent_arg(ctx: &Ctx<'_>, agent: Option<String>) -> Result<String, Verb> {
    let agent = agent.unwrap_or_default();
    if !agent.is_empty() && super::agent_md_of(ctx, &agent).is_none() {
        return Err(Verb::die(&format!("no agent '{agent}' (kith ls)"), 2));
    }
    Ok(agent)
}

/// Python's `Pick` as the dict `pick()` returns: the pin, the candidates in order and what was skipped and why.
pub fn pick_value(agent: &str, cross_harness: bool, p: &super::Pick) -> Value {
    let skipped: Vec<Value> = p
        .skipped
        .iter()
        .map(|x| {
            let mut m = Map::new();
            m.insert("id".into(), json!(x.id));
            m.insert("status".into(), json!(x.status.status));
            if let Some(w) = &x.status.why {
                m.insert("why".into(), json!(w));
            }
            if let Some(u) = x.status.out_until {
                m.insert("out_until".into(), json!(u));
            }
            if let Some(r) = &x.status.reason {
                m.insert("reason".into(), json!(r));
            }
            Value::Object(m)
        })
        .collect();
    json!({
        "version": 1,
        "agent": agent,
        "harness": p.harness,
        "policy": p.policy,
        "pin": p.pin.as_ref().map(|x| json!({"id": x.id, "only": x.only})),
        "cross_harness": cross_harness,
        "candidates": p.candidates,
        "skipped": skipped,
        "all_out": p.all_out,
    })
}

/// The state is read or written next: a tombstone (the state moved to the core and this process does not ask it) stops the
/// verb as `Tombstoned` stops `kith-account`.
fn guard(ctx: &Ctx<'_>) -> Result<(), Verb> {
    match change::tombstone_error(&ctx.machine) {
        Some(e) => Err(Verb::die(&e, 1)),
        None => Ok(()),
    }
}

/// A settings file in the older shape is rewritten in the new one before every command (`kith_settings.migrate_file()`
/// at the top of `kith-account`'s `main`); a failure is noted and the command goes on.
fn migrate(ctx: &Ctx<'_>) {
    if let Err(e) = settings::migrate_file(ctx) {
        crate::log::note(&format!(
            "settings migration: couldn't lock or write {}: {e}",
            ctx.machine.join("settings.json").display()
        ));
    }
}

/// Runs one verb. `stdin` supplies the reading of `usage ID --json -`.
pub fn run_verb(ctx: &Ctx<'_>, verb: &str, args: Vec<String>, stdin: &dyn Fn() -> String) -> Verb {
    migrate(ctx);
    let res = match verb {
        "list" | "ls" => list(ctx, args),
        "pick" => pick(ctx, args),
        "out" => out(ctx, args),
        "clear" => clear(ctx, args),
        "usage" => usage(ctx, args, stdin),
        "state" => state_json(ctx),
        "started" => started(ctx, args),
        other => Err(Verb::die(
            &format!("unknown command: {other} (kith account --help)"),
            2,
        )),
    };
    res.unwrap_or_else(|v| v)
}

fn list(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let json = flag(&mut args, "--json");
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    if !args.is_empty() {
        return Err(Verb::die("account list [--agent A] [--json]", 2));
    }
    guard(ctx)?;
    Ok(Verb::out(listing::list(ctx, json, &agent)))
}

fn pick(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let as_json = flag(&mut args, "--json");
    if args.len() != 1 {
        return Err(Verb::die("account pick <agent> [--json]", 2));
    }
    let agent = agent_arg(ctx, Some(args.remove(0)))?;
    guard(ctx)?;
    let s = settings::read(ctx);
    let p = pick_from(ctx, &s, &agent, None, true, clock::now(), &state::FileState);
    if as_json {
        let v = pick_value(&agent, settings::cross_harness(&s, &agent), &p);
        return Ok(Verb::out(pyjson::dumps(&v) + "\n"));
    }
    let mut out = String::new();
    let _ = write!(out, "{agent} ({}, policy {}", p.harness, p.policy);
    if let Some(pin) = &p.pin {
        let _ = write!(
            out,
            ", pinned to {}{}",
            pin.id,
            if pin.only { " only" } else { "" }
        );
    }
    out += "):\n";
    for (n, i) in p.candidates.iter().enumerate() {
        let _ = write!(out, "  {}. {i}", n + 1);
        if p.all_out {
            out += "   (all are out or signed out; this one runs and says what is wrong)";
        }
        out.push('\n');
    }
    for s in &p.skipped {
        let st = &s.status;
        let _ = write!(
            out,
            "  -  {}  skipped: {}",
            s.id,
            st.status.replace('_', " ")
        );
        if let Some(u) = st.out_until.filter(|u| *u != 0) {
            let _ = write!(out, " until {}", clock::when(u));
        }
        if let Some(w) = st
            .why
            .as_deref()
            .filter(|w| !w.is_empty() && st.status != "out")
        {
            let _ = write!(out, " ({w})");
        }
        out.push('\n');
    }
    Ok(Verb::out(out))
}

fn out(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let until = option(&mut args, "--until")?;
    let reason = option(&mut args, "--reason")?.unwrap_or_else(|| "manual".to_owned());
    let extend = flag(&mut args, "--extend");
    if args.len() != 1 {
        return Err(Verb::die(
            "account out <id> [--until T] [--reason R] [--extend]",
            2,
        ));
    }
    let entry = args.remove(0);
    valid_id(&entry)?;
    if !change::reason_ok(&reason) {
        return Err(Verb::die(
            "--reason is a short word (letters, digits, space . _ -)",
            2,
        ));
    }
    let now = change::now_secs();
    let t = match until {
        Some(u) => change::parse_until(&u, now).map_err(|e| Verb::die(&e, 2))?,
        None => now + OUT_FOR,
    };
    if t <= now {
        return Err(Verb::die("--until is in the past", 2));
    }
    let c = Change::Out {
        id: entry.clone(),
        until: Some(t),
        reason: reason.clone(),
        extend_only: extend,
    };
    change::make(&ctx.machine, &c, now).map_err(|e| Verb::die(&e, 1))?;
    Ok(Verb::out(format!(
        "{entry} is out until {} ({reason})\n",
        clock::when(t)
    )))
}

fn clear(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    if args.len() != 1 {
        return Err(Verb::die("account clear <id>", 2));
    }
    let entry = args.remove(0);
    valid_id(&entry)?;
    let c = Change::Clear {
        id: entry.clone(),
        host_until: change::host_until(&ctx.machine, &entry),
    };
    change::make(&ctx.machine, &c, change::now_secs()).map_err(|e| Verb::die(&e, 1))?;
    Ok(Verb::out(format!("{entry} is ready again\n")))
}

/// Python's `json.loads` error text for the cases a typed reading hits: where the first bad character is.
fn json_error(text: &str, e: &serde_json::Error) -> String {
    let chars: Vec<char> = text.chars().collect();
    // serde reports the position after the character it choked on.
    let mut pos: usize = text
        .split_inclusive('\n')
        .take(e.line().saturating_sub(1))
        .map(|l| l.chars().count())
        .sum::<usize>()
        + e.column().saturating_sub(1);
    let msg = e.to_string();
    if !e.is_eof() && (msg.starts_with("expected value") || msg.starts_with("expected ident")) {
        // Python names where the bad value starts, not the character inside it that was wrong.
        pos = pos.saturating_sub(1).min(chars.len());
        while pos > 0 && chars.get(pos - 1).is_some_and(|c| c.is_alphanumeric()) {
            pos -= 1;
        }
    }
    let before = &chars[..pos.min(chars.len())];
    let line = before.iter().filter(|c| **c == '\n').count() + 1;
    let col = before.iter().rev().take_while(|c| **c != '\n').count() + 1;
    format!("Expecting value: line {line} column {col} (char {pos})")
}

fn usage(ctx: &Ctx<'_>, mut args: Vec<String>, stdin: &dyn Fn() -> String) -> Result<Verb, Verb> {
    let reading = option(&mut args, "--json")?;
    if args.len() != 1 {
        return Err(Verb::die("account usage <id> [--json READING]", 2));
    }
    let entry = args.remove(0);
    valid_id(&entry)?;
    let now = change::now_secs();
    let Some(reading) = reading else {
        guard(ctx)?;
        let state = change::read_state(&ctx.machine);
        let shown = listing::usage_json(state.get(&entry), clock::now());
        return Ok(Verb::out(pyjson::dumps(&shown) + "\n"));
    };
    let text = if reading == "-" { stdin() } else { reading };
    let parsed: Value =
        serde_json::from_str(&text).map_err(|e| Verb::die(&json_error(&text, &e), 2))?;
    let usage = change::validate_usage(&parsed, now).map_err(|e| Verb::die(&e, 2))?;
    let c = Change::Usage {
        id: entry,
        usage: usage.clone(),
    };
    change::make(&ctx.machine, &c, now).map_err(|e| Verb::die(&e, 1))?;
    Ok(Verb::out(pyjson::dumps(&usage) + "\n"))
}

/// Every account's state as one JSON object: the Python readers' view of it once the state is in the core.
fn state_json(ctx: &Ctx<'_>) -> Result<Verb, Verb> {
    guard(ctx)?;
    Ok(Verb::out(
        pyjson::dumps(&Value::Object(change::read_state(&ctx.machine))) + "\n",
    ))
}

/// A launch started on `<id>`: the Python launch notes it once the state is in the core.
fn started(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    if args.len() != 1 {
        return Err(Verb::die("account-state started <id>", 2));
    }
    let entry = args.remove(0);
    valid_id(&entry)?;
    let c = Change::Started { id: entry };
    change::make(&ctx.machine, &c, change::now_secs()).map_err(|e| Verb::die(&e, 1))?;
    Ok(Verb::default())
}
