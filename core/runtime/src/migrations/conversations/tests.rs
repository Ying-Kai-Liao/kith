#![allow(clippy::many_single_char_names)]

use rusqlite::params;

use crate::db::{Core, MIGRATIONS};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

/// A database as the M2 lanes leave it: every step below the conversations lane.
fn m2_database(l: &Layout) -> i64 {
    let m2: Vec<_> = MIGRATIONS
        .iter()
        .copied()
        .filter(|m| m.version < 80)
        .collect();
    let head = m2.last().expect("m2 steps").version;
    drop(start_with(l, &m2).expect("m2 db"));
    head
}

fn conversation(c: &rusqlite::Connection, id: &str) {
    c.execute(
        "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'a', 'claude', 1, 1)",
        [id],
    )
    .expect("conversation");
}

#[test]
fn an_m2_database_migrates_to_conversations() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let head = m2_database(&l);
    let c = start_with(&l, MIGRATIONS).expect("migrates");
    assert_eq!(c.migrated_from(), Some(head));
    assert_eq!(
        c.schema_version(),
        MIGRATIONS.last().expect("steps").version
    );
    for t in ["conversations", "claims", "turns", "reports"] {
        let n: i64 = c
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [t],
                |r| r.get(0),
            )
            .expect("master");
        assert_eq!(n, 1, "{t}");
    }
}

#[test]
fn one_claim_tx_takes_a_claim_and_the_index_refuses_a_second_writer() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let mut c = start_with(&l, MIGRATIONS).expect("start");
    let g = c.generation();
    let tx = c.claim_tx().expect("tx");
    conversation(&tx, "c1");
    let id = Core::next_claim_id(&tx).expect("id");
    tx.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', ?2, 'turn', 'claimed', 1)",
        params![id, g],
    )
    .expect("claim");
    tx.execute(
        "INSERT INTO turns (conversation, claim_id, source, state, queued_at) VALUES ('c1', ?1, 'app', 'claimed', 1)",
        [id],
    )
    .expect("turn");
    // A second live claim on the conversation is refused by the row itself.
    let id2 = Core::next_claim_id(&tx).expect("id2");
    assert_eq!(id2, id + 1);
    let e = tx
        .execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', ?2, 'terminal', 'running', 1)",
            params![id2, g],
        )
        .expect_err("second writer");
    assert!(e.to_string().contains("UNIQUE"), "{e}");
    // Once the first has ended, the next one is fine; a queued claim never counts.
    tx.execute(
        "UPDATE claims SET state = 'ended', ended_at = 2 WHERE claim_id = ?1",
        [id],
    )
    .expect("end");
    tx.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', ?2, 'terminal', 'running', 1)",
        params![id2, g],
    )
    .expect("next writer");
    let id3 = Core::next_claim_id(&tx).expect("id3");
    tx.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', ?2, 'turn', 'queued', 1)",
        params![id3, g],
    )
    .expect("queued does not count");
    tx.commit().expect("commit");
}

#[test]
fn an_external_claim_names_its_process_and_a_park_names_its_reason() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let c = start_with(&l, MIGRATIONS).expect("start");
    let db = c.conn();
    conversation(db, "c1");
    assert!(
        db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (1, 'c1', 1, 'external', 'running', 1)",
            [],
        )
        .is_err(),
        "an external writer needs pid and start time"
    );
    db.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, pid, pid_started, created_at) VALUES (1, 'c1', 1, 'external', 'running', 42, 7, 1)",
        [],
    )
    .expect("external");
    assert!(
        db.execute(
            "INSERT INTO turns (conversation, source, state, queued_at) VALUES ('c1', 'schedule', 'parked', 1)",
            [],
        )
        .is_err(),
        "parked without a reason"
    );
    db.execute(
        "INSERT INTO turns (conversation, source, state, parked_reason, parked_at, deadline, queued_at) VALUES ('c1', 'schedule', 'parked', 'lock_busy', 2, 9, 1)",
        [],
    )
    .expect("parked with deadline");
}

