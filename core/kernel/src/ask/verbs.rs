//! The verbs that are one or two rows: `--wait`, `--to`, `--answer`, `--reply`, `--cap`, `--cap-told`, `--end`, `delegations`.

use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Host, R, Stop, caller_person, die, dumps, from_agent_run, plain, words};
use crate::delegation::{chain_cost_cap, pct_g, safe_id};
use crate::procstart::{self, Probe, Sig};
use crate::wake::Fail;

/// `kith ask delegations --json`: every ask record, newest first (the host reads them).
pub(super) fn delegations(host: &Host, out: &mut dyn Write) -> R<i32> {
    let ans = host.core("ask.delegations", &host.view(json!({})))?;
    let _ = writeln!(out, "{}", dumps(&ans));
    Ok(0)
}

/// The words of a row of the mailbox as `--wait` prints them: a progress note says so.
fn said(row: &Value) -> String {
    let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
    if row.get("kind").and_then(Value::as_str) == Some("progress") {
        format!("(progress) {text}")
    } else {
        text.to_owned()
    }
}

fn is_final(row: &Value) -> bool {
    matches!(
        row.get("kind").and_then(Value::as_str),
        Some("answer" | "lost")
    )
}

/// Whether a refusal says the ask is not there (a `--wait` for an id nothing made waits for it, as the Python did).
fn no_such_ask(why: &str) -> bool {
    plain(why).starts_with("no ask ")
}

