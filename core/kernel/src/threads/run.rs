//! `kith kernel threads run AGENT -- [stub...] claude ARGS...`: the wrapper an interactive `kith AGENT` ends in (the `run` of
//! `kernel/bin/kith-sessions`). It runs the engine as its CHILD, not in its own process: the launcher's shell pid keeps the
//! terminal's writer claim, claude is a child in the same process group, and the notes about getting the session back can
//! follow it. So it forks and waits, as the Python does. (The thread-writers model's "execs claude, same pid" row is
//! `kith-lifecycle compact`'s, not this.)
//!
//! What it does, in this order:
//!
//! 1. under `kith-stub`, a status pipe: the stub says on it whether it holds the conversation lock (`locked`, `busy`; nothing
//!    at all is a lock error), because the engine can itself exit 75, 126 or 127;
//! 2. forks the child ([`super::runproc::spawn`]: signals and fds as the architect's MUSTs on #1090 say) and ignores `^C`, passes
//!    TERM and HUP on;
//! 3. waits, and every `KITH_IDLE_POLL` seconds (30) judges the idle clock: the one place a human session sleeps (rule 3);
//! 4. when the child is gone: the quit intent of a clean `/exit`, the claims this process held given back, the sleep decision
//!    when the wrapper itself stopped it, the take-over note (else the resume note), and the wake queue's delivery. These run
//!    whatever happened before, a failed exec included, and none of them can stop the next.
//!
//! The helpers that outlive the wrapper (the sweep at sleep, the wake queue) start in a session of their own with no stdio.
//! Nothing here writes to stderr with a macro that panics: a closed stderr must not stop `wake_owed`.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::core::Core;
use super::resolve::Host;
use super::runproc::{self, Wait};
use super::writer::Taker;
use super::{Ctx, intent};
use crate::lifecycle::transcript;

/// The cache life at or below which there is no timer, so the decision to compact is made at the moment of sleep.
const SHORT_CACHE: i64 = transcript::SHORT_CACHE;
/// `KITH_SLEEP_MINUTES`' default: an hour.
const DEFAULT_SLEEP_AFTER: f64 = 3600.0;
/// How long the status pipe is read after the child is gone (a descendant may have kept it open).
const STATUS_READ: Duration = Duration::from_secs(3);

/// Where the notes go: a line each (stderr in the program, a list in the tests).
pub type Say<'a> = &'a dyn Fn(&str);

/// Says a line on stderr. A closed or full stderr is not an error worth stopping for.
pub fn say(line: &str) {
    let mut e = std::io::stderr();
    let _ = e.write_all(line.as_bytes());
    let _ = e.write_all(b"\n");
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn float_env(c: &Ctx, name: &str) -> Option<f64> {
    (c.env)(name)?.trim().parse::<f64>().ok()
}

/// Seconds between the wrapper's looks at its child's idle clock (`KITH_IDLE_POLL`, default 30).
fn idle_poll(c: &Ctx) -> f64 {
    (c.env)("KITH_IDLE_POLL").map_or(30.0, |v| {
        v.trim().parse::<f64>().map_or(30.0, |n| n.max(0.05))
    })
}

/// Seconds of idle after which a wrapper stops its own child (`KITH_SLEEP_MINUTES`, default 60).
fn sleep_after(c: &Ctx) -> f64 {
    float_env(c, "KITH_SLEEP_MINUTES").map_or(DEFAULT_SLEEP_AFTER, |m| (m * 60.0).max(1.0))
}

/// `[{pid, ppid}]` rows of `ps`, within `limit`; `None` when `ps` does not answer in time.
fn ps_text(limit: Duration) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(
            out.ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned()),
        );
    });
    rx.recv_timeout(limit).ok().flatten()
}

/// The pid the registry names for the run: the child itself, or, when the child is kith-stub (the conversation lock's
/// launcher, which keeps claude as its own child), that child. `None` while the stub has not started claude yet.
fn engine_pid(child: i64, stub: bool) -> Option<i64> {
    if !stub {
        return Some(child);
    }
    ps_text(Duration::from_secs(3))?.lines().find_map(|row| {
        let mut f = row.split_whitespace();
        let (own, parent) = (f.next()?, f.next()?);
        if f.next().is_some()
            || parent.parse::<i64>().ok() != Some(child)
            || !own.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        own.parse().ok()
    })
}

