//! `terminal.open` once the claims are the core's (P5): the `terminal` claim row is the writer and no claim file is taken,
//! read or given back. Through a real daemon and holder, like the tests of `terminal_tests.rs`; the switch is the one
//! `kith migrate step claims` will write (the switch file and the marker in core.db).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::tests::daemon::{RUNNING, Rig, answered, open, plain, until};
use crate::conversations::warm::{self, WarmProc};
use crate::cutover::state::{self, Phase, Switch};
use crate::layout::Layout;

fn switch(rig: &Rig) {
    state::write(
        &Layout::new(&rig.m),
        "claims",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 7,
        },
    )
    .unwrap();
    rig.d
        .shared()
        .with_core(|c| {
            crate::cutover::marker::write(c, "claims", 7, "d", None)?;
            // The claims open once the holder's hello has been reconciled; the rig's holder lists nothing.
            crate::conversations::claims::reconcile(c, &[], &[], i64::MAX, 1)?;
            Ok(())
        })
        .unwrap();
}

fn claim_file(rig: &Rig, sid: &str) -> std::path::PathBuf {
    rig.m.join("live/terminal").join(format!("{sid}.claim"))
}

fn claim_rows(rig: &Rig, sid: &str) -> Vec<(String, String, Option<i64>)> {
    rig.d
        .shared()
        .with_core(|c| {
            let mut st = c.conn().prepare(
                "SELECT kind, state, pid FROM claims WHERE conversation = ?1 ORDER BY claim_id",
            )?;
            Ok(st
                .query_map([sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap()
}

#[test]
fn a_switched_terminal_is_a_core_claim_and_writes_no_claim_file() {
    let rig = Rig::new(RUNNING);
    switch(&rig);
    // The session's pending intent ends, as a file take would have ended it.
    let intent = rig.m.join("sessions/intent");
    std::fs::create_dir_all(&intent).unwrap();
    std::fs::write(intent.join("sess-1.json"), "{}").unwrap();
    let a = answered(open(&rig.m, "sess-1", &["sh", "-c", "cat"], &plain()));
    assert_eq!(a["status"], "open", "{a}");
    assert!(
        !claim_file(&rig, "sess-1").exists(),
        "no claim file is written"
    );
    assert!(
        !intent.join("sess-1.json").exists(),
        "the taker's bookkeeping stays"
    );
    let rows = claim_rows(&rig, "sess-1");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str()),
        ("terminal", "running")
    );
    assert_eq!(
        rows[0].2,
        a["pid"].as_i64(),
        "the claim names the stub's pid"
    );
    // A second open of the session is busy, and the core says who holds it.
    let again = answered(open(&rig.m, "sess-1", &["sh", "-c", "cat"], &plain()));
    assert_eq!(again["status"], "busy", "{again}");
    assert_eq!(again["held_by"]["pid"], a["pid"]);
    assert_eq!(again["held_by"]["kind"], "terminal");
    assert!(!claim_file(&rig, "sess-1").exists());
}

#[test]
fn a_switched_terminal_ignores_a_stale_claim_file_of_the_old_days() {
    let rig = Rig::new(RUNNING);
    switch(&rig);
    // A file left from before the switch (P9 imports it): never read, so it neither blocks nor crashes the open.
    std::fs::create_dir_all(rig.m.join("live/terminal")).unwrap();
    std::fs::write(claim_file(&rig, "old-1"), r#"{"pid":1,"kind":"terminal"}"#).unwrap();
    std::fs::write(rig.m.join("live/terminal/old-1.takeover.claim"), "not json").unwrap();
    let a = answered(open(&rig.m, "old-1", &["sh", "-c", "cat"], &plain()));
    assert_eq!(a["status"], "open", "{a}");
}

#[test]
fn a_switched_terminal_waits_for_the_apps_idle_agent_and_for_a_takeover() {
    let rig = Rig::new(RUNNING);
    switch(&rig);
    rig.d
        .shared()
        .with_core(|c| {
            c.conn().execute(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('idle-1', 'kai', 'claude', 1, 1)",
                [],
            )?;
            warm::register(
                c,
                &WarmProc {
                    pid: 4_000_001,
                    started: "w1",
                    instance: "kith-serve-host/1-e1",
                    person: "",
                    agent: "kai",
                    conversation: Some("idle-1"),
                    revoke_seen: None,
                },
            )
        })
        .unwrap();
    let a = answered(open(&rig.m, "idle-1", &["sh", "-c", "cat"], &plain()));
    assert_eq!(a["status"], "busy", "{a}");
    assert!(
        a["note"].as_str().unwrap().contains("idle agent"),
        "the note says why: {a}"
    );
    let kill_at: Option<i64> = rig
        .d
        .shared()
        .with_core(|c| {
            Ok(c.conn().query_row(
                "SELECT kill_at FROM warm_procs WHERE pid = 4000001",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(kill_at.is_some(), "the idle process is marked for the kill");
    assert!(
        claim_rows(&rig, "idle-1").is_empty(),
        "no claim while it lives"
    );

    rig.d
        .shared()
        .with_core(|c| {
            c.conn().execute(
                "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('take-1', 'kai', 'claude', 1, 1)",
                [],
            )?;
            c.conn().execute(
                "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at)
                 VALUES (9000, 'take-1', 1, 'terminal', 'queued', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let t = answered(open(&rig.m, "take-1", &["sh", "-c", "cat"], &plain()));
    assert_eq!(t["status"], "busy", "{t}");
    assert_eq!(t["held_by"]["kind"], "takeover");
}

#[test]
fn a_marker_file_alone_does_not_switch_the_terminal() {
    let rig = Rig::new(RUNNING);
    std::fs::create_dir_all(rig.m.join("live/terminal")).unwrap();
    std::fs::write(rig.m.join("live/terminal/claims.tombstone"), "{}").unwrap();
    let a = answered(open(&rig.m, "sess-2", &["sh", "-c", "cat"], &plain()));
    assert_eq!(a["status"], "open", "{a}");
    until("today's claim file", || {
        claim_file(&rig, "sess-2").is_file()
    });
    let claim: Value =
        serde_json::from_str(&std::fs::read_to_string(claim_file(&rig, "sess-2")).unwrap())
            .unwrap();
    assert_eq!(
        claim["pid"], a["pid"],
        "not switched: the file is the writer"
    );
    assert_eq!(json!(claim_rows(&rig, "sess-2").len()), 0);
}
