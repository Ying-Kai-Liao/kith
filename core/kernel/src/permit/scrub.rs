//! `kith_paths.guest_scrub`: an environment without any variable that names a path in a guest's writable folders (any entry of a
//! list such as PATH counts). A process outside their sandbox must not take a temp file, a cache or a program from where they
//! write. Ported so the door's cards, the door's helper calls and the lookup of `python3` all use the same scrubbed run.

use super::Env;
use crate::launch::prompt;
use crate::paths::realpath;
use sha2::{Digest as _, Sha256};
use std::path::Path;

/// `str.lower` on macOS, whose volumes fold case; nothing elsewhere (`kith_paths.in_guest_folders`).
fn fold(s: &str) -> String {
    if cfg!(target_os = "macos") {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// `kith_paths.guest_tmp(pid)`: the guest's Claude Code temp folder (real path), if one fits.
fn guest_tmp(env: &Env, kernel: &Path, pid: &str) -> Option<String> {
    let hash = |x: &str| {
        use std::fmt::Write as _;
        Sha256::digest(x.as_bytes())
            .iter()
            .fold(String::new(), |mut a, b| {
                let _ = write!(a, "{b:02x}");
                a
            })
    };
    let machine = realpath(&env.paths(kernel).machine);
    prompt::guest_tmp(&machine, pid, crate::threads::os::uid(), &hash)
        .map(|d| d.to_string_lossy().into_owned())
}

/// `kith_paths.in_guest_folders(path, pid)`: whether `path` is, by real path, in a folder `pid`'s runs write in (their files
/// folder of any agent, or their Claude Code temp folder).
fn in_guest_folders(path: &str, pid: &str, env: &Env, kernel: &Path) -> bool {
    let real = fold(&realpath(Path::new(path)).to_string_lossy());
    let agents = fold(&realpath(&env.paths(kernel).agents).to_string_lossy());
    if let Ok(rest) = Path::new(&real).strip_prefix(&agents) {
        let parts: Vec<_> = rest.components().collect();
        if parts.len() >= 4
            && parts[1].as_os_str() == "files"
            && parts[2].as_os_str() == "people"
            && parts[3].as_os_str().to_string_lossy() == fold(pid)
        {
            return true;
        }
    }
    guest_tmp(env, kernel, pid).is_some_and(|r| {
        let r = fold(&realpath(Path::new(&r)).to_string_lossy());
        real == r || real.starts_with(&format!("{}/", r.trim_end_matches('/')))
    })
}

/// `guest_scrub(env, pid)`: the environment left, and the names it dropped.
pub fn guest_scrub(env: &Env, kernel: &Path, pid: &str) -> (Env, Vec<String>) {
    let names = |v: &str| {
        v.split(':')
            .any(|x| x.starts_with('/') && in_guest_folders(x, pid, env, kernel))
    };
    let mut out = env.clone();
    let mut dropped = Vec::new();
    for (k, v) in env.vars() {
        if names(v) {
            out.vars.remove(k);
            dropped.push(k.clone());
        }
    }
    (out, dropped)
}
