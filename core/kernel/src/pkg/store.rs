//! The snapshot store (`~/.kith/store`, `KITH_STORE`): `<agent>@<version>/` folders made once, read-only, with `.pointers/`
//! naming the good and the pinned version of each agent.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::Duration;

use serde_json::{Value, json};

#[cfg(unix)]
use super::git::run;
use super::problems::{needs, read};
use super::py::{R, is_dir, mtime, now, os_err, py_int, read_text, rmtree, strip, write_text};
use super::text::frontmatter;
use super::{Ctx, KEEP, SKIP, STATE, life, name_of, package_files};
use crate::secrets::pyjson;

/// `pointers(agent)`: `(good, pin)`, the files that name an agent's good and pinned version.
pub fn pointers(ctx: &Ctx, agent: &str) -> R<(PathBuf, PathBuf)> {
    let d = ctx.store.join(".pointers");
    fs::create_dir_all(&d).map_err(|e| os_err(&e, &d))?;
    Ok((
        d.join(format!("{agent}.good")),
        d.join(format!("{agent}.pin")),
    ))
}

/// `write_atomic(p, text)`.
pub fn write_atomic(p: &Path, text: &str) -> R<()> {
    let tmp = p.with_file_name(format!(".{}.{}", name_of(p), std::process::id()));
    write_text(&tmp, &format!("{text}\n"))?;
    fs::rename(&tmp, p).map_err(|e| os_err(&e, &tmp))
}

/// The folder of one version.
pub fn version_dir(ctx: &Ctx, agent: &str, version: &str) -> PathBuf {
    ctx.store.join(format!("{agent}@{version}"))
}

/// The version a snapshot folder is: `agent@VERSION` after the first `@`.
pub fn version_of(d: &Path) -> String {
    let n = name_of(d);
    n.split_once('@').map_or(n.clone(), |(_, v)| v.to_owned())
}

/// `made(d)`: when a version was made. Not the folder's mtime: that moves whenever a run is recorded in it.
pub fn made(d: &Path) -> f64 {
    let from_meta = || -> Option<f64> {
        let t = read_text(&d.join(".meta.json")).ok()?;
        let v: Value = serde_json::from_str(&t).ok()?;
        v.get("made")?.as_f64()
    };
    from_meta().unwrap_or_else(|| fs::metadata(d).map_or(0.0, |m| mtime(&m)))
}

/// `all_versions(agent)`: the snapshot folders of an agent, newest first.
pub fn all_versions(ctx: &Ctx, agent: &str) -> Vec<PathBuf> {
    let prefix = format!("{agent}@");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&ctx.store)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| name_of(p).starts_with(&prefix) && is_dir(p))
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    let mut keyed: Vec<(f64, PathBuf)> = dirs.into_iter().map(|d| (made(&d), d)).collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    keyed.into_iter().map(|(_, d)| d).collect()
}

/// `os.kill(int(pid), 0)`: a pid of zero or below names a process group; EPERM still means alive.
#[cfg(unix)]
fn pid_alive(pid: i64) -> bool {
    crate::threads::os::pid_probe(pid)
}

/// Whether the system has a process with this pid. There are no process groups to name, so a pid of zero or below is no one's;
/// a process that cannot be asked about counts as alive, as an `EPERM` does, so its snapshot is kept.
#[cfg(not(unix))]
fn pid_alive(pid: i64) -> bool {
    use crate::procstart::{Probe, probe};
    u32::try_from(pid).is_ok_and(|p| p > 0 && !matches!(probe(p), Probe::Gone))
}

