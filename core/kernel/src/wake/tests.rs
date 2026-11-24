#![allow(clippy::unwrap_used)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        let d = std::env::temp_dir().join(format!(
            "kith-wake-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

/// Answers each method with `answer`; keeps what it was asked.
struct Fake {
    answer: Result<Value, Fail>,
    asked: RefCell<Vec<(String, Value)>>,
}

impl Fake {
    fn says(answer: Value) -> Self {
        Self {
            answer: Ok(answer),
            asked: RefCell::new(Vec::new()),
        }
    }

    fn fails(why: Fail) -> Self {
        Self {
            answer: Err(why),
            asked: RefCell::new(Vec::new()),
        }
    }

    fn call(&self) -> impl Fn(&str, &Value) -> Result<Value, Fail> + '_ {
        |m, p| {
            self.asked.borrow_mut().push((m.to_owned(), p.clone()));
            match &self.answer {
                Ok(v) => Ok(v.clone()),
                Err(Fail::Refused(w)) => Err(Fail::Refused(w.clone())),
                Err(Fail::NoAnswer(w)) => Err(Fail::NoAnswer(w.clone())),
            }
        }
    }
}

fn kith(line: &str, env: &[(&str, &str)], fake: &Fake) -> (i32, String, String) {
    let get = |k: &str| {
        env.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&words(line), &get, &fake.call(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn the_tombstone_or_a_switch_record_past_stepping_makes_the_queue_the_cores() {
    let t = Tmp::new();
    let (state, machine) = (t.0.join("state"), t.0.join("machine"));
    assert!(!switched(Some(&state), Some(&machine)));
    let cutover = machine.join("core").join("cutover");
    std::fs::create_dir_all(&cutover).unwrap();
    std::fs::write(cutover.join("wake.switch"), "state=stepping\n").unwrap();
    assert!(
        !switched(Some(&state), Some(&machine)),
        "a step part-way is still the files"
    );
    std::fs::write(cutover.join("wake.switch"), "state=switched\n").unwrap();
    assert!(switched(Some(&state), Some(&machine)));
    std::fs::write(
        cutover.join("wake.switch"),
        "state=switched\nstate=stepping\n",
    )
    .unwrap();
    assert!(
        !switched(Some(&state), Some(&machine)),
        "the last state= line wins"
    );
    std::fs::write(cutover.join("wake.switch"), "at=1\n").unwrap();
    assert!(
        switched(Some(&state), Some(&machine)),
        "no state= line is damaged: switched"
    );
    std::fs::write(cutover.join("wake.switch"), "state=switched\n").unwrap();
    std::fs::remove_file(cutover.join("wake.switch")).unwrap();
    let queue = state.join("sessions").join("queue");
    std::fs::create_dir_all(&queue).unwrap();
    std::fs::write(queue.join(TOMBSTONE), "{}").unwrap();
    assert!(
        switched(Some(&state), None),
        "the tombstone alone is enough"
    );
}

#[test]
fn enqueue_sends_the_rows_params_and_prints_the_result() {
    let fake = Fake::says(json!({"result": "queued", "lines": ["queued"]}));
    let (code, out, _) = kith(
        "enqueue s1 --id card:1 --agent designer --person anna --door host --why x -- yes, go",
        &[],
        &fake,
    );
    assert_eq!((code, out.as_str()), (0, "queued\n"));
    let asked = fake.asked.borrow();
    assert_eq!(asked[0].0, "wake.enqueue");
    assert_eq!(
        asked[0].1,
        json!({"session": "s1", "id": "card:1", "agent": "designer", "text": ["yes, go"], "door": "host",
               "why": "x", "person": "anna"})
    );
}

#[test]
fn a_core_that_does_not_answer_is_a_refusal_and_a_rejection_says_why() {
    let down = Fake::fails(Fail::NoAnswer("no core daemon answers".into()));
    let (code, out, err) = kith("withdraw s1", &[], &down);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("did not answer") && err.contains("may not have been made"));
    let no = Fake::fails(Fail::Refused(
        "kith-core: s1 belongs to another agent".into(),
    ));
    let (code, _, err) = kith("enqueue s1 --id a --agent b -- x", &[], &no);
    assert_eq!(
        (code, err.as_str()),
        (1, "kith-wake: s1 belongs to another agent\n")
    );
}

#[test]
fn withdraw_and_status_print_what_the_core_says() {
    let fake = Fake::says(json!({"count": 2, "lines": ["withdrew 2"]}));
    assert_eq!(kith("withdraw s1 --id x", &[], &fake).1, "withdrew 2\n");
    assert_eq!(
        fake.asked.borrow()[0].1,
        json!({"session": "s1", "id": "x"})
    );
    assert_eq!(
        kith("withdraw-person anna bob", &[], &fake).1,
        "withdrew 2\n"
    );
    assert_eq!(
        fake.asked.borrow()[1].1,
        json!({"persons": ["anna", "bob"]})
    );
    let st =
        Fake::says(json!({"id": "i", "state": "answered", "run": "r1", "lines": ["answered r1"]}));
    assert_eq!(kith("status i", &[], &st).1, "answered r1\n");
    assert_eq!(
        kith("status i --json", &[], &st).1,
        "{\"id\": \"i\", \"state\": \"answered\", \"run\": \"r1\"}\n"
    );
    let none =
        Fake::says(json!({"id": "i", "state": "unknown", "run": null, "lines": ["unknown"]}));
    assert_eq!(
        kith("status i --json", &[], &none).1,
        "{\"id\": \"i\", \"state\": \"unknown\", \"run\": null}\n"
    );
}

#[test]
fn take_done_and_back_end_as_kith_wake_does() {
    let taken = Fake::says(json!({"turn": {"run": "r"}, "lines": ["{\"run\":\"r\"}"]}));
    let (code, out, _) = kith("take --holder 7 --session s1", &[], &taken);
    assert_eq!((code, out.as_str()), (0, "{\"run\":\"r\"}\n"));
    assert_eq!(
        taken.asked.borrow()[0].1,
        json!({"holder": 7, "session": "s1"})
    );
    let nothing = Fake::says(json!({"turn": null, "lines": []}));
    assert_eq!(kith("take --holder 7", &[], &nothing).0, 1);
    let done = Fake::says(json!({"done": true, "lines": []}));
    assert_eq!(kith("done r1 --how withdrawn", &[], &done).0, 0);
    assert_eq!(
        done.asked.borrow()[0].1,
        json!({"run": "r1", "how": "withdrawn"})
    );
    assert_eq!(kith("back r1 --held", &[], &done).0, 0);
    assert_eq!(done.asked.borrow()[1].1, json!({"run": "r1", "held": true}));
    let gone = Fake::says(json!({"done": false, "lines": []}));
    assert_eq!(kith("done nope", &[], &gone).0, 1);
}

#[test]
fn wait_names_the_caller_and_drain_does_nothing() {
    let fake = Fake::says(json!({"waiter": {}, "lines": ["waiting w1: x"]}));
    let env = [
        ("CLAUDE_CODE_SESSION_ID", "s9"),
        ("KITH_AGENT", "coder"),
        ("KITH_VIA", "ask"),
    ];
    let (code, out, _) = kith("wait --on at:+30m -- ping me", &env, &fake);
    assert_eq!((code, out.as_str()), (0, "waiting w1: x\n"));
    assert_eq!(
        fake.asked.borrow()[0].1,
        json!({"on": "at:+30m", "timeout": "6h", "why": "", "note": ["ping", "me"], "via": "ask",
               "session": "s9", "agent": "coder"})
    );
    let idle = Fake::says(json!({}));
    assert_eq!(kith("drain", &[], &idle).0, 0);
    assert_eq!(kith("deliver s1", &[], &idle).0, 0);
    assert!(idle.asked.borrow().is_empty(), "no tick from here");
}

#[test]
fn ls_prints_the_cores_lines_and_the_items_as_json_dumps_indents_them() {
    let fake = Fake::says(
        json!({"items": [{"session": "s1"}], "waiters": [], "lines": ["s1  a  owner  run  owed  1 request(s)"]}),
    );
    assert_eq!(
        kith("ls", &[], &fake).1,
        "s1  a  owner  run  owed  1 request(s)\n"
    );
    assert_eq!(
        kith("ls --json", &[], &fake).1,
        "[\n {\n  \"session\": \"s1\"\n }\n]\n"
    );
    assert_eq!(kith("", &[], &fake).0, 0, "no verb is ls");
}

#[test]
fn released_makes_one_call_and_a_lost_one_is_fine() {
    let fake = Fake::says(json!({"unparked": 0}));
    released(&fake.call(), "s1");
    assert_eq!(
        *fake.asked.borrow(),
        [("wake.released".to_owned(), json!({"session": "s1"}))]
    );
    released(&Fake::fails(Fail::NoAnswer("down".into())).call(), "s1");
}
