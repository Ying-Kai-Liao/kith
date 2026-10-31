//! The owner's card for a call that Claude Code's auto-mode classifier blocked in their own run, and "Always allow".
//!
//! kith-permit's `denied` hook sends the call, with the classifier's reason, as a `requestPermission` that carries
//! `autoMode`; only the owner's run token may (runauth.rs). The card shows the agent, the action and the reason, and offers
//! Allow once, Always allow and Deny. A yes lets the run retry that one call (kith-permit records a one-shot for its
//! `PreToolUse` hook). Always allow also remembers it for the agent through `kith allow` (`lib/kith_allowed.py` decides what may
//! be remembered and as what: the host only asks, then saves what the card showed). What the card offered is kept on its
//! entry (`autoAllow`), so the answer saves that and nothing else, as `edge` does for a guest's edge asks.
//!
//! No process waits on a late answer: a yes after the run ended or stopped waiting does not resume it (the call that was
//! blocked is gone), but an Always allow still saves its rule for the next run.

use super::{Card, Hub, edge};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The option that remembers the call. Not `edge::ALWAYS`: that one writes a guest's grant through kith-auth.
pub const ALWAYS: &str = "kith-auto-always";
/// A card of this kind has its own resource key, so it never joins or uses a grant of an ordinary approval card.
const PREFIX: &str = "auto-mode:";
const MAX_REASON: usize = 300;

pub fn resource_of(key: &str) -> String {
    format!("{PREFIX}{key}")
}

pub fn is_auto(card: &Card) -> bool {
    card.resource.starts_with(PREFIX)
}

/// The classifier's reason as one short line for a card: no control characters, cut to a length.
pub fn clean_reason(reason: &str) -> String {
    reason.chars().filter(|c| !c.is_control()).take(MAX_REASON).collect::<String>().trim().to_owned()
}

/// What "Always allow" would remember: the kind (`rule` or `prose`) and the rule text, as `kith allow --derive` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub kind: String,
    pub rule: String,
}

fn allow_bin(auth: &std::path::Path) -> PathBuf {
    auth.with_file_name("kith-allow")
}

async fn run(agent: &str, flag: &str, body: &Value) -> Option<std::process::Output> {
    let bin = allow_bin(&crate::remote::account::bin().await?);
    crate::channels::hq::output_with(&bin, &[agent, flag], Some(&body.to_string())).await
}

/// `kith allow <agent> --derive`: None when the kernel refuses to remember the call (or can't be reached).
pub async fn derive(agent: &str, tool: &str, input: &Value) -> Option<Derived> {
    let out = run(agent, "--derive", &json!({"tool": tool, "input": input})).await?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    if v["ok"] != true {
        return None;
    }
    Some(Derived { kind: v["kind"].as_str()?.to_owned(), rule: v["rule"].as_str()?.to_owned() })
}

/// What the owner's card adds: the classifier's reason (and what Always allow would remember) as a line of text, the
/// Always allow option when the kernel will remember this call, and what it offered, kept on the entry.
pub async fn extras(agent: &str, tool: &str, input: &Value, reason: &str) -> edge::Extra {
    let reason = clean_reason(reason);
    let mut text = format!("Auto mode blocked this: {reason}");
    let mut options = Vec::new();
    let mut kept = json!({"kind": "auto", "reason": reason});
    if let Some(d) = derive(agent, tool, input).await {
        let shown = if d.kind == "rule" { d.rule.clone() } else { format!("this exact action, for {agent}") };
        text = format!("{text}\nAlways allow remembers {shown}.");
        options.push(json!({"optionId": ALWAYS, "name": "Always allow", "kind": "allow_always"}));
        kept["rule"] = d.rule.into();
        kept["ruleKind"] = d.kind.into();
    }
    (text, options, kept)
}

/// Always allow: saves what the card offered. Derives again from the card's own tool and input and refuses to save
/// anything but the offered rule. `Err` says why it wasn't kept.
pub async fn save(card: &Card, kept: &Value) -> Result<String, String> {
    let offered = kept["rule"].as_str().ok_or("this card offered nothing to remember")?;
    let input: Value = serde_json::from_str(&card.input).unwrap_or(Value::Null);
    let body = json!({"tool": card.tool, "input": input, "reason": kept["reason"]});
    let out = run(&card.agent, "--add-json", &body).await.ok_or("kith allow can't be reached")?;
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if !out.status.success() {
        return Err(v["why"]
            .as_str()
            .map_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_owned(), str::to_owned));
    }
    if v["rule"].as_str() != Some(offered) {
        return Err("it is no longer what the card offered".into());
    }
    Ok(offered.to_owned())
}

/// The card's outcome line after Always allow: what was remembered, for whom.
pub fn remembered(hub: &Hub, card: &Card, rule: &str, kind: &str) {
    let Some(id) = &card.entry_id else { return };
    let Some(mut e) = hub.store.entry(id) else { return };
    let what = if kind == "rule" { rule.to_owned() } else { "this exact action".to_owned() };
    e.data["outcome"] = format!("Allowed, and remembered for {}: {what}", card.agent).into();
    hub.set_entry(id, &e.data);
}
