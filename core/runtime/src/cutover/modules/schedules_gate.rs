//! Whether this build may step `schedules` (the refusal rule of the architect, 2026-11-16). Stepping tombstones
//! `sched/state.json`, so `kith-sched` stands down and the core's fire becomes the only scheduler: a build whose core
//! cannot run the turns a fire makes would stop every routine and cron job. The step holds the core lock, so there is no
//! daemon to ask; this binary answers for itself, from four facts that must all hold.

use std::path::{Path, PathBuf};

use kith_api::Served;
use sha2::{Digest as _, Sha256};

/// The rows the core must serve, and the daemon route, for the fire to run anywhere.
const ROWS: [&str; 3] = ["schedule.tick", "schedule.runNow", "schedule.fire"];

/// Said by `step`, `step --dry-run` and `check`, exactly.
pub(super) const MESSAGE: &str = "not stepped: this build can't run the turns its schedules fire; kith-sched keeps running your jobs.";

/// What the gate looked at. Each field refuses alone.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent yes/no facts, each tested alone"
)]
pub(super) struct Facts {
    pub runs_fired_turns: bool,
    pub depends_on_claims: bool,
    pub serves_rows: bool,
    pub launchd_runs_this_build: bool,
}

pub(super) fn refusal(f: Facts) -> Option<String> {
    let ok =
        f.runs_fired_turns && f.depends_on_claims && f.serves_rows && f.launchd_runs_this_build;
    (!ok).then(|| MESSAGE.to_owned())
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
pub(super) fn launchd_runs_this_build(home: &Path) -> bool {
    let Ok(this) = std::env::current_exe() else {
        return false;
    };
    launchd_runs(home, &this)
}

pub(super) fn launchd_runs(home: &Path, this: &Path) -> bool {
    let plist = home.join("Library/LaunchAgents/dev.kith.serve.plist");
    let Some(program) = std::fs::read_to_string(plist)
        .ok()
        .and_then(|t| program_of(&t))
    else {
        return false;
    };
    let (Ok(theirs), Ok(mine)) = (PathBuf::from(program).canonicalize(), this.canonicalize())
    else {
        return false;
    };
    theirs == mine || build_id(&theirs).is_some_and(|t| build_id(&mine) == Some(t))
}

fn build_id(exe: &Path) -> Option<[u8; 32]> {
    std::fs::read(exe).ok().map(|b| Sha256::digest(b).into())
}

/// The program of an XML plist's `ProgramArguments`: its first word, or the word after `--` when the job runs the host
/// under the holder (`kith-holder supervise ... -- <host>`), as the host's installer writes it.
fn program_of(xml: &str) -> Option<String> {
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
    match words.iter().position(|w| w == "--") {
        Some(i) => words.get(i + 1).cloned(),
        None => words.first().cloned(),
    }
}
