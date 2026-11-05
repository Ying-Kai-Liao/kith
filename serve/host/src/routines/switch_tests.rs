//! The host's routines once the core owns the schedules (`routines::switch`): not switched is today's behaviour (every other
//! test of this module), switched forwards, and a gate the host cannot read fires nothing.
use super::hooks::{Headers, Refused};
use super::switch::{Gate, Link, decide};
use crate::core_front::Answer;
use crate::hub::Hub;
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type Script = Box<dyn Fn(&str, &Value) -> Answer + Send + Sync>;

/// A stand-in for the core.
pub(super) struct Fake {
    pub gate: Mutex<Gate>,
    pub calls: Mutex<Vec<(String, String, Value)>>,
    script: Mutex<Script>,
}

impl Fake {
    pub fn says(&self, f: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static) {
        *self.script.lock().unwrap() = Box::new(f);
    }

    pub fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls.lock().unwrap().iter().filter(|c| c.1 == method).map(|c| c.2.clone()).collect()
    }

    fn ask(&self, who: &str, method: &str, params: &Value) -> Answer {
        self.calls.lock().unwrap().push((who.into(), method.into(), params.clone()));
        (self.script.lock().unwrap())(method, params)
    }
}

impl Link for Fake {
    fn gate(&self) -> Gate {
        self.gate.lock().unwrap().clone()
    }
    fn owner(&self, method: &str, params: &Value) -> Answer {
        self.ask("owner", method, params)
    }
    fn host(&self, method: &str, params: &Value) -> Answer {
        self.ask("host", method, params)
    }
}

