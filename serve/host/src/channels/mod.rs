//! Chat channels: the routing core of hq's chat bridge (`kernel/lib/hq_bridge/core.py`) inside the host, with no
//! platform of its own. Discord, Telegram and Slack plug in as [`Adapter`]s; each is cut over from the Python
//! bridge on its own (grant its profile to the client `serve` in hq-auth, revoke it from `bridge`).
//!
//! What it does for a message, whatever the platform: finds the person (`hq-auth who`, `/link CODE` to link an
//! account), picks the agent (`@agent`, the thread's last agent, the room's default, or asks "who should take
//! this?"), keeps a thread per task with a session per agent, queues messages sent while the agent works (merging
//! them, and with Jev stopping, restarting or splitting the work), and runs each turn as a subprocess
//! `hq <agent> <task>` under the resource gate. Agents' own posts (`hq-send`) reach the thread through the run's
//! outbox folder (`HQ_BRIDGE_OUTBOX`).
//!
//! Cutting a bot over from the bridge: first `hq-auth revoke bridge <platform>` and restart the bridge
//! (`launchctl kickstart -k gui/$UID/dev.hq.bridge`; it reads its grants only when it starts), and only then
//! `hq-auth grant serve <platform> <profile>`: this host copies the bridge's state (last message seen, threads)
//! when the connection first starts, which must be after the bridge stopped handling messages, or what the
//! bridge already answered is run again. The router needs the Jev
//! key too: `hq-auth grant serve jev-browser <profile>`, or every message sent while an agent works is merged.
//!
//! State: `<data dir>/channels/state.json` (the bridge's v2 schema), run outboxes and attachments next to it. The
//! first time a connection starts here its entries are copied from the bridge's `~/.hq/bridge/state.json`, which
//! is only read.
//!
//! A host whose `KITH_SERVE_HOME` is not the default data folder runs no channels (hq-auth and `~/.hq` are global, so a
//! test host would take over the real bots): it asks neither hq-auth nor reads the bridge's state. `KITH_SERVE_CHANNELS=1`
//! opts a non-default home in; `/health` shows `channels.off` with the reason.
//!
//! Settings: the `channels` object in `<data dir>/config.json`, read at start:
//! `enabled` (default true), `dm_default` (the agent the owner's new DMs go to, default `personal`),
//! `debounce_s` (seconds to wait for more messages before a run starts, default 2), `router` (`jev` or `off`,
//! default `jev` when a Jev key is found), `interrupt` (false: corrections wait for the next turn instead of
//! restarting the run, default true), `context_show` (a reply ends with a "context N%" line once the session is at
//! least this full, default 30, `"off"` never; the apps get it as `contextPercent` on a turn's last agent entry), `permission_mode` (the owner's runs; unset leaves it to the agent's
//! settings and hq's default), `discord_guild` (server name or id for the Discord adapter).
//!
//! Connections: for each of discord, telegram and slack, the profiles `hq-auth granted --for serve <platform>`
//! lists; their secrets come from `hq-auth get --for serve --profile <p> <platform> <VAR>` and are never logged.

#![allow(dead_code)] // the adapters (next packages) use what is not called yet

mod adapter;
mod attachments;
mod core;
mod discord;
pub(crate) mod hq;
mod judge;
mod manager;
mod queue;
pub(crate) mod run;
pub(crate) mod settings;
mod slack;
mod state;
mod telegram;
pub(crate) mod text;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use adapter::{Adapter, Alive, Constructor, Ended, Fut, Incoming, Reaction, Secrets, StopPress};
pub use core::Core;
#[allow(unused_imports)]
pub use manager::Registry;
pub use manager::status;
pub use queue::Wake;

use crate::hub::Hub;
use std::path::Path;
use std::sync::Arc;

/// The running core, for the host to reach the chat runs of a person whose access ended.
static CORE: std::sync::OnceLock<Arc<Core>> = std::sync::OnceLock::new();

/// Ends the chat-channel runs of `person` (all, or only those on agents they may no longer use). A host without
/// channels has none.
pub async fn stop_person_runs(person: &str, only_denied: bool) {
    if let Some(core) = CORE.get() {
        let n = core.stop_person(person, only_denied).await;
        if n > 0 {
            tracing::info!(person, n, "stopped channel runs after an access change");
        }
    }
}

/// An approved card's resume for a channel thread (`wk` is `<thread>#<agent>`). A host without channels reports
/// `Offline`: the resume stays owed.
pub fn wake(wk: &str, text: &str, claim: impl FnOnce() -> Vec<String>) -> Wake {
    CORE.get().map_or(Wake::Offline, |core| core.wake(wk, text, claim))
}

