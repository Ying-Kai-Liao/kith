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
        "hostclaim.proc",
        "hostclaim.procGone",
        "hostclaim.adopt",
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

/// A queued turn claimed (as `claim_next` does, through the claims API) and redeemed on this process; its claim id and turn.
fn running_turn(core: &mut Core, sid: &str) -> (i64, i64) {
    use crate::conversations::claims::{Kind, NewClaim, claim};
    let q = row(
        core,
        "hostclaim.enqueue",
        &json!({"conversation": sid, "agent": "echo", "engine": "claude", "source": "app", "person": "",
                "prompt": "", "revokeSeen": 0}),
    );
    let turn = q["turn"].as_i64().unwrap();
    let id = claim(
        core,
        &NewClaim {
            conversation: sid,
            kind: Kind::Turn,
            person: "owner",
            turn: Some(turn),
            holder_instance: Some("kith-serve-host"),
            account: None,
            process: None,
            revoke_seen: 0,
        },
        MS,
    )
    .expect("claim")
    .claim_id;
    let started = kith_kernel::procstart::start_stamp(std::process::id()).expect("own start");
    let red = row(
        core,
        "hostclaim.redeem",
        &json!({"claimId": id, "pid": i64::from(std::process::id()), "started": started,
                "holderInstance": "kith-serve-host"}),
    );
    assert_eq!(red["running"], true, "{red}");
    (id, turn)
}

