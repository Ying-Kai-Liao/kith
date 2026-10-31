//! The owner's peek at a handoff (a `kith ask`): its brief, where it stands, the worker's thread and what it is doing
//! now, and the limits it runs under, so the apps can draw the peek sheet. Read-only, and the owner's alone: the method
//! names are on neither the member nor the bot-guest lists (`roles.rs`), and the brief is not part of any RPC contract
//! for them. The records are the kernel's (`delegation::list`); the host keeps none.

use super::str_arg;
use crate::delegation;
use crate::hub::Hub;
use crate::store::EntryKind;
use anyhow::{Result, anyhow};
use kith_wire::kernel::{Delegation, DelegationState};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The kernel's depth caps (`MAX_LOCAL_HOPS` / `MAX_CROSS_HOPS` in kernel/bin/kith-ask).
const HOP_MAX: i64 = 8;
const CROSS_MAX: i64 = 2;
/// An app that polls reads the kernel at most this often (a read may take up to 20 s).
const FRESH: Duration = Duration::from_secs(3);
/// The longest live line.
const LIVE_MAX: usize = 200;

static CACHE: Mutex<Option<(Instant, Vec<Delegation>)>> = Mutex::new(None);

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "delegationPeek" => {
            let id = str_arg(&b, "id")?;
            if !delegation::plain(id) {
                return Err(anyhow!("no such handoff"));
            }
            let all = records().await?;
            let d = all.iter().find(|d| d.id == id).ok_or_else(|| anyhow!("no record of that handoff"))?;
            let live = live_of(hub, d).await;
            json!({"peek": peek(d, parked(id), live, now_ms())})
        }
        // The ones still going or waiting on the owner, without the live line: the apps need not know ids.
        "delegationsPeek" => {
            let now = now_ms();
            let all = records().await?;
            let open: Vec<Value> = all
                .iter()
                .filter(|d| matches!(d.state, DelegationState::Open | DelegationState::Capped))
                .map(|d| peek(d, delegation::plain(&d.id) && parked(&d.id), None, now))
                .collect();
            json!({"peeks": open})
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

/// The kernel's records, read at most every few seconds. A failed read answers the error and is not remembered.
async fn records() -> Result<Vec<Delegation>> {
    let held = CACHE.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some((at, all)) = held
        && at.elapsed() < FRESH
    {
        return Ok(all);
    }
    let all = delegation::list().await?;
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), all.clone()));
    Ok(all)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Whether the ask's run waits on a card (`ID.parked` beside its answers). `id` is a safe kernel id.
fn parked(id: &str) -> bool {
    parked_in(&crate::kernel::machine_dir(&crate::service::home()), id)
}

fn parked_in(machine: &std::path::Path, id: &str) -> bool {
    machine.join("live").join("ask").join(format!("{id}.parked")).exists()
}

/// Where the handoff stands, in the words the apps show: `sent`, `working`, `needsYou`, `done`, `stopped`, or `unknown`
/// for a state this version doesn't know. A capped chain whose cap was not raised since waits on the owner too.
fn state_of(d: &Delegation, parked: bool) -> &'static str {
    match d.state {
        DelegationState::Stopped => "stopped",
        DelegationState::Answered => "done",
        DelegationState::Unknown => "unknown",
        DelegationState::Capped => {
            let under = matches!((d.chain_cost_micro_usd, d.cap_micro_usd), (Some(cost), Some(cap)) if cost < cap);
            if under && !parked { "working" } else { "needsYou" }
        }
        DelegationState::Open if parked => "needsYou",
        DelegationState::Open if d.session.is_some() => "working",
        DelegationState::Open => "sent",
    }
}

