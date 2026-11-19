#![allow(
    clippy::unwrap_used,
    clippy::result_large_err,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    clippy::type_complexity
)]
//! The takers in Writer mode (the claims are the core's) and the registry's spool, against a fake core: the cases of
//! `kernel/tests/kith-sessions-core-check` (F1, W1-W7, W12, W13, R1-R6) that a Rust verb must answer as the Python does. The
//! fake knows nothing but the rows' answers; the real daemon is `serve/host/tests/threads_writer_e2e.rs`.

use std::cell::RefCell;
use std::fs;
use std::time::Duration;

use serde_json::{Value, json};

use super::core::{Core, CoreErr, claims_switched, registry_switched};
use super::resolve::{NoHost, resolve_taking};
use super::tests::{Sleeper, Tmp, args, ctx, me, no_env, run_env, transcript};
use super::writer::{ProcErr, Taker};
use super::*;

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, CoreErr>>;

/// A core that answers every call with `answer` and remembers what it was asked.
struct Fake {
    answer: Answer,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Fake {
    fn new(answer: impl Fn(&str, &Value) -> Result<Value, CoreErr> + 'static) -> Self {
        Self {
            answer: Box::new(answer),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn ok() -> Self {
        Self::new(|m, _| match m {
            "claim.external" => Ok(json!({"claimId": 1})),
            "claim.epoch" => Ok(json!({"epoch": 7})),
            "claim.release" => Ok(json!({"ended": true})),
            "claim.sweepPerson" => Ok(json!({"kill": []})),
            _ => Ok(json!({})),
        })
    }

    fn down() -> Self {
        Self::new(|_, _| Err(CoreErr::NoAnswer("no core daemon answers".to_owned())))
    }

    fn asked(&self) -> Vec<(String, Value)> {
        self.calls.borrow().clone()
    }
}

impl Core for Fake {
    fn call(&self, method: &str, params: &Value, _: Option<Duration>) -> Result<Value, CoreErr> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        (self.answer)(method, params)
    }
}

/// The claims are the core's: the marker, as `kith migrate step claims` leaves it.
fn switch(t: &Tmp) {
    t.put("claims.tombstone", "");
}

fn claim_files(t: &Tmp) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(t.reg())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| Path::new(n).extension().is_some_and(|x| x == "claim"))
        .collect();
    v.sort();
    v
}

#[test]
fn writer_mode_is_either_source_and_fails_closed() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    assert!(!claims_switched(&c) && !registry_switched(&c));
    let cutover = t.0.join("machine").join("core").join("cutover");
    fs::create_dir_all(&cutover).unwrap();
    // The module's own record counts once it says more than `stepping`.
    fs::write(cutover.join("claims.switch"), "state=stepping\n").unwrap();
    assert!(!claims_switched(&c));
    fs::write(cutover.join("claims.switch"), "state=switched\nsince=1\n").unwrap();
    assert!(claims_switched(&c));
    assert!(
        !registry_switched(&c),
        "the registry is the transcripts' own switch"
    );
    fs::write(cutover.join("transcripts.switch"), "state=rolling-back\n").unwrap();
    assert!(registry_switched(&c));
    // A marker alone counts, a dangling link too.
    let t2 = Tmp::new();
    let c2 = ctx(&t2, &no_env);
    fs::create_dir_all(t2.reg()).unwrap();
    std::os::unix::fs::symlink("/nonexistent", t2.reg().join("claims.tombstone")).unwrap();
    assert!(claims_switched(&c2));
    // No state folder, no mode.
    let none = Ctx {
        state: None,
        machine: None,
        home: t.0.clone(),
        env: &no_env,
    };
    assert!(!claims_switched(&none) && !Taker::new(&none, None).writer());
}

#[test]
fn files_mode_writes_claim_files_and_never_asks_the_core() {
    // F1
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let core = Fake::new(|_, _| panic!("Files mode asked the core"));
    let taker = Taker::new(&c, Some(&core));
    let holder = Sleeper::new();
    taker
        .claim_start(
            "s-1",
            holder.pid(),
            "oneshot",
            Duration::ZERO,
            None,
            Some("coder"),
        )
        .unwrap();
    assert_eq!(claim_files(&t), vec!["s-1.claim"]);
    assert_eq!(taker.epoch("alice"), Ok(String::new()));
    assert!(core.asked().is_empty());
}

