//! Failover on a real core.db (M3-9b): an exit on a usage limit marks the account out and requeues the turn in the ack's
//! transaction, the next claim picks the backup and starts a session on it, and the one-writer rule holds throughout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::assert_is_empty,
    clippy::match_wildcard_for_single_variants
)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use kith_kernel::accounts::{Ctx, Outside};
use kith_kernel::threads;
use serde_json::json;
use tempfile::TempDir;

use super::failover::{NewSession, Next, OUT_OF_USAGE, Started, claim_next, next_account, tried};
use super::{CORE_WRITES, mark_out, read_all};
use crate::conversations::claims::{Kind, NewClaim, Reported, reconcile, redeem, report, stop};
use crate::db::{Core, MIGRATIONS};
use crate::error::Error;
use crate::layout::Layout;
use crate::start_with;
use crate::tests::machine;

/// Every harness is installed and signed in.
struct SignedIn(RefCell<()>);

impl Outside for SignedIn {
    fn status(&self, _argv: &[&str], _env: &[(&str, &str)]) -> (Option<i32>, String) {
        (Some(0), r#"{"loggedIn":true}"#.to_owned())
    }
    fn granted(&self, _agent: &str, _service: &str) -> Vec<String> {
        vec![]
    }
    fn on_path(&self, _program: &str) -> bool {
        true
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

/// 2030-03-17 in seconds and milliseconds; every clock below is this plus a little.
const T0: i64 = 1_900_000_000;
const MS: i64 = T0 * 1000;

struct World {
    _t: TempDir,
    m: PathBuf,
    core: Core,
}

fn put(m: &Path, rel: &str, text: &str) {
    let f = m.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, text).unwrap();
}

/// A core whose reconcile has run, with the agent `echo` on `accounts` (in that order) and a conversation `c1`.
fn world(accounts: &[&str]) -> World {
    let (t, m) = machine();
    let mut core = start_with(&Layout::new(&m), MIGRATIONS).expect("start");
    reconcile(&mut core, &[], &[], i64::MAX, 1).expect("reconcile");
    put(
        &m,
        "settings.json",
        &json!({ "accounts": accounts }).to_string(),
    );
    for a in accounts {
        if let Some((_, name)) = a.split_once(':') {
            std::fs::create_dir_all(m.join("accounts/claude").join(name)).unwrap();
        }
    }
    put(
        &m,
        "agents/echo/role/agent.md",
        "---\nname: echo\nharness: claude-code\n---\nbody\n",
    );
    core.conn()
        .execute(
            "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('c1', 'echo', 'claude', 1, 1)",
            [],
        )
        .unwrap();
    World { _t: t, m, core }
}

impl World {
    fn ctx<'a>(&self, out: &'a dyn Outside) -> Ctx<'a> {
        Ctx {
            machine: self.m.clone(),
            agents: self.m.join("agents"),
            kernel: self.m.join("kernel"),
            home: self.m.join("home"),
            claude_config_dir: None,
            out,
        }
    }
    fn tctx(&self) -> threads::Ctx<'static> {
        threads::Ctx {
            state: None,
            machine: Some(self.m.clone()),
            home: self.m.join("home"),
            env: &no_env,
        }
    }
    fn switch(&self) {
        self.core
            .conn()
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, '1')",
                [CORE_WRITES],
            )
            .unwrap();
    }
    /// A turn with a deadline, queued for sam.
    fn turn(&self) -> i64 {
        self.core
            .conn()
            .execute(
                "INSERT INTO turns (conversation, person, source, state, queued_at, deadline)
                 VALUES ('c1', 'sam', 'app', 'queued', 1, ?1)",
                [MS + 1_000_000_000],
            )
            .unwrap();
        self.core.conn().last_insert_rowid()
    }
    /// The next claim for `turn` through the failover pick.
    fn next(&mut self, turn: i64, sid: &str) -> Result<Started, Error> {
        self.next_as(turn, sid, "sam")
    }
    fn next_as(&mut self, turn: i64, sid: &str, person: &str) -> Result<Started, Error> {
        self.next_with(turn, sid, person, None)
    }
    /// The claim as the host asks for it: with the account its warm process runs as.
    fn next_with(
        &mut self,
        turn: i64,
        sid: &str,
        person: &str,
        account: Option<&str>,
    ) -> Result<Started, Error> {
        let out = SignedIn(RefCell::new(()));
        let ctx = self.ctx(&out);
        let tctx = self.tctx();
        let req = NewClaim {
            conversation: "c1",
            kind: Kind::Turn,
            person,
            turn: Some(turn),
            holder_instance: Some("h1"),
            account,
            process: None,
            revoke_seen: 0,
        };
        claim_next(
            &mut self.core,
            &ctx,
            &req,
            &NewSession {
                ctx: &tctx,
                cwd: "/work/p",
                session_id: sid,
            },
            MS + 10,
        )
    }
    fn claimed(&mut self, turn: i64, sid: &str, pid: u32) -> (i64, String) {
        let Started::Claimed {
            claimed, account, ..
        } = self.next(turn, sid).expect("claim")
        else {
            panic!("exhausted")
        };
        redeem(
            &mut self.core,
            claimed.claim_id,
            pid,
            &format!("e{pid}"),
            "h1",
            MS + 20,
        )
        .expect("redeem");
        (claimed.claim_id, account)
    }
    fn out_report(&mut self, claim: i64, pid: u32, resets: Option<i64>) -> Reported {
        let mut body = json!({"pid": pid, "out_of_usage": true});
        if let Some(r) = resets {
            body["resets_at"] = json!(r);
        }
        report(&mut self.core, claim, "exit", &body.to_string(), MS + 30).expect("report")
    }
    fn one(&self, sql: &str) -> String {
        self.core
            .conn()
            .query_row(sql, [], |r| r.get::<_, rusqlite::types::Value>(0))
            .map(|v| match v {
                rusqlite::types::Value::Text(s) => s,
                rusqlite::types::Value::Integer(i) => i.to_string(),
                rusqlite::types::Value::Null => "NULL".into(),
                other => format!("{other:?}"),
            })
            .unwrap()
    }
}