/// The peek of one record. `live` is the worker's latest line when its thread is known here.
fn peek(d: &Delegation, parked: bool, live: Option<Value>, now: i64) -> Value {
    let state = state_of(d, parked);
    // A stopped ask has no end time of its own: no clock to show.
    let elapsed = match (d.answered_ms, d.state) {
        (Some(end), _) => Some((end - d.started_ms).max(0)),
        (None, DelegationState::Stopped | DelegationState::Unknown) => None,
        (None, _) => Some((now - d.started_ms).max(0)),
    };
    json!({
        "id": d.id,
        "state": state,
        "brief": d.brief,
        "from": {"agent": d.caller.agent, "person": d.caller.person},
        "to": d.agent,
        "crossed": d.crossed,
        "chain": d.chain,
        "session": d.session,
        "thread": live.as_ref().map(|l| l["thread"].clone()),
        "live": live.map(|l| l["line"].clone()),
        "limits": {
            "costMicroUsd": d.cost_micro_usd,
            "capMicroUsd": d.cap_micro_usd,
            "chainCostMicroUsd": d.chain_cost_micro_usd,
            "hop": d.hop,
            "hopMax": HOP_MAX,
            "crossHops": d.cross_hops,
            "crossMax": CROSS_MAX,
            "elapsedMs": elapsed,
        },
        "startedMs": d.started_ms,
        "answeredMs": d.answered_ms,
        "cappedMs": d.capped_ms,
    })
}

/// The worker's thread and latest line, from the session the record names; none when this Mac's host doesn't know it
/// (an ask from before sessions were recorded, a run on another Mac, a thread since deleted).
async fn live_of(hub: &Hub, d: &Delegation) -> Option<Value> {
    let found = crate::terminal::resolve(hub, d.session.as_deref().filter(|s| delegation::plain(s))?).await;
    let bot = found["botId"].as_str()?;
    let thread = found["threadId"].as_str();
    Some(json!({
        "thread": {"botId": bot, "threadId": thread, "sessionId": found["sessionId"]},
        "line": live_line(hub, bot, thread),
    }))
}

