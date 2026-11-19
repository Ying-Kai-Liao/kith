//! `exportHostBot {botId}`: a host-native bot (an agent row with no kith agent behind it, see `BotConfig::is_native`)
//! written out as a folder, so it can be remade as a kith agent (one-bot-model C5). The host no longer runs such a bot;
//! its row stays listed and readable, and this is how its keep-sake leaves it.
//!
//! The bundle is `<data dir>/bots/<id>/kith-export/`, next to the bot's own memory files:
//! - `bot.json`: name, description, engine, model, effort and the other settings the row had;
//! - `memory/`: a copy of its host chat-memory files (the folder `chat::memory::files::folder` names);
//! - `routines.json`: each routine's name, prompt, triggers and whether it is paused. A webhook trigger is only the word
//!   `webhook`: the key that authorizes it is never written;
//! - `vault.json`: the vault env keys the bot used. Their NAMES would need the vault to be read, and nothing in
//!   `market::vault` lists names without values, so none are exported and the file says so.
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

async fn export(hub: &Arc<Hub>, bot_id: &str) -> Result<Value> {
    let row = hub.store.bot(bot_id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
    if !row.config.is_native() {
        bail!("only a host-native bot can be exported; this one is a kith agent or a room");
    }
    let routines = routines_of(hub, bot_id);
    let (cfg, id) = (row.config, bot_id.to_owned());
    let path = tokio::task::spawn_blocking(move || write_bundle(&cfg, &routines, &bundle_dir(&id))).await??;
    Ok(json!({"path": path.to_string_lossy()}))
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
fn write_bundle(cfg: &BotConfig, routines: &[Value], dir: &Path) -> Result<PathBuf> {
    let staging = dir.with_extension("tmp");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("couldn't clear an earlier unfinished export")?;
    }
    std::fs::create_dir_all(&staging)?;
    let put = |name: &str, v: &Value| -> Result<()> {
        std::fs::write(staging.join(name), serde_json::to_string_pretty(v)?).with_context(|| format!("writing {name}"))
    };
    put("bot.json", &settings_of(cfg))?;
    put("routines.json", &json!({"routines": routines}))?;
    put(
        "vault.json",
        &json!({
            "keyNames": null,
            "note": "The names of the vault keys this bot used are not exported: the vault has no way to list names without reading their values. Re-enter the engine's key for the new kith agent.",
        }),
    )?;
    copy_memory(&crate::chat::memory::files::folder(Some(&cfg.id)), &staging.join("memory"))?;
    if dir.exists() {
        std::fs::remove_dir_all(dir).context("couldn't replace the earlier export")?;
    }
    std::fs::rename(&staging, dir)?;
    Ok(dir.to_owned())
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