#[test]
fn an_out_report_marks_the_account_out_requeues_and_the_next_claim_runs_the_backup() {
    let mut w = world(&["claude", "claude:a", "claude:b"]);
    w.switch();
    let turn = w.turn();
    let (first, account) = w.claimed(turn, "s-main", 100);
    assert_eq!(account, "claude");
    assert_eq!(
        w.one(&format!(
            "SELECT account FROM claims WHERE claim_id = {first}"
        )),
        "claude"
    );

    let r = w.out_report(first, 100, Some(T0 + 7200));
    assert_eq!(
        r,
        Reported::Stored {
            applied: Some("exit")
        }
    );
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {first}"
        )),
        "ended"
    );
    assert_eq!(
        w.one(&format!(
            "SELECT end_reason FROM claims WHERE claim_id = {first}"
        )),
        OUT_OF_USAGE
    );
    // Requeued, keeping its deadline.
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
    assert_eq!(
        w.one(&format!("SELECT deadline FROM turns WHERE id = {turn}")),
        (MS + 1_000_000_000).to_string()
    );
    assert_eq!(
        w.one(&format!("SELECT requeues FROM turns WHERE id = {turn}")),
        "1"
    );
    // The account is out until the provider's time.
    let state = read_all(w.core.conn()).unwrap();
    assert_eq!(state["claude"]["out_until"], json!(T0 + 7200));
    assert_eq!(state["claude"]["reason"], json!("rate_limit"));
    assert_eq!(
        tried(w.core.conn(), turn).unwrap(),
        vec!["claude".to_owned()]
    );

    // The next claim is a new claim on the backup, with its own new session.
    let (second, account) = w.claimed(turn, "s-backup", 101);
    assert_ne!(first, second);
    assert_eq!(account, "claude:a");
    assert_eq!(
        w.one(&format!(
            "SELECT account FROM claims WHERE claim_id = {second}"
        )),
        "claude:a"
    );
    let sessions = crate::transcripts::rows::list(w.core.conn(), "c1").unwrap();
    assert_eq!(sessions.len(), 2);
    let a = sessions.iter().find(|s| s.account == "claude:a").unwrap();
    assert_eq!(a.session_id, "s-backup");
    assert_eq!(a.engine, "claude");
    assert_eq!(a.cwd, "/work/p");
    assert!(
        a.pointer.ends_with("/projects/-work-p/s-backup.jsonl"),
        "{}",
        a.pointer
    );
    assert!(a.hash.is_none(), "nothing was copied from the old session");
    let main = sessions.iter().find(|s| s.account == "claude").unwrap();
    assert_eq!(main.session_id, "s-main");
}