/// The newest tool row or text of a lane, as `{kind, text}`.
fn live_line(hub: &Hub, bot: &str, thread: Option<&str>) -> Value {
    let rows = match thread {
        Some(root) => hub.store.thread(bot, root, 40),
        None => hub.store.history(bot, i64::MAX, 40),
    }
    .unwrap_or_default();
    let line = rows.iter().rev().find_map(|e| {
        let (kind, text) = if e.kind == EntryKind::Tool.as_str() {
            ("tool", e.data["title"].as_str()?)
        } else if e.kind == EntryKind::Agent.as_str() {
            ("text", e.data["text"].as_str()?)
        } else {
            return None;
        };
        let text = text.trim();
        (!text.is_empty()).then(|| json!({"kind": kind, "text": text.chars().take(LIVE_MAX).collect::<String>()}))
    });
    line.unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kith_wire::kernel::{Caller, ReplyTo};

    fn rec(id: &str, state: DelegationState) -> Delegation {
        Delegation {
            id: id.into(),
            agent: "echo".into(),
            chain: id.into(),
            caller: Caller { agent: Some("architect".into()), person: Some("mia".into()), via: None },
            reply_to: ReplyTo::default(),
            state,
            cost_micro_usd: Some(250_000),
            chain_cost_micro_usd: Some(1_000_000),
            cap_micro_usd: Some(5_000_000),
            started_ms: 1_000,
            brief: Some("Write the plan".into()),
            ..Delegation::default()
        }
    }

    #[test]
    fn each_state_has_its_word() {
        let mut working = rec("a", DelegationState::Open);
        assert_eq!(state_of(&working, false), "sent");
        working.session = Some("sess".into());
        assert_eq!(state_of(&working, false), "working");
        assert_eq!(state_of(&working, true), "needsYou", "a run parked on a card waits on the owner");
        assert_eq!(state_of(&rec("b", DelegationState::Answered), false), "done");
        assert_eq!(state_of(&rec("c", DelegationState::Stopped), false), "stopped");
        assert_eq!(state_of(&rec("d", DelegationState::Unknown), false), "unknown");
    }

    #[test]
    fn a_capped_chain_waits_on_the_owner_until_the_cap_is_raised() {
        let mut capped = rec("a", DelegationState::Capped);
        capped.chain_cost_micro_usd = Some(5_250_000);
        assert_eq!(state_of(&capped, false), "needsYou");
        capped.cap_micro_usd = Some(10_000_000);
        assert_eq!(state_of(&capped, false), "working", "raised since: under the cap again");
        capped.cap_micro_usd = None;
        assert_eq!(state_of(&capped, false), "needsYou", "no cap to compare: still the owner's");
    }

    #[test]
    fn the_peek_carries_the_brief_the_route_and_the_limits() {
        let mut d = rec("a", DelegationState::Open);
        d.hop = 2;
        d.cross_hops = 1;
        d.crossed = true;
        d.agent = "echo@mini".into();
        let v = peek(&d, false, None, 5_000);
        assert_eq!(v["brief"], "Write the plan");
        assert_eq!(v["from"], json!({"agent": "architect", "person": "mia"}));
        assert_eq!(v["to"], "echo@mini");
        assert_eq!(v["crossed"], true);
        assert_eq!(v["limits"]["hop"], 2);
        assert_eq!(v["limits"]["hopMax"], 8);
        assert_eq!(v["limits"]["crossHops"], 1);
        assert_eq!(v["limits"]["crossMax"], 2);
        assert_eq!(v["limits"]["costMicroUsd"], 250_000);
        assert_eq!(v["limits"]["chainCostMicroUsd"], 1_000_000);
        assert_eq!(v["limits"]["capMicroUsd"], 5_000_000);
        assert_eq!(v["limits"]["elapsedMs"], 4_000);
        assert_eq!((v["thread"].is_null(), v["live"].is_null()), (true, true));
    }

    #[test]
    fn an_old_record_has_no_brief_session_or_costs() {
        let d = Delegation {
            id: "old".into(),
            agent: "echo".into(),
            chain: "old".into(),
            state: DelegationState::Open,
            started_ms: 10,
            ..Delegation::default()
        };
        let v = peek(&d, false, None, 20);
        assert!(v["brief"].is_null() && v["session"].is_null());
        assert!(v["limits"]["costMicroUsd"].is_null() && v["limits"]["capMicroUsd"].is_null());
        assert_eq!(v["state"], "sent");
        assert!(v["from"]["agent"].is_null(), "asked from a terminal");
    }

    #[test]
    fn an_answered_ask_stops_its_clock_and_a_stopped_one_shows_none() {
        let mut done = rec("a", DelegationState::Answered);
        done.answered_ms = Some(61_000);
        assert_eq!(peek(&done, false, None, 999_999)["limits"]["elapsedMs"], 60_000);
        assert_eq!(peek(&done, false, None, 999_999)["answeredMs"], 61_000);
        let stopped = rec("b", DelegationState::Stopped);
        assert!(peek(&stopped, false, None, 999_999)["limits"]["elapsedMs"].is_null());
    }

    #[test]
    fn a_parked_run_is_read_from_its_file() {
        let machine = std::env::temp_dir().join(format!("kith-peek-{}", uuid::Uuid::new_v4()));
        let ask = machine.join("live").join("ask");
        std::fs::create_dir_all(&ask).unwrap();
        assert!(!parked_in(&machine, "ask-1"));
        std::fs::write(ask.join("ask-1.parked"), "").unwrap();
        assert!(parked_in(&machine, "ask-1"));
        assert!(!parked_in(&machine, "ask-2"));
    }

    mod on_a_hub {
        use super::*;
        use crate::remote::identity::Identity;
        use crate::store::{Lane, Store};

        fn hub() -> Arc<Hub> {
            let dir = std::env::temp_dir().join(format!("kith-peek-hub-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Store::open(std::path::Path::new(":memory:")).unwrap();
            Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0)
        }

        #[test]
        fn the_live_line_is_the_newest_tool_row_or_text_of_the_thread() {
            let hub = hub();
            let main = Lane::main("bot");
            let root = hub.add_entry(&main, EntryKind::User, 1, &json!({"text": "go"})).unwrap();
            let thread = Lane { chat: "bot".into(), thread: Some(root.id.clone()) };
            assert_eq!(live_line(&hub, "bot", Some(&root.id)), Value::Null, "nothing yet");
            hub.add_entry(&thread, EntryKind::Agent, 1, &json!({"text": "Reading the plan", "final": false})).unwrap();
            hub.add_entry(&thread, EntryKind::Tool, 1, &json!({"title": "Edit plan.md", "status": "pending"})).unwrap();
            hub.add_entry(&thread, EntryKind::Thought, 1, &json!({"text": "hmm"})).unwrap();
            assert_eq!(live_line(&hub, "bot", Some(&root.id)), json!({"kind": "tool", "text": "Edit plan.md"}));
            hub.add_entry(&thread, EntryKind::Agent, 1, &json!({"text": "x".repeat(500), "final": true})).unwrap();
            let line = live_line(&hub, "bot", Some(&root.id));
            assert_eq!(line["kind"], "text");
            assert_eq!(line["text"].as_str().unwrap().len(), LIVE_MAX);
        }

        #[tokio::test]
        async fn a_session_this_host_does_not_know_has_no_live_line() {
            let hub = hub();
            let mut d = rec("a", DelegationState::Open);
            assert!(live_of(&hub, &d).await.is_none(), "no session recorded");
            d.session = Some("sess-elsewhere".into());
            assert!(live_of(&hub, &d).await.is_none(), "a session on another Mac");
            d.session = Some("../x".into());
            assert!(live_of(&hub, &d).await.is_none(), "an unsafe id is never looked up");
        }

        /// A stand-in kernel whose `ask delegations --json` prints `body` and exits `code`.
        fn fake_kernel(body: &str, code: i32) -> std::path::PathBuf {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = std::env::temp_dir().join(format!("kith-peek-kernel-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("kith");
            std::fs::write(&bin, format!("#!/bin/sh\ncat <<'EOF'\n{body}\nEOF\nexit {code}\n")).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        }

        #[tokio::test]
        async fn the_calls_answer_from_the_kernels_records_and_a_kernel_failure_is_an_error() {
            let _one = crate::remote::account::ONE_AT_A_TIME.lock().await;
            let hub = hub();
            let bad = call(&hub, "delegationPeek", json!({"id": "../etc"})).await.unwrap_err();
            assert_eq!(bad.to_string(), "no such handoff");
            assert!(call(&hub, "delegationPeek", json!({})).await.is_err(), "`id` is required");
            assert!(matches!(call(&hub, "somethingElse", json!({})).await.unwrap(), ControlFlow::Continue(_)));

            let mut open = rec("ask-1", DelegationState::Open);
            open.session = Some("sess-elsewhere".into());
            let all = json!({"delegations": [open, rec("ask-2", DelegationState::Answered)]});
            *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kernel(&all.to_string(), 0));
            *CACHE.lock().unwrap() = None;
            let ControlFlow::Break(one) = call(&hub, "delegationPeek", json!({"id": "ask-1"})).await.unwrap() else {
                panic!("answered")
            };
            assert_eq!(one["peek"]["state"], "working");
            assert_eq!(one["peek"]["brief"], "Write the plan");
            assert!(one["peek"]["live"].is_null(), "a session this host doesn't know");
            let err = call(&hub, "delegationPeek", json!({"id": "ask-9"})).await.unwrap_err();
            assert_eq!(err.to_string(), "no record of that handoff");
            let ControlFlow::Break(list) = call(&hub, "delegationsPeek", json!({})).await.unwrap() else { panic!() };
            let ids: Vec<&str> = list["peeks"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
            assert_eq!(ids, ["ask-1"], "only the open ones");

            // The records are cached: a kernel that now fails is not asked inside the window.
            *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kernel("boom", 1));
            assert!(call(&hub, "delegationPeek", json!({"id": "ask-1"})).await.is_ok());
            *CACHE.lock().unwrap() = None;
            assert!(
                call(&hub, "delegationPeek", json!({"id": "ask-1"})).await.is_err(),
                "a kernel failure answers an error"
            );
            assert!(call(&hub, "delegationsPeek", json!({})).await.is_err());
            *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;
            assert!(call(&hub, "delegationsPeek", json!({})).await.is_err(), "no kernel installed");
        }
    }
}
