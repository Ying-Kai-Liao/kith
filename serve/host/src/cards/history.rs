//! `kith cards history`: the card history, for an agent whose package says `cards: read` (designs/2026-11-02-grant-card).
//!
//! The run asks over the run socket with its owner ask token (`cardsHistory`); the agent is the token's, never the
//! request's. The kernel says whether that agent may read (`kith-cards --may-read`, its capabilities.yaml). The answer is
//! when, which agent, what kind of action, how it ended and which button: never the tool's input, the resource, a token
//! or who answered. serve.db stays secret: nothing here reads it as a file.

use super::{Hub, autoallow, edge, session, tool_kind};
use crate::runauth::RunIdentity;
use serde_json::{Value, json};

/// Whether `agent` may read the history, as the kernel says. No kernel, no answer: no.
async fn may_read(agent: &str) -> bool {
    let Some(auth) = crate::remote::account::bin().await else { return false };
    let bin = auth.with_file_name("kith-cards");
    crate::channels::hq::output_with(&bin, &["--may-read", agent], None).await.is_some_and(|o| o.status.success())
}

/// A tapped option as a fixed word: the host's own option ids only, anything else is "other".
fn button(answer: Option<&str>) -> Value {
    match answer {
        None => Value::Null,
        Some("allow") => "Allow once".into(),
        Some("deny") => "Deny".into(),
        Some("cancel") => "Cancelled".into(),
        Some(session::SESSION) => "This session".into(),
        Some(autoallow::ALWAYS | edge::ALWAYS) => "Always allow".into(),
        Some(_) => "other".into(),
    }
}

/// The history line of the run socket: `{"ok": true, "cards": [{at, agent, kind, outcome, button, auto}]}`, newest
/// first, or a refusal for an agent without `cards: read`.
pub async fn list(hub: &Hub, who: &RunIdentity, limit: usize) -> Value {
    if !may_read(&who.agent).await {
        return json!({"ok": false, "error": format!(
            "{} may not read the card history: its package needs `cards: read` in capabilities.yaml (builder adds it)",
            who.agent
        )});
    }
    let cards: Vec<Value> = hub
        .store
        .cards_recent(limit)
        .iter()
        .map(|c| {
            json!({
                "at": c.created_at,
                "agent": c.agent,
                "kind": tool_kind(&c.tool),
                "outcome": c.state.as_str(),
                "button": button(c.answer.as_deref()),
                "auto": autoallow::is_auto(c),
            })
        })
        .collect();
    json!({"ok": true, "cards": cards})
}
