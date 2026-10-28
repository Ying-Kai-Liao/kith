//! Durable bot routines, shared by API clients and the built-in MCP server.
mod editor;
pub use editor::preview as schedule_preview;
pub mod hooks;
use hooks::Refused;
mod runner;
mod schedule;
pub use runner::start;
pub use schedule::Trigger;

use crate::{
    LockExt,
    hub::Hub,
    store::{EntryKind, Lane, Store, now_ms},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

const STATE_KEY: &str = "routines.v1";
const MAX_FINISHED_RUNS: usize = 1000;
fn default_timeout() -> u64 {
    3600
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: String,
    pub bot_id: String,
    pub name: String,
    pub instruction: String,
    pub triggers: Vec<Trigger>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub next_runs: Vec<Option<i64>>,
    pub webhook_key: String,
    pub deleted: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Pending,
    Starting,
    Running,
    Recovering,
    Succeeded,
    Failed,
    Interrupted,
    Cancelled,
}

impl Status {
    fn active(self) -> bool {
        matches!(self, Self::Pending | Self::Starting | Self::Running | Self::Recovering)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub routine_id: String,
    pub bot_id: String,
    pub status: Status,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub detail: Option<String>,
    pub root_id: Option<String>,
    pub event: Value,
    pub delivery_id: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    report_pending: bool,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    routines: Vec<Routine>,
    runs: Vec<Run>,
}

pub struct Routines {
    state: Mutex<State>,
    active: Mutex<HashSet<String>>,
    scheduler: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stopping: std::sync::atomic::AtomicBool,
    /// Bumped whenever the webhook set the relay registers (`hooks()`) changes.
    hooks_rev: tokio::sync::watch::Sender<u64>,
}

impl Default for Routines {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            active: Mutex::default(),
            scheduler: Mutex::default(),
            stopping: std::sync::atomic::AtomicBool::default(),
            hooks_rev: tokio::sync::watch::channel(0).0,
        }
    }
}

/// One routine's public webhook as the cloud stores it.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Hook {
    pub id: String,
    pub key: String,
    pub enabled: bool,
}

fn hook_set(state: &State) -> Vec<Hook> {
    state
        .routines
        .iter()
        .filter(|r| !r.deleted && r.triggers.iter().any(|t| matches!(t, Trigger::Webhook | Trigger::Event { .. })))
        .map(|r| Hook { id: r.id.clone(), key: r.webhook_key.clone(), enabled: r.enabled })
        .collect()
}

fn webhook_key() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

impl Routines {
    pub fn restore(&self, store: &Store) -> Result<()> {
        let mut state = match store.kv_read(STATE_KEY)? {
            Some(raw) => serde_json::from_str::<State>(&raw).context("reading routines")?,
            None => State::default(),
        };
        let mut active = self.active.locked();
        active.clear();
        for run in &mut state.runs {
            match run.status {
                Status::Starting => run.status = Status::Pending,
                Status::Running | Status::Recovering => {
                    run.status = Status::Recovering;
                    run.finished_at = None;
                    active.insert(run.bot_id.clone());
                }
                _ => {}
            }
        }
        store.kv_set(STATE_KEY, &serde_json::to_string(&state)?)?;
        *self.state.locked() = state;
        self.hooks_rev.send_modify(|v| *v += 1);
        Ok(())
    }

    /// The webhook routines the relay registers with the cloud.
    pub fn hooks(&self) -> Vec<Hook> {
        hook_set(&self.state.locked())
    }

    pub fn subscribe_hooks(&self) -> tokio::sync::watch::Receiver<u64> {
        self.hooks_rev.subscribe()
    }

    fn change<T>(&self, store: &Store, update: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut guard = self.state.locked();
        let mut next = guard.clone();
        let result = update(&mut next)?;
        let mut kept = 0;
        next.runs.reverse();
        next.runs.retain(|run| {
            if run.status.active() || run.report_pending {
                return true;
            }
            kept += 1;
            kept <= MAX_FINISHED_RUNS
        });
        next.runs.reverse();
        next.routines
            .retain(|r| !r.deleted || next.runs.iter().any(|run| run.routine_id == r.id && run.status.active()));
        store.kv_set(STATE_KEY, &serde_json::to_string(&next)?)?;
        let hooks_changed = hook_set(&guard) != hook_set(&next);
        *guard = next;
        drop(guard);
        if hooks_changed {
            self.hooks_rev.send_modify(|v| *v += 1);
        }
        Ok(result)
    }

