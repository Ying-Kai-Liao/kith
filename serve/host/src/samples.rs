//! Sample cards (`sampleCards`, `kith serve sample-cards`): one entry of every card the apps draw, in a "Samples" chat
//! of the owner's, so a new card can be seen without waiting for the event that posts it. Owner only, like every method
//! no guest list names. The chat is a group with no bots, so nothing runs there, and its cards are inert: `refuse`
//! turns away every answer or button aimed at it before any method runs. Each run replaces the chat, so every card shows
//! once and there is one Samples chat.
use crate::hub::Hub;
use crate::rich_card::{self, news};
use crate::routines::{Routine, Trigger};
use crate::store::{BotConfig, EntryKind, Lane};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::sync::Arc;

/// kv key holding the Samples chat's id.
const CHAT: &str = "samples.chat";
/// The hq agent the samples name: none is called this, so no app finds a real job behind a card.
const AGENT: &str = "kith-samples";
const JOB: &str = "sample-daily-brief";

/// Methods that answer a card by its entry.
const BY_ENTRY: [&str; 5] =
    ["respondPermission", "provideSecret", "connectorRequestFinish", "editHandoff", "keepConversation"];
/// Methods that change a chat's jobs, routines or settings by its id.
const BY_CHAT: [&str; 8] = [
    "kithSchedule",
    "routines",
    "saveRoutine",
    "setRoutineEnabled",
    "runRoutine",
    "deleteRoutine",
    "updateBot",
    "send",
];

/// The Samples chat, when there is one.
fn chat(hub: &Hub) -> Option<String> {
    let id = hub.store.kv_get(CHAT)?;
    hub.store.bot(&id).ok().flatten().filter(|b| !b.deleted).map(|_| id)
}

/// Refuses a call aimed at a sample card or the Samples chat, before the method runs.
pub fn refuse(hub: &Hub, method: &str, b: &Value) -> Result<()> {
    let sample = if BY_ENTRY.contains(&method) {
        let entry = b["entryId"].as_str().and_then(|id| hub.store.entry(id));
        entry.is_some_and(|e| e.data["sample"] == true || chat(hub).is_some_and(|c| c == e.bot_id))
    } else if BY_CHAT.contains(&method) {
        let target = if method == "updateBot" { &b["id"] } else { &b["botId"] };
        target.as_str().is_some_and(|id| chat(hub).is_some_and(|c| c == id))
    } else {
        false
    };
    if sample {
        bail!(crate::api::UserError("This is a sample card: it does nothing.".into()));
    }
    Ok(())
}

