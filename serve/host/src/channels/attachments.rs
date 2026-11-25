//! Downloaded attachments are kept for [`KEEP`] and then deleted: the folder would otherwise grow forever.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// How long a downloaded file stays (a reversible choice: the agent has had a month to use it).
const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);
const EVERY: Duration = Duration::from_secs(24 * 3600);

/// Prunes now (at host start) and then once a day.
pub async fn prune_daily(root: std::path::PathBuf) {
    let mut tick = tokio::time::interval(EVERY);
    loop {
        tick.tick().await; // the first one is immediate
        let dir = root.clone();
        if let Ok(n) = tokio::task::spawn_blocking(move || prune(&dir, SystemTime::now(), KEEP)).await
            && n > 0
        {
            tracing::info!("channels: pruned {n} old attachments");
        }
    }
}

/// Deletes the files under `root` last modified more than `keep` before `now`, and the folders that empties.
/// Symlinks are neither followed nor removed. Returns how many files went.
pub fn prune(root: &Path, now: SystemTime, keep: Duration) -> usize {
    fn sweep(dir: &Path, now: SystemTime, keep: Duration, top: bool) -> usize {
        let mut gone = 0;
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let path = e.path();
            if kind.is_dir() {
                gone += sweep(&path, now, keep, false);
            } else if kind.is_file()
                && e.metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > keep))
                && std::fs::remove_file(&path).is_ok()
            {
                gone += 1;
            }
            // symlinks are neither dir nor file here (file_type does not follow them): left alone
        }
        if !top {
            let _ = std::fs::remove_dir(dir); // only succeeds when empty
        }
        gone
    }
    sweep(root, now, keep, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn old_files_go_new_ones_stay_and_links_are_left_alone() {
        let dir = std::env::temp_dir().join(format!("kith-prune-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("kith-prune-out-{}", uuid::Uuid::new_v4()));
        let (old, new) = (dir.join("tg/1/2/old.txt"), dir.join("tg/1/3/new.txt"));
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&old, "o").unwrap();
        std::fs::write(&new, "n").unwrap();
        std::fs::write(outside.join("keep.txt"), "k").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("tg/link")).unwrap();
        // A clock 31 days ahead makes every file old, except the one stamped just before it.
        let later = SystemTime::now() + Duration::from_secs(31 * 24 * 3600);
        let fresh = std::fs::File::options().write(true).open(&new).unwrap();
        fresh.set_modified(later - Duration::from_secs(60)).unwrap();
        assert_eq!(prune(&dir, later, KEEP), 1);
        assert!(!old.exists() && !old.parent().unwrap().exists(), "old file and its empty folder are gone");
        assert!(new.exists());
        assert!(outside.join("keep.txt").exists(), "nothing outside the folder is touched");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
