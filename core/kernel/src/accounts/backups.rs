//! `kith account backup list|add|remove|set|reset` (and `kith backup list|add|remove`, which are the same code): the older
//! view of the account set, the accounts after the first one, as `cmd_backup` in `kernel/bin/kith-account` writes it. A
//! write goes through the set (`edits`), so the first account stays where it is.

use super::edits::{self, edit_with};
use super::set_verbs::{current, not_ready_note, risk_note};
use super::verbs::{Verb, agent_arg, flag, option, valid_id};
use super::{Ctx, J, Map, settings};
use std::fmt::Write as _;

/// `(main, backups, where)`: the first account and the ones after it, and whether they are the agent's own list.
fn chain_target(ctx: &Ctx<'_>, s: &Map, agent: &str) -> (String, Vec<String>, &'static str) {
    let (order, wher) = if agent.is_empty() {
        let ids = current(s, "");
        (
            if ids.is_empty() {
                vec!["claude".to_owned()]
            } else {
                ids
            },
            "global",
        )
    } else {
        let harness = super::agent_harness(super::agent_md_of(ctx, agent).as_deref());
        let wher = if settings::accounts(s, agent).1 == "agent" {
            "agent"
        } else {
            "global"
        };
        (super::static_order(s, agent, &harness).0, wher)
    };
    let mut it = order.into_iter();
    let main = it.next().unwrap_or_else(|| "claude".to_owned());
    (main, it.collect(), wher)
}

/// `write_backups`: the set becomes the main account and `ids`. A global list naming another harness's account turns the
/// cross-harness fallback on, which is what the older chain meant.
fn write_backups(ctx: &Ctx<'_>, s: &mut Map, agent: &str, ids: &[String]) {
    let main = chain_target(ctx, s, agent).0;
    let own = match super::parse(&main) {
        Ok((h, _)) if h != "claude-api" => h,
        _ => "claude".to_owned(),
    };
    if agent.is_empty() && ids.iter().any(|i| !super::in_family(i, &own)) {
        edits::set_cross_harness(s, "", Some(true));
    }
    let mut all = vec![main];
    all.extend_from_slice(ids);
    edits::set_accounts(s, agent, Some(&all));
}

fn scope(agent: &str) -> String {
    if agent.is_empty() {
        "for every agent".to_owned()
    } else {
        format!("for {agent}")
    }
}

fn who(agent: &str) -> &str {
    if agent.is_empty() {
        "every agent"
    } else {
        agent
    }
}

fn jagent(agent: &str) -> J {
    if agent.is_empty() {
        J::Null
    } else {
        J::Str(agent.to_owned())
    }
}

fn strs(ids: &[String]) -> J {
    J::Arr(ids.iter().map(|i| J::Str(i.clone())).collect())
}

/// `int(text)` as a position: a whole number from 1.
fn position(at: &str) -> Result<i64, String> {
    let t = at.trim();
    match t.strip_prefix('+').unwrap_or(t).parse::<i64>() {
        Ok(n) if n >= 1 => Ok(n),
        Ok(_) => Err("--at counts from 1".to_owned()),
        Err(_) => Err(format!("invalid literal for int() with base 10: '{at}'")),
    }
}

pub(super) fn backup(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let as_json = args.iter().any(|a| a == "--json");
    if args.is_empty() || !["list", "add", "remove", "set", "reset"].contains(&args[0].as_str()) {
        return Err(Verb::die(
            "backup list|add|remove|set|reset (kith account --help)",
            2,
        ));
    }
    let sub = args.remove(0);
    flag(&mut args, "--json");
    match sub.as_str() {
        "list" => list(ctx, args, as_json),
        "set" | "reset" => write_all(ctx, &sub, args),
        _ => change(ctx, &sub, args, as_json),
    }
}

fn list(ctx: &Ctx<'_>, mut args: Vec<String>, as_json: bool) -> Result<Verb, Verb> {
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    let s = settings::read(ctx);
    let (_, ids, wher) = chain_target(ctx, &s, &agent);
    let mut rows = Vec::new();
    let mut table: Vec<(String, bool, String)> = Vec::new();
    for i in &ids {
        let (ready, why) = super::check(ctx, i, &agent).unwrap_or_else(|e| (false, Some(e)));
        let fix = if ready {
            None
        } else {
            super::fix(i, &agent).ok().flatten()
        };
        let mut f = vec![
            ("id", J::Str(i.clone())),
            (
                "model",
                super::split(i).1.map_or(J::Null, |m| J::Str(m.to_owned())),
            ),
            ("ready", J::Bool(ready)),
        ];
        if !ready {
            f.push(("why", why.clone().map_or(J::Null, J::Str)));
        }
        if let Some(fix) = fix.filter(|f| !f.is_empty()) {
            f.push(("fix", J::Str(fix)));
        }
        rows.push(J::Obj(f));
        table.push((i.clone(), ready, why.unwrap_or_default()));
    }
    if as_json {
        let out = J::Obj(vec![
            ("agent", jagent(&agent)),
            ("source", J::Str(wher.to_owned())),
            ("backups", J::Arr(rows)),
            ("main", super::main_json(ctx, &agent)),
        ]);
        return Ok(Verb::out(out.dumps() + "\n"));
    }
    let mut out = String::new();
    if table.is_empty() {
        let who = if agent.is_empty() {
            "any agent"
        } else {
            &agent
        };
        let _ = writeln!(out, "no backups for {who} (kith account add)");
    } else {
        let list = if wher == "agent" {
            "its own list"
        } else {
            "the global list"
        };
        let _ = writeln!(out, "backups for {} ({list}):", who(&agent));
    }
    for (n, (id, ready, why)) in table.iter().enumerate() {
        let state = if *ready {
            "ready".to_owned()
        } else {
            format!("not ready: {why}")
        };
        let _ = writeln!(out, "  {}. {id:<22} {state}", n + 1);
    }
    Ok(Verb::out(out))
}

