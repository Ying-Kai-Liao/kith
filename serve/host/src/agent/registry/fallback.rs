//! Which build of a registry agent to start when the registry's own entry fails.
//!
//! The registry can name a version npm doesn't have yet (codex-acp 2.12.0 while npm's latest
//! was 2.10.0). Order: the registry's entry, then the entry that last started on this Mac
//! (`last-good.json`), then a reviewed npm floor from [`PINS`]. Nothing else: never a version
//! that never ran here or that nobody reviewed.

use crate::LockExt;
use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Reviewed floors, agent id -> full npm spec. Drop an entry once the registry and npm agree again.
const PINS: &[(&str, &str)] = &[("codex-acp", "@agentclientprotocol/codex-acp@2.9.0")];

/// Agent id -> the whole registry entry that last started on this Mac from the registry.
pub(super) fn store() -> PathBuf {
    crate::service::data_dir().join("last-good.json")
}

/// The name in a `name@version` spec (scoped names keep their leading `@`).
fn package_name(spec: &str) -> &str {
    spec.get(1..).and_then(|rest| rest.rfind('@')).map_or(spec, |i| &spec[..=i])
}

/// A file that can't be read or parsed counts as empty.
fn read_all(store: &Path) -> Map<String, Value> {
    std::fs::read_to_string(store).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn last_good(store: &Path, id: &str) -> Option<Value> {
    read_all(store).remove(id).filter(Value::is_object)
}

/// Records `entry` as the one `id` last started from, if it changed. Temp file plus rename, since
/// the host and a one-shot `engine-run` can both write it. Best effort: the lock is per process,
/// so two processes recording different agents at once can lose one row; it comes back on that
/// agent's next registry start. A failed write likewise only costs a later fallback.
fn record(store: &Path, id: &str, entry: &Value) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _one = WRITING.locked();
    let mut all = read_all(store);
    if all.get(id) == Some(entry) {
        return;
    }
    all.insert(id.to_owned(), entry.clone());
    let tmp = store.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, Value::Object(all).to_string()).and_then(|()| std::fs::rename(&tmp, store));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!(%error, agent = id, "couldn't record the agent's last good build");
    }
}

/// What tells two entries' builds apart.
fn build_of(entry: &Value) -> String {
    let npx = entry["distribution"]["npx"]["package"].as_str().unwrap_or_default();
    format!("{}|{npx}", entry["version"].as_str().unwrap_or_default())
}

/// The current entry with its npm package swapped for `id`'s reviewed floor, when the floor is
/// the same package (a package renamed upstream gets no floor).
fn pinned(id: &str, entry: &Value) -> Option<Value> {
    let npx = &entry["distribution"]["npx"];
    let current = package_name(npx["package"].as_str()?);
    let (_, spec) = PINS.iter().find(|(agent, spec)| *agent == id && package_name(spec) == current)?;
    let mut npx = npx.clone();
    npx["package"] = json!(spec);
    let mut out = entry.clone();
    out["version"] = json!(spec.rsplit('@').next());
    out["distribution"] = json!({ "npx": npx });
    Some(out)
}

/// The entries to try, in order, each build once.
fn candidates(id: &str, entry: &Value, recorded: Option<Value>) -> Vec<(Value, &'static str)> {
    let mut out = vec![(entry.clone(), "registry")];
    for (c, source) in [(recorded, "last good"), (pinned(id, entry), "reviewed floor")] {
        if let Some(c) = c
            && !out.iter().any(|(o, _)| build_of(o) == build_of(&c))
        {
            out.push((c, source));
        }
    }
    out
}