pub(super) fn hub() -> Arc<Hub> {
    let dir = std::env::temp_dir().join(format!("kith-switch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(&dir.join("t.db")).unwrap();
    Hub::new(store, "host".into(), crate::remote::identity::Identity::load_or_create(&dir).unwrap(), "token".into(), 0)
}

/// A bot (with a kith agent when `agent` is set) and a routine with these triggers; `(bot, routine)`.
pub(super) fn routine(hub: &Hub, agent: Option<&str>, triggers: &Value) -> (String, String) {
    let mut cfg = json!({"id": "b1", "name": "B", "cwd": "/tmp", "backend": "custom", "command": "true"});
    if let Some(a) = agent {
        cfg["hqAgent"] = json!(a);
    }
    hub.store.save_bot(&serde_json::from_value(cfg).unwrap()).unwrap();
    let saved =
        hub.routines.save(hub, "b1", &json!({"name": "R", "instruction": "do it", "triggers": triggers})).unwrap();
    ("b1".into(), saved["routine"]["id"].as_str().unwrap().to_owned())
}

/// The routines now ask this stand-in, which answers `gate`.
pub(super) fn switched(hub: &Hub, gate: Gate) -> Arc<Fake> {
    let fake = Arc::new(Fake {
        gate: Mutex::new(gate),
        calls: Mutex::default(),
        script: Mutex::new(Box::new(|_, _| Answer::Done(json!({})))),
    });
    hub.routines.use_link(fake.clone());
    hub.routines.refresh_gate();
    fake
}

fn headers(pairs: &[(&str, &str)]) -> Headers {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn said(r: Result<Value>) -> String {
    format!("{:#}", r.unwrap_err())
}

#[test]
fn the_gate_follows_the_rule_in_the_module_doc() {
    let done = Answer::Done;
    let down = || Answer::Down { why: "no core is running".into(), lost: false };
    assert_eq!(decide(&done(json!({"ok": true, "warning": null})), true), Gate::Core);
    assert_eq!(
        decide(&done(json!({"ok": true, "warning": "WARNING"})), true),
        Gate::Core,
        "a warning on a yes is still a yes"
    );
    assert_eq!(decide(&done(json!({"ok": false, "warning": null})), false), Gate::Own);
    // A no with a reason: held only when a switch file says it could be the core's.
    assert!(matches!(decide(&done(json!({"ok": false, "warning": "schedules is stepping"})), true), Gate::Held(_)));
    assert_eq!(decide(&done(json!({"ok": false, "warning": "1 difference(s)"})), false), Gate::Own);
    // Unreadable: held with a switch file, the host's own without one.
    assert!(matches!(decide(&down(), true), Gate::Held(_)));
    assert_eq!(decide(&down(), false), Gate::Own);
    assert!(matches!(decide(&Answer::Refused("not served".into()), true), Gate::Held(_)));
    assert_eq!(decide(&Answer::Refused("not served".into()), false), Gate::Own);
    assert!(matches!(decide(&done(json!({"nonsense": 1})), true), Gate::Held(_)));
    assert_eq!(decide(&done(json!({"nonsense": 1})), false), Gate::Own);
}

#[test]
fn edits_and_run_now_are_refused_while_the_core_owns_the_schedules_and_work_as_before_otherwise() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "webhook"}]));
    // Not switched: today's behaviour.
    assert!(hub.routines.set_enabled(&hub.store, &bot, &id, false).is_ok());
    assert!(hub.routines.set_enabled(&hub.store, &bot, &id, true).is_ok());
    assert!(hub.routines.run_now(&hub, &bot, &id).unwrap()["run"]["id"].is_string());

    let fake = switched(&hub, Gate::Core);
    assert!(said(hub.routines.set_enabled(&hub.store, &bot, &id, false)).contains("kith schedule"));
    assert!(said(hub.routines.remove(&hub.store, &bot, &id)).contains("Kith core"));
    let new = json!({"name": "N", "instruction": "i", "triggers": [{"type": "webhook"}]});
    assert!(said(hub.routines.save(&hub, &bot, &new)).contains("Kith core"));
    assert!(said(hub.routines.credentials(&hub, &bot, &id, true)).contains("Kith core"));
    assert!(hub.routines.credentials(&hub, &bot, &id, false).is_ok(), "reading the webhook is not an edit");
    assert!(hub.routines.list(&bot)["routines"][0]["enabled"].as_bool().unwrap(), "serve.db is left as it was");
    assert!(hub.routines.keeps_awake(), "the host cannot see what is due, so it stays awake");

    // Run now goes to the core, to the routine's job, and starts nothing in the host.
    fake.says(|m, _| match m {
        "schedule.runNow" => Answer::Done(json!({"outcome": "fired", "turn": "t1"})),
        _ => Answer::Done(json!({})),
    });
    let runs_before = hub.routines.list(&bot)["runs"].as_array().unwrap().len();
    let out = hub.routines.run_now(&hub, &bot, &id).unwrap();
    assert_eq!(out["core"]["turn"], "t1");
    let calls = fake.calls_of("schedule.runNow");
    assert_eq!((calls[0]["agent"].as_str(), calls[0]["id"].as_str()), (Some("mel"), Some(id.as_str())));
    assert_eq!(hub.routines.list(&bot)["runs"].as_array().unwrap().len(), runs_before);
    assert!(said(hub.routines.run_now(&hub, "someone-else", &id)).contains("not found"));
    fake.says(|_, _| Answer::Done(json!({"outcome": "busy", "retry": true})));
    assert!(said(hub.routines.run_now(&hub, &bot, &id)).contains("active run"));
    fake.says(|_, _| Answer::Down { why: "no core is running".into(), lost: false });
    assert!(said(hub.routines.run_now(&hub, &bot, &id)).contains("not started"));
}

#[test]
fn a_held_gate_refuses_edits_and_run_now_and_forwards_nothing() {
    let hub = hub();
    let (bot, id) = routine(&hub, Some("mel"), &json!([{"type": "webhook"}]));
    let fake = switched(&hub, Gate::Held("the core cannot be asked".into()));
    assert!(hub.routines.set_enabled(&hub.store, &bot, &id, false).is_err());
    assert!(hub.routines.run_now(&hub, &bot, &id).is_err());
    assert_eq!(hub.routines.receive(&hub, &id, &headers(&[]), b"{}").unwrap_err(), Refused::Held);
    assert!(fake.calls.lock().unwrap().is_empty(), "nothing is forwarded while the host cannot tell");
}