/// Whether a descendant of `leader` lives in another session than `leader`'s own: a dev server or build that outlives the
/// shell that started it. `rows` are `(pid, parent)`; `session` finds a live process's session. Unsure counts as busy (the
/// same rule as the host's idle check, `detached_descendant`): `leader` gone is busy, a descendant gone since the listing
/// holds nothing.
pub fn detached_descendant(
    rows: &[(i64, i64)],
    leader: i64,
    session: impl Fn(i64) -> Option<i64>,
) -> bool {
    let Some(own) = session(leader) else {
        return true;
    };
    let mut kids: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    for &(pid, parent) in rows {
        kids.entry(parent).or_default().push(pid);
    }
    let mut seen = std::collections::HashSet::from([leader]);
    let mut stack = vec![leader];
    while let Some(parent) = stack.pop() {
        for &pid in kids.get(&parent).into_iter().flatten() {
            let Some(sid) = session(pid) else { continue };
            if sid != own {
                return true;
            }
            if seen.insert(pid) {
                stack.push(pid);
            }
        }
    }
    false
}

/// [`detached_descendant`] for the real process table. `ps` that does not answer, or answers with a row that is not two
/// numbers, is busy: a check that cannot see must not end a session.
fn detached_children(pid: i64) -> bool {
    let Some(text) = ps_text(Duration::from_secs(5)) else {
        return true;
    };
    let mut rows = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 2 {
            continue;
        }
        let (Ok(p), Ok(pp)) = (f[0].parse::<i64>(), f[1].parse::<i64>()) else {
            return true;
        };
        rows.push((p, pp));
    }
    detached_descendant(&rows, pid, runproc::session_of)
}

