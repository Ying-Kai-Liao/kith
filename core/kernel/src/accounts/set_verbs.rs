//! `kith account order|move|set|policy|cross-harness|migrate|pin|unpin|use`, as `kernel/bin/kith-account` runs them
//! (`cmd_order` ... `cmd_use`): the words, the usage lines and the exit codes are the Python's. Each one reads the set it
//! changes inside the edit, so two writers (a terminal and the host) cannot lose each other's update.

use super::edits::{self, edit_with};
use super::verbs::{Verb, agent_arg, flag, option, valid_id};
use super::{Ctx, J, Map, settings};
use std::fmt::Write as _;

fn scope(agent: &str) -> &str {
    if agent.is_empty() { "every agent" } else { agent }
}

/// The ids a command edits: the agent's own list or the one it follows (the global set), none when there is none yet.
pub(super) fn current(s: &Map, agent: &str) -> Vec<String> {
    settings::accounts(s, agent).0.unwrap_or_default()
}

/// `not_ready_note`: said on stderr when the account cannot run yet, with the command that fixes it.
pub(super) fn not_ready_note(ctx: &Ctx<'_>, entry: &str, agent: &str) -> String {
    match super::check(ctx, entry, agent) {
        Ok((false, why)) => {
            let fix = super::fix(entry, agent).ok().flatten();
            format!(
                "note: not ready yet ({}){}\n",
                why.unwrap_or_default(),
                fix.map(|f| format!("; to make it ready: {f}"))
                    .unwrap_or_default()
            )
        }
        _ => String::new(),
    }
}

/// `risk_note`: an extra Claude subscription account is used at the person's own risk.
pub(super) fn risk_note(entry: &str) -> String {
    match super::parse(entry) {
        Ok((h, Some(_))) if h == "claude" => {
            "note: extra Claude subscription accounts are used at your own risk; see the warning in kith account add claude.\n"
                .to_owned()
        }
        _ => String::new(),
    }
}

pub(super) fn order(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    if args.is_empty() {
        return Err(Verb::die("account order <id>... [--agent A]", 2));
    }
    // the ids and the set they must be in, checked id by id as the Python does
    let plan = |s: &Map| -> Result<Vec<String>, Verb> {
        let cur = current(s, &agent);
        for i in &args {
            valid_id(i)?;
            if !cur.contains(i) {
                return Err(Verb::die(
                    &format!("{i} is not in the set (kith account add)"),
                    2,
                ));
            }
        }
        let ids = edits::dedup(&args);
        let mut all = ids.clone();
        all.extend(cur.into_iter().filter(|i| !ids.contains(i)));
        Ok(all)
    };
    // refused on what the file says now (an unreadable one reads as no set), then again under the lock
    plan(&settings::read(ctx))?;
    let now = edit_with(ctx, |s| {
        let all = plan(s)?;
        edits::set_accounts(s, &agent, Some(&all));
        Ok(current(s, &agent))
    })?;
    Ok(Verb::out(format!(
        "order for {}: {}\n",
        scope(&agent),
        now.join(", ")
    )))
}

/// Python's `int()` on the text of `--to`, `None` when it is not a whole number.
fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let t = t.strip_prefix('+').unwrap_or(t);
    t.parse::<i64>().ok()
}

pub(super) fn move_(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    let to = option(&mut args, "--to")?;
    let (Some(to), true) = (to, args.len() == 1) else {
        return Err(Verb::die("account move <id> --to N [--agent A]", 2));
    };
    let entry = args.remove(0);
    valid_id(&entry)?;
    let Some(place) = py_int(&to).filter(|p| *p >= 1) else {
        return Err(Verb::die("--to counts from 1", 2));
    };
    let plan = |s: &Map| -> Result<Vec<String>, Verb> {
        let cur = current(s, &agent);
        if !cur.contains(&entry) {
            return Err(Verb::die(
                &format!("{entry} is not in the set (kith account add)"),
                2,
            ));
        }
        let mut ids: Vec<String> = cur.into_iter().filter(|i| *i != entry).collect();
        let at = usize::try_from(place - 1).unwrap_or(usize::MAX).min(ids.len());
        ids.insert(at, entry.clone());
        Ok(ids)
    };
    plan(&settings::read(ctx))?;
    let at = edit_with(ctx, |s| {
        let ids = plan(s)?;
        edits::set_accounts(s, &agent, Some(&ids));
        Ok(ids.iter().position(|i| *i == entry).map_or(0, |p| p + 1))
    })?;
    Ok(Verb::out(format!(
        "{entry} is account {at} for {}\n",
        scope(&agent)
    )))
}