#[test]
fn the_account_row_is_not_written_while_the_core_is_not_the_writer_but_the_turn_still_moves() {
    let mut w = world(&["claude", "claude:a"]);
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    assert!(
        read_all(w.core.conn()).unwrap().is_empty(),
        "Python stays the writer"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
    // The state still says claude is fine, but this turn already ran out there: once per entry.
    let (_, account) = w.claimed(turn, "s-backup", 101);
    assert_eq!(account, "claude:a");
}

#[test]
fn no_backup_left_ends_the_turn_with_the_out_of_usage_notice() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    let (second, account) = w.claimed(turn, "s-backup", 101);
    assert_eq!(account, "claude:a");
    w.out_report(second, 101, Some(T0 + 600));

    let claims = w.one("SELECT count(*) FROM claims");
    match w.next(turn, "s-none").expect("next") {
        Started::Exhausted(text) => {
            assert!(
                text.starts_with("Every account for this agent is out of usage: "),
                "{text}"
            );
            assert!(text.contains("claude (back in 2h)"), "{text}");
            assert!(text.contains("claude:a (back in 10m)"), "{text}");
            assert!(text.ends_with("The first is back in 10m."), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "failed"
    );
    assert_eq!(
        w.one("SELECT count(*) FROM claims"),
        claims,
        "no new claim, no endless requeue"
    );
    // Asking again changes nothing more.
    assert!(matches!(
        w.next(turn, "s-none").unwrap(),
        Started::Exhausted(_)
    ));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "failed"
    );
}

#[test]
fn an_entry_is_tried_once_per_turn_even_when_its_mark_is_gone() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    // The main account comes back (its mark cleared) before the backup is claimed: this turn already ran out there, so
    // the top of the chain is not tried a second time.
    w.core
        .conn()
        .execute("DELETE FROM account_state", [])
        .unwrap();
    let (_, account) = w.claimed(turn, "s-backup", 101);
    assert_eq!(account, "claude:a");
}

#[test]
fn a_duplicate_out_report_does_not_fail_over_twice() {
    let mut w = world(&["claude", "claude:a", "claude:b"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    assert_eq!(
        w.out_report(first, 100, Some(T0 + 7200)),
        Reported::Stored {
            applied: Some("exit")
        }
    );
    let (second, account) = w.claimed(turn, "s-backup", 101);
    assert_eq!(account, "claude:a");
    // The retry of the same (claim_id, kind, pid) report, now that the turn runs on the backup.
    assert_eq!(
        w.out_report(first, 100, Some(T0 + 7200)),
        Reported::Stored { applied: None }
    );
    assert_eq!(
        w.one(&format!("SELECT requeues FROM turns WHERE id = {turn}")),
        "1"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "running"
    );
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {second}"
        )),
        "running"
    );
    assert_eq!(
        tried(w.core.conn(), turn).unwrap(),
        vec!["claude".to_owned()]
    );
}

#[test]
fn a_report_for_an_ended_claim_or_another_process_changes_nothing() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    // Another process of the claim (a respawn that lost the lock) says it ran out: not the writer's word.
    assert_eq!(
        w.out_report(first, 999, Some(T0 + 7200)),
        Reported::Stored { applied: None }
    );
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {first}"
        )),
        "running"
    );
    assert!(read_all(w.core.conn()).unwrap().is_empty());
    // Ended: a repeat of the writer's report (kept once per claim, kind and pid), and one for an unknown claim, change
    // nothing; a first out report after the claim ended is `an_out_exit_marks_the_account_whatever_state_the_claim_is_in`.
    w.out_report(first, 100, Some(T0 + 7200));
    let after = read_all(w.core.conn()).unwrap();
    assert_eq!(
        w.out_report(first, 100, Some(T0 + 99_999)),
        Reported::Stored { applied: None }
    );
    assert_eq!(read_all(w.core.conn()).unwrap(), after);
    assert_eq!(w.out_report(4242, 1, None), Reported::Unknown);
}

