//! Installed connectors as kith services: one secret-free `<machine dir>/services/<id>.json` per connector, so
//! `kith <agent> connect <id>` can grant it and the kernel's `kith-auth mcp-config` can start it as
//! `kith serve mcp local|remote --connector <id>`. Env and header values, URLs and OAuth tokens stay in the vault; the
//! proxy asks this host for them with the local API token when the agent's session starts.
//!
//! A file is ours when it has a `connector` key. Files without one (a service the owner wrote) are never touched, and
//! their names count as taken when a connector gets its id. Composio apps are a separate mechanism (their tools come
//! through `kith-composio` per bot, not through `Connector`s), so they are not mirrored.

use super::{Connector, slug};
use crate::LockExt;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct Target {
    /// `<machine dir>/services`.
    services: PathBuf,
    /// `<kernel>/services`, whose names a connector id must not take.
    kernel_services: Option<PathBuf>,
}

/// Unset until `start`: tests and a host that never started write nothing into the owner's folders.
static TARGET: OnceLock<Target> = OnceLock::new();
/// Clashing ids already reported, so a log line comes once per run, not on every update.
static REPORTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The mirror of a connector, or None when its id can't be a file name.
fn service(c: &Connector) -> Option<Value> {
    if slug(&c.id) != c.id {
        return None;
    }
    let public = c.public();
    let kind = public["kind"].clone();
    let description =
        if c.description.is_empty() { format!("{} (MCP connector)", c.name) } else { c.description.clone() };
    Some(json!({
        "description": description,
        "kind": "mcp",
        "connector": {
            "id": c.id, "name": c.name, "kind": kind,
            "registryName": c.registry_name, "auth": public["auth"],
        },
        "mcp": {
            "command": "kith",
            "args": ["serve", "mcp", kind.as_str().unwrap_or("local"), "--connector", c.id],
        },
        "secrets": [],
    }))
}

fn read(file: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

fn is_ours(file: &Path) -> bool {
    read(file).is_some_and(|v| v["connector"].is_object())
}

fn stems(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
        .collect()
}

/// Names a new connector id must avoid: the kernel's services and the owner's own files in the machine folder.
fn taken_in(target: &Target) -> Vec<String> {
    let mut names = target.kernel_services.as_deref().map(stems).unwrap_or_default();
    names.extend(stems(&target.services).into_iter().filter(|n| !is_ours(&target.services.join(format!("{n}.json")))));
    names
}

/// For `unique_id`: the service names a connector id must not take (none before `start`).
pub(super) fn taken_names() -> Vec<String> {
    TARGET.get().map(taken_in).unwrap_or_default()
}

fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(value)? + "\n")?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Makes the folder match the connectors: writes what is missing or changed, deletes our files for connectors that
/// are gone, leaves every other file alone.
fn sync(target: &Target, all: &[Connector]) -> Result<()> {
    std::fs::create_dir_all(&target.services).with_context(|| format!("creating {}", target.services.display()))?;
    let kernel: HashSet<String> =
        target.kernel_services.as_deref().map(stems).unwrap_or_default().into_iter().collect();
    let mut keep = HashSet::new();
    for c in all {
        let file = target.services.join(format!("{}.json", c.id));
        let Some(want) = service(c) else { continue };
        if kernel.contains(&c.id) || (file.exists() && !is_ours(&file)) {
            let mut reported = REPORTED.locked();
            if !reported.contains(&c.id) {
                tracing::warn!(connector = %c.id, "not offered to kith agents: its id is already a service name");
                reported.push(c.id.clone());
            }
            continue;
        }
        keep.insert(c.id.clone());
        if read(&file).as_ref() != Some(&want) {
            write_atomic(&file, &want).with_context(|| format!("writing {}", file.display()))?;
        }
    }
    for name in stems(&target.services) {
        let file = target.services.join(format!("{name}.json"));
        if !keep.contains(&name) && is_ours(&file) {
            let _ = std::fs::remove_file(&file);
        }
    }
    Ok(())
}

/// After every change to the connector list (called under the list's lock).
pub(super) fn refresh(all: &[Connector]) {
    if let Some(target) = TARGET.get()
        && let Err(error) = sync(target, all)
    {
        tracing::warn!(error = format!("{error:#}"), "couldn't update the connector services");
    }
}

