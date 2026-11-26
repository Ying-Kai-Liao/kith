//! `kith ask AGENT [options] "task"` and `--bg`: the ask's row (`ask.open`), the callee run in this process (foreground) or
//! left to the host (`--bg`), and the end of the row (`ask.finish`). R8: a foreground ask lives and dies with its caller.

use std::io::{Read as _, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{EFFORTS, Host, R, REFUSED, Stop, conv, die, dumps, new_id, plain, tail, verbs, words};
use crate::procstart;
use crate::wake::Fail;

const USAGE: &str = "usage: kith ask <agent> [-c] [--as NAME] [-C DIR] [-p PROJECT] [-m MODEL] [-e EFFORT] [-H HARNESS] [--json] \"task\"";

/// The command line of `kith ask AGENT ...` after the agent.
struct Parsed {
    cont: bool,
    name: Option<String>,
    as_json: bool,
    dir: Option<String>,
    /// `-p`, `-m`, `-e`, `-H` (long or short, as given) with their values, in order.
    opts: Vec<(String, String)>,
    task: String,
}

fn parse(rest: &[String]) -> R<Parsed> {
    let mut p = Parsed {
        cont: false,
        name: None,
        as_json: false,
        dir: None,
        opts: Vec::new(),
        task: String::new(),
    };
    let mut words_: Vec<String> = Vec::new();
    let mut i = 0;
    let value = |i: usize| rest.get(i + 1).cloned().ok_or_else(|| die(USAGE));
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            "-c" | "--continue" => {
                p.cont = true;
                i += 1;
            }
            "--as" => {
                p.name = Some(value(i)?);
                i += 2;
            }
            "--json" => {
                p.as_json = true;
                i += 1;
            }
            "-C" | "--dir" => {
                p.dir = Some(value(i)?);
                i += 2;
            }
            "-p" | "--project" | "-m" | "--model" | "-e" | "--effort" | "-H" | "--harness" => {
                let v = value(i)?;
                if matches!(a, "-e" | "--effort") && !EFFORTS.contains(&v.as_str()) {
                    return Err(die(format!(
                        "kith ask: effort must be one of {}",
                        EFFORTS.join(", ")
                    )));
                }
                p.opts.push((a.to_owned(), v));
                i += 2;
            }
            "--" => {
                words_.extend(rest[i + 1..].iter().cloned());
                break;
            }
            _ => {
                words_.push(a.to_owned());
                i += 1;
            }
        }
    }
    p.task = words(&words_);
    if p.task.is_empty() {
        return Err(die(USAGE));
    }
    Ok(p)
}

/// `-p PROJECT` (its memory, and its folder unless -C says).
fn project_of(opts: &[(String, String)]) -> Option<String> {
    opts.iter()
        .find(|(f, _)| f == "-p" || f == "--project")
        .map(|(_, v)| v.clone())
}

/// The flag a stored option is kept under: `-p`, `-m`, `-e`, `-H`.
fn short(flag: &str) -> String {
    if flag.len() == 2 {
        return flag.to_owned();
    }
    match &flag[2..] {
        "harness" => "-H".to_owned(),
        rest => format!("-{}", &rest[..1]),
    }
}