/// Starts the first entry that works. `launch` is `registry::launch` in use and a stub in tests.
/// Only the registry's own entry is recorded, so the record stays the newest build that ran here.
pub(super) async fn launch_best<T, F, Fut>(entry: &Value, store: &Path, mut launch: F) -> Result<T>
where
    F: FnMut(Value) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let id = entry["id"].as_str().unwrap_or_default();
    let name = entry["name"].as_str().unwrap_or(id);
    let mut first_error = None;
    for (c, source) in candidates(id, entry, last_good(store, id)) {
        let version = c["version"].as_str().unwrap_or("?").to_owned();
        if first_error.is_some() {
            tracing::info!(agent = id, version, source, "the registry's build failed; trying a fallback");
        }
        match launch(c).await {
            Ok(t) => {
                if first_error.is_none() {
                    record(store, id, entry);
                }
                return Ok(t);
            }
            Err(e) => {
                tracing::info!(agent = id, version, source, error = format!("{e:#}"), "agent build unavailable");
                first_error.get_or_insert(e);
            }
        }
    }
    let version = entry["version"].as_str().unwrap_or("?");
    let why = first_error.map_or_else(String::new, |e| format!("{e:#}"));
    Err(anyhow!(
        "{name} ({id} {version}) couldn't start, and no other build of it has run on this computer: {why}. \
         Fix: check that this computer can reach npm and the internet, or update kith for a reviewed fallback version."
    ))
}

/// npm installs that just failed, so a registry entry npm lacks isn't retried on every start.
const FAILED_FOR: Duration = Duration::from_secs(10 * 60);
static FAILED: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);

pub(super) fn recent_failure(spec: &str) -> Option<String> {
    let failed = FAILED.locked();
    let (at, why) = failed.as_ref()?.get(spec)?;
    (at.elapsed() < FAILED_FOR).then(|| why.clone())
}

/// Remembers only failures that will repeat (npm has no such version), not a network blip.
pub(super) fn remember_failure(spec: &str, why: &str) {
    if !(why.contains("E404") || why.contains("ETARGET")) {
        return;
    }
    let mut failed = FAILED.locked();
    let failed = failed.get_or_insert_with(HashMap::new);
    failed.retain(|_, (at, _)| at.elapsed() < FAILED_FOR);
    failed.insert(spec.to_owned(), (Instant::now(), why.to_owned()));
}

