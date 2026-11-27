//! The bottom box with no tag (phase2-feed, section 7): which of the owner's agents a text fits best, from
//! `kith who <text> --json -n 3`. Read-only. Anything that goes wrong answers the home agent, `main`, so the box
//! never blocks on this.

use crate::hqmem::{Layout, tidy};
use serde_json::{Value, json};
use std::time::Duration;

const HOME: &str = "main";
const TIMEOUT: Duration = Duration::from_secs(8);

pub async fn route(text: &str) -> Value {
    let layout = Layout::from_env();
    let out = ask(text).await;
    pick(out.as_deref(), |agent| tidy::agent_dir(&layout, agent).is_some())
}

/// `kith who`'s answer for `text`: `None` when there is no kith to ask, it fails or it is too slow.
async fn ask(text: &str) -> Option<Vec<u8>> {
    let bin = tokio::task::spawn_blocking(crate::agent::hq::bin).await.ok()??;
    let args = ["who", text, "--json", "-n", "3"];
    let run = crate::channels::hq::output(&bin, &args);
    let out = tokio::time::timeout(TIMEOUT, run).await.ok()??;
    out.status.success().then_some(out.stdout)
}

/// The answer from `kith who`'s JSON, keeping the agents `has` says the owner has.
pub(crate) fn pick(json_out: Option<&[u8]>, has: impl Fn(&str) -> bool) -> Value {
    let rows: Vec<Value> = json_out.and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default();
    let mut found = rows.iter().filter_map(|r| Some((r["agent"].as_str()?, r))).filter(|(agent, _)| has(agent));
    let Some((agent, top)) = found.next() else { return json!({"agent": HOME, "why": "", "others": []}) };
    let why = top["matched"][0]["text"].as_str().unwrap_or_default();
    let others: Vec<&str> = found.map(|(agent, _)| agent).collect();
    json!({"agent": agent, "why": why, "others": others})
}

#[cfg(test)]
mod tests;