#[test]
fn a_running_claim_never_moves_and_a_main_account_coming_back_cannot_take_the_conversation() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    let (second, account) = w.claimed(turn, "s-backup", 101);
    assert_eq!(account, "claude:a");
    // The main account is back while the backup runs.
    w.core
        .conn()
        .execute("DELETE FROM account_state", [])
        .unwrap();
    let again = w.next(turn, "s-main-2");
    assert!(
        matches!(&again, Err(Error::Refused(m)) if m.contains("has a writer")),
        "{again:?}"
    );
    // The index itself refuses a second live claim in the conversation, whatever its account.
    let direct = w.core.conn().execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, account, created_at)
         VALUES (900, 'c1', 1, 'turn', 'claimed', 'claude', 1)",
        [],
    );
    assert!(direct.is_err());
    assert_eq!(
        w.one(&format!(
            "SELECT account FROM claims WHERE claim_id = {second}"
        )),
        "claude:a"
    );
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {second}"
        )),
        "running"
    );
}

#[test]
fn a_usage_exit_racing_a_stop_does_not_bring_the_turn_back() {
    // The stop cancelled the turn: the exit that follows ends the claim and records the limit, the turn stays cancelled.
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    stop(&mut w.core, first, "stop", false, MS + 25).expect("stop");
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {first}"
        )),
        "ending"
    );
    w.out_report(first, 100, Some(T0 + 7200));
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {first}"
        )),
        "ended"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "cancelled"
    );
    assert_eq!(
        w.one(&format!("SELECT requeues FROM turns WHERE id = {turn}")),
        "0"
    );
    // The account did run out, so it is out all the same.
    assert_eq!(
        read_all(w.core.conn()).unwrap()["claude"]["out_until"],
        json!(T0 + 7200)
    );

    // A stop that requeues: the turn is requeued once, not twice, and still skips the account it ran out on.
    let turn2 = w.turn();
    let (second, _) = w.claimed(turn2, "s-main-2", 200);
    stop(&mut w.core, second, "restart", true, MS + 26).expect("stop");
    w.out_report(second, 200, None);
    assert_eq!(
        w.one(&format!("SELECT requeues FROM turns WHERE id = {turn2}")),
        "1"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn2}")),
        "queued"
    );
    // claude was already out from the first turn, so the second ran on the backup.
    assert_eq!(
        tried(w.core.conn(), turn2).unwrap(),
        vec!["claude:a".to_owned()]
    );
}

#[test]
fn a_nameless_usage_exit_marks_out_and_requeues_while_the_claim_waits_for_its_process() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    let body = json!({"out_of_usage": true}).to_string();
    report(&mut w.core, first, "exit", &body, MS + 30).unwrap();
    assert_eq!(
        w.one(&format!(
            "SELECT state FROM claims WHERE claim_id = {first}"
        )),
        "ending"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
    // Still the writer: the backup cannot be claimed until the process is gone.
    assert!(matches!(w.next(turn, "s-backup"), Err(Error::Refused(_))));
}

#[test]
fn an_exit_without_the_flag_or_a_done_does_not_fail_over() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    report(&mut w.core, first, "exit", r#"{"pid": 100}"#, MS + 30).unwrap();
    assert_eq!(
        w.one(&format!(
            "SELECT end_reason FROM claims WHERE claim_id = {first}"
        )),
        "exit"
    );
    assert!(read_all(w.core.conn()).unwrap().is_empty());
    assert!(tried(w.core.conn(), turn).unwrap().is_empty());
}

