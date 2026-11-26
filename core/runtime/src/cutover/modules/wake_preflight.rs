//! The `wake` step's preflight (design 2026-11-21-wake-s4, WK6 brief 1a). Plain refusals, none of which writes anything: a
//! refused step leaves no snapshot, record, marker or tombstone. Each item refuses alone and names what it saw.
//!
//! 1. the host is stopped (the words `claims` uses);
//! 2. not Windows (a Windows core is born switched, `switched.rs`, and `wake` is off there);
//! 3. no `kith-wake deliver` is running: the process lister must list this very process with its own arguments, else it cannot
//!    see processes and refuses (#1046). A deliverer that is running holds a turn the import would copy as a run in flight;
//! 4. every turn the files carry (`{run, pid, procStart}`) has a holder that is gone, by `kith_procstart`'s rule (the pid and
//!    its start stamp). With the host stopped a host-door take's holder is gone, so this also catches a deliverer item 3 missed.
//!
//! None of them may refuse what an earlier run of the step left behind: the tombstone, a `stepping` record, Python in Core
//! mode. They read the files (the queue's own `.tombstone` is not a file they look at), never the core, so a rerun after the
//! tombstone passes them.

use std::path::Path;

use super::schedules::{HostBlock, host_blocker};
use crate::cutover::Env;
use crate::cutover::authority_preflight::{Item, Preflight};
use crate::error::Result;
use crate::import::wake::{read_files, read_files_held};
use crate::wake::{Outside as _, System};

/// The process table as `(pid, arguments)`; `Err` when it cannot be read.
pub(super) type Lister = fn() -> std::result::Result<Vec<(u32, Vec<String>)>, String>;

/// The real lister: this user's processes. It must list the calling process with its own arguments, or the table is not read
/// here at all and an empty list would read as "no deliverer".
pub(super) fn own_processes() -> std::result::Result<Vec<(u32, Vec<String>)>, String> {
    let me = std::process::id();
    let all = kith_kernel::procstart::own_commands();
    if all.iter().any(|(pid, args)| *pid == me && !args.is_empty()) {
        Ok(all)
    } else {
        Err("cannot see processes: the process list does not hold this process".to_owned())
    }
}

/// Running `kith-wake deliver SESSION` processes other than this one: `[python, .../kith-wake, deliver, SESSION]`.
fn delivers(all: &[(u32, Vec<String>)]) -> Vec<String> {
    let me = std::process::id();
    all.iter()
        .filter(|(pid, args)| {
            *pid != me
                && args.windows(2).any(|w| {
                    w[1] == "deliver"
                        && Path::new(&w[0])
                            .file_name()
                            .is_some_and(|n| n == "kith-wake")
                })
        })
        .map(|(pid, args)| {
            let session = args.last().map_or("", String::as_str);
            format!("pid {pid} delivering {session}")
        })
        .collect()
}

pub(super) fn item(n: u8, what: &'static str, ids: Vec<String>) -> Item {
    Item {
        n,
        what,
        ids,
        stops: true,
        skipped: None,
    }
}

/// The host's refusal line in the words of `running` (see `claims`), or none.
pub(super) fn host_line(env: &Env, running: &str) -> Option<String> {
    match host_blocker(env)? {
        HostBlock::Running => Some(running.to_owned()),
        HostBlock::Unknown(e) => Some(format!("host.lock cannot be checked: {e}")),
    }
}

/// Items 1 to 4. `held`: the step holds both queue locks, so the files are read without asking for them again; a dry run holds
/// nothing and reads them under the Python's own shared locks.
pub(super) fn preflight(
    env: &Env,
    state: &Path,
    host_running: &str,
    list: Lister,
    held: bool,
) -> Result<Preflight> {
    let mut items = vec![
        item(
            1,
            "the host is stopped",
            host_line(env, host_running).into_iter().collect(),
        ),
        item(
            2,
            "this is not Windows",
            if cfg!(windows) {
                vec!["the wake step is not available on Windows (a Windows core is born switched, switched.rs)".into()]
            } else {
                Vec::new()
            },
        ),
    ];
    items.push(match list() {
        Ok(all) => item(3, "no kith-wake deliver is running", delivers(&all)),
        Err(why) => item(
            3,
            "no kith-wake deliver is running",
            vec![format!(
                "{why}; nothing can be said about a running deliver"
            )],
        ),
    });
    items.push(item(
        4,
        "every turn the files carry has a holder that is gone",
        live_holders(state, held)?,
    ));
    Ok(Preflight {
        items,
        notes: Vec::new(),
    })
}

/// The sessions whose turn in the files is held by a process that is still running (`kith_procstart`'s rule: the pid, started
/// when the turn says). A turn with no pid has no holder to wait for.
fn live_holders(state: &Path, held: bool) -> Result<Vec<String>> {
    let (files, _, _) = if held {
        read_files_held(state)?
    } else {
        read_files(state)?
    };
    let sys = System {
        state: state.to_path_buf(),
        gh: None,
    };
    Ok(files
        .items
        .values()
        .filter_map(|it| {
            let t = it.turn.as_ref()?;
            let pid = t.pid.filter(|p| *p > 0)?;
            sys.alive(pid, t.proc_start.as_deref())
                .then(|| format!("{} (turn {} held by pid {pid})", it.session, t.run))
        })
        .collect())
}
