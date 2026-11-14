//! Writer mode on the real actor (`writer.rs`, P7b): native bots whose agent is a fixture (`tests/fixtures/writer_agent.py`) run
//! under a REAL `kith-stub --resident`, against the scripted core of `terminal/core_writer_tests.rs`. Each test sends a message
//! through the host's own API and reads what the core was asked and what the chat shows. The rules of the core's rows are tested on
//! a real core in `core/runtime`; here is the host's side: the answer table of design 1.3, one begin at a time, the end reports, a
//! process that goes, and the modes (the rig expresses "switched, core down" and "not switched, core up" separately: the TEST_CORE
//! lesson of #973).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::assert_is_empty,
    clippy::unnecessary_wraps,
    clippy::explicit_iter_loop,
    clippy::doc_markdown
)]

use super::super::Cmd;
use crate::api::devices::Caller;
use crate::hub::Hub;
use crate::store::{BotConfig, Lane, Store};
use crate::terminal::core_claims::tests::{Fake, register};
use crate::terminal::core_writer::tests::{C, World};
use kith_kernel::paths::Paths;
use serde_json::json;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The stub the actor spawns its processes under (the real one, built once).
pub(super) fn stub_bin() -> Option<PathBuf> {
    Some(crate::terminal::core_writer::tests::stub_bin())
}

fn folders_registry() -> &'static Mutex<std::collections::HashMap<PathBuf, Paths>> {
    static R: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, Paths>>> = std::sync::OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// The core folders a test registered for a state folder (`writer::core` asks here first).
pub(super) fn folders_for(state: &std::path::Path) -> Option<Paths> {
    folders_registry().lock().unwrap().get(state).cloned()
}

/// How the Mac stands: the module's switch file, and whether a core answers.
#[derive(Clone, Copy)]
struct Mac {
    /// `claims.switch` exists (the module was switched).
    file: bool,
    /// The gate says `ok` (a core that answers and agrees).
    ok: bool,
}

const SWITCHED: Mac = Mac { file: true, ok: true };

