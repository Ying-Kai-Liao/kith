//! What is left of the host's chat memory for bots. Kith agents use kith memory (`hqmem`, `kith memory`), and the
//! host no longer runs host-native bots (C5), so the bot memory tools, their chat cards, the memory API, the file
//! scan and the one-time move from Engram are gone. Native bots' memory files stay on disk and `exportHostBot`
//! still copies them.
//!
//! What stays: the folder names `exportHostBot` copies from, whether a bot used the shared memory, and the memory
//! screen's revision.

pub mod files;

pub use crate::hqmem::tidier::{TidyEvent, spawn_tidier};

use crate::hub::Hub;
use crate::store::BotConfig;

/// Whether a bot uses the memory all bots share. Unless the user chose, a bot in its own workspace does and one
/// working in a project folder doesn't.
pub fn shared(cfg: &BotConfig) -> bool {
    cfg.shared_memory.unwrap_or_else(|| crate::agent::workspace::is_managed(cfg))
}

/// What a bot's memory screen shows changed outside the store (Claude Code's notes came or went): moves
/// `memoryRev` so the apps fetch it again, without telling the bot's sessions.
pub fn screen_changed(hub: &Hub, bot: &str) {
    if let Err(error) = hub.store.memory_write(|_, _, _| Ok(())) {
        tracing::warn!(bot, error = format!("{error:#}"), "couldn't move the memory revision");
    }
    hub.emit_bot(bot);
}