fn turn_state(core: &Core, turn: i64) -> String {
    core.conn()
        .query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn a_host_turn_that_ends_done_is_not_requeued_but_an_exit_requeues_it() {
    let (_t, mut core, _m) = world();
    let pid = i64::from(std::process::id());
    // The host's release of a finished turn reports `done`: the turn is done and nothing is queued again.
    let (id, turn) = running_turn(&mut core, "s1");
    let rep = row(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": id, "kind": "done", "body": json!({"pid": pid}).to_string()}),
    );
    assert_eq!(rep["applied"], "done", "{rep}");
    assert_eq!(turn_state(&core, turn), "done");
    assert_eq!(
        row(&mut core, "hostclaim.show", &json!({"claimId": id}))["claim"]["state"],
        "ended"
    );
    // `exit` is the holder's report of a process that died mid-run: the core requeues the turn (not a host turn's end).
    let (id, turn) = running_turn(&mut core, "s2");
    row(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": id, "kind": "exit", "body": json!({"pid": pid}).to_string()}),
    );
    assert_eq!(turn_state(&core, turn), "queued");
}

#[test]
fn show_names_the_holder_instance_so_a_restarted_host_can_end_its_own_stale_claim() {
    let (_t, mut core, _m) = world();
    // A host turn claim is left `running` (the host died before it reported). The host that comes up with no memory reads
    // the instance from `show`, ends the claim as a finished turn, and the conversation can be claimed again.
    let (id, turn) = running_turn(&mut core, "s1");
    let shown = row(&mut core, "hostclaim.show", &json!({"conversation": "s1"}));
    assert_eq!(
        shown["claim"]["holderInstance"], "kith-serve-host",
        "{shown}"
    );
    assert_eq!(shown["claim"]["claimId"], id);
    assert!(
        terminal(&mut core, "s1", "").get("refused").is_some(),
        "held until it is ended"
    );
    let rep = row(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": id, "kind": "done", "body": json!({"pid": shown["claim"]["pid"]}).to_string()}),
    );
    assert_eq!(rep["applied"], "done", "{rep}");
    assert_eq!(turn_state(&core, turn), "done", "not requeued");
    assert!(
        terminal(&mut core, "s1", "")["claimId"].is_i64(),
        "and claimed again"
    );
    // A claim of the kernel's (no instance) shows none, so the host never reads it as its own.
    ensure(&mut core, "s2");
    let other = terminal(&mut core, "s2", "")["claimId"].as_i64().unwrap();
    let shown = row(&mut core, "hostclaim.show", &json!({"claimId": other}));
    assert!(shown["claim"]["holderInstance"].is_null(), "{shown}");
}

#[test]
fn a_claim_left_ending_by_a_stop_is_ended_by_done_and_the_turn_stays_cancelled() {
    let (_t, mut core, _m) = world();
    let pid = i64::from(std::process::id());
    let (id, turn) = running_turn(&mut core, "s1");
    // A stop (not a requeue) of a running turn claim: the process is alive, so the claim stays the writer as `ending`.
    let stop = row(
        &mut core,
        "hostclaim.stop",
        &json!({"claimId": id, "reason": "stopped by the user"}),
    );
    assert_eq!(stop["kill"]["pid"], pid, "{stop}");
    let shown = row(&mut core, "hostclaim.show", &json!({"claimId": id}));
    assert_eq!(shown["claim"]["state"], "ending", "{shown}");
    assert_eq!(terminal(&mut core, "s1", "")["refused"], "held");
    // The host's release reports `done` with the same pid: the claim ends; the turn is neither done nor queued again.
    let rep = row(
        &mut core,
        "hostclaim.report",
        &json!({"claimId": id, "kind": "done", "body": json!({"pid": pid}).to_string()}),
    );
    assert_eq!(rep["applied"], "done", "{rep}");
    assert_eq!(
        row(&mut core, "hostclaim.show", &json!({"claimId": id}))["claim"]["state"],
        "ended"
    );
    assert_eq!(turn_state(&core, turn), "cancelled");
    assert!(
        terminal(&mut core, "s1", "")["claimId"].is_i64(),
        "claimable again"
    );
}

fn room(core: &mut Core, extra: &Value) -> Value {
    let mut p = json!({
        "conversation": "rc1", "agent": "echo", "engine": "claude", "source": "room",
        "person": "room-r1", "prompt": "hi", "revokeSeen": 0, "level": "room", "basis": ["ann"],
        "epochs": {"ann": 0}, "room": "r1", "gen": 4, "joinEpoch": 1, "author": "ann", "budget": 1,
    });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    row(core, "hostclaim.enqueue", &p)
}

/// The room fields of `hostclaim.enqueue` and the three process rows, as the host calls them: refusals are data, a retry
/// with the same `key` answers the same turn.
#[test]
fn the_room_rows_carry_the_basis_the_budget_and_the_processes() {
    let (_t, mut core, _m) = world();
    let root = room(&mut core, &json!({}));
    let root_id = root["turn"].as_i64().unwrap_or_else(|| panic!("{root}"));
    let follow = json!({"author": "bot:1", "budget": null, "budgetFrom": root_id, "key": "k"});
    let one = room(&mut core, &follow.clone());
    assert!(one["turn"].is_i64(), "{one}");
    assert_eq!(
        room(&mut core, &follow)["turn"],
        one["turn"],
        "a retry is the same turn"
    );
    let spent = room(
        &mut core,
        &json!({"author": "bot:1", "budget": null, "budgetFrom": one["turn"]}),
    );
    assert_eq!(spent["refused"], "budget", "{spent}");
    let bot = room(&mut core, &json!({"author": "bot:1"}));
    assert_eq!(bot["refused"], "other", "{bot}");
    let bare = room(&mut core, &json!({"basis": []}));
    assert_eq!(bare["refused"], "other", "{bare}");
    // ann is revoked: the same enqueue with the epoch she had is refused as revoked.
    crate::conversations::claims::revoke_person(&mut core, "ann", MS).expect("revoke");
    let stale = room(&mut core, &json!({"key": "k2"}));
    assert_eq!(stale["refused"], "revoked", "{stale}");
    // The key of the conversation: a second (agent, room, epoch) row is refused as data.
    let dup = row(
        &mut core,
        "hostclaim.ensure",
        &json!({"conversation": "rc2", "agent": "echo", "engine": "claude", "room": "r1", "joinEpoch": 1}),
    );
    assert_eq!(dup["refused"], "other", "{dup}");
    // Processes: register, move, revoke-reach, forget.
    let w = json!({"pid": 900, "started": "e900", "instance": "kith-serve-host/1-a", "person": "bob", "agent": "echo", "revokeSeen": 0});
    assert_eq!(row(&mut core, "hostclaim.proc", &w)["registered"], true);
    assert_eq!(
        row(
            &mut core,
            "hostclaim.procGone",
            &json!({"pid": 900, "started": "e900"})
        )["gone"],
        true
    );
    assert_eq!(
        row(
            &mut core,
            "hostclaim.procGone",
            &json!({"pid": 900, "started": "e900"})
        )["gone"],
        false
    );
    let adopted = row(
        &mut core,
        "hostclaim.adopt",
        &json!({"instance": "kith-serve-host/2-b"}),
    );
    assert_eq!(adopted["ended"], json!([]), "{adopted}");
}

#[test]
fn bad_room_params_are_errors_not_quiet_choices() {
    let (_t, mut core, _m) = world();
    let both = json!({
        "conversation": "rc1", "agent": "echo", "engine": "claude", "source": "room", "person": "room-r1",
        "prompt": "hi", "revokeSeen": 0, "level": "room", "basis": ["ann"], "epochs": {"ann": 0},
        "room": "r1", "joinEpoch": 1, "author": "ann", "budget": 1, "budgetFrom": 5,
    });
    assert!(call_at(&mut core, "hostclaim.enqueue", &both, MS).is_err());
    // A guest's process without the epoch is an error; a stale one is data.
    let w = json!({"pid": 9, "started": "e9", "instance": "kith-serve-host/1-a", "person": "ann"});
    assert!(call_at(&mut core, "hostclaim.proc", &w, MS).is_err());
    crate::conversations::claims::revoke_person(&mut core, "ann", MS).expect("revoke");
    let mut stale = w.clone();
    stale["revokeSeen"] = json!(0);
    assert_eq!(
        row(&mut core, "hostclaim.proc", &stale)["refused"],
        "revoked"
    );
}
