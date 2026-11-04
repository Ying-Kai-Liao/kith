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
    /// Who holds the lock (pid, start time). Apart from `lock` because on Windows a held lock covers the whole file.
    pub holder: PathBuf,
    pub marker: PathBuf,
    pub snapshots: PathBuf,
    /// The local owner token: what a CLI shows the daemon to be the owner (with the same OS user). Rewritten at each
    /// daemon start.
    pub token: PathBuf,
    /// The host secret: what `kith serve`'s own client shows the daemon to be the host (the relay owner), apart from the
    /// owner token so that a terminal or an owner agent holding only the owner token cannot speak as the host. Rewritten
    /// at each daemon start; never in any run's environment (it sits in the core folder the guest deny list covers).
    pub host_token: PathBuf,
    /// The key the holder was started with; it never leaves this folder (the holder reads it from a pipe).
    pub holder_secret: PathBuf,
    /// The holder's own folder (socket, lock, log), inside the core folder so the same lists cover it.
    pub holder_dir: PathBuf,
}

/// The name of the folder below the machine folder. `kernel/lib/kith_guard.py` (`SECRET_DIRS`) and its Rust ports name the same.
pub(crate) const DIR: &str = "core";

impl Layout {
    pub(crate) fn new(machine: &Path) -> Self {
        let dir = machine.join(DIR);
        Self {
            db: dir.join("core.db"),
            lock: dir.join("core.lock"),
            holder: dir.join("core.holder"),
            marker: dir.join("core.marker"),
            snapshots: dir.join("snapshots"),
            token: dir.join("owner.token"),
            host_token: dir.join("host.token"),
            holder_secret: dir.join("holder.secret"),
            holder_dir: dir.join("holder"),
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
            self.holder.clone(),
            self.marker.clone(),
            self.token.clone(),
            self.host_token.clone(),
            self.holder_secret.clone(),
        ]
    }
}
