//! The writers of account state in the core (accounts-s1): `account.out|clear|usage|started` end in the state the file's
//! writers leave (the same `apply_change`), only a switched core takes them, and a bad call changes nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_kernel::accounts::{Change, apply_change};
use serde_json::{Value, json};

use super::read_all;

use crate::db::{Core, MIGRATIONS};
use crate::error::Error;
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

const NOW: i64 = 1_900_000_000;

fn core() -> (tempfile::TempDir, Core) {
    let (t, m) = machine();
    let core = start_with(&Layout::new(&m), MIGRATIONS).expect("start");
    (t, core)
}

fn allow(core: &Core) {
    // A real switch is the only thing that opens these writers (the failover flag does not).
    super::tests_switched::set(true);
    let _ = core;
}

fn call(core: &Core, method: &str, params: &Value) -> Result<Value, Error> {
    core.account_call(method, params)
}

fn state(core: &Core) -> Value {
    Value::Object(read_all(core.conn()).unwrap())
}

#[test]
fn an_unswitched_core_takes_no_write() {
    let (_t, core) = core();
    for (m, p) in [
        (
            "account.out",
            json!({"id": "claude:work", "until": NOW + 10, "now": NOW}),
        ),
        ("account.clear", json!({"id": "claude:work", "now": NOW})),
        ("account.started", json!({"id": "claude:work", "now": NOW})),
        (
            "account.usage",
            json!({"id": "claude:work", "reading": {"windows": [{"name": "w", "percent": 1}]}, "now": NOW}),
        ),
    ] {
        let e = call(&core, m, &p).unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{m}: {e}");
        assert!(e.to_string().contains("still in state.json"), "{e}");
    }
    assert_eq!(state(&core), json!({}));
}

#[test]
fn the_writers_end_where_the_files_writers_do() {
    let (_t, core) = core();
    allow(&core);
    // The same sequence over the file's shape (the rules the Python and the Rust file writers apply) and over the table.
    let mut file = serde_json::Map::new();
    let steps = [
        (
            "account.out",
            json!({"id": "claude:work", "until": NOW + 100, "reason": "maintenance", "now": NOW}),
            Change::Out {
                id: "claude:work".into(),
                until: Some(NOW + 100),
                reason: "maintenance".into(),
                extend_only: false,
            },
        ),
        (
            "account.out",
            json!({"id": "claude:work", "until": NOW + 50, "extend": true, "now": NOW + 1}),
            Change::Out {
                id: "claude:work".into(),
                until: Some(NOW + 50),
                reason: "manual".into(),
                extend_only: true,
            },
        ),
        (
            "account.started",
            json!({"id": "codex:b", "now": NOW + 2}),
            Change::Started {
                id: "codex:b".into(),
            },
        ),
        (
            "account.usage",
            json!({"id": "codex:b", "reading": {"windows": [{"name": "five_hour", "percent": 71.5, "resets_at": NOW + 9}], "read_at": NOW}, "now": NOW + 3}),
            Change::Usage {
                id: "codex:b".into(),
                usage: json!({"read_at": NOW, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": NOW + 9}]}),
            },
        ),
        (
            "account.clear",
            json!({"id": "claude:work", "hostUntil": NOW + 7, "now": NOW + 4}),
            Change::Clear {
                id: "claude:work".into(),
                host_until: NOW + 7,
            },
        ),
    ];
    for (method, params, change) in steps {
        let now = params["now"].as_i64().unwrap();
        call(&core, method, &params).unwrap();
        apply_change(&mut file, &change, now);
        assert_eq!(state(&core), Value::Object(file.clone()), "after {method}");
    }
    assert_eq!(
        state(&core),
        json!({
            "claude:work": {"cleared_at": NOW + 4, "cleared_host_until": NOW + 7},
            "codex:b": {"last_started": NOW + 2, "usage": {"read_at": NOW, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": NOW + 9}]}},
        })
    );
}

#[test]
fn an_entry_the_file_would_make_for_an_unchanged_mark_is_made_here_too() {
    let (_t, core) = core();
    allow(&core);
    call(
        &core,
        "account.out",
        &json!({"id": "claude:work", "until": NOW + 500, "now": NOW}),
    )
    .unwrap();
    let before = state(&core);
    // Out for less, `extend`: the row stays as it was.
    call(
        &core,
        "account.out",
        &json!({"id": "claude:work", "until": NOW + 100, "extend": true, "now": NOW + 1}),
    )
    .unwrap();
    assert_eq!(state(&core), before);
}

#[test]
fn a_bad_call_is_refused_and_changes_nothing() {
    let (_t, core) = core();
    allow(&core);
    for (m, p, why) in [
        (
            "account.out",
            json!({"id": "bad!id", "now": NOW}),
            "not an account id",
        ),
        ("account.out", json!({"now": NOW}), "id is required"),
        (
            "account.out",
            json!({"id": "claude:work", "reason": "Bad Reason", "now": NOW}),
            "reason is a short word",
        ),
        (
            "account.out",
            json!({"id": "claude:work", "until": NOW - 1, "now": NOW}),
            "in the past",
        ),
        (
            "account.usage",
            json!({"id": "claude:work", "now": NOW}),
            "reading is required",
        ),
        (
            "account.usage",
            json!({"id": "claude:work", "reading": {"windows": []}, "now": NOW}),
            "1 to 8",
        ),
    ] {
        let e = call(&core, m, &p).unwrap_err();
        assert!(matches!(e, Error::BadParams(_)), "{m} {p}: {e}");
        assert!(e.to_string().contains(why), "{e}");
    }
    assert_eq!(state(&core), json!({}));
}

#[test]
fn a_write_is_one_transaction() {
    // Two writers on two ids: each leaves the other's row alone, and the second sees the first.
    let (_t, core) = core();
    allow(&core);
    call(
        &core,
        "account.started",
        &json!({"id": "claude", "now": NOW}),
    )
    .unwrap();
    call(
        &core,
        "account.out",
        &json!({"id": "codex", "until": NOW + 9, "now": NOW}),
    )
    .unwrap();
    let s = state(&core);
    assert_eq!(s["claude"], json!({"last_started": NOW}));
    assert_eq!(s["codex"]["out_until"], NOW + 9);
}
