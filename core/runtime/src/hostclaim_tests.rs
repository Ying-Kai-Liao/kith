//! The `hostclaim.*` rows (M3-10f) on a real core.db: one writer per conversation, a stop that stays the writer until the
//! process is confirmed gone, the refusals as data, and the rows open to the host principal alone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_api::Principal;
use serde_json::{Value, json};

use super::call_at;
use crate::conversations::claims::reconcile;
use crate::db::{Core, MIGRATIONS};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

const MS: i64 = 1_900_000_000_000;

fn world() -> (tempfile::TempDir, Core, std::path::PathBuf) {
    let (t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).expect("start");
    reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
    (t, core, m)
}

fn row(core: &mut Core, method: &str, p: &Value) -> Value {
    call_at(core, method, p, MS).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn ensure(core: &mut Core, sid: &str) {
    row(
        core,
        "hostclaim.ensure",
        &json!({"conversation": sid, "agent": "echo", "engine": "claude"}),
    );
}

fn terminal(core: &mut Core, sid: &str, person: &str) -> Value {
    let epoch = row(core, "hostclaim.epoch", &json!({"person": person}))["epoch"].clone();
    row(
        core,
        "hostclaim.claim",
        &json!({"conversation": sid, "kind": "terminal", "person": person, "revokeSeen": epoch}),
    )
}

#[test]
fn the_rows_are_the_hosts_alone() {
    for name in [
        "hostclaim.ensure",
        "hostclaim.epoch",
        "hostclaim.claim",
        "hostclaim.enqueue",
        "hostclaim.turn",
        "hostclaim.redeem",
        "hostclaim.report",
        "hostclaim.stop",
        "hostclaim.settle",
        "hostclaim.show",
        "hostclaim.cancel",
        "hostclaim.park",
        "hostclaim.unpark",
    ] {
        let m = kith_api::method(name).unwrap_or_else(|| panic!("{name} is not in the table"));
        assert_eq!(m.principals, &[Principal::Host], "{name}");
        assert!(m.cli.hidden, "{name} has no command line");
        assert_eq!(m.stability, kith_api::Stability::Experimental, "{name}");
    }
}

#[test]
fn one_writer_per_conversation_and_a_refusal_is_data() {
    let (_t, mut core, _m) = world();
    ensure(&mut core, "s1");
    let first = terminal(&mut core, "s1", "");
    assert!(first["claimId"].is_i64(), "{first}");
    let second = terminal(&mut core, "s1", "");
    assert_eq!(second["refused"], "held", "{second}");
    // Another conversation is free.
    ensure(&mut core, "s2");
    assert!(terminal(&mut core, "s2", "")["claimId"].is_i64());
    // An unknown conversation is refused, not created by a claim.
    assert_eq!(terminal(&mut core, "nope", "")["refused"], "other");
    let shown = row(&mut core, "hostclaim.show", &json!({"conversation": "s1"}));
    assert_eq!(shown["claim"]["state"], "claimed");
    assert_eq!(shown["claim"]["kind"], "terminal");
}

#[test]
fn a_take_over_waits_for_ended_and_the_writer_holds_until_then() {
    let (_t, mut core, _m) = world();
    ensure(&mut core, "s1");
    // The terminal's process: this test process, so the core confirms it alive.
    let pid = i64::from(std::process::id());
    let started = kith_kernel::procstart::start_stamp(std::process::id()).expect("own start");
    let id = terminal(&mut core, "s1", "")["claimId"].as_i64().unwrap();
    let red = row(
        &mut core,
        "hostclaim.redeem",
        &json!({"claimId": id, "pid": pid, "started": started, "holderInstance": "h"}),
    );
    assert_eq!(red["running"], true, "{red}");
    // The stop makes it `ending`: still the writer, so a second claim is refused.
    let stop = row(
        &mut core,
        "hostclaim.stop",
        &json!({"claimId": id, "reason": "taken over in the app"}),
    );
    assert_eq!(stop["kill"]["pid"], pid, "{stop}");
    let held = terminal(&mut core, "s1", "");
    assert_eq!(held["refused"], "held", "ending is still a writer: {held}");
    assert_eq!(
        row(&mut core, "hostclaim.show", &json!({"claimId": id}))["claim"]["state"],
        "ending"
    );
    // The process is alive: settle ends nothing.
    assert_eq!(
        row(&mut core, "hostclaim.settle", &json!({}))["ended"],
        json!([])
    );
    // The terminal reports its exit with that pid: ended, and only now a claim is taken.
    let rep = row(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": id, "kind": "exit", "body": json!({"pid": pid}).to_string()}),
    );
    assert_eq!(rep["applied"], "exit", "{rep}");
    assert_eq!(
        row(&mut core, "hostclaim.show", &json!({"claimId": id}))["claim"]["state"],
        "ended"
    );
    assert!(terminal(&mut core, "s1", "")["claimId"].is_i64());
}

#[test]
fn a_revoked_person_is_refused_and_the_caller_cancels_the_turn() {
    let (_t, mut core, _m) = world();
    let epoch = row(&mut core, "hostclaim.epoch", &json!({"person": "sam"}))["epoch"].clone();
    let q = row(
        &mut core,
        "hostclaim.enqueue",
        &json!({"conversation": "s1", "agent": "echo", "engine": "claude", "source": "app", "person": "sam",
                "prompt": "hi", "revokeSeen": epoch}),
    );
    let turn = q["turn"].as_i64().unwrap();
    // A revoke commits after the may check: the epoch the caller carried is stale.
    crate::conversations::claims::revoke_person(&mut core, "sam", MS).unwrap();
    let again = row(
        &mut core,
        "hostclaim.enqueue",
        &json!({"conversation": "s1", "agent": "echo", "engine": "claude", "source": "app", "person": "sam",
                "prompt": "hi", "revokeSeen": epoch}),
    );
    assert_eq!(again["refused"], "revoked", "{again}");
    // The revoke sweep cancelled the first turn already; cancelling again is a no-op, never an error.
    let c = row(
        &mut core,
        "hostclaim.cancel",
        &json!({"turn": turn, "reason": "revoked"}),
    );
    assert_eq!(c["cancelled"], false);
}

#[test]
fn park_unpark_and_cancel_a_queued_turn() {
    let (_t, mut core, _m) = world();
    let q = row(
        &mut core,
        "hostclaim.enqueue",
        &json!({"conversation": "s1", "agent": "echo", "engine": "claude", "source": "app", "person": "",
                "prompt": "hi", "revokeSeen": 0}),
    );
    let turn = q["turn"].as_i64().unwrap();
    assert_eq!(
        row(
            &mut core,
            "hostclaim.park",
            &json!({"turn": turn, "reason": "terminal_live"})
        )["parked"],
        true
    );
    // A terminal is still live: stays parked; once it is not, the turn is queued again.
    let stay = row(
        &mut core,
        "hostclaim.unpark",
        &json!({"conversation": "s1", "lockFree": true, "terminalLive": true}),
    );
    assert_eq!(stay["turns"], json!([]));
    let woke = row(
        &mut core,
        "hostclaim.unpark",
        &json!({"conversation": "s1", "lockFree": true, "terminalLive": false}),
    );
    assert_eq!(woke["turns"], json!([turn]));
    assert_eq!(
        row(
            &mut core,
            "hostclaim.cancel",
            &json!({"turn": turn, "reason": "refused"})
        )["cancelled"],
        true
    );
    assert_eq!(
        row(
            &mut core,
            "hostclaim.park",
            &json!({"turn": turn, "reason": "lock_busy"})
        )["parked"],
        false
    );
}

#[cfg(unix)]
#[test]
fn a_turn_claim_goes_through_claim_next() {
    let (_t, mut core, m) = world();
    let agent = m.join("agents/echo/role");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("agent.md"),
        "---\nname: echo\nharness: claude-code\n---\nbody\n",
    )
    .unwrap();
    let q = row(
        &mut core,
        "hostclaim.enqueue",
        &json!({"conversation": "s1", "agent": "echo", "engine": "claude", "source": "app", "person": "",
                "prompt": "hi", "revokeSeen": 0}),
    );
    let turn = q["turn"].as_i64().unwrap();
    let ask = json!({"conversation": "s1", "turn": turn, "person": "", "revokeSeen": 0, "cwd": "/work",
        "sessionId": "s1", "home": m.join("home"), "kernel": m.join("kernel"),
        "env": {"KITH_MACHINE": m, "KITH_AGENTS": m.join("agents")}});
    // A direct turn claim is refused by the claims API itself: this row is the only way in and it is claim_next. No
    // account is set up on this tmp machine, so it answers claimed or exhausted; it never skips its pick.
    let got = row(&mut core, "hostclaim.turn", &ask);
    let status = got["status"].as_str().unwrap_or_default();
    assert!(status == "claimed" || status == "exhausted", "{got}");
    if status == "claimed" {
        let again = row(&mut core, "hostclaim.turn", &ask);
        assert!(
            again["refused"] == "held" || again["refused"] == "notWaiting",
            "{again}"
        );
    }
}
