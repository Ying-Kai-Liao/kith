//! Whether this build may step `schedules` (the refusal rule of the architect, 2026-11-16). Stepping tombstones
//! `sched/state.json`, so `kith-sched` stands down and the core's fire becomes the only scheduler: a build whose core
//! cannot run the turns a fire makes would stop every routine and cron job. The step holds the core lock, so there is no
//! daemon to ask; this binary answers for itself, from six facts that must all hold (1 to 5 and 7). One more, condition 6, which only a running system can
//! answer (an old `kith-sched tick` still in flight), is waited out by `schedules_switch` once the step holds `tick.lock`.

use std::path::{Path, PathBuf};

use crate::cutover::Env;
use kith_api::Served;
use sha2::{Digest as _, Sha256};

/// Where launchd's plists are: under the user's `HOME` (`~/Library/LaunchAgents`), which is not the Kith folder
/// (`env.home` is `$KITH_HOME`, `~/kith` or `~/.kith`: no plist lives there). Tests have no home of their own to set (the
/// environment is shared between them), so a test's `env.home` stands in for it.
pub(super) fn user_home(env: &Env) -> PathBuf {
    #[cfg(test)]
    return env.home.clone();
    #[cfg(not(test))]
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(|| env.home.clone(), PathBuf::from)
}

/// The rows the core must serve, and the daemon route, for the fire to run anywhere.
const ROWS: [&str; 3] = ["schedule.tick", "schedule.runNow", "schedule.fire"];

/// Said by `step`, `step --dry-run` and `check`, exactly.
pub(super) const MESSAGE: &str = "not stepped: this build can't run the turns its schedules fire; kith-sched keeps running your jobs.";

/// What the gate looked at. Each field refuses alone; `launchd` is `Err` with what launchd runs next to this binary.
pub(super) struct Facts {
    pub runs_fired_turns: bool,
    pub depends_on_claims: bool,
    pub serves_rows: bool,
    pub launchd: Result<(), String>,
    /// `Err` with why launchd's `kith-sched` does not take `sched/tick.lock`.
    pub sched_locks: Result<(), String>,
    /// `Err` with why the core could not start a `kith-holder`.
    pub holder: Result<(), String>,
}

/// `MESSAGE`, then which conditions failed.
pub(super) fn refusal(f: &Facts) -> Option<String> {
    let mut failed = Vec::new();
    if !f.runs_fired_turns {
        failed.push("1: the core does not run fired turns yet".to_owned());
    }
    if !f.depends_on_claims {
        failed.push("2: the module does not depend on claims".to_owned());
    }
    if !f.serves_rows {
        failed.push(format!(
            "3: the core does not serve {} and answer them in the daemon",
            ROWS.join(", ")
        ));
    }
    if let Err(why) = &f.launchd {
        failed.push(format!("4: {why}"));
    }
    if let Err(why) = &f.sched_locks {
        failed.push(format!("5: {why}"));
    }
    if let Err(why) = &f.holder {
        failed.push(format!("7: {why}"));
    }
    (!failed.is_empty()).then(|| format!("{MESSAGE} Failed: {}.", failed.join("; ")))
}

/// Condition 3: the method table has all three rows as core-served, and the daemon's own router answers them.
pub(super) fn serves_rows() -> bool {
    ROWS.iter().all(|name| {
        kith_api::method(name).is_some_and(|m| m.served == Served::Core)
            && crate::schedules::serves_write(name)
    })
}

/// Condition 7: the host launchd starts has a `kith-holder` to start. A cron job's turn is a `kith-sched run-turn` the
/// core runs in the holder, so without one the fired turns stay queued and every job stops. It answers for that host
/// (the program condition 4 reads and the plist's `KITH_HOLDER_BIN`), never this process or the shell's environment.
/// Only the program is checked, not a connected holder or its version (`kith doctor` shows those): the step holds the
/// core lock, so there is no daemon to ask.
pub(super) fn launchd_has_holder(home: &Path) -> Result<(), String> {
    let plist = home.join("Library/LaunchAgents/dev.kith.serve.plist");
    let text = std::fs::read_to_string(&plist).ok();
    let kith = text.as_deref().and_then(program_of);
    let over = text.as_deref().and_then(plist_env_holder);
    holder_for(kith.as_deref().map(Path::new), over)
}

