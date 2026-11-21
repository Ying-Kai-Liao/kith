//! The runner's rule over what `hostclaim.queued` lists (`core_runs.rs`): which turns become items, which run as a cron job's
//! own process, which are cancelled (with a reason and a notice), and where each shows. The run itself, on a real actor and
//! stub, is in `agent/bot/writer_tests.rs`; the cron run's process in `core_cron_tests.rs`.

#![allow(clippy::unwrap_used)]

use super::core_runs::{Ops, Started};
use super::switch_tests::{hub, routine};
use super::{LockExt, Value, json};
use std::cell::RefCell;

fn fired(turn: i64, agent: &str, job: &str) -> Value {
    json!({"turn": turn, "conversation": format!("schedule:{agent}:{job}:claude"), "agent": agent, "engine": "claude",
           "job": job, "person": "kai", "owner": true, "prompt": "do it", "deadline": null, "parkedReason": null,
           "redeemedBefore": false, "settings": null})
}

/// The core and the system as the rule sees them: what it cancelled (and why), what it started as a cron run.
#[derive(Default)]
struct Fake {
    cancelled: RefCell<Vec<(i64, String)>>,
    crons: RefCell<Vec<(i64, String)>>,
    start: RefCell<Option<Started>>,
}

impl Ops for Fake {
    fn cancel(&self, turn: i64, why: &str) -> bool {
        self.cancelled.borrow_mut().push((turn, why.to_owned()));
        true
    }
    fn cron(&self, t: &Value, job: &str) -> Started {
        self.crons.borrow_mut().push((t["turn"].as_i64().unwrap(), job.to_owned()));
        self.start.borrow_mut().take().unwrap_or(Started::Ran)
    }
}

fn own_bot(hub: &super::Hub, id: &str, agent: &str) {
    hub.store
        .save_bot(
            &serde_json::from_value(json!({"id": id, "name": agent, "cwd": "/tmp", "backend": "custom",
                                                 "command": "true", "hqAgent": agent}))
            .unwrap(),
        )
        .unwrap();
}

fn notices(hub: &super::Hub, bot: &str) -> Vec<String> {
    hub.store
        .history(bot, i64::MAX, 50)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "notice")
        .filter_map(|e| e.data["text"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_routines_job_runs_in_its_bot_and_a_kith_sched_job_as_a_process_of_its_own() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    own_bot(&hub, "b2", "ann");
    let ops = Fake::default();
    hub.routines.take(&hub, &[fired(1, "mel", &id), fired(2, "ann", "daily")], &ops);
    assert_eq!(hub.routines.pulled_items(), vec![(1, bot, true)], "only the routine goes to a bot's actor");
    assert_eq!(*ops.crons.borrow(), vec![(2, "daily".to_owned())], "the cron job is started by its own process");
    assert!(ops.cancelled.borrow().is_empty());
    let (turn, pulled) = hub.routines.pulled_of("core-1").unwrap();
    assert_eq!(turn, 1);
    assert_eq!(pulled.conversation, format!("schedule:mel:{id}:claude"));
    assert_eq!(pulled.person, "kai", "it begins as the turn's person");
}

#[test]
fn a_guests_cron_job_and_one_with_no_bot_here_run_too_as_their_own_processes() {
    let hub = hub();
    let ops = Fake::default();
    let mut guest = fired(5, "mel", "weekly");
    guest["person"] = json!("sam");
    guest["owner"] = json!(false);
    hub.routines.take(&hub, &[guest, fired(6, "nobody", "daily")], &ops);
    assert_eq!(*ops.crons.borrow(), vec![(5, "weekly".to_owned()), (6, "daily".to_owned())]);
    assert!(ops.cancelled.borrow().is_empty(), "{:?}", ops.cancelled.borrow());
    assert_eq!(hub.routines.pulled_items(), []);
}

#[test]
fn a_cron_run_that_cannot_start_is_cancelled_with_its_reason_and_a_notice_in_the_agents_chat() {
    let hub = hub();
    own_bot(&hub, "b2", "ann");
    let ops = Fake::default();
    *ops.start.borrow_mut() = Some(Started::Failed("couldn't start kith-sched run-turn: no such file".to_owned()));
    hub.routines.take(&hub, &[fired(3, "ann", "daily")], &ops);
    assert_eq!(*ops.cancelled.borrow(), vec![(3, "couldn't start kith-sched run-turn: no such file".to_owned())]);
    assert!(notices(&hub, "b2").iter().any(|n| n.contains("did not run: couldn't start")), "{:?}", notices(&hub, "b2"));
}

#[test]
fn a_turn_that_waits_or_is_gone_is_not_cancelled() {
    let hub = hub();
    let ops = Fake::default();
    *ops.start.borrow_mut() = Some(Started::Later);
    hub.routines.take(&hub, &[fired(3, "ann", "daily")], &ops);
    *ops.start.borrow_mut() = Some(Started::Gone);
    hub.routines.take(&hub, &[fired(4, "ann", "daily")], &ops);
    assert!(ops.cancelled.borrow().is_empty(), "pulled again later, or already ended: nothing to cancel");
}

#[test]
fn a_turn_the_host_cannot_place_is_cancelled_at_once_with_a_reason_not_left_to_its_deadline() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let ops = Fake::default();
    // A fire that names no job.
    let mut nameless = fired(1, "mel", "x");
    nameless["job"] = Value::Null;
    // A routine's turn that is not the owner's.
    let mut others = fired(2, "mel", &id);
    others["owner"] = json!(false);
    hub.routines.take(&hub, &[nameless, others], &ops);
    let why: Vec<_> = ops.cancelled.borrow().iter().map(|(t, w)| (*t, w.clone())).collect();
    assert_eq!(why, vec![(1, "the fire names no job".to_owned()), (2, "a routine runs for the owner only".to_owned())]);
    assert!(notices(&hub, &bot).iter().any(|n| n.contains("a routine runs for the owner only")));
    assert_eq!(hub.routines.pulled_items(), []);
}

