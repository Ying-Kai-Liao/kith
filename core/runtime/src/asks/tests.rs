//! The ask tree (M3-8a): write parity with the golden corpus (`kernel/tests/difftest/golden/delegation.json`), the
//! `delegations` view against the contract's fixture, and the rules the transaction adds (the cap check and the insert as
//! one step, crashed children counting, settle, end, the import).
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cast_precision_loss
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};

use super::call::call_at;
use super::{Open, open_in};
use crate::db::Core;
use crate::error::Error;
use crate::import::asks as import;
use crate::layout::Layout;
use crate::tests::open_all;

const NOW: i64 = 1_800_000_000_000;

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn json_file(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo(rel)).unwrap()).unwrap()
}

/// A scratch tree: `home/.kith` is the Mac's folder (`KITH_STATE`), `core` the core's own.
struct Tree {
    _t: tempfile::TempDir,
    root: PathBuf,
    layout: Layout,
}

impl Tree {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let layout = Layout::new(&root.join("core"));
        Self {
            _t: t,
            root,
            layout,
        }
    }
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    fn reg(&self, id: &str, v: &Value) {
        self.write(&format!("home/.kith/live/ask/{id}.reg"), &v.to_string());
    }
    /// What the Mac's folder is, as the core derives it from a caller's view.
    fn params(&self, env: &Value) -> Value {
        let mut env = env.clone();
        env["KITH_STATE"] = json!(self.home().join(".kith"));
        json!({"home": self.home(), "env": env})
    }
    fn import(&self) {
        import::run(&self.layout, &self.home().join(".kith"), false).unwrap();
    }
    fn core(&self) -> Core {
        open_all(&self.layout)
    }
}

fn with(params: &Value, extra: &Value) -> Value {
    let mut p = params.clone();
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value, Error> {
    call_at(core, method, params, NOW)
}

fn why(e: &Error) -> String {
    match e {
        Error::Refused(m) => m.clone(),
        other => panic!("not a refusal: {other}"),
    }
}

#[test]
fn the_migration_makes_the_table() {
    let t = Tree::new();
    let core = t.core();
    let n: i64 = core
        .conn()
        .query_row("SELECT COUNT(*) FROM asks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let bad = core.conn().execute(
        "INSERT INTO asks (id, chain, state) VALUES ('x', 'x', 'paused')",
        [],
    );
    assert!(bad.is_err(), "a state outside the four is refused");
}

// --- write parity ----------------------------------------------------------------------------------------------------

/// Cases of delegation.json that cannot be replayed as core calls, and why.
const UNMAPPED: &[(&str, &str)] = &[(
    "parked",
    "`ID.parked` is a mailbox marker (the wake lane's), not an ask record; the core keeps no such row here",
)];

#[test]
fn every_golden_delegation_case_answers_as_the_files_do() {
    let cases = json_file("kernel/tests/difftest/golden/delegation.json");
    let mut replayed = 0;
    let mut skipped: Vec<String> = Vec::new();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let facets = json_file(&format!("kernel/tests/difftest/golden/facets/{name}.json"));
        let t = Tree::new();
        for w in case["setup"].as_array().unwrap() {
            t.write(w["write"].as_str().unwrap(), w["text"].as_str().unwrap());
        }
        let mut env = serde_json::Map::new();
        for (k, v) in case["env"].as_object().unwrap() {
            let home = t.home().display().to_string();
            env.insert(
                k.clone(),
                json!(v.as_str().unwrap().replace("{HOME}", &home)),
            );
        }
        let env = Value::Object(env);
        // The import is what carries the setup's `.reg` files into the rows (damaged ones are left out, as the reader does).
        t.import();
        let mut core = t.core();
        let params = t.params(&env);
        let mut cap_cases = 0;
        for (i, step) in case["run"].as_array().unwrap().iter().enumerate() {
            let args: Vec<&str> = step
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .collect();
            let exit = facets[format!("step{i}.exit")].as_str().unwrap().trim();
            let expect_json = facets.get(format!("step{i}.json"));
            let expect_stdout = facets.get(format!("step{i}.stdout"));
            let expect_stderr = facets[format!("step{i}.stderr")].as_str().unwrap_or("");
            let ctx = format!("{name} step {i} {args:?}");
            match args[0] {
                "parked" => {
                    skipped.push(format!("{name}: {}", UNMAPPED[0].1));
                }
                "limits" => {
                    let mut got = call(&mut core, "ask.limits", &params).unwrap();
                    got.as_object_mut().unwrap().remove("cap_usd");
                    let want: Value =
                        serde_json::from_str(expect_json.unwrap().as_str().unwrap()).unwrap();
                    assert_eq!(got, want, "{ctx}");
                    replayed += 1;
                }
                "cap" => {
                    let got = call(&mut core, "ask.limits", &params).unwrap();
                    let text = got["cap_usd"]
                        .as_f64()
                        .map_or_else(|| "none".to_owned(), kith_kernel::delegation::pct_g);
                    let want = expect_stdout
                        .or(expect_json)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .trim();
                    assert_eq!(text, want, "{ctx}");
                    cap_cases += 1;
                    replayed += 1;
                }
                "record" => {
                    let got = call(&mut core, "ask.record", &json!({"id": args[1]}));
                    if exit == "1" {
                        assert!(got.is_err(), "{ctx}: {got:?}");
                    } else {
                        let want: Value =
                            serde_json::from_str(expect_json.unwrap().as_str().unwrap()).unwrap();
                        assert_eq!(got.unwrap(), want, "{ctx}");
                    }
                    replayed += 1;
                }
                "check" => {
                    // The Python opens the record and then removes it; so does this.
                    let mut id = "ask-check";
                    if let Some(p) = args.iter().position(|a| *a == "--id") {
                        id = args[p + 1];
                    }
                    let p = with(
                        &params,
                        &json!({"id": id, "asked": "probe", "crossing": args.contains(&"--crossing")}),
                    );
                    let got = call(&mut core, "ask.open", &p);
                    match got {
                        Ok(v) => {
                            assert_eq!(exit, "0", "{ctx}");
                            let want: Value =
                                serde_json::from_str(expect_json.unwrap().as_str().unwrap())
                                    .unwrap();
                            assert_eq!(v["decision"], want, "{ctx}");
                            core.conn()
                                .execute("DELETE FROM asks WHERE id = ?1", [id])
                                .unwrap();
                        }
                        Err(e) => {
                            assert_eq!(exit, "4", "{ctx}: {e}");
                            assert_eq!(format!("kith ask: {}\n", why(&e)), expect_stderr, "{ctx}");
                        }
                    }
                    replayed += 1;
                }
                other => panic!("{ctx}: a verb the replay does not know: {other}"),
            }
        }
        let _ = cap_cases;
    }
    let total: usize = cases
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["run"].as_array().unwrap().len())
        .sum();
    assert_eq!(
        replayed + skipped.len(),
        total,
        "every step is replayed or listed"
    );
    // Listed on purpose: the cases that cannot map.
    assert_eq!(skipped.len(), 3, "{skipped:?}");
}

