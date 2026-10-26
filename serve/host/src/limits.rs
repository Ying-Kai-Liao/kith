//! What the agents cost at API prices, and their monthly limits: the apps' Usage screens read the kernel's ledger
//! (`kith cost usage|agent|members --json`) and set limits through it (`kith AGENT set limit N`). The kernel holds the
//! numbers; this module only asks it.
//!
//! An owner's message to an agent over its limit is never refused: with the agent's "at the limit" set to ask (the
//! default) or hold, the message parks under a limit card (Allow this run / Raise / Stop; hold has no Allow) and runs
//! once answered; with tell, a line says so once a month and the run goes on. A guest's run stops at their own budget
//! as before (the kernel refuses it); the owner then gets a card in that chat to add to it.

use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::agent::hq;
use crate::hub::Hub;
use crate::remote::push::{self, AlertKind};
use crate::store::{BotConfig, Entry, EntryKind, Lane};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// A ledger read can sync first: a busy month takes a few seconds.
const READ: Duration = Duration::from_secs(60);
/// The check before an owner's turn: a slow ledger lets the run go (a limit never stops the owner by failing).
const CHECK: Duration = Duration::from_secs(20);

/// The month each agent's "tell me" line was last written, so it shows once a month, not on every turn.
static TOLD: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// An agent name the kernel could have made; nothing else reaches its command line.
pub fn valid_agent(agent: &str) -> bool {
    agent.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && agent.len() <= 64
        && agent.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn period_of(b: &Value) -> Result<&str> {
    match b["period"].as_str().unwrap_or("month") {
        p @ ("today" | "week" | "month") => Ok(p),
        other => bail!("`period` is today, week or month, not {other}"),
    }
}

fn agent_of(b: &Value) -> Result<&str> {
    let agent = b["agent"].as_str().ok_or_else(|| anyhow!("`agent` is required"))?;
    if !valid_agent(agent) {
        bail!("`{agent}` isn't an agent name");
    }
    Ok(agent)
}

async fn kernel() -> Result<std::path::PathBuf> {
    tokio::task::spawn_blocking(hq::bin)
        .await?
        .ok_or_else(|| anyhow!("kith isn't installed on this computer (set KITH_KERNEL_BIN, or put `kith` on PATH)"))
}

/// `kith args…`, its stdout as JSON.
async fn kith_json(args: &[&str], budget: Duration) -> Result<Value> {
    let out = hq::run(&kernel().await?, args, None, budget).await?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("kith {} gave something that isn't JSON: {e}", args.join(" ")))
}

/// The owner's API (owner-only: none of these are in `roles::MEMBER_METHODS`).
pub async fn call(method: &str, b: &Value) -> Result<Option<Value>> {
    Ok(Some(match method {
        // Profile > Usage: the period's total at API prices, agents vs your own sessions, a row per agent.
        "usageReport" => kith_json(&["cost", "usage", period_of(b)?, "--json"], READ).await?,
        // One agent: its total, days, forecast, models and costliest runs.
        "agentUsage" => kith_json(&["cost", "agent", agent_of(b)?, period_of(b)?, "--json"], READ).await?,
        // The limit sheet: `limit` dollars a month (null forgets it), `at` ask | tell | hold; answers the agent view.
        "setAgentLimit" => {
            let agent = agent_of(b)?;
            let bin = kernel().await?;
            if let Some(limit) = b.get("limit") {
                let value = match limit {
                    Value::Null => "none".to_owned(),
                    Value::Number(n) if n.as_f64().is_some_and(|v| v > 0.0 && v < 1e7) => n.to_string(),
                    _ => bail!("`limit` is dollars a month, or null for no limit"),
                };
                hq::run(&bin, &[agent, "set", "limit", &value], None, READ).await?;
            }
            if let Some(at) = b["at"].as_str() {
                if !matches!(at, "ask" | "tell" | "hold") {
                    bail!("`at` is ask, tell or hold");
                }
                hq::run(&bin, &[agent, "set", "at-limit", at], None, READ).await?;
            }
            let period = b["period"].as_str().map_or(Ok("month"), |_| period_of(b))?;
            kith_json(&["cost", "agent", agent, period, "--json"], READ).await?
        }
        // Access > People: what each person spent on the agent this month, and their budget on it.
        "agentPeople" => kith_json(&["cost", "members", agent_of(b)?, "--json"], READ).await?,
        // A person's budget on an agent (`budget` dollars a month, null for none), as `kith-auth member A P budget=`;
        // answers agentPeople.
        "setPersonBudget" => {
            let agent = agent_of(b)?;
            let person =
                b["person"].as_str().filter(|p| valid_agent(p)).ok_or_else(|| anyhow!("`person` is a person id"))?;
            let budget = match &b["budget"] {
                Value::Null => String::new(),
                Value::Number(n) if n.as_f64().is_some_and(|v| v > 0.0 && v < 1e7) => n.to_string(),
                _ => bail!("`budget` is dollars a month, or null for none"),
            };
            let bin = crate::remote::account::bin().await.ok_or_else(|| anyhow!("kith-auth wasn't found"))?;
            {
                let _w = crate::api::people::AUTH_WRITES.lock().await;
                crate::remote::account::run(&bin, &["member", agent, person, &format!("budget={budget}")]).await?;
            }
            kith_json(&["cost", "members", agent, "--json"], READ).await?
        }
        _ => return Ok(None),
    }))
}