fn registry_entries(c: &Ctx) -> Vec<(PathBuf, Map<String, Value>)> {
    let Some(dir) = c.reg_dir() else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
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

fn started_at(e: &Map<String, Value>) -> &str {
    e.get("startedAt").and_then(Value::as_str).unwrap_or("")
}

/// The newest registry entry that names `pid` (and a session, when `with_session`), by `startedAt`; the later file wins a tie.
fn entry_of(c: &Ctx, pid: i64, with_session: bool) -> Option<Map<String, Value>> {
    let mut last: Option<Map<String, Value>> = None;
    for (_, e) in registry_entries(c) {
        if e.get("pid").and_then(Value::as_i64) != Some(pid)
            || (with_session && !transcript::truthy(e.get("sessionId")))
        {
            continue;
        }
        if last
            .as_ref()
            .is_none_or(|l| started_at(&e) >= started_at(l))
        {
            last = Some(e);
        }
    }
    last
}

fn str_of<'a>(e: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    e.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Rule 3: the one place a human session sleeps. The idle clock is the latest of the last turn, the process start and the last
/// keystroke (the terminal's access time); after `sleep_after` of it, with nothing outstanding (rule 7), the wrapper writes
/// its intent and ends its own child. Nothing outside the terminal's own process tree ever signals a terminal session on a
/// timer. True once the signal is sent.
fn idle_stop(c: &Ctx, child: i64, born: f64, eng: Option<i64>) -> bool {
    let eng = eng.unwrap_or(child);
    let Some(e) = entry_of(c, eng, true) else {
        return false;
    };
    let (Some(path), Some(sid), Some(state)) = (
        str_of(&e, "transcriptPath"),
        str_of(&e, "sessionId"),
        crate::lifecycle::state_dir(c),
    ) else {
        return false; // not a session we can judge (not listed, or it has not written yet): never slept
    };
    let transcript_path = Path::new(path);
    let tty = runproc::stdin_is_tty();
    let key = if tty { runproc::stdin_atime() } else { None };
    let since = [transcript::last_turn_at(transcript_path), Some(born), key]
        .into_iter()
        .flatten()
        .fold(f64::MIN, f64::max);
    if now() - since < sleep_after(c) {
        return false;
    }
    if !transcript::outstanding(transcript_path).is_empty() || detached_children(eng) {
        return false;
    }
    if !intent::write_intent(&state, sid, "idle", eng) {
        return false;
    }
    // Kai may have typed since the check above: looked at again just before the signal (S2). A terminal whose time cannot be
    // read is one that cannot be judged: left awake.
    if tty && runproc::stdin_atime().is_none_or(|t| t > key.unwrap_or(0.0)) {
        intent::clear_intent(&state, sid);
        return false;
    }
    i32::try_from(child).is_ok_and(runproc::terminate)
}

/// The wrapper just put its session to sleep: on a 5-minute cache there is no timer, so the decision to compact is made now
/// (F12 rule 5). Detached; a cache with a timer needs nothing here, the tick decides at 54 minutes.
fn asleep(c: &Ctx, kernel: &Path, pid: i64) {
    let Some(e) = entry_of(c, pid, true) else {
        return;
    };
    let Some(sid) = str_of(&e, "sessionId") else {
        return;
    };
    if transcript::cache_life(c.env) > SHORT_CACHE {
        return;
    }
    let args: Vec<OsString> = [
        kernel.join("bin").join("kith-lifecycle").into_os_string(),
        "sweep".into(),
        "--session".into(),
        sid.into(),
        "--at-sleep".into(),
    ]
    .into();
    let _ = runproc::detached("python3", &args);
}

fn record_quit(c: &Ctx, pid: i64) {
    let Some(e) = entry_of(c, pid, true) else {
        return;
    };
    let (Some(sid), Some(state)) = (str_of(&e, "sessionId"), crate::lifecycle::state_dir(c)) else {
        return;
    };
    if intent::read_intent(&state, sid).is_none_or(|i| i.is_empty()) {
        intent::write_intent(&state, sid, "quit", pid);
    }
}

/// A terminal held its session while wakes for it queued (a reply, a card, a waiter): now that it let go, the kernel's wake
/// queue may deliver them without waiting for the next tick. A kith ask this session made and didn't read the answer of
/// becomes one of them first (kith-ask --sweep, M8).
fn wake_owed(c: &Ctx, kernel: &Path) {
    let Some(state) = c.state.as_deref() else {
        return;
    };
    let bin = kernel.join("bin");
    let script = r#""$0" "$1" --sweep; ls "$3"/*.json >/dev/null 2>&1 && exec "$0" "$2" drain"#;
    let args: Vec<OsString> = [
        OsString::from("-c"),
        script.into(),
        "python3".into(),
        bin.join("kith-ask").into_os_string(),
        bin.join("kith-wake").into_os_string(),
        state.join("sessions").join("queue").into_os_string(),
    ]
    .into();
    let _ = runproc::detached("/bin/sh", &args);
}

/// `%Y-%m-%dT%H:%M:%SZ` as epoch seconds.
#[allow(clippy::many_single_char_names)]
fn iso_epoch(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    #[allow(clippy::cast_precision_loss)]
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss) as f64)
}