    pub fn list(&self, bot: &str) -> Value {
        let state = self.state.locked();
        json!({"routines": state.routines.iter().filter(|r| r.bot_id == bot && !r.deleted).map(public).collect::<Vec<_>>(),
            "runs": state.runs.iter().rev().filter(|r| r.bot_id == bot).take(200).collect::<Vec<_>>()})
    }

    pub fn save(&self, hub: &Hub, bot: &str, body: &Value) -> Result<Value> {
        let row = hub
            .store
            .bot(bot)?
            .filter(|b| !b.deleted && !b.config.is_group())
            .context("routine requires an existing agent bot")?;
        let name = required(body, "name")?.trim();
        let instruction = required(body, "instruction")?.trim();
        if name.is_empty() || name.len() > 200 || instruction.is_empty() || instruction.len() > 32_000 {
            bail!("routine name or instruction is empty or too long");
        }
        let triggers: Vec<Trigger> = if let Some(draft) = body.get("schedule") {
            serde_json::from_value::<editor::ScheduleDraft>(draft.clone()).context("invalid schedule")?.triggers()?
        } else {
            serde_json::from_value(body["triggers"].clone()).context("invalid triggers")?
        };
        if triggers.is_empty() || triggers.len() > 20 {
            bail!("a routine requires 1 to 20 triggers");
        }
        let timeout = body
            .get("timeoutSeconds")
            .map(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str()?.parse::<u64>().ok())
                    .filter(|seconds| (1..=86_400).contains(seconds))
                    .context("timeoutSeconds must be an integer between 1 and 86400")
            })
            .transpose()?;
        let now = now_ms();
        let id = body["id"].as_str().map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
        let updated = body["id"].is_string();
        let routine = self.change(&hub.store, |s| {
            let old = s.routines.iter().find(|r| r.id == id && r.bot_id == bot && !r.deleted).cloned();
            if updated && old.is_none() {
                bail!("routine not found");
            }
            for trigger in &triggers {
                if matches!(trigger, Trigger::Once { .. }) && old.as_ref().is_some_and(|r| r.triggers.contains(trigger))
                {
                    continue;
                }
                trigger.validate(now)?;
            }
            if !updated && s.routines.iter().filter(|r| !r.deleted).count() >= 1000 {
                bail!("routine limit reached");
            }
            let r = Routine {
                id: id.clone(),
                bot_id: row.config.id.clone(),
                name: name.into(),
                instruction: instruction.into(),
                enabled: body["enabled"].as_bool().unwrap_or_else(|| old.as_ref().is_none_or(|r| r.enabled)),
                created_at: old.as_ref().map_or(now, |r| r.created_at),
                updated_at: now,
                next_runs: if let Some(old) = &old
                    && old.triggers == triggers
                    && (body["enabled"] != true || old.enabled)
                {
                    old.next_runs.clone()
                } else {
                    triggers.iter().map(|t| t.next(now)).collect::<Result<_>>()?
                },
                triggers,
                webhook_key: old.as_ref().map_or_else(webhook_key, |r| r.webhook_key.clone()),
                deleted: false,
                timeout_seconds: timeout
                    .unwrap_or_else(|| old.as_ref().map_or_else(default_timeout, |r| r.timeout_seconds)),
                last_error: None,
            };
            if !(1..=86_400).contains(&r.timeout_seconds) {
                bail!("timeoutSeconds must be between 1 and 86400");
            }
            if !r.enabled {
                cancel_pending(s, &id);
            }
            s.routines.retain(|r| r.id != id);
            s.routines.push(r.clone());
            Ok(r)
        })?;
        hub.add_entry(&Lane::main(bot), EntryKind::Notice, 0, &json!({"text":format!("{} routine: {name}", if updated {"Updated"} else {"Created"}), "routineId": id, "style":"info", "richCard": crate::rich_card::routine(&routine, updated, chrono::Local::now().fixed_offset())}));
        Ok(json!({"routine":public(&routine)}))
    }

    pub fn set_enabled(&self, store: &Store, bot: &str, id: &str, enabled: bool) -> Result<Value> {
        self.change(store, |s| {
            let r = s
                .routines
                .iter_mut()
                .find(|r| r.id == id && r.bot_id == bot && !r.deleted)
                .context("routine not found")?;
            if enabled && !r.enabled {
                r.next_runs = r.triggers.iter().map(|t| t.next(now_ms())).collect::<Result<_>>()?;
            }
            r.enabled = enabled;
            r.updated_at = now_ms();
            if !enabled {
                for run in &mut s.runs {
                    if run.routine_id == id && run.status == Status::Pending {
                        cancel_run(run);
                    }
                }
            }
            Ok(json!({"routine":public(r)}))
        })
    }

    pub fn remove(&self, store: &Store, bot: &str, id: &str) -> Result<Value> {
        self.change(store, |s| {
            let r = s
                .routines
                .iter_mut()
                .find(|r| r.id == id && r.bot_id == bot && !r.deleted)
                .context("routine not found")?;
            r.deleted = true;
            r.enabled = false;
            r.webhook_key.clear();
            for run in &mut s.runs {
                if run.routine_id == id && run.status == Status::Pending {
                    run.status = Status::Cancelled;
                    run.finished_at = Some(now_ms());
                }
            }
            Ok(json!({"ok":true}))
        })
    }

    pub fn enqueue(
        &self,
        store: &Store,
        bot: &str,
        id: &str,
        event: Value,
        delivery: Option<String>,
        test: bool,
    ) -> Result<Value> {
        self.change(store, |s| {
            let r =
                s.routines.iter().find(|r| r.id == id && r.bot_id == bot && !r.deleted).context("routine not found")?;
            if !r.enabled && !test {
                bail!(Refused::Paused);
            }
            if let Some(key) = &delivery
                && let Some(run) =
                    s.runs.iter().find(|run| run.routine_id == id && run.delivery_id.as_ref() == Some(key))
            {
                return Ok(json!({"run":run}));
            }
            if s.runs.iter().any(|run| run.routine_id == id && run.status.active()) {
                bail!(Refused::Busy);
            }
            let run = new_run(r, event, delivery);
            let result = json!({"run":run});
            s.runs.push(run);
            Ok(result)
        })
    }

    /// The routine's webhook: the public URL through the Kith cloud (when it's on), the
    /// local one, and the key. `rotate` replaces the key first, cutting off the old one.
    pub fn credentials(&self, hub: &Hub, bot: &str, id: &str, rotate: bool) -> Result<Value> {
        if rotate {
            self.change(&hub.store, |s| {
                let r = s
                    .routines
                    .iter_mut()
                    .find(|r| r.id == id && r.bot_id == bot && !r.deleted)
                    .context("routine not found")?;
                r.webhook_key = webhook_key();
                r.updated_at = now_ms();
                Ok(())
            })?;
        }
        let key = {
            let s = self.state.locked();
            s.routines
                .iter()
                .find(|r| r.id == id && r.bot_id == bot && !r.deleted)
                .context("routine not found")?
                .webhook_key
                .clone()
        };
        let cloud = hub.cloud.status();
        let url = crate::remote::cloud::url(&hub.store)
            .map(|base| format!("{}/v1/hooks/{}/{id}", base.trim_end_matches('/'), hub.identity.computer_id()));
        Ok(json!({
            "url": url,
            "localUrl": format!("http://127.0.0.1:{}/hooks/routines/{id}", hub.port),
            "key": key,
            "connected": url.is_some() && cloud.connected,
        }))
    }

    /// One delivery from the local endpoint or the cloud: checks the key or signature, then
    /// queues a run when the event matches. Retries of a delivery id return its first run.
    pub fn receive(&self, hub: &Hub, id: &str, headers: &hooks::Headers, body: &[u8]) -> Result<Value, Refused> {
        let r = self.state.locked().routines.iter().find(|r| r.id == id && !r.deleted).cloned();
        let Some(r) = r.filter(|r| hooks::authorized(&r.webhook_key, headers, body)) else {
            return Err(Refused::Unauthorized);
        };
        let delivery = hooks::delivery_id(headers)?;
        let Some(event) = hooks::event(headers, body)? else {
            return Ok(json!({"ping": true}));
        };
        if !r.enabled {
            return Err(Refused::Paused);
        }
        if !r.triggers.iter().any(|t| matches!(t, Trigger::Webhook) || t.matches(&event)) {
            return Ok(json!({"ignored": "event does not match this routine"}));
        }
        self.enqueue(&hub.store, &r.bot_id, id, event, delivery, false).map_err(|e| match e.downcast::<Refused>() {
            Ok(refused) => refused,
            Err(e) => Refused::Invalid(format!("{e:#}")),
        })
    }
}