// --- the view ----------------------------------------------------------------------------------------------------------

/// The `.reg` a `Delegation` of the contract's fixture would have come from.
fn reg_of(d: &Value) -> Value {
    let secs = |k: &str| d[k].as_i64().map(|ms| json!(ms as f64 / 1000.0));
    let mut r = json!({
        "asked": d["agent"], "agent": d["caller"]["agent"], "person": d["caller"]["person"], "via": d["caller"]["via"],
        "reply_to": d["reply_to"], "chain": d["chain"], "hop": d["hop"], "cross_hops": d["cross_hops"],
        "crossed": d["crossed"], "state": d["state"],
        "cost": d["cost_micro_usd"].as_i64().map(|m| m as f64 / 1e6),
        "at": secs("started_ms"),
    });
    for (k, f) in [
        ("answered_at", "answered_ms"),
        ("capped_at", "capped_ms"),
        ("cap_card_at", "cap_card_ms"),
    ] {
        if let Some(v) = secs(f) {
            r[k] = v;
        }
    }
    if !d["brief"].is_null() {
        r["brief"] = d["brief"].clone();
    }
    r
}

#[test]
fn the_delegations_view_is_the_contracts() {
    let fixture = json_file("serve/contract/kernel/fixtures/delegations.json");
    let t = Tree::new();
    let want = &fixture["body"];
    for d in want["delegations"].as_array().unwrap() {
        t.reg(d["id"].as_str().unwrap(), &reg_of(d));
    }
    t.import();
    let mut core = t.core();
    let got = call(&mut core, "ask.delegations", &t.params(&json!({}))).unwrap();
    assert_eq!(&got, want);
}

