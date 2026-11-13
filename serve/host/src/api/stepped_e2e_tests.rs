//! The switched-Mac end to end (design 2026-11-13, P2): a scratch home is stepped for real (`kith migrate step authority`)
//! with the switch OFF, which is the runbook's own configuration, and the owner's paths are run in one sequence against
//! the REAL kith-auth (on the tombstone, answering from the core through `kith kernel core-call`), the real core daemon
//! and the real `kith rollback authority`. Stubbed: the cloud (the rig's fake), the engines (a `sleep` for a run), the
//! Keychain (a `security` stand-in), Telegram and Clerk. Never stubbed: kith-auth, core-call, the daemon, the step, the
//! rollback.
//!
//! It needs the built `kith` program (`KITH_BIN`), so it is `#[ignore]`d: the other jobs list it as ignored, never as
//! passed. Only the named step of host-build runs it (`--run-ignored only`), after building `kith`, and there a missing
//! binary is a failure: the test never skips.
//!
//! Throughout, auth.json is the tombstone byte for byte until the rollback, and a logging wrapper around kith-auth
//! records each exit code: an exit 3 may only be one of [`EXPECTED_EXIT_3`].
//!
//! Pending until the core routes of the P1 package land (`rust-migrate-blockers-routes`): the `/link` and `linkCode`
//! cases (step 3), the terminal `kith kernel core-call people.suspend` reconcile (step 4), and `kith-auth share`
//! (step 6). They are the `#[ignore]` tests at the end of this file, and their verbs are in the expected-exit-3 list.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::format_push_string, clippy::many_single_char_names)]

