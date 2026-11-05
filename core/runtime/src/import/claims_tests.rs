#![allow(clippy::unwrap_used)]

use std::path::Path;

use serde_json::json;

use super::{read_files, run_in};
use crate::db::{Core, MIGRATIONS};
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

fn core() -> (tempfile::TempDir, Core) {
    let (t, m) = machine();
    let c = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
    (t, c)
}

fn conversation(c: &Core, id: &str) {
    c.conn()
        .execute(
            "INSERT OR IGNORE INTO conversations (id, agent, engine, created_at, updated_at) VALUES (?1, 'a', 'claude', 1, 1)",
            [id],
        )
        .unwrap();
}

fn file(state: &Path, name: &str, body: &serde_json::Value) {
    let dir = state.join("live").join("terminal");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), body.to_string()).unwrap();
}

fn live(c: &Core, conv: &str) -> Vec<(String, String, i64, Option<String>)> {
    let mut st = c
        .conn()
        .prepare(
            "SELECT kind, state, pid, pid_started FROM claims WHERE conversation = ?1 AND state = 'running'",
        )
        .unwrap();
    st.query_map([conv], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The old `ps -o lstart=` text of a live pid, as the Python wrote it into claim files before the stamps.
fn lstart(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn live_claim_files_become_running_claim_rows_and_the_rest_are_listed() {
    let (t, mut c) = core();
    let state = t.path().join("state");
    let me = i64::from(std::process::id());
    for s in [
        "s-stamp",
        "s-old",
        "s-one",
        "s-gone",
        "s-start",
        "s-nowhere",
    ] {
        conversation(&c, s);
    }
    let stamp = match kith_kernel::procstart::probe(std::process::id()) {
        kith_kernel::procstart::Probe::Alive(Some(s)) => s,
        other => panic!("{other:?}"),
    };
    file(
        &state,
        "s-stamp.claim",
        &json!({"pid": me, "procStart": stamp, "at": "t", "kind": "terminal", "person": "sam"}),
    );
    // The form before the stamps: kept as written.
    let old = lstart(std::process::id());
    file(
        &state,
        "s-old.claim",
        &json!({"pid": me, "procStart": old, "at": "t", "kind": "terminal"}),
    );
    file(
        &state,
        "s-one.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "oneshot"}),
    );
    file(
        &state,
        "s-gone.claim",
        &json!({"pid": 2_000_000_000_i64, "procStart": null, "at": "t", "kind": "terminal"}),
    );
    file(
        &state,
        "s-start.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "starting"}),
    );
    file(
        &state,
        "s-one.takeover.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "terminal", "takeover": true}),
    );
    file(
        &state,
        "nowhere.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "terminal"}),
    );
    std::fs::write(state.join("live/terminal/junk.claim"), "this is not json").unwrap();

    let dry = run_in(&mut c, &state, true).unwrap();
    assert!(dry.changed, "{dry:?}");
    assert_eq!(live(&c, "s-stamp"), vec![], "a dry run writes nothing");

    let rep = run_in(&mut c, &state, false).unwrap();
    assert!(rep.changed, "{rep:?}");
    let text = rep.lines.join("\n");
    assert!(
        text.contains("s-gone.claim: its claimer is gone")
            && text.contains("s-start.claim: a start in flight")
            && text.contains("s-one.takeover.claim: a take-over claim")
            && text.contains("junk.claim: not a claim")
            && text.contains("nowhere: no such conversation"),
        "{text}"
    );
    assert_eq!(
        live(&c, "s-stamp"),
        vec![("terminal".into(), "running".into(), me, Some(stamp))]
    );
    let person: String = c
        .conn()
        .query_row(
            "SELECT person FROM claims WHERE conversation = 's-stamp'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(person, "sam");
    assert_eq!(
        live(&c, "s-old"),
        vec![("terminal".into(), "running".into(), me, Some(old))],
        "the old lstart form is kept as written"
    );
    assert_eq!(
        live(&c, "s-one"),
        vec![("external".into(), "running".into(), me, Some(String::new()))]
    );
    assert_eq!(live(&c, "s-gone"), vec![]);
    assert_eq!(live(&c, "s-start"), vec![]);
}

#[test]
fn a_second_run_changes_nothing_and_another_pid_is_never_replaced() {
    let (t, mut c) = core();
    let state = t.path().join("state");
    let me = i64::from(std::process::id());
    conversation(&c, "s1");
    conversation(&c, "s2");
    file(
        &state,
        "s1.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "terminal"}),
    );
    file(
        &state,
        "s2.claim",
        &json!({"pid": me, "procStart": null, "at": "t", "kind": "terminal"}),
    );
    // s2 already has a live claim row on another process.
    c.conn()
        .execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, pid, pid_started, created_at)
             VALUES (900, 's2', 1, 'terminal', 'running', 77, 'e1', 1)",
            [],
        )
        .unwrap();
    let first = run_in(&mut c, &state, false).unwrap();
    assert!(first.changed);
    assert!(
        first
            .lines
            .join("\n")
            .contains("s2: core.db already has live claim 900"),
        "{first:?}"
    );
    let rows = |c: &Core| -> i64 {
        c.conn()
            .query_row("SELECT COUNT(*) FROM claims", [], |r| r.get(0))
            .unwrap()
    };
    let logs = |c: &Core| -> i64 {
        c.conn()
            .query_row(
                "SELECT COUNT(*) FROM import_log WHERE step = 'claims'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!((rows(&c), logs(&c)), (2, 1));
    let second = run_in(&mut c, &state, false).unwrap();
    assert!(!second.changed, "{second:?}");
    assert_eq!(
        (rows(&c), logs(&c)),
        (2, 1),
        "nothing written the second time"
    );
    let dry = run_in(&mut c, &state, true).unwrap();
    assert!(!dry.changed, "{dry:?}");
}

#[test]
fn no_claim_folder_is_nothing_to_import() {
    let (t, _c) = core();
    let (wanted, skipped) = read_files(&t.path().join("none")).unwrap();
    assert!(wanted.is_empty() && skipped.is_empty());
}
