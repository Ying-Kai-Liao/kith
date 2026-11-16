//! The runner's rule over what `hostclaim.queued` lists (`core_runs.rs`): which turns become items, which are cancelled, which
//! are left to expire, and where each shows. The run itself, on a real actor and stub, is in `agent/bot/writer_tests.rs`.

#![allow(clippy::unwrap_used)]

use super::switch_tests::{hub, routine};
use super::{LockExt, Value, json};
use std::cell::RefCell;

fn fired(turn: i64, agent: &str, job: &str) -> Value {
    json!({"turn": turn, "conversation": format!("schedule:{agent}:{job}:claude"), "agent": agent, "engine": "claude",
           "job": job, "person": "kai", "owner": true, "prompt": "do it", "deadline": null, "parkedReason": null,
           "redeemedBefore": false})
}

#[test]
fn a_routines_job_runs_in_its_bot_and_a_kith_sched_job_in_the_agents_own_chat() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    hub.store
        .save_bot(
            &serde_json::from_value(json!({"id": "b2", "name": "Ann", "cwd": "/tmp", "backend": "custom",
                                                 "command": "true", "hqAgent": "ann"}))
            .unwrap(),
        )
        .unwrap();
    let cancelled = RefCell::new(Vec::new());
    hub.routines.take(&hub, &[fired(1, "mel", &id), fired(2, "ann", "daily")], |t| {
        cancelled.borrow_mut().push(t);
        true
    });
    assert_eq!(hub.routines.pulled_items(), vec![(1, bot, true), (2, "b2".to_owned(), true)]);
    assert!(cancelled.borrow().is_empty());
    let (turn, pulled) = hub.routines.pulled_of("core-1").unwrap();
    assert_eq!(turn, 1);
    assert_eq!(pulled.conversation, format!("schedule:mel:{id}:claude"));
    assert_eq!(pulled.person, "kai", "it begins as the turn's person");
}

#[test]
fn a_turn_that_ran_before_is_cancelled_with_a_notice_and_never_run() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let mut t = fired(7, "mel", &id);
    t["redeemedBefore"] = json!(true);
    let cancelled = RefCell::new(Vec::new());
    hub.routines.take(&hub, &[t], |t| {
        cancelled.borrow_mut().push(t);
        true
    });
    assert_eq!(*cancelled.borrow(), vec![7]);
    assert_eq!(hub.routines.pulled_items(), [], "never begun again");
    let notices: Vec<String> = hub
        .store
        .history(&bot, i64::MAX, 50)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "notice")
        .filter_map(|e| e.data["text"].as_str().map(str::to_owned))
        .collect();
    assert!(notices.iter().any(|n| n.contains("a restart cut this run off")), "{notices:?}");
}

#[test]
fn a_turn_held_here_is_not_pulled_twice_and_one_with_no_place_or_owner_is_left() {
    let hub = hub();
    let (_bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let never = |_: i64| -> bool { panic!("nothing is cancelled") };
    hub.routines.take(&hub, &[fired(1, "mel", &id)], never);
    // Listed again (and now said to have run before, as a usage-limit requeue would): ours, left to the actor.
    let mut again = fired(1, "mel", &id);
    again["redeemedBefore"] = json!(true);
    let mut someone = fired(2, "mel", "other");
    someone["owner"] = json!(false);
    hub.routines.take(&hub, &[again, someone, fired(3, "nobody", "daily")], never);
    assert_eq!(hub.routines.pulled_items().len(), 1, "{:?}", hub.routines.pulled_items());
    // A pending item whose turn stopped waiting (expired at its deadline) is dropped.
    hub.routines.take(&hub, &[], never);
    assert_eq!(hub.routines.pulled_items(), []);
}

#[test]
fn a_started_run_whose_actor_is_gone_frees_its_bots_slot() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let never = |_: i64| -> bool { panic!("nothing is cancelled") };
    hub.routines.take(&hub, &[fired(1, "mel", &id)], never);
    hub.routines.active.locked().insert(bot.clone());
    for item in hub.routines.pulled.locked().values_mut() {
        item.status = super::Status::Running;
    }
    // No actor runs for the bot (it was stopped or replaced): nothing would ever complete the item.
    hub.routines.take(&hub, &[], never);
    assert_eq!(hub.routines.pulled_items(), []);
    assert!(!hub.routines.active.locked().contains(&bot), "the slot is free for the next run");
}
