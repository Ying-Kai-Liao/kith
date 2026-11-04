//! The switch state of one module: ONE small file per module, `<core folder>/cutover/<module>.switch`, plain `key=value`
//! lines. This is the single source of truth for "has this module moved to the core on this Mac". A file, not a core.db
//! table, because the readers that must see it cannot open core.db: the bash router (`kernel/lib/kith-core.sh`), the Python
//! tombstone helper, and a Mac whose core is down or whose daemon holds the lock. It is per Mac and never in git (each Mac
//! imports its own data), which is why it does not live in `kernel/lib/core-switched.txt` (that file is the verb map the
//! repo ships). A missing file means unswitched. Writes are atomic (temp file, then rename).
//!
//! ```text
//! state=switched          stepping | switched | rolling-back
//! snapshot=/path/core-cutover-x-123.db
//! at=1730000000
//! ```
//! The router refuses `KITH_CORE=python` while the state is `switched` or `rolling-back` (a half-done rollback is still
//! not the Python's to write).

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::perms;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Snapshot taken, import running or failed; nothing is tombstoned, Python is still the writer.
    Stepping,
    /// Verified and recorded; the module's sources are (or are about to be) tombstoned. The core answers.
    Switched,
    /// Export-back started and not finished; stays closed to Python until it ends.
    RollingBack,
}

impl Phase {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Stepping => "stepping",
            Self::Switched => "switched",
            Self::RollingBack => "rolling-back",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Switch {
    pub phase: Phase,
    pub snapshot: Option<String>,
    pub at: i64,
}

pub(crate) fn dir(layout: &Layout) -> PathBuf {
    layout.dir.join("cutover")
}

/// Module names are file names and shell words: lowercase letters, digits and `-`.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub(crate) fn path(layout: &Layout, module: &str) -> Result<PathBuf> {
    if !valid_name(module) {
        return Err(Error::BadParams(format!(
            "cutover: {module:?} is not a module name"
        )));
    }
    Ok(dir(layout).join(format!("{module}.switch")))
}

pub(crate) fn read(layout: &Layout, module: &str) -> Result<Option<Switch>> {
    let p = path(layout, module)?;
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(format!("cutover: {}: {e}", p.display()))),
    };
    let mut phase = None;
    let (mut snapshot, mut at) = (None, 0);
    for line in text.lines() {
        match line.split_once('=') {
            Some(("state", "stepping")) => phase = Some(Phase::Stepping),
            Some(("state", "switched")) => phase = Some(Phase::Switched),
            Some(("state", "rolling-back")) => phase = Some(Phase::RollingBack),
            Some(("snapshot", s)) if !s.is_empty() => snapshot = Some(s.to_owned()),
            Some(("at", n)) => at = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    // A file we cannot read is never "unswitched": that would hand the module back to Python.
    let phase = phase.ok_or_else(|| {
        Error::Refused(format!(
            "cutover: {} is damaged (no state= line); the module counts as switched until it is fixed",
            p.display()
        ))
    })?;
    Ok(Some(Switch {
        phase,
        snapshot,
        at,
    }))
}

pub(crate) fn write(layout: &Layout, module: &str, s: &Switch) -> Result<()> {
    let p = path(layout, module)?;
    perms::ensure_dir(&dir(layout))?;
    let mut text = format!("state={}\n", s.phase.word());
    if let Some(snap) = &s.snapshot {
        let _ = writeln!(text, "snapshot={snap}");
    }
    let _ = writeln!(text, "at={}", s.at);
    let tmp = p.with_extension("switch.tmp");
    std::fs::write(&tmp, text)
        .map_err(|e| Error::Io(format!("cutover: {}: {e}", tmp.display())))?;
    perms::make_private(&tmp)?;
    std::fs::rename(&tmp, &p).map_err(|e| Error::Io(format!("cutover: {}: {e}", p.display())))
}

pub(crate) fn clear(layout: &Layout, module: &str) -> Result<()> {
    let p = path(layout, module)?;
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(format!("cutover: {}: {e}", p.display()))),
    }
}
