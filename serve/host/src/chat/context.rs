//! The prompt snapshots a host-native bot kept per ACP session (kv keys `context.<bot>.<session>`). One-bot-model C5:
//! nothing renders them any more. The tidier still clears one before a compaction (`refresh`) and the memory import
//! clears what an old host left (`invalidate`); chatmem and the memory deletion remove those two.

use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: String,
    pub epoch: u32,
    /// The rendered instructions, exactly as the agent got them.
    pub system: String,
    /// The identity rendered into `system`.
    pub system_identity: Identity,
    /// The identity the agent last heard of (`system` or a later update block).
    pub announced: Identity,
    /// The memory revision `system` holds (`memory::current_revision` before it was rendered).
    #[serde(default)]
    pub memory: String,
}

fn key(bot_id: &str, session: &str) -> String {
    format!("context.{bot_id}.{session}")
}

#[allow(dead_code)]
fn load(store: &Store, bot_id: &str, session: &str) -> Option<Snapshot> {
    store.kv_get(&key(bot_id, session)).and_then(|v| serde_json::from_str(&v).ok())
}

#[allow(dead_code)]
fn save(store: &Store, bot_id: &str, s: &Snapshot) -> Result<()> {
    store.kv_set(&key(bot_id, &s.session), &serde_json::to_string(s)?)
}

/// One session's instructions are rendered again on its next turn: before a compaction the host starts, so the
/// compaction keeps them as they are now.
#[allow(dead_code)] // #1091's tidier calls it
pub fn refresh(store: &Store, bot_id: &str, session: &str) -> Result<()> {
    match load(store, bot_id, session) {
        Some(mut s) => {
            s.system.clear();
            save(store, bot_id, &s)
        }
        None => Ok(()),
    }
}

/// Every session of the bot renders its instructions again on its next turn (after the move from the old memory).
pub fn invalidate(store: &Store, bot_id: &str) -> Result<()> {
    for k in store.kv_prefix(&format!("context.{bot_id}.")) {
        match store.kv_get(&k).and_then(|v| serde_json::from_str::<Snapshot>(&v).ok()) {
            Some(mut s) => {
                s.system.clear();
                save(store, bot_id, &s)?;
            }
            None => store.kv_set(&k, "")?,
        }
    }
    Ok(())
}
