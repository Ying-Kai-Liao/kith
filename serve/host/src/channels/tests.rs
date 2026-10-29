//! The core against a fake platform, a stub hq (`tests/fixtures/channels_stub.py`) and a fake Jev.

#![cfg(unix)]

use super::adapter::{Adapter, Ended, Reaction, StopPress};
use super::fake::{Event, Fake};
use super::judge::Judge;
use super::manager::{Backoff, Manager, Registry, supervise};
use super::settings::Settings;
use super::text::INTERRUPTED;
use super::{Core, Incoming, Secrets, core};
use crate::LockExt;
use crate::agent::gate::{Gate, Settings as GateSettings, memory};
use crate::chat::router::Jev;
use axum::{Json, extract::State, routing::post};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

mod backup;

const STUB: &str = include_str!("../../tests/fixtures/channels_stub.py");

/// The stub script, written once so that no test writes an executable that another is about to start.
fn stub_script() -> &'static Path {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kith-channels-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("channels_stub.py");
        std::fs::write(&path, STUB).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    })
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A fake Jev: scores by keywords in the latest message. Returns it and the requests it got.
async fn fake_jev() -> (Jev, Arc<Mutex<Vec<Value>>>) {
    #[allow(clippy::unused_async)] // axum handlers are async
    async fn handler(State(seen): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>) -> Json<Value> {
        let latest = body["state"]["latest"].as_str().unwrap_or("").to_owned();
        seen.locked().push(body.clone());
        let score = |k: &str| -> f64 {
            match k {
                "stop" if latest.contains("STOPNOW") => 0.97,
                "change" if latest.contains("CORRECT") => 0.96,
                "other" if latest.contains("OTHER") => 0.95,
                "partial" if latest.contains("PARTIAL") => 0.9,
                _ => 0.05,
            }
        };
        let answers: serde_json::Map<String, Value> =
            body["questions"].as_object().unwrap().keys().map(|k| (k.clone(), json!({"noul": score(k)}))).collect();
        Json(json!({"answers": answers}))
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new().route("/v1/systemone", post(handler)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (Jev::at("jev-secret-key".into(), &base, "jev-test".into()), seen)
}

struct Rig {
    dir: PathBuf,
    core: Arc<Core>,
    fake: Arc<Fake>,
    ad: Arc<dyn Adapter>,
    backups: Arc<crate::agent::backups::Backups>,
    /// Keeps the gate's task alive, when a test needs slots handed on.
    _hub: Option<Arc<crate::hub::Hub>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Opts {
    settings: Settings,
    jev: Option<Jev>,
    gate: GateSettings,
    /// Runs the gate's task (a released slot reaches the next turn in line through it).
    gate_task: bool,
}

impl Opts {
    fn new() -> Self {
        let settings = Settings {
            debounce: Duration::from_millis(100),
            hold: Duration::from_millis(400),
            poll: Duration::from_millis(40),
            sync_hold: Duration::from_secs(5),
            sync_poll: Duration::from_millis(50),
            ..Settings::default()
        };
        Self { settings, jev: None, gate: GateSettings::unlimited(), gate_task: false }
    }
}

impl Rig {
    fn new() -> Self {
        Self::with(Opts::new())
    }

    fn with(opts: Opts) -> Self {
        let dir = std::env::temp_dir().join(format!("kith-channels-{}", uuid::Uuid::new_v4()));
        let (bin, stub) = (dir.join("bin"), dir.join("stub"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&stub).unwrap();
        for tool in ["kith", "kith-auth"] {
            std::os::unix::fs::symlink(stub_script(), bin.join(tool)).unwrap();
        }
        for (name, card) in [
            ("house", "description: Keeps the house\ndisplay_name: Home\n"),
            ("personal", "description: Personal\n"),
            ("shop", "description: Shopping\n"),
            ("coder", "description: Codes\n"),
        ] {
            std::fs::create_dir_all(dir.join("agents").join(name)).unwrap();
            std::fs::write(dir.join("agents").join(name).join("agent.md"), card).unwrap();
        }
        let paths = super::hq::Paths {
            hq_bin: bin.join("kith"),
            auth_bin: bin.join("kith-auth"),
            machine: dir.join("machine"),
            agent_roots: vec![dir.join("agents")],
            data: dir.join("data"),
        };
        let rig_files = [
            (
                "people",
                json!({"fake:1": {"person": "kai", "owner": true, "was": []},
                              "fake:2": {"person": "guest", "owner": false, "was": []}}),
            ),
            ("access", json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": ["shop"]}})),
            ("codes", json!({"GOODCODE1": "newbie"})),
        ];
        for (name, v) in rig_files {
            std::fs::write(stub.join(format!("{name}.json")), v.to_string()).unwrap();
        }
        let gate = Gate::new(opts.gate, Box::new(memory::System));
        let hub = opts.gate_task.then(|| {
            let store = crate::store::Store::open(Path::new(":memory:")).unwrap();
            let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
            let hub =
                crate::hub::Hub::new_with_gate(store, "channels-test".into(), identity, "test".into(), 0, gate.clone());
            tokio::spawn(crate::agent::gate::run(gate.clone(), Arc::downgrade(&hub)));
            hub
        });
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let backups = Arc::new(crate::agent::backups::Backups::new(dir.join("data/backups.json")));
        let core = Core::new(core::Deps {
            paths: paths.clone(),
            settings: opts.settings,
            gate,
            judge: Judge::fixed(opts.jev, paths),
            backups: backups.clone(),
        });
        let fake = Arc::new(Fake::new("fake/x"));
        let ad: Arc<dyn Adapter> = fake.clone();
        Self { dir, core, fake, ad, backups, _hub: hub }
    }

    fn put(&self, name: &str, v: &Value) {
        std::fs::write(self.dir.join("stub").join(format!("{name}.json")), v.to_string()).unwrap();
    }

    fn calls(&self, tool: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("stub/calls.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|c| c["tool"] == tool).collect()
    }

    fn msg(uid: &str, text: &str) -> Incoming {
        Incoming {
            uid: uid.into(),
            text: text.into(),
            room: "fake/x:room".into(),
            origin: json!({"msg": text}),
            addressed: true,
            ..Incoming::default()
        }
    }

    fn in_thread(mut m: Incoming, key: &str) -> Incoming {
        m.thread = Some(key.into());
        m
    }

    async fn send(&self, m: &Incoming) {
        self.core.handle(&self.ad, m).await.unwrap();
    }

    /// Waits until the turn has finished and nothing is left to run.
    async fn idle(&self) {
        let core = self.core.clone();
        until("the queue to empty", move || core.with(|i| i.busy.is_empty())).await;
    }

    fn inbox_empty(&self, wk: &str) -> bool {
        self.core.with(|i| i.st.inbox.get(wk).is_none_or(Vec::is_empty))
    }
}

fn echo(task: &str) -> String {
    format!("echo:{task}\n\nFOOTER")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_sender_is_told_to_link() {
    let r = Rig::new();
    let mut m = Rig::msg("77", "hello");
    m.private = true;
    r.send(&m).await;
    assert!(r.fake.replies()[0].contains("/link CODE"));
    // Talk in a room that isn't for hq stays unanswered.
    let mut chatter = Rig::msg("77", "lunch?");
    chatter.addressed = false;
    r.send(&chatter).await;
    assert_eq!(r.fake.replies().len(), 1);
    assert!(r.calls("kith").is_empty(), "nothing ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_with_a_code_links_the_account() {
    let r = Rig::new();
    r.send(&Rig::msg("55", "/link BADCODE1")).await;
    assert_eq!(r.fake.replies()[0], "that code did not work");
    r.send(&Rig::msg("55", "/link GOODCODE1")).await;
    assert_eq!(r.fake.replies()[1], "Linked: you are newbie in kith.");
    let people: Value =
        serde_json::from_str(&std::fs::read_to_string(r.dir.join("stub/people.json")).unwrap()).unwrap();
    assert_eq!(people["fake:55"]["person"], "newbie");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_agent_picks_the_agent_and_the_thread_resumes_its_session() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house hello")).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.fake.posts()[0], ("house".into(), echo("hello")));
    assert!(r.fake.events().contains(&Event::OpenThread { agent: "house".into(), title: "hello".into() }));
    let first = &r.calls("kith")[0];
    assert!(first["resume"].is_null() && first["person"].is_null());
    assert_eq!(
        (first["via"].as_str(), first["output"].as_str(), first["extra"].as_str()),
        (Some("fake"), Some("json"), Some("1"))
    );
    assert_eq!(first["withheld"], json!([]));
    assert_eq!(first["outbox"], true);

    // The next message in the thread goes to the agent that answered last, in its session.
    let key = "fake/x:room:1";
    r.send(&Rig::in_thread(Rig::msg("1", "and more"), key)).await;
    until("the second answer", || r.fake.posts().len() == 2).await;
    let calls = r.calls("kith");
    assert_eq!((calls[1]["agent"].as_str(), calls[1]["resume"].as_str()), (Some("house"), Some("sess-house-1")));
    assert_eq!(r.fake.posts()[1].1, echo("and more"));

    // A mention anywhere picks another agent, which answers in its own session of the same thread.
    r.send(&Rig::in_thread(Rig::msg("1", "ask @shop about milk"), key)).await;
    until("the shop's answer", || r.fake.posts().len() == 3).await;
    let calls = r.calls("kith");
    assert_eq!((calls[2]["agent"].as_str(), calls[2]["resume"].is_null()), (Some("shop"), true));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_cards_wake_is_its_own_turn_and_a_refusal_leaves_nothing_behind() {
    use super::Wake;
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house hello")).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    r.idle().await;
    let wk = "fake/x:room:1#house";
    assert_eq!(
        r.core.wake(wk, "never", || vec!["x".into()]),
        Wake::Offline,
        "no connection yet: the resume stays owed"
    );
    r.core.resume(&r.ad);
    assert_eq!(r.core.wake("fake/x:gone#house", "never", || vec!["x".into()]), Wake::NoThread);
    assert_eq!(r.core.wake(wk, "never", Vec::new), Wake::Claimed, "someone else took the card's resume");
    assert!(r.inbox_empty(wk));
    assert_eq!(r.core.wake(wk, "[Kith] Kai allowed ls once; go on.", || vec!["card-0".into()]), Wake::Queued);
    until("the wake's turn", || r.fake.posts().len() == 2).await;
    assert_eq!(r.fake.posts()[1].1, echo("[Kith] Kai allowed ls once; go on."));
    r.idle().await;
    assert!(r.core.wakes.locked().is_empty(), "a wake whose turn ran is settled");

    // A turn that never started (the kernel refused a held session) hands the cards back, once.
    r.core.wakes.locked().push(("[Kith] Kai allowed ls once; go on.".into(), vec!["card-1".into()]));
    assert_eq!(r.core.settle_wake("the same words: [Kith] Kai allowed ls once; go on.", true).len(), 0);
    r.core.wakes.locked().push(("[Kith] Kai allowed ls once; go on.".into(), vec!["card-1".into()]));
    assert_eq!(r.core.settle_wake("[Kith] Kai allowed ls once; go on.", false), ["card-1"]);
    assert_eq!(r.core.settle_wake("[Kith] Kai allowed ls once; go on.", false).len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_room_default_and_the_default_command() {
    let r = Rig::new();
    let mut chat = Rig::msg("1", "hi there");
    chat.addressed = false;
    r.send(&chat).await;
    assert!(r.fake.events().is_empty(), "no default and not addressed: silent");

    r.send(&Rig::msg("1", "/default")).await;
    assert!(r.fake.replies()[0].starts_with("No default here"));
    r.send(&Rig::msg("1", "/default nobody")).await;
    assert_eq!(r.fake.replies()[1], "No agent of yours called 'nobody'. /agents lists them.");
    r.send(&Rig::msg("2", "/default house")).await; // the guest may not use house
    assert!(r.fake.replies()[2].starts_with("No agent of yours"));
    r.send(&Rig::msg("1", "/default house")).await;
    assert_eq!(r.fake.replies()[3], "Messages without @agent here now go to house.");

    r.send(&chat).await;
    until("the default agent to answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.calls("kith")[0]["agent"], "house");
    r.idle().await;

    r.send(&Rig::msg("1", "/default none")).await;
    assert_eq!(r.fake.replies()[4], "No default: I'll ask who should take each message.");
    r.send(&Rig::msg("1", "/whoami")).await;
    assert_eq!(
        r.fake.replies()[5],
        "You are kai in kith. Write @agent to pick one; /agents lists yours, /default sets this room's."
    );
    r.send(&Rig::msg("2", "/agents")).await;
    assert_eq!(r.fake.replies()[6], "Yours: shop\nWrite @agent, or answer in a thread to go on with it.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_new_dm_goes_to_the_dm_default() {
    let r = Rig::new();
    let mut dm = Rig::msg("1", "good morning");
    dm.room = "fake/x:dm".into();
    dm.private = true;
    dm.addressed = false;
    r.send(&dm).await;
    until("the DM answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.calls("kith")[0]["agent"], "personal");
    r.idle().await;

    // Someone else's new DM has no default: the bot asks.
    let mut theirs = Rig::msg("2", "hello");
    theirs.room = "fake/x:dm2".into();
    theirs.private = true;
    r.send(&theirs).await;
    let asked = r.fake.events().into_iter().find_map(|e| match e {
        Event::Ask { agents, .. } => Some(agents),
        _ => None,
    });
    assert_eq!(asked, Some(vec!["shop".to_owned()]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_told_message_goes_to_the_persons_newest_one_to_one_thread_with_the_agent_only() {
    let r = Rig::new();
    r.put(
        "people",
        &json!({"fake:1": {"person": "kai", "owner": true, "was": []},
                "fake:2": {"person": "guest", "owner": false, "was": []},
                "kith:-": {"person": null, "owner": false, "was": [], "owner_id": "kai"}}),
    );
    let dm = |uid: &str, room: &str, text: &str| {
        let mut m = Rig::msg(uid, text);
        m.room = room.into();
        m.private = true;
        m
    };
    r.send(&dm("1", "fake/x:dm", "@house hi")).await;
    until("the owner's answer", || r.fake.posts().len() == 1).await;
    r.idle().await;
    r.send(&dm("2", "fake/x:dm2", "@shop hi")).await;
    until("the guest's answer", || r.fake.posts().len() == 2).await;
    r.idle().await;
    r.send(&Rig::msg("2", "@shop milk please")).await;
    until("the guest's room answer", || r.fake.posts().len() == 3).await;
    r.idle().await;
    let told = |r: &Rig| {
        r.fake.events().into_iter().filter_map(|e| match e {
            Event::Post { thread, agent, text, .. } if text.starts_with("Added") => Some((thread, agent, text)),
            _ => None,
        })
    };

    assert!(!r.core.tell(None, "house", "Added: tidy up").await, "no connection is up yet");
    r.core.resume(&r.ad);
    assert!(r.core.tell(None, "house", "Added: tidy up").await, "None is the owner");
    assert!(r.core.tell(Some("guest"), "shop", "Added: milk").await);
    assert_eq!(
        told(&r).collect::<Vec<_>>(),
        [
            ("fake/x:dm:1".into(), "house".into(), "Added: tidy up".into()),
            ("fake/x:dm2:2".into(), "shop".into(), "Added: milk".into()),
        ],
        "the guest's newer thread in the room is not theirs alone"
    );
    assert!(!r.core.tell(Some("guest"), "house", "Added: x").await, "no thread with that agent");
    assert!(!r.core.tell(Some("nobody"), "shop", "Added: x").await);
    r.put("access", &json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": []}}));
    assert!(!r.core.tell(Some("guest"), "shop", "Added: x").await, "an agent they may no longer use says nothing");
    assert_eq!(told(&r).count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn who_should_take_this_then_a_button_picks() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "do the thing")).await;
    let (pid, agents) = r
        .fake
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::Ask { pid, agents } => Some((pid, agents)),
            _ => None,
        })
        .expect("asked");
    assert_eq!(agents, ["coder", "house", "personal", "shop"]);
    assert!(r.calls("kith").is_empty(), "never guesses");

    // Another person's press, and an unknown question, are refused.
    assert!(!r.core.picked(&r.ad, &pid, "house", "2").await.unwrap());
    assert!(!r.core.picked(&r.ad, "nope", "house", "1").await.unwrap());
    assert!(r.core.picked(&r.ad, &pid, "house", "1").await.unwrap());
    until("the picked agent to answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.fake.posts()[0], ("house".into(), echo("do the thing")));
    assert!(!r.core.picked(&r.ad, &pid, "house", "1").await.unwrap(), "a question is answered once");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_owner_runs_as_themselves_and_stays_out_of_others_threads() {
    let r = Rig::new();
    r.send(&Rig::msg("2", "@shop milk please")).await;
    until("the guest's answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.calls("kith")[0]["person"], "guest");
    r.idle().await;

    r.send(&Rig::msg("2", "@house hi")).await;
    assert_eq!(
        r.fake.replies()[0],
        "guest may not use house. Ask the owner to add you (kith auth member house guest)."
    );

    r.send(&Rig::msg("1", "@house owner task")).await;
    until("the owner's answer", || r.fake.posts().len() == 2).await;
    r.idle().await;
    let calls = r.calls("kith");
    assert!(calls[1]["person"].is_null());
    r.send(&Rig::in_thread(Rig::msg("2", "@shop can I join"), "fake/x:room:2")).await;
    assert_eq!(r.fake.replies()[1], "This thread is kai's. Start your own outside it.");
    assert_eq!(r.calls("kith").len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_that_wait_for_a_turn_become_one_prompt() {
    let mut opts = Opts::new();
    // The stub starts slowly on a busy machine: leave room between the two messages.
    opts.settings.debounce = Duration::from_secs(2);
    let r = Rig::with(opts);
    r.send(&Rig::msg("1", "@house one")).await;
    r.send(&Rig::in_thread(Rig::msg("1", "two"), "fake/x:room:1")).await;
    until("the merged answer", || !r.fake.posts().is_empty()).await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 1);
    let task = calls[0]["task"].as_str().unwrap();
    assert!(task.contains("answer them together") && task.contains("[1] one") && task.contains("[2] two"), "{task}");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_sent_while_it_works_run_together_next() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house SLEEP a")).await;
    until("the first run", || !r.calls("kith").is_empty()).await;
    let key = "fake/x:room:1";
    r.send(&Rig::in_thread(Rig::msg("1", "b"), key)).await;
    r.send(&Rig::in_thread(Rig::msg("1", "c"), key)).await;
    assert!(r.fake.events().contains(&Event::StatusText("working… · 2 queued".into())), "{:?}", r.fake.events());
    until("both answers", || r.fake.posts().len() == 2).await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 2);
    let second = calls[1]["task"].as_str().unwrap();
    assert!(second.contains("[1] b") && second.contains("[2] c"), "{second}");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jev_stop_ends_the_run() {
    let (jev, seen) = fake_jev().await;
    let r = Rig::with(Opts { jev: Some(jev), ..Opts::new() });
    r.send(&Rig::msg("1", "@house SLOW work")).await;
    until("the run to start", || !r.calls("kith").is_empty()).await;
    r.send(&Rig::in_thread(Rig::msg("1", "STOPNOW please"), "fake/x:room:1")).await;
    r.idle().await;
    let events = r.fake.events();
    assert!(events.contains(&Event::React(Reaction::Stop)));
    assert!(events.contains(&Event::StatusEnd(Ended::Stopped)));
    assert!(r.fake.posts().is_empty(), "a stopped run posts nothing");
    assert!(r.inbox_empty("fake/x:room:1#house"));
    assert!(seen.locked()[0]["questions"].as_object().unwrap().contains_key("partial"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jev_interrupt_restarts_the_work_with_the_correction() {
    let (jev, _) = fake_jev().await;
    let r = Rig::with(Opts { jev: Some(jev), ..Opts::new() });
    r.send(&Rig::msg("1", "@house SLOW first")).await;
    until("the run to start", || !r.calls("kith").is_empty()).await;
    r.send(&Rig::in_thread(Rig::msg("1", "CORRECT it"), "fake/x:room:1")).await;
    until("the redo answer", || !r.fake.posts().is_empty()).await;
    r.idle().await;
    let events = r.fake.events();
    assert!(events.contains(&Event::React(Reaction::Redo)));
    assert!(events.contains(&Event::StatusText("restarting with your correction…".into())));
    assert!(events.contains(&Event::StatusEnd(Ended::Interrupted)));
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 2);
    let redo = calls[1]["task"].as_str().unwrap();
    assert!(redo.starts_with(INTERRUPTED), "{redo}");
    assert!(redo.contains("[1] SLOW first") && redo.contains("[2] CORRECT it"), "{redo}");
    assert_eq!(r.fake.posts().len(), 1, "only the corrected run answers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jev_separate_gives_a_different_subject_its_own_turn() {
    let (jev, _) = fake_jev().await;
    let r = Rig::with(Opts { jev: Some(jev), ..Opts::new() });
    r.send(&Rig::msg("1", "@house SLEEP first")).await;
    until("the run to start", || !r.calls("kith").is_empty()).await;
    r.send(&Rig::in_thread(Rig::msg("1", "OTHER subject"), "fake/x:room:1")).await;
    until("both answers", || r.fake.posts().len() == 2).await;
    let calls = r.calls("kith");
    assert_eq!(calls[1]["task"], "OTHER subject\n\nFOOTER");
    assert!(calls[0]["task"].as_str().unwrap().starts_with("SLEEP first"));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_that_ends_mid_thought_waits_for_the_rest() {
    let (jev, seen) = fake_jev().await;
    let mut opts = Opts { jev: Some(jev), ..Opts::new() };
    opts.settings.debounce = Duration::from_millis(1500);
    opts.settings.hold = Duration::from_secs(3);
    let r = Rig::with(opts);
    r.send(&Rig::msg("1", "@house first")).await;
    r.send(&Rig::in_thread(Rig::msg("1", "PARTIAL half a thought"), "fake/x:room:1")).await;
    // Past the debounce, inside the hold: still part of the same turn.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(r.calls("kith").is_empty(), "held for the rest");
    r.send(&Rig::in_thread(Rig::msg("1", "the rest"), "fake/x:room:1")).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 1);
    let task = calls[0]["task"].as_str().unwrap();
    assert!(
        task.contains("[1] first") && task.contains("[2] PARTIAL half a thought") && task.contains("[3] the rest"),
        "{task}"
    );
    assert!(!seen.locked().is_empty());
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_and_the_stop_button() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house SLOW work")).await;
    let key = "fake/x:room:1";
    until("the run to start", || !r.calls("kith").is_empty()).await;
    let wid = r
        .fake
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::Status { wid } => Some(wid),
            _ => None,
        })
        .unwrap();
    assert_eq!(r.core.stop_pressed(&r.ad, &wid, "2").await, StopPress::NotYours);
    assert_eq!(r.core.stop_pressed(&r.ad, "unknown", "1").await, StopPress::NotYours);
    assert_eq!(r.core.stop_pressed(&r.ad, &wid, "1").await, StopPress::Stopping);
    // Pressed twice: still stopping, or already over.
    assert!(matches!(r.core.stop_pressed(&r.ad, &wid, "1").await, StopPress::Stopping | StopPress::Finished));
    r.idle().await;
    assert!(r.fake.events().contains(&Event::StatusEnd(Ended::Stopped)));
    assert_eq!(r.core.stop_pressed(&r.ad, &wid, "1").await, StopPress::Finished);

    // /stop in a thread with nothing running, and with a run going.
    r.send(&Rig::in_thread(Rig::msg("1", "/stop"), key)).await;
    assert_eq!(r.fake.replies()[0], "Nothing is running here.");
    r.send(&Rig::in_thread(Rig::msg("1", "SLOW again"), key)).await;
    until("the second run", || r.calls("kith").len() == 2).await;
    r.send(&Rig::in_thread(Rig::msg("1", "/stop"), key)).await;
    r.idle().await;
    assert!(r.fake.events().contains(&Event::React(Reaction::Stop)));
    assert!(r.fake.posts().is_empty(), "stopped runs post nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_waits_for_the_gate_and_can_be_stopped_while_it_waits() {
    let gate = GateSettings { max_running: 1, ..GateSettings::unlimited() };
    let r = Rig::with(Opts { gate, gate_task: true, ..Opts::new() });
    let mut held = r.core.gate.enter("test:holder");
    let permit = held.granted().await;
    r.send(&Rig::msg("1", "@house waits")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(r.calls("kith").is_empty(), "no slot, no run");
    drop(permit);
    until("the run once there is room", || !r.fake.posts().is_empty()).await;
    r.idle().await;

    let permit = r.core.gate.enter("test:holder").granted().await;
    r.send(&Rig::in_thread(Rig::msg("1", "waits again"), "fake/x:room:1")).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    r.send(&Rig::in_thread(Rig::msg("1", "/stop"), "fake/x:room:1")).await;
    r.idle().await;
    drop(permit);
    assert_eq!(r.calls("kith").len(), 1, "stopped before it started");
    assert!(r.fake.events().contains(&Event::StatusEnd(Ended::Stopped)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_agent_sends_with_hq_send_is_posted_in_order() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house SEND things")).await;
    until("the answer", || r.fake.posts().len() == 2).await;
    let events = r.fake.events();
    assert!(
        matches!(&events[2], Event::Post { agent, text, .. } if agent == "image" && text == "tool says hi"),
        "{events:?}"
    );
    assert!(
        matches!(&events[3], Event::Files { agent, files, caption } if agent == "image" && files == &["/tmp/pic.png"] && caption == "a picture")
    );
    assert_eq!(r.fake.posts()[1].0, "house");
    r.idle().await;
    let outboxes = std::fs::read_dir(r.dir.join("data/outbox")).map_or(0, Iterator::count);
    assert_eq!(outboxes, 0, "the run's outbox is removed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_messages_say_so_and_big_contexts_are_shown() {
    let r = Rig::new();
    let mut late = Rig::msg("1", "@house BIGCTX hello");
    late.sent = Some(core::now() - 300.0);
    late.quoted = "an earlier note".into();
    late.files = vec!["/tmp/a.png".into()];
    r.send(&late).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    let task = r.calls("kith")[0]["task"].as_str().unwrap().to_owned();
    assert!(task.starts_with("(Sent at "), "{task}");
    assert!(task.contains("picked it up late") && task.contains("(Replying to: \"an earlier note\")"));
    assert!(task.contains("BIGCTX hello") && task.contains("Attached files: /tmp/a.png"), "{task}");
    let ctx = r.fake.events().into_iter().find_map(|e| match e {
        Event::Post { ctx, .. } => ctx,
        _ => None,
    });
    assert!((ctx.unwrap() - 60.0).abs() < 1e-6);
    r.idle().await;

    let mut fresh = Rig::msg("1", "@house just now");
    fresh.sent = Some(core::now() - 5.0);
    r.send(&fresh).await;
    until("the second answer", || r.fake.posts().len() == 2).await;
    assert!(!r.calls("kith")[1]["task"].as_str().unwrap().contains("picked it up late"));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_run_says_so() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house FAIL please")).await;
    until("the failure", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.fake.posts()[0], ("house".into(), "⚠️ house failed:\n```\nboom\n\n```".into()));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_messages_at_once_in_one_thread_are_both_delivered() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "@house start")).await;
    r.idle().await;
    let key = "fake/x:room:1";
    let first = Rig::in_thread(Rig::msg("1", "alpha"), key);
    let second = Rig::in_thread(Rig::msg("1", "beta"), key);
    let (done_first, done_second) = tokio::join!(r.core.handle(&r.ad, &first), r.core.handle(&r.ad, &second));
    done_first.unwrap();
    done_second.unwrap();
    r.idle().await;
    let tasks: String = r.calls("kith").iter().skip(1).map(|c| c["task"].as_str().unwrap().to_owned()).collect();
    assert!(tasks.contains("alpha") && tasks.contains("beta"), "{tasks}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connections_state_comes_over_from_the_bridge_and_only_that_one() {
    let r = Rig::new();
    let bridge = json!({
        "version": 2,
        "conns": {"fake/a": {"offset": 5}, "fake/b": {"offset": 6}},
        "rooms": {"fake/a:room": {"default": "house"}, "fake/b:room": {"default": "shop"}},
        "threads": {
            "fake/a:room:9": {"person": "kai", "owner": "house", "last": "house", "room": "fake/a:room",
                              "sessions": {"house": "old-session"}, "conv": {"key": "fake/a:room:9"}},
            "fake/b:room:3": {"person": "kai", "last": "shop", "sessions": {"shop": "other"}}
        },
        "replies": {}, "inbox": {}, "pending": {}
    });
    let file = r.dir.join("machine/bridge/state.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, bridge.to_string()).unwrap();

    let imported = r.core.import_bridge("fake/a").unwrap();
    assert_eq!((imported.rooms, imported.threads), (1, 1));
    assert!(r.core.import_bridge("fake/a").is_none(), "only the first time");
    assert_eq!(r.core.conn_state("fake/a")["offset"], 5);
    assert_eq!(r.core.conn_state("fake/b"), json!({}));
    r.core.with(|i| assert!(!i.st.threads.contains_key("fake/b:room:3") && !i.st.rooms.contains_key("fake/b:room")));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), bridge.to_string(), "the bridge's file is never written");
    assert!(r.dir.join("data/state.json").is_file());

    // The imported thread goes on in its session.
    let mut m = Rig::in_thread(Rig::msg("1", "carry on"), "fake/a:room:9");
    m.room = "fake/a:room".into();
    r.send(&m).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.calls("kith")[0]["resume"], "old-session");
    r.idle().await;
}

/// Writes tracing output into a buffer.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.locked().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn connections_follow_the_grants_and_secrets_stay_out_of_logs() {
    let logs = Logs::default();
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (jev, _) = fake_jev().await;
    let r = Rig::with(Opts { jev: Some(jev), ..Opts::new() });
    let secret = "s3cr3t-bot-token-value";
    r.put("granted", &json!({"telegram": ["telegram/hq"], "slack": ["slack/x"], "discord": ["discord/hq"]}));
    r.put("secrets", &json!({"telegram/hq": {"TELEGRAM_BOT_TOKEN": secret}, "slack/x": {"SLACK_BOT_TOKEN": "xoxb-1"}}));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut registry = Registry::default();
    {
        let seen = seen.clone();
        registry.add(
            "telegram",
            Arc::new(move |conn: &str, secrets: Secrets, _core: Arc<Core>| -> anyhow::Result<Arc<dyn Adapter>> {
                assert_eq!(format!("{secrets:?}"), "[\"TELEGRAM_BOT_TOKEN\"]");
                seen.locked().push(format!("{conn}={}", secrets.get("TELEGRAM_BOT_TOKEN").unwrap()));
                let fake: Arc<dyn Adapter> = Arc::new(Fake::new(conn));
                Ok(fake)
            }),
        );
    }
    registry.add(
        "discord",
        Arc::new(|_: &str, _: Secrets, _: Arc<Core>| -> anyhow::Result<Arc<dyn Adapter>> {
            anyhow::bail!("no token for you")
        }),
    );
    let bridge = json!({"conns": {"telegram/hq": {"offset": 3}}, "threads": {"telegram/hq:1:2": {"person": "kai"}}});
    std::fs::create_dir_all(r.dir.join("machine/bridge")).unwrap();
    std::fs::write(r.dir.join("machine/bridge/state.json"), bridge.to_string()).unwrap();

    let mut manager = Manager::new(r.core.clone(), registry, Backoff::default());
    manager.sync().await;
    assert_eq!(manager.running(), ["telegram/hq"]);
    assert_eq!(seen.locked().as_slice(), [format!("telegram/hq={secret}")]);
    assert_eq!(r.core.conn_state("telegram/hq")["offset"], 3, "imported when it started");
    let status = super::status().to_string();
    assert!(status.contains("slack/x") && status.contains("no adapter"), "{status}");

    // A message, with the router in play, for the logs to see.
    r.send(&Rig::msg("1", "@house SLEEP x")).await;
    until("the run to start", || !r.calls("kith").is_empty()).await;
    r.send(&Rig::in_thread(Rig::msg("1", "more"), "fake/x:room:1")).await;
    r.idle().await;

    // The grant is revoked: the connection stops.
    r.put("granted", &json!({}));
    manager.sync().await;
    assert!(manager.running().is_empty(), "revoked: stopped");

    let text = String::from_utf8(logs.0.locked().clone()).unwrap();
    assert!(text.contains("no adapter for slack"), "{text}");
    for hidden in [secret, "xoxb-1", "jev-secret-key"] {
        assert!(!text.contains(hidden), "a secret reached the logs");
    }
}

#[tokio::test]
async fn a_dropped_connection_starts_again_after_a_pause() {
    let mut failing = Fake::new("fake/f");
    failing.fail_start = true;
    let failing = Arc::new(failing);
    let backoff =
        Backoff { first: Duration::from_millis(10), max: Duration::from_millis(40), healthy: Duration::from_secs(60) };
    let ad: Arc<dyn Adapter> = failing.clone();
    let task = tokio::spawn(supervise(ad, backoff));
    until("three starts", || failing.starts.load(std::sync::atomic::Ordering::SeqCst) >= 3).await;
    task.abort();

    // One that goes quiet is restarted too.
    let mut quiet = Fake::new("fake/q");
    quiet.quiet = Some(Duration::from_millis(100));
    let quiet = Arc::new(quiet);
    let ad: Arc<dyn Adapter> = quiet.clone();
    let task = tokio::spawn(supervise(ad, backoff));
    until("a restart of the quiet one", || quiet.starts.load(std::sync::atomic::Ordering::SeqCst) >= 2).await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_that_arrive_while_a_turn_waits_for_the_gate_join_it() {
    let gate = GateSettings { max_running: 1, ..GateSettings::unlimited() };
    let r = Rig::with(Opts { gate, gate_task: true, ..Opts::new() });
    let permit = r.core.gate.enter("test:holder").granted().await;
    r.send(&Rig::msg("1", "@house first")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    r.send(&Rig::in_thread(Rig::msg("1", "second"), "fake/x:room:1")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(r.calls("kith").is_empty(), "waiting for room");
    drop(permit);
    until("the answer", || !r.fake.posts().is_empty()).await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 1, "one turn for both");
    let task = calls[0]["task"].as_str().unwrap();
    assert!(task.contains("[1] first") && task.contains("[2] second"), "{task}");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_process_holding_the_output_does_not_hang_the_turn() {
    let mut opts = Opts::new();
    opts.settings.reader_grace = Duration::from_millis(300);
    let r = Rig::with(opts);
    r.send(&Rig::msg("1", "@house BGHOLD now")).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    assert_eq!(r.fake.posts()[0].1, echo("BGHOLD now"));
    r.idle().await;
    // What was left of the agent's process group was killed with it.
    let pid = std::fs::read_to_string(r.dir.join("stub/bg.pid")).unwrap();
    until("the background process to be gone", || {
        !std::process::Command::new("kill").args(["-0", pid.trim()]).output().unwrap().status.success()
    })
    .await;
}

#[tokio::test]
async fn a_revoked_connection_takes_its_adapter_with_it() {
    let fake = Arc::new(Fake::new("fake/r"));
    let ad: Arc<dyn Adapter> = fake.clone();
    let task = tokio::spawn(supervise(ad, Backoff::default()));
    until("the adapter to start", || fake.starts.load(std::sync::atomic::Ordering::SeqCst) == 1).await;
    task.abort();
    until("the adapter's task to be dropped", || fake.dropped.load(std::sync::atomic::Ordering::SeqCst)).await;
}

#[tokio::test]
async fn a_failing_grants_listing_keeps_the_connections() {
    let r = Rig::with(Opts::new());
    r.put("granted", &json!({"telegram": ["telegram/hq"]}));
    r.put("secrets", &json!({"telegram/hq": {"TELEGRAM_BOT_TOKEN": "t"}}));
    let mut registry = Registry::default();
    registry.add(
        "telegram",
        Arc::new(|conn: &str, _: Secrets, _: Arc<Core>| -> anyhow::Result<Arc<dyn Adapter>> {
            let ad: Arc<dyn Adapter> = Arc::new(Fake::new(conn));
            Ok(ad)
        }),
    );
    let mut manager = Manager::new(r.core.clone(), registry, Backoff::default());
    manager.sync().await;
    assert_eq!(manager.running(), ["telegram/hq"]);
    r.put("granted", &json!({"_fail": true}));
    manager.sync().await;
    assert_eq!(manager.running(), ["telegram/hq"], "an error is not a revocation");
    r.put("granted", &json!({}));
    manager.sync().await;
    assert!(manager.running().is_empty(), "revoked: stopped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adapters_can_look_at_rooms_and_threads() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "/default house")).await;
    r.send(&Rig::msg("1", "@house hi")).await;
    until("the answer", || !r.fake.posts().is_empty()).await;
    r.idle().await;
    assert!(r.core.has_thread("fake/x:room:1") && !r.core.has_thread("fake/x:room:2"));
    assert_eq!(r.core.room_default("fake/x:room").as_deref(), Some("house"));
    assert_eq!(r.core.room_default("fake/x:other"), None);
    assert_eq!(r.core.rooms("fake/x").len(), 1);
    assert!(r.core.rooms("fake/y").is_empty(), "other connections have no rooms here");
    assert!(r.core.attachments_dir("telegram/hq").ends_with("attachments/telegram_hq"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pressing_a_button_twice_at_once_runs_the_task_once() {
    let r = Rig::new();
    r.send(&Rig::msg("1", "do the thing")).await;
    let pid = r
        .fake
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::Ask { pid, .. } => Some(pid),
            _ => None,
        })
        .expect("asked");
    let (one, two) = tokio::join!(r.core.picked(&r.ad, &pid, "house", "1"), r.core.picked(&r.ad, &pid, "house", "1"));
    let wins = [one.unwrap(), two.unwrap()];
    assert_eq!(wins.iter().filter(|w| **w).count(), 1, "{wins:?}");
    until("the answer", || !r.fake.posts().is_empty()).await;
    r.idle().await;
    assert_eq!(r.calls("kith").len(), 1);
}

/// The guest loses `shop` (the stub's access file is what kith-auth would say).
fn revoke_guest(r: &Rig) {
    r.put("access", &json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": []}}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_person_whose_run_waits_for_the_gate_never_starts_and_the_owners_does() {
    let gate = GateSettings { max_running: 1, ..GateSettings::unlimited() };
    let r = Rig::with(Opts { gate, gate_task: true, ..Opts::new() });
    let permit = r.core.gate.enter("test:holder").granted().await;
    r.send(&Rig::msg("2", "@shop guest waits")).await;
    r.send(&Rig::msg("1", "@house owner waits")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(r.calls("kith"), [] as [Value; 0]);
    revoke_guest(&r);
    r.core.stop_person("guest", false).await;
    drop(permit);
    r.idle().await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 1, "only the owner's run: {calls:?}");
    assert!(calls[0]["person"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_dequeued_before_the_sweep_but_not_spawned_is_refused_at_the_spawn_point() {
    let gate = GateSettings { max_running: 1, ..GateSettings::unlimited() };
    let r = Rig::with(Opts { gate, gate_task: true, ..Opts::new() });
    let permit = r.core.gate.enter("test:holder").granted().await;
    r.send(&Rig::msg("2", "@shop guest waits")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The sweep has not reached this run (it ran before it, or found nothing to stop): only kith-auth changed.
    revoke_guest(&r);
    drop(permit);
    r.idle().await;
    assert!(r.calls("kith").is_empty(), "no run after the revoke");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_left_in_the_inbox_by_an_earlier_host_is_swept_with_the_person() {
    let r = Rig::new();
    r.send(&Rig::msg("2", "@shop hi")).await;
    r.idle().await;
    let key = r.core.with(|i| i.st.threads.keys().next().cloned().unwrap());
    let wk = format!("{key}#shop");
    r.core.with(|i| {
        i.st.inbox.insert(wk.clone(), vec![vec!["left over".into()]]);
    });
    r.core.stop_person("guest", false).await;
    assert!(r.inbox_empty(&wk));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_person_who_is_invited_again_runs_again() {
    let r = Rig::new();
    revoke_guest(&r);
    r.core.stop_person("guest", false).await;
    r.put("access", &json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": ["shop"]}}));
    r.send(&Rig::msg("2", "@shop back")).await;
    until("the guest's answer", || !r.fake.posts().is_empty()).await;
}

/// The guest's messages and turns wait while this computer is out of sync with the Kith cloud (what kith-auth says).
fn hold_guest(r: &Rig, held: bool) {
    r.put("held", &json!(held));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_person_is_told_their_access_was_removed_and_nothing_runs() {
    let r = Rig::new();
    r.put(
        "people",
        &json!({"fake:1": {"person": "kai", "owner": true, "was": []},
                "fake:2": {"person": "guest", "owner": false, "was": [], "revoked": true}}),
    );
    r.send(&Rig::msg("2", "@shop hi")).await;
    let mut chatter = Rig::msg("2", "lunch?");
    chatter.addressed = false;
    r.send(&chatter).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(r.fake.replies(), ["Your access to kith on this computer was removed."]);
    assert!(r.calls("kith").is_empty(), "nothing ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_sent_while_out_of_sync_waits_and_runs_once_in_sync() {
    let r = Rig::new();
    hold_guest(&r, true);
    r.send(&Rig::msg("2", "@shop milk please")).await;
    r.send(&Rig::msg("1", "@house owner task")).await;
    until("the owner's answer", || !r.fake.posts().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let calls = r.calls("kith");
    assert_eq!(calls.len(), 1, "only the owner's run, never held: {calls:?}");
    assert!(calls[0]["person"].is_null());
    assert!(r.fake.replies().is_empty(), "{:?}", r.fake.replies());
    hold_guest(&r, false);
    until("the guest's answer", || r.fake.posts().iter().any(|(a, t)| a == "shop" && *t == echo("milk please"))).await;
    r.idle().await;
    assert_eq!(r.calls("kith")[1]["person"], "guest");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_held_past_the_limit_is_asked_again_and_nothing_runs() {
    let mut opts = Opts::new();
    opts.settings.sync_hold = Duration::from_millis(500);
    let r = Rig::with(opts);
    hold_guest(&r, true);
    r.send(&Rig::msg("2", "@shop milk please")).await;
    until("the reply", || !r.fake.replies().is_empty()).await;
    assert_eq!(
        r.fake.replies(),
        ["Kith on this computer is catching up with your other computers. Send it again in a minute."]
    );
    r.idle().await;
    assert!(r.calls("kith").is_empty(), "nothing ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_that_would_start_out_of_sync_waits_and_is_refused_after_the_limit() {
    let gate = GateSettings { max_running: 1, ..GateSettings::unlimited() };
    let mut opts = Opts { gate, gate_task: true, ..Opts::new() };
    opts.settings.sync_hold = Duration::from_millis(500);
    let r = Rig::with(opts);
    let permit = r.core.gate.enter("test:holder").granted().await;
    r.send(&Rig::msg("2", "@shop guest waits")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Taken while in sync; the sync is lost before its turn starts.
    hold_guest(&r, true);
    drop(permit);
    r.idle().await;
    assert!(r.calls("kith").is_empty(), "no run while out of sync");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_is_not_delivered_once_access_is_lost_during_the_turn() {
    let r = Rig::new();
    r.send(&Rig::msg("2", "@shop REVOKE then answer")).await;
    r.idle().await;
    assert_eq!(r.calls("kith").len(), 1, "the turn ran");
    assert!(r.fake.posts().is_empty(), "nothing delivered: {:?}", r.fake.posts());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_agent_sends_is_not_delivered_once_access_is_lost_during_the_turn() {
    let r = Rig::new();
    r.send(&Rig::msg("2", "@shop REVOKE SEND things")).await;
    r.idle().await;
    assert_eq!(r.calls("kith").len(), 1, "the turn ran");
    let events = r.fake.events();
    assert!(
        !events.iter().any(|e| matches!(e, Event::Post { agent, .. } | Event::Files { agent, .. } if agent == "image")),
        "{events:?}"
    );
    let outboxes = std::fs::read_dir(r.dir.join("data/outbox")).map_or(0, Iterator::count);
    assert_eq!(outboxes, 0, "the run's outbox is removed, posts and all");
}