#[test]
fn writer_mode_claims_in_the_core_and_writes_no_file() {
    // W1
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let core = Fake::ok();
    let taker = Taker::new(&c, Some(&core));
    let holder = Sleeper::new();
    taker
        .claim_start(
            "s-1",
            holder.pid(),
            "terminal",
            Duration::ZERO,
            None,
            Some("coder"),
        )
        .unwrap();
    assert!(claim_files(&t).is_empty());
    let asked = core.asked();
    assert_eq!(asked[0].0, "claim.external");
    let p = &asked[0].1;
    assert_eq!(
        (
            p["conversation"].as_str(),
            p["pid"].as_i64(),
            p["person"].as_str(),
            p["agent"].as_str()
        ),
        (Some("s-1"), Some(holder.pid()), Some(""), Some("coder"))
    );
    assert!(p["started"].is_string());
    assert!(p.get("revokeSeen").is_none() && p.get("engine").is_none());
    // A second writer is refused, with the line kith-wake's HELD pattern knows.
    let busy = Fake::new(|_, _| {
        Ok(json!({"refused": "held", "holder": {"claimId": 3, "kind": "terminal", "pid": 4242}}))
    });
    let e = Taker::new(&c, Some(&busy))
        .claim_start("s-1", holder.pid(), "terminal", Duration::ZERO, None, None)
        .unwrap_err();
    assert_eq!(
        e,
        "that session is busy: held by terminal (pid 4242); try again in a moment"
    );
}

#[test]
fn a_core_that_does_not_answer_is_never_answered_with_the_files() {
    // W2 and MUST 4: no core client at all reads the same
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let holder = Sleeper::new();
    for core in [Some(&Fake::down() as &dyn Core), None] {
        // The owner's run goes on under the stub's lock alone, with one line, no file, and the mark for the stub's clear.
        let owner = Taker::new(&c, core);
        owner
            .claim_start("s-1", holder.pid(), "terminal", Duration::ZERO, None, None)
            .unwrap();
        assert!(owner.alone());
        assert!(
            owner.said().contains("held by the conversation lock alone"),
            "{}",
            owner.said()
        );
        assert!(claim_files(&t).is_empty());
        // A person's run refuses: a revoke could not end it without a core claim.
        let person = Taker::new(&c, core);
        let e = person
            .claim_start(
                "s-1",
                holder.pid(),
                "terminal",
                Duration::ZERO,
                Some("alice"),
                Some("coder"),
            )
            .unwrap_err();
        assert!(
            e.starts_with("the session is busy until the Kith core answers"),
            "{e}"
        );
        assert!(!person.alone());
        // The epoch read and the sweep fail closed.
        assert!(
            Taker::new(&c, core)
                .epoch("alice")
                .unwrap_err()
                .contains("not answering")
        );
        let e = Taker::new(&c, core)
            .sweep(&["alice".to_owned()], Duration::ZERO)
            .unwrap_err();
        assert!(
            e.starts_with("kith-sessions: the claims moved to the core, and it gave no answer"),
            "{e}"
        );
        // proc: exit 9 with the line.
        let e = Taker::new(&c, core)
            .proc(holder.pid(), "alice", "coder")
            .unwrap_err();
        assert!(
            matches!(&e, ProcErr::Core(m) if m.starts_with("kith: the session is busy until the Kith core answers")),
            "{e:?}"
        );
        assert!(
            claim_files(&t).is_empty()
                && !t
                    .reg()
                    .join(format!("proc-{}.claim", holder.pid()))
                    .exists()
        );
    }
}