/// Whether the thread behind `wk` is one of the channels' (a card's session names it, see `runauth::grant_for_env`).
pub fn has_work_key(wk: &str) -> bool {
    wk.rsplit_once('#').is_some_and(|(key, _)| CORE.get().is_some_and(|core| core.has_thread(key)))
}

/// Starts the channels in the background: nothing happens without hq, or with `channels.enabled` off.
pub fn start(hub: &Arc<Hub>) {
    tokio::spawn(run(hub.clone()));
}

async fn run(hub: Arc<Hub>) {
    let settings = settings::Settings::load();
    if !settings.enabled {
        tracing::info!("channels are off (channels.enabled in config.json)");
        manager::set_off("disabled");
        return;
    }
    let opt_in = std::env::var_os(OPT_IN_ENV).is_some_and(|v| v == "1");
    let home = crate::service::data_dir();
    let paths = match prepare(&home, &crate::service::default_data_dir(), opt_in, hq::Paths::discover).await {
        Ok(Some(paths)) => paths,
        Ok(None) => {
            tracing::info!("channels: no hq found; chat channels are off");
            return;
        }
        Err(why) => {
            tracing::info!("channels are off: {why}; set {OPT_IN_ENV}=1 to run them in this home anyway");
            manager::set_off(why);
            return;
        }
    };
    let judge = judge::Judge::from_hub(hub.clone(), paths.clone());
    let core = Core::new(core::Deps { paths, settings, gate: hub.gate.clone(), judge, backups: hub.backups.clone() });
    // A channel run can ring the person it serves (see `calls::run_env`).
    let calls_hub = hub.clone();
    let _ = core.call_env.set(Box::new(move |agent, person| crate::calls::run_env(&calls_hub, agent, person)));
    tokio::spawn(attachments::prune_daily(core.attachments_root()));
    let _ = CORE.set(core.clone());
    manager::Manager::new(core, Registry::builtin(), manager::Backoff::default()).run().await;
}

/// Env var that lets a host with a temporary `KITH_SERVE_HOME` run channels anyway.
const OPT_IN_ENV: &str = "KITH_SERVE_CHANNELS";

/// Why channels must not start in `home`: hq-auth and `~/.hq` are global, so a throwaway host would otherwise
/// connect Kai's real bots (second Telegram poller, double Discord answers) and import the bridge's state.
fn home_guard(home: &Path, default: &Path, opt_in: bool) -> Option<&'static str> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    (!opt_in && canon(home) != canon(default)).then_some("temporary home")
}

/// Looks for hq (which is what reads grants and the bridge's state later) only when the home allows channels.
async fn prepare(
    home: &Path,
    default: &Path,
    opt_in: bool,
    discover: impl FnOnce() -> Option<hq::Paths> + Send + 'static,
) -> Result<Option<hq::Paths>, &'static str> {
    if let Some(why) = home_guard(home, default, opt_in) {
        return Err(why);
    }
    Ok(tokio::task::spawn_blocking(discover).await.ok().flatten())
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kith-guard-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn default_home_runs_and_others_do_not() {
        let (default, other) = (tmp("default"), tmp("other"));
        assert_eq!(home_guard(&default, &default, false), None);
        assert_eq!(home_guard(&other, &default, false), Some("temporary home"));
        assert_eq!(home_guard(&other, &default, true), None);
    }

    #[test]
    fn a_symlink_to_the_default_is_the_default() {
        let default = tmp("sym-default");
        let link = tmp("sym-parent").join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&default, &link).unwrap();
        assert_eq!(home_guard(&link, &default, false), None);
    }

    #[test]
    fn uncanonicalizable_paths_compare_as_given() {
        let (a, b) = (PathBuf::from("/nonexistent/kith-a"), PathBuf::from("/nonexistent/kith-b"));
        assert_eq!(home_guard(&a, &a, false), None);
        assert_eq!(home_guard(&a, &b, false), Some("temporary home"));
    }

    #[tokio::test]
    async fn a_temporary_home_never_looks_for_hq_unless_opted_in() {
        let (default, other) = (tmp("p-default"), tmp("p-other"));
        let called = Arc::new(AtomicBool::new(false));
        let probe = |called: Arc<AtomicBool>| {
            move || {
                called.store(true, Ordering::SeqCst);
                None
            }
        };
        let off = prepare(&other, &default, false, probe(called.clone())).await;
        assert_eq!(off.unwrap_err(), "temporary home");
        assert!(!called.load(Ordering::SeqCst), "no grants or bridge state may be reached");
        let on = prepare(&other, &default, true, probe(called.clone())).await;
        assert!(on.unwrap().is_none());
        assert!(called.load(Ordering::SeqCst));
        assert!(prepare(&default, &default, false, probe(called)).await.is_ok());
    }
}
