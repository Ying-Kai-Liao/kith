//! `exportHostBot {botId}`: a host-native bot (an agent row with no kith agent behind it, see `BotConfig::is_native`)
//! written out as a folder, so it can be remade as a kith agent (one-bot-model C5). The host no longer runs such a bot;
//! its row stays listed and readable, and this is how its keep-sake leaves it.
//!
//! The bundle is `<data dir>/bots/<id>/kith-export/`, next to the bot's own memory files:
//! - `bot.json`: name, description, engine, model, effort and the other settings the row had;
//! - `memory/`: a copy of its host chat-memory files (the folder `chat::memory::files::folder` names);
//! - `entries.json`: its kith-mem facts (the entries of its own layer in serve.db);
//! - `routines.json`: each routine's name, prompt, triggers and whether it is paused. A webhook trigger is only the word
//!   `webhook`: the key that authorizes it is never written;
//! - `vault.json`: the NAMES of the engine's saved env keys (`agent::auth::saved_env`), never a value. They stay where
//!   they are and a kith agent on that engine already uses them; `null` when the vault could not be read.
//!
//! The owner's alone (on no member or bot list). Running it again rebuilds the bundle in place. It refuses a kith agent's
//! row and a group.

use super::str_arg;
use crate::hub::Hub;
use crate::store::BotConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One export at a time: two share the staging folder, and one's cleanup would wipe the other's half-written copy.
static EXPORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "exportHostBot" => export(hub, str_arg(&b, "botId")?).await?,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

/// Where a bot's bundle goes.
pub(crate) fn bundle_dir(bot_id: &str) -> PathBuf {
    crate::service::data_dir().join("bots").join(bot_id).join("kith-export")
}

/// The row if it is still a host-native bot that exists.
fn native_row(hub: &Hub, bot_id: &str) -> Result<Option<crate::store::BotRow>> {
    Ok(hub.store.bot(bot_id)?.filter(|r| !r.deleted && r.config.is_native()))
}

async fn export(hub: &Arc<Hub>, bot_id: &str) -> Result<Value> {
    let _one = EXPORTS.lock().await;
    let row = hub.store.bot(bot_id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
    if !row.config.is_native() {
        bail!("only a host-native bot can be exported; this one is a kith agent or a room");
    }
    let routines = routines_of(hub, bot_id);
    let cfg = row.config;
    // Names only; a vault that cannot be read now (locked, no key store) exports none, and says so.
    let slot = crate::agent::auth::env_slot(&cfg.backend);
    let unlocked = crate::market::vault::unlock_if_sealed(hub, slot).await;
    let names = unlocked.and_then(|()| crate::agent::auth::saved_env(&hub.store, &cfg.backend)).ok();
    // The row's kith-mem facts live in serve.db, not in the memory folder, and nothing else reads them now.
    let entries = hub.store.memory_read(|c| kith_mem::store::layer(c, Some(bot_id)))?;
    let id = bot_id.to_owned();
    let path = tokio::task::spawn_blocking(move || {
        write_bundle(&cfg, &routines, &entries, names.as_deref(), &bundle_dir(&id))
    })
    .await??;
    settle(hub, bot_id, &path)
}

/// Delete holds no lock the export could take, and removes the bot's folders after its row write. Read the row again
/// now the bundle is in place: either this sees the delete and removes the bundle, or the delete's own removal runs
/// after the rename and takes it.
pub(super) fn settle(hub: &Hub, bot_id: &str, path: &Path) -> Result<Value> {
    if native_row(hub, bot_id)?.is_none() {
        remove_bundle(path);
        bail!("unknown bot");
    }
    Ok(json!({"path": path.to_string_lossy()}))
}

/// A missing folder is already what this wants.
fn remove_bundle(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), error = %error, "couldn't remove the export of a deleted bot");
    }
}

/// The routines as a bundle keeps them: no id, no key, no run state.
fn routines_of(hub: &Hub, bot_id: &str) -> Vec<Value> {
    let listed = hub.routines.list(bot_id);
    listed["routines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            json!({
                "name": r["name"], "prompt": r["instruction"], "triggers": r["triggers"],
                "paused": r["enabled"] != true, "timeoutSeconds": r["timeoutSeconds"],
            })
        })
        .collect()
}

/// Builds the bundle next to `dir` and swaps it in, so a failed export leaves the last good one.
fn write_bundle(
    cfg: &BotConfig,
    routines: &[Value],
    entries: &[kith_mem::store::Entry],
    key_names: Option<&[String]>,
    dir: &Path,
) -> Result<PathBuf> {
    let staging = staging_dir(dir);
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("couldn't clear an earlier unfinished export")?;
    }
    // Whatever fails below, no half-written staging folder stays behind.
    let built = stage(cfg, routines, entries, key_names, &staging).and_then(|()| swap_in(&staging, dir));
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    built.map(|()| dir.to_owned())
}

/// The folder a bundle is built in before it replaces `dir`.
pub(crate) fn staging_dir(dir: &Path) -> PathBuf {
    dir.with_extension("tmp")
}

fn stage(
    cfg: &BotConfig,
    routines: &[Value],
    entries: &[kith_mem::store::Entry],
    key_names: Option<&[String]>,
    staging: &Path,
) -> Result<()> {
    std::fs::create_dir_all(staging)?;
    let put = |name: &str, v: &Value| -> Result<()> {
        std::fs::write(staging.join(name), serde_json::to_string_pretty(v)?).with_context(|| format!("writing {name}"))
    };
    put("bot.json", &settings_of(cfg))?;
    put("routines.json", &json!({"routines": routines}))?;
    put("entries.json", &json!({"entries": entries}))?;
    put(
        "vault.json",
        &json!({
            "keyNames": key_names,
            "note": "Names only, never values. The engine's keys stay where they are (saved per engine), and a kith agent on the same engine already uses them: nothing needs to be copied or entered again. keyNames is null when the vault could not be read just now.",
        }),
    )?;
    copy_memory(&crate::chat::memory::files::folder(Some(&cfg.id)), &staging.join("memory"))
}

fn swap_in(staging: &Path, dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).context("couldn't replace the earlier export")?;
    }
    std::fs::rename(staging, dir)?;
    Ok(())
}

fn settings_of(cfg: &BotConfig) -> Value {
    json!({
        "name": cfg.name,
        "description": cfg.description,
        "engine": cfg.backend,
        "model": cfg.model,
        "effort": cfg.model_options.get("effort"),
        "modelOptions": cfg.model_options,
        "permission": cfg.permission,
        "project": cfg.project,
        "folder": cfg.cwd,
        "connectors": cfg.connectors,
        "skills": cfg.skills,
        // Where it kept things; the folders are not copied (memory/ holds the bot's own files).
        "usedSharedMemory": crate::chat::memory::shared(cfg),
        "memoryFolder": crate::chat::memory::files::folder(Some(&cfg.id)),
        "sharedMemoryFolder": crate::chat::memory::files::folder(None),
        "managedWorkspace": crate::agent::workspace::is_managed(cfg),
    })
}

/// Copies the regular files under `from` (none when it has no folder yet) into `to`; links are not followed.
fn copy_memory(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let Ok(entries) = std::fs::read_dir(from) else { return Ok(()) };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_memory(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