/// `kith ask --wait ID [--timeout S]`: the answers since last time; the first may take a while.
pub(super) fn wait(
    host: &Host,
    argv: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> R<i32> {
    let usage = || die("usage: kith ask --wait ID [--timeout SECONDS]");
    let mid = argv.first().cloned().unwrap_or_default();
    if !safe_id(&mid) {
        return Err(usage());
    }
    let timeout = match argv.iter().position(|a| a == "--timeout") {
        Some(i) => argv
            .get(i + 1)
            .and_then(|t| t.parse::<f64>().ok())
            .filter(|t| t.is_finite())
            .ok_or_else(usage)?,
        None => 600.0,
    };
    let end = Instant::now() + Duration::from_secs_f64(timeout.clamp(0.0, 31_536_000.0));
    let mut rows: Vec<Value> = Vec::new();
    let mut state = String::new();
    loop {
        match (host.call)("ask.read", &json!({ "id": mid })) {
            Ok(got) => {
                got.get("state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .clone_into(&mut state);
                rows.extend(
                    got.get("rows")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
            Err(Fail::Refused(why)) if no_such_ask(&why) => {}
            Err(f) => return Err(Stop::from_fail(&f)),
        }
        let now = Instant::now();
        if rows.iter().any(is_final) || now >= end {
            break;
        }
        std::thread::sleep((end - now).min(Duration::from_secs(2)));
    }
    for row in &rows {
        let _ = writeln!(out, "{}", said(row));
    }
    // Progress is not the answer: still waiting.
    let Some(last) = rows.iter().rev().find(|r| is_final(r)) else {
        let _ = writeln!(
            err,
            "kith ask: no answer to {mid} yet; ask again with: kith ask --wait {mid}"
        );
        return Ok(3);
    };
    // A run that failed (settled `stopped`) or never answered (`lost`) passes its failure on, as a `--bg` run's exit did.
    let failed = last.get("kind").and_then(Value::as_str) == Some("lost") || state == "stopped";
    Ok(i32::from(failed))
}

/// `kith ask --answer ID [--as AGENT] [--progress] -- TEXT`.
pub(super) fn answer(host: &Host, argv: &[String], out: &mut dyn Write) -> R<i32> {
    let usage = || die("usage: kith ask --answer ID [--as AGENT] [--progress] -- TEXT");
    let Some(at) = argv.iter().position(|a| a == "--") else {
        return Err(usage());
    };
    let (head, text) = (&argv[..at], words(&argv[at + 1..]));
    if text.is_empty() || head.is_empty() || !safe_id(&head[0]) {
        return Err(usage());
    }
    let (mut who, mut kind, mut i) = (None, "answer", 1);
    while i < head.len() {
        if head[i] == "--as" && i + 1 < head.len() {
            who = Some(head[i + 1].clone());
            i += 2;
        } else if head[i] == "--progress" {
            kind = "progress";
            i += 1;
        } else {
            return Err(usage());
        }
    }
    let mut p = json!({ "id": head[0], "text": text, "kind": kind });
    if let Some(w) = who {
        p["as"] = json!(w);
    }
    let got = host.core("ask.post", &p)?;
    let word = if got.get("stopped").and_then(Value::as_bool) == Some(true) {
        // Stop wins over an answer on its way (R8): nothing is kept, nothing wakes.
        "stopped"
    } else if got.get("row").is_none_or(Value::is_null) {
        "duplicate"
    } else if kind == "answer" {
        // The asker is owed its wake in the same step (R3).
        "woke"
    } else {
        "kept"
    };
    let _ = writeln!(out, "{word}");
    Ok(0)
}

/// `kith ask --reply ID AGENT --resume SESSION -- NOTE`: the host's resume of a run that parked on a card for the ask ID. The
/// card's answer makes the callee's resume the ask's new turn, which the host runs (R7, `ask.unpark`).
pub(super) fn reply(host: &Host, argv: &[String], err: &mut dyn Write) -> R<i32> {
    if argv.len() < 6 || !safe_id(&argv[0]) || argv[2] != "--resume" || argv[4] != "--" {
        return Err(die(
            "usage: kith ask --reply ID AGENT --resume SESSION -- NOTE",
        ));
    }
    let (mid, session, note) = (&argv[0], &argv[3], words(&argv[5..]));
    let got = host.core(
        "ask.unpark",
        &host.view(json!({ "id": mid, "note": note, "session": session })),
    )?;
    if got.get("turn").is_none_or(Value::is_null) {
        // Stop wins over a yes already given (R8): the run is not resumed.
        let _ = writeln!(err, "kith ask: {mid} was ended; nothing to resume");
    }
    Ok(0)
}

/// `kith ask --cap [N|none]`: the cost cap of a delegation chain, US$ (shown, or set for this Mac).
pub(super) fn cap(host: &Host, argv: &[String], out: &mut dyn Write) -> R<i32> {
    if argv.len() > 1 {
        return Err(die("usage: kith ask --cap [N|none]"));
    }
    if let Some(value) = argv.first() {
        if from_agent_run(host) {
            return Err(die(
                "kith ask: the cap is the owner's to change (the Needs-you card, or kith ask --cap in a terminal), not an agent's",
            ));
        }
        let set = ["set", "delegation.chain_cost_cap_usd", value.as_str()].map(str::to_owned);
        let (code, _, stderr) = (host.settings)(&set);
        if code != 0 {
            let why = stderr.trim();
            return Err(die(format!(
                "kith ask: {}",
                why.strip_prefix("kith: ").unwrap_or(why)
            )));
        }
    }
    let shown = chain_cost_cap(&host.machine).map_or_else(|| "none".to_owned(), pct_g);
    let _ = writeln!(out, "{shown}");
    Ok(0)
}

/// `kith ask --cap-told CHAIN`: the host raised the chain's cap card; it is not raised again for this cap.
pub(super) fn cap_told(host: &Host, argv: &[String]) -> R<i32> {
    if argv.len() != 1 || !safe_id(&argv[0]) {
        return Err(die("usage: kith ask --cap-told CHAIN"));
    }
    if from_agent_run(host) {
        return Err(die("kith ask: only the host marks a cap card raised"));
    }
    host.core("ask.capTold", &json!({ "chain": argv[0] }))?;
    Ok(0)
}

/// `kith ask --end [--session S]... [--person P]...`: Stop, revoke, removal. The rows settle `stopped`, the wakes and waiters
/// the callers are owed are withdrawn, and what runs for them is stopped: all but the stop is one transaction of the core.
pub(super) fn end(
    host: &Host,
    argv: &[String],
    out: &mut dyn Write,
    _err: &mut dyn Write,
) -> R<i32> {
    if from_agent_run(host) {
        return Err(die(
            "kith ask: only the owner (or the host, on Stop, a revoke or a removal) ends delegations",
        ));
    }
    let (mut sessions, mut persons, mut i) = (Vec::new(), Vec::new(), 0);
    while i < argv.len() {
        if matches!(argv[i].as_str(), "--session" | "--person") && i + 1 < argv.len() {
            if argv[i] == "--session" {
                sessions.push(argv[i + 1].clone());
            } else {
                persons.push(argv[i + 1].clone());
            }
            i += 2;
        } else {
            return Err(die(
                "usage: kith ask --end [--session S]... [--person P]...",
            ));
        }
    }
    let ids = end_rows(host, &json!({ "sessions": sessions, "persons": persons }))?;
    let _ = writeln!(out, "ended {}", ids.join(" "));
    Ok(0)
}

/// `ask.end` and what it leaves to a process: the rows settle `stopped` in one transaction, then the runs they name are
/// stopped. Answers the ids ended.
pub(super) fn end_rows(host: &Host, params: &Value) -> R<Vec<String>> {
    let got = host.core("ask.end", params)?;
    let ids: Vec<String> = strings(&got, "ended");
    // The foreground asks of the ended rows are processes of callers (a `kith ask` waiting on its callee): stopped too, as the
    // Python stopped the pid of every record it ended. A row that is no record any more is skipped.
    let mut pids: Vec<u32> = ids.iter().filter_map(|id| record_pid(host, id)).collect();
    let kills = got
        .get("kill")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for k in &kills {
        let pid = k
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok());
        let started = k.get("started").and_then(Value::as_str).unwrap_or_default();
        if let Some(pid) = pid
            && claimed_by(pid, started)
        {
            pids.push(pid);
        }
    }
    stop(&pids);
    Ok(ids)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

/// The process a foreground ask recorded (`pid`, taken when the row opened) when it is still the one that began then: a pid
/// reused since began after the record (`started_before`).
fn record_pid(host: &Host, id: &str) -> Option<u32> {
    let rec = (host.call)("ask.record", &json!({ "id": id })).ok()?;
    let pid = u32::try_from(rec.get("pid")?.as_u64()?).ok()?;
    let at = rec
        .get("pid_at")
        .or_else(|| rec.get("at"))
        .and_then(Value::as_f64)?;
    (pid != std::process::id() && started_before(pid, at)).then_some(pid)
}

/// Whether `pid` is a live process that began no later than `t` (seconds): a pid recorded at `t` and reused since began
/// after it. Only a stamp that is a time (`e<epoch ms>`) can say; any other form is not taken for a match.
fn started_before(pid: u32, t: f64) -> bool {
    let Probe::Alive(Some(stamp)) = procstart::probe(pid) else {
        return false;
    };
    stamp
        .strip_prefix('e')
        .and_then(|ms| ms.parse::<f64>().ok())
        .is_some_and(|ms| ms / 1000.0 <= t + 2.0)
}

/// Whether `pid` still is the claimer recorded with `started` (stamps of one form that are equal).
fn claimed_by(pid: u32, started: &str) -> bool {
    match procstart::probe(pid) {
        Probe::Alive(Some(now)) => {
            started.is_empty() || (procstart::comparable(started, &now) && started == now)
        }
        Probe::Alive(None) => started.is_empty(),
        Probe::Gone | Probe::Unknown => false,
    }
}

fn alive(pid: u32) -> bool {
    !matches!(procstart::probe(pid), Probe::Gone)
}

/// TERM to every process of every run first, then one shared grace, then KILL for whoever is still there (SIGTERM can be
/// ignored or take its time). The wait is the same for one run or fifty.
fn stop(roots: &[u32]) {
    let mut victims: Vec<u32> = Vec::new();
    for root in roots {
        let v = procstart::claim_victims(*root);
        victims.extend(v.pids);
        victims.push(*root);
        if let Some(g) = v.group {
            procstart::signal_group(g, Sig::Term);
        }
    }
    victims.dedup();
    procstart::signal(&victims, Sig::Term);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && victims.iter().any(|v| alive(*v)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let left: Vec<u32> = victims.into_iter().filter(|v| alive(*v)).collect();
    procstart::signal(&left, Sig::Kill);
}

/// `kith ask --to ID "message"`: a follow-up to the `--bg` run ID, a wake of its session.
pub(super) fn to(host: &Host, argv: &[String], out: &mut dyn Write, err: &mut dyn Write) -> R<i32> {
    if argv.len() < 3 || words(&argv[2..]).is_empty() {
        return Err(die("usage: kith ask --to ID \"message\""));
    }
    let text = words(
        &argv[2..]
            .iter()
            .filter(|a| *a != "--")
            .cloned()
            .collect::<Vec<_>>(),
    );
    let mid = &argv[1];
    let nothing = || {
        die(format!(
            "kith ask: no job {mid} on this Mac (kith ask --bg prints the ids)"
        ))
    };
    if !safe_id(mid) {
        return Err(nothing());
    }
    match (host.call)("ask.to", &json!({ "id": mid })) {
        Ok(_) => {}
        Err(Fail::Refused(why)) if no_such_ask(&why) => return Err(nothing()),
        Err(f) => return Err(Stop::from_fail(&f)),
    }
    let job = job_of(host, mid)?;
    let (Some(session), Some(agent)) = (job.session.clone(), job.agent.clone()) else {
        return Err(die(format!(
            "kith ask: {mid} has no session to send to (it ended before it began one, or runs on a harness other than claude); ask anew without --to"
        )));
    };
    let caller = caller_person(host)?;
    if let Some(c) = &caller
        && (job.person.as_deref() != Some(c.as_str()) || !may_ask(host, &agent))
    {
        return Err(die(format!("kith ask: {mid} is not one of your jobs")));
    }
    let rid = format!("ask-to:{mid}/{}", &sha256_hex(&text)[..12]);
    let who = host
        .set("KITH_AGENT")
        .or_else(|| caller.clone())
        .unwrap_or_else(|| "the owner".to_owned());
    let got = follow_up(
        host,
        &session,
        &agent,
        job.person.as_deref(),
        &rid,
        &text,
        &format!("{who} sent a follow-up to kith ask {mid}"),
        err,
    )?;
    if got != "duplicate" {
        let when = if job.open {
            "is still running: your message reaches it when its turn ends"
        } else {
            "had ended: its session resumes now with your message"
        };
        let _ = writeln!(err, "{mid} {when}; follow it: kith ask --wait {mid}");
    }
    let _ = writeln!(out, "{got}");
    Ok(0)
}

/// One wake request on `sid` (the wake queue merges it with what else the session is owed and delivers it once when nobody
/// holds it). Answers `queued`, `merged` or `duplicate`.
#[allow(clippy::too_many_arguments)]
pub(super) fn follow_up(
    host: &Host,
    sid: &str,
    agent: &str,
    person: Option<&str>,
    rid: &str,
    text: &str,
    why: &str,
    err: &mut dyn Write,
) -> R<String> {
    let mut p = json!({
        "session": sid, "id": rid, "agent": agent, "text": [text],
        "door": if person.is_some() { "host" } else { "run" }, "why": why,
    });
    if let Some(person) = person {
        p["person"] = json!(person);
    }
    let got = host.core("wake.enqueue", &p)?;
    let word = got
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if word == "duplicate" {
        let how = host
            .core("wake.queueStatus", &json!({ "id": rid }))
            .ok()
            .and_then(|s| s.get("state").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let how = match how.as_str() {
            "queued" => "still queued".to_owned(),
            "running" => "in the turn now".to_owned(),
            _ => how,
        };
        let _ = writeln!(err, "kith ask: that message was sent already ({how})");
    }
    Ok(word)
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut a, b| {
            let _ = write!(a, "{b:02x}");
            a
        })
}

/// What `--to` needs of a `--bg` run: its agent, the session it holds, the person it serves, whether it still runs.
struct Job {
    agent: Option<String>,
    session: Option<String>,
    person: Option<String>,
    open: bool,
}

/// The job's row of `ask.delegations`; a run still starting gets a few seconds to take its session.
fn job_of(host: &Host, mid: &str) -> R<Job> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let all = host.core("ask.delegations", &host.view(json!({})))?;
        let row = all
            .get("delegations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|d| d.get("id").and_then(Value::as_str) == Some(mid))
            .cloned()
            .unwrap_or(Value::Null);
        let text = |v: Option<&Value>| {
            v.and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let job = Job {
            agent: text(row.get("agent")),
            session: text(row.get("session")),
            person: text(row.pointer("/caller/person")),
            open: row.get("state").and_then(Value::as_str) == Some("open"),
        };
        if job.session.is_some() || !job.open || Instant::now() >= end {
            return Ok(job);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Whether the person this call is for may still ask `agent` today (kith-auth, as bin/kith checks a run). No answer is no.
#[cfg(unix)]
fn may_ask(host: &Host, agent: &str) -> bool {
    let mut cmd = std::process::Command::new(host.kernel.join("bin/kith-auth"));
    cmd.arg("resolve").arg(agent);
    if let Some(via) = host
        .var("KITH_AGENT")
        .filter(|v| !v.is_empty() && v != agent)
    {
        cmd.arg("--via").arg(via);
    }
    cmd.env_clear().envs(host.env.iter().cloned());
    cmd.stdin(std::process::Stdio::null());
    let Ok(done) = cmd.output() else { return false };
    serde_json::from_slice::<Value>(&done.stdout)
        .ok()
        .and_then(|v| v.get("allowed").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// A person's `--to` on Windows waits for `kith-auth` there (retire-python P3-1): no answer is no.
#[cfg(not(unix))]
fn may_ask(_host: &Host, _agent: &str) -> bool {
    false
}