/// Replaces the Samples chat with a new one holding one entry of every card: its id and how many cards it holds.
pub fn post(hub: &Arc<Hub>) -> Result<Value> {
    if let Some(old) = chat(hub) {
        hub.delete_bot(&old)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let cfg: BotConfig = serde_json::from_value(json!({
        "id": id,
        "kind": "group",
        "name": "Samples",
        "description": "One of every card. Their buttons do nothing.",
        "createdAt": crate::store::now_ms(),
    }))?;
    hub.store.kv_set(CHAT, &id)?;
    hub.add_host_group(&cfg)?;
    let cards = cards(&id);
    let lane = Lane::main(&id);
    for (kind, mut data) in cards.iter().cloned() {
        data["sample"] = true.into();
        data["author"] = id.as_str().into();
        hub.add_entry(&lane, kind, 0, &data);
    }
    Ok(json!({"botId": id, "cards": cards.len()}))
}

/// Every card, as the code that posts it for real shapes it.
fn cards(chat: &str) -> Vec<(EntryKind, Value)> {
    let now = chrono::Local::now().fixed_offset();
    let tomorrow = (now + chrono::Duration::days(1)).date_naive().and_hms_opt(7, 30, 0).unwrap_or_default();
    let next = tomorrow.and_local_timezone(chrono::Local).single().map(|t| t.fixed_offset().to_rfc3339());
    let job = json!({
        "source": "kith", "id": JOB, "cron": "30 7 * * 1-5", "paused": false, "person": null, "next_run": next,
        "task": "Read the news I follow and send me the five stories that matter, one line each with a link.",
    });
    let op = rich_card::Op { id: JOB.into(), op: "added".into() };
    let schedule = rich_card::schedule(AGENT, None, &op, &[job], None, now).unwrap_or_default();

    let routine = Routine {
        id: "sample-routine".into(),
        bot_id: chat.into(),
        name: "Weekly tidy".into(),
        instruction: "Every Friday, archive the week's finished threads and tell me what is still open.".into(),
        triggers: vec![Trigger::Cron { expression: "0 17 * * 5".into(), time_zone: "Australia/Sydney".into() }],
        enabled: true,
        created_at: now.timestamp_millis(),
        updated_at: now.timestamp_millis(),
        next_runs: vec![None],
        webhook_key: String::new(),
        deleted: false,
        timeout_seconds: 600,
        last_error: None,
    };
    let routine = json!({
        "text": "Created routine: Weekly tidy", "routineId": routine.id, "style": "info",
        "richCard": rich_card::routine(&routine, false, now),
    });

    let brief = "Your morning brief:\n\
        - **Rust 1.99 is out**: faster builds and a new lint for unused results ([Rust blog](https://blog.rust-lang.org/))\n\
        - **SwiftUI adds glass toolbars**: floating bars come to iPad apps ([Apple](https://developer.apple.com/news/))\n\
        - **Sydney trains**: weekend trackwork on the T1 line ([Transport NSW](https://transportnsw.info/))\n\
        \nThat's all for today.";
    let news = json!({"text": brief, "final": true, "richCard": news::card(brief, AGENT, JOB, Some("0 7 * * *"))});

    let approval = json!({
        "title": "Run git push", "toolKind": "execute", "command": "git push origin coder/sample-branch",
        "detail": "git push origin coder/sample-branch", "diffs": [], "cwd": "/Users/you/Workspace/kith",
        "options": [
            {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
            {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
        ],
        "status": "pending", "selected": null, "approver": "owner", "agent": AGENT, "source": "run",
    });
    let decision = json!({
        "title": "Which name should the new screen use?", "toolKind": "other", "command": null,
        "detail": "The design has two candidates.", "diffs": [], "cwd": null,
        "options": [
            {"optionId": "a", "name": "Inbox", "kind": "allow_once"},
            {"optionId": "b", "name": "Activity", "kind": "allow_once"},
            {"optionId": "later", "name": "Ask me later", "kind": "reject_once"},
        ],
        "status": "pending", "selected": null, "approver": "owner", "agent": AGENT, "source": "run",
    });
    let secret = |card: &str, title: &str, detail: &str, options: Value, accounts: Value| {
        json!({
            "title": title, "toolKind": "secret", "command": null, "detail": detail, "diffs": [], "cwd": null,
            "options": options, "status": "pending", "selected": null, "approver": "owner", "agent": AGENT,
            "source": "run", "card": card, "accounts": accounts,
            "secret": {
                "service": "github", "vars": ["GITHUB_TOKEN"], "profile": null, "account": null,
                "purpose": "Open pull requests on your repositories.", "replaces": false, "state": "pending",
            },
        })
    };
    let access = secret(
        "account",
        "Samples wants access to github",
        "Choose which github account Samples may use.",
        json!([
            {"optionId": "profile:github/work", "name": "Use work", "kind": "allow_once"},
            {"optionId": "another", "name": "Another account", "kind": "allow_once"},
            {"optionId": "deny", "name": "Not now", "kind": "reject_once"},
        ]),
        json!([{"profile": "github/work", "account": "work", "agents": ["coder"]}]),
    );
    let key = secret(
        "key",
        "Samples needs a key for github",
        "Update Kith to add the key here.",
        json!([{"optionId": "deny", "name": "Not now", "kind": "reject_once"}]),
        json!([]),
    );

    let month = now.format("%B").to_string();
    let state = json!({
        "agent": AGENT, "spent": 51.2, "limit": 50, "at": "ask", "perRun": 1.6, "runs": 32, "month": month,
        "raiseTo": 100, "resets": null,
    });
    let agent_limit = crate::limits::agent_card(chat, &state, &[]);
    let person_limit = crate::limits::guest_card(chat, AGENT, "sample-person", "Sam", 10.4, 10.0, Some(&month), &[]);

    let handoff = json!({
        "text": "This conversation is getting long. Here is a summary to start fresh with.", "style": "info",
        "handoff": {
            "status": "draft", "reason": "full",
            "text": "We are adding sample cards to Kith. Done: the host posts them. Next: check them on the phone.",
        },
    });
    let login = json!({
        "text": "I need to sign in to check your order status.",
        "connectionRequest": {"kind": "login", "status": "pending", "title": "example.com", "site": "example.com", "botId": chat},
    });

    vec![
        (EntryKind::Agent, schedule),
        (EntryKind::Notice, routine),
        (EntryKind::Agent, news),
        (EntryKind::Permission, approval),
        (EntryKind::Permission, decision),
        (EntryKind::Permission, access),
        (EntryKind::Permission, key),
        (EntryKind::Permission, agent_limit),
        (EntryKind::Permission, person_limit),
        (EntryKind::Notice, handoff),
        (EntryKind::Notice, login),
    ]
}

#[cfg(test)]
mod tests;
