//! The lock-alone clear (design 2026-11-16-offturn-lock-alone, rule L-clear): a one-shot stub whose run goes on under the lock
//! alone (no core answered its claim, `--lock-alone`) takes the lock, then, **holding it and before the engine starts**, clears
//! every warm engine marked as having the conversation loaded (`loaded`):
//! - a marker whose pid is gone, or runs with another start, is stale: removed;
//! - a marked stub that holds another key's lock is running a host turn: the run is refused ([`Cleared::HostTurn`], exit 75);
//! - any other is SIGKILLed with its process group (a resident stub leads its group, engine and children in it) and waited
//!   for, at most `within`; one that is not confirmed gone refuses the run ([`Cleared::NotGone`], exit 75).
//!
//! The order is the rule: lock, then clear, then the engine. Cleared before the lock, the host could load, run a turn and wake
//! itself in between. A warm engine that woke itself writes with no claim and no lock, and with the core down nothing else
//! can stop it; the host's own kill of an off-turn wake (offturn.rs) comes only after its first writes.
//!
//! This runs with the lock held, so it does in-process work only: kill(2), readdir, non-blocking lock probes and process
//! table reads. Never a fork, an exec or a spawned helper (a child started under the lock outlives the stub's accounting;
//! the test below reads this file for those names). It reads nothing of the core: the stub stays policy-free.

use std::path::Path;
use std::time::{Duration, Instant};

use kith_procstart::{Probe, comparable, probe};

use crate::{Error, Taken, loaded, take};

/// What the clear came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Cleared {
    /// No live warm engine has the conversation loaded any more: the engine may start.
    Clear,
    /// The marked stub `pid` holds another conversation's lock (a host turn runs in that process). Nothing was killed.
    HostTurn(u32),
    /// The marked stub `pid` could not be checked, or was signalled and is not confirmed gone.
    NotGone(u32),
}

/// Clears `key`'s marked warm engines (module docs). The caller holds `key`'s lock.
pub fn clear(folder: &Path, key: &str, within: Duration) -> Result<Cleared, Error> {
    let me = std::process::id();
    for (name, pid, start) in loaded::markers(folder, key)? {
        if pid == me {
            continue;
        }
        match same(pid, &start) {
            Some(false) => {
                loaded::unmark(folder, key, &name)?;
                continue;
            }
            None => return Ok(Cleared::NotGone(pid)),
            Some(true) => {}
        }
        for other in loaded::keys_of(folder, &name)? {
            if other != key && !matches!(take(folder, &other)?, Taken::Held(_)) {
                return Ok(Cleared::HostTurn(pid));
            }
        }
        if !kill_group(pid, &start, within) {
            return Ok(Cleared::NotGone(pid));
        }
        // Its stub is gone, so nothing else removes its markers; ours goes now.
        loaded::unmark(folder, key, &name)?;
    }
    Ok(Cleared::Clear)
}

/// Whether `pid` is the process that wrote the marker: `Some(false)` gone or another start; `None` when it cannot be told
/// (no start readable, or stamps of different forms): never killed on a guess, never ignored either.
fn same(pid: u32, start: &str) -> Option<bool> {
    match probe(pid) {
        Probe::Gone => Some(false),
        Probe::Alive(Some(now)) if comparable(start, &now) => Some(now == start),
        Probe::Alive(_) | Probe::Unknown => None,
    }
}

/// SIGKILL to `pid`'s process group (it must lead one: an engine in a group of its own would outlive the kill), then waits
/// until neither it nor anything of its group runs. True when confirmed gone within `within`.
fn kill_group(pid: u32, start: &str, within: Duration) -> bool {
    let Ok(group) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: getpgid only reads the process table.
    let leads = unsafe { libc::getpgid(group) } == group;
    match same(pid, start) {
        Some(false) => return true,
        Some(true) if leads => {}
        _ => return false,
    }
    // SAFETY: killpg(2) on a group this marker names, its leader checked by start just now.
    unsafe { libc::killpg(group, libc::SIGKILL) };
    let until = Instant::now() + within;
    loop {
        if probe(pid) == Probe::Gone && group_runs(group) == Some(false) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a process of group `group` still runs (a zombie does not: it waits only for its parent to reap it). `None` when
/// the table cannot be read.
#[cfg(target_os = "linux")]
fn group_runs(group: libc::pid_t) -> Option<bool> {
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(name) = entry.map(|e| e.file_name()) else {
            continue;
        };
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        // Gone between the readdir and the read: not running.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // After the command's closing parenthesis: state, ppid, pgrp.
        let rest = stat.rfind(')').map(|i| &stat[i + 1..])?;
        let mut fields = rest.split_whitespace();
        let (Some(state), Some(_), Some(pgrp)) = (fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if pgrp.parse() == Ok(group) && state != "Z" {
            return Some(true);
        }
    }
    Some(false)
}

/// `sysctl(kern.proc.pgrp)`: one `kinfo_proc` (648 bytes on Intel and Apple silicon) per member, its `p_stat` byte at 36, as
/// `kith-procstart` reads it (libc has no `kinfo_proc` for macOS).
#[cfg(target_os = "macos")]
fn group_runs(group: libc::pid_t) -> Option<bool> {
    const ROW: usize = 648;
    const SZOMB: u8 = 5;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PGRP, group];
    // Room for 64 members, more than a warm engine's group has; a full buffer is ENOMEM and reads as "cannot tell".
    let mut buf = vec![0_u8; ROW * 64];
    let mut len = buf.len();
    // SAFETY: `mib` is four ints; `buf` is `len` bytes and outlives the call, which writes at most `len` bytes into it.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || !len.is_multiple_of(ROW) {
        return None;
    }
    Some(
        buf[..len]
            .as_chunks::<ROW>()
            .0
            .iter()
            .any(|row| row[36] != SZOMB),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn group_runs(_: libc::pid_t) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    /// The clear runs under the writer lock: nothing in it may start a process. A source check, as for the redeem in main.rs.
    #[test]
    fn the_clear_forks_nothing() {
        let src = include_str!("clear.rs").replace("\r\n", "\n");
        let code = &src[..src.find("#[cfg(test)]").expect("the tests are last")];
        for banned in [
            "Command", "spawn", "fork(", "exec", "system(", "popen", "daemon(",
        ] {
            let hits: Vec<&str> = code
                .lines()
                .filter(|l| !l.trim_start().starts_with("//") && l.contains(banned))
                .collect();
            assert!(
                hits.is_empty(),
                "the clear runs with the lock held and must not start a process: {hits:?}"
            );
        }
    }
}