/// `backup set [id ...] [--agent A]` and `backup reset --agent A`.
fn write_all(ctx: &Ctx<'_>, sub: &str, mut args: Vec<String>) -> Result<Verb, Verb> {
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    if sub == "reset" {
        if agent.is_empty() || !args.is_empty() {
            return Err(Verb::die("backup reset --agent A", 2));
        }
        edit_with(ctx, |s| {
            edits::set_accounts(s, &agent, None);
            Ok(())
        })?;
        return Ok(Verb::out(format!("{agent} follows the global accounts\n")));
    }
    for i in &args {
        valid_id(i)?;
        super::check_entry_model(i).map_err(|e| Verb::die(&e, 2))?;
    }
    let ids = edits::dedup(&args);
    edit_with(ctx, |s| {
        write_backups(ctx, s, &agent, &ids);
        Ok(())
    })?;
    let list = if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    };
    Ok(Verb::out(format!("backups for {}: {list}\n", who(&agent))))
}

/// What a `backup add|remove` found and did, for what it prints.
struct Done {
    changed: bool,
    ids: Vec<String>,
    place: usize,
}

/// What the verb does to the backups `ids`: the answer, and whether the set must be written (`ids` is then the new list).
fn plan(ids: Vec<String>, entry: &str, add: bool, place: Option<i64>) -> Done {
    if add {
        if ids.contains(&entry.to_owned()) && place.is_none() {
            let place = ids.iter().position(|i| i == entry).map_or(0, |p| p + 1);
            return Done {
                changed: false,
                ids,
                place,
            };
        }
        let mut new: Vec<String> = ids.into_iter().filter(|i| i != entry).collect();
        let at = place.map_or(new.len(), |p| {
            usize::try_from(p - 1).unwrap_or(usize::MAX).min(new.len())
        });
        new.insert(at, entry.to_owned());
        Done {
            changed: true,
            place: at + 1,
            ids: new,
        }
    } else if !ids.contains(&entry.to_owned()) {
        Done {
            changed: false,
            ids,
            place: 0,
        }
    } else {
        Done {
            changed: true,
            ids: ids.into_iter().filter(|i| i != entry).collect(),
            place: 0,
        }
    }
}

fn change(ctx: &Ctx<'_>, sub: &str, mut args: Vec<String>, as_json: bool) -> Result<Verb, Verb> {
    let at = option(&mut args, "--at")?;
    let agent = agent_arg(ctx, option(&mut args, "--agent")?)?;
    let add = sub == "add";
    if args.len() != 1 {
        let tail = if add { " [--at N]" } else { "" };
        return Err(Verb::die(
            &format!("backup {sub} <id> [--agent A]{tail}"),
            2,
        ));
    }
    let entry = args.remove(0);
    super::parse(&entry).map_err(|e| Verb::die(&e, 2))?;
    if add {
        super::check_entry_model(&entry).map_err(|e| Verb::die(&e, 2))?;
    }
    let place = match at.as_deref() {
        Some(a) => Some(position(a).map_err(|e| Verb::die(&e, 2))?),
        None => None,
    };
    // what the file says now (an unreadable one reads as nothing) decides whether anything is written at all
    let mut done = plan(
        chain_target(ctx, &settings::read(ctx), &agent).1,
        &entry,
        add,
        place,
    );
    if done.changed {
        done = edit_with(ctx, |s| {
            let done = plan(chain_target(ctx, s, &agent).1, &entry, add, place);
            if done.changed {
                write_backups(ctx, s, &agent, &done.ids);
            }
            Ok(done)
        })?;
    }
    let sc = scope(&agent);
    let base = |changed: bool| {
        vec![
            ("ok", J::Bool(true)),
            ("changed", J::Bool(changed)),
            ("entry", J::Str(entry.clone())),
            ("agent", jagent(&agent)),
        ]
    };
    if add {
        if !done.changed {
            if as_json {
                let mut f = base(false);
                f.push((
                    "position",
                    J::Int(i64::try_from(done.place).unwrap_or(i64::MAX)),
                ));
                f.push(("backups", strs(&done.ids)));
                return Ok(Verb::out(J::Obj(f).dumps() + "\n"));
            }
            return Ok(Verb::out(format!(
                "{entry} is already in the backups {sc} (--at N moves it)\n"
            )));
        }
        let (ready, why) = super::check(ctx, &entry, &agent).map_err(|e| Verb::die(&e, 1))?;
        if as_json {
            let mut f = base(true);
            f.push((
                "position",
                J::Int(i64::try_from(done.place).unwrap_or(i64::MAX)),
            ));
            f.push(("backups", strs(&done.ids)));
            f.push(("ready", J::Bool(ready)));
            if !ready {
                f.push(("why", why.map_or(J::Null, J::Str)));
            }
            return Ok(Verb::out(J::Obj(f).dumps() + "\n"));
        }
        return Ok(Verb {
            stdout: format!("{entry} is backup {} {sc}\n", done.place),
            stderr: not_ready_note(ctx, &entry, &agent) + &risk_note(&entry),
            code: 0,
        });
    }
    if as_json {
        let mut f = base(done.changed);
        f.push(("backups", strs(&done.ids)));
        return Ok(Verb::out(J::Obj(f).dumps() + "\n"));
    }
    Ok(Verb::out(if done.changed {
        format!("removed {entry} {sc}\n")
    } else {
        format!("{entry} is not in the backups {sc}\n")
    }))
}