#[test]
fn a_session_id_the_stub_cannot_lock_runs_unclaimed_for_the_owner_only() {
    // W4
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let core = Fake::ok();
    let holder = Sleeper::new();
    let owner = Taker::new(&c, Some(&core));
    owner
        .claim_start(
            "Not_Lockable",
            holder.pid(),
            "terminal",
            Duration::ZERO,
            None,
            None,
        )
        .unwrap();
    assert!(core.asked().is_empty() && !owner.alone());
    assert!(
        owner.said().contains("cannot be locked"),
        "{}",
        owner.said()
    );
    let e = Taker::new(&c, Some(&core))
        .claim_start(
            "Not_Lockable",
            holder.pid(),
            "terminal",
            Duration::ZERO,
            Some("alice"),
            None,
        )
        .unwrap_err();
    assert!(e.contains("cannot be locked"), "{e}");
    let strict = run_env(&[("KITH_REQUIRE_STUB", "1")]);
    let e = Taker::new(&ctx(&t, &strict), Some(&core))
        .claim_start(
            "Not_Lockable",
            holder.pid(),
            "terminal",
            Duration::ZERO,
            None,
            None,
        )
        .unwrap_err();
    assert!(e.contains("cannot be locked"), "{e}");
}

#[test]
fn a_persons_claim_carries_the_epoch_bin_kith_read_before_kith_auth() {
    // W5: revokeSeen only from KITH_REVOKE_SEEN, only for that person; the core's refusal is the line
    let t = Tmp::new();
    switch(&t);
    let holder = Sleeper::new();
    let seen = run_env(&[("KITH_REVOKE_SEEN", "alice:3")]);
    let c = ctx(&t, &seen);
    let core = Fake::ok();
    Taker::new(&c, Some(&core))
        .claim_start(
            "s-1",
            holder.pid(),
            "oneshot",
            Duration::ZERO,
            Some("alice"),
            Some("coder"),
        )
        .unwrap();
    Taker::new(&c, Some(&core))
        .claim_start(
            "s-2",
            holder.pid(),
            "oneshot",
            Duration::ZERO,
            Some("bob"),
            Some("coder"),
        )
        .unwrap();
    let asked = core.asked();
    assert_eq!(
        asked.len(),
        2,
        "the epoch is never read afresh inside a claim"
    );
    assert_eq!(asked[0].1["revokeSeen"], 3);
    assert_eq!(asked[0].1["person"], "alice");
    assert!(asked[1].1.get("revokeSeen").is_none());
    let revoked = Fake::new(|_, _| {
        Ok(json!({"refused": "revoked", "why": "alice was revoked after it was read"}))
    });
    let e = Taker::new(&c, Some(&revoked))
        .claim_start(
            "s-1",
            holder.pid(),
            "oneshot",
            Duration::ZERO,
            Some("alice"),
            None,
        )
        .unwrap_err();
    assert_eq!(
        e,
        "the Kith core refused the claim (revoked): alice was revoked after it was read"
    );
    let rejected = Fake::new(|_, _| Err(CoreErr::Refused("bad param".to_owned())));
    let e = Taker::new(&c, Some(&rejected))
        .claim_start("s-1", holder.pid(), "oneshot", Duration::ZERO, None, None)
        .unwrap_err();
    assert_eq!(e, "the Kith core refused the claim: bad param");
}

#[test]
fn the_epoch_is_printed_or_fails_with_the_reason() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    assert_eq!(
        Taker::new(&c, Some(&Fake::ok())).epoch("alice"),
        Ok(String::new()),
        "Files mode prints nothing"
    );
    switch(&t);
    let core = Fake::ok();
    assert_eq!(
        Taker::new(&c, Some(&core)).epoch("alice"),
        Ok("alice:7\n".to_owned())
    );
    assert_eq!(
        core.asked()[0],
        ("claim.epoch".to_owned(), json!({"person": "alice"}))
    );
    for bad in [
        json!({"epoch": true}),
        json!({"epoch": "7"}),
        json!({"epoch": 1.5}),
        json!({}),
    ] {
        let core = Fake::new(move |_, _| Ok(bad.clone()));
        assert_eq!(
            Taker::new(&c, Some(&core)).epoch("alice"),
            Err("the Kith core gave no revoke epoch".to_owned())
        );
    }
    let refused = Fake::new(|_, _| Err(CoreErr::Refused("nope".to_owned())));
    let e = Taker::new(&c, Some(&refused)).epoch("alice").unwrap_err();
    assert!(
        e.contains("(nope); try again when kith serve is running"),
        "{e}"
    );
}