#[test]
fn a_turn_that_never_failed_over_runs_the_first_even_when_everything_is_out() {
    let w = world(&["claude"]);
    w.switch();
    mark_out(
        w.core.conn(),
        "claude",
        Some(T0 + 600),
        "rate_limit",
        T0,
        true,
    )
    .unwrap();
    let turn = w.turn();
    let out = SignedIn(RefCell::new(()));
    let ctx = w.ctx(&out);
    assert_eq!(
        next_account(&w.core, &ctx, "echo", Some(turn), MS).unwrap(),
        Next::Account("claude".into())
    );
}

#[test]
fn mark_out_is_the_pythons_mark_out() {
    let (_t, m) = machine();
    let c = start_with(&Layout::new(&m), MIGRATIONS).unwrap();
    let conn = c.conn();
    // Default: an hour from now.
    mark_out(conn, "claude", None, "rate_limit", T0, true).unwrap();
    let s = read_all(conn).unwrap();
    assert_eq!(s["claude"]["out_until"], json!(T0 + 3600));
    assert_eq!(s["claude"]["since"], json!(T0));
    // Already out for longer stays so.
    mark_out(conn, "claude", Some(T0 + 60), "rate_limit", T0 + 10, true).unwrap();
    assert_eq!(
        read_all(conn).unwrap()["claude"]["out_until"],
        json!(T0 + 3600)
    );
    // Exact sets it, and `since` stays while it was out.
    mark_out(conn, "claude", Some(T0 + 60), "usage", T0 + 10, false).unwrap();
    let s = read_all(conn).unwrap();
    assert_eq!(s["claude"]["out_until"], json!(T0 + 60));
    assert_eq!(s["claude"]["since"], json!(T0));
    assert_eq!(s["claude"]["reason"], json!("usage"));
    // Out in the past: a new since.
    mark_out(
        conn,
        "claude",
        Some(T0 + 9000),
        "rate_limit",
        T0 + 5000,
        true,
    )
    .unwrap();
    assert_eq!(read_all(conn).unwrap()["claude"]["since"], json!(T0 + 5000));
    // Other keys of the entry stay.
    conn.execute("UPDATE account_state SET extra_json = '{\"note\": 1}'", [])
        .unwrap();
    mark_out(
        conn,
        "claude",
        Some(T0 + 99_999),
        "rate_limit",
        T0 + 5000,
        true,
    )
    .unwrap();
    assert_eq!(read_all(conn).unwrap()["claude"]["note"], json!(1));
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "test clocks are whole seconds"
)]
fn out_until(w: &World, id: &str) -> Option<i64> {
    read_all(w.core.conn())
        .unwrap()
        .get(id)
        .and_then(|e| e["out_until"].as_f64())
        .map(|f| f as i64)
}

#[test]
fn an_out_exit_marks_the_account_whatever_state_the_claim_is_in() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    // The turn finished its reply (`done` ended the claim), then the same process hit the limit.
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    report(&mut w.core, first, "done", r#"{"pid":100}"#, MS + 25).unwrap();
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "done"
    );
    assert_eq!(out_until(&w, "claude"), None);
    let r = w.out_report(first, 100, Some(T0 + 7200));
    assert_eq!(r, Reported::Stored { applied: None });
    assert_eq!(out_until(&w, "claude"), Some(T0 + 7200));
    assert_eq!(
        w.one(&format!(
            "SELECT end_reason FROM claims WHERE claim_id = {first}"
        )),
        "done"
    );
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "done"
    );
    // Another process's word still marks nothing.
    let turn2 = w.turn();
    let (second, account) = w.claimed(turn2, "s-b", 110);
    assert_eq!(account, "claude:a");
    w.out_report(second, 999, Some(T0 + 7200));
    assert_eq!(out_until(&w, "claude:a"), None);
}

#[test]
fn an_out_exit_after_end_gone_marks_the_account() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.core
        .conn()
        .execute(
            "UPDATE claims SET state = 'ended', ended_at = 1, end_reason = 'gone' WHERE claim_id = ?1",
            [first],
        )
        .unwrap();
    w.out_report(first, 100, Some(T0 + 600));
    assert_eq!(out_until(&w, "claude"), Some(T0 + 600));
}

