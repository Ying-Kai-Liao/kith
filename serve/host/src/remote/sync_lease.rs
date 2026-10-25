//! Whether this Mac is in sync with the Kith cloud, for this host and for the kernel. In the host it is
//! `hub.cloud_synced`: account devices and bot asks wait for it. For the kernel it is `<data dir>/cloud-sync.json`
//! `{linked, until}`, which `kith-auth who` reads to tell the bridge whom to hold: with `linked` set, the bridge serves
//! anyone but the owner only while `until` (ms) is ahead (revoke-person R8). The lease runs while the relay
//! connection is live and a pull of the cloud state succeeded on it; it ends at once when the cloud says something
//! changed (until the pull that applies it), when the connection drops, and by itself `LEASE` after the relay was
//! last heard, so a Mac that slept or lost the network holds guests until it has pulled again.
//! `linked: false` (the cloud is off) means nothing waits on the cloud, and kith-auth revokes on its own.

use crate::hub::Hub;
use crate::store::now_ms;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How long one pull, or one ping heard while synced, keeps the bridge serving: past the relay's silence limit plus
/// one ping, so a live connection renews it in time and a dead one lets it run out.
pub const LEASE: Duration = Duration::from_secs(120);

pub fn path() -> PathBuf {
    crate::service::data_dir().join("cloud-sync.json")
}

/// In sync (a pull on the live connection succeeded, or a ping was heard since) or not.
pub fn set(hub: &Hub, synced: bool) {
    hub.cloud_synced.store(synced, Ordering::Release);
    let linked = super::cloud::url(&hub.store).is_some();
    let until = if synced && linked { now_ms() + i64::try_from(LEASE.as_millis()).unwrap_or(i64::MAX) } else { 0 };
    write(&path(), linked, until);
}

/// The cloud said its state changed: out of sync until a pull that starts after this one succeeds.
pub fn changed(hub: &Hub) {
    hub.cloud_changes.fetch_add(1, Ordering::AcqRel);
    set(hub, false);
}

/// Where the cloud's changes stand; taken before a pull reads the state.
pub fn mark(hub: &Hub) -> u64 {
    hub.cloud_changes.load(Ordering::Acquire)
}

/// A pull that started at `mark` succeeded: in sync, unless the cloud changed again since it read the state.
pub fn pulled(hub: &Hub, mark: u64) {
    if hub.cloud_changes.load(Ordering::Acquire) == mark {
        set(hub, true);
    }
}

/// The cloud is off: account devices can't be honored, and nothing waits on it.
pub fn unlinked(hub: &Hub) {
    hub.cloud_synced.store(false, Ordering::Release);
    write(&path(), false, 0);
}

/// Renews the lease while synced (the relay was just heard).
pub fn renew(hub: &Hub) {
    if hub.cloud_synced.load(Ordering::Acquire) {
        set(hub, true);
    }
}

/// Waits up to `max` for this host to be in sync (true), for a door that must not start a turn on stale access: a bot
/// ask right after the cloud recorded a revoke runs only once the pull applied it (R7). With the cloud off there is
/// nothing to wait for (and no relay that could carry a bot).
pub async fn wait(hub: &Hub, max: Duration) -> bool {
    if super::cloud::url(&hub.store).is_none() {
        return true;
    }
    let end = tokio::time::Instant::now() + max;
    loop {
        if hub.cloud_synced.load(Ordering::Acquire) {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn write(path: &Path, linked: bool, until: i64) {
    let body = json!({"linked": linked, "until": until}).to_string();
    let tmp = path.with_extension("json.tmp");
    let done = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&tmp, body))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = done {
        // The bridge then reads the last lease, which runs out by itself: it holds guests rather than serve them.
        tracing::warn!(error = %e, "couldn't write the cloud sync lease");
    }
}

/// The lease as written, for tests.
#[cfg(test)]
pub fn read(path: &Path) -> Option<(bool, i64)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((v["linked"].as_bool()?, v["until"].as_i64()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces_the_lease() {
        let dir = std::env::temp_dir().join(format!("kith-lease-{}", std::process::id()));
        let p = dir.join("cloud-sync.json");
        write(&p, true, 42);
        assert_eq!(read(&p), Some((true, 42)));
        write(&p, false, 0);
        assert_eq!(read(&p), Some((false, 0)));
        assert!(!p.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