pub(super) fn forget_failure(spec: &str) {
    if let Some(failed) = FAILED.locked().as_mut() {
        failed.remove(spec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const REG: &str = "@agentclientprotocol/codex-acp@2.12.0";

    fn entry(id: &str, spec: &str) -> Value {
        let version = spec.rsplit('@').next().unwrap();
        json!({"id": id, "name": "Codex", "version": version, "distribution": {"npx": {"package": spec, "args": ["--acp"]}}})
    }

    fn temp_store() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-lastgood-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("last-good.json")
    }

    type Asked = Arc<Mutex<Vec<String>>>;

    /// A stub launcher that starts only the npm specs in `works`, and keeps what it was asked for.
    fn stub(works: &[&str]) -> (impl FnMut(Value) -> std::future::Ready<Result<String>>, Asked) {
        let works: Vec<String> = works.iter().map(|s| (*s).to_owned()).collect();
        let asked = Asked::default();
        let log = asked.clone();
        let f = move |e: Value| {
            let spec = e["distribution"]["npx"]["package"].as_str().unwrap().to_owned();
            log.locked().push(spec.clone());
            std::future::ready(if works.contains(&spec) { Ok(spec) } else { Err(anyhow!("npm error 404 {spec}")) })
        };
        (f, asked)
    }

    #[test]
    fn package_names_keep_their_scope() {
        assert_eq!(package_name(REG), "@agentclientprotocol/codex-acp");
        assert_eq!(package_name("cowsay@1.0.0"), "cowsay");
        assert_eq!(package_name("@s/x"), "@s/x");
        assert_eq!(package_name("x"), "x");
    }

    #[tokio::test]
    async fn last_good_is_used_when_the_registry_build_fails() {
        let store = temp_store();
        let good = entry("codex-acp", "@agentclientprotocol/codex-acp@2.10.0");
        record(&store, "codex-acp", &good);
        let (launch, asked) = stub(&["@agentclientprotocol/codex-acp@2.10.0", "@agentclientprotocol/codex-acp@2.9.0"]);
        let got = launch_best(&entry("codex-acp", REG), &store, launch).await.unwrap();
        assert_eq!(got, "@agentclientprotocol/codex-acp@2.10.0");
        assert_eq!(*asked.locked(), [REG, "@agentclientprotocol/codex-acp@2.10.0"]);
        // A fallback never rewrites the record.
        assert_eq!(last_good(&store, "codex-acp"), Some(good));
    }

    #[tokio::test]
    async fn the_floor_is_used_with_nothing_recorded() {
        let store = temp_store();
        let (launch, _) = stub(&["@agentclientprotocol/codex-acp@2.9.0"]);
        let got = launch_best(&entry("codex-acp", REG), &store, launch).await.unwrap();
        assert_eq!(got, "@agentclientprotocol/codex-acp@2.9.0");
        assert_eq!(last_good(&store, "codex-acp"), None);
    }

    #[test]
    fn the_floor_keeps_the_entrys_args_and_drops_its_binaries() {
        let mut e = entry("codex-acp", REG);
        e["distribution"]["binary"] = json!({"darwin-aarch64": {"archive": "https://x"}});
        let p = pinned("codex-acp", &e).unwrap();
        assert_eq!(p["version"], "2.9.0");
        assert_eq!(
            p["distribution"],
            json!({"npx": {"package": "@agentclientprotocol/codex-acp@2.9.0", "args": ["--acp"]}})
        );
    }

    #[tokio::test]
    async fn no_record_and_no_floor_is_a_clear_error() {
        let store = temp_store();
        let (launch, asked) = stub(&[]);
        let e = launch_best(&entry("other-acp", "@s/other-acp@3.0.0"), &store, launch).await.unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("Codex (other-acp 3.0.0)"), "{msg}");
        assert!(msg.contains("npm error 404 @s/other-acp@3.0.0"), "{msg}");
        assert!(msg.contains("Fix:"), "{msg}");
        assert_eq!(*asked.locked(), ["@s/other-acp@3.0.0"]);
    }

    #[tokio::test]
    async fn a_registry_start_updates_the_record() {
        let store = temp_store();
        record(&store, "codex-acp", &entry("codex-acp", "@agentclientprotocol/codex-acp@2.9.0"));
        record(&store, "gemini", &entry("gemini", "@google/gemini-cli@1.0.0"));
        let (launch, _) = stub(&[REG]);
        launch_best(&entry("codex-acp", REG), &store, launch).await.unwrap();
        assert_eq!(last_good(&store, "codex-acp"), Some(entry("codex-acp", REG)));
        assert_eq!(last_good(&store, "gemini"), Some(entry("gemini", "@google/gemini-cli@1.0.0")));
    }

    #[tokio::test]
    async fn a_working_registry_build_wins_over_record_and_floor() {
        let store = temp_store();
        record(&store, "codex-acp", &entry("codex-acp", "@agentclientprotocol/codex-acp@2.10.0"));
        let (launch, asked) =
            stub(&[REG, "@agentclientprotocol/codex-acp@2.10.0", "@agentclientprotocol/codex-acp@2.9.0"]);
        assert_eq!(launch_best(&entry("codex-acp", REG), &store, launch).await.unwrap(), REG);
        assert_eq!(*asked.locked(), [REG]);
    }

    #[test]
    fn each_build_is_tried_once_and_renamed_packages_get_no_floor() {
        let reg = entry("codex-acp", REG);
        assert_eq!(candidates("codex-acp", &reg, Some(reg.clone())).len(), 2);
        let renamed = entry("codex-acp", "@new/codex@3.0.0");
        assert_eq!(candidates("codex-acp", &renamed, None).len(), 1);
    }

    #[test]
    fn an_unreadable_record_counts_as_empty() {
        let store = temp_store();
        std::fs::write(&store, "{not json").unwrap();
        assert_eq!(last_good(&store, "codex-acp"), None);
        record(&store, "codex-acp", &entry("codex-acp", REG));
        assert_eq!(last_good(&store, "codex-acp"), Some(entry("codex-acp", REG)));
    }

    #[test]
    fn failures_are_remembered_until_forgotten() {
        let spec = format!("@s/flaky@{}", uuid::Uuid::new_v4());
        assert_eq!(recent_failure(&spec), None);
        remember_failure(&spec, "npm error code ETIMEDOUT");
        assert_eq!(recent_failure(&spec), None, "a network error isn't remembered");
        remember_failure(&spec, "npm error code E404");
        assert_eq!(recent_failure(&spec).as_deref(), Some("npm error code E404"));
        forget_failure(&spec);
        assert_eq!(recent_failure(&spec), None);
    }
}
