//! Where a host-native bot's memory files lived, and what `agent/bot` still asks about them.
//!
//! The host no longer runs native bots (C5), so nothing reads, scans or writes these files. They stay on disk,
//! untouched, and `exportHostBot` copies them (`api::native_export`). The scan hooks below are no-ops until
//! `agent/bot`'s native arms, their last callers, are deleted.

use crate::hub::Hub;
use crate::store::{BotConfig, Store};
use kith_mem::files as mf;
use std::path::PathBuf;
use std::sync::Arc;

/// A layer's folder: the shared one (`None`), or a bot's own.
pub fn folder(bot: Option<&str>) -> PathBuf {
    let data = crate::service::data_dir();
    match bot {
        Some(id) => data.join("bots").join(id).join("bot-memory"),
        None => data.join("shared-memory"),
    }
}

/// The layers a bot sees: its own, and the shared one when it uses it.
pub fn layers(cfg: &BotConfig) -> Vec<Option<String>> {
    let mut layers = vec![Some(cfg.id.clone())];
    if super::shared(cfg) {
        layers.push(None);
    }
    layers
}

/// The folders a bot sees, created so an agent can be given them.
pub fn folders(cfg: &BotConfig) -> Vec<PathBuf> {
    layers(cfg)
        .iter()
        .map(|layer| {
            let dir = folder(layer.as_deref());
            if let Err(error) = std::fs::create_dir_all(&dir) {
                tracing::warn!(dir = %dir.display(), error = %error, "couldn't create a memory folder");
            }
            dir
        })
        .collect()
}

/// The index of the files a bot sees, for its opening.
pub fn index(store: &Store, cfg: &BotConfig) -> String {
    match store.memory_read(|c| mf::index(c, &cfg.id, super::shared(cfg))) {
        Ok(index) => index.render(|bot| folder(bot).display().to_string()),
        Err(error) => {
            tracing::warn!(bot = %cfg.id, error = format!("{error:#}"), "couldn't read the memory files");
            String::new()
        }
    }
}

/// A native bot's tool call that touched a memory file: not recorded any more.
pub fn touched(_store: &Store, _cfg: &BotConfig, _tool: &serde_json::Value) {}

/// A scan of the folders a bot sees: not done any more. Nothing was too fresh to take.
pub fn scan(_hub: &Hub, _cfg: &BotConfig) -> bool {
    false
}

/// [`scan`] at the end of a turn.
pub fn scan_after_turn(_hub: Arc<Hub>, _cfg: BotConfig) {}
