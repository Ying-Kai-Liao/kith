//! Place or adopt (runtime-design section 4): before a start the core puts its own copy of a transcript into the
//! engine's folder only when the engine's copy is missing or unchanged since the core last read it; otherwise the
//! engine's copy is newer than anything the core knows and the core adopts it (copies it into its own store).
//!
//! [`decide`] is the rule, pure, on three hashes. [`step`] is the filesystem half: it hashes, decides and copies. The
//! core's copy has the engine's own layout (`projects/<project>/<sid>.jsonl` and `<sid>/`) under a folder of its own, so
//! one [`SessionPtr`] type names both. Nothing calls [`step`] in production yet; a start will, after the switch.

use std::path::{Path, PathBuf};

use super::SessionPtr;
use super::hash::{Digest, hash_session, session_files};

/// What the rule says to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Neither side has a copy.
    Nothing,
    /// Both sides hold the same bytes.
    InSync,
    /// Put the core's copy into the engine's folder.
    Place,
    /// Take the engine's copy as the truth.
    Adopt,
}

/// `engine`: the hash of the engine's copy now (`None`: missing). `last_read`: the engine's hash the core recorded when
/// it last read it. `ours`: the hash of the core's own copy (`None`: it holds none).
///
/// The core's copy goes over the engine's only when the engine's is missing, or is exactly what the core last saw (so
/// nothing was written there since, and the core's copy is the later one). A hash the core never recorded, or a different
/// one, means something wrote to the engine's copy meanwhile (a transcript still being written, a terminal session): adopt.
pub(crate) fn decide(engine: Option<&str>, last_read: Option<&str>, ours: Option<&str>) -> Choice {
    match (engine, ours) {
        (None, None) => Choice::Nothing,
        (None, Some(_)) => Choice::Place,
        (Some(e), Some(o)) if e == o => Choice::InSync,
        (Some(e), Some(_)) if last_read == Some(e) => Choice::Place,
        (Some(_), _) => Choice::Adopt,
    }
}

/// What a [`step`] did, and the engine's copy as it is afterwards (what the table records as the last read).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub choice: Choice,
    /// `None`: the engine has no copy after the step.
    pub engine: Option<Digest>,
}

/// Hashes both copies, applies [`decide`] and does the copy. `last_read` is the hash the table holds.
/// An unreadable copy is an error before anything is copied.
///
/// The caller must hold the conversation's writer claim (M3-2) for the whole call: that is what keeps a core-started
/// engine from writing the folder meanwhile. A writer that does not take the claim (a terminal session) is covered only
/// as far as [`apply`] re-checking the engine's copy just before it overwrites it.
pub(crate) fn step(
    ours: &SessionPtr,
    engine: &SessionPtr,
    last_read: Option<&str>,
) -> std::io::Result<Outcome> {
    let theirs = hash_session(engine)?;
    let mine = hash_session(ours)?;
    let choice = decide(
        theirs.as_ref().map(|d| d.hash.as_str()),
        last_read,
        mine.as_ref().map(|d| d.hash.as_str()),
    );
    apply(
        choice,
        ours,
        engine,
        theirs.as_ref().map(|d| d.hash.as_str()),
    )
}

/// Does what [`decide`] said, given the hash of the engine's copy it was decided on (`seen`). A [`Choice::Place`] hashes
/// the engine's copy again first: if it is no longer `seen`, something wrote it since the decision and the core's copy
/// must not overwrite it, so the engine's copy is adopted instead. After an adopt the copy is hashed again and must be
/// the engine's copy as it is now; the hash returned is the one to record as the last read.
pub(crate) fn apply(
    choice: Choice,
    ours: &SessionPtr,
    engine: &SessionPtr,
    seen: Option<&str>,
) -> std::io::Result<Outcome> {
    let mut choice = choice;
    if choice == Choice::Place {
        let now = hash_session(engine)?;
        // Gone since the decision is still safe to place over; anything else there is newer than what was decided on.
        if now.is_some() && now.as_ref().map(|d| d.hash.as_str()) != seen {
            choice = Choice::Adopt;
        }
    }
    match choice {
        Choice::Place => {
            copy_session(ours, engine)?;
            Ok(Outcome {
                choice,
                engine: hash_session(engine)?,
            })
        }
        Choice::Adopt => {
            copy_session(engine, ours)?;
            let copied = hash_session(ours)?;
            let now = hash_session(engine)?;
            if copied != now {
                return Err(std::io::Error::other(super::hash::CHANGED));
            }
            Ok(Outcome {
                choice,
                engine: now,
            })
        }
        Choice::Nothing | Choice::InSync => Ok(Outcome {
            choice,
            engine: hash_session(engine)?,
        }),
    }
}

/// Makes `dst` hold exactly `src`'s files: each file is written whole beside its place and renamed over it, and sidecar
/// files `src` does not have are removed. The main file is written last: the sidecar goes first, so a reader that
/// finds the new `.jsonl` finds its sidecar too.
fn copy_session(src: &SessionPtr, dst: &SessionPtr) -> std::io::Result<()> {
    let files = session_files(src)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no transcript to copy at {}", src.jsonl().display()),
        )
    })?;
    let prefix = format!("{}/", src.sid);
    let side_root = dst.sidecar();
    let mut keep: Vec<PathBuf> = Vec::new();
    for (name, from) in files.iter().skip(1) {
        let rel = name.strip_prefix(&prefix).unwrap_or(name);
        let to = rel.split('/').fold(side_root.clone(), |p, c| p.join(c));
        copy_file(from, &to)?;
        keep.push(to);
    }
    prune(&side_root, &keep)?;
    copy_file(&files[0].1, &dst.jsonl())
}

fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(super::hash::TMP);
    let tmp = PathBuf::from(tmp);
    std::fs::copy(from, &tmp)?;
    std::fs::rename(&tmp, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Removes the regular files under `dir` that are not in `keep`.
fn prune(dir: &Path, keep: &[PathBuf]) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            prune(&entry.path(), keep)?;
        } else if kind.is_file() && !keep.contains(&entry.path()) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