fn required<'a>(body: &'a Value, key: &str) -> Result<&'a str> {
    body[key].as_str().with_context(|| format!("{key} is required"))
}
fn public(r: &Routine) -> Value {
    json!({"id":r.id,"botId":r.bot_id,"name":r.name,"instruction":r.instruction,"triggers":r.triggers,"enabled":r.enabled,"createdAt":r.created_at,"updatedAt":r.updated_at,
        "timeoutSeconds":r.timeout_seconds,"lastError":r.last_error,
        "triggerDescriptions":r.triggers.iter().map(Trigger::description).collect::<Vec<_>>(),"nextRunAt": if r.enabled {r.next_runs.iter().flatten().min().copied()} else {None}})
}
fn new_run(r: &Routine, event: Value, delivery_id: Option<String>) -> Run {
    Run {
        id: uuid::Uuid::new_v4().to_string(),
        routine_id: r.id.clone(),
        bot_id: r.bot_id.clone(),
        status: Status::Pending,
        created_at: now_ms(),
        finished_at: None,
        detail: None,
        root_id: None,
        event,
        delivery_id,
        started_at: None,
        report_pending: false,
        result: None,
    }
}

fn cancel_run(run: &mut Run) {
    run.status = Status::Cancelled;
    run.finished_at = Some(now_ms());
    run.event = Value::Null;
}
fn cancel_pending(state: &mut State, id: &str) {
    for run in &mut state.runs {
        if run.routine_id == id && run.status == Status::Pending {
            cancel_run(run);
        }
    }
}