pub(super) fn set(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let reset = flag(&mut args, "--reset");
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    if reset {
        if agent.is_empty() || !args.is_empty() {
            return Err(Verb::die("account set --reset --agent A", 2));
        }
        edit_with(ctx, |s| {
            edits::set_accounts(s, &agent, None);
            Ok(())
        })?;
        return Ok(Verb::out(format!("{agent} follows the global set\n")));
    }
    for i in &args {
        valid_id(i)?;
    }
    let ids: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for i in &args {
            if !seen.contains(i) {
                seen.push(i.clone());
            }
        }
        seen
    };
    edit_with(ctx, |s| {
        edits::set_accounts(s, &agent, Some(&ids));
        Ok(())
    })?;
    let list = if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    };
    Ok(Verb::out(format!(
        "accounts for {}: {list}\n",
        scope(&agent)
    )))
}

pub(super) fn policy(ctx: &Ctx<'_>, args: &[String]) -> Result<Verb, Verb> {
    if args.is_empty() {
        return Ok(Verb::out(format!("{}\n", settings::policy(&settings::read(ctx)))));
    }
    if args.len() != 1 {
        return Err(Verb::die("account policy [fill|spread]", 2));
    }
    let p = &args[0];
    if !["fill", "spread"].contains(&p.as_str()) {
        return Err(Verb::die("policy is one of fill, spread", 2));
    }
    // a settings file that cannot be written back is exit 2 here (the Python catches its `ValueError` for this verb)
    edit_with(ctx, |s| {
        s.insert("account_policy".into(), serde_json::json!(p));
        Ok(())
    })
    .map_err(|mut v| {
        if v.code == 1 {
            v.code = 2;
        }
        v
    })?;
    Ok(Verb::out(format!("policy: {p}\n")))
}

pub(super) fn cross_harness(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    let word = |on: bool| if on { "on" } else { "off" };
    if args.is_empty() {
        let s = settings::read(ctx);
        return Ok(Verb::out(format!(
            "{}\n",
            word(settings::cross_harness(&s, &agent))
        )));
    }
    let usage = "account cross-harness [on|off] [--agent A] | account cross-harness reset --agent A";
    if args.len() != 1
        || !["on", "off", "reset"].contains(&args[0].as_str())
        || (args[0] == "reset" && agent.is_empty())
    {
        return Err(Verb::die(usage, 2));
    }
    let value = (args[0] != "reset").then(|| args[0] == "on");
    edit_with(ctx, |s| {
        edits::set_cross_harness(s, &agent, value);
        Ok(())
    })?;
    let s = settings::read(ctx);
    Ok(Verb::out(format!(
        "cross-harness fallback for {}: {}\n",
        scope(&agent),
        word(settings::cross_harness(&s, &agent))
    )))
}

pub(super) fn migrate_verb(ctx: &Ctx<'_>) -> Result<Verb, Verb> {
    let wrote = settings::migrate_file(ctx)
        .map_err(|e| Verb::die(&format!("{}: {e}", ctx.machine.join("settings.json").display()), 1))?;
    Ok(Verb::out(
        if wrote {
            "settings.json is now in the new shape\n"
        } else {
            "settings.json needs no change\n"
        }
        .to_owned(),
    ))
}

pub(super) fn pin(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let only = flag(&mut args, "--only");
    if args.len() != 2 {
        return Err(Verb::die("account pin <agent> <id> [--only]", 2));
    }
    let agent = agent_arg(ctx, Some(args.remove(0)))?;
    let entry = args.remove(0);
    valid_id(&entry)?;
    let harness = super::agent_harness(super::agent_md_of(ctx, &agent).as_deref());
    super::check_main(&entry, &harness).map_err(|e| Verb::die(&e, 2))?;
    edit_with(ctx, |s| {
        edits::set_pin(s, &agent, &entry, only);
        Ok(())
    })?;
    let mut out = format!("{agent} starts on {entry}");
    out += if only {
        " and never uses another account\n"
    } else {
        "; when it is out, the rest of the set\n"
    };
    Ok(Verb {
        stdout: out,
        stderr: not_ready_note(ctx, &entry, &agent) + &risk_note(&entry),
        code: 0,
    })
}

