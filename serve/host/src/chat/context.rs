//! The prompt snapshots a host-native bot kept per ACP session (kv keys `context.<bot>.<session>`). One-bot-model C5:
//! nothing renders or reads them any more; only the memory import still clears what an old host left.

use crate::store::Store;
use anyhow::Result;

/// Clears every snapshot the bot's sessions kept, so none is read as current.
pub fn invalidate(store: &Store, bot_id: &str) -> Result<()> {
    for k in store.kv_prefix(&format!("context.{bot_id}.")) {
        store.kv_set(&k, "")?;
    }
    Ok(())
}