/// The thread name for a session as the host knows it (`resolveThread`), else the session id.
fn thread_name(host: &dyn Host, sid: &str) -> String {
    host.resolve_thread(sid)
        .as_ref()
        .and_then(|r| r.get("threadId"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map_or_else(|| sid.to_owned(), str::to_owned)
}

/// Prints the take-over note when the host took the session over while it ran; true if it did.
fn taken_note(c: &Ctx, host: &dyn Host, say: Say, agent: &str, pid: i64) -> bool {
    let mut noted = false;
    for (f, e) in registry_entries(c) {
        let taken = f.with_extension("taken");
        if e.get("pid").and_then(Value::as_i64) != Some(pid) || !taken.exists() {
            continue;
        }
        // A `.taken` left by an earlier crash is older than this run.
        let Some(began) = iso_epoch(started_at(&e)) else {
            continue;
        };
        let fresh = std::fs::metadata(&taken)
            .and_then(|m| m.modified())
            .is_ok_and(|t| {
                t.duration_since(UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
                    >= began
            });
        let _ = std::fs::remove_file(&taken);
        if fresh && let Some(sid) = str_of(&e, "sessionId") {
            say(&format!(
                "continued in Kith — get it back with: kith {agent} --resume {}",
                thread_name(host, sid)
            ));
            noted = true;
        }
    }
    noted
}

/// claude ends with "claude --resume <id>", which skips the agent (its account, memory, project): say the kith command. The
/// registry names the session this process ended with (the id changes on /clear); a session with no transcript, which claude
/// does not offer to resume, gets no note.
fn resume_note(c: &Ctx, host: &dyn Host, say: Say, agent: &str, pid: i64) {
    let Some(last) = entry_of(c, pid, false) else {
        return;
    };
    let (Some(t), Some(sid)) = (str_of(&last, "transcriptPath"), str_of(&last, "sessionId")) else {
        return;
    };
    if Path::new(t).exists() {
        say(&format!(
            "Resume it as {agent} with: kith {agent} --resume {}",
            thread_name(host, sid)
        ));
    }
}

/// What the stub said on its status pipe, against the exit code: "busy" is busy (75) whatever the engine would exit with,
/// "locked" then 126/127 means the engine never started, and no word at all (EOF) is a lock error, not an engine exit.
fn stub_verdict(fd: i32, code: i32, say: Say) -> i32 {
    let said = runproc::read_status(fd, STATUS_READ);
    crate::lifecycle::proc::close(fd);
    let word = String::from_utf8_lossy(&said).trim().to_owned();
    if word == "busy" {
        return 75;
    }
    if word == "locked" && (code == 126 || code == 127) {
        say(&format!(
            "kith: the engine did not start ({code}); it never ran"
        ));
    } else if word.is_empty() && code == 70 {
        say("kith: the conversation lock could not be taken (not busy); this run did not start");
    }
    code
}

/// `kith-stub`'s command line gets `--status-fd N` before its `--`, once, when it has none.
fn with_status_fd(cmd: &[String]) -> Option<Vec<String>> {
    let base = Path::new(&cmd[0]).file_name()?;
    if base != "kith-stub" || cmd.iter().any(|a| a == "--status-fd") {
        return None;
    }
    let cut = cmd.iter().position(|a| a == "--")?;
    let mut out = cmd[..cut].to_vec();
    out.extend(["--status-fd".to_owned(), runproc::STATUS_FD.to_string()]);
    out.extend_from_slice(&cmd[cut..]);
    Some(out)
}

/// Runs `cmd` as a child so the take-over note can follow it, and returns the exit code the wrapper ends with (128+n for a
/// signal). `kernel` is the kernel folder, where the helpers that outlive the wrapper are.
pub fn run(
    c: &Ctx,
    host: &dyn Host,
    core: Option<&dyn Core>,
    kernel: &Path,
    agent: &str,
    cmd: &[String],
    say: Say,
) -> i32 {
    // The signal handlers and the pid they pass on are this process's: one run at a time.
    let _one = crate::lifecycle::proc::exclusive();
    let stub = Path::new(&cmd[0])
        .file_name()
        .is_some_and(|b| b == "kith-stub");
    let mut status = None;
    let mut argv = cmd.to_vec();
    if let Some(with) = with_status_fd(cmd)
        && let Ok(pipe) = crate::lifecycle::proc::pipe()
    {
        argv = with;
        status = Some(pipe);
    }
    let argv_os: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let spawned = match runproc::spawn(&argv_os, status) {
        Ok(s) => s,
        Err(e) => {
            if let Some((r, w)) = status {
                crate::lifecycle::proc::close(r);
                crate::lifecycle::proc::close(w);
            }
            say(&format!("kith: cannot run {}: {e}", cmd[0]));
            return 1;
        }
    };
    if let Some(errno) = spawned.exec_failed {
        let why = std::io::Error::from_raw_os_error(errno).to_string();
        let why = why.split(" (os error").next().unwrap_or(&why);
        say(&format!(
            "kith: cannot run {}: [Errno {errno}] {why}",
            cmd[0]
        ));
    }
    let child = i64::from(spawned.pid);
    let born = now();
    let (mut st, mut stopped, mut eng) = (0, false, None);
    let mut next_check = born + idle_poll(c);
    loop {
        match runproc::try_wait(spawned.pid) {
            Wait::Done(s) => {
                st = s;
                break;
            }
            Wait::Gone => break,
            Wait::Running => {}
        }
        // Until the engine is found a short tick: an engine that exits within one long tick would leave no pid to name in
        // the quit intent (a clean /exit then reads as a crash).
        std::thread::sleep(Duration::from_millis(if eng.is_some() { 250 } else { 50 }));
        if eng.is_none() {
            eng = engine_pid(child, stub); // kept: the stub is gone by the time the notes below are written
        }
        if !stopped && now() >= next_check {
            next_check = now() + idle_poll(c);
            stopped = idle_stop(c, child, born, eng);
        }
    }
    runproc::forget_child();
    spawned.saved.restore();
    after(
        c,
        host,
        core,
        say,
        (kernel, agent),
        eng.unwrap_or(child),
        (st, stopped),
    );
    let mut ended = runproc::exit_code(st);
    if let Some((r, _)) = status {
        ended = stub_verdict(r, ended, say);
    }
    if ended < 0 { 128 - ended } else { ended }
}

/// What follows the child, whatever became of it: each step on its own, in this order (the take-over note is the resume
/// note's alternative, and the wake queue's delivery is last).
fn after(
    c: &Ctx,
    host: &dyn Host,
    core: Option<&dyn Core>,
    say: Say,
    (kernel, agent): (&Path, &str),
    eng: i64,
    (st, stopped): (i32, bool),
) {
    if !stopped && libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0 {
        record_quit(c, eng); // Kai's own /exit: not a crash (rule 8)
    }
    Taker::new(c, core).release_claims(i64::from(std::process::id()));
    if stopped {
        asleep(c, kernel, eng);
    }
    if !taken_note(c, host, say, agent, eng) {
        resume_note(c, host, say, agent, eng);
    }
    wake_owed(c, kernel);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_descendant_in_another_session_is_detached_and_one_in_the_same_is_not() {
        let rows = [(2, 1), (3, 2), (4, 3), (9, 8)];
        let same = |p: i64| Some(if p == 9 { 7 } else { 5 });
        assert!(!detached_descendant(&rows, 1, same));
        let dev_server = |p: i64| Some(if p == 4 { 77 } else { 5 });
        assert!(detached_descendant(&rows, 1, dev_server));
    }

    #[test]
    fn unsure_is_busy_but_a_descendant_that_left_holds_nothing() {
        let rows = [(2, 1), (3, 1)];
        // the leader itself cannot be looked up
        assert!(detached_descendant(&rows, 1, |_| None));
        // a child gone since the listing, whatever is under it
        let gone = |p: i64| (p != 2).then_some(5);
        assert!(!detached_descendant(&[(2, 1), (3, 2)], 1, gone));
    }

    #[test]
    fn a_process_table_with_a_loop_ends() {
        let rows = [(2, 1), (1, 2)];
        assert!(!detached_descendant(&rows, 1, |_| Some(5)));
    }

    #[test]
    fn the_registry_time_reads_as_python_strptime_does() {
        assert_eq!(iso_epoch("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(iso_epoch("2026-11-20T15:13:20Z"), Some(1_795_187_600.0));
        assert_eq!(iso_epoch("2026-11-20 15:13:20"), None);
        assert_eq!(iso_epoch("2026-13-20T15:13:20Z"), None);
        assert_eq!(iso_epoch(""), None);
    }

    #[test]
    fn the_stub_is_told_where_to_say_whether_it_holds_the_lock() {
        let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            with_status_fd(&v(&["/x/kith-stub", "--key", "s", "--", "claude"])),
            Some(v(&[
                "/x/kith-stub",
                "--key",
                "s",
                "--status-fd",
                "9",
                "--",
                "claude"
            ]))
        );
        // already told, no `--`, or not the stub: left as it is
        assert_eq!(
            with_status_fd(&v(&["kith-stub", "--status-fd", "4", "--", "c"])),
            None
        );
        assert_eq!(with_status_fd(&v(&["kith-stub", "c"])), None);
        assert_eq!(with_status_fd(&v(&["claude", "--", "x"])), None);
    }
}
