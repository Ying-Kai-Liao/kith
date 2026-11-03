//! The core's side of the holder (docs/plans/runtime-design.md section 4, steps 5 and 7; M0c).
//!
//! The daemon starts `kith-holder` detached when none answers, says hello with the secret and its generation, and
//! records the holder's `instance` and the processes it listed in core.db. When a hello meets an instance that is not
//! the one recorded, the old holder is gone and on Unix nothing ended its processes: every process group it reported
//! is ended, matched by pid and start time so a reused pid is never hit, and confirmed gone before the new instance is
//! recorded. There are no claims yet (M3), so there is nothing to release.
//!
//! Until M1 nothing uses the holder, so none of this may cost anything: a missing binary, a holder that will not
//! start, one that crashes or one that answers garbage is a line in the core log and a retry later, never an error
//! for `kith serve` or the CLI. Every step here runs on its own thread; startup never waits for it. A `refused`
//! frame is only ever a failed connection: generation authority stays in core.db, so a fake holder cannot make a
//! core step down.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kith_holder::client::{Client, ConnectError};
use kith_holder::{launch, proto};

use crate::daemon::Shared;
use crate::db::{HolderRecord, RecordedProc};
use crate::error::Result;
use crate::layout::Layout;
use crate::{lock, log, perms};

pub(crate) struct HolderConfig {
    /// The holder program. `None`: look for `$KITH_HOLDER_BIN`, then `kith-holder` beside this program.
    pub exe: Option<PathBuf>,
    /// How long a holder with no connection and no process lives before it exits by itself.
    pub idle: Duration,
    /// How often a connected core asks the holder for its list.
    pub poll: Duration,
    pub retry_min: Duration,
    pub retry_max: Duration,
    /// How long ending an old instance's process groups may take before the core tries again later.
    pub kill_wait: Duration,
}

impl Default for HolderConfig {
    fn default() -> Self {
        Self {
            exe: None,
            idle: Duration::from_secs(120),
            poll: Duration::from_secs(5),
            retry_min: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            kill_wait: Duration::from_secs(10),
        }
    }
}

/// What `kith doctor` shows of the holder.
#[derive(Clone, Debug, Default)]
pub(crate) struct HolderState {
    /// `starting`, `connected`, `absent` (no program to start), `unreachable` (retrying), `stopped`.
    pub state: &'static str,
    pub instance: Option<String>,
    pub pid: Option<u32>,
    /// Windows: the holder is in the starter's job, so ending the daemon's job ends it too.
    pub tied_to_starter: bool,
    pub error: Option<String>,
    pub binary: Option<PathBuf>,
    pub procs: usize,
}

fn find_exe(cfg: &HolderConfig) -> Option<PathBuf> {
    if let Some(e) = &cfg.exe {
        return e.is_file().then(|| e.clone());
    }
    if let Some(e) = std::env::var_os("KITH_HOLDER_BIN").filter(|v| !v.is_empty()) {
        let e = PathBuf::from(e);
        return e.is_file().then_some(e);
    }
    let name = if cfg!(windows) {
        "kith-holder.exe"
    } else {
        "kith-holder"
    };
    let beside = std::env::current_exe().ok()?.parent()?.join(name);
    beside.is_file().then_some(beside)
}

/// The holder secret: made once, owner-only, in the core folder.
fn secret(layout: &Layout) -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(&layout.holder_secret) {
        let t = text.trim();
        if !t.is_empty() {
            return Ok(t.to_owned());
        }
    }
    let secret = crate::owner::random_secret()?;
    perms::create_private_file(&layout.holder_secret)?;
    std::fs::write(&layout.holder_secret, &secret)?;
    perms::make_private(&layout.holder_secret)?;
    Ok(secret)
}

fn sleep_unless_stopped(shared: &Shared, total: Duration) {
    let end = Instant::now() + total;
    while !shared.stopping() && Instant::now() < end {
        std::thread::sleep(
            Duration::from_millis(40).min(end.saturating_duration_since(Instant::now())),
        );
    }
}