#[test]
fn the_view_is_newest_first_and_equal_starts_keep_id_order() {
    let t = Tree::new();
    for (id, at) in [("b", 100.0), ("a", 100.0), ("c", 200.0), ("d", 50.0)] {
        t.reg(id, &json!({"asked": "echo", "at": at}));
    }
    t.import();
    let mut core = t.core();
    let got = call(&mut core, "ask.delegations", &t.params(&json!({}))).unwrap();
    let ids: Vec<&str> = got["delegations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c", "a", "b", "d"]);
}

// --- the transaction -------------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), json!(v)))
            .collect(),
    )
}

#[test]
fn an_open_records_its_chain_hop_and_caller() {
    let t = Tree::new();
    let mut core = t.core();
    let root = call(
        &mut core,
        "ask.open",
        &with(
            &t.params(&env(&[
                ("KITH_AGENT", "architect"),
                ("CLAUDE_CODE_SESSION_ID", "s1"),
            ])),
            &json!({"id": "root", "asked": "echo", "brief": "x".repeat(3000), "pid": 4242}),
        ),
    )
    .unwrap();
    assert_eq!(root["record"]["chain"], "root");
    assert_eq!(root["record"]["hop"], 0);
    assert_eq!(root["record"]["via"], "terminal");
    assert_eq!(root["record"]["brief"].as_str().unwrap().len(), 2000);
    let child = call(
        &mut core,
        "ask.open",
        &with(
            &t.params(&env(&[("KITH_ASK_ID", "root")])),
            &json!({"id": "child", "asked": "relay"}),
        ),
    )
    .unwrap();
    assert_eq!(child["record"]["chain"], "root");
    assert_eq!(child["record"]["hop"], 1);
    assert_eq!(child["record"]["reply_to"]["ask"], "root");
    // the same id twice, and an id that is no id
    let again = call(
        &mut core,
        "ask.open",
        &with(
            &t.params(&env(&[])),
            &json!({"id": "root", "asked": "echo"}),
        ),
    );
    assert!(why(&again.unwrap_err()).contains("already has a record"));
    let bad = call(
        &mut core,
        "ask.open",
        &with(&t.params(&env(&[])), &json!({"id": "a/b", "asked": "echo"})),
    );
    assert!(matches!(bad, Err(Error::BadParams(_))));
}

