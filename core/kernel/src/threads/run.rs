//! `run AGENT -- CMD...`: the wrapper an interactive `kith AGENT` execs into, a port of `run` and its helpers in
//! `kernel/bin/kith-sessions`. It runs CMD (claude) as a child so the take-over note can follow it: signals and the exit code
//! pass through; the one place a human session sleeps is here (rule 3: after `KITH_SLEEP_MINUTES` of idle with nothing
//! outstanding, it writes its intent and ends its own child); afterwards it gives back the claims of this process and says
//! how to get the session back: `kith AGENT --resume ID`, also when the host took it over.
//!
//! The lifecycle's compaction decision and the kernel's wake queue stay programs of their own (`kith-lifecycle`,
//! `kith-ask`, `kith-wake`), started detached from the kernel folder exactly as the Python wrapper starts them.

use std::ffi::OsString;
use std::io::IsTerminal as _;
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use super::idle::{SHORT_CACHE, cache_life, idle_since, outstanding, sleep_after};
use super::resolve::Host;
use super::{Ctx, clock, intent, os, release_claims};

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Seconds between the wrapper's looks at its child's idle clock (`KITH_IDLE_POLL`, default 30).
fn idle_poll(c: &Ctx) -> f64 {
    (c.env)("KITH_IDLE_POLL")
        .map_or(30.0, |v| v.trim().parse::<f64>().unwrap_or(30.0))
        .max(0.05)
}

fn text(e: &Map<String, Value>, k: &str) -> String {
    e.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// The registry entries in the registry folder, by file name.
fn entries(c: &Ctx) -> Vec<(std::path::PathBuf, Map<String, Value>)> {
    let Some(d) = c.reg_dir() else {
        return Vec::new();
    };
    let mut files: Vec<_> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|f| {
            match serde_json::from_str::<Value>(&std::fs::read_to_string(&f).ok()?).ok()? {
                Value::Object(o) => Some((f, o)),
                _ => None,
            }
        })
        .collect()
}

/// The registry entry of the session process `pid` runs (the newest one naming it); with `named`, one that names a session.
fn entry_of(c: &Ctx, pid: i64, named: bool) -> Option<Map<String, Value>> {
    let mut last: Option<Map<String, Value>> = None;
    for (_, e) in entries(c) {
        let has_sid = match e.get("sessionId") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
        };
        if e.get("pid").and_then(Value::as_i64) == Some(pid)
            && (!named || has_sid)
            && last
                .as_ref()
                .is_none_or(|l| text(&e, "startedAt") >= text(l, "startedAt"))
        {
            last = Some(e);
        }
    }
    last
}

/// Rule 3: the one place a human session sleeps. The idle clock is the latest of the last turn, the process start and the
/// last keystroke (the terminal's access time); after `sleep_after` of it, with nothing outstanding (rule 7), the wrapper
/// writes its intent and ends its own child. Nothing outside the terminal's own process tree ever signals a terminal
/// session on a timer. True once the signal is sent.
fn idle_stop(c: &Ctx, child: i64, born: f64) -> bool {
    let (Some(state), Some(e)) = (c.state.as_deref(), entry_of(c, child, true)) else {
        return false;
    };
    let transcript = text(&e, "transcriptPath");
    let sid = text(&e, "sessionId");
    if transcript.is_empty() {
        return false; // not a session we can judge (not listed, or it has not written yet): never slept
    }
    let tty = std::io::stdin().is_terminal();
    let key = if tty { os::stdin_atime() } else { None };
    let transcript = Path::new(&transcript);
    let since = idle_since(Some(transcript), born, key);
    if now() - since < sleep_after(c.env) || !outstanding(Some(transcript), Some(child)).is_empty()
    {
        return false;
    }
    intent::write_intent(state, &sid, "idle", child);
    // The person may have typed since the check above: looked at again just before the signal (S2).
    if tty && os::stdin_atime().unwrap_or(0.0) > key.unwrap_or(0.0) {
        intent::clear_intent(state, &sid);
        return false;
    }
    os::signal(child, os::SIGTERM);
    true
}

/// The wrapper just put its session to sleep: on a 5-minute cache there is no timer, so the decision to compact is made now
/// (F12 rule 5). Detached; a cache with a timer needs nothing here, the tick decides at 54 minutes.
fn asleep(c: &Ctx, kernel: &Path, child: i64) {
    let Some(e) = entry_of(c, child, true) else {
        return;
    };
    if cache_life(c.env) > SHORT_CACHE {
        return;
    }
    os::spawn_detached(&[
        kernel.join("bin").join("kith-lifecycle").into_os_string(),
        "sweep".into(),
        "--session".into(),
        text(&e, "sessionId").into(),
        "--at-sleep".into(),
    ]);
}

/// The wrapper's child said it was done and the person's own `/exit` is not a crash (rule 8).
fn record_quit(c: &Ctx, child: i64) {
    let (Some(state), Some(e)) = (c.state.as_deref(), entry_of(c, child, true)) else {
        return;
    };
    let sid = text(&e, "sessionId");
    if intent::read_intent(state, &sid).is_none_or(|i| i.is_empty()) {
        intent::write_intent(state, &sid, "quit", child);
    }
}

