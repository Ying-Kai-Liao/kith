#![allow(clippy::unwrap_used)]
//! The `claims` step (P9) on temp homes only: a real core.db, real-shaped claim files and real child processes (the claims
//! name a pid that must be alive). Each preflight refusal fails on its own while the others pass; a step, work and a rollback
//! go round once. Nothing here touches the machine's own Kith folder.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use rusqlite::Connection;
use serde_json::{Value, json};

use super::claims::{Claims, STUB, TestStub};
use crate::cutover::same_answer::may_serve;
use crate::cutover::state::{self, Phase, Switch};
use crate::cutover::{Env, Module, Opts, registry, rollback, step, tombstone};
use crate::tests::machine;

fn opts() -> Opts {
    Opts::default()
}

fn dry() -> Opts {
    Opts {
        dry_run: true,
        ..Opts::default()
    }
}

fn force() -> Opts {
    Opts {
        force: true,
        ..Opts::default()
    }
}

/// A child that outlives the test body and is killed when dropped: a claim names a pid that is alive.
struct Proc(Child);

impl Proc {
    fn new() -> Self {
        Self(Command::new("sleep").arg("120").spawn().unwrap())
    }

    fn pid(&self) -> i64 {
        i64::from(self.0.id())
    }

    fn stamp(&self) -> String {
        match kith_kernel::procstart::probe(self.0.id()) {
            kith_kernel::procstart::Probe::Alive(Some(s)) => s,
            other => panic!("{other:?}"),
        }
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Rig {
    dir: tempfile::TempDir,
    env: Env,
    host: PathBuf,
}

fn rig() -> Rig {
    let (t, m) = machine();
    let mut paths = crate::tests::paths(&m);
    paths.state = m.join("state");
    paths.home = m.join("home");
    let mut env = Env::with_paths(&paths);
    // The host's data folder is the tmp folder's, so `host.lock` is this test's to hold.
    let host = t.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    env.serve_db = Some(host.join("serve.db"));
    std::fs::create_dir_all(env.state.as_ref().unwrap().join("live/terminal")).unwrap();
    // A stub that exists and runs; the real lookup is not the test's business.
    let stub = t.path().join("kith-stub");
    std::fs::write(&stub, "#!/bin/sh\n").unwrap();
    set_mode(&stub, 0o755);
    STUB.with(|s| *s.borrow_mut() = Some(TestStub::At(stub)));
    // core.db exists and is migrated (a Mac that has run the core), and the lock is let go again.
    drop(crate::tests::open_all(&env.layout));
    Rig { dir: t, env, host }
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

impl Rig {
    fn terminal(&self) -> PathBuf {
        self.env.state.as_ref().unwrap().join("live/terminal")
    }

    fn marker(&self) -> PathBuf {
        self.terminal().join("claims.tombstone")
    }

    fn file(&self, name: &str, body: &Value) {
        std::fs::write(self.terminal().join(name), body.to_string()).unwrap();
    }

    fn db(&self) -> Connection {
        Connection::open(&self.env.layout.db).unwrap()
    }

    /// The host holding `host.lock`, as a running host does.
    fn running_host(&self) -> std::fs::File {
        use fs2::FileExt as _;
        let f = std::fs::File::create(self.host.join("host.lock")).unwrap();
        f.lock_exclusive().unwrap();
        f
    }

    fn phase(&self) -> Option<Phase> {
        state::read(&self.env.layout, "claims")
            .unwrap()
            .map(|s| s.phase)
    }

    fn serves(&self) -> bool {
        may_serve(&Claims, &self.env, &self.db(), false).ok
    }

    fn step(&self, o: Opts) -> crate::error::Result<Vec<String>> {
        step(&registry(), &self.env, "claims", o)
    }

    fn rollback(&self, o: Opts) -> crate::error::Result<Vec<String>> {
        rollback(&registry(), &self.env, "claims", o)
    }

    /// Nothing was written: no record, no marker, no file the takers read.
    fn untouched(&self) {
        assert_eq!(self.phase(), None, "no switch record");
        assert!(!self.marker().exists(), "no marker");
        assert!(crate::cutover::marker::read(&self.db(), "claims").is_none());
    }

    fn live_claim(&self, sid: &str, p: &Proc) {
        self.file(
            &format!("{sid}.claim"),
            &json!({"pid": p.pid(), "procStart": p.stamp(), "at": "t", "kind": "terminal"}),
        );
    }
}

fn err(r: crate::error::Result<Vec<String>>) -> String {
    r.unwrap_err().to_string()
}

// --- the preflight: each refusal on its own ---------------------------------------------------------------------------------

#[test]
fn a_healthy_mac_passes_the_dry_run() {
    let r = rig();
    let out = r.step(dry()).unwrap();
    assert!(
        out.iter().any(|l| l.contains("preflight: ready")),
        "{out:?}"
    );
    r.untouched();
}

#[test]
fn each_preflight_refusal_stops_a_dry_run_and_a_step_on_its_own_and_leaves_nothing() {
    type Break = fn(&Rig, &mut Vec<Proc>) -> Option<std::fs::File>;
    let cases: Vec<(&str, Break, &str)> = vec![
        (
            "no stub",
            |_, _| {
                STUB.with(|s| *s.borrow_mut() = Some(TestStub::Nowhere));
                None
            },
            "kith-stub was not found",
        ),
        (
            "a stub that does not run",
            |r, _| {
                let stub = r.dir.path().join("kith-stub");
                set_mode(&stub, 0o644);
                None
            },
            "is not executable",
        ),
        (
            "the host running",
            |r, _| Some(r.running_host()),
            "the host is running",
        ),
        (
            "a session id the stub cannot lock",
            |r, kids| {
                kids.push(Proc::new());
                r.live_claim("Bad_Sid", &kids[0]);
                None
            },
            "cannot be locked by the stub",
        ),
        (
            "a conversation no agent is known for",
            |r, kids| {
                kids.push(Proc::new());
                r.live_claim("s-nowhere", &kids[0]);
                None
            },
            "names its agent",
        ),
        (
            "a take-over in flight",
            |r, kids| {
                kids.push(Proc::new());
                r.file(
                    "s-t.takeover.claim",
                    &json!({"pid": kids[0].pid(), "procStart": kids[0].stamp(), "at": "t", "kind": "terminal", "takeover": true}),
                );
                None
            },
            "a take-over is in flight",
        ),
        (
            "a person's run with no start stamp",
            |r, kids| {
                kids.push(Proc::new());
                r.file(
                    &format!("proc-{}.claim", kids[0].pid()),
                    &json!({"pid": kids[0].pid(), "procStart": null, "at": "t", "kind": "app", "person": "sam", "agent": "pilot"}),
                );
                None
            },
            "recorded no start stamp",
        ),
    ];
    for (name, breaks, line) in cases {
        let r = rig();
        let mut kids = Vec::new();
        let _held = breaks(&r, &mut kids);
        for o in [dry(), opts()] {
            let e = err(r.step(o));
            assert!(e.contains(line), "{name}: {e}");
            // Only that one: the other four items count zero.
            assert_eq!(e.matches(": 1 (").count(), 1, "{name}: {e}");
        }
        r.untouched();
    }
}

// --- a step, work, a rollback --------------------------------------------------------------------------------------------

#[test]
fn a_step_work_and_a_rollback_go_round() {
    use crate::conversations::external::call_at;
    let r = rig();
    let (mut one, mut two) = (Proc::new(), Proc::new());
    // A terminal's claim on a session the core has not imported (the registry record names its agent), and a person's
    // session-less run.
    r.live_claim("s-live", &one);
    r.file(
        "s-live.json",
        &json!({"agent": "scout", "engine": "claude"}),
    );
    r.file(
        &format!("proc-{}.claim", two.pid()),
        &json!({"pid": two.pid(), "procStart": two.stamp(), "at": "t", "kind": "app", "person": "sam", "agent": "pilot"}),
    );

    let out = r.step(opts()).unwrap();
    assert!(out.iter().any(|l| l == "claims: switched"), "{out:?}");
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(
        tombstone::is_tombstone(&r.marker()),
        "the marker the takers read"
    );
    assert_eq!(tombstone::module_of(&r.marker()).as_deref(), Some("claims"));
    assert!(r.serves(), "the core serves the claims");
    let rows = |sid: &str| -> i64 {
        r.db()
            .query_row(
                "SELECT COUNT(*) FROM claims WHERE conversation = ?1 AND kind = 'external' AND state = 'running'",
                [sid],
                |x| x.get(0),
            )
            .unwrap()
    };
    assert_eq!(rows("s-live"), 1);
    let key = crate::import::claims::proc_key(two.pid(), &two.stamp());
    assert_eq!(rows(&key), 1, "the run is a claim a revoke finds");

    // Work after the step: a new run claims through the core, is listed, and gives its claim back.
    let mut core = crate::tests::open_all(&r.env.layout);
    // The generation's claims wait for its holder's reconcile (the host's holder does it once connected); none lists a claim here.
    crate::conversations::claims::reconcile(&mut core, &[], &[], i64::MAX, 1).unwrap();
    let three = Proc::new();
    let ask = json!({"conversation": "s-work", "pid": three.pid(), "started": three.stamp(), "agent": "scout"});
    let got = call_at(
        &mut core,
        "claim.external",
        &ask,
        1_000,
        &crate::gate::System,
    )
    .unwrap();
    assert!(got["claimId"].as_i64().is_some(), "{got}");
    let list = call_at(
        &mut core,
        "claim.list",
        &json!({}),
        1_000,
        &crate::gate::System,
    )
    .unwrap();
    assert_eq!(list["live"].as_array().unwrap().len(), 3, "{list}");
    // A file claim taken before the step is given back after it through the same release (`kith-sessions release`), as is the new one.
    for (p, conv) in [(&three, "s-work"), (&one, "s-live")] {
        let rel = json!({"pid": p.pid(), "started": p.stamp(), "conversation": conv});
        call_at(
            &mut core,
            "claim.release",
            &rel,
            2_000,
            &crate::gate::System,
        )
        .unwrap();
    }
    drop(core);
    assert_eq!(
        rows("s-live") + rows("s-work"),
        0,
        "the imported claim ended with its release"
    );

    // A rollback while a core claim is live is refused, forced or not, and changes nothing.
    let e = err(r.rollback(opts()));
    assert!(
        e.contains("1 conversation claim(s) or take(s) are live"),
        "{e}"
    );
    let e = err(r.rollback(force()));
    assert!(
        e.contains("not rolling back while a core claim is live") && e.contains("proc-"),
        "{e}"
    );
    assert!(e.contains("--force does not override this"), "{e}");
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(r.marker().exists());

    // Their processes end; the claims of a process that is gone are reaped, not refused, and the rest of the rollback runs.
    one.kill();
    two.kill();
    drop(three);
    let out = r.rollback(opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("rolled back")), "{out:?}");
    assert_eq!(r.phase(), None);
    assert!(!r.marker().exists(), "the takers write files again");
    assert!(!r.serves());

    // And a step again is the same step (the imported rows are ended, nothing new).
    r.step(opts()).unwrap();
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(r.marker().exists());
}

#[test]
fn a_claim_whose_process_is_gone_does_not_block_a_rollback() {
    let r = rig();
    r.step(opts()).unwrap();
    let mut gone = Proc::new();
    let (pid, stamp) = (gone.pid(), gone.stamp());
    gone.kill();
    let db = r.db();
    db.execute(
        "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('s-dead', 'a', 'claude', 1, 1)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO claims (claim_id, conversation, generation, kind, state, pid, pid_started, created_at)
         VALUES (901, 's-dead', 1, 'external', 'running', ?1, ?2, 1)",
        rusqlite::params![pid, stamp],
    )
    .unwrap();
    drop(db);
    let out = r.rollback(opts()).unwrap();
    assert!(out.iter().any(|l| l.contains("rolled back")), "{out:?}");
    let state: String = r
        .db()
        .query_row("SELECT state FROM claims WHERE claim_id = 901", [], |x| {
            x.get(0)
        })
        .unwrap();
    assert_eq!(state, "ended");
}

#[test]
fn a_running_host_refuses_a_rollback_even_forced() {
    let r = rig();
    r.step(opts()).unwrap();
    let _host = r.running_host();
    for o in [opts(), force()] {
        let e = err(r.rollback(o));
        assert!(
            e.contains("the host is running") && e.contains("--force does not override"),
            "{e}"
        );
    }
    assert_eq!(r.phase(), Some(Phase::Switched));
    assert!(r.marker().exists());
}

// --- the step's own re-checks ---------------------------------------------------------------------------------------------

#[test]
fn a_host_started_after_the_preflight_makes_the_step_refuse_before_the_marker() {
    let r = rig();
    let _host = r.running_host();
    let e = err(Claims.before_tombstone(&r.env).map(|()| Vec::new()));
    assert!(
        e.contains("the host is running") && e.contains("NOT switched"),
        "{e}"
    );
    assert!(!r.marker().exists(), "the marker comes after the probe");
}

#[test]
fn a_live_claim_that_appears_after_the_marker_stops_the_step_and_a_cancel_takes_the_marker_away() {
    let r = rig();
    let kid = Proc::new();
    // A taker that checked the mode before the marker and linked its file after it.
    r.live_claim("s-late", &kid);
    let e = err(Claims.before_tombstone(&r.env).map(|()| Vec::new()));
    assert!(
        e.contains("a live claim appeared or cannot be imported") && e.contains("s-late"),
        "{e}"
    );
    assert!(
        r.marker().exists(),
        "the marker is down first, so no later taker writes a file"
    );
    // The step stayed `stepping`; cancelling it removes the file (before the record), so no taker fails closed for ever.
    state::write(
        &r.env.layout,
        "claims",
        &Switch {
            phase: Phase::Stepping,
            snapshot: None,
            at: 1,
        },
    )
    .unwrap();
    let out = r.rollback(opts()).unwrap();
    assert!(
        out.iter()
            .any(|l| l.contains("unfinished step was cancelled")),
        "{out:?}"
    );
    assert!(!r.marker().exists());
    assert_eq!(r.phase(), None);
}

// --- the other modules ------------------------------------------------------------------------------------------------------

#[test]
fn transcripts_waits_for_claims_and_still_refuses_after_them() {
    let r = rig();
    assert_eq!(
        crate::cutover::modules::transcripts::Transcripts.depends_on(),
        ["claims"]
    );
    r.step(opts()).unwrap();
    let e = err(step(&registry(), &r.env, "transcripts", opts()));
    assert!(e.contains("kith-sessions record"), "{e}");
}