use super::access_core_tests::Core;
use super::devices::Caller;
use super::dispatch;
use super::rig::Rig;
use super::roles;
use crate::cards::{self, edge};
use crate::channels::hq::TEST_ENV;
use crate::remote::account::TEST_BIN;
use crate::remote::members::{self, Listed};
use crate::runauth::{RunIdentity, TokenScope};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The kith-auth verbs allowed to exit 3 on this stepped Mac: `once`, the room secret, which fails closed (Kai's q236
/// decision). Phase 2 keeps it at this one; no other verb may exit 3.
const EXPECTED_EXIT_3: &[&str] = &["once"];

const CAPS: &str = "helpers: [money]\njobs:\n  - name: make-videos\n    label: Make videos\n    domains: [registry.npmjs.org, github.com]\n";

/// A marker built at run time that stands for a key's value: it must reach the stand-in Keychain and nothing else.
fn marker() -> String {
    format!("sentinel-e2e-{}", uuid::Uuid::new_v4().simple())
}

fn kith_bin() -> PathBuf {
    std::env::var_os("KITH_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .expect("the stepped e2e needs the built kith program (KITH_BIN): a missing binary is a failure, not a skip")
}

struct World {
    r: Rig,
    core: Core,
    kith: PathBuf,
    /// The wrapper's log: one `<exit code> <verb>` line per kith-auth run.
    log: PathBuf,
    /// auth.json as the step left it.
    tomb: Vec<u8>,
    /// The stand-in Keychain's folder (`keychain.json`).
    rec: PathBuf,
    sam: Caller,
    /// The tombstone: `<machine>/secrets/auth.json`.
    auth_file: PathBuf,
}

impl World {
    /// Step 0 (the setup on the file, then the real step with the opt-in off).
    async fn new() -> Self {
        let kith = kith_bin();
        let mut r = Rig::new().await;
        std::fs::write(r.dir.join("home/agents/house/capabilities.yaml"), CAPS).unwrap();
        r.owner_bot("house");
        // A logging wrapper around the REAL kith-auth: it records the exit code of every run, and changes nothing.
        let log = r.dir.join("kith-auth-exits");
        let wrapper = r.dir.join("kith-auth-logged");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n'{}' \"$@\"\nrc=$?\necho \"$rc $1\" >> '{}'\nexit $rc\n",
                r.bin.display(),
                log.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        r.bin.clone_from(&wrapper);
        *TEST_BIN.lock().unwrap() = Some(wrapper);
        let rec = cards::room_secret_tests::keychain(&r);
        TEST_ENV.lock().unwrap().push(("KITH_BIN", kith.clone()));

        // The file: owner kai, Sam invited to house as a member, and one profile whose key is in the (stand-in) Keychain.
        let made =
            dispatch(&r.hub, &Caller::Local, "createInvite", json!({"name": "Sam", "agents": {"house": "member"}}))
                .await
                .unwrap();
        let listed = [Listed {
            membership_id: "mem_sam".into(),
            user_id: "user_sam".into(),
            invite_id: made["inviteId"].as_str().unwrap().to_owned(),
            ..Default::default()
        }];
        members::apply(&r.hub, Some(&r.bin), &listed, Some("user_owner")).await.unwrap();
        let sam = r.device("sam-phone", Some("sam"));
        let stored = auth_in(
            &r,
            &["set", "grok/work", "--stdin-json", "--grant", "house"],
            &json!({"GROK_WEBHOOK_URL": "https://example.invalid/hook", "GROK_WEBHOOK_KEY": marker()}).to_string(),
        );
        assert_eq!(stored.0, 0, "the file takes the key: {stored:?}");
        assert!(r.allowed("sam", "house"), "the file lets Sam in before the step");

        // The real step, with the opt-in OFF: nothing sets KITH_CORE_AUTHORITY, the test flag is not set either. The
        // home is laid out as production lays it out, and a child is told only KITH_STATE, KITH_HOME and KITH_AGENTS.
        let core = Core::start_stepped_on_home(&r);
        let auth_file = core.data().folders.secrets.join("auth.json");
        let tomb = std::fs::read(&auth_file).unwrap();
        let w = Self { r, core, kith, log, tomb, rec, sam, auth_file };
        w.assert_off_and_tombstone();
        w
    }

    /// The switch is off and auth.json is the tombstone, byte for byte.
    fn assert_off_and_tombstone(&self) {
        assert!(!super::authority::opted_in(), "KITH_CORE_AUTHORITY is set");
        assert!(
            !super::authority::TEST_OPT_IN.load(std::sync::atomic::Ordering::SeqCst),
            "the test opt-in stands: this is not the runbook's configuration"
        );
        assert!(super::authority::switched_now(), "the step left no tombstone");
        let now = std::fs::read(&self.auth_file).unwrap();
        assert_eq!(now, self.tomb, "auth.json moved: the tombstone is not the same bytes");
        let v: Value = serde_json::from_slice(&now).unwrap();
        assert_eq!(v["kith_tombstone"], 1, "{v}");
    }

    fn core_rows(&self, person: &str) -> Vec<(String, String)> {
        let list = self.core.ask("people.list", json!({"person": person})).expect("list");
        let mut rows: Vec<_> = list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["agent"].as_str().unwrap().to_owned(), r["preset"].as_str().unwrap().to_owned()))
            .collect();
        rows.sort();
        rows
    }

    /// The exit codes the wrapper saw: no exit 3 but the expected ones.
    fn assert_exit_3s(&self) -> Vec<String> {
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let threes: Vec<String> = log
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(rc, _)| *rc == "3")
            .map(|(_, verb)| verb.to_owned())
            .collect();
        for v in &threes {
            assert!(EXPECTED_EXIT_3.contains(&v.as_str()), "kith-auth {v} exited 3, and nothing expects that: {log}");
        }
        assert!(log.lines().count() > 20, "the wrapper saw too few runs to mean anything: {log}");
        threes
    }

    fn keychain(&self) -> Value {
        std::fs::read_to_string(self.rec.join("keychain.json")).map_or(json!({}), |s| serde_json::from_str(&s).unwrap())
    }

    /// A fixed set of reads of the one authority, as the router check's `post_state`: per person and agent a `may` and a
    /// `resolve`, then the lists. Times and ids are blanked.
    fn reads(&self) -> String {
        let mut asked: Vec<Vec<&str>> = Vec::new();
        for p in ["kai", "sam", "ann", "nobody"] {
            for a in ["house", "money"] {
                asked.push(vec!["may", p, a]);
                asked.push(vec!["resolve", a, "--person", p]);
            }
        }
        asked.extend([
            vec!["people", "--json"],
            vec!["keys", "--json"],
            vec!["ledger", "--json"],
            vec!["jobs", "house", "--json"],
        ]);
        let mut out = String::new();
        for args in asked {
            let (rc, said) = run_auth(&self.r, &args);
            out.push_str(&format!("$ {}\n{said}\nrc={rc}\n", args.join(" ")));
        }
        blank(&out)
    }
}

