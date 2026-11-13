//! An extra Claude account's folder made to behave like the main login: `prepare_claude_dir` of
//! `kernel/lib/kith_accounts.py`. With `CLAUDE_CONFIG_DIR` set Claude Code reads nothing of `~/.claude`, so a backup
//! otherwise ran with no settings, no kith plugin and auto mode never entered. Never touches the account's login, machine
//! id, projects or history. Runs on every launch: it writes only what differs, atomically, and any failure is left for the
//! next launch.

use super::state::write_atomic;
use super::{Ctx, Map, pyjson};
use crate::paths::realpath;
use fs2::FileExt as _;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The acknowledgements a login makes once (onboarding, the auto-mode warnings): without them auto mode is not entered
/// silently, and Claude Code asks instead. Copied from the main login into an account's `.claude.json`, nothing else of it.
const MIRRORED_FLAGS: [&str; 6] = [
    "hasCompletedOnboarding",
    "lastOnboardingVersion",
    "hasSeenAutoModeEntryWarning",
    "hasSeenAutoModeOutsideReadPrompt",
    "hasSeenAutoDefaultNotice",
    "autoModeEnvSetup",
];
/// Plugin state: the JSON files are copied (Claude Code rewrites them in place, so a link would not survive), the folders
/// are linked (their paths in `installed_plugins.json` are absolute, into the main login's).
const PLUGIN_FILES: [&str; 3] = [
    "installed_plugins.json",
    "known_marketplaces.json",
    "blocklist.json",
];
const PLUGIN_DIRS: [&str; 3] = ["marketplaces", "cache", "data"];

fn read_json(path: &Path) -> Option<Map> {
    match serde_json::from_slice::<Value>(&std::fs::read(path).ok()?) {
        Ok(Value::Object(d)) => Some(d),
        _ => None,
    }
}

/// Copy `src` to `dst` (atomically) when the bytes differ; whether it wrote.
fn copy_if_changed(src: &Path, dst: &Path) -> std::io::Result<bool> {
    let data = std::fs::read(src)?;
    if std::fs::read(dst).is_ok_and(|have| have == data) {
        return Ok(false);
    }
    write_atomic(dst, &data, 0o600)?;
    Ok(true)
}

/// `(config dir, .claude.json)` of the main Claude login. `CLAUDE_CONFIG_DIR` is the main login's only when it is not one of
/// the accounts' own folders (a run on an account has it set, and must not take itself for the main login).
pub fn main_claude_paths(ctx: &Ctx<'_>) -> (PathBuf, PathBuf) {
    if let Some(env) = ctx.claude_config_dir.as_deref().filter(|e| !e.is_empty()) {
        let env = PathBuf::from(env);
        let accounts = ctx.machine.join("accounts").join("claude");
        if env.parent().map(realpath) != Some(realpath(&accounts)) {
            let json = env.join(".claude.json");
            return (env, json);
        }
    }
    (ctx.home.join(".claude"), ctx.home.join(".claude.json"))
}

/// Python's `[Errno N] text`, close enough for a warning.
fn errno_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let msg = e.to_string();
            let msg = msg.split(" (os error").next().unwrap_or(&msg);
            format!("[Errno {n}] {msg}")
        }
        None => e.to_string(),
    }
}

#[cfg(unix)]
fn chmod_dir(d: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // same shape as the unix one
fn chmod_dir(_: &Path) -> std::io::Result<()> {
    Ok(())
}

fn mkdir_private(d: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(d) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && d.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

fn sync(
    directory: &Path,
    main_dir: &Path,
    main_json: &Path,
    changed: &mut Vec<String>,
) -> std::io::Result<()> {
    if !directory.is_dir() || realpath(directory) == realpath(main_dir) {
        return Ok(());
    }
    chmod_dir(directory)?;
    // settings.json: a copy, taken again whenever the main one changed. A missing or broken main file changes nothing.
    let src = main_dir.join("settings.json");
    if read_json(&src).is_some() && copy_if_changed(&src, &directory.join("settings.json"))? {
        changed.push("settings.json".to_owned());
    }
    // plugins
    let (plugins_src, plugins_dst) = (main_dir.join("plugins"), directory.join("plugins"));
    if plugins_src.is_dir() {
        mkdir_private(&plugins_dst)?;
        for name in PLUGIN_FILES {
            let from = plugins_src.join(name);
            if from.is_file()
                && read_json(&from).is_some()
                && copy_if_changed(&from, &plugins_dst.join(name))?
            {
                changed.push(format!("plugins/{name}"));
            }
        }
        for name in PLUGIN_DIRS {
            let link = plugins_dst.join(name);
            if plugins_src.join(name).is_dir() && link.symlink_metadata().is_err() {
                symlink(&plugins_src.join(name), &link)?;
                changed.push(format!("plugins/{name}"));
            }
        }
    }
    // .claude.json: only the acknowledgement flags, into the file Claude Code keeps rewriting
    let cfg_path = directory.join(".claude.json");
    if let Some(main_cfg) = read_json(main_json) {
        let want: Vec<(&str, &Value)> = MIRRORED_FLAGS
            .iter()
            .filter_map(|k| main_cfg.get(*k).map(|v| (*k, v)))
            .collect();
        let current = || {
            if cfg_path.exists() {
                read_json(&cfg_path)
            } else {
                Some(Map::new())
            }
        };
        if current().is_some_and(|cfg| want.iter().any(|(k, v)| cfg.get(*k) != Some(*v))) {
            // Two launches on the same account mirror at once: one at a time (`.claude.json.lock`, flock; kith_accounts
            // takes the same). Made only when a write is due, so a launch with nothing to mirror leaves the folder as it is.
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .truncate(false)
                .create(true)
                .open(directory.join(".claude.json.lock"))?;
            lock.lock_exclusive()?;
            // Read again under the lock, just before the swap: Claude Code may have written in between.
            if let Some(mut cfg) = current() {
                for (k, v) in want {
                    cfg.insert(k.to_owned(), v.clone());
                }
                write_atomic(
                    &cfg_path,
                    pyjson::dumps_pretty(&Value::Object(cfg)).as_bytes(),
                    0o600,
                )?;
                changed.push(".claude.json".to_owned());
            }
        }
    }
    Ok(())
}

/// Makes an extra Claude account's folder behave like the main login: its settings (hooks, plugins on, model), the plugins
/// themselves, and the auto-mode/onboarding acknowledgements. The names of what it changed.
pub fn prepare_claude_dir(ctx: &Ctx<'_>, directory: &Path) -> Vec<String> {
    let (main_dir, main_json) = main_claude_paths(ctx);
    let mut changed = vec![];
    if let Err(e) = sync(directory, &main_dir, &main_json, &mut changed) {
        eprintln!(
            "kith: could not sync {} with the main Claude login: {}",
            directory.display(),
            errno_text(&e)
        );
    }
    changed
}