pub fn call(hub: &Arc<Hub>, bot: &str, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_routines" => Ok(hub.routines.list(bot)),
        "save_routine" => hub.routines.save(hub, bot, args),
        "set_routine_enabled" => hub.routines.set_enabled(
            &hub.store,
            bot,
            required(args, "id")?,
            args["enabled"].as_bool().context("enabled is required")?,
        ),
        "delete_routine" => hub.routines.remove(&hub.store, bot, required(args, "id")?),
        "run_routine" => {
            hub.routines.enqueue(&hub.store, bot, required(args, "id")?, json!({"source":"test"}), None, true)
        }
        "routine_webhook" => {
            hub.routines.credentials(hub, bot, required(args, "id")?, args["rotate"].as_bool().unwrap_or(false))
        }
        _ => bail!("unknown routine tool"),
    }
}

pub const INSTRUCTIONS: &str = "Create and manage persistent Kith routines when the user asks for scheduled or event-driven work. Use the built-in routines tools (save_routine, list_routines, set_routine_enabled, delete_routine, run_routine, routine_webhook), not operating-system cron or an improvised background process. For recurring clock schedules, always use a five-field cron trigger with an explicit IANA timezone, including minute/hour frequencies; do not create interval triggers for those requests. Preserve existing intervals when editing unrelated fields. Cron steps reset within their field: */7 minutes is not an exact seven-minute duration across hours. Use once for a single dated execution; never approximate a one-time task with an annually repeating cron expression. Before saving, resolve the task, trigger and intended timezone for calendar/one-time schedules from the conversation; ask only for missing or ambiguous details. If the request is already clear, create it without another confirmation. Check that tools, connector sign-ins and files needed by the task are available; describe any missing setup rather than promising the task will work. Use list_routines before creating or editing to avoid duplicates; preserve unrelated triggers, enabled state and instructions. Save returns the actual routine ID, trigger descriptions and nextRunAt: use these to report what was really saved, including timezone, next occurrence and enabled/paused state. Do not claim success if the tool failed. run_routine executes the real task immediately, including its side effects; use it when the user requested a test or immediate execution, not merely to validate a schedule. A routine runs locally in its own conversation; the host must be running and awake, results appear in the main chat, and (pass) stays silent. Webhook triggers: routine_webhook explains how senders deliver; never claim a third-party integration is connected until a real delivery has been verified. Keep secrets out of routine instructions and ordinary status summaries.";

