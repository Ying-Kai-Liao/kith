//! Whether this build may step `schedules` (the refusal rule of the architect, 2026-11-16). Stepping tombstones
//! `sched/state.json`, so `kith-sched` stands down and the core's fire becomes the only scheduler: a build whose core
//! cannot run the turns a fire makes would stop every routine and cron job. The step holds the core lock, so there is no
//! daemon to ask; this binary answers for itself, from five facts that must all hold.

use std::path::{Path, PathBuf};

use kith_api::Served;
use sha2::{Digest as _, Sha256};

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
    (!failed.is_empty()).then(|| format!("{MESSAGE} Failed: {}.", failed.join("; ")))
}

/// Condition 3: the method table has all three rows as core-served, and the daemon's own router answers them.
pub(super) fn serves_rows() -> bool {
    ROWS.iter().all(|name| {
        kith_api::method(name).is_some_and(|m| m.served == Served::Core)
            && crate::schedules::serves_write(name)
    })
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
        .map(|(s, _)| {
            s.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&")
        })
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
    let plist = home.join("Library/LaunchAgents/dev.kith.sched.plist");
    let program = std::fs::read_to_string(&plist)
        .ok()
        .and_then(|t| plist_words(&t))
        .and_then(|w| {
            let i = (1..w.len()).rev().find(|&i| w[i] == "tick")?;
            w.get(i - 1).cloned()
        })
        .ok_or_else(|| {
            format!(
                "no kith-sched program found in {}; update kith first",
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