/// Where an agent stands against its limit when an owner's turn would start; None when it has none, is under it,
/// or the ledger can't say (the run goes on).
pub async fn over(agent: &str) -> Option<Value> {
    if !valid_agent(agent) {
        return None;
    }
    let state = kith_json(&["cost", "limit", agent, "--json"], CHECK)
        .await
        .inspect_err(|e| tracing::warn!(agent, error = format!("{e:#}"), "couldn't read the agent's limit; running"))
        .ok()?;
    (state["over"] == true).then_some(state)
}

/// Whether the "tell me" line for `agent` is due (once a month); marks it told.
pub fn tell_due(agent: &str, month: &str) -> bool {
    let mut told = TOLD.locked();
    let told = told.get_or_insert_with(HashMap::new);
    if told.get(agent).is_some_and(|m| m == month) {
        return false;
    }
    told.insert(agent.to_owned(), month.to_owned());
    true
}

fn money(x: f64) -> String {
    if x >= 10.0 && x.fract() == 0.0 { format!("${x:.0}") } else { format!("${x:.2}") }
}

fn dollars(v: &Value) -> f64 {
    v.as_f64().unwrap_or_default()
}

/// The open limit card in `lane`, if any (a second message joins it rather than making another).
fn open_card(hub: &Hub, lane: &Lane, kind: &str) -> Option<Entry> {
    hub.store.pending_limit_card(&lane.chat, lane.thread.as_deref(), kind)
}

fn add_ids(data: &mut Value, ids: &[String]) {
    let mut all = data["limit"]["entryIds"].as_array().cloned().unwrap_or_default();
    for id in ids {
        if !all.iter().any(|v| v == id.as_str()) {
            all.push(json!(id));
        }
    }
    data["limit"]["entryIds"] = Value::Array(all);
}

/// The card's words and answers for an agent at its limit (`state`, from `kith cost limit --json`).
pub fn agent_card(bot: &str, state: &Value, ids: &[String]) -> Value {
    let agent = state["agent"].as_str().unwrap_or_default();
    let (limit, month) = (dollars(&state["limit"]), state["month"].as_str().unwrap_or("this month"));
    let hold = state["at"] == "hold";
    let raise = dollars(&state["raiseTo"]);
    let title = if hold {
        format!(
            "{agent} has used its {} for {month}. Its runs wait until next month or until you raise it.",
            money(limit)
        )
    } else {
        format!("This run is waiting: {agent} has used its {} for {month}.", money(limit))
    };
    let mut options = Vec::new();
    if !hold {
        options.push(json!({"optionId": "allow", "name": "Allow this run", "kind": "limit"}));
    }
    options.push(json!({"optionId": "raise", "name": format!("Raise to {}", money(raise)), "kind": "limit"}));
    options.push(json!({"optionId": "stop", "name": "Stop", "kind": "limit"}));
    json!({
        "author": bot, "agent": agent, "title": title, "toolKind": "limit", "status": "pending", "options": options,
        "limit": {
            "kind": "agent", "agent": agent, "spent": state["spent"], "limit": state["limit"], "at": state["at"],
            "perRun": state["perRun"], "runs": state["runs"], "month": month, "raiseTo": state["raiseTo"],
            "resets": state["resets"], "prices": "api", "entryIds": ids,
        },
    })
}