/// [`launchd_has_holder`] for a host that runs `kith` with `KITH_HOLDER_BIN` = `over`.
pub(super) fn holder_for(kith: Option<&Path>, over: Option<String>) -> Result<(), String> {
    if crate::holder_link::holder_exe(over.map(Into::into), kith).is_some() {
        return Ok(());
    }
    Err(format!(
        "there is no kith-holder for launchd's host ({}; looked for KITH_HOLDER_BIN in its plist, then beside kith), so cron turns would stay queued; update kith first",
        kith.map_or_else(
            || "no program found in dev.kith.serve.plist".to_owned(),
            |k| k.display().to_string()
        )
    ))
}

/// `KITH_HOLDER_BIN` in the plist's `EnvironmentVariables`, unescaped.
fn plist_env_holder(xml: &str) -> Option<String> {
    let env = xml.split_once("<key>EnvironmentVariables</key>")?.1;
    let rest = env.split_once("<key>KITH_HOLDER_BIN</key>")?.1;
    let value = rest.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(unescape(value))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// Condition 4: the program launchd's `dev.kith.serve` starts is this binary, or has the same build id (the hash of its
/// bytes, as the host reports as `binaryHash`). An unreadable or missing plist, or a program that cannot be read, is no.
pub(super) fn launchd_runs_this_build(home: &Path) -> Result<(), String> {
    let this = std::env::current_exe().map_err(|e| format!("this binary is unknown ({e})"))?;
    launchd_runs(home, &this)
}

pub(super) fn launchd_runs(home: &Path, this: &Path) -> Result<(), String> {
    let plist = home.join("Library/LaunchAgents/dev.kith.serve.plist");
    let Some(program) = std::fs::read_to_string(&plist)
        .ok()
        .and_then(|t| program_of(&t))
    else {
        return Err(format!(
            "no program found in {}; this binary is {}",
            plist.display(),
            this.display()
        ));
    };
    let differs = || format!("launchd runs {program}, this binary is {}", this.display());
    let (Ok(theirs), Ok(mine)) = (PathBuf::from(&program).canonicalize(), this.canonicalize())
    else {
        return Err(differs());
    };
    if theirs == mine || build_id(&theirs).is_some_and(|t| build_id(&mine) == Some(t)) {
        Ok(())
    } else {
        Err(differs())
    }
}

fn build_id(exe: &Path) -> Option<[u8; 32]> {
    std::fs::read(exe).ok().map(|b| Sha256::digest(b).into())
}

/// The program of an XML plist's `ProgramArguments`: the word just before the last `serve` that is followed by `--port`.
/// The installer writes `[launcher.. , holder.. --, <kith bin>, serve, --port, N]`, so this finds the host whichever
/// launcher or holder is in front of it.
fn program_of(xml: &str) -> Option<String> {
    let words = plist_words(xml)?;
    let i = (1..words.len().saturating_sub(1))
        .rev()
        .find(|&i| words[i] == "serve" && words[i + 1] == "--port")?;
    words.get(i - 1).cloned()
}

/// The `<string>`s of an XML plist's `ProgramArguments`, unescaped.
fn plist_words(xml: &str) -> Option<Vec<String>> {
    let rest = xml.split_once("<key>ProgramArguments</key>")?.1;
    let array = rest.split_once("<array>")?.1.split_once("</array>")?.0;
    let words: Vec<String> = array
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split_once("</string>"))
        .map(|(s, _)| unescape(s))
        .collect();
    Some(words)
}

/// The line of `kith-sched` that says it takes `sched/tick.lock` (#1033); a kith-sched without it can tick while the
/// step holds the lock and fire a slot twice.
const TICK_LOCK_MARKER: &str = "KITH_SCHED_TICK_LOCK = 1";