#[test]
fn proc_is_a_conversation_of_its_own_and_never_held_when_the_core_is_silent() {
    // W7 and MUST 7
    let t = Tmp::new();
    switch(&t);
    let c = ctx(&t, &no_env);
    let holder = Sleeper::new();
    let core = Fake::ok();
    Taker::new(&c, Some(&core))
        .proc(holder.pid(), "alice", "coder")
        .unwrap();
    let asked = core.asked();
    let p = &asked[0].1;
    let started = os::proc_start(holder.pid()).unwrap();
    assert_eq!(
        p["conversation"].as_str().unwrap(),
        format!("proc-{}-{started}", holder.pid())
            .to_lowercase()
            .replace(':', ".")
    );
    assert_eq!(
        (
            p["engine"].as_str(),
            p["person"].as_str(),
            p["agent"].as_str()
        ),
        (Some("proc"), Some("alice"), Some("coder"))
    );
    assert!(claim_files(&t).is_empty());
    // A core that says held is still no failure of proc (the Python ignores the answer); only no answer is exit 9.
    let held =
        Fake::new(|_, _| Ok(json!({"refused": "held", "holder": {"pid": 1, "kind": "terminal"}})));
    assert_eq!(
        Taker::new(&c, Some(&held)).proc(holder.pid(), "alice", "coder"),
        Ok(())
    );
    let e = Taker::new(&c, Some(&Fake::down())).proc(holder.pid(), "alice", "coder");
    assert!(matches!(e, Err(ProcErr::Core(_))), "{e:?}");
}

#[test]
fn the_apps_idle_agent_is_waited_for_a_terminal_too() {
    // W3
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[("KITH_CLAIM_WAIT", "1")]);
    let c = ctx(&t, &env);
    let holder = Sleeper::new();
    let n = std::cell::Cell::new(0);
    let core = Fake::new(move |_, _| {
        n.set(n.get() + 1);
        Ok(if n.get() < 3 {
            json!({"refused": "waiting", "why": "stopping"})
        } else {
            json!({"claimId": 9})
        })
    });
    Taker::new(&c, Some(&core))
        .claim_start(
            "s-1",
            holder.pid(),
            "terminal",
            Duration::ZERO,
            None,
            Some("coder"),
        )
        .unwrap();
    assert_eq!(core.asked().len(), 3);
    let stuck = Fake::new(|_, _| Ok(json!({"refused": "waiting", "why": "stopping"})));
    let e = Taker::new(
        &ctx(&t, &run_env(&[("KITH_CLAIM_WAIT", "0")])),
        Some(&stuck),
    )
    .claim_start("s-1", holder.pid(), "terminal", Duration::ZERO, None, None)
    .unwrap_err();
    assert_eq!(
        e,
        "that session is busy: the app's idle agent is still stopping; try again in a moment"
    );
}

#[test]
fn release_gives_back_what_the_parent_holds_in_either_mode() {
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let ppid = os::parent_pid();
    let mine = Claim {
        pid: Some(ppid),
        proc_start: os::proc_start(ppid),
        at: iso_utc(),
        kind: "terminal".to_owned(),
        person: None,
        agent: None,
        takeover: false,
    };
    t.put("s-1.claim", &pyjson::dumps(&mine.to_value()));
    let other = Sleeper::new();
    t.put(
        "s-2.claim",
        &pyjson::dumps(
            &Claim {
                pid: Some(other.pid()),
                ..mine
            }
            .to_value(),
        ),
    );
    let taker = Taker::new(&c, None);
    assert!(taker.release_own("s-1") && !t.reg().join("s-1.claim").exists());
    assert!(!taker.release_own("s-2") && t.reg().join("s-2.claim").exists());
    assert!(!taker.release_own("s-3"));
    // Writer mode: claim.release for the parent, on that conversation.
    switch(&t);
    let core = Fake::ok();
    assert!(Taker::new(&c, Some(&core)).release_own("s-1"));
    let asked = core.asked();
    assert_eq!(asked[0].0, "claim.release");
    assert_eq!(asked[0].1["pid"], ppid);
    assert_eq!(asked[0].1["conversation"], "s-1");
    let none = Fake::down();
    assert!(
        !Taker::new(&c, Some(&none)).release_own("s-1"),
        "a core that does not answer leaves the claim to its process"
    );
}

