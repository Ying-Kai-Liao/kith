//! `kith-pkg resolve`: the pinned package folder a run starts from, ported from `kernel/bin/kith-pkg`. Every run is pinned to a
//! snapshot of the agent's package; edits, by anyone, become a new version for the next run, and a version that fails its
//! checks is never used while an earlier good one exists.
//!
//! `resolve` is not read-only, and this port does what the Python does: it downloads the third-party skills `skills.json`
//! links (the git of `KITH_GITHUB_GIT`, else GitHub), checks the working copy, snapshots it into the store, moves the `.good`
//! pointer, removes versions nothing needs any more and commits the good version into the agent's own repo. The other
//! verbs of `kith-pkg` stay with the Python.
//!
//! Python is still used for one thing: judging the `permissions.allow` / `autoMode.allow` rules of a package's `settings.json`
//! against the must-ask classes (`kith_guard.must_ask_rule`), which has not moved to Rust (`launch/guard.rs` ports the other
//! half of that file). It is asked only when `settings.json` has such rules.
//!
//! The port keeps the shape of the Python: one function each, with the same checks in the same order, because the order of
//! the messages and of the files written is part of what the golden record (`kernel/tests/difftest/golden/pkg-resolve.json`)
//! freezes. `KITH_CORE=python` stays the rollback: the `pkg` line of `kernel/lib/core-switched.txt` is the switch, removing it the rollback.

// The ports keep the shape of their Python originals (one line each way) so a reader can hold them side by side.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::format_push_string,
    clippy::needless_pass_by_value
)]

pub mod cli;
mod fetch;
mod git;
mod problems;
mod py;
mod resolve;
mod sha1;
mod store;
mod text;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use py::{PyErr, R, is_dir, is_file, os_err};

/// Folders `kith-pkg` leaves out of a package: live state, never snapshotted (`STATE`, at the top only) and noise (`SKIP`).
const STATE: [&str; 2] = ["memory", "data"];
const SKIP: [&str; 5] = [
    "__pycache__",
    ".DS_Store",
    "node_modules",
    ".editing",
    ".git",
];
/// Bump when the contents of a snapshot change (it is part of the fingerprint).
const FORMAT: &str = "2";
/// A `.editing` marker older than this (seconds) stops holding releases back.
const EDITING_MAX_AGE: f64 = 3600.0;
/// Versions kept besides good, pinned, running and recent ones.
const KEEP: usize = 2;

/// Where things are, and the environment a run depends on.
pub struct Ctx {
    /// The kernel folder (`KITH`): its `PROTOCOL.md`, `services/`, `agents/` and `lib/`.
    pub kernel: PathBuf,
    /// The agents folder (`KITH_AGENTS`).
    pub agents: PathBuf,
    /// The state folder (`KITH_STATE`).
    pub state: PathBuf,
    /// The snapshot store (`KITH_STORE`).
    pub store: PathBuf,
    /// `KITH_GITHUB_GIT`, else GitHub, without a trailing slash.
    pub github: String,
    /// Where the Python helper is found (`python3`).
    pub python: String,
}

impl Ctx {
    /// For this process: its folders and environment, and the given kernel folder.
    pub fn from_process(kernel: &Path) -> Self {
        let p = crate::paths::from_process(kernel);
        let github = std::env::var("KITH_GITHUB_GIT")
            .unwrap_or_else(|_| "https://github.com".to_owned())
            .trim_end_matches('/')
            .to_owned();
        Self {
            kernel: kernel.to_path_buf(),
            agents: p.agents,
            state: p.state,
            store: p.store,
            github,
            python: "python3".to_owned(),
        }
    }
}

/// `installed()`: `{name: package folder}`, the agents folder first, then the kernel's own `agents/`.
pub fn installed(ctx: &Ctx) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    for root in [ctx.agents.clone(), ctx.kernel.join("agents")] {
        let Ok(rd) = fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let pkg = if is_file(&d.join("role").join("agent.md")) {
                d.join("role")
            } else {
                d.clone()
            };
            if !name.starts_with('.') && is_file(&pkg.join("agent.md")) {
                out.entry(name).or_insert(pkg);
            }
        }
    }
    out
}

/// `life(agent)`: where an agent's life is (memory, data, files).
pub fn life(ctx: &Ctx, agent: &str) -> PathBuf {
    if is_dir(&ctx.agents.join(agent).join("role")) {
        ctx.agents.join(agent)
    } else {
        ctx.state.join("agents").join(agent)
    }
}

/// `agent_home(agent)`: the package folder, or the `die()` for an agent that is not installed.
pub fn agent_home(ctx: &Ctx, agent: &str) -> R<PathBuf> {
    installed(ctx)
        .remove(agent)
        .ok_or_else(|| PyErr::die(&format!("no agent '{agent}' (kith-pkg list)"), 1))
}

/// The name of a folder, as `Path.name`.
pub fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn walk(dir: &Path, top: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let (mut dirs, mut files) = (vec![], vec![]);
    for e in rd.flatten() {
        let name = e.file_name();
        if is_dir(&e.path()) {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    let name_in = |n: &std::ffi::OsString, set: &[&str]| set.iter().any(|s| n == s);
    dirs.retain(|d| !name_in(d, &SKIP) && (!top || !name_in(d, &STATE)));
    files.retain(|f| !name_in(f, &SKIP));
    dirs.sort();
    files.sort();
    out.extend(files.iter().map(|f| dir.join(f)));
    for d in dirs {
        let p = dir.join(d);
        // `os.walk` lists a link to a folder among the folders but does not go into it.
        if !fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            walk(&p, false, out);
        }
    }
}

/// `package_files(home)`: every file that is part of the package, in `os.walk` order.
pub fn package_files(home: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    walk(home, true, &mut out);
    out
}

/// `fingerprint(home)`: ten hex digits over the format, where the agent's life is, and each file's name, size and mtime.
pub fn fingerprint(ctx: &Ctx, home: &Path) -> R<String> {
    let at = if name_of(home) == "role" {
        home.parent().unwrap_or(home).to_path_buf()
    } else {
        ctx.state.clone()
    };
    let protocol = ctx.kernel.join("PROTOCOL.md");
    let files = package_files(home);
    let labelled = files
        .iter()
        .map(|p| (p.as_path(), p.strip_prefix(home).unwrap_or(p)))
        .chain(std::iter::once((
            protocol.as_path(),
            Path::new("PROTOCOL.md"),
        )));
    let mut entries = vec![];
    for (p, label) in labelled {
        let st = fs::metadata(p).map_err(|e| os_err(&e, p))?;
        let ns = i128::from(st.mtime()) * 1_000_000_000 + i128::from(st.mtime_nsec());
        entries.push((label.as_os_str().as_bytes().to_vec(), st.size(), ns));
    }
    Ok(fingerprint_of(&at.display().to_string(), &entries))
}

/// The hash itself: `(name, size, mtime in ns)` of each file, after the format and where the life is.
pub fn fingerprint_of(at: &str, entries: &[(Vec<u8>, u64, i128)]) -> String {
    let mut data = format!("format {FORMAT}\nstate {at}\n").into_bytes();
    for (label, size, ns) in entries {
        data.extend_from_slice(label);
        data.extend_from_slice(format!("\0{size}\0{ns}\n").as_bytes());
    }
    sha1::hex(&data)[..10].to_owned()
}
