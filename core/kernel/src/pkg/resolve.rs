//! `resolve(agent)`: the package folder a run starts from, and a note when it is not the latest edits.

use std::fs;
use std::path::PathBuf;

use super::fetch::fetch;
use super::git::commit;
use super::problems::{problems, read};
use super::py::{Kind, R, exists, is_dir, mtime, now, write_text};
use super::store::{gc, pointers, running, snapshot, version_dir, write_atomic};
use super::{Ctx, EDITING_MAX_AGE, agent_home, fingerprint};

/// `(package folder, note)`. The note is set when the run doesn't use the latest edits.
pub type Resolved = (PathBuf, String);

/// `err` collects what the Python prints to stderr on the way (a fetch that failed).
pub fn resolve(ctx: &Ctx, agent: &str, err: &mut String) -> R<Resolved> {
    let home = agent_home(ctx, agent)?;
    let (good, pin) = pointers(ctx, agent)?;
    let pinned = read(&pin)?;
    if !pinned.is_empty() && is_dir(&version_dir(ctx, agent, &pinned)) {
        return Ok((
            version_dir(ctx, agent, &pinned),
            format!("pinned to {pinned} (kith unpin {agent} to follow edits)"),
        ));
    }
    let last = read(&good)?;
    let editing = home.join(".editing");
    if exists(&editing)
        && now() - fs::metadata(&editing).map_or(0.0, |m| mtime(&m)) < EDITING_MAX_AGE
        && !last.is_empty()
        && is_dir(&version_dir(ctx, agent, &last))
    {
        return Ok((
            version_dir(ctx, agent, &last),
            format!("being edited (.editing), running {last}"),
        ));
    }
    // first run after install, or a changed pin: download what skills.json links
    if let Err(e) = fetch(ctx, &home, false) {
        if matches!(
            e.kind,
            Kind::Runtime | Kind::Value | Kind::Os | Kind::Timeout
        ) {
            // the checks below then fail and the last good version runs
            err.push_str(&format!("kith: {agent}: {e}\n"));
        } else {
            return Err(e);
        }
    }
    let version = fingerprint(ctx, &home)?;
    let dir = version_dir(ctx, agent, &version);
    if is_dir(&dir) {
        running(&dir); // clears records of finished runs
        if exists(&dir.join(".unchecked")) {
            return Ok((
                dir,
                "no good version yet, running unchecked edits".to_owned(),
            ));
        }
        write_atomic(&good, &version)?;
        gc(ctx, agent)?;
        return Ok((dir, String::new()));
    }
    let bad = problems(ctx, &home)?;
    if !bad.is_empty() {
        let detail = bad.join("; ");
        if !last.is_empty() && is_dir(&version_dir(ctx, agent, &last)) {
            return Ok((
                version_dir(ctx, agent, &last),
                format!("latest edits fail checks, running {last}: {detail}"),
            ));
        }
        let dest = snapshot(ctx, agent, &home, &version)?;
        write_text(&dest.join(".unchecked"), &format!("{detail}\n"))?;
        return Ok((
            dest,
            format!("no good version yet, running unchecked edits: {detail}"),
        ));
    }
    let dest = snapshot(ctx, agent, &home, &version)?;
    write_atomic(&good, &version)?;
    gc(ctx, agent)?;
    // every good version is in the agent's history, to restore later
    commit(ctx, &home, &format!("{agent} {version}"));
    Ok((dest, String::new()))
}
