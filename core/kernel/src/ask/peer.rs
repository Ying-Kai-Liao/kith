//! `kith ask AGENT@COMPUTER "task"` over the Mac link (design 2026-11-24-ask-ledger §9, A5b): the agent lives on another computer
//! of the owner's that this one is paired with. The row is opened here (`ask.open`, mode `peer`: the caps apply before anything is
//! sent, and only the owner's runs cross); the host's tick sends it, follows its rows and carries a stop, so this call never talks
//! to the other computer and nothing is lost when it dies. A foreground ask polls its rows (`ask.read`) until the answer comes;
//! `--bg` prints its id at once and `--wait ID` reads the rows as for any ask.
//!
//! A computer that is in `remotes.json` is the older ssh way (`remote.rs`) and stays on it; a name that is neither is refused.
//! Works on every platform: it is the core's rows and nothing else.

use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Host, R, Stop, conv, die, dumps, fg, new_id, verbs};

/// How often a foreground ask reads its rows.
const POLL: Duration = Duration::from_millis(500);

/// `AGENT@COMPUTER` of the first word.
pub(super) fn split(first: &str) -> Option<(&str, &str)> {
    first
        .rsplit_once('@')
        .filter(|(a, m)| !a.is_empty() && !m.is_empty())
}

/// Whether `machine` is one of `<machine>/remotes.json`'s (the ssh way).
fn in_remotes(host: &Host, machine: &str) -> bool {
    std::fs::read_to_string(host.machine.join("remotes.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().map(|m| m.contains_key(machine)))
        .unwrap_or(false)
}

/// Where `AGENT@COMPUTER` goes: ssh for a computer in `remotes.json`, else the link.
pub(super) fn route(
    host: &Host,
    bg: bool,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> R<i32> {
    let Some((agent, machine)) = split(&args[0]) else {
        return Err(die(
            "kith ask: AGENT@COMPUTER needs both an agent and a computer",
        ));
    };
    if in_remotes(host, machine) {
        return super::remote::ask(host, bg, args, err);
    }
    ask(host, bg, agent, machine, &args[1..], out, err)
}

fn ask(
    host: &Host,
    bg: bool,
    agent: &str,
    machine: &str,
    rest: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> R<i32> {
    let p = fg::parse(rest)?;
    let who = format!("{agent}@{machine}");
    let cwd = std::env::current_dir().unwrap_or_default();
    let key = format!(
        "{who}|{}",
        p.name
            .as_ref()
            .map_or_else(|| cwd.display().to_string(), |n| format!("as:{n}"))
    );
    let prev = p
        .cont
        .then(|| conv::load(host).get(&key).cloned())
        .flatten();
    if p.cont && prev.is_none() {
        return Err(die(format!(
            "kith ask: no conversation with {who} {} to continue; start one without -c",
            p.name.as_ref().map_or_else(
                || "from this directory".to_owned(),
                |n| format!("named {n}")
            )
        )));
    }
    let mid = new_id(if bg { "bg" } else { "ask" });
    // A TERM between here and the open leaves nothing open: the handler is in place first.
    (host.watch_term)();
    let mut open = json!({ "id": mid, "asked": agent, "crossing": true, "mode": "peer", "peer": machine, "task": p.task });
    if let Some(last) = rest.last() {
        open["brief"] = json!(last);
    }
    // What the run there is asked to use. `-C` and `-H` are this computer's own (a folder, a harness), so they stay here.
    let kept = fg::merged_opts(&p, prev.as_ref());
    let mut opts = serde_json::Map::new();
    for (flag, name) in [("-m", "model"), ("-e", "effort"), ("-p", "project")] {
        if let Some((f, v)) = kept.iter().rev().find(|(f, _)| fg::short(f) == flag) {
            open[name] = json!(v);
            opts.insert(fg::short(f), json!(v));
        }
    }
    // `-c` goes on the earlier ask's conversation there (its session resumes).
    if let Some(prev_key) = fg::prev_str(prev.as_ref(), "peer_key") {
        open["cont"] = json!(prev_key);
    }
    if let Err(stop) = fg::open_row(host, open) {
        // The core says this computer is not paired with the name; the caller also has `remotes.json` to think of.
        return Err(if stop.msg.contains("is not paired with") {
            die(format!(
                "kith ask: {machine} is neither a computer paired with this one (kith link {machine}) nor one in remotes.json"
            ))
        } else {
            stop
        });
    }
    let turns = prev
        .as_ref()
        .and_then(|v| v.get("turns"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let _ = conv::keep(
        host,
        &key,
        json!({
            "agent": who, "peer_key": mid,
            "at": chrono::Local::now().format("%Y-%m-%dT%H:%M").to_string(),
            "turns": turns, "cost": 0, "opts": opts,
            "dir": cwd.display().to_string(),
            "last": p.task.split_whitespace().collect::<Vec<_>>().join(" "),
        }),
    );
    if bg {
        let _ = writeln!(
            out,
            "started {who} in the background as {mid}; its answer: kith ask --wait {mid}"
        );
        return Ok(0);
    }
    follow(host, &p, &who, &mid, turns, out, err)
}

/// A foreground ask: its rows read until a final one. A TERM ends the row (`--end`, which the host then carries to the other
/// computer); a caller that dies with no handler leaves the row, whose answer reaches this session as a wake.
fn follow(
    host: &Host,
    p: &fg::Parsed,
    who: &str,
    mid: &str,
    turn: i64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> R<i32> {
    let mut text = Vec::new();
    let mut state = String::new();
    let mut final_kind = String::new();
    let mut told_waiting = false;
    loop {
        if (host.terminated)() {
            return Err(fg::stopped(host, mid));
        }
        match (host.call)("ask.read", &json!({ "id": mid })) {
            Ok(got) => {
                got.get("state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .clone_into(&mut state);
                for row in got
                    .get("rows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if verbs::is_final(row) {
                        row.get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .clone_into(&mut final_kind);
                        text.push(verbs::said(row));
                    } else if !p.as_json {
                        let _ = writeln!(err, "{}", verbs::said(row));
                    }
                }
            }
            Err(f) => return Err(Stop::from_fail(&f)),
        }
        if !final_kind.is_empty() {
            break;
        }
        // The ask ended with no row (stopped from elsewhere, or by `--end`): there is nothing more to wait for.
        if matches!(state.as_str(), "stopped" | "cut_off" | "refused") {
            break;
        }
        if !told_waiting {
            told_waiting = true;
            let _ = writeln!(
                err,
                "{who}: waiting for its answer (it goes on if this call ends: kith ask --wait {mid})"
            );
        }
        std::thread::sleep(POLL);
    }
    let failed = final_kind != "answer" || state == "stopped";
    let answer = text.join("\n");
    if p.as_json {
        let _ = writeln!(
            out,
            "{}",
            dumps(&json!({
                "agent": who, "answer": answer, "session_id": Value::Null, "error": failed,
                "id": mid, "turn": turn,
            }))
        );
    } else {
        let _ = writeln!(out, "{answer}");
    }
    Ok(i32::from(failed))
}