#[test]
fn a_revoked_claim_keeps_its_end_reason_when_the_account_ran_out() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    crate::conversations::claims::revoke_person(&mut w.core, "sam", MS + 25).unwrap();
    w.out_report(first, 100, Some(T0 + 600));
    assert_eq!(
        w.one(&format!(
            "SELECT end_reason FROM claims WHERE claim_id = {first}"
        )),
        "revoked"
    );
    assert_eq!(out_until(&w, "claude"), Some(T0 + 600));
}

#[test]
fn a_stops_end_reason_is_replaced_by_out_of_usage() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    stop(&mut w.core, first, "stopped", true, MS + 25).unwrap();
    w.out_report(first, 100, Some(T0 + 600));
    assert_eq!(
        w.one(&format!(
            "SELECT end_reason FROM claims WHERE claim_id = {first}"
        )),
        OUT_OF_USAGE
    );
}

#[test]
fn resets_at_is_capped_and_a_float_is_truncated_like_pythons_int() {
    let mut w = world(&["claude", "claude:a", "claude:b"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    let mut body = json!({"pid": 100, "out_of_usage": true, "resets_at": 1_900_007_200.9});
    report(&mut w.core, first, "exit", &body.to_string(), MS + 30).unwrap();
    assert_eq!(out_until(&w, "claude"), Some(T0 + 7200));
    // Milliseconds for seconds: far beyond 8 days, so the default hour applies.
    let (second, account) = w.claimed(turn, "s-a", 101);
    assert_eq!(account, "claude:a");
    body = json!({"pid": 101, "out_of_usage": true, "resets_at": T0 * 1000});
    report(&mut w.core, second, "exit", &body.to_string(), MS + 30).unwrap();
    assert_eq!(out_until(&w, "claude:a"), Some(T0 + 3600));
}

#[test]
fn the_exhausted_notice_names_accounts_tried_without_a_mark() {
    // Not switched: no account row is written, so the notice has only the turn's own record to name them.
    let mut w = world(&["claude", "claude:a"]);
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, None);
    let (second, _) = w.claimed(turn, "s-b", 101);
    w.out_report(second, 101, None);
    match w.next(turn, "s-none").expect("next") {
        Started::Exhausted(text) => {
            assert!(text.contains("claude (ran out on this turn)"), "{text}");
            assert!(text.contains("claude:a (ran out on this turn)"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn exhausting_fails_only_the_callers_own_turn() {
    let mut w = world(&["claude"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 600));
    // Another person naming this turn does not fail it.
    assert!(matches!(
        w.next_as(turn, "s-x", "eve"),
        Ok(Started::Exhausted(_))
    ));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
    assert!(matches!(w.next(turn, "s-y"), Ok(Started::Exhausted(_))));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "failed"
    );
}

#[test]
fn a_failing_session_write_rolls_the_claim_back() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    w.core
        .conn()
        .execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON engine_sessions BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
    assert!(w.next(turn, "s-main").is_err());
    assert_eq!(w.one("SELECT count(*) FROM claims"), "0");
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
    // The conversation is not blocked: the same call goes through once the write can.
    w.core.conn().execute("DROP TRIGGER boom", []).unwrap();
    assert!(matches!(
        w.next(turn, "s-main"),
        Ok(Started::Claimed { fresh: true, .. })
    ));
}

#[test]
fn an_account_with_a_session_resumes_it_and_is_not_fresh() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    let (second, _) = w.claimed(turn, "s-backup", 101);
    w.out_report(second, 101, Some(T0 + 600));
    // The main account is back: its row from the first claim is the one the engine resumes.
    w.core
        .conn()
        .execute("DELETE FROM account_state", [])
        .unwrap();
    let turn2 = w.turn();
    let Started::Claimed {
        account,
        fresh,
        session,
        ..
    } = w.next(turn2, "s-new").unwrap()
    else {
        panic!("exhausted")
    };
    assert_eq!(account, "claude");
    assert!(!fresh);
    assert_eq!(session.session_id, "s-main");
}

impl World {
    /// A stopped ask on `c1` (its wake and ask turns are fenced for good).
    fn stopped_ask(&self) {
        self.core
            .conn()
            .execute(
                "INSERT INTO asks (id, chain, parent, hop, caller_person, conversation, state, at_ms, cost)
                 VALUES ('a1', 'r', NULL, 0, 'sam', 'c1', 'stopped', 1, 0.0)",
                [],
            )
            .unwrap();
    }
}

#[test]
fn claim_next_cancels_a_turn_of_a_stopped_ask() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    w.core
        .conn()
        .execute("UPDATE turns SET source = 'wake' WHERE id = ?1", [turn])
        .unwrap();
    w.stopped_ask();
    assert!(matches!(w.next(turn, "s-x"), Err(Error::Stopped(_))));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "cancelled"
    );
    assert_eq!(w.one("SELECT count(*) FROM claims"), "0");
}

#[test]
fn a_stopped_asks_wake_that_finds_no_account_is_cancelled_not_failed() {
    let mut w = world(&["claude"]);
    w.switch();
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 600));
    w.core
        .conn()
        .execute("UPDATE turns SET source = 'wake' WHERE id = ?1", [turn])
        .unwrap();
    w.stopped_ask();
    assert!(matches!(w.next(turn, "s-y"), Err(Error::Stopped(_))));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "cancelled"
    );
}