/// A terminal held its session while wakes for it queued (a reply, a card, a waiter): now that it let go, the kernel's wake
/// queue may deliver them without waiting for the next tick. A kith ask this session made and didn't read the answer of
/// becomes one of them first (`kith-ask --sweep`, M8).
fn wake_owed(c: &Ctx, kernel: &Path) {
    let (Some(state), Some(_)) = (c.state.as_deref(), c.reg_dir()) else {
        return;
    };
    let bin = kernel.join("bin");
    let script = r#""$0" --sweep; ls "$2"/*.json >/dev/null 2>&1 && exec "$1" drain"#;
    os::spawn_detached(&[
        OsString::from("/bin/sh"),
        "-c".into(),
        script.into(),
        bin.join("kith-ask").into_os_string(),
        bin.join("kith-wake").into_os_string(),
        state.join("sessions").join("queue").into_os_string(),
    ]);
}

fn thread_name(host: &dyn Host, sid: &str, entry_sid: &str) -> String {
    let r = host.resolve_thread(sid).unwrap_or(Value::Null);
    match r.get("threadId").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => entry_sid.to_owned(),
    }
}

/// `claude` ends with "claude --resume <id>", which skips the agent (its account, memory, project): say the kith command. The
/// registry names the session this process ended with (the id changes on /clear); a session with no transcript, which claude
/// does not offer to resume, gets no note.
fn resume_note(c: &Ctx, host: &dyn Host, agent: &str, pid: i64, err: &dyn Fn(&str)) {
    if !c.reg_dir().is_some_and(|d| d.is_dir()) {
        return;
    }
    let Some(last) = entry_of(c, pid, false) else {
        return;
    };
    let transcript = text(&last, "transcriptPath");
    if !transcript.is_empty() && Path::new(&transcript).exists() {
        let sid = text(&last, "sessionId");
        err(&format!(
            "Resume it as {agent} with: kith {agent} --resume {}\n",
            thread_name(host, &sid, &sid)
        ));
    }
}

/// Prints the take-over note when the host took the session over while it ran; true if it did. The error is what Python's
/// `strptime` or a missing key says, which `run` prints as `kith-sessions: ...` and then skips the rest.
fn taken_note(
    c: &Ctx,
    host: &dyn Host,
    agent: &str,
    pid: i64,
    err: &dyn Fn(&str),
) -> Result<bool, String> {
    if !c.reg_dir().is_some_and(|d| d.is_dir()) {
        return Ok(false);
    }
    let mut noted = false;
    for (f, e) in entries(c) {
        let taken = f.with_extension("taken");
        if e.get("pid").and_then(Value::as_i64) != Some(pid) || !taken.exists() {
            continue;
        }
        // A .taken left by an earlier crash is older than this run.
        let Some(started) = e.get("startedAt") else {
            return Err("'startedAt'".to_owned());
        };
        let started = started.as_str().unwrap_or("");
        let began = clock::parse(started)
            .filter(|s| {
                s.timed && s.offset == Some(0) && started.len() == 20 && started.ends_with('Z')
            })
            .map(clock::Stamp::epoch)
            .ok_or_else(|| {
                format!("time data '{started}' does not match format '%Y-%m-%dT%H:%M:%SZ'")
            })?;
        let mtime = std::fs::metadata(&taken)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |d| d.as_secs_f64());
        let _ = std::fs::remove_file(&taken);
        if mtime >= began {
            let sid = text(&e, "sessionId");
            err(&format!(
                "continued in Kith \u{2014} get it back with: kith {agent} --resume {}\n",
                thread_name(host, &sid, &sid)
            ));
            noted = true;
        }
    }
    Ok(noted)
}

/// `Exec format error`-style text as Python's `OSError` prints it: `[Errno 2] No such file or directory`.
fn errno_text(e: &std::io::Error) -> String {
    let msg = e.to_string();
    let msg = msg.split(" (os error").next().unwrap_or(&msg);
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {msg}"),
        None => msg.to_owned(),
    }
}

/// Runs `cmd` as a child of ours so the take-over note can follow it. The child gets the default signals back; we ignore ^C
/// (the child decides what it means), pass TERM/HUP on and return its exit code (128+n for a signal). `err` says what the
/// wrapper has to say, at once.
pub fn run(
    c: &Ctx,
    host: &dyn Host,
    kernel: &Path,
    agent: &str,
    cmd: &[String],
    err: &dyn Fn(&str),
) -> i32 {
    let (mut code, mut clean_exit, mut stopped) = (127, false, false);
    let mut child_pid = -1_i64;
    let born = now();
    match os::spawn_child(cmd) {
        Err(e) => err(&format!(
            "kith: cannot run {}: {}\n",
            cmd[0],
            errno_text(&e)
        )),
        Ok(mut child) => {
            child_pid = i64::from(child.id());
            os::guard_signals(child_pid);
            let poll = idle_poll(c);
            let mut next_check = born + poll;
            loop {
                match child.try_wait() {
                    Ok(Some(st)) => {
                        use std::os::unix::process::ExitStatusExt as _;
                        clean_exit = st.code() == Some(0);
                        code = st
                            .code()
                            .or_else(|| st.signal().map(|s| 128 + s))
                            .unwrap_or(0);
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
                std::thread::sleep(Duration::from_millis(250));
                if !stopped && now() >= next_check {
                    next_check = now() + poll;
                    stopped = idle_stop(c, child_pid, born);
                }
            }
        }
    }
    if !stopped && clean_exit {
        record_quit(c, child_pid); // the person's own /exit: not a crash (rule 8)
    }
    release_claims(c, i64::from(std::process::id()));
    if stopped {
        asleep(c, kernel, child_pid);
    }
    match taken_note(c, host, agent, child_pid, err) {
        Ok(true) => {}
        Ok(false) => resume_note(c, host, agent, child_pid, err),
        Err(m) => {
            err(&format!("kith-sessions: {m}\n"));
            return code;
        }
    }
    wake_owed(c, kernel);
    code
}
