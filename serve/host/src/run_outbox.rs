//! Runs' outboxes: what `kith-send` leaves for the app from an app chat's run or a `kith-sched` job.
//!
//! `<machine>/outbox/<run>/` holds a run's items (`<n>.json`, and the files they name). `<run>.json` beside it, written
//! by whoever started the run and out of the run's reach (a guest's sandbox may write that one folder only), says whom
//! the run was for. Each item lands once, across restarts too, in that person's own chat with the agent, never in a
//! group. The run may still be writing, so items and their files are moved into `.taken/<run>/`, where nothing but the
//! host writes, before anything in them is trusted.
use crate::chat::files;
use crate::hub::Hub;
use crate::store::{EntryKind, Lane};
use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const MAX_ITEM: u64 = 1 << 20;
const MAX_FILES: usize = 10;
/// A schedule run's box, empty this long, is done with.
const KEEP_EMPTY: Duration = Duration::from_secs(24 * 3600);
const TAKEN: &str = ".taken";

#[derive(Deserialize)]
struct Meta {
    agent: String,
    person: Option<String>,
    #[serde(default)]
    via: String,
    job: Option<String>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default, rename = "as")]
    sender: String,
    text: Option<String>,
    caption: Option<String>,
    #[serde(default)]
    files: Vec<Stored>,
}

#[derive(Deserialize)]
struct Stored {
    file: String,
    name: String,
}

fn root() -> Option<PathBuf> {
    // Unit tests never touch a real outbox.
    if cfg!(test) {
        return Some(crate::service::default_data_dir().join("outbox"));
    }
    let machine = crate::kernel::var("MACHINE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| crate::kernel::machine_dir(&home)))?;
    Some(machine.join("outbox"))
}

/// The outbox of `agent`'s runs for `person`'s app chat (`None` is the owner), made if it isn't there:
/// `KITH_RUN_OUTBOX` for them.
pub fn for_chat(agent: &str, person: Option<&str>) -> Result<PathBuf> {
    make(&root().context("no home folder")?, agent, person)
}

fn make(root: &Path, agent: &str, person: Option<&str>) -> Result<PathBuf> {
    // A hash, so no two people's chats can ever share a folder however their ids are spelled.
    let who = files::digest_hex(&Sha256::digest(format!("{agent}\0{}", person.unwrap_or("\0owner"))));
    let run = format!("chat-{}-{}", clean(agent), &who[..16]);
    let dir = root.join(&run);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let meta = root.join(format!("{run}.json"));
    let tmp = root.join(format!(".{run}.json.tmp"));
    std::fs::write(&tmp, json!({"agent": agent, "person": person, "via": "app"}).to_string())?;
    std::fs::rename(&tmp, &meta)?;
    Ok(dir)
}

fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect()
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A file an item names: never another item.
fn is_data(name: &str) -> bool {
    valid(name) && !Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

pub fn start(hub: &Arc<Hub>) {
    let Some(root) = root() else { return };
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        let mut clock = tokio::time::interval(Duration::from_secs(2));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            clock.tick().await;
            let Some(hub) = weak.upgrade() else { break };
            let root = root.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || drain(&hub, &root)).await {
                tracing::warn!(%error, "run outbox will retry");
            }
        }
    });
}

/// Posts every item waiting in `root`, each once.
fn drain(hub: &Hub, root: &Path) {
    let Ok(list) = std::fs::read_dir(root) else { return };
    for meta_path in list.flatten().map(|e| e.path()) {
        let Some(run) = meta_path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if !valid(run) || !regular(&meta_path) {
            continue;
        }
        if let Err(error) = drain_run(hub, root, run, &meta_path) {
            tracing::warn!(run, error = format!("{error:#}"), "run outbox item will retry");
        }
    }
}