#[test]
fn a_turn_that_ran_before_is_cancelled_with_a_notice_and_never_run() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let mut t = fired(7, "mel", &id);
    t["redeemedBefore"] = json!(true);
    let ops = Fake::default();
    hub.routines.take(&hub, &[t], &ops);
    assert_eq!(*ops.cancelled.borrow(), vec![(7, super::core_runs::INTERRUPTED.to_owned())]);
    assert_eq!(hub.routines.pulled_items(), [], "never begun again");
    assert!(notices(&hub, &bot).iter().any(|n| n.contains("a restart cut this run off")));
}

#[test]
fn a_turn_held_here_is_not_pulled_twice() {
    let hub = hub();
    let (_bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let ops = Fake::default();
    hub.routines.take(&hub, &[fired(1, "mel", &id)], &ops);
    // Listed again (and now said to have run before, as a usage-limit requeue would): ours, left to the actor.
    let mut again = fired(1, "mel", &id);
    again["redeemedBefore"] = json!(true);
    hub.routines.take(&hub, &[again], &ops);
    assert_eq!(hub.routines.pulled_items().len(), 1, "{:?}", hub.routines.pulled_items());
    assert!(ops.cancelled.borrow().is_empty());
    // A pending item whose turn stopped waiting (expired at its deadline) is dropped.
    hub.routines.take(&hub, &[], &ops);
    assert_eq!(hub.routines.pulled_items(), []);
}

#[test]
fn a_started_run_whose_actor_is_gone_frees_its_bots_slot() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "interval", "seconds": 60}]));
    let ops = Fake::default();
    hub.routines.take(&hub, &[fired(1, "mel", &id)], &ops);
    hub.routines.active.locked().insert(bot.clone());
    for item in hub.routines.pulled.locked().values_mut() {
        item.status = super::Status::Running;
    }
    // No actor runs for the bot (it was stopped or replaced): nothing would ever complete the item.
    hub.routines.take(&hub, &[], &ops);
    assert_eq!(hub.routines.pulled_items(), []);
    assert!(!hub.routines.active.locked().contains(&bot), "the slot is free for the next run");
}