/// `kith-auth args` with the rig's environment: (exit code, stdout).
fn run_auth(r: &Rig, args: &[&str]) -> (i32, String) {
    auth_in(r, args, "")
}

fn auth_in(r: &Rig, args: &[&str], stdin: &str) -> (i32, String) {
    use std::io::Write as _;
    let mut c = std::process::Command::new(&r.bin);
    c.args(args).envs(TEST_ENV.lock().unwrap().iter().map(|(k, v)| (*k, v)));
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    if code == 3 {
        eprintln!("kith-auth {args:?} exited {code}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    (code, String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Times (13 digits of milliseconds, an ISO stamp) differ between two reads of the same state.
fn blank(s: &str) -> String {
    let mut out = String::new();
    let (mut run, mut digits) = (String::new(), 0usize);
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits += 1;
            run.push(c);
            continue;
        }
        out += &if digits == 13 { "<ms>".to_owned() } else { std::mem::take(&mut run) };
        run.clear();
        digits = 0;
        out.push(c);
    }
    out += &if digits == 13 { "<ms>".to_owned() } else { run };
    // ISO stamps (`2026-11-13T10:11:12`)
    let mut v = out.into_bytes();
    let n = v.len();
    let mut i = 0;
    while i + 19 <= n {
        let w = &v[i..i + 19];
        let shape = |k: usize, c: u8| w[k] == c;
        if shape(4, b'-') && shape(7, b'-') && shape(10, b'T') && shape(13, b':') && shape(16, b':') {
            v.splice(i..i + 19, *b"<t>");
            continue;
        }
        i += 1;
    }
    String::from_utf8(v).unwrap()
}

fn guest(person: &str) -> RunIdentity {
    RunIdentity {
        person: person.into(),
        agent: "house".into(),
        room: Some(format!("1:1:house:{person}")),
        run_id: format!("run-{person}"),
        scope: TokenScope::Guest,
        session: format!("run:1:1:house:{person}"),
        project: None,
        reply: None,
    }
}

/// A run of `person` that the host lost track of: a live claim naming them, held by a process of its own.
fn claimed_run(r: &Rig, sid: &str, person: &str) -> std::process::Child {
    let child = std::process::Command::new("sleep").arg("300").spawn().unwrap();
    let state = r.hub.terminal.env().state;
    crate::terminal::claim::take_as(
        &state,
        sid,
        crate::terminal::claim::Kind::Bot,
        child.id(),
        false,
        Some(person),
        Some("house"),
    )
    .unwrap()
    .unwrap();
    child
}

/// One of Sam's own chats on `agent` (a member's chat the guard knows the owner of).
fn sams_chat(r: &Rig, agent: &str) -> String {
    let cfg = serde_json::from_value(json!({
        "id": "", "name": agent, "backend": "claude", "cwd": r.dir.to_string_lossy(), "hqAgent": agent,
        "person": "sam", "permission": "default", "tasks": false, "connectors": [], "skills": [],
    }))
    .unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

/// True once `what` holds, checked for up to ten seconds.
async fn wait_for(what: impl Fn() -> bool) -> bool {
    for _ in 0..400 {
        if what() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// A claimed run that must be gone within a few seconds, ended by a signal (not one that simply exited).
async fn killed(child: &mut std::process::Child) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    for _ in 0..200 {
        if let Some(status) = child.try_wait().unwrap() {
            return status.signal().is_some();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

async fn shown(r: &Rig) -> crate::store::Card {
    for _ in 0..200 {
        if let Some(c) = r.hub.store.cards_open(None).into_iter().find(|c| c.state == crate::store::CardState::Live) {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no live card appeared");
}

async fn call(r: &Rig, who: &Caller, method: &str, b: Value) -> anyhow::Result<Value> {
    dispatch(&r.hub, who, method, b).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the built kith program: run by the host-build step (--run-ignored only)"]
async fn a_stepped_mac_with_the_switch_off_works_from_invite_to_rollback() {
    let mut w = World::new().await;
    let r = &w.r;

    // 1. Invitee access: with the switch OFF the guard still asks the core in-process (the tombstone selects it).
    let started = Instant::now();
    assert!(roles::may_use("sam", "house").await, "Sam holds house");
    let one = started.elapsed();
    assert!(!roles::may_use("sam", "money").await, "Sam holds no money");
    assert!(!roles::may_use("stranger", "house").await, "an unknown person holds nothing");
    let mine = sams_chat(r, "house");
    let money = sams_chat(r, "money");
    roles::guard(&r.hub, "sam", true, "history", &json!({"botId": mine})).await.expect("a member method on house");
    assert!(roles::guard(&r.hub, "sam", true, "history", &json!({"botId": money})).await.is_err(), "money is refused");
    assert!(call(r, &w.sam, "hello", json!({})).await.is_ok(), "Sam's phone");
    let unknown = Caller::Device { key: "nope".into(), scopes: vec![crate::store::Scope::Control] };
    assert!(call(r, &unknown, "hello", json!({})).await.is_err(), "an unknown device");
    assert!(call(r, &Caller::Local, "people", json!({})).await.is_ok(), "the owner");
    // R6: what one guest request costs with the switch off. The guard asks the core in-process (`people.may`, and now
    // `people.resolve` for the whole bundle); a launch reads it through a Python kith-auth that runs core-call.
    let timed = Instant::now();
    for _ in 0..5 {
        assert!(roles::may_use("sam", "house").await);
    }
    let guard_mean = timed.elapsed() / 5;
    let timed = Instant::now();
    for _ in 0..5 {
        assert!(roles::resolve("sam", "house").await.is_some_and(|v| v["allowed"] == true));
    }
    let bundle_mean = timed.elapsed() / 5;
    // And the path the design names, a Python kith-auth `resolve` that runs core-call (what a launch reads).
    let timed = Instant::now();
    for _ in 0..5 {
        assert_eq!(run_auth(r, &["resolve", "house", "--person", "sam"]).0, 0);
    }
    eprintln!(
        "R6, switch off: the host's guard (core asked in-process) first {one:?}, mean of 5 {guard_mean:?}; \
         the whole resolve bundle in-process, mean of 5 {bundle_mean:?}; \
         kith-auth resolve through core-call, mean of 5 {:?}",
        timed.elapsed() / 5
    );
    w.assert_off_and_tombstone();

    // 2. Invites: a new person, and the core holds her and her grant.
    let ann =
        call(r, &Caller::Local, "createInvite", json!({"name": "Ann", "agents": {"house": "guest"}})).await.unwrap();
    assert!(ann["inviteId"].is_string(), "{ann}");
    assert_eq!(w.core_rows("ann"), [("house".to_owned(), "guest".to_owned())], "the core holds ann and her grant");
    w.assert_off_and_tombstone();

    // 4. Revoke and undo, from the app.
    let before_rows = w.core_rows("sam");
    let before_ledger = blank(&run_auth(r, &["ledger", "--json"]).1);
    let state = r.hub.terminal.env().state;
    let mut run = claimed_run(r, "sess-sam", "sam");
    let mut owners = claimed_run(r, "sess-kai", "kai");
    let owed = crate::wake::Request {
        session: "sess-sam",
        id: "owed-1",
        agent: "house",
        person: Some("sam"),
        room: None,
        host_door: true,
        why: "a reply",
        text: "an answer",
    };
    crate::wake::enqueue(&state, &owed).unwrap();
    let out = call(r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out["revoked"], true, "{out}");
    assert!(!roles::may_use("sam", "house").await, "Sam is refused at once");
    assert!(roles::guard(&r.hub, "sam", true, "history", &json!({"botId": mine})).await.is_err());
    assert!(killed(&mut run).await, "his running turn is killed by a signal");
    assert!(owners.try_wait().unwrap().is_none(), "the owner's claim is never touched");
    assert_eq!(
        crate::wake::status(&state, "owed-1").as_deref(),
        Some("withdrawn"),
        "the wake he was owed is withdrawn"
    );
    assert!(call(r, &w.sam, "hello", json!({})).await.is_err(), "his phone is out");
    w.assert_off_and_tombstone();
    let out = call(r, &Caller::Local, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(out["revoked"], false, "{out}");
    assert_eq!(w.core_rows("sam"), before_rows, "exactly the rows he had");
    assert_eq!(blank(&run_auth(r, &["ledger", "--json"]).1), before_ledger, "and the same ledger");
    assert!(roles::may_use("sam", "house").await && call(r, &w.sam, "hello", json!({})).await.is_ok(), "Sam is back");
    // The core stopped: the owner is told plainly that the revoke is not complete, and Sam is cut off on this Mac from the
    // host's own record. The core coming back does not give him back; the owner's undo does.
    w.core.stop();
    let mut second = claimed_run(r, "sess-sam-2", "sam");
    let cut = call(r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap_err().to_string();
    assert!(cut.contains("cut off on this computer") && cut.contains("revoke again"), "{cut}");
    assert!(!cut.contains('/') && !cut.contains("people.") && !cut.contains("socket"), "plain words only: {cut}");
    assert!(!roles::may_use("sam", "house").await, "a core that is down grants nothing");
    assert!(killed(&mut second).await, "his turn was stopped without the core");
    assert!(call(r, &w.sam, "hello", json!({})).await.is_err(), "his phone is set aside");
    w.core.restart();
    assert_eq!(w.core_rows("sam"), before_rows, "nothing reached the core");
    assert!(call(r, &w.sam, "hello", json!({})).await.is_err(), "and the core coming back does not restore him");
    w.assert_off_and_tombstone();
    call(r, &Caller::Local, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    assert_eq!(w.core_rows("sam"), before_rows);
    assert!(roles::may_use("sam", "house").await && call(r, &w.sam, "hello", json!({})).await.is_ok(), "the undo does");
    let _ = owners.kill();
    let _ = owners.wait();

    // 5. Cards. An escape card, Always, is the core's access row.
    r.hub.store.kv_set("cards.live_window_s", "20").unwrap();
    let docs = std::fs::canonicalize(&r.dir).unwrap().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let docs = docs.to_string_lossy().into_owned();
    let who = guest("ann");
    let said = cards::access::request(
        &r.hub,
        &who,
        &json!({"kind": "path", "target": docs, "reason": "to read", "write": false}),
    )
    .await;
    assert_eq!(said["carded"], true, "{said}");
    let card = shown(r).await;
    cards::answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("always"), None).unwrap();
    // An edge card: a host (Always), and a helper once.
    let host = tokio::spawn({
        let hub = r.hub.clone();
        async move {
            cards::request_permission(&hub, &guest("ann"), "SandboxNetworkAccess", &json!({"host": "github.com"})).await
        }
    });
    let card = shown(r).await;
    cards::answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(edge::ALWAYS), None).unwrap();
    assert!(host.await.unwrap().allow);
    let helper = tokio::spawn({
        let hub = r.hub.clone();
        async move {
            cards::request_permission(&hub, &guest("ann"), edge::HELPER_TOOL, &json!({"agent": "money", "task": "x"}))
                .await
        }
    });
    let card = shown(r).await;
    cards::answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    assert!(helper.await.unwrap().allow);
    assert!(
        run_auth(r, &["may", "ann", "money", "--via", "house", "--once"]).0 == 0,
        "the helper's once is the core's"
    );
    // What reaches her next run: the core's rows through the host's own `consumeAccess`, not through kith-auth.
    let launch = RunIdentity { run_id: "run-ann-launch".into(), ..guest("ann") };
    // The card's write runs after its answer: wait (with a deadline) for the rows, never for a fixed time.
    assert!(
        wait_for(|| {
            let v: Value =
                serde_json::from_str(&run_auth(r, &["resolve", "house", "--person", "ann"]).1).unwrap_or_default();
            v["access"]["read"] == json!([docs]) && v["access"]["domains"] == json!(["github.com"])
        })
        .await,
        "the always read and the always host were written"
    );
    let took = cards::access::consume(&r.hub, &launch).await;
    assert_eq!(took["grants"]["read"], json!([docs]), "the always read reaches her run: {took}");
    assert_eq!(took["grants"]["domains"], json!(["github.com"]), "and the host: {took}");
    // A member's own in-grants card: Sam answers it, and his revoke withdraws it and takes his answer away.
    let note = tokio::spawn({
        let hub = r.hub.clone();
        async move { cards::request_permission(&hub, &guest("sam"), "WebSearch", &json!({"query": "weather"})).await }
    });
    let card = shown(r).await;
    assert_eq!(card.answerable_by.as_deref(), Some("sam"), "Sam is the approver of his own in-grants card");
    let entry = card.entry_id.clone().unwrap();
    roles::guard(&r.hub, "sam", true, "respondPermission", &json!({"entryId": entry})).await.expect("Sam may answer");
    call(r, &Caller::Local, "revokePerson", json!({"personId": "sam"})).await.unwrap();
    assert!(!note.await.unwrap().allow, "his card went with the revoke");
    assert!(roles::guard(&r.hub, "sam", true, "respondPermission", &json!({"entryId": entry})).await.is_err());
    // A run of his left alive asks no one: no card opens while he is set aside.
    let open_now = r.hub.store.cards_open(None).len();
    let late = cards::request_permission(&r.hub, &guest("sam"), "WebSearch", &json!({"query": "late"})).await;
    assert!(!late.allow && r.hub.store.cards_open(None).len() == open_now, "{late:?}");
    call(r, &Caller::Local, "unrevokePerson", json!({"personId": "sam"})).await.unwrap();
    // A revoked Ann's card goes with the revoke (the call that waited on it is told no), and her phone may not answer one.
    let open_before = r.hub.store.cards_open(None).len();
    let waiting = tokio::spawn({
        let hub = r.hub.clone();
        async move {
            cards::request_permission(&hub, &guest("ann"), "SandboxNetworkAccess", &json!({"host": "pypi.org"})).await
        }
    });
    let card = shown(r).await;
    call(r, &Caller::Local, "revokePerson", json!({"personId": "ann"})).await.unwrap();
    assert!(!waiting.await.unwrap().allow, "the card of a revoked person is not a yes");
    assert_eq!(r.hub.store.cards_open(None).len(), open_before, "her card is withdrawn");
    let refused =
        roles::guard(&r.hub, "ann", true, "respondPermission", &json!({"entryId": card.entry_id.unwrap()})).await;
    assert!(refused.is_err(), "a revoked person answers nothing");
    call(r, &Caller::Local, "unrevokePerson", json!({"personId": "ann"})).await.unwrap();
    w.assert_off_and_tombstone();

    // 6. Secrets. A key card: provide, list, revoke; then a room secret fails closed.
    let opened =
        call(r, &Caller::Local, "openKeyCard", json!({"service": "grok", "profile": "grok/home", "grant": "house"}))
            .await
            .unwrap();
    assert_eq!(opened["opened"], true, "{opened}");
    let entry = r
        .hub
        .store
        .cards_open(None)
        .into_iter()
        .find(|c| c.tool == cards::secret::TOOL)
        .and_then(|c| c.entry_id)
        .expect("the key card");
    let value = marker();
    let done = call(
        r,
        &Caller::Local,
        "provideSecret",
        json!({"entryId": entry, "account": "home", "values": {"GROK_WEBHOOK_URL": "https://example.invalid/home", "GROK_WEBHOOK_KEY": value}}),
    )
    .await
    .unwrap();
    assert_eq!(done["saved"]["profile"], "grok/home", "{done}");
    assert_eq!(w.keychain()["kith/grok/home#GROK_WEBHOOK_KEY"], json!(value), "the stand-in Keychain has the key");
    let keys = call(r, &Caller::Local, "listKeys", json!({})).await.unwrap().to_string();
    assert!(keys.contains("grok/home") && keys.contains("grok/work"), "{keys}");
    assert!(!keys.contains(&value), "the list carries no value");
    let gone =
        call(r, &Caller::Local, "revokeKey", json!({"agent": "house", "service": "grok", "profile": "grok/home"}))
            .await
            .unwrap();
    assert_eq!(gone, json!({"ok": true}));
    // A room secret: fails closed on a stepped Mac, and nothing is stored.
    let kept_before = w.keychain();
    let house = r.owner_bot("house");
    let room = r
        .hub
        .create_bot(
            serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": [house]})).unwrap(),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let secret = marker();
    let sent = call(r, &Caller::Local, "sendSecret", json!({"room": room, "to": house, "value": secret})).await;
    assert!(sent.is_err(), "a room secret is refused on a stepped Mac: {sent:?}");
    assert_eq!(w.keychain(), kept_before, "and nothing was stored");
    assert!(!w.keychain().to_string().contains(&secret));
    w.assert_off_and_tombstone();
    let threes = w.assert_exit_3s();
    assert!(threes.iter().any(|v| v == "once"), "the room secret's exit 3 was seen: {threes:?}");

    // 7. Rollback: the real one. The core's own reads (the daemon gone, so embedded) are the file's after it.
    w.core.stop();
    let at_rest = w.reads();
    assert!(at_rest.lines().count() > 40, "{at_rest}");
    let out = std::process::Command::new(&w.kith)
        .args(["rollback", "authority"])
        .env("KITH_KERNEL", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel"))
        .envs(TEST_ENV.lock().unwrap().iter().map(|(k, v)| (*k, v)))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!super::authority::switched_now(), "the file is a file again");
    let file: Value = serde_json::from_slice(&std::fs::read(&w.auth_file).unwrap()).unwrap();
    assert!(file.get("kith_tombstone").is_none(), "{file}");
    let text = file.to_string();
    for needle in ["ann", "sam", "grok/work"] {
        assert!(text.contains(needle), "the restored auth.json lacks {needle}: {text}");
    }
    assert!(text.contains("keychain:kith/grok/work#GROK_WEBHOOK_KEY"), "the secret reference is back: {text}");
    assert!(text.contains(&docs), "the access row is back: {text}");
    assert!(w.keychain()["kith/grok/work#GROK_WEBHOOK_KEY"].is_string(), "the Keychain item is still there");
    assert_eq!(w.reads(), at_rest, "the file answers what the core answered");
    w.assert_exit_3s();
}

// ---- pending until the P1 package (core routes for link, owner, share, browser, the terminal suspend) lands ----

#[tokio::test]
#[ignore = "pending P1 (rust-migrate-blockers-routes): linkCode and /link route to the core"]
async fn pending_links_bind_a_telegram_handle_once_and_refuse_the_rest() {
    // linkCode as the owner and as Sam's device: a 6-character code each, listed by people.invites for the right person.
    // hq::link("telegram", uid, code) binds Sam; `who telegram <uid>` (routed) says sam. Refused, with the file's words:
    // the same code twice; a revoked person's code; a uid that is already another person's. With the core stopped: one
    // generic line and nothing written.
    unimplemented!("P1");
}

#[tokio::test]
#[ignore = "pending P1 (rust-migrate-blockers-routes): the host reconciles a terminal people.suspend"]
async fn pending_a_terminal_suspend_is_set_aside_on_the_host_within_one_tick() {
    // `kith kernel core-call people.suspend sam`, then one poll tick sets him aside on the host; a failed core write
    // during a revoke (core stopped) is cut fail-closed and a tick does not restore it.
    unimplemented!("P1");
}

#[tokio::test]
#[ignore = "pending P1 (rust-migrate-blockers-routes): kith-auth owner, share and browser write through the core"]
async fn pending_share_lends_a_profile_to_sam_and_rm_takes_it_away() {
    // `kith-auth share` lends a profile to Sam (his resolve sees it); `--rm` takes it away; both survive the rollback.
    unimplemented!("P1");
}