fn drain_run(hub: &Hub, root: &Path, run: &str, meta_path: &Path) -> Result<()> {
    let meta: Meta = serde_json::from_slice(&std::fs::read(meta_path)?).context("reading the run's meta")?;
    let dir = root.join(run);
    let taken = root.join(TAKEN).join(run);
    let in_box = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
    if in_box {
        take(&dir, &taken)?;
    }
    let chat = crate::calls::chat_of(hub, &meta.agent, meta.person.as_deref());
    if let Ok(items) = std::fs::read_dir(&taken) {
        let mut stems: Vec<String> = items
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_owned))
            .filter(|s| valid(s))
            .collect();
        stems.sort_by_key(|s| s.parse::<u128>().unwrap_or(u128::MAX));
        for stem in stems {
            post(hub, chat.as_deref(), &meta, run, &taken, &stem)?;
        }
        let _ = std::fs::remove_dir(&taken);
    }
    // A schedule run's box is used once; a chat's stays for the chat's next run.
    if meta.via == "schedule"
        && (!in_box || std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_none()))
        && std::fs::metadata(meta_path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() > KEEP_EMPTY)
    {
        let _ = std::fs::remove_dir(&dir);
        std::fs::remove_file(meta_path)?;
    }
    Ok(())
}

/// Moves the box's finished items, and the files they name, out of the run's reach.
fn take(dir: &Path, taken: &Path) -> Result<()> {
    let names: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.strip_suffix(".json").is_some_and(valid))
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(taken)?;
    for name in names {
        let at = taken.join(&name);
        std::fs::rename(dir.join(&name), &at)?;
        for f in read_item(&at).map(|i| i.files).unwrap_or_default() {
            if is_data(&f.file) {
                let _ = std::fs::rename(dir.join(&f.file), taken.join(&f.file));
            }
        }
    }
    Ok(())
}

/// A plain file of its own: not a link, not a second name of another file.
fn regular(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| {
        #[cfg(unix)]
        let single = std::os::unix::fs::MetadataExt::nlink(&m) == 1;
        #[cfg(not(unix))]
        let single = true;
        m.file_type().is_file() && single
    })
}

fn read_item(path: &Path) -> Option<Item> {
    let size = std::fs::symlink_metadata(path).ok()?.len();
    if !regular(path) || size > MAX_ITEM {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn post(hub: &Hub, chat: Option<&str>, meta: &Meta, run: &str, taken: &Path, stem: &str) -> Result<()> {
    let at = taken.join(format!("{stem}.json"));
    let item = read_item(&at);
    let id = format!("outbox-{run}-{stem}");
    if let (Some(chat), Some(item)) = (chat, &item)
        && hub.store.entry(&id).is_none()
    {
        let root = files::root(chat);
        let mut snapshots = Vec::new();
        for f in item.files.iter().take(MAX_FILES) {
            let ok = valid(&f.file) && regular(&taken.join(&f.file));
            let shot = files::name(&f.name)
                .and_then(|name| ok.then_some(name).context("not a plain file"))
                .and_then(|name| files::snapshot(&root, taken, &f.file, Some(&name), &AtomicBool::new(false)));
            match shot {
                Ok(s) => snapshots.push(s),
                Err(error) => tracing::warn!(run, file = f.name, error = format!("{error:#}"), "outbox file skipped"),
            }
        }
        let caption = item.caption.as_deref().or(item.text.as_deref()).unwrap_or_default().trim().to_owned();
        let text = if caption.is_empty() {
            snapshots.iter().map(|s| format!("{} ({} bytes)", s.meta.name, s.meta.size)).collect::<Vec<_>>().join(", ")
        } else {
            caption
        };
        if !text.is_empty() {
            let mut data = json!({
                "text": text,
                "final": true,
                "outbox": {"agent": meta.agent, "person": meta.person, "via": meta.via, "job": meta.job, "as": item.sender},
            });
            if !snapshots.is_empty() {
                data["files"] = Value::from(snapshots.iter().map(|s| json!(s.meta)).collect::<Vec<_>>());
            }
            hub.add_entry_once(&id, &Lane::main(chat), EntryKind::Agent, &data)?;
            snapshots.iter_mut().for_each(files::Snapshot::commit);
        }
    } else if chat.is_none() {
        tracing::info!(run, agent = meta.agent, "run outbox item dropped: the person has no app chat with the agent");
    }
    for f in item.iter().flat_map(|i| &i.files) {
        if is_data(&f.file) {
            let _ = std::fs::remove_file(taken.join(&f.file));
        }
    }
    std::fs::remove_file(&at)?;
    Ok(())
}

#[cfg(test)]
mod tests;