/// Runs until the daemon stops. Never returns an error and never panics the daemon: every failure is a state and a
/// log line.
pub(crate) fn supervise(shared: &Arc<Shared>, layout: &Layout, cfg: &HolderConfig) {
    let mut delay = cfg.retry_min;
    let mut last_error = String::new();
    while !shared.stopping() {
        match connect(shared, layout, cfg) {
            Ok(client) => {
                delay = cfg.retry_min;
                last_error.clear();
                match connected(shared, client, cfg) {
                    Ok(()) => continue,
                    Err(why) => fail(shared, &mut last_error, "unreachable", &why),
                }
            }
            Err((state, why)) => {
                fail(shared, &mut last_error, state, &why);
                delay = if state == "absent" {
                    cfg.retry_max
                } else {
                    (delay * 2).min(cfg.retry_max)
                };
            }
        }
        sleep_unless_stopped(shared, delay);
    }
    shared.set_holder(|h| h.state = "stopped");
}

/// Notes a failure once per distinct message in the core log, so a holder that stays down is one line, not one per try.
fn fail(shared: &Shared, last: &mut String, state: &'static str, why: &str) {
    if *last != why {
        log::note(
            &shared.logs,
            &format!("holder: {why} (will retry; nothing uses the holder before M1)"),
        );
        why.clone_into(last);
    }
    shared.set_holder(|h| {
        h.state = state;
        h.error = Some(why.to_owned());
        h.procs = 0;
    });
}

fn connect(
    shared: &Arc<Shared>,
    layout: &Layout,
    cfg: &HolderConfig,
) -> std::result::Result<Client, (&'static str, String)> {
    let unreachable = |why: String| ("unreachable", why);
    let Some(exe) = find_exe(cfg) else {
        shared.set_holder(|h| h.binary = None);
        return Err((
            "absent",
            "no kith-holder program to start (looked for $KITH_HOLDER_BIN and beside kith)"
                .to_owned(),
        ));
    };
    shared.set_holder(|h| h.binary = Some(exe.clone()));
    let secret = secret(layout).map_err(|e| unreachable(e.to_string()))?;
    let generation = shared
        .with_core(|c| Ok(c.generation()))
        .map_err(|e| unreachable(e.to_string()))?;
    let generation = u64::try_from(generation).unwrap_or(0);
    let dir = &layout.holder_dir;
    let first = match Client::connect(dir, &secret, generation) {
        Err(ConnectError::Io(e)) if nothing_listens(&e) => {
            // Nothing answers: start one detached (its own session; out of our job on Windows), then say hello.
            launch::ensure(&exe, dir, &secret, cfg.idle)
                .map_err(|e| unreachable(format!("can't start {}: {e}", exe.display())))?;
            Client::connect(dir, &secret, generation)
        }
        other => other,
    };
    first.map_err(|e| match e {
        ConnectError::Refused(r) => unreachable(format!(
            "the holder refused the hello ({:?}); that settles nothing, the generation in core.db is the authority",
            r.why
        )),
        ConnectError::Impostor => {
            unreachable("what answers at the holder's socket does not know the holder secret".to_owned())
        }
        ConnectError::Io(e) => unreachable(format!("the holder did not answer properly: {e}")),
    })
}

fn nothing_listens(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    matches!(e.kind(), NotFound | ConnectionRefused)
}

/// Reconciles the record with this hello, then keeps the record current until the connection ends. `Ok` means the
/// supervisor was told to stop; `Err` carries why the link was lost.
fn connected(
    shared: &Arc<Shared>,
    mut client: Client,
    cfg: &HolderConfig,
) -> std::result::Result<(), String> {
    let procs = record_procs(&client.welcome.procs);
    let instance = client.welcome.instance.clone();
    let (pid, tied) = (client.welcome.pid, client.welcome.tied_to_starter);
    reconcile(shared, &instance, &procs, cfg)?;
    shared.set_holder(|h| {
        h.state = "connected";
        h.instance = Some(instance.clone());
        h.pid = Some(pid);
        h.tied_to_starter = tied;
        h.error = None;
        h.procs = procs.len();
    });
    log::note(
        &shared.logs,
        &format!(
            "holder: connected (instance {instance}, pid {pid}, {} process(es))",
            procs.len()
        ),
    );
    let mut known = procs;
    while !shared.stopping() {
        sleep_unless_stopped(shared, cfg.poll);
        if shared.stopping() {
            break;
        }
        let held = client
            .list()
            .map_err(|e| format!("the holder stopped answering: {e}"))?;
        let now = record_procs(&held);
        if now != known {
            reconcile(shared, &instance, &now, cfg)?;
            shared.set_holder(|h| h.procs = now.len());
            known = now;
        }
    }
    Ok(())
}