/// What the scripted core does once, after it answers a method (a test's way to change the world between two calls of one begin).
type Hooks = Arc<Mutex<Vec<(&'static str, Box<dyn FnOnce() + Send>)>>>;

struct Rig {
    hub: Arc<Hub>,
    dir: PathBuf,
    world: Arc<Mutex<World>>,
    locks: PathBuf,
    held: Vec<Child>,
    hooks: Hooks,
}

fn folders(dir: &std::path::Path) -> Paths {
    let state = dir.join("state");
    Paths {
        home: dir.to_path_buf(),
        state: state.clone(),
        agents: dir.join("agents"),
        home_real: dir.to_path_buf(),
        state_real: state,
        agents_real: dir.join("agents"),
        machine: dir.join("machine"),
        secrets: dir.join("secrets"),
        logs: dir.join("logs"),
        cache: dir.join("cache"),
        store: dir.join("store"),
    }
}

impl Rig {
    /// A host with one native bot per id (their sessions are `sess-<id>`), on a Mac that is `mac`.
    fn new(bots: &[&str], mac: Mac) -> Self {
        Self::new_as(bots, mac, None)
    }

    /// [`Self::new`] with the bots running as `person` (a room person's actor, say).
    fn new_as(bots: &[&str], mac: Mac, person: Option<&str>) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kw-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let f = folders(&dir);
        if mac.file {
            let switch = f.machine.join("core").join("cutover");
            std::fs::create_dir_all(&switch).unwrap();
            std::fs::write(switch.join("claims.switch"), "").unwrap();
        }
        let world =
            Arc::new(Mutex::new(World { account: "fixture".into(), switched: Some(mac.ok), ..World::default() }));
        let w = world.clone();
        let hooks: Hooks = Hooks::default();
        let h = hooks.clone();
        let fake: Fake = Arc::new(move |m, p| {
            let answer = w.lock().unwrap().answer(m, p);
            // Outside the world's lock: a hook may read the world.
            let due = {
                let mut all = h.lock().unwrap();
                let at = all.iter().position(|(on, _)| *on == m);
                at.map(|i| all.remove(i).1)
            };
            if let Some(hook) = due {
                hook();
            }
            answer
        });
        register(&f.state, fake);
        // The core this host asks (Shadow's `enable` is not it: that would shadow every file claim).
        folders_registry().lock().unwrap().insert(f.state.clone(), f.clone());
        let store = Store::open(&dir.join("test.db")).unwrap();
        let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/writer_agent.py");
        for id in bots {
            let cwd = dir.join(id);
            std::fs::create_dir_all(&cwd).unwrap();
            let cfg: BotConfig = serde_json::from_value(json!({
                "id": id, "name": id, "backend": "fixture", "cwd": cwd, "tasks": false,
                "command": crate::shell::python(&agent), "notify": false, "person": person,
            }))
            .unwrap();
            store.save_bot(&cfg).unwrap();
        }
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = Hub::new(store, "writer-test".into(), identity, "test-token".into(), 19223);
        hub.terminal.set_env(crate::terminal::Env { state: f.state.clone(), sessions: None });
        hub.start().unwrap();
        Self { hub, locks: f.machine.join("core").join("locks"), dir, world, held: Vec::new(), hooks }
    }

    fn w(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn send(&self, bot: &str, text: &str) {
        crate::api::dispatch(&self.hub, &Caller::Local, "send", json!({"botId": bot, "text": text})).await.unwrap();
    }

    /// A message into the thread whose root is `root` (an entry of the main chat).
    async fn send_thread(&self, bot: &str, root: &str, text: &str) {
        crate::api::dispatch(&self.hub, &Caller::Local, "send", json!({"botId": bot, "text": text, "threadId": root}))
            .await
            .unwrap();
    }

    /// The id of the first message in the bot's main chat (a thread's root).
    fn first_message(&self, bot: &str) -> String {
        let all = self.hub.store.history(bot, i64::MAX, 300).unwrap();
        all.into_iter().find(|e| e.kind == "user").map(|e| e.id).expect("a message")
    }

    /// Runs `f` once, right after the core answers `method` (and outside its lock).
    fn after(&self, method: &'static str, f: impl FnOnce() + Send + 'static) {
        self.hooks.lock().unwrap().push((method, Box::new(f)));
    }

    /// The sessions the fixture agent of `bot` was asked to load, in order.
    fn loads(&self, bot: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(bot).join("loads.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<String>(l).ok())
            .collect()
    }

    fn texts(&self, bot: &str, kind: &str) -> Vec<String> {
        self.hub
            .store
            .history(bot, i64::MAX, 300)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kind)
            .filter_map(|e| e.data["text"].as_str().map(str::to_owned))
            .collect()
    }

    /// Whether the agent answered `text` in the thread `root`.
    fn replied_in(&self, bot: &str, root: &str, text: &str) -> bool {
        let entries = self.hub.store.thread(bot, root, 500).unwrap();
        entries.iter().any(|e| e.kind == "agent" && e.data["text"].as_str().is_some_and(|t| t.contains(text)))
    }

    fn replied(&self, bot: &str, text: &str) -> bool {
        self.texts(bot, "agent").iter().any(|t| t.contains(text))
    }

    fn noticed(&self, bot: &str, text: &str) -> bool {
        self.texts(bot, "notice").iter().any(|t| t.contains(text))
    }

    fn calls(&self, method: &str) -> usize {
        self.w().log.iter().filter(|m| *m == method).count()
    }

    fn turn_state(&self, id: i64) -> String {
        self.w().turns.iter().find(|t| t.0 == id).map(|t| t.1.clone()).unwrap_or_default()
    }

    fn live_claims(&self) -> usize {
        self.w().claims.iter().filter(|c| ["claimed", "running", "ending"].contains(&c.state.as_str())).count()
    }

    /// Waits for `ok`, and checks that never more than one claim of a conversation is live while it waits.
    async fn until(&self, what: &str, mut ok: impl FnMut(&Self) -> bool) {
        let by = Instant::now() + Duration::from_secs(40);
        while !ok(self) {
            assert!(Instant::now() < by, "timed out waiting for {what}; the core was asked: {:?}", self.w().log);
            let live = |c: &str| {
                self.w()
                    .claims
                    .iter()
                    .filter(|x| x.conv == c && ["claimed", "running", "ending"].contains(&x.state.as_str()))
                    .count()
            };
            for c in ["sess-a", "sess-b"] {
                assert!(live(c) <= 1, "two live claims on {c}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A claim of someone else on `conv`: a terminal's (`external`) or another host instance's turn.
    fn seed(&self, conv: &str, kind: &str, instance: Option<&str>) {
        let mut w = self.w();
        let id = i64::try_from(w.claims.len()).unwrap() + 900;
        w.claims.push(C {
            id,
            conv: conv.into(),
            state: "running".into(),
            kind: kind.into(),
            pid: Some(1),
            started: None,
            instance: instance.map(str::to_owned),
            turn: None,
        });
    }

    fn end_seeded(&self) {
        for c in self.w().claims.iter_mut().filter(|c| c.id >= 900) {
            c.state = "ended".into();
        }
    }

    /// A `--direct` run holding the writer lock of `key` until it is dropped or ended.
    fn direct(&mut self, key: &str) {
        std::fs::create_dir_all(&self.locks).unwrap();
        for _ in 0..20 {
            let mut child = Command::new(stub_bin().unwrap())
                .arg("--lock-dir")
                .arg(&self.locks)
                .args(["--key", key, "--", "cat"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(300));
            if child.try_wait().unwrap().is_none() {
                self.held.push(child);
                return;
            }
        }
        panic!("the one-shot stub never took the lock");
    }

    fn end_direct(&mut self) {
        for mut c in self.held.drain(..) {
            drop(c.stdin.take());
            let _ = c.wait();
        }
    }

    /// The stub's pid the claim of `conv` is redeemed on.
    fn running_pid(&self, conv: &str) -> Option<i32> {
        self.w()
            .claims
            .iter()
            .find(|c| c.conv == conv && c.state == "running")
            .and_then(|c| c.pid)
            .and_then(|p| i32::try_from(p).ok())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.end_direct();
        for c in self.w().claims.iter() {
            if let Some(pid) = c.pid.and_then(|p| i32::try_from(p).ok()).filter(|p| *p > 1) {
                // SAFETY: a signal to a stub group this test started.
                unsafe { libc::killpg(pid, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn kill_group(pid: i32) {
    // SAFETY: a signal to the stub's process group, which this test's actor started.
    unsafe { libc::killpg(pid, libc::SIGKILL) };
}

macro_rules! actor_test {
    ($(#[$m:meta])* async fn $name:ident($rig:ident) $body:block) => {
        $(#[$m])*
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            #[allow(unused_mut)]
            let mut $rig = test_rig();
            $body
        }
    };
}

fn test_rig() -> Rig {
    Rig::new(&["a"], SWITCHED)
}

// MARK: the answer table (design 1.3 step 3)

actor_test! {
    /// claimed: the turn runs under the lock, and the core hears each step in order. The epoch is read before the enqueue, the account
    /// sent is the process's own, a native bot claims as `bot:<id>`, and the process has its `warm_procs` row.
    async fn a_claimed_turn_runs_and_reports_done(rig) {
        rig.send("a", "hello").await;
        rig.until("the reply", |r| r.replied("a", "hello")).await;
        rig.until("the end report", |r| r.w().reports.iter().any(|(k, _)| k == "done")).await;
        let w = rig.w();
        let at = |m: &str| w.log.iter().position(|x| x == m).unwrap_or_else(|| panic!("no {m} in {:?}", w.log));
        assert!(at("hostclaim.epoch") < at("hostclaim.enqueue"), "the epoch is read before the turn is made");
        assert!(at("hostclaim.enqueue") < at("hostclaim.turn") && at("hostclaim.turn") < at("hostclaim.redeem"));
        assert!(at("hostclaim.redeem") < at("hostclaim.report"));
        let enqueue = &w.asked.iter().find(|(m, _)| m == "hostclaim.enqueue").unwrap().1;
        assert_eq!(enqueue["agent"], "bot:a", "a native bot claims as bot:<id> (B10)");
        assert!(enqueue["key"].as_str().is_some_and(|k| !k.is_empty()), "every enqueue carries its key");
        assert_eq!(w.sent_accounts, [Some("fixture".to_owned())], "the process's own account, not a fixed one");
        assert_eq!(w.turns[0].1, "done");
        assert_eq!(w.claims[0].state, "ended");
        assert!(!w.procs.is_empty(), "the warm process is known to the core");
    }
}

actor_test! {
    /// held, the writer is a terminal: the turn parks (`terminal_live`), the agent does not run, and when the terminal ends the
    /// core is asked to unpark and the SAME turn is claimed (the enqueue is not repeated).
    async fn a_terminal_claim_parks_the_turn_until_it_ends(rig) {
        rig.seed("sess-a", "external", None);
        rig.send("a", "park me").await;
        rig.until("the park", |r| r.turn_state(1) == "parked").await;
        assert_eq!(rig.w().turns[0].2.as_deref(), Some("terminal_live"));
        assert!(rig.hub.runtime("a").activity.contains("waiting for your terminal session"));
        assert!(rig.texts("a", "agent").is_empty(), "nothing runs under a terminal's claim");
        rig.end_seeded();
        rig.until("the reply", |r| r.replied("a", "park me")).await;
        assert_eq!(rig.calls("hostclaim.enqueue"), 1, "the retried begin is the same turn");
        assert!(rig.calls("hostclaim.unpark") >= 1);
        assert_eq!(rig.turn_state(1), "done");
    }
}

actor_test! {
    /// held by another turn's claim: stays queued, begins again on the tick, runs when it is gone.
    async fn another_turns_claim_keeps_the_turn_queued(rig) {
        rig.seed("sess-a", "turn", Some("kith-serve-host/other-1"));
        rig.send("a", "after you").await;
        rig.until("two tries", |r| r.calls("hostclaim.turn") >= 2).await;
        assert!(rig.texts("a", "agent").is_empty());
        assert_eq!(rig.turn_state(1), "queued", "not parked: the core keeps it queued behind the claim");
        rig.end_seeded();
        rig.until("the reply", |r| r.replied("a", "after you")).await;
        assert_eq!(rig.calls("hostclaim.enqueue"), 1);
    }
}

actor_test! {
    /// waiting (the core reconciles): the same turn is tried again.
    async fn a_reconcile_in_progress_is_waited_out(rig) {
        rig.w().turn_refusal = Some("waiting");
        rig.send("a", "soon").await;
        rig.until("two tries", |r| r.calls("hostclaim.turn") >= 2).await;
        assert!(rig.texts("a", "agent").is_empty());
        rig.w().turn_refusal = None;
        rig.until("the reply", |r| r.replied("a", "soon")).await;
        assert_eq!(rig.calls("hostclaim.enqueue"), 1);
    }
}

actor_test! {
    /// notWaiting and stopped: the item is dropped and never tried again.
    async fn a_turn_the_core_no_longer_waits_for_is_dropped(rig) {
        rig.w().turn_refusal = Some("notWaiting");
        rig.send("a", "gone").await;
        rig.until("the drop", |r| r.noticed("a", "Stopped.")).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(rig.calls("hostclaim.turn"), 1, "no second try");
        assert!(rig.texts("a", "agent").is_empty());
    }
}

actor_test! {
    async fn a_turn_the_core_stopped_is_dropped(rig) {
        rig.w().turn_refusal = Some("stopped");
        rig.send("a", "gone").await;
        rig.until("the drop", |r| r.noticed("a", "Stopped.")).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(rig.calls("hostclaim.turn"), 1);
        assert!(rig.texts("a", "agent").is_empty());
    }
}

actor_test! {
    /// revoked, at the claim (the turn is cancelled) and at the enqueue (nothing was made): "Access was revoked.", never run.
    async fn a_revoked_person_is_refused_and_the_turn_cancelled(rig) {
        rig.w().turn_refusal = Some("revoked");
        rig.send("a", "no").await;
        rig.until("the notice", |r| r.noticed("a", "Access was revoked.")).await;
        assert_eq!(rig.turn_state(1), "cancelled");
        assert!(rig.texts("a", "agent").is_empty());
        rig.w().turn_refusal = None;
        rig.w().revoke_enqueue = true;
        rig.send("a", "no again").await;
        rig.until("the second notice", |r| r.texts("a", "notice").iter().filter(|t| t.contains("Access was revoked.")).count() == 2).await;
        assert_eq!(rig.w().turns.len(), 1, "a refused enqueue makes no turn");
    }
}

actor_test! {
    /// exhausted: its text is posted, the item fails, the turn is cancelled.
    async fn no_account_left_posts_its_text_and_fails_the_item(rig) {
        rig.w().exhausted = true;
        rig.send("a", "nothing").await;
        rig.until("the text", |r| r.noticed("a", "no account left")).await;
        assert_eq!(rig.turn_state(1), "cancelled");
        assert!(rig.texts("a", "agent").is_empty());
    }
}

actor_test! {
    /// busy: a `--direct` run holds the lock. The claim ends `busy`, the turn parks `lock_busy`, and it runs when the lock is free.
    async fn a_busy_lock_parks_the_turn_until_the_direct_run_ends(rig) {
        rig.direct("sess-a");
        rig.send("a", "behind direct").await;
        rig.until("the park", |r| r.turn_state(1) == "parked").await;
        assert_eq!(rig.w().turns[0].2.as_deref(), Some("lock_busy"));
        assert!(rig.w().reports.iter().any(|(k, _)| k == "busy"));
        assert!(rig.texts("a", "agent").is_empty());
        rig.end_direct();
        rig.until("the reply", |r| r.replied("a", "behind direct")).await;
        assert_eq!(rig.calls("hostclaim.enqueue"), 1);
    }
}

actor_test! {
    /// redeem refused (the claim ended meanwhile): the lock goes back, the turn is cancelled, nothing runs.
    async fn a_refused_redeem_gives_the_lock_back(rig) {
        rig.w().redeem_refused = true;
        rig.send("a", "late").await;
        rig.until("the drop", |r| r.noticed("a", "Stopped.")).await;
        assert_eq!(rig.turn_state(1), "cancelled");
        assert!(rig.texts("a", "agent").is_empty());
        rig.w().redeem_refused = false;
        // The lock is free: a `--direct` run takes it at once.
        rig.direct("sess-a");
    }
}

actor_test! {
    /// A redeem refused with a lock that cannot be given back is `LockStuck`: the claim is stopped, the process killed, the start fails.
    async fn a_lock_that_cannot_be_given_back_kills_the_process(rig) {
        rig.w().kill_on_redeem = true;
        rig.send("a", "stuck").await;
        rig.until("the failure", |r| r.noticed("a", "session lock could not be released")).await;
        assert!(rig.calls("hostclaim.stop") >= 1, "the claim is stopped");
        assert!(rig.texts("a", "agent").is_empty());
    }
}

// MARK: a process the core marks for a kill (the kill_at gap of P7a)

actor_test! {
    /// The core refuses a redeem on a process marked for a kill (`warm_procs.kill_at`, see `claims::redeem`): the lock goes back, the
    /// claim is stopped with the turn put back (not cancelled), and the same turn claims again once the process is gone.
    async fn a_process_marked_for_a_kill_takes_no_turn_and_the_turn_goes_on(rig) {
        rig.w().redeem_killing = 1;
        rig.send("a", "second try").await;
        rig.until("the reply", |r| r.replied("a", "second try")).await;
        let w = rig.w();
        assert_eq!(w.turns.len(), 1, "the same turn, never made twice");
        assert_eq!(w.claims.len(), 2, "claimed again after the refusal");
        assert_eq!(w.claims[0].state, "ended");
        assert!(w.asked.iter().any(|(m, p)| m == "hostclaim.stop" && p["requeue"] == true), "put back, not cancelled");
    }
}

// MARK: the modes

actor_test! {
    /// Switched, and the core does not answer: `Held`. The turn waits with the notice, nothing runs, no claim is asked for. When the
    /// core is back it runs.
    async fn a_switched_mac_whose_core_is_down_waits_and_never_runs_unclaimed(rig) {
        rig.w().down = true;
        rig.send("a", "hold").await;
        rig.until("the notice", |r| r.noticed("a", "core not answering")).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(rig.texts("a", "agent").is_empty(), "never runs unclaimed");
        assert_eq!(rig.texts("a", "notice").iter().filter(|t| t.contains("core not answering")).count(), 1, "said once");
        rig.w().down = false;
        rig.until("the reply", |r| r.replied("a", "hold")).await;
        assert_eq!(rig.turn_state(1), "done");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_that_cannot_be_read_holds_too() {
    let rig = Rig::new(&["a"], SWITCHED);
    rig.w().gate_warning = true;
    rig.send("a", "stepping").await;
    rig.until("the notice", |r| r.noticed("a", "core not answering")).await;
    assert!(rig.w().log.iter().all(|m| m == "cutover.mayServe"), "only the gate is asked: {:?}", rig.w().log);
    rig.w().gate_warning = false;
    rig.until("the reply", |r| r.replied("a", "stepping")).await;
}

/// Not switched and a core that answers: Files, today's path. The core is asked the gate and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unswitched_mac_with_a_core_keeps_the_files_path() {
    let rig = Rig::new(&["a"], Mac { file: false, ok: false });
    rig.send("a", "plain").await;
    rig.until("the reply", |r| r.replied("a", "plain")).await;
    let w = rig.w();
    assert!(w.log.iter().all(|m| m == "cutover.mayServe"), "no hostclaim row is called: {:?}", w.log);
    assert!(w.procs.is_empty() && w.turns.is_empty(), "no claim, no process row");
}

// MARK: one begin at a time, two conversations

actor_test! {
    /// A second message while the first runs waits behind it: one begin per conversation, one live claim at a time.
    async fn one_begin_per_conversation_at_a_time(rig) {
        rig.send("a", "SLOW one").await;
        rig.until("the first claim", |r| r.live_claims() == 1).await;
        rig.send("a", "two").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(rig.calls("hostclaim.turn"), 1, "the second turn does not begin while the first holds the conversation");
        rig.until("both replies", |r| r.replied("a", "one") && r.replied("a", "two")).await;
        assert_eq!(rig.calls("hostclaim.turn"), 2);
        assert_eq!(rig.w().turns.iter().filter(|t| t.1 == "done").count(), 2);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_bots_get_two_conversations_and_run_side_by_side() {
    // B2: every session id is the engine's own (`session/new`), one per actor, so two bots never share a conversation and need not
    // wait for each other.
    let rig = Rig::new(&["a", "b"], SWITCHED);
    rig.send("a", "SLOW from a").await;
    rig.send("b", "SLOW from b").await;
    rig.until("both claimed at once", |r| r.live_claims() == 2).await;
    rig.until("both replies", |r| r.replied("a", "from a") && r.replied("b", "from b")).await;
    let convs: std::collections::BTreeSet<String> = rig.w().claims.iter().map(|c| c.conv.clone()).collect();
    assert_eq!(convs.len(), 2, "two conversations: {convs:?}");
}

// MARK: ends

actor_test! {
    /// A user's Stop is `session/cancel` and the normal end: `done`, never `exit` (only the 15 s kill reports it).
    async fn a_stop_ends_with_done_not_exit(rig) {
        rig.send("a", "HANG").await;
        rig.until("running", |r| r.running_pid("sess-a").is_some()).await;
        rig.hub.send_to_actor("a", Cmd::Stop).unwrap();
        rig.until("the end report", |r| !r.w().reports.is_empty()).await;
        let w = rig.w();
        assert_eq!(w.reports.len(), 1);
        assert_eq!(w.reports[0].0, "done", "{:?}", w.reports);
        assert_eq!(w.turns[0].1, "done");
    }
}

actor_test! {
    /// The process is killed under a running turn (the core's revoke reached a sibling session of the same process, or the idle-kill
    /// before a terminal claim): the end is `exit`, not `done`, and the turn it puts back is cancelled after: nothing in the host
    /// runs a requeued core turn, so it must not be left queued.
    async fn a_process_killed_under_a_turn_ends_exit_and_the_turn_is_cancelled_not_left_queued(rig) {
        rig.send("a", "HANG").await;
        rig.until("running", |r| r.running_pid("sess-a").is_some()).await;
        kill_group(rig.running_pid("sess-a").unwrap());
        rig.until("the end report", |r| !r.w().reports.is_empty()).await;
        {
            let w = rig.w();
            assert_eq!(w.reports[0].0, "exit", "{:?}", w.reports);
        }
        rig.until("the turn is cancelled", |r| r.turn_state(1) == "cancelled").await;
    }
}

actor_test! {
    /// An idle warm process that is killed (the revoke of its person, swept by the core) is forgotten by the core when it is gone, and
    /// the next message starts a new one that is registered afresh.
    async fn an_idle_process_that_is_killed_is_forgotten_and_the_next_turn_starts_another(rig) {
        rig.send("a", "first").await;
        rig.until("done", |r| r.w().reports.iter().any(|(k, _)| k == "done")).await;
        // One row for the process, one for its session: the same pid.
        let pids = |r: &Rig| -> std::collections::BTreeSet<u64> { r.w().procs.iter().copied().collect() };
        let first = pids(&rig);
        assert_eq!(first.len(), 1);
        kill_group(i32::try_from(*first.iter().next().unwrap()).unwrap());
        rig.until("forgotten", |r| r.w().procs.is_empty()).await;
        rig.send("a", "second").await;
        rig.until("the reply", |r| r.replied("a", "second")).await;
        let second = pids(&rig);
        assert_eq!(second.len(), 1);
        assert_ne!(first, second, "a new process");
    }
}

actor_test! {
    /// The agent wakes itself (an off-turn turn): the host begins for it, and a begin that does not give it the session stops the
    /// process, because the engine is already writing.
    async fn an_agent_that_wakes_itself_without_a_claim_is_stopped(rig) {
        rig.send("a", "WAKE").await;
        rig.until("the first turn", |r| r.w().turns.first().is_some_and(|t| t.1 == "done")).await;
        let pid = rig.w().claims[0].pid.and_then(|p| i32::try_from(p).ok()).unwrap();
        // Another writer takes the conversation before the agent speaks again.
        rig.seed("sess-a", "external", None);
        rig.until("the begin for the wake", |r| r.calls("hostclaim.turn") >= 2).await;
        rig.until("the process is stopped", |_| {
            // SAFETY: signal 0 only asks whether the group is still there.
            unsafe { libc::killpg(pid, 0) != 0 }
        })
        .await;
    }
}

actor_test! {
    /// The 15 s kill: a cancelled turn whose agent ignores the cancel is killed, and that end (only that one) reports `exit`; the turn
    /// it puts back is cancelled, not left queued.
    async fn the_fifteen_second_kill_reports_exit_and_cancels_the_turn(rig) {
        rig.send("a", "STUBBORN").await;
        rig.until("running", |r| r.running_pid("sess-a").is_some()).await;
        rig.hub.send_to_actor("a", Cmd::Stop).unwrap();
        rig.until("the end report", |r| !r.w().reports.is_empty()).await;
        let w = rig.w();
        assert_eq!(w.reports[0].0, "exit", "{:?}", w.reports);
        assert_eq!(w.turns[0].1, "cancelled");
    }
}

actor_test! {
    /// A stub that dies between the claim and its take is restarted under the begin: the claim is reported `exit`, the process comes
    /// back (a new connection, registered again, its session loaded again) and the SAME turn claims again and runs.
    async fn a_stub_that_dies_before_its_take_is_restarted_and_the_turn_runs(rig) {
        rig.send("a", "warm up").await;
        rig.until("done", |r| r.w().reports.iter().any(|(k, _)| k == "done")).await;
        rig.w().kill_procs_on_turn = 1;
        rig.send("a", "after the restart").await;
        rig.until("the reply", |r| r.replied("a", "after the restart")).await;
        let w = rig.w();
        assert!(w.reports.iter().any(|(k, _)| k == "exit"), "the first claim was reported: {:?}", w.reports);
        assert_eq!(w.turns.len(), 2, "no turn made twice");
        assert_eq!(w.turns[1].1, "done");
    }
}

/// A room-level turn (the actor of a room person, `room-<id>`) ends with `stop room-turn-end` and the process group killed, and the
/// core forgets the process once it is gone (no warm room process).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_room_turn_ends_with_a_stop_and_no_warm_process() {
    let rig = Rig::new_as(&["a"], SWITCHED, Some("room-r-1"));
    rig.send("a", "room words").await;
    rig.until("the reply", |r| r.replied("a", "room words")).await;
    rig.until("the stop", |r| r.w().asked.iter().any(|(m, p)| m == "hostclaim.stop" && p["reason"] == "room-turn-end"))
        .await;
    rig.until("the process forgotten", |r| r.w().procs.is_empty()).await;
}

actor_test! {
    /// A reload that fails after `Running` (the restarted process's session row is refused) gives the claim up instead of leaving it in
    /// `writer.run`: the turn is cancelled, nothing is prompted, and the next message is a turn of its own.
    async fn a_reload_that_fails_after_running_gives_the_claim_up(rig) {
        rig.send("a", "warm up").await;
        rig.until("done", |r| r.turn_state(1) == "done").await;
        rig.w().kill_procs_on_turn = 1;
        rig.w().session_register_refused = true;
        rig.send("a", "doomed").await;
        rig.until("the turn is cancelled", |r| r.turn_state(2) == "cancelled").await;
        assert!(rig.texts("a", "agent").iter().all(|t| !t.contains("doomed")), "never prompted");
        rig.w().session_register_refused = false;
        rig.send("a", "after").await;
        rig.until("the reply", |r| r.replied("a", "after")).await;
        let w = rig.w();
        assert_eq!(w.turns.len(), 3, "the next message is a turn of its own: {:?}", w.turns);
        assert_eq!(w.turns[2].1, "done");
    }
}

// MARK: smaller rules

/// MUST 8: the host's count decides; the chain opens with the conversation's own budget, the default when it is not known.
#[test]
fn a_room_turn_opens_its_chain_with_the_hosts_own_cap() {
    assert_eq!(super::budget(), crate::chat::group::budget_of(None));
    assert_eq!(super::budget(), crate::chat::group::DEFAULT_BUDGET);
}

/// Architect nit: a thread lane with no session key is not on the turn's session either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_lane_with_no_session_key_is_not_the_turns_session() {
    let rig = Rig::new(&["a"], SWITCHED);
    let lane = Lane::in_thread("a", "root-1");
    let got = crate::terminal::sync_for_turn(&rig.hub, &lane, "sess-a").await.unwrap();
    assert_eq!(got, crate::terminal::Synced::Forked { session: String::new() });
    // A main lane has no thread to follow.
    assert_eq!(
        crate::terminal::sync_for_turn(&rig.hub, &Lane::main("a"), "sess-a").await.unwrap(),
        crate::terminal::Synced::Unchanged
    );
}

// MARK: step 7 (design 1.3): the transcript sync under the lock

actor_test! {
    /// A terminal wrote since the session loaded (the thread's stale mark is set between the claim and the sync): the turn holds its
    /// claim, the session is loaded again before the prompt, and the turn runs on the same claim.
    async fn a_thread_that_moved_reloads_its_session_under_the_claim(rig) {
        rig.send("a", "first").await;
        rig.until("the first reply", |r| r.replied("a", "first")).await;
        let root = rig.first_message("a");
        rig.send_thread("a", &root, "in thread").await;
        rig.until("the thread's reply", |r| r.replied_in("a", &root, "in thread")).await;
        rig.until("its end", |r| r.w().turns.iter().filter(|t| t.1 == "done").count() == 2).await;
        let before = rig.loads("a").len();
        let hub = rig.hub.clone();
        let stale = format!("terminal.stale.a/{root}");
        rig.after("hostclaim.redeem", move || hub.store.kv_set(&stale, "1").unwrap());
        rig.send_thread("a", &root, "after the terminal").await;
        rig.until("the reply", |r| r.replied_in("a", &root, "after the terminal")).await;
        assert_eq!(rig.loads("a").len(), before + 1, "the session was loaded again before the prompt: {:?}", rig.loads("a"));
        let w = rig.w();
        assert_eq!(w.turns.len(), 3, "the same turn, no new one");
        assert_eq!(w.turns[2].1, "done");
        assert_eq!(w.claims.len(), 3, "one claim for the turn");
    }
}

actor_test! {
    /// A terminal forked the thread's session between the claim and the sync: the claim on the old session is given up (the turn
    /// cancelled), and the turn begins again, as a new turn, on the session the lane follows now.
    async fn a_thread_that_forked_abandons_the_claim_and_begins_on_the_new_session(rig) {
        rig.send("a", "first").await;
        rig.until("the first reply", |r| r.replied("a", "first")).await;
        let root = rig.first_message("a");
        rig.send_thread("a", &root, "in thread").await;
        rig.until("the thread's reply", |r| r.replied_in("a", &root, "in thread")).await;
        rig.until("its end", |r| r.w().turns.iter().filter(|t| t.1 == "done").count() == 2).await;
        let hub = rig.hub.clone();
        let key = crate::store::lane_key("session", "a", &Lane::in_thread("a", &root));
        rig.after("hostclaim.redeem", move || hub.store.kv_set(&key, "sess-fork").unwrap());
        rig.send_thread("a", &root, "after the fork").await;
        rig.until("the reply", |r| r.replied_in("a", &root, "after the fork")).await;
        assert!(rig.loads("a").contains(&"sess-fork".to_owned()), "the new session loaded: {:?}", rig.loads("a"));
        let w = rig.w();
        assert_eq!(w.turns[2].1, "cancelled", "the turn on the old session is cancelled: {:?}", w.turns);
        assert_eq!(w.turns[3].1, "done", "a new turn on the new session");
        let keys: Vec<_> = w.keys.iter().filter(|k| k.2 == 3 || k.2 == 4).collect();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0].1, keys[1].1, "a new key");
        assert_eq!(keys[0].0, "sess-a");
        assert_eq!(keys[1].0, "sess-fork", "on the session the lane follows now");
    }
}

// MARK: off-turn: any error stops the process (check-in MUST 6)

actor_test! {
    /// An error before the begin (the core stops answering after the first turn, so the revoke epochs can't be read) is not a begin
    /// refusal, and the engine is writing all the same: the process is stopped.
    async fn an_off_turn_that_fails_before_its_begin_stops_the_process(rig) {
        rig.send("a", "WAKE").await;
        rig.until("the first turn", |r| r.w().turns.first().is_some_and(|t| t.1 == "done")).await;
        let pid = rig.w().claims[0].pid.and_then(|p| i32::try_from(p).ok()).unwrap();
        rig.w().down = true;
        rig.until("the process is stopped", |_| {
            // SAFETY: signal 0 only asks whether the group is still there.
            unsafe { libc::killpg(pid, 0) != 0 }
        })
        .await;
    }
}

actor_test! {
    /// An off-turn turn's engine is already writing the session: a sync that finds it moved (here a fork) cannot be dealt with by
    /// loading another session, so the process is stopped (check-in MUST 6, the architect's r2 MUST 2).
    async fn an_off_turn_that_finds_the_thread_forked_stops_the_process(rig) {
        rig.send("a", "first").await;
        rig.until("the first reply", |r| r.replied("a", "first")).await;
        let root = rig.first_message("a");
        rig.send_thread("a", &root, "in thread").await;
        rig.until("the thread's reply", |r| r.replied_in("a", &root, "in thread")).await;
        rig.until("its end", |r| r.w().turns.iter().filter(|t| t.1 == "done").count() == 2).await;
        let pid = rig.w().claims[1].pid.and_then(|p| i32::try_from(p).ok()).unwrap();
        // The thread's next turn wakes the agent by itself half a second after it answers; the fork happens at the wake's redeem.
        let (hooks, hub) = (rig.hooks.clone(), rig.hub.clone());
        let key = crate::store::lane_key("session", "a", &Lane::in_thread("a", &root));
        rig.after("hostclaim.report", move || {
            let fork: Box<dyn FnOnce() + Send> = Box::new(move || hub.store.kv_set(&key, "sess-fork").unwrap());
            hooks.lock().unwrap().push(("hostclaim.redeem", fork));
        });
        rig.send_thread("a", &root, "WAKE").await;
        rig.until("the process is stopped", |_| {
            // SAFETY: signal 0 only asks whether the group is still there.
            unsafe { libc::killpg(pid, 0) != 0 }
        })
        .await;
    }
}

// MARK: parked behind a --direct run: the waits grow

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_parked_behind_a_direct_run_asks_less_and_less_often() {
    let mut rig = Rig::new(&["a"], SWITCHED);
    rig.direct("sess-a");
    rig.send("a", "behind direct").await;
    // The times the core was asked to claim the turn: the first, then one after each wait (2 s, then 4 s, ...).
    let mut at: Vec<Instant> = Vec::new();
    let by = Instant::now() + Duration::from_secs(40);
    while at.len() < 3 {
        assert!(Instant::now() < by, "timed out; asked {} times: {:?}", at.len(), rig.w().log);
        if rig.calls("hostclaim.turn") > at.len() {
            at.push(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let (first, second) = (at[1] - at[0], at[2] - at[1]);
    assert!(first >= Duration::from_millis(1800), "the first wait is the slow tick: {first:?}");
    assert!(
        second >= first + Duration::from_secs(1),
        "the second wait is longer, it was reset every time before: {first:?} then {second:?}"
    );
    assert_eq!(rig.calls("hostclaim.enqueue"), 1, "the same turn each time");
    rig.end_direct();
    rig.until("the reply", |r| r.replied("a", "behind direct")).await;
}

// MARK: the end choice, the backoff and the chain (units)

mod units {
    use crate::agent::bot::writer::{Rel, budget, chain, chain_opened, chain_root};
    use crate::agent::bot::writer_turn::{cancel_reason, end_for};
    use crate::agent::bot::writer_wait::lock_backoff;
    use crate::terminal::core_writer::End;
    use std::path::Path;
    use std::time::Duration;

    /// What a release reports. `OutOfUsage` only for a move to the next account, which `failover` sets after its refusals (the turn
    /// acted, was sent, is hidden, is at MAX_MOVES or AllOut: each returns before it); everything else ends `Done`.
    #[test]
    fn the_end_is_out_of_usage_only_for_a_move_and_every_other_ending_is_done() {
        assert_eq!(end_for(Some(Rel::OutOfUsage), false, false), End::OutOfUsage);
        assert_eq!(end_for(None, false, false), End::Done, "no failover: done");
        assert_eq!(end_for(Some(Rel::Abandon), false, false), End::Done);
        assert_eq!(
            end_for(Some(Rel::Died), false, false),
            End::Done,
            "a failed prompt on a live process is an ordinary end"
        );
        assert_eq!(end_for(Some(Rel::Died), true, false), End::Died, "the process is gone");
        assert_eq!(end_for(Some(Rel::Killed), false, false), End::Killed);
        assert_eq!(end_for(Some(Rel::Left), false, false), End::Left, "a fork leaves the claim, the process lives on");
        assert_eq!(end_for(None, false, true), End::Room, "a room person's turn");
        assert_eq!(end_for(Some(Rel::OutOfUsage), false, true), End::OutOfUsage);
    }

    /// An end that reports `exit` cancels its turn after, because nothing in the host runs a requeued core turn.
    #[test]
    fn every_end_that_reports_exit_cancels_its_turn() {
        assert!(cancel_reason(End::Killed).is_some());
        assert!(cancel_reason(End::Died).is_some());
        assert!(cancel_reason(End::OutOfUsage).is_some());
        assert!(cancel_reason(End::Left).is_some());
        assert!(cancel_reason(End::Done).is_none());
        assert!(cancel_reason(End::Room).is_none());
    }

    #[test]
    fn a_turn_parked_behind_a_direct_run_waits_longer_each_time_up_to_a_cap() {
        let waits: Vec<_> = (0..8).map(|n| lock_backoff(n).as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 30, 30, 30, 30]);
        assert_eq!(lock_backoff(u32::MAX), Duration::from_secs(30), "no overflow");
    }

    #[test]
    fn a_later_round_follows_the_first_rounds_turn_and_the_first_opens_the_chain() {
        let state = Path::new("kw-chain-test");
        assert_eq!(chain_root(state, "conv-1"), None);
        assert_eq!(
            chain(None, 40),
            ("budget", 40),
            "the first round opens the chain with the conversation's own budget"
        );
        assert_eq!(chain(None, budget()), ("budget", budget()));
        chain_opened(state, "conv-1", 7);
        chain_opened(state, "conv-1", 9);
        assert_eq!(chain_root(state, "conv-1"), Some(7), "the first stays");
        assert_eq!(
            chain(chain_root(state, "conv-1"), 40),
            ("budgetFrom", 7),
            "a later round follows, whatever its budget"
        );
        assert_eq!(chain_root(state, "conv-2"), None, "another conversation opens its own chain");
        assert_eq!(chain_root(Path::new("kw-other"), "conv-1"), None, "another host's state");
    }
}
