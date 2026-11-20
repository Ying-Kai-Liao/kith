//! A rollover's handoff card in an old host-native chat. One-bot-model C5: the host drafts none any more (an agent's
//! context is its own), but a card an earlier host left in a chat is still a draft the user can edit or keep, until
//! their next message; `editHandoff` and `keepConversation` act on it. The card is a notice whose `handoff` field holds
//! the draft, so an app that doesn't know it still shows the text.

use crate::hub::Hub;
use crate::store::Entry;
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The longest handoff a user can save.
const MAX_CHARS: usize = 20_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// The next message starts a new conversation with it.
    Draft,
    /// The user kept the old conversation.
    Kept,
}

fn set_status(hub: &Hub, id: &str, status: Status, line: &str) -> Option<Entry> {
    let mut data = hub.store.entry(id)?.data;
    data["handoff"]["status"] = json!(status);
    data["text"] = line.into();
    hub.set_entry(id, &data)
}

/// `editHandoff` and `keepConversation`.
pub fn call(hub: &Hub, method: &str, b: &Value) -> Result<Value> {
    let id = b["entryId"].as_str().ok_or_else(|| anyhow!("entryId required"))?;
    let entry = hub.store.entry(id).filter(|e| e.data["handoff"].is_object()).ok_or_else(|| anyhow!("no such card"))?;
    if entry.data["handoff"]["status"] != json!(Status::Draft) {
        bail!("This conversation already moved on.");
    }
    let entry = match method {
        "editHandoff" => {
            let text =
                b["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| anyhow!("text required"))?;
            if text.chars().count() > MAX_CHARS {
                bail!("A handoff can be at most {MAX_CHARS} characters.");
            }
            let mut data = entry.data;
            data["handoff"]["text"] = text.into();
            hub.set_entry(id, &data)
        }
        "keepConversation" => set_status(hub, id, Status::Kept, "Kept this conversation."),
        _ => bail!("unknown method {method}"),
    };
    Ok(json!({"entry": entry}))
}
