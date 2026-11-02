//! "This session" on the owner's auto-mode card (designs/2026-11-02-grant-card): the action the card showed, as
//! `kith allow --derive` names it (the rule Always allow would remember, never a looser one), is allowed for the rest of
//! the conversation the run continues. A later block of the same action in that conversation is answered at once, with
//! no card.
//!
//! A grant belongs to the run token's conversation and to the Claude session the hook ran in: a resume keeps both, a new
//! task (or a clear) is a new Claude session. It ends at the first of: a block in that conversation from another Claude
//! session ("ended with the task"), 24 h ("ended after 24 h"), a change to the agent's permission settings ("ended,
//! settings changed"), or the owner's Remove. Must-ask actions are never offered (`kith allow --derive` refuses them, as
//! for Always allow). Owner runs only: an auto-mode card is the owner's.
//!
//! The card's entry carries `sessionGrant {id, state, endedAt?, endReason?}`; so does each tool entry the grant let
//! through (its "this session" tag, see [`tag_tool`]).

use super::autoallow::Derived;
use super::{Card, Hub};
use crate::runauth::RunIdentity;
use crate::store::{NewSessionGrant, SessionGrant, now_ms};
use serde_json::{Value, json};
use std::sync::{LazyLock, Mutex};

/// The card option.
pub const SESSION: &str = "kith-auto-session";
/// How long a grant lives at most.
pub const LIFETIME_MS: i64 = 24 * 3_600_000;
/// How long after a grant let a call through its tool entry may still be tagged.
const TAG_WINDOW_MS: i64 = 120_000;

/// The owner tapped This session: the grant is saved from what the card offered (`autoAllow` on its entry) and the card
/// says so. Late taps save too, as Always allow does.
pub fn grant(hub: &Hub, card: &Card, kept: &Value) {
    let (Some(rule), Some(kind)) = (kept["rule"].as_str(), kept["ruleKind"].as_str()) else {
        once_only(hub, card, "this card offered nothing to allow for the session");
        return;
    };
    // Without a Claude session the grant could never end with the task.
    let claude_session = kept["claudeSession"].as_str().unwrap_or_default();
    if claude_session.is_empty() {
        once_only(hub, card, "this run has no task to end it with");
        return;
    }
    let now = now_ms();
    let new = NewSessionGrant {
        agent: &card.agent,
        run_session: &card.session,
        claude_session,
        kind,
        rule,
        settings: kept["settings"].as_str().unwrap_or_default(),
        card_id: &card.id,
        entry_id: card.entry_id.as_deref(),
        expires_at: now.saturating_add(LIFETIME_MS),
    };
    match hub.store.session_grant_add(&new, now) {
        Ok(g) => {
            mark(hub, &g);
            // The retry the tap lets through is the grant's first call.
            let input: Value = serde_json::from_str(&card.input).unwrap_or(Value::Null);
            hit(&g, &card.tool, &input);
        }
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), card = card.id, "couldn't save a session grant");
            once_only(hub, card, "it couldn't be saved");
        }
    }
}

/// A This session tap that couldn't be kept: the run still got its one yes, and the card says what happened.
fn once_only(hub: &Hub, card: &Card, why: &str) {
    let Some(id) = &card.entry_id else { return };
    let Some(mut e) = hub.store.entry(id) else { return };
    e.data["outcome"] = format!("Allowed once only: not kept for the session ({why})").into();
    hub.set_entry(id, &e.data);
}

/// The live grant of this run's conversation that covers exactly `derived`, if any. Ends, on the way, the grants this
/// block shows are over: another Claude session's (a new task), past 24 h, or made under other settings.
pub fn granted(
    hub: &Hub,
    who: &RunIdentity,
    claude_session: &str,
    derived: &Derived,
    settings: &str,
) -> Option<SessionGrant> {
    let now = now_ms();
    let mut found = None;
    for g in hub.store.session_grants_live(&who.agent, &who.session) {
        let over = if g.expires_at <= now {
            Some("expired")
        } else if g.claude_session != claude_session {
            Some("task")
        } else if g.settings != settings {
            Some("settings")
        } else {
            None
        };
        if let Some(reason) = over {
            end(hub, &g.id, "ended", reason, now);
        } else if found.is_none() && g.kind == derived.kind && g.rule == derived.rule {
            found = Some(g);
        }
    }
    found
}

/// `removeSessionGrant {id}` (the owner): the grant ends now; the next block of that action asks again. A second call
/// answers "not found".
pub fn remove(hub: &Hub, id: &str) -> anyhow::Result<Value> {
    let user = |m: &str| anyhow::Error::from(crate::api::UserError(m.into()));
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return Err(user("That grant can't be removed"));
    }
    if end(hub, id, "removed", "removed", now_ms()).is_none() {
        return Err(user("Not found: it may already have ended"));
    }
    Ok(json!({"ok": true}))
}