/// Parks an owner's messages (`ids`, in `lane` of `bot`) under a limit card; a card already open there takes them.
pub fn park(hub: &Hub, bot: &BotConfig, lane: &Lane, ids: &[String], state: &Value) {
    for id in ids {
        if let Some(mut e) = hub.store.entry(id) {
            e.data["status"] = "held".into();
            hub.set_entry(id, &e.data);
        }
    }
    if let Some(mut card) = open_card(hub, lane, "agent") {
        add_ids(&mut card.data, ids);
        hub.set_entry(&card.id, &card.data);
        return;
    }
    let data = agent_card(&bot.id, state, ids);
    if hub.add_entry(lane, EntryKind::Permission, hub.store.max_turn(&lane.chat), &data).is_none() {
        tracing::warn!(bot = %bot.id, "couldn't save the limit card");
        return;
    }
    hub.set_task_status(lane, crate::chat::tasks::TaskState::NeedsYou);
    let target = hub.store.bot(&lane.chat).ok().flatten().map_or_else(|| bot.clone(), |r| r.config);
    let body = data["title"].as_str().unwrap_or_default();
    push::notify(hub, &target, Some(&bot.id), &format!("{} needs you", bot.name), body, AlertKind::NeedsInput);
}

/// A guest's run the kernel refused at their budget (`refused`, its text): the owner gets a card in that chat to add
/// to the budget, which runs the message again. Reads the month's numbers first, so it runs on its own task.
pub fn guest_refused(hub: std::sync::Arc<Hub>, bot: BotConfig, lane: Lane, ids: Vec<String>, refused: &str) {
    let (Some(person), Some(agent)) = (bot.person.clone(), bot.hq_agent.clone()) else { return };
    if !refused.contains("has used $") || !valid_agent(&agent) {
        return;
    }
    tokio::spawn(async move {
        let people = match kith_json(&["cost", "members", &agent, "--json"], READ).await {
            Ok(v) => v,
            Err(e) => return tracing::warn!(agent, error = format!("{e:#}"), "couldn't read the budgets for the card"),
        };
        let budget = dollars(&people["budgets"][&person]);
        if budget <= 0.0 {
            return;
        }
        let spent = dollars(&people["people"][&person]);
        let name = hub.store.person(&person).map_or_else(|| person.clone(), |p| p.name);
        let card = guest_card(&bot.id, &agent, &person, &name, spent, budget, people["month"].as_str(), &ids);
        if let Some(mut open) = open_card(&hub, &lane, "person") {
            add_ids(&mut open.data, &ids);
            hub.set_entry(&open.id, &open.data);
            return;
        }
        if hub.add_entry(&lane, EntryKind::Permission, hub.store.max_turn(&lane.chat), &card).is_none() {
            return;
        }
        let mut owner = bot.clone();
        owner.person = None;
        let body = card["title"].as_str().unwrap_or_default();
        push::notify(&hub, &owner, Some(&bot.id), &format!("{name} reached their limit"), body, AlertKind::NeedsInput);
    });
}

#[allow(clippy::too_many_arguments)]
pub fn guest_card(
    bot: &str,
    agent: &str,
    person: &str,
    name: &str,
    spent: f64,
    budget: f64,
    month: Option<&str>,
    ids: &[String],
) -> Value {
    let month = month.unwrap_or("this month");
    let (add, raise) = (budget + 10.0, budget * 2.0);
    json!({
        "author": bot, "agent": agent, "approver": "owner", "person": person, "personName": name,
        "title": format!("{name} reached their {} for {month}. Their last message wasn't run.", money(budget)),
        "toolKind": "limit", "status": "pending",
        "options": [
            {"optionId": "add", "name": format!("Add {}", money(10.0)), "kind": "limit"},
            {"optionId": "raise", "name": format!("Raise to {}", money(raise)), "kind": "limit"},
            {"optionId": "leave", "name": "Leave it", "kind": "limit"},
        ],
        "limit": {
            "kind": "person", "agent": agent, "person": person, "spent": spent, "limit": budget, "month": month,
            "addTo": add, "raiseTo": raise, "prices": "api", "entryIds": ids,
        },
    })
}

