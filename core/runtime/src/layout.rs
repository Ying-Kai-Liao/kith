//! Where kith-core's files are: the one place that knows core.db's path (the boundary check fails when the name
//! `core.db` or this function shows up in any other crate). Everything sits in one folder, `core/`, of the machine
//! folder, so a guest sandbox and the guard lists can name it as a whole. The machine folder is on this computer's own
//! disk, and never one of the folders `~/.kith` tracks in git (projects/ ...).

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub dir: PathBuf,
    pub db: PathBuf,
    pub lock: PathBuf,
    pub marker: PathBuf,
    pub snapshots: PathBuf,
}

/// The name of the folder below the machine folder. `kernel/lib/kith_guard.py` (`SECRET_DIRS`) and its Rust ports name the same.
pub(crate) const DIR: &str = "core";

impl Layout {
    pub(crate) fn new(machine: &Path) -> Self {
        let dir = machine.join(DIR);
        Self {
            db: dir.join("core.db"),
            lock: dir.join("core.lock"),
            marker: dir.join("core.marker"),
            snapshots: dir.join("snapshots"),
            dir,
        }
    }

    /// The files the permission rule covers, whether they exist or not (SQLite adds the -wal and -shm files).
    pub(crate) fn files(&self) -> Vec<PathBuf> {
        let db = self.db.to_string_lossy();
        vec![
            self.db.clone(),
            PathBuf::from(format!("{db}-wal")),
            PathBuf::from(format!("{db}-shm")),
            self.lock.clone(),
            self.marker.clone(),
        ]
    }
}