#[test]
fn a_delivery_goes_to_the_core_with_the_raw_headers_and_body_and_its_answers_keep_todays_statuses() {
    let hub = hub();
    let (_, id) = routine(&hub, Some("mel"), &json!([{"type": "webhook"}]));
    let fake = switched(&hub, Gate::Core);
    let h = headers(&[("authorization", "Bearer sekrit"), ("x-delivery-id", "d1")]);
    let answer = |v: Value| move |_: &str, _: &Value| Answer::Done(v.clone());

    fake.says(answer(json!({"outcome": "fired", "turn": "t9"})));
    assert!(hub.routines.receive(&hub, &id, &h, br#"{"text":"x"}"#).is_ok());
    let sent = &fake.calls_of("schedule.fire")[0];
    assert_eq!((sent["agent"].as_str(), sent["id"].as_str()), (Some("mel"), Some(id.as_str())));
    assert_eq!(sent["body"], r#"{"text":"x"}"#);
    assert_eq!(sent["headers"]["x-delivery-id"], "d1");
    assert_eq!(fake.calls.lock().unwrap()[0].0, "host", "as the Host principal");

    let refused = |v: Value| {
        fake.says(answer(v));
        hub.routines.receive(&hub, &id, &h, b"{}").unwrap_err()
    };
    assert_eq!(refused(json!({"outcome": "refused", "code": "unauthorized", "reason": "r"})), Refused::Unauthorized);
    assert_eq!(refused(json!({"outcome": "refused", "code": "paused", "reason": "r"})), Refused::Paused);
    assert_eq!(
        refused(json!({"outcome": "refused", "code": "invalid", "reason": "bad event"})),
        Refused::Invalid("bad event".into())
    );
    assert_eq!(refused(json!({"outcome": "refused", "code": "not-here", "reason": "r"})), Refused::NotHere);
    assert_eq!(refused(json!({"outcome": "busy", "retry": true})), Refused::Busy);
    fake.says(answer(json!({"outcome": "ping"})));
    assert_eq!(hub.routines.receive(&hub, &id, &h, b"{}").unwrap(), json!({"ping": true}));
    fake.says(answer(json!({"outcome": "ignored"})));
    assert!(hub.routines.receive(&hub, &id, &h, b"{}").unwrap()["ignored"].is_string());

    // A core that does not answer: the sender is told to try again, and the key is nowhere in the words.
    fake.says(|_, _| Answer::Down { why: "no core is running".into(), lost: false });
    let held = hub.routines.receive(&hub, &id, &h, b"{}").unwrap_err();
    assert_eq!(held, Refused::Held);
    assert!(held.retry() && !held.to_string().contains("sekrit"));
    // A body that is not text cannot go through JSON unchanged: refused, never converted.
    assert!(matches!(hub.routines.receive(&hub, &id, &h, &[0xff, 0xfe]), Err(Refused::Invalid(_))));
    // A routine nobody has is an unknown routine.
    assert_eq!(hub.routines.receive(&hub, "nope", &h, b"{}").unwrap_err(), Refused::Unauthorized);
}

#[test]
fn several_triggers_are_several_jobs_and_a_routine_the_import_refused_has_none() {
    let hub = hub();
    let (_, id) = routine(
        &hub,
        Some("mel"),
        &json!([{"type": "interval", "seconds": 60}, {"type": "webhook"}, {"type": "event", "source": "github", "event": "*", "filters": {}}]),
    );
    let fake = switched(&hub, Gate::Core);
    fake.says(|_, p| {
        // The event job (3) takes it; the webhook job (2) does not match.
        if p["id"].as_str().is_some_and(|j| j.ends_with("-3")) {
            Answer::Done(json!({"outcome": "fired", "turn": "t"}))
        } else {
            Answer::Done(json!({"outcome": "ignored"}))
        }
    });
    assert!(hub.routines.receive(&hub, &id, &headers(&[]), b"{}").unwrap().get("turn").is_some());
    let jobs: Vec<_> = fake.calls_of("schedule.fire").iter().map(|c| c["id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(jobs, [format!("{id}-2"), format!("{id}-3")], "the interval trigger takes no deliveries");

    // No kith agent behind the bot: no job, so it answers as an unknown routine does.
    let other = self::hub();
    let (_, native) = routine(&other, None, &json!([{"type": "webhook"}]));
    let fake = switched(&other, Gate::Core);
    assert_eq!(other.routines.receive(&other, &native, &headers(&[]), b"{}").unwrap_err(), Refused::Unauthorized);
    assert!(fake.calls.lock().unwrap().is_empty());
}