/// Host start-up: turns the mirror on and brings the folder in line with what is installed.
pub fn start(store: &crate::store::Store) {
    let home = crate::service::home();
    let target = Target {
        services: crate::kernel::machine_dir(&home).join("services"),
        kernel_services: crate::kernel::current_kernel().map(|k| k.join("services")),
    };
    let _ = TARGET.set(target);
    let _guard = store.connector_lock.locked();
    match super::connectors(store) {
        Ok(all) => refresh(&all),
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "couldn't read the connectors to offer to kith agents");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn connector(id: &str, remote: bool) -> Connector {
        Connector {
            id: id.into(),
            name: id.to_uppercase(),
            description: String::new(),
            registry_name: Some(format!("io.example/{id}")),
            command: (!remote).then(|| "npx".to_owned()),
            args: vec!["--token=ARGSECRET".into()],
            env: BTreeMap::from([("API_KEY".into(), "ENVSECRET".into())]),
            url: remote.then(|| "https://user:URLSECRET@example.com/mcp?key=QUERYSECRET".to_owned()),
            headers: BTreeMap::from([("Authorization".into(), "HEADERSECRET".into())]),
            oauth: None,
        }
    }

    fn setup() -> (PathBuf, Target) {
        let dir = std::env::temp_dir().join(format!("kith-mirror-{}", uuid::Uuid::new_v4()));
        let kernel = dir.join("kernel/services");
        std::fs::create_dir_all(&kernel).unwrap();
        std::fs::write(kernel.join("slack.json"), "{}").unwrap();
        let target = Target { services: dir.join("machine/services"), kernel_services: Some(kernel) };
        (dir, target)
    }

    #[test]
    fn the_file_has_the_contract_shape_and_no_secret() {
        let (dir, target) = setup();
        sync(&target, &[connector("linear", true), connector("files", false)]).unwrap();
        let text = std::fs::read_to_string(target.services.join("linear.json")).unwrap();
        for secret in ["ENVSECRET", "HEADERSECRET", "URLSECRET", "QUERYSECRET", "ARGSECRET", "example.com"] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["kind"], "mcp");
        assert_eq!(v["secrets"], json!([]));
        assert_eq!(v["connector"]["kind"], "remote");
        assert_eq!(v["connector"]["auth"], "none");
        assert_eq!(v["connector"]["registryName"], "io.example/linear");
        assert_eq!(v["mcp"], json!({"command": "kith", "args": ["serve", "mcp", "remote", "--connector", "linear"]}));
        let local = read(&target.services.join("files.json")).unwrap();
        assert_eq!(local["mcp"]["args"][2], "local");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sync_adds_updates_and_removes_only_its_own_files() {
        let (dir, target) = setup();
        std::fs::create_dir_all(&target.services).unwrap();
        std::fs::write(target.services.join("mine.json"), r#"{"kind":"cli"}"#).unwrap();
        sync(&target, &[connector("a", true), connector("b", true)]).unwrap();
        assert!(target.services.join("a.json").is_file() && target.services.join("b.json").is_file());
        sync(&target, &[connector("a", false)]).unwrap();
        assert!(!target.services.join("b.json").exists());
        assert_eq!(read(&target.services.join("a.json")).unwrap()["connector"]["kind"], "local");
        assert!(target.services.join("mine.json").is_file());
        sync(&target, &[]).unwrap();
        assert!(!target.services.join("a.json").exists());
        assert!(target.services.join("mine.json").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_clashing_id_is_not_mirrored_and_names_are_taken() {
        let (dir, target) = setup();
        std::fs::create_dir_all(&target.services).unwrap();
        std::fs::write(target.services.join("mine.json"), r#"{"kind":"cli"}"#).unwrap();
        sync(&target, &[connector("slack", true), connector("mine", true), connector("ok", true)]).unwrap();
        assert!(!target.services.join("slack.json").exists());
        assert_eq!(std::fs::read_to_string(target.services.join("mine.json")).unwrap(), r#"{"kind":"cli"}"#);
        assert!(target.services.join("ok.json").is_file());
        let taken = taken_in(&target);
        assert!(taken.contains(&"slack".to_owned()) && taken.contains(&"mine".to_owned()));
        assert!(!taken.contains(&"ok".to_owned()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