/// The owner's answer on a limit card (`respondPermission`): allow runs the held messages, raise sets the limit (or
/// the person's budget) and runs them, stop or leave keeps them unrun. Only the owner answers one.
pub async fn respond(hub: &Hub, entry: &Entry, option: Option<&str>, by: &str) -> Result<Value> {
    if by != "owner" {
        bail!("Only the owner answers a limit card.");
    }
    if entry.data["status"] != "pending" {
        return Ok(json!({"closed": true}));
    }
    let l = &entry.data["limit"];
    let agent = l["agent"].as_str().filter(|a| valid_agent(a)).ok_or_else(|| anyhow!("the card has no agent"))?;
    let bot = entry.data["author"].as_str().unwrap_or(&entry.bot_id).to_owned();
    let ids: Vec<String> =
        l["entryIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let option = option.unwrap_or("stop");
    let run = match (l["kind"].as_str(), option) {
        (Some("agent"), "allow") if l["at"] != "hold" => {
            for id in &ids {
                if let Some(mut e) = hub.store.entry(id) {
                    e.data["limitAllowed"] = true.into();
                    hub.set_entry(id, &e.data);
                }
            }
            true
        }
        (Some("agent"), "raise") => {
            let to = dollars(&l["raiseTo"]);
            if to <= 0.0 {
                bail!("the card has no amount to raise to");
            }
            hq::run(&kernel().await?, &[agent, "set", "limit", &to.to_string()], None, READ).await?;
            true
        }
        (Some("person"), "add" | "raise") => {
            let person = l["person"].as_str().ok_or_else(|| anyhow!("the card has no person"))?;
            let to = dollars(if option == "add" { &l["addTo"] } else { &l["raiseTo"] });
            let bin = crate::remote::account::bin().await.ok_or_else(|| anyhow!("kith-auth wasn't found"))?;
            let budget = format!("budget={to}");
            let _w = crate::api::people::AUTH_WRITES.lock().await;
            crate::remote::account::run(&bin, &["member", agent, person, &budget]).await?;
            true
        }
        (_, "stop" | "leave") => false,
        _ => bail!("That isn't one of the choices on this card."),
    };
    let mut data = entry.data.clone();
    data["status"] = "answered".into();
    data["selected"] = option.into();
    hub.set_entry(&entry.id, &data);
    for id in &ids {
        let Some(mut e) = hub.store.entry(id) else { continue };
        if !run {
            e.data["status"] = "cancelled".into();
            hub.set_entry(id, &e.data);
            continue;
        }
        resend(hub, &bot, e)?;
    }
    Ok(json!({}))
}

/// Sends a held message to its bot again (as the sign-in card does).
fn resend(hub: &Hub, bot: &str, mut entry: Entry) -> Result<()> {
    let config = hub.store.bot(bot)?.filter(|b| !b.deleted).ok_or_else(|| anyhow!("the agent is gone"))?.config;
    let attachments = entry.data["attachments"].as_array().cloned().unwrap_or_default();
    let text = entry.data["text"].as_str().unwrap_or_default();
    let call = entry.data["via"] == "call";
    let prompt = crate::chat::prompt::of_message(&hub.store, &config, text, &attachments, call);
    entry.data["status"] = "queued".into();
    hub.set_entry(&entry.id, &entry.data);
    let lane = Lane { chat: entry.bot_id.clone(), thread: entry.thread_id.clone() };
    hub.send_cmd(bot, Cmd::Send { lane, entry_id: entry.id, text: prompt, route: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_reach_the_kernel_only_when_plain() {
        assert!(valid_agent("architect"));
        assert!(valid_agent("lab-2"));
        assert!(!valid_agent("-x"));
        assert!(!valid_agent("a b"));
        assert!(!valid_agent("../etc"));
        assert!(!valid_agent(""));
    }

    #[test]
    fn the_card_offers_allow_raise_stop_and_hold_drops_allow() {
        let state = json!({"agent": "architect", "limit": 300, "at": "ask", "spent": 300.4, "perRun": 2.92,
            "runs": 99, "month": "October", "raiseTo": 400, "resets": "2026-11-01"});
        let card = agent_card("bot1", &state, &["e1".into()]);
        assert_eq!(card["title"], "This run is waiting: architect has used its $300 for October.");
        let names: Vec<&str> = card["options"].as_array().unwrap().iter().filter_map(|o| o["name"].as_str()).collect();
        assert_eq!(names, ["Allow this run", "Raise to $400", "Stop"]);
        assert_eq!(card["limit"]["entryIds"], json!(["e1"]));
        let mut held = state.clone();
        held["at"] = "hold".into();
        let card = agent_card("bot1", &held, &[]);
        let ids: Vec<&str> =
            card["options"].as_array().unwrap().iter().filter_map(|o| o["optionId"].as_str()).collect();
        assert_eq!(ids, ["raise", "stop"]);
    }

    #[test]
    fn a_guest_card_waits_for_the_owner() {
        let card = guest_card("g1", "video", "mia", "Mia", 20.1, 20.0, Some("October"), &["e1".into()]);
        assert_eq!(card["approver"], "owner");
        assert_eq!(card["title"], "Mia reached their $20 for October. Their last message wasn't run.");
        let names: Vec<&str> = card["options"].as_array().unwrap().iter().filter_map(|o| o["name"].as_str()).collect();
        assert_eq!(names, ["Add $10", "Raise to $40", "Leave it"]);
    }

    #[test]
    fn tell_is_once_a_month() {
        assert!(tell_due("tell-test", "October"));
        assert!(!tell_due("tell-test", "October"));
        assert!(tell_due("tell-test", "November"));
    }
}