/// Condition 5: the `kith-sched` that launchd's `dev.kith.sched` runs (read through `cli/current` and any other
/// symlink, not the copy beside this binary) carries the tick-lock marker. The installer writes `[launcher run --quiet
/// --script <kith-sched> tick]` or `[<kith-sched> tick]`, so the program is the word before the last `tick`.
pub(super) fn launchd_sched_locks(home: &Path) -> Result<(), String> {
    let plist = sched_plist(home);
    let program = sched_program_word(home).ok_or_else(|| {
        format!(
            "no kith-sched program found in {}; run `kith-sched --install`",
            plist.display()
        )
    })?;
    let stale = || {
        format!("launchd's kith-sched ({program}) does not take sched/tick.lock; update kith first")
    };
    let real = PathBuf::from(&program)
        .canonicalize()
        .map_err(|_| stale())?;
    let text = std::fs::read_to_string(&real).map_err(|_| stale())?;
    if text.lines().any(|l| l.starts_with(TICK_LOCK_MARKER)) {
        Ok(())
    } else {
        Err(stale())
    }
}

fn sched_plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/dev.kith.sched.plist")
}

fn sched_program_word(home: &Path) -> Option<String> {
    std::fs::read_to_string(sched_plist(home))
        .ok()
        .and_then(|t| plist_words(&t))
        .and_then(|w| {
            let i = (1..w.len()).rev().find(|&i| w[i] == "tick")?;
            w.get(i - 1).cloned()
        })
}

/// The file condition 5 reads: launchd's `kith-sched`, through every symlink. `None` when the plist or the file is not
/// there (condition 5 refuses that on its own).
pub(super) fn sched_program(home: &Path) -> Option<PathBuf> {
    PathBuf::from(sched_program_word(home)?).canonicalize().ok()
}

/// One running `kith-sched tick` of this home: its pid and, when it could be read, its start stamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tick {
    pub pid: u32,
    pub stamp: Option<String>,
}

/// Lists the ticks of the program `kith-sched` (given as its real path) that are running now.
/// `Err` when the process table cannot be read: condition 6 then refuses (fails closed), never passes on an empty list.
pub(super) type Lister = fn(&Path) -> Result<Vec<Tick>, String>;

/// The real lister: processes of our own user whose arguments hold a word that resolves to `program`, followed by
/// `tick` (the launcher form `launcher run --script <kith-sched> tick` and the plain `<kith-sched> tick` both match;
/// `run-turn`, `ls` and another home's kith-sched do not). Arguments are read exactly, never split from `ps`.
pub(super) fn list_ticks(program: &Path) -> Result<Vec<Tick>, String> {
    let me = std::process::id();
    let all = kith_kernel::procstart::own_commands();
    // A table that cannot be read lists nothing, which must not read as "no tick is running": this process is always in it.
    if !all.iter().any(|(pid, args)| *pid == me && !args.is_empty()) {
        return Err("the process list cannot be read here".to_owned());
    }
    Ok(all
        .into_iter()
        .filter(|(pid, args)| {
            *pid != me
                && args.windows(2).any(|w| {
                    w[1] == "tick"
                        && w[0].contains("kith-sched")
                        && PathBuf::from(&w[0])
                            .canonicalize()
                            .is_ok_and(|p| p == program)
                })
        })
        .map(|(pid, _)| Tick {
            pid,
            stamp: kith_kernel::procstart::start_stamp(pid),
        })
        .collect())
}

/// Whether `old` is still among `now`: the same pid with the same stamp, or by pid alone when either stamp is unknown
/// (a wrong "gone" would let a tick that holds old code go on).
fn still_there(old: &Tick, now: &[Tick]) -> bool {
    now.iter().any(|n| {
        n.pid == old.pid
            && match (&old.stamp, &n.stamp) {
                (Some(a), Some(b)) => kith_kernel::procstart::same_process(a, b),
                _ => true,
            }
    })
}

/// Condition 6, taken right after `tick.lock`: the ticks that were running when the lock was taken may be launchd's old
/// code (started before `cli/current` moved), which cannot be told apart by any clock. Waits until each is gone. A tick
/// that starts after the listing is new code and blocks on our lock. `Err` says why not: one still running at `wait`, or a
/// process table that cannot be read (never taken for "no tick").
pub(super) fn wait_out_old_ticks(
    list: Lister,
    program: &Path,
    wait: std::time::Duration,
) -> Result<(), String> {
    let old = list(program)?;
    let until = std::time::Instant::now() + wait;
    loop {
        let now = list(program)?;
        let Some(left) = old.iter().find(|o| still_there(o, &now)) else {
            return Ok(());
        };
        if std::time::Instant::now() >= until {
            return Err(format!(
                "a kith-sched tick (pid {}) is still running; try again in a minute",
                left.pid
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