#[test]
fn the_sweep_asks_the_core_and_signals_only_the_positively_identified() {
    // W6
    let t = Tmp::new();
    switch(&t);
    let c = ctx(&t, &no_env);
    let victim = Sleeper::new();
    let bystander = Sleeper::new();
    let (vp, started) = (victim.pid(), os::proc_start(victim.pid()).unwrap());
    let bp = bystander.pid();
    let core = Fake::new(move |_, _| {
        Ok(json!({"kill": [
            {"pid": vp, "started": started},
            {"pid": bp, "started": "e1"},
            {"pid": 1, "started": "e1"},
        ]}))
    });
    let ended = Taker::new(&c, Some(&core))
        .sweep(
            &["bob".to_owned(), "alice".to_owned(), "bob".to_owned()],
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(ended, vec![vp]);
    assert_eq!(
        core.asked()[0],
        (
            "claim.sweepPerson".to_owned(),
            json!({"persons": ["alice", "bob"]})
        )
    );
    assert!(os::pid_runs(bp));
}

#[test]
fn a_file_linked_after_the_marker_went_down_is_taken_back() {
    // W12: the step's race
    let t = Tmp::new();
    let c = ctx(&t, &no_env);
    let holder = Sleeper::new();
    let out =
        take_claim_checked(&c, "s-1", holder.pid(), &Take::new("terminal"), &|| true).unwrap();
    assert_eq!(out, Outcome::Switched);
    assert!(claim_files(&t).is_empty());
    // Another process's file is not ours to take back.
    let other = Sleeper::new();
    let theirs = Claim {
        pid: Some(other.pid()),
        proc_start: os::proc_start(other.pid()),
        at: iso_utc(),
        kind: "terminal".to_owned(),
        person: None,
        agent: None,
        takeover: false,
    };
    t.put("s-2.claim", &pyjson::dumps(&theirs.to_value()));
    let out =
        take_claim_checked(&c, "s-2", holder.pid(), &Take::new("terminal"), &|| true).unwrap();
    assert!(matches!(out, Outcome::Held(_)));
    assert_eq!(claim_files(&t), vec!["s-2.claim"]);
}

#[test]
fn resolve_claims_in_the_core_with_the_agent_and_says_when_it_goes_on_alone() {
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let work = t.0.join("work");
    fs::create_dir_all(&work).unwrap();
    transcript(&t, "abc-1", &work);
    let holder = Sleeper::new();
    let core = Fake::ok();
    let taken = resolve_taking(
        &c,
        &NoHost,
        Some(&core),
        "coder",
        "abc-1",
        holder.pid(),
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(
        taken.shell(),
        format!("r_sid='abc-1' r_cwd='{}' r_from='abc-1'", work.display())
    );
    assert_eq!(core.asked()[0].1["agent"], "coder");
    assert!(claim_files(&t).is_empty());
    let down = Fake::down();
    let taken = resolve_taking(
        &c,
        &NoHost,
        Some(&down),
        "coder",
        "abc-1",
        holder.pid(),
        Duration::ZERO,
    )
    .unwrap();
    assert!(taken.alone && taken.shell().ends_with(" r_alone=1"));
    assert!(taken.said.contains("held by the conversation lock alone"));
    let busy =
        Fake::new(|_, _| Ok(json!({"refused": "held", "holder": {"pid": 9, "kind": "oneshot"}})));
    let e = resolve_taking(
        &c,
        &NoHost,
        Some(&busy),
        "coder",
        "abc-1",
        holder.pid(),
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(
        e,
        "that session is open in another writer (pid 9, oneshot); close it first"
    );
}

#[test]
fn the_cli_prints_lock_alone_and_exits_nine_for_proc() {
    let t = Tmp::new();
    switch(&t);
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let holder = Sleeper::new();
    let pid = holder.pid().to_string();
    let stdin = String::new;
    let down = Fake::down();
    let o = cli::run(
        &c,
        &NoHost,
        Some(&down),
        &args(&["terminal", "s-1", &pid]),
        &stdin,
    );
    assert_eq!((o.stdout.as_str(), o.code), ("lock-alone\n", 0));
    let o = cli::run(
        &c,
        &NoHost,
        Some(&down),
        &args(&["proc", &pid, "alice", "coder"]),
        &stdin,
    );
    assert_eq!(o.code, 9);
    assert!(
        o.stderr
            .starts_with("kith: the session is busy until the Kith core answers"),
        "{}",
        o.stderr
    );
    let o = cli::run(&c, &NoHost, Some(&down), &args(&["sweep", "alice"]), &stdin);
    assert_eq!(o.code, 3);
    let o = cli::run(
        &c,
        &NoHost,
        Some(&Fake::ok()),
        &args(&["epoch", "alice"]),
        &stdin,
    );
    assert_eq!((o.stdout.as_str(), o.code), ("alice:7\n", 0));
    let o = cli::run(&c, &NoHost, None, &args(&["caps"]), &stdin);
    assert_eq!(o.stdout, "registry-core\n");
    // release SID: nothing held by our parent is exit 1.
    let o = cli::run(
        &c,
        &NoHost,
        Some(&Fake::new(|_, _| Ok(json!({"ended": false})))),
        &args(&["release", "s-1"]),
        &stdin,
    );
    assert_eq!(o.code, 1);
}

// --- the registry --------------------------------------------------------------------------------------------------------

fn spooled(t: &Tmp) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(t.reg().join("inbox"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

const HOOK: &str = r#"{"session_id":"s-1","cwd":"/w"}"#;

#[test]
fn a_writer_mode_hook_spools_first_and_a_settling_answer_removes_its_entry() {
    // R1, R2
    let t = Tmp::new();
    t.put("registry.tombstone", "");
    let env = run_env(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")]);
    let c = ctx(&t, &env);
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let (s2, dir) = (seen.clone(), t.reg().join("inbox"));
    let core = Fake::new(move |m, p| {
        // The entry is there while the core is asked.
        s2.borrow_mut().push((
            m.to_owned(),
            fs::read_dir(&dir).unwrap().count(),
            p["record"]["sessionId"].clone(),
        ));
        Ok(json!({"outcome": "recorded"}))
    });
    assert!(spool::record(&c, Some(&core), HOOK).is_empty());
    assert_eq!(
        seen.borrow()[0],
        ("session.record".to_owned(), 1, json!("s-1"))
    );
    assert!(spooled(&t).is_empty(), "settled: the hook's own entry goes");
    assert!(
        !t.reg().join("s-1.json").exists(),
        "the core writes the file, never the hook"
    );
    // Older settles too; invalid is deleted and said.
    let older = Fake::new(|_, _| Ok(json!({"outcome": "older"})));
    assert!(spool::record(&c, Some(&older), HOOK).is_empty() && spooled(&t).is_empty());
    let invalid = Fake::new(|_, _| Ok(json!({"refused": "invalid", "why": "bad"})));
    let said = spool::record(&c, Some(&invalid), HOOK);
    assert_eq!(
        said,
        vec!["the core refused the record (bad); not listed".to_owned()]
    );
    assert!(spooled(&t).is_empty());
}

#[test]
fn anything_else_keeps_the_spool_entry_for_the_cores_drain() {
    // R3
    let t = Tmp::new();
    t.put("registry.tombstone", "");
    let env = run_env(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")]);
    let c = ctx(&t, &env);
    for (core, why) in [
        (Some(&Fake::down() as &dyn Core), "no core daemon answers"),
        (
            Some(&Fake::new(|_, _| Err(CoreErr::Refused("bad".to_owned()))) as &dyn Core),
            "the core rejected the call: bad",
        ),
        (
            Some(&Fake::new(|_, _| Ok(json!({"refused": "not serving"}))) as &dyn Core),
            "not serving",
        ),
        (
            Some(&Fake::new(|_, _| Ok(json!({}))) as &dyn Core),
            "no outcome in the answer",
        ),
        (None, "no kith program to ask"),
    ] {
        let said = spool::record(&c, core, HOOK);
        assert_eq!(said, vec![format!("kept for the core ({why})")]);
    }
    assert_eq!(spooled(&t).len(), 5, "one entry per hook, none lost");
    // A session id the registry cannot name is not listed and not spooled.
    let said = spool::record(&c, None, r#"{"session_id":"a.b","cwd":"/w"}"#);
    assert!(
        said[0].contains("is not one the registry can name"),
        "{said:?}"
    );
}

#[test]
fn files_mode_writes_the_file_then_drains_newer_entries_and_rechecks_the_marker() {
    // R4, R5
    let t = Tmp::new();
    let env = run_env(&[("KITH_AGENT", "coder"), ("KITH_RUN_KIND", "schedule")]);
    let c = ctx(&t, &env);
    let inbox = t.reg().join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let rec = |at: u64, cwd: &str| {
        json!({"v": 1, "sessionId": "s-2", "agent": "coder", "cwd": cwd, "recordedAt": at})
            .to_string()
    };
    fs::write(inbox.join("s-2.5.1.json"), rec(5, "/newer")).unwrap();
    fs::write(inbox.join("s-3.1.1.json"), r#"{"v":1,"sessionId":"other"}"#).unwrap();
    fs::write(inbox.join("junk.json"), "x").unwrap();
    t.put("s-2.json", &rec(3, "/older"));
    let never = Fake::new(|_, _| panic!("Files mode asked the core"));
    assert!(spool::record(&c, Some(&never), HOOK).is_empty());
    assert!(t.reg().join("s-1.json").exists());
    let kept: Value =
        serde_json::from_str(&fs::read_to_string(t.reg().join("s-2.json")).unwrap()).unwrap();
    assert_eq!(
        kept["cwd"], "/newer",
        "the newer entry took the file's place"
    );
    assert!(
        spooled(&t).is_empty(),
        "the invalid ones are deleted, never placed"
    );
}

#[test]
fn a_record_names_the_kith_stub_it_runs_under() {
    // MUST 9: claude under the conversation lock
    let t = Tmp::new();
    // A real program named kith-stub (a copy of a system binary does not run): it starts a sleeper below it and waits.
    let src = t.0.join("stub.c");
    fs::write(
        &src,
        "#include <unistd.h>\nint main(void) { if (fork() == 0) { execlp(\"sleep\", \"sleep\", \"30\", (char *)0); } sleep(30); return 0; }\n",
    )
    .unwrap();
    let stub = t.0.join("kith-stub");
    let built = std::process::Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".to_owned()))
        .arg(&src)
        .arg("-o")
        .arg(&stub)
        .status();
    if !built.is_ok_and(|s| s.success()) {
        eprintln!("no C compiler: skipped");
        return;
    }
    let mut child = std::process::Command::new(&stub).spawn().unwrap();
    let stub_pid = i64::from(child.id());
    let mut sleeper = None;
    for _ in 0..100 {
        let below = std::process::Command::new("pgrep")
            .args(["-P", &stub_pid.to_string()])
            .output()
            .unwrap();
        sleeper = String::from_utf8_lossy(&below.stdout)
            .trim()
            .lines()
            .next()
            .and_then(|l| l.parse::<i64>().ok());
        if sleeper.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let sleeper = sleeper.expect("the stub started no child");
    let got = registry::launcher_of(Some(sleeper));
    let _ = std::process::Command::new("kill")
        .arg(sleeper.to_string())
        .status();
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(got[0], ("launcher", json!(stub_pid)));
    assert_eq!(got[1].0, "launcherStart");
    assert!(registry::launcher_of(Some(me())).is_empty());
}

// --- pick and latest -----------------------------------------------------------------------------------------------------

fn ledger(t: &Tmp, rows: &[(&str, &str, &str, &str, &str, Option<&str>)]) {
    let machine = t.0.join("machine");
    fs::create_dir_all(&machine).unwrap();
    let db = rusqlite::Connection::open(machine.join("cost.db")).unwrap();
    db.execute_batch("create table runs (session text, agent text, started text, via text, project text, person text)").unwrap();
    for (s, a, st, via, pr, person) in rows {
        db.execute(
            "insert into runs values (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![s, a, st, via, pr, person],
        )
        .unwrap();
    }
}

#[test]
fn pick_and_latest_name_a_session_from_the_ledger() {
    let t = Tmp::new();
    let env = run_env(&[]);
    let c = ctx(&t, &env);
    let (a, b) = (t.0.join("a"), t.0.join("b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    transcript(&t, "old-1", &a);
    transcript(&t, "new-1", &b);
    ledger(
        &t,
        &[
            (
                "old-1",
                "coder",
                "2026-11-01T10:00:00Z",
                "terminal",
                "kith",
                None,
            ),
            ("new-1", "coder", "2026-11-02T10:00:00Z", "ask", "", None),
            (
                "gone-1",
                "coder",
                "2026-11-03T10:00:00Z",
                "terminal",
                "kith",
                None,
            ),
            (
                "codex:x",
                "coder",
                "2026-11-04T10:00:00Z",
                "terminal",
                "kith",
                None,
            ),
            (
                "guest-1",
                "coder",
                "2026-11-05T10:00:00Z",
                "terminal",
                "kith",
                Some("alice"),
            ),
            (
                "other-1",
                "reviewer",
                "2026-11-05T10:00:00Z",
                "terminal",
                "kith",
                None,
            ),
        ],
    );
    let stdin = String::new;
    let go = |a: &[&str], line: &'static str| {
        let prompts = RefCell::new(Vec::new());
        let o = cli::run_with(&c, &NoHost, None, &args(a), &stdin, &|p| {
            prompts.borrow_mut().push(p.to_owned());
            line.to_owned()
        });
        (o, prompts.into_inner())
    };
    let (o, prompts) = go(&["pick", "coder"], "2\n");
    assert_eq!((o.stdout.as_str(), o.code), ("old-1\n", 0));
    assert!(prompts[0].starts_with("  1  "), "{}", prompts[0]);
    assert!(
        prompts[0].contains("ask       -            ")
            && prompts[0].ends_with("resume which? (number, Enter to cancel) "),
        "{}",
        prompts[0]
    );
    assert_eq!(go(&["pick", "coder"], "\n").0.code, 1);
    assert_eq!(
        go(&["pick", "coder"], "3\n").0.stderr,
        "kith: no session 3 in the list\n"
    );
    assert_eq!(
        go(&["pick", "coder"], "x\n").0.stderr,
        "kith: no session x in the list\n"
    );
    let o = go(&["pick", "nobody"], "1\n").0;
    assert!(
        o.stderr.starts_with("kith: no nobody sessions to resume"),
        "{}",
        o.stderr
    );
    let (o, _) = go(&["latest", "coder", &a.display().to_string()], "");
    assert_eq!(o.stdout, "old-1\n");
    let o = go(&["latest", "coder", "/nowhere/at/all"], "").0;
    assert!(
        o.stderr
            .starts_with("kith: no coder session started in /nowhere/at/all to continue"),
        "{}",
        o.stderr
    );
    // No ledger on this Mac: nothing to pick.
    let t2 = Tmp::new();
    assert!(
        pick::recent(&ctx(&t2, &env), "coder", 15, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_first_thing_typed_is_the_title_without_tags() {
    let t = Tmp::new();
    let f = t.0.join("t.jsonl");
    fs::write(
        &f,
        concat!(
            "not json\n",
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"meta\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>/x</command-name>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"image\"},{\"type\":\"text\",\"text\":\"  fix   the\\n build \"}]}}\n",
        ),
    )
    .unwrap();
    assert_eq!(pick::first_text(&f, 70), "fix the build");
    assert_eq!(pick::first_text(&f, 7), "fix th…");
    assert_eq!(pick::first_text(&t.0.join("none"), 70), "");
}