/// `running(pkg)`: true while a run started from this snapshot is alive; clears records of finished runs.
pub fn running(pkg: &Path) -> bool {
    let runs = pkg.join(".runs");
    let Ok(rd) = fs::read_dir(&runs) else {
        return false;
    };
    let mut alive = false;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name.split('.').next().unwrap_or("");
        match py_int(pid).map(pid_alive) {
            Some(true) => alive = true,
            _ => {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    alive
}

/// `gc(agent)`: removes versions nothing needs: not good, not pinned, not among the newest, not recent, not running.
pub fn gc(ctx: &Ctx, agent: &str) -> R<()> {
    let (good, pin) = pointers(ctx, agent)?;
    let keep: HashSet<String> = [read(&good)?, read(&pin)?].into_iter().collect();
    for d in all_versions(ctx, agent).into_iter().skip(KEEP) {
        // Subagent sessions don't record runs, so anything made in the last two days stays too.
        let recent = now() - made(&d) < 2.0 * 86400.0;
        if !keep.contains(&version_of(&d)) && !recent && !running(&d) {
            rmtree(&d);
        }
    }
    Ok(())
}

/// Hard-links `target` to the previous snapshot's copy when the file didn't change (same size and mtime to the second, as
/// rsync decides).
fn link_same(src: &Path, old: &Path, target: &Path) -> bool {
    let go = || -> std::io::Result<bool> {
        let (a, b) = (fs::metadata(src)?, fs::metadata(old)?);
        let (sa, sb) = (
            a.modified()?.duration_since(std::time::UNIX_EPOCH),
            b.modified()?.duration_since(std::time::UNIX_EPOCH),
        );
        let same =
            a.len() == b.len() && matches!((sa, sb), (Ok(x), Ok(y)) if x.as_secs() == y.as_secs());
        if !same {
            return Ok(false);
        }
        fs::hard_link(old, target)?;
        Ok(true)
    };
    go().unwrap_or(false)
}

/// `shutil.copy2`: the content, the permission bits and the times.
fn copy2(src: &Path, dst: &Path) -> R<()> {
    fs::copy(src, dst).map_err(|e| os_err(&e, src))?;
    if let Ok(t) = fs::metadata(src).and_then(|m| m.modified())
        && let Ok(f) = fs::File::options().write(true).open(dst)
    {
        let _ = f.set_modified(t);
    }
    Ok(())
}

/// Removes what the fingerprint ignores from a fresh copy: `SKIP` folders and files at any depth. (`os.walk` lists a link to
/// a folder among the folders and does not follow it; `rmtree` refuses a link.)
fn drop_skipped(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let skip = SKIP.iter().any(|s| e.file_name() == *s);
        let link = fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        if is_dir(&p) {
            if skip {
                if !link {
                    let _ = fs::remove_dir_all(&p);
                }
            } else if !link {
                drop_skipped(&p);
            }
        } else if skip {
            let _ = fs::remove_file(&p);
        }
    }
}

/// `os.chmod` of every file to read-only (executables keep their execute bit, so `tools/<x>` still runs).
pub(super) fn lock_files(dir: &Path) -> R<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let p = e.path();
        if is_dir(&p) {
            if !fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                lock_files(&p)?;
            }
        } else {
            let mut perms = fs::metadata(&p).map_err(|e| os_err(&e, &p))?.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let to = if perms.mode() & 0o111 != 0 {
                    0o555
                } else {
                    0o444
                };
                perms.set_mode(to);
            }
            // Windows has no execute bit: the read-only flag is all there is to lock
            #[cfg(not(unix))]
            perms.set_readonly(true);
            fs::set_permissions(&p, perms).map_err(|e| os_err(&e, &p))?;
        }
    }
    Ok(())
}

/// `cp -c -R -p ENTRIES... DEST` on a Mac: clones, so a snapshot is instant and never changes when the working copy does. Where
/// `cp` has no such flag (Linux) it fails and the caller copies file by file.
#[cfg(unix)]
fn clone_entries(entries: &[PathBuf], dest: &Path) -> bool {
    let mut cp = Command::new("cp");
    cp.args(["-c", "-R", "-p"]).args(entries).arg(dest);
    run(cp, Duration::from_secs(3600)).is_ok_and(|r| r.code == 0)
}