#[test]
fn a_crashed_childs_cost_still_counts_towards_the_cap() {
    let t = Tree::new();
    // the child's process is long gone (pid 999999), its cost stays on the chain
    t.reg(
        "root",
        &json!({"asked": "a", "chain": "root", "cost": 0.5, "state": "open", "at": 1.0}),
    );
    t.reg(
        "dead",
        &json!({"asked": "b", "chain": "root", "hop": 1, "cost": 4.6, "state": "open", "pid": 999_999, "at": 2.0,
                "reply_to": {"session": null, "ask": "root"}}),
    );
    t.import();
    let mut core = t.core();
    let e = call(
        &mut core,
        "ask.open",
        &with(
            &t.params(&env(&[("KITH_ASK_ID", "root")])),
            &json!({"id": "next", "asked": "c"}),
        ),
    )
    .unwrap_err();
    assert!(
        why(&e).contains("chain root has spent US$5.10 of its US$5 cap"),
        "{}",
        why(&e)
    );
    let n: i64 = core
        .conn()
        .query_row("SELECT COUNT(*) FROM asks WHERE id = 'next'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 0, "a refused ask writes nothing");
}

#[test]
fn settle_tips_the_cap_once_and_a_settled_ask_only_adds_cost() {
    let t = Tree::new();
    t.reg(
        "a",
        &json!({"asked": "x", "chain": "a", "cost": 4.0, "state": "open", "at": 1.0}),
    );
    t.reg(
        "b",
        &json!({"asked": "x", "chain": "a", "hop": 1, "cost": 0.5, "state": "open", "at": 2.0}),
    );
    t.import();
    let mut core = t.core();
    let p = t.params(&env(&[]));
    let first = call(
        &mut core,
        "ask.settle",
        &with(&p, &json!({"id": "b", "cost": 0.75, "state": "answered"})),
    )
    .unwrap();
    assert_eq!(first["tipped"], true);
    assert_eq!(first["state"], "capped");
    let rec = call(&mut core, "ask.record", &json!({"id": "b"})).unwrap();
    assert_eq!(rec["cap_usd"], 5.0);
    assert!(rec["capped_at"].as_f64().unwrap() > 0.0);
    // settling it again adds cost, changes no state, and does not tip a second time
    let again = call(
        &mut core,
        "ask.settle",
        &with(&p, &json!({"id": "b", "cost": 0.25, "state": "stopped"})),
    )
    .unwrap();
    assert_eq!(again["tipped"], false);
    assert_eq!(again["state"], "capped");
    assert_eq!(again["cost"], 1.5);
    // another run of the chain finishing does not raise a second card for the same cap
    let other = call(
        &mut core,
        "ask.settle",
        &with(&p, &json!({"id": "a", "cost": 1.0, "state": "answered"})),
    )
    .unwrap();
    assert_eq!(other["tipped"], false);
    assert_eq!(other["state"], "answered");
    // the card was raised: mark it, once per record that carries the cap
    assert_eq!(
        call(&mut core, "ask.capTold", &json!({"chain": "a"})).unwrap()["marked"],
        1
    );
    let rec = call(&mut core, "ask.record", &json!({"id": "b"})).unwrap();
    assert!(rec["cap_card_at"].as_f64().unwrap() > 0.0);
    // no such ask
    assert!(
        call(
            &mut core,
            "ask.settle",
            &with(&p, &json!({"id": "nope", "cost": 1.0}))
        )
        .is_err()
    );
}

#[test]
fn end_settles_stopped_and_says_which_sessions_are_owed() {
    let t = Tree::new();
    let reply = |s: Value| json!({"session": s, "ask": null});
    t.reg(
        "mine",
        &json!({"asked": "x", "reply_to": reply(json!("s1")), "at": 1.0, "state": "open"}),
    );
    t.reg(
        "theirs",
        &json!({"asked": "x", "reply_to": reply(json!("s2")), "at": 2.0, "state": "open"}),
    );
    t.reg("kai", &json!({"asked": "x", "person": "kai", "reply_to": reply(json!("s3")), "at": 3.0, "state": "open"}));
    t.reg("done", &json!({"asked": "x", "reply_to": reply(json!("s1")), "at": 4.0, "state": "answered", "delivered": true}));
    t.reg("old", &json!({"asked": "x", "at": 5.0}));
    t.import();
    let mut core = t.core();
    core.conn()
        .execute(
            "UPDATE asks SET conversation = 'bg-1' WHERE id = 'mine'",
            [],
        )
        .unwrap();
    let out = call(
        &mut core,
        "ask.end",
        &json!({"sessions": ["s1"], "persons": ["kai"]}),
    )
    .unwrap();
    assert_eq!(out["ended"], json!(["kai", "mine"]));
    assert_eq!(out["sessions"], json!(["bg-1", "s1", "s3"]));
    let mut state = |id: &str| call(&mut core, "ask.record", &json!({"id": id})).unwrap();
    let mine = state("mine");
    assert_eq!(mine["state"], "stopped");
    assert_eq!(mine["done"], true);
    assert!(mine["ended_at"].as_f64().unwrap() > 0.0);
    assert_eq!(state("theirs")["state"], "open");
    assert_eq!(state("done")["state"], "answered", "history stays");
    assert_eq!(state("old")["state"], "open", "no session, no person named");
    // ending twice ends nothing more
    let again = call(
        &mut core,
        "ask.end",
        &json!({"sessions": ["s1"], "persons": ["kai"]}),
    )
    .unwrap();
    assert_eq!(again["ended"], json!([]));
}

// --- the race ----------------------------------------------------------------------------------------------------------

fn second_connection(l: &Layout) -> Connection {
    let c = Connection::open(&l.db).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(20)).unwrap();
    c
}

#[test]
fn two_opens_of_one_id_on_two_connections_let_exactly_one_through() {
    let t = Tree::new();
    let core = t.core();
    let machine = t.home().join(".kith");
    for round in 0..10 {
        let id = format!("dup-{round}");
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|n| {
                let (l, m, id, b) = (
                    t.layout.clone(),
                    machine.clone(),
                    id.clone(),
                    barrier.clone(),
                );
                std::thread::spawn(move || {
                    let mut c = second_connection(&l);
                    b.wait();
                    let tx = c
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .unwrap();
                    let o = Open {
                        id: &id,
                        asked: &format!("agent-{n}"),
                        crossing: false,
                        brief: None,
                        pid: None,
                        conversation: None,
                    };
                    let r = open_in(&tx, &m, &|_| None, &o, false, NOW);
                    if r.is_ok() {
                        tx.commit().unwrap();
                    }
                    r.is_ok()
                })
            })
            .collect();
        let passed = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(passed, 1, "round {round}");
    }
    drop(core);
}