#[test]
fn reports_are_kept_per_claim_and_a_retry_is_idempotent() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let c = start_with(&l, MIGRATIONS).expect("start");
    let db = c.conn();
    conversation(db, "c1");
    for id in [1, 2] {
        db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', 1, 'turn', 'ended', 1)",
            [id],
        )
        .expect("claim");
    }
    let put = |claim: i64, kind: &str, body: &str| {
        db.execute(
            "INSERT OR IGNORE INTO reports (claim_id, kind, body, received_at) VALUES (?1, ?2, ?3, 1)",
            params![claim, kind, body],
        )
        .expect("report")
    };
    assert_eq!(put(2, "done", "first"), 1);
    // The holder retries the same report: nothing changes.
    assert_eq!(put(2, "done", "retry"), 0);
    // A busy report for the same claim sits beside the unacked done, never over it.
    assert_eq!(put(2, "busy", "late"), 1);
    // A report for the older claim id is its own row and leaves the current claim's alone.
    assert_eq!(put(1, "done", "old"), 1);
    let body: String = db
        .query_row(
            "SELECT body FROM reports WHERE claim_id = 2 AND kind = 'done'",
            [],
            |r| r.get(0),
        )
        .expect("done");
    assert_eq!(body, "first");
    let unacked: i64 = db
        .query_row(
            "SELECT count(*) FROM reports WHERE claim_id = 2 AND acked_at IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(unacked, 2);
}

#[test]
fn a_claim_being_killed_is_still_the_writer_and_one_takeover_may_wait() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let c = start_with(&l, MIGRATIONS).expect("start");
    let db = c.conn();
    conversation(db, "c1");
    let put = |id: i64, state: &str| {
        db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, created_at) VALUES (?1, 'c1', 1, 'turn', ?2, 1)",
            params![id, state],
        )
    };
    put(1, "ending").expect("ending");
    assert!(put(2, "running").is_err(), "ending still holds the writer");
    put(2, "queued").expect("a take-over waits");
    assert!(put(3, "queued").is_err(), "at most one waiting");
    db.execute("UPDATE claims SET state = 'ended' WHERE claim_id = 1", [])
        .expect("gone");
    db.execute("UPDATE claims SET state = 'claimed' WHERE claim_id = 2", [])
        .expect("take-over proceeds");
}

#[test]
fn a_version_80_database_with_a_report_row_migrates_and_keeps_the_pid_the_report_names() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let v80: Vec<_> = MIGRATIONS
        .iter()
        .copied()
        .filter(|s| s.version <= 80)
        .collect();
    {
        let c = start_with(&l, &v80).expect("v80 db");
        assert_eq!(c.schema_version(), 80);
        let db = c.conn();
        conversation(db, "c1");
        db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, pid, pid_started, created_at)
             VALUES (1, 'c1', 1, 'turn', 'running', 100, 's100', 1)",
            [],
        )
        .expect("claim");
        for (kind, body) in [("done", r#"{"pid":100}"#), ("busy", "not json")] {
            db.execute(
                "INSERT INTO reports (claim_id, kind, body, received_at) VALUES (1, ?1, ?2, 1)",
                params![kind, body],
            )
            .expect("report");
        }
    }
    let c = start_with(&l, MIGRATIONS).expect("migrates");
    assert_eq!(c.migrated_from(), Some(80));
    let db = c.conn();
    let pid_of = |kind: &str| -> i64 {
        db.query_row(
            "SELECT pid FROM reports WHERE claim_id = 1 AND kind = ?1",
            [kind],
            |r| r.get(0),
        )
        .expect("report")
    };
    assert_eq!(pid_of("done"), 100);
    assert_eq!(pid_of("busy"), 0);
    let pid: i64 = db
        .query_row("SELECT pid FROM claims WHERE claim_id = 1", [], |r| {
            r.get(0)
        })
        .expect("claim pid");
    assert_eq!(pid, 100);
}