/// Where the agent works: -C, else the folder of the project -p names, else a named conversation's own folder; the process
/// goes there (the conversation belongs to the directory it ran in).
fn work_in(host: &Host, agent: &str, p: &Parsed, err: &mut dyn Write) -> R<()> {
    let mut dir = p.dir.clone();
    if dir.is_none()
        && let Some(project) = project_of(&p.opts)
    {
        let ctx = crate::project::Ctx {
            state: host.state.clone(),
            home: host.home.clone(),
            cwd: std::env::current_dir().unwrap_or_default(),
            guest: crate::project::Guest::from_env(|n| host.var(n).map(Into::into)),
        };
        let found = crate::project::run(&ctx, &["--where".to_owned(), project.clone()]);
        let at = found.stdout.trim();
        dir = (!at.is_empty()).then(|| at.to_owned());
        if dir.is_none() && found.stderr.is_empty() {
            // An unknown project: bin/kith says so.
            let _ = writeln!(
                err,
                "kith ask: {project} has no folder on this Mac, so {agent} works here; to give it one: '{}' --register {project} DIR",
                host.kernel.join("bin/project-id").display()
            );
        }
    }
    if dir.is_none()
        && p.cont
        && let Some(name) = &p.name
    {
        dir = conv::load(host)
            .get(&format!("{agent}|as:{name}"))
            .and_then(|v| v.get("dir"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if let Some(d) = dir {
        let target = expand(host, &d);
        std::env::set_current_dir(&target).map_err(|e| {
            die(format!(
                "kith ask: cannot work in {d}: {}",
                e.to_string().split(" (os error").next().unwrap_or_default()
            ))
        })?;
    }
    Ok(())
}

/// `os.path.expanduser`.
fn expand(host: &Host, d: &str) -> PathBuf {
    match d.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            host.home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(d),
    }
}

/// What the conversation of this agent in this directory (or under this name) has kept.
struct Where {
    key: String,
    prev: Option<Value>,
}

fn conversation(host: &Host, agent: &str, p: &Parsed) -> Where {
    let cwd = std::env::current_dir().unwrap_or_default();
    let key = format!(
        "{agent}|{}",
        p.name
            .as_ref()
            .map_or_else(|| cwd.display().to_string(), |n| format!("as:{n}"))
    );
    let prev = p
        .cont
        .then(|| conv::load(host).get(&key).cloned())
        .flatten();
    Where { key, prev }
}

fn prev_str<'a>(prev: Option<&'a Value>, key: &str) -> Option<&'a str> {
    prev?.get(key)?.as_str()
}

/// Whether a live writer holds `sid`, as `kith-wake`'s `writer_live` reads it. The claims are the core's (a `claim.list`: a terminal,
/// a run or a turn), or, while the `claims` module is not switched (the core says so in those words), the kernel's own files
/// `<state>/live/terminal/<sid>.claim` and `.takeover.claim` of a process that still runs. Any other answer, or none (down, busy,
/// lost), reads as a writer: a follow-up must never start an engine on a session someone may be writing.
pub(super) fn writer_live(host: &Host, sid: &str) -> bool {
    match (host.call)("claim.list", &json!({})) {
        Ok(got) => got
            .get("live")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|c| c.get("conversation").and_then(Value::as_str) == Some(sid)),
        // The core's refusal (core/runtime/src/conversations/external.rs) carries a code; its words are free to change.
        Err(Fail::Refused(why)) if crate::wake::has_code(&why, crate::wake::CLAIMS_NOT_SWITCHED) => {
            let dir = host.state.join("live").join("terminal");
            [format!("{sid}.claim"), format!("{sid}.takeover.claim")]
                .iter()
                .any(|f| claim_file_alive(&dir.join(f)))
        }
        Err(_) => true,
    }
}

/// A kernel claim file whose process still runs (the start stamp is compared when the file holds one and it is of the same form).
fn claim_file_alive(path: &std::path::Path) -> bool {
    let Some(claim) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return false;
    };
    let Some(pid) = claim
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
    else {
        return false;
    };
    match procstart::probe(pid) {
        procstart::Probe::Gone => false,
        procstart::Probe::Alive(now) => {
            let recorded = claim.get("procStart").and_then(Value::as_str);
            match (recorded, now) {
                (Some(r), Some(n)) if procstart::comparable(r, &n) => r == n,
                _ => true,
            }
        }
        procstart::Probe::Unknown => true,
    }
}

/// A continued conversation keeps its project, model, effort and harness unless this call names new ones.
fn merged_opts(p: &Parsed, prev: Option<&Value>) -> Vec<(String, String)> {
    let mut kith_opts = p.opts.clone();
    for (flag, alias) in [
        ("-p", "--project"),
        ("-m", "--model"),
        ("-e", "--effort"),
        ("-H", "--harness"),
    ] {
        let given = p.opts.iter().any(|(f, _)| f == flag || f == alias);
        let kept = prev
            .and_then(|v| v.get("opts"))
            .and_then(|o| o.get(flag))
            .and_then(Value::as_str);
        if let (false, Some(v)) = (given, kept) {
            kith_opts.push((flag.to_owned(), v.to_owned()));
        }
    }
    kith_opts
}