/// The listed processes with the start time the system gives their pid now, noted while the holder is alive so a
/// later kill can tell them from a stranger that was handed the pid.
fn record_procs(held: &[proto::Held]) -> Vec<RecordedProc> {
    held.iter()
        .filter(|h| h.exit.is_none())
        .map(|h| RecordedProc {
            pid: h.pid,
            started: lock::process_start(h.pid).unwrap_or_default(),
            conv: h.conv.clone(),
        })
        .collect()
}

/// Step 7: a hello that meets an instance other than the recorded one ends what the old one held, confirms it gone,
/// and only then records the new instance. A record from M0a has no instance: nothing is ended on its account.
pub(crate) fn reconcile(
    shared: &Shared,
    instance: &str,
    procs: &[RecordedProc],
    cfg: &HolderConfig,
) -> std::result::Result<(), String> {
    let recorded = shared
        .with_core(|c| c.holder_record())
        .map_err(|e| e.to_string())?;
    if let Some(old) = recorded.instance.as_deref().filter(|old| *old != instance) {
        // Never a process the new instance lists: it is alive and its own by that holder's account.
        let stale: Vec<RecordedProc> = recorded
            .procs
            .iter()
            .filter(|p| !procs.iter().any(|n| n.pid == p.pid))
            .cloned()
            .collect();
        log::note(
            &shared.logs,
            &format!(
                "holder: instance {old} is gone, now {instance}; ending {} process group(s) it held",
                stale.len()
            ),
        );
        if !end_groups(&shared.logs, &stale, cfg.kill_wait) {
            return Err(format!(
                "processes of the old holder instance {old} are still running; not recording the new one yet"
            ));
        }
    }
    if recorded.instance.as_deref() != Some(instance) || recorded.procs != procs {
        let rec = HolderRecord {
            instance: Some(instance.to_owned()),
            procs: procs.to_vec(),
        };
        shared
            .with_core(|c| c.save_holder_record(&rec))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Ends the process group of each recorded process whose pid still has the recorded start time, and waits for the
/// groups to be empty. A pid with another start time (or none we can read) is not touched. True when nothing it was
/// asked to end is left.
#[cfg(unix)]
fn end_groups(logs: &Path, procs: &[RecordedProc], wait: Duration) -> bool {
    let mut mine = Vec::new();
    for p in procs {
        if p.started.is_empty() {
            log::note(
                logs,
                &format!(
                    "holder: pid {} has no recorded start time; left alone",
                    p.pid
                ),
            );
        } else if lock::process_start(p.pid).as_deref() == Some(p.started.as_str()) {
            mine.push(p.pid);
        } else {
            log::note(
                logs,
                &format!(
                    "holder: pid {} is not the process that was recorded (another start time, or gone); left alone",
                    p.pid
                ),
            );
        }
    }
    if mine.is_empty() {
        return true;
    }
    for pid in &mine {
        signal_group(*pid, "TERM");
    }
    let begun = Instant::now();
    let mut forced = false;
    loop {
        mine.retain(|pid| group_alive(*pid));
        if mine.is_empty() {
            return true;
        }
        if begun.elapsed() >= wait {
            return false;
        }
        // Politely first; whatever ignored it gets the hard signal after a third of the time.
        if !forced && begun.elapsed() >= wait / 3 {
            for pid in &mine {
                signal_group(*pid, "KILL");
            }
            forced = true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Windows needs no extra step: each spawn's job object ends with the holder (runtime-design section 4, step 7).
#[cfg(not(unix))]
fn end_groups(_: &Path, _: &[RecordedProc], _: Duration) -> bool {
    true
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) -> bool {
    // An absolute path, never `kill` from PATH: this ends process groups, and the daemon's PATH is not its own to trust.
    // (unsafe code is forbidden here, so no killpg.)
    let kill = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or("/bin/kill");
    std::process::Command::new(kill)
        .args(["-s", signal, "--"])
        .arg(format!("-{pid}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    signal_group(pid, "0")
}