pub fn tools() -> Value {
    let id = json!({"type":"string"});
    let mut tools = vec![
        json!({"name":"list_routines","description":"List this bot's routines and run history.","inputSchema":{"type":"object","properties":{}}}),
    ];
    tools.push(json!({"name":"save_routine","description":"Create or replace a routine. Omit id to create; include id to update. triggers is an OR list. Use cron for recurring clock schedules; interval is for explicitly requested elapsed-duration semantics or existing schedules: interval {type:interval,seconds}; once {type:once,at:Unix milliseconds}; cron {type:cron,expression:five cron fields,timeZone:IANA zone}; webhook {type:webhook}; event {type:event,source:slack/github/origin/microsoftTeams/linear/sentry/pagerduty/email,event:event name or *,filters:field-to-value map}. Events require a configured webhook sender. Minimum interval is one second.","inputSchema":{"type":"object","properties":{"id":id,"name":{"type":"string"},"instruction":{"type":"string"},"enabled":{"type":"boolean"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":86400},"triggers":{"type":"array","items":{"type":"object"}}},"required":["name","instruction","triggers"]}}));
    for (name, description) in [
        ("set_routine_enabled", "Pause or resume a routine; enabled boolean is required."),
        ("delete_routine", "Delete a routine and cancel pending runs."),
        ("run_routine", "Start a test run, including for a paused routine."),
        (
            "routine_webhook",
            "Get the routine's webhook: its public url (through the Kith cloud; null when the cloud is off, then only localUrl on this computer works), localUrl and per-routine key. Senders POST with Authorization: Bearer <key>; for GitHub, set the webhook's content type to application/json and its secret to the key (deliveries are verified by X-Hub-Signature-256) and they become events {source:github, event:<event>.<action>, repo, sender, text, url, payload} that event triggers can filter. Deliveries made while the computer is offline wait up to 72 hours in the cloud, which sees their contents (not end-to-end encrypted). rotate:true replaces the key; the old one stops working at once. This doesn't register a provider subscription: set that up with the user, and report the webhook as awaiting connection until then.",
        ),
    ] {
        let required = if name == "set_routine_enabled" { vec!["id", "enabled"] } else { vec!["id"] };
        let mut properties = json!({"id": id});
        match name {
            "set_routine_enabled" => properties["enabled"] = json!({"type":"boolean"}),
            "routine_webhook" => properties["rotate"] = json!({"type":"boolean"}),
            _ => {}
        }
        tools.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required}}));
    }
    json!(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn routine() -> Routine {
        Routine {
            id: "routine".into(),
            bot_id: "bot".into(),
            name: "Check".into(),
            instruction: "Check status".into(),
            triggers: vec![Trigger::Interval { seconds: 60 }],
            enabled: true,
            created_at: 0,
            updated_at: 0,
            next_runs: vec![Some(60_000)],
            webhook_key: "secret".into(),
            deleted: false,
            timeout_seconds: default_timeout(),
            last_error: None,
        }
    }
    #[test]
    fn pending_runs_survive_restart_but_running_runs_are_not_duplicated() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let routines = Routines::default();
        let r = routine();
        let pending = new_run(&r, Value::Null, None);
        let mut running = new_run(&r, Value::Null, None);
        running.status = Status::Running;
        routines
            .change(&store, |s| {
                s.routines.push(r);
                s.runs.extend([pending, running]);
                Ok(())
            })
            .unwrap();
        let restored = Routines::default();
        restored.restore(&store).unwrap();
        let state = restored.state.locked();
        assert!(state.runs[0].status == Status::Pending);
        assert!(state.runs[1].status == Status::Recovering);
    }
    #[test]
    fn pause_cancels_pending_and_resume_recalculates_schedule() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let routines = Routines::default();
        let r = routine();
        routines
            .change(&store, |s| {
                s.runs.push(new_run(&r, Value::Null, None));
                s.routines.push(r);
                Ok(())
            })
            .unwrap();
        routines.set_enabled(&store, "bot", "routine", false).unwrap();
        assert!(routines.state.locked().runs[0].status == Status::Cancelled);
        routines.set_enabled(&store, "bot", "routine", true).unwrap();
        assert!(routines.state.locked().routines[0].next_runs[0].unwrap() > now_ms());
    }
    #[test]
    fn duplicate_delivery_is_idempotent_and_cross_bot_edits_fail() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let routines = Routines::default();
        routines
            .change(&store, |s| {
                s.routines.push(routine());
                Ok(())
            })
            .unwrap();
        let first = routines.enqueue(&store, "bot", "routine", Value::Null, Some("same".into()), false).unwrap();
        let second = routines.enqueue(&store, "bot", "routine", Value::Null, Some("same".into()), false).unwrap();
        assert_eq!(first["run"]["id"], second["run"]["id"]);
        assert!(routines.set_enabled(&store, "other", "routine", false).is_err());
        assert!(routines.remove(&store, "other", "routine").is_err());
        assert!(routines.enqueue(&store, "bot", "routine", Value::Null, None, false).is_err());
    }
}