#[test]
fn a_version_81_database_with_live_claims_migrates_and_its_rows_stay_owner_level() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let v81: Vec<_> = MIGRATIONS
        .iter()
        .copied()
        .filter(|s| s.version <= 81)
        .collect();
    {
        let c = start_with(&l, &v81).expect("v81 db");
        assert_eq!(c.schema_version(), 81);
        let db = c.conn();
        conversation(db, "c1");
        conversation(db, "c2");
        db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, pid, pid_started, person, turn, created_at)
             VALUES (1, 'c1', 1, 'turn', 'running', 100, 'e100', 'sam', 1, 1)",
            [],
        )
        .expect("claim");
        db.execute(
            "INSERT INTO turns (id, conversation, claim_id, person, source, state, queued_at) VALUES (1, 'c1', 1, 'sam', 'app', 'running', 1)",
            [],
        )
        .expect("running turn");
        db.execute(
            "INSERT INTO turns (id, conversation, person, source, state, queued_at) VALUES (2, 'c2', 'sam', 'app', 'queued', 1)",
            [],
        )
        .expect("queued turn");
    }
    let mut c = start_with(&l, MIGRATIONS).expect("migrates");
    assert_eq!(c.migrated_from(), Some(81));
    {
        let db = c.conn();
        let one = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).expect(sql) };
        assert_eq!(
            one(
                "SELECT count(*) FROM turns WHERE level = 'owner' AND budget_left IS NULL AND room IS NULL AND author IS NULL"
            ),
            2
        );
        assert_eq!(one("SELECT count(*) FROM turn_people"), 0);
        assert_eq!(
            one("SELECT count(*) FROM conversations WHERE room IS NULL AND join_epoch IS NULL"),
            2
        );
        assert_eq!(one("SELECT count(*) FROM warm_procs"), 0);
        assert_eq!(
            one("SELECT count(*) FROM claims WHERE state = 'running' AND pid = 100"),
            1
        );
    }
    // The live claim is still the writer, and a revoke reaches it by its person as before.
    let r = crate::conversations::claims::revoke_person(&mut c, "sam", 5).expect("revoke");
    assert_eq!(r.kill.len(), 1);
    assert_eq!(r.kill[0].pid, Some(100));
    assert_eq!(r.cancelled_turns, 1, "the queued turn");
    let db = c.conn();
    let state: String = db
        .query_row("SELECT state FROM claims WHERE claim_id = 1", [], |r| {
            r.get(0)
        })
        .expect("state");
    assert_eq!(state, "ending");
}

#[test]
fn a_version_82_database_with_warm_processes_migrates_each_row_as_its_one_conversation() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let v82: Vec<_> = MIGRATIONS
        .iter()
        .copied()
        .filter(|s| s.version <= 82)
        .collect();
    {
        let c = start_with(&l, &v82).expect("v82 db");
        assert_eq!(c.schema_version(), 82);
        // A process with a conversation, one with none, one marked for a kill.
        for (pid, conv, kill) in [
            (10, Some("c1"), None),
            (11, None, None),
            (12, Some("c2"), Some(7)),
        ] {
            c.conn()
                .execute(
                    "INSERT INTO warm_procs (pid, started, instance, person, agent, conversation, kill_at)
                     VALUES (?1, 'e', 'kith-serve-host/1-a', 'ann', 'echo', ?2, ?3)",
                    params![pid, conv, kill],
                )
                .expect("warm row");
        }
    }
    let c = start_with(&l, MIGRATIONS).expect("migrates");
    assert_eq!(c.migrated_from(), Some(82));
    let db = c.conn();
    let row = |pid: i64| -> (String, Option<i64>, String) {
        db.query_row(
            "SELECT conversation, kill_at, person FROM warm_procs WHERE pid = ?1",
            [pid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row")
    };
    assert_eq!(row(10), ("c1".into(), None, "ann".into()));
    assert_eq!(row(11), (String::new(), None, "ann".into()));
    assert_eq!(row(12), ("c2".into(), Some(7), "ann".into()));
    // Now one process may hold several conversations; the same one twice is refused.
    let ins = |conv: &str| {
        db.execute(
            "INSERT INTO warm_procs (pid, started, instance, conversation) VALUES (10, 'e', 'i', ?1)",
            [conv],
        )
    };
    ins("c9").expect("a second conversation");
    assert!(ins("c9").is_err(), "one row per process and conversation");
}