/// Grants past their 24 h end, and their cards say so.
pub fn sweep(hub: &Hub, now: i64) {
    match hub.store.session_grants_sweep(now) {
        Ok(ended) => ended.iter().for_each(|g| mark(hub, g)),
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't end session grants"),
    }
}

fn end(hub: &Hub, id: &str, state: &str, reason: &str, now: i64) -> Option<SessionGrant> {
    match hub.store.session_grant_end(id, state, reason, now) {
        Ok(Some(g)) => {
            mark(hub, &g);
            Some(g)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), grant = id, "couldn't end a session grant");
            None
        }
    }
}

/// What an entry carries about the grant.
fn badge(g: &SessionGrant) -> Value {
    let mut v = json!({"id": g.id, "state": g.state});
    if let Some(at) = g.ended_at {
        v["endedAt"] = at.into();
    }
    if let Some(r) = &g.end_reason {
        v["endReason"] = r.as_str().into();
    }
    v
}

/// The card's outcome line in words, for an app that doesn't read `sessionGrant` (the Mac builds its own, with the time).
fn outcome(g: &SessionGrant) -> String {
    match (g.state.as_str(), g.end_reason.as_deref()) {
        ("live", _) => "Allowed this session".into(),
        ("removed", _) => "Allowed this session · removed".into(),
        (_, Some("task")) => "Allowed this session · ended with the task".into(),
        (_, Some("expired")) => "Allowed this session · ended after 24 h".into(),
        (_, Some("settings")) => "Allowed this session · ended, settings changed".into(),
        _ => "Allowed this session · ended".into(),
    }
}

/// Writes the grant's state on its card and on the tool entries it let through.
fn mark(hub: &Hub, g: &SessionGrant) {
    if let Some(id) = &g.entry_id
        && let Some(mut e) = hub.store.entry(id)
    {
        e.data["sessionGrant"] = badge(g);
        e.data["outcome"] = outcome(g).into();
        hub.set_entry(id, &e.data);
    }
    for id in &g.tools {
        if let Some(mut e) = hub.store.entry(id) {
            e.data["sessionGrant"] = json!({"id": g.id, "state": g.state});
            hub.set_entry(id, &e.data);
        }
    }
}

// ---- the "this session" tag on the tool rows a grant let through

/// A call a grant let through, waiting for its tool entry.
struct Hit {
    grant: String,
    agent: String,
    /// The conversation the run belongs to: two owner threads of one agent can run the same command.
    run_session: String,
    key: String,
    at: i64,
}

static HITS: LazyLock<Mutex<Vec<Hit>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// What identifies one call across its hook and its transcript entry: the tool's input without a Bash description
/// (the model's own words, as kith-permit's one-shot leaves it out).
fn call_key(input: &Value) -> String {
    let mut v = input.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("description");
    }
    v.to_string()
}

/// The grant let `tool` with `input` through: its tool entry, when it comes, gets the tag.
pub fn hit(g: &SessionGrant, _tool: &str, input: &Value) {
    let now = now_ms();
    let mut hits = HITS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    hits.retain(|h| now - h.at < TAG_WINDOW_MS);
    hits.push(Hit {
        grant: g.id.clone(),
        agent: g.agent.clone(),
        run_session: g.run_session.clone(),
        key: call_key(input),
        at: now,
    });
}

/// A tool entry of `agent`'s chat is being added (`raw` is the call's input as the agent's adapter reports it): when a
/// grant let that call through a moment ago, the entry's data gets the grant's tag and the grant notes the entry. The
/// returned grant id is for [`tagged`], once the entry has an id.
pub fn tag_tool(hub: &Hub, agent: &str, run_session: &str, raw: &Value, data: &mut Value) -> Option<String> {
    if !raw.is_object() {
        return None;
    }
    let key = call_key(raw);
    let now = now_ms();
    let grant = {
        let mut hits = HITS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The newest: the row follows the call the grant let through last.
        let at = hits.iter().rposition(|h| h.agent == agent && h.run_session == run_session && h.key == key && now - h.at < TAG_WINDOW_MS)?;
        hits.remove(at).grant
    };
    let g = hub.store.session_grant(&grant)?;
    data["sessionGrant"] = json!({"id": g.id, "state": g.state});
    Some(g.id)
}

/// The tagged entry now has an id: the grant keeps it, so a removal or an end reaches the row too.
pub fn tagged(hub: &Hub, grant: &str, entry_id: &str) {
    if let Err(error) = hub.store.session_grant_tag(grant, entry_id) {
        tracing::warn!(error = format!("{error:#}"), grant, "couldn't note a session grant's tool entry");
    }
}