/// `open` begins its transaction with `Core::claim_tx` (`BEGIN IMMEDIATE`): while another connection holds the write lock
/// the call waits, and it goes on when the lock is let go. (A deferred transaction would read at once.) An open adds no cost,
/// so two opens cannot take a chain over its cap between them; the same-id test above is what the transaction excludes.
#[test]
fn open_waits_for_the_write_lock_as_claim_tx_does() {
    let t = Tree::new();
    let mut core = t.core();
    let machine = t.home().join(".kith");
    let mut other = second_connection(&t.layout);
    let held = other
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let o = Open {
            id: "waits",
            asked: "x",
            crossing: false,
            brief: None,
            pid: None,
            conversation: None,
        };
        let r = super::open(&mut core, &machine, &|_| None, &o, false, NOW);
        done.send(()).unwrap();
        r.map(|_| ())
    });
    assert!(
        finished
            .recv_timeout(std::time::Duration::from_millis(400))
            .is_err(),
        "open went on while another connection held the write lock"
    );
    held.commit().unwrap();
    finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    worker.join().unwrap().unwrap();
}

// --- the import --------------------------------------------------------------------------------------------------------

fn import_log(core: &Core) -> i64 {
    core.conn()
        .query_row(
            "SELECT COUNT(*) FROM import_log WHERE step = 'asks'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn the_import_is_idempotent_read_only_and_lists_what_it_leaves_out() {
    let t = Tree::new();
    t.reg("a", &json!({"asked": "x", "chain": "a", "cost": 1.5, "state": "open", "at": 10.5, "odd": {"k": 1}}));
    t.reg("old", &json!({"session": "s-old", "asked": "echo"}));
    t.reg("paused", &json!({"asked": "x", "state": "paused"}));
    t.write("home/.kith/live/ask/junk.reg", "{not json");
    let before: Vec<(String, Vec<u8>)> = ["a", "old", "paused", "junk"]
        .iter()
        .map(|n| {
            let p = format!("home/.kith/live/ask/{n}.reg");
            (p.clone(), std::fs::read(t.root.join(p)).unwrap())
        })
        .collect();
    let machine = t.home().join(".kith");

    // a dry run opens nothing for writing: no core.db appears
    let dry = import::run(&t.layout, &machine, true).unwrap();
    assert!(dry.changed && dry.written == 0);
    assert!(!t.layout.db.exists());

    let first = import::run(&t.layout, &machine, false).unwrap();
    assert_eq!(first.written, 2);
    assert!(
        first
            .lines
            .iter()
            .any(|l| l.contains("paused") && l.contains("left out")),
        "{:?}",
        first.lines
    );
    {
        let core = t.core();
        assert_eq!(import_log(&core), 1);
        let a = super::record(&core, "a").unwrap().unwrap();
        assert_eq!(
            a["odd"],
            json!({"k": 1}),
            "keys the table has no column for are kept"
        );
        assert_eq!(a["at"], 10.5);
        let old = super::record(&core, "old").unwrap().unwrap();
        assert_eq!(old["reply_to"]["session"], "s-old");
        assert!(super::record(&core, "paused").unwrap().is_none());
    }
    // a dry run on a migrated database reports nothing to do, and a second import writes nothing
    let dry2 = import::run(&t.layout, &machine, true).unwrap();
    assert!(!dry2.changed);
    let second = import::run(&t.layout, &machine, false).unwrap();
    assert!(!second.changed && second.written == 0 && second.backup.is_none());
    assert_eq!(import_log(&t.core()), 1);
    // a record that changed since (kith-ask is still the writer) replaces its row
    t.reg(
        "a",
        &json!({"asked": "x", "chain": "a", "cost": 2.5, "state": "answered", "at": 10.5}),
    );
    let third = import::run(&t.layout, &machine, false).unwrap();
    assert_eq!(third.written, 1);
    assert_eq!(super::record(&t.core(), "a").unwrap().unwrap()["cost"], 2.5);
    // the sources were never touched
    for (p, bytes) in before.iter().filter(|(p, _)| !p.ends_with("/a.reg")) {
        assert_eq!(&std::fs::read(t.root.join(p)).unwrap(), bytes, "{p}");
    }
}
