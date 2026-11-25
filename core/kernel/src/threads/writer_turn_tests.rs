//! A launch under a host-claimed turn (`KITH_TURN_CLAIM`, S4 WK3): `claim.attach` for the launcher's own pid and start
//! stamp and the conversation it runs, before `claim.external`; a refusal starts nothing and has its own exit code. The
//! core's side of the same calls (the check-and-set, "again", the release that leaves the claim) is
//! `core/runtime/src/conversations/detached_tests.rs`, through the real dispatch.

use std::cell::RefCell;
use std::time::Duration;

use serde_json::{Value, json};

use super::core::{Core, CoreErr};
use super::resolve::NoHost;
use super::tests::{Sleeper, Tmp, args, ctx, run_env};
use super::writer::{TURN_CLAIM_EXIT, Taker};
use super::*;

/// A core that answers `claim.attach` with `attach` (an error when `None`) and everything else as a free conversation; it
/// keeps what it was asked.
struct Fake {
    attach: Option<Value>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Fake {
    fn new(attach: Option<Value>) -> Self {
        Self {
            attach,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c.0.clone()).collect()
    }
}

impl Core for Fake {
    fn call(&self, method: &str, params: &Value, _: Option<Duration>) -> Result<Value, CoreErr> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        match (method, &self.attach) {
            ("claim.attach", Some(a)) => Ok(a.clone()),
            ("claim.attach", None) => Err(CoreErr::NoAnswer("no core daemon answers".to_owned())),
            ("claim.external", _) => Ok(json!({"claimId": 41})),
            _ => Ok(json!({})),
        }
    }
}

fn switched() -> Tmp {
    let t = Tmp::new();
    t.put("claims.tombstone", "");
    t
}

#[test]
fn the_claim_is_attached_for_the_launchers_pid_before_claim_external_and_again_on_the_next_step() {
    let t = switched();
    let env = run_env(&[("KITH_TURN_CLAIM", "41")]);
    let c = ctx(&t, &env);
    let core = Fake::new(Some(json!({"attached": true})));
    let holder = Sleeper::new();
    let taker = Taker::new(&c, Some(&core));
    // `resolve` takes the claim, then the one-shot's `claim` step asks again for the same pid.
    for _ in 0..2 {
        taker
            .claim_start("s-1", holder.pid(), "oneshot", Duration::ZERO, None, None)
            .expect("claim_start");
    }
    assert_eq!(
        core.asked(),
        [
            "claim.attach",
            "claim.external",
            "claim.attach",
            "claim.external"
        ]
    );
    let first = core.calls.borrow()[0].1.clone();
    assert_eq!(first["claimId"], 41);
    assert_eq!(first["conversation"], "s-1");
    assert_eq!(first["pid"], holder.pid());
    assert!(first["started"].as_str().is_some_and(|s| !s.is_empty()));
    // Nothing is released here: the claim is the host's to report.
    assert!(!core.asked().iter().any(|m| m == "claim.release"));
}

#[test]
fn a_refused_attach_starts_nothing_and_exits_with_its_own_code() {
    let t = switched();
    let env = run_env(&[("KITH_TURN_CLAIM", "41")]);
    let c = ctx(&t, &env);
    let holder = Sleeper::new();
    let pid = holder.pid().to_string();
    // Cancelled, adopted by a host restart, or another conversation's: the core answers `gone` for each.
    for state in ["cancelled", "queued", "claimed"] {
        let core = Fake::new(Some(json!({"refused": "gone", "state": state})));
        let o = cli::run(
            &c,
            &NoHost,
            Some(&core),
            &args(&["claim", "s-1", &pid]),
            &String::new,
        );
        assert_eq!(o.code, TURN_CLAIM_EXIT, "{state}: {}", o.stderr);
        assert!(
            o.stderr.contains("the turn claim was refused") && o.stderr.contains(state),
            "{}",
            o.stderr
        );
        assert_eq!(
            core.asked(),
            ["claim.attach"],
            "no claim.external after a refusal"
        );
    }
    // A core that does not answer is no attach either.
    let down = Fake::new(None);
    let o = cli::run(
        &c,
        &NoHost,
        Some(&down),
        &args(&["terminal", "s-1", &pid]),
        &String::new,
    );
    assert_eq!(o.code, TURN_CLAIM_EXIT, "{}", o.stderr);
    assert_eq!(down.asked(), ["claim.attach"]);
    // Not a claim id.
    let bad = run_env(&[("KITH_TURN_CLAIM", "soon")]);
    let o = cli::run(
        &ctx(&t, &bad),
        &NoHost,
        Some(&Fake::new(Some(json!({"attached": true})))),
        &args(&["claim", "s-1", &pid]),
        &String::new,
    );
    assert_eq!(o.code, TURN_CLAIM_EXIT, "{}", o.stderr);
}

#[test]
fn without_the_variable_nothing_is_attached() {
    let t = switched();
    let holder = Sleeper::new();
    for env in [run_env(&[]), run_env(&[("KITH_TURN_CLAIM", "")])] {
        let c = ctx(&t, &env);
        let core = Fake::new(Some(json!({"refused": "gone", "state": "ended"})));
        Taker::new(&c, Some(&core))
            .claim_start("s-1", holder.pid(), "oneshot", Duration::ZERO, None, None)
            .expect("claim_start");
        assert_eq!(core.asked(), ["claim.external"]);
    }
}

#[test]
fn in_files_mode_a_claimed_turn_is_refused_and_nothing_is_asked() {
    // No tombstone: the claims are files, so the host's claim has nothing to attach to.
    let t = Tmp::new();
    let env = run_env(&[("KITH_TURN_CLAIM", "41")]);
    let c = ctx(&t, &env);
    let core = Fake::new(Some(json!({"attached": true})));
    let holder = Sleeper::new();
    let pid = holder.pid().to_string();
    for verb in ["claim", "terminal"] {
        let o = cli::run(
            &c,
            &NoHost,
            Some(&core),
            &args(&[verb, "s-1", &pid]),
            &String::new,
        );
        assert_eq!(o.code, TURN_CLAIM_EXIT, "{verb}: {}", o.stderr);
        assert!(
            o.stderr.contains("the turn claim was refused"),
            "{}",
            o.stderr
        );
    }
    assert!(core.asked().is_empty(), "{:?}", core.asked());
}

#[test]
fn a_run_without_a_conversation_attaches_for_its_own_pid_and_says_why_it_did_not() {
    use super::writer::AttachErr;
    let t = switched();
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    // The launch's own pid: the claim names the process the engine descends from (sched-turn S-R1).
    let me = i64::from(std::process::id());
    let core = Fake::new(Some(json!({"attached": true})));
    Taker::new(&c, Some(&core))
        .attach_run(41, me)
        .expect("attached");
    let asked = core.calls.borrow()[0].clone();
    assert_eq!(asked.0, "claim.attach");
    assert_eq!(asked.1["claimId"], 41);
    assert_eq!(asked.1["pid"], me);
    assert!(asked.1["started"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(asked.1.get("conversation").is_none());
    let gone = Fake::new(Some(json!({"refused": "gone", "state": "cancelled"})));
    assert_eq!(
        Taker::new(&c, Some(&gone)).attach_run(41, me),
        Err(AttachErr::Gone("cancelled".to_owned()))
    );
    let down = Fake::new(None);
    assert!(matches!(
        Taker::new(&c, Some(&down)).attach_run(41, me),
        Err(AttachErr::Down(_))
    ));
}