#[test]
fn a_stale_revoke_epoch_leaves_the_turn_queued_in_claim_next() {
    let mut w = world(&["claude"]);
    w.switch();
    // the person was revoked and restored; the turn was queued after that, and the claimer's epoch is the old one
    crate::conversations::claims::revoke_person(&mut w.core, "sam", MS).unwrap();
    let turn = w.turn();
    assert!(matches!(w.next(turn, "s-x"), Err(Error::Refused(_))));
    assert_eq!(
        w.one(&format!("SELECT state FROM turns WHERE id = {turn}")),
        "queued"
    );
}

#[test]
fn unswitched_the_claim_uses_the_account_the_host_sends_and_records_it() {
    let mut w = world(&["claude", "claude:a"]);
    let turn = w.turn();
    // The pick would say `claude`; the host's process runs as claude:a, so that is the account, with its own session.
    let Started::Claimed {
        claimed,
        account,
        fresh,
        session,
    } = w
        .next_with(turn, "s-a", "sam", Some("claude:a"))
        .expect("claim")
    else {
        panic!("exhausted")
    };
    assert_eq!(account, "claude:a");
    assert!(fresh);
    assert_eq!(session.account, "claude:a");
    assert_eq!(
        w.one(&format!(
            "SELECT account FROM claims WHERE claim_id = {}",
            claimed.claim_id
        )),
        "claude:a"
    );
    let sessions = crate::transcripts::rows::list(w.core.conn(), "c1").unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "no session row or pointer for another account"
    );
    assert_eq!(sessions[0].account, "claude:a");
    assert!(read_all(w.core.conn()).unwrap().is_empty());
}

#[test]
fn unswitched_an_account_this_turn_ran_out_on_is_still_the_hosts_to_choose() {
    // Nobody writes the marks while unswitched, and a turn that ran out is failed over by the host (it restarts on another
    // account and asks again), so the account it sends is the one that runs.
    let mut w = world(&["claude", "claude:a"]);
    let turn = w.turn();
    let (first, _) = w.claimed(turn, "s-main", 100);
    w.out_report(first, 100, Some(T0 + 7200));
    let Started::Claimed { account, .. } = w
        .next_with(turn, "s-main", "sam", Some("claude"))
        .expect("claim")
    else {
        panic!("exhausted")
    };
    assert_eq!(
        account, "claude",
        "the host's account stands, it was not picked"
    );
}

#[test]
fn switched_the_pick_stands_whatever_account_the_host_sends() {
    let mut w = world(&["claude", "claude:a"]);
    w.switch();
    let turn = w.turn();
    let Started::Claimed { account, .. } = w
        .next_with(turn, "s-main", "sam", Some("claude:a"))
        .expect("claim")
    else {
        panic!("exhausted")
    };
    assert_eq!(account, "claude");
}