/// Windows has no `cp` (and no clones): the same recursive copy in Rust, with the times kept.
#[cfg(not(unix))]
fn clone_entries(entries: &[PathBuf], dest: &Path) -> bool {
    entries.iter().all(|e| {
        let to = dest.join(e.file_name().unwrap_or_default());
        copy_all(e, &to).is_ok()
    })
}

/// `cp -R -p`: a folder is made and filled, a file copied with its time. A link fails the copy, as it cannot be recreated here
/// (Windows makes links only with a privilege most accounts do not have): the caller then copies file by file, as it does
/// where `cp` cannot clone.
#[cfg_attr(unix, allow(dead_code))]
pub(super) fn copy_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "a link inside a package ({})",
            src.display()
        )));
    }
    if meta.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_all(&e.path(), &dst.join(e.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(src, dst)?;
    if let Ok(t) = meta.modified()
        && let Ok(f) = fs::File::options().write(true).open(dst)
    {
        let _ = f.set_modified(t);
    }
    Ok(())
}

/// `snapshot(agent, home, version)`: the working copy as a read-only version in the store.
pub fn snapshot(ctx: &Ctx, agent: &str, home: &Path, version: &str) -> R<PathBuf> {
    let dest = version_dir(ctx, agent, version);
    if super::py::exists(&dest) {
        return Ok(dest);
    }
    fs::create_dir_all(&ctx.store).map_err(|e| os_err(&e, &ctx.store))?;
    let tmp = ctx
        .store
        .join(format!(".tmp-{agent}-{version}-{}", std::process::id()));
    fs::create_dir(&tmp).map_err(|e| os_err(&e, &tmp))?;
    // APFS clones, one cp per top-level entry: instant and copy-on-write, so a snapshot never changes when the working copy
    // does. Then drop what the fingerprint ignores.
    let mut entries: Vec<PathBuf> = fs::read_dir(home)
        .map_err(|e| os_err(&e, home))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = name_of(p);
            !STATE.contains(&n.as_str()) && !SKIP.contains(&n.as_str())
        })
        .collect();
    entries.sort();
    let cloned = clone_entries(&entries, &tmp);
    if !cloned {
        // another disk: no clones, so share what didn't change instead
        rmtree(&tmp);
        fs::create_dir(&tmp).map_err(|e| os_err(&e, &tmp))?;
        let prev = all_versions(ctx, agent).into_iter().next();
        for p in package_files(home) {
            let rel = p.strip_prefix(home).unwrap_or(&p);
            let target = tmp.join(rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| os_err(&e, parent))?;
            }
            if !prev
                .as_ref()
                .is_some_and(|d| link_same(&p, &d.join(rel), &target))
            {
                copy2(&p, &target)?;
            }
        }
    }
    drop_skipped(&tmp);
    let body = frontmatter(&read_text(&home.join("agent.md"))?).body;
    let protocol = read_text(&ctx.kernel.join("PROTOCOL.md"))?;
    for name in ["prompt.md", ".home", ".meta.json"] {
        // written below: never through a link into an older snapshot
        match fs::remove_file(tmp.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(os_err(&e, &tmp.join(name))),
        }
    }
    write_text(
        &tmp.join("prompt.md"),
        &format!("{}\n\n{protocol}", strip(&body)),
    )?;
    if !super::py::exists(&tmp.join("settings.json")) {
        write_text(&tmp.join("settings.json"), "{}\n")?;
    }
    write_text(
        &tmp.join(".home"),
        &format!("{}\n", life(ctx, agent).display()),
    )?;
    let meta = json!({
        "agent": agent,
        "version": version,
        "made": now(),
        "needs": needs(home)?,
    });
    write_text(&tmp.join(".meta.json"), &pyjson::dumps(&meta))?;
    lock_files(&tmp)?;
    if fs::rename(&tmp, &dest).is_err() {
        // another run made the same version first
        rmtree(&tmp);
    }
    Ok(dest)
}