/// The text of `kith ask AGENT ...` (the agent is the first word; `bg` was named anywhere before the task's `--`).
pub(super) fn ask(
    host: &Host,
    bg: bool,
    agent: &str,
    rest: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> R<i32> {
    let p = parse(rest)?;
    let started = std::env::current_dir().unwrap_or_default();
    work_in(host, agent, &p, err)?;
    let at = conversation(host, agent, &p);
    let prev = at.prev.as_ref();
    if bg {
        return background(host, agent, rest, &p, &at, out);
    }
    // The conversation is still running (a `--bg` job, or another run holds its session): the message is its next turn. (The
    // Python looked the job up by its key in `ask-bg/`; the ledger's `--bg` callee holds the session, which this finds.)
    if let Some(sid) = prev_str(prev, "session_id")
        && host.set("KITH_PERSON").is_none()
        && writer_live(host, sid)
    {
        let text = verbs::follow_up(
            host,
            sid,
            agent,
            None,
            &format!("ask-c:{sid}/{}", sha12(&p.task)),
            &p.task,
            &format!(
                "{} continued this conversation while it was running",
                host.set("KITH_AGENT")
                    .unwrap_or_else(|| "the owner".to_owned())
            ),
            err,
        )?;
        if text != "duplicate" {
            let _ = writeln!(
                err,
                "{agent} is still answering in that conversation: your message is its next turn"
            );
        }
        let _ = writeln!(out, "{text}");
        return Ok(0);
    }
    if p.cont && prev.is_none() {
        return Err(die(format!(
            "kith ask: no conversation with {agent} {} to continue; start one without -c",
            p.name.as_ref().map_or_else(
                || "from this directory".to_owned(),
                |n| format!("named {n}")
            )
        )));
    }

    let kith_opts = merged_opts(&p, prev);
    let mut opts = Map::new();
    for (f, v) in &kith_opts {
        opts.insert(short(f), json!(v));
    }

    // The same for every harness (bin/kith). The asked agent's room shows the run as a card, "asked by <caller>" (a terminal
    // when no agent is asking): bin/kith-sessions.
    let mut env: Vec<(String, String)> = host
        .env
        .iter()
        .filter(|(k, _)| k != "KITH_ASK_BG")
        .cloned()
        .collect();
    let mut put = |k: &str, v: String| {
        env.retain(|(n, _)| n != k);
        env.push((k.to_owned(), v));
    };
    put("KITH_VIA", "ask".to_owned());
    put("KITH_OUTPUT", "json".to_owned());
    put("KITH_LAUNCH_KIND", "ask".to_owned());
    put(
        "KITH_LAUNCH_LABEL",
        host.set("KITH_AGENT").unwrap_or_default(),
    );
    if let Some(mode) = host.set("KITH_ASK_PERMISSION_MODE") {
        put("KITH_PERMISSION_MODE", mode);
    }
    if let Some(sid) = prev_str(prev, "session_id") {
        put("KITH_RESUME", sid.to_owned());
    }

    // This call's id: a card the run parks on answers it later. A TERM from the helper wait ends this call and its row.
    let mid = new_id("ask");
    (host.watch_term)();
    let pid = std::process::id();
    let mut open = json!({
        "id": mid, "asked": agent, "brief": p.task, "mode": "fg", "task": p.task,
        "pid": pid, "cliPid": pid,
    });
    if let Some(stamp) = procstart::start_stamp(pid) {
        open["cliStart"] = json!(stamp);
    }
    open_row(host, open)?;
    // A run kith-remote started for another Mac's ask got its chain in KITH_ASK_*: the row holds it now, and the run's own asks
    // find it through KITH_ASK_ID.
    env.retain(|(k, _)| {
        !matches!(
            k.as_str(),
            "KITH_ASK_CHAIN"
                | "KITH_ASK_HOP"
                | "KITH_ASK_CROSS"
                | "KITH_ASK_FROM"
                | "KITH_ASK_COST"
        )
    });
    let mut put = |k: &str, v: String| {
        env.retain(|(n, _)| n != k);
        env.push((k.to_owned(), v));
    };
    put("KITH_LAUNCH_ASK_ID", mid.clone());
    // No host picks the account: bin/kith walks the account candidates (kith account pick).
    put("KITH_BACKUP_CHAIN", "on".to_owned());

    let mut argv = vec![
        host.kernel.join("bin/kith").display().to_string(),
        agent.to_owned(),
    ];
    for (f, v) in &kith_opts {
        argv.push(f.clone());
        argv.push(v.clone());
    }
    argv.push("--".to_owned());
    argv.push(p.task.clone());
    let ended = run_callee(host, &argv, &env);

    let (code, stdout, stderr) = match ended {
        Ended::Terminated => return Err(stopped(host, &mid)),
        Ended::Done {
            code,
            stdout,
            stderr,
        } => (code, stdout, stderr),
        Ended::NoStart(why) => (
            1,
            String::new(),
            format!("kith ask: cannot run the agent ({why})\n"),
        ),
    };
    let res = serde_json::from_str::<Value>(&stdout)
        .ok()
        .filter(Value::is_object);
    let Some(res) = res else {
        // Never answered: the row ends (parked on a card, it stays open for the card's answer).
        let _ = finish(host, json!({ "id": mid }));
        let said = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("kith ask: {agent} gave no answer\n")
        };
        let _ = write!(err, "{}", tail(&said, 3000));
        return Ok(if code == 0 { 1 } else { code });
    };
    let get = |k: &str| res.get(k).cloned().unwrap_or(Value::Null);
    let cost = get("total_cost_usd");
    let is_error = res.get("is_error").is_some_and(|v| {
        v.as_bool()
            .unwrap_or_else(|| v.as_i64().is_some_and(|n| n != 0))
    });
    let sid = res
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let text = res
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // The answer goes straight back to this call, so nothing is left for a wake (the row's `fg` mode says so). Parked, the
    // card's answer comes.
    let mut done = json!({
        "id": mid, "result": text, "cost": cost.as_f64().unwrap_or(0.0), "isError": is_error,
    });
    if !sid.is_empty() {
        done["session"] = json!(sid);
    }
    let parked = match finish(host, done) {
        Ok(got) => got.get("parked").and_then(Value::as_bool) == Some(true),
        Err(stop) => {
            let _ = writeln!(
                err,
                "{}",
                stop.msg.replacen(
                    "kith ask: ",
                    "kith ask: could not record the end of this ask: ",
                    1
                )
            );
            false
        }
    };

    let turns = prev
        .and_then(|v| v.get("turns"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + 1;
    let total = prev
        .and_then(|v| v.get("cost"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        + cost.as_f64().unwrap_or(0.0);
    if !sid.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_default();
        let _ = conv::keep(
            host,
            &at.key,
            json!({
                "agent": agent, "session_id": sid,
                "at": chrono::Local::now().format("%Y-%m-%dT%H:%M").to_string(),
                "turns": turns, "cost": (total * 1e4).round() / 1e4, "opts": opts,
                "dir": cwd.display().to_string(),
                "last": p.task.split_whitespace().collect::<Vec<_>>().join(" "),
            }),
        );
    }
    let sid_json = if sid.is_empty() {
        Value::Null
    } else {
        json!(sid)
    };
    if parked {
        // Not an answer: that comes when the card is answered.
        if p.as_json {
            let _ = writeln!(
                out,
                "{}",
                dumps(&json!({
                    "agent": agent, "answer": Value::Null, "waiting": true, "id": mid, "session_id": sid_json,
                    "turn": turns, "cost_usd": cost, "num_turns": get("num_turns"),
                }))
            );
        } else {
            let _ = writeln!(
                out,
                "{agent} is waiting on an approval card; its answer: kith ask --wait {mid}"
            );
        }
        return Ok(3);
    }
    if p.as_json {
        let _ = writeln!(
            out,
            "{}",
            dumps(&json!({
                "agent": agent, "answer": text, "session_id": sid_json, "error": is_error,
                "turn": turns, "cost_usd": cost, "num_turns": get("num_turns"),
            }))
        );
    } else {
        let _ = writeln!(out, "{text}");
        let cwd = std::env::current_dir().unwrap_or_default();
        let moved = if p.name.is_some() || cwd == started {
            String::new()
        } else if let Some(project) = project_of(&p.opts) {
            format!(" -p {project}")
        } else {
            format!(" -C '{}'", cwd.display())
        };
        let again = format!(
            "kith ask {agent} -c{} \"…\"",
            p.name.as_ref().map_or(moved, |n| format!(" --as {n}"))
        );
        let _ = writeln!(
            err,
            "\n[{agent} · turn {turns} · ${:.2} this turn · continue: {again}]",
            cost.as_f64().unwrap_or(0.0)
        );
    }
    Ok(i32::from(is_error))
}

/// `ask.open`: the caps and the insert in one step. A cap that refuses is exit 4 with the words `kith ask` prints.
pub(super) fn open_row(host: &Host, params: Value) -> R<Value> {
    match (host.call)("ask.open", &host.view(params)) {
        Ok(v) => Ok(v),
        Err(Fail::Refused(why)) => {
            let why = why.trim();
            if why.starts_with("kith-core:") {
                Err(die(format!("kith ask: {}", plain(why))))
            } else {
                Err(Stop {
                    code: REFUSED,
                    msg: format!("kith ask: {why}"),
                })
            }
        }
        Err(f) => Err(Stop::from_fail(&f)),
    }
}

/// `ask.finish` with the caller's view.
fn finish(host: &Host, params: Value) -> R<Value> {
    host.core("ask.finish", &host.view(params))
}

/// A TERM reached this call (the helper wait ran out, or a stop): the row ends as `--end` leaves it, and the runs below it
/// are stopped. Exit 143, as the Python's handler.
pub(super) fn stopped(host: &Host, mid: &str) -> Stop {
    let _ = verbs::end_rows(host, &json!({ "roots": [mid] }));
    Stop {
        code: 128 + 15,
        msg: String::new(),
    }
}

fn sha12(text: &str) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let hex = Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::new(), |mut a, b| {
            let _ = write!(a, "{b:02x}");
            a
        });
    hex[..12].to_owned()
}

/// `kith ask --bg AGENT ...`: the row carries the callee's turn, which the host runs (R2); this returns at once.
fn background(
    host: &Host,
    agent: &str,
    rest: &[String],
    p: &Parsed,
    at: &Where,
    out: &mut dyn Write,
) -> R<i32> {
    let mid = new_id("bg");
    // What a refused ask says, it says now, not in a log. The brief is the last word, as the record kept it.
    let mut open = json!({ "id": mid, "asked": agent, "mode": "bg", "task": p.task });
    if let Some(last) = rest.last() {
        open["brief"] = json!(last);
    }
    // The host's run gets what `-m -e -C -p` said (a continued conversation keeps its own), and the folder this call works in: the
    // detached wrapper ran in the caller's folder, and the host's run has none of its own. -H names the engine.
    let kith_opts = merged_opts(p, at.prev.as_ref());
    for (flag, key) in [("-m", "model"), ("-e", "effort"), ("-p", "project")] {
        if let Some((_, v)) = kith_opts.iter().rev().find(|(f, _)| short(f) == flag) {
            open[key] = json!(v);
        }
    }
    if let Some((_, h)) = kith_opts.iter().rev().find(|(f, _)| short(f) == "-H") {
        open["engine"] = json!(h);
    }
    if let Ok(dir) = std::env::current_dir() {
        open["folder"] = json!(dir.to_string_lossy());
    }
    // `-c` goes on the session the last turn of this conversation kept.
    if let Some(sid) = prev_str(at.prev.as_ref(), "session_id") {
        open["conversation"] = json!(sid);
    }
    open_row(host, open)?;
    let _ = writeln!(
        out,
        "started {agent} in the background as {mid}; its answer: kith ask --wait {mid}"
    );
    Ok(0)
}

/// How the callee's run ended.
pub(super) enum Ended {
    Done {
        code: i32,
        stdout: String,
        stderr: String,
    },
    /// A SIGTERM reached this process while it waited: the callee is killed.
    Terminated,
    /// The program could not start.
    NoStart(String),
}

/// Runs `argv` with `env` and no stdin, collecting what it prints. This is the one place a callee starts, so the Windows launch
/// (retire-python P1-12) is a swap of this function: `bin/kith` is a bash entry, which Windows has no shell for (the start
/// fails there and says so).
pub(super) fn run_callee(host: &Host, argv: &[String], env: &[(String, String)]) -> Ended {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Ended::NoStart(e.to_string()),
    };
    let (mut piped_out, mut piped_err) = (child.stdout.take(), child.stderr.take());
    let (out_reader, err_reader) = (
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(p) = piped_out.as_mut() {
                let _ = p.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }),
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(p) = piped_err.as_mut() {
                let _ = p.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }),
    );
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ended::Done {
                    code: status.code().unwrap_or(1),
                    stdout: out_reader.join().unwrap_or_default(),
                    stderr: err_reader.join().unwrap_or_default(),
                };
            }
            Ok(None) if (host.terminated)() => {
                let _ = child.kill();
                let _ = child.wait();
                return Ended::Terminated;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => return Ended::NoStart(e.to_string()),
        }
    }
}