pub(super) fn unpin(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    if args.len() != 1 {
        return Err(Verb::die("account unpin <agent>", 2));
    }
    let name = args.remove(0);
    let agent = agent_arg(ctx, Some(name.clone()))?;
    edit_with(ctx, |s| {
        edits::clear_pin(s, &agent);
        Ok(())
    })?;
    Ok(Verb::out(format!("{name} follows the set\n")))
}

fn field<'a>(j: &'a J, key: &str) -> Option<&'a J> {
    match j {
        J::Obj(f) => f.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn text(j: Option<&J>) -> Option<String> {
    match j {
        Some(J::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// The main account as `use` tells it (`--json`: the object as it is).
fn main_report(ctx: &Ctx<'_>, agent: &str, as_json: bool) -> Verb {
    let m = super::main_json(ctx, agent);
    if as_json {
        return Verb::out(m.dumps() + "\n");
    }
    let Some(id) = text(field(&m, "id")) else {
        return Verb::out(
            "main account for every agent: the default login of its harness (kith account use <id> chooses one)\n"
                .to_owned(),
        );
    };
    let how = match text(field(&m, "source")).as_deref() {
        Some("agent") => "its own choice",
        Some("global") => "the global choice",
        _ => "the default login",
    };
    let mut out = format!("main account for {}: {id} ({how})", scope(agent));
    if matches!(field(&m, "ready"), Some(J::Bool(false))) {
        let why = text(field(&m, "why")).unwrap_or_else(|| "None".to_owned());
        let _ = write!(out, ", not ready: {why}");
    }
    out.push('\n');
    if let Some(fix) = text(field(&m, "fix")) {
        let _ = writeln!(out, "  to make it ready: {fix}");
    }
    Verb::out(out)
}

pub(super) fn use_(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let as_json = flag(&mut args, "--json");
    let reset = flag(&mut args, "--reset");
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    if reset || args.is_empty() {
        if !args.is_empty() {
            return Err(Verb::die("account use --reset [--agent A]", 2));
        }
        if reset {
            edit_with(ctx, |s| {
                if !agent.is_empty() {
                    edits::clear_pin(s, &agent);
                } else if let Some(ids) = settings::accounts(s, "").0.filter(|i| !i.is_empty()) {
                    let mut all = vec!["claude".to_owned()];
                    all.extend(ids);
                    edits::set_accounts(s, "", Some(&all));
                }
                Ok(())
            })?;
        }
        return Ok(main_report(ctx, &agent, as_json));
    }
    if args.len() != 1 {
        return Err(Verb::die(
            "account use <id> [--agent A] | account use --reset [--agent A]",
            2,
        ));
    }
    let entry = args.remove(0);
    let (h, _) = super::parse(&entry).map_err(|e| Verb::die(&e, 2))?;
    if !agent.is_empty() {
        let harness = super::agent_harness(super::agent_md_of(ctx, &agent).as_deref());
        super::check_main(&entry, &harness).map_err(|e| Verb::die(&e, 2))?;
    } else if !["claude", "claude-api", "codex"].contains(&h.as_str()) {
        return Err(Verb::die(
            &format!(
                "{entry}: a main account is claude, claude:<name>, claude-api:<profile>, codex or codex:<name>"
            ),
            2,
        ));
    }
    edit_with(ctx, |s| {
        if agent.is_empty() {
            let mut all = vec![entry.clone()];
            all.extend(current(s, "").into_iter().filter(|i| *i != entry));
            edits::set_accounts(s, "", Some(&all));
        } else {
            edits::set_pin(s, &agent, &entry, false);
        }
        Ok(())
    })?;
    let mut out = format!("main account for {}: {entry}", scope(&agent));
    if agent.is_empty() {
        let _ = write!(
            out,
            " (agents on {} only; the others keep their default login)",
            if h == "codex" { "Codex" } else { "Claude" }
        );
    }
    out.push('\n');
    Ok(Verb {
        stdout: out,
        stderr: "the next turn uses it; a conversation made under another account continues as a new session\n"
            .to_owned()
            + &not_ready_note(ctx, &entry, &agent)
            + &risk_note(&entry),
        code: 0,
    })
}
