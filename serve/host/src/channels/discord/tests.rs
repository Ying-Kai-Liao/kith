//! The Discord adapter against a fake Discord (REST + gateway on a local port), the real core and the stub hq.

#![cfg(unix)]

mod fake;

use self::fake::{BOT, DM, Fd, GENERAL, GUILD, OWNER, Post};
use super::api::time_snowflake;
use super::{Config, Discord};
use crate::LockExt;
use crate::agent::gate::{Gate, Settings as GateSettings, memory};
use crate::channels::adapter::Adapter;
use crate::channels::adapter::Incoming;
use crate::channels::core::{self, Core};
use crate::channels::hq::Paths;
use crate::channels::judge::Judge;
use crate::channels::manager::{Backoff, supervise};
use crate::channels::settings::Settings;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const STUB: &str = include_str!("../../../tests/fixtures/channels_stub.py");
const CONN: &str = "discord/hq";
const GUEST: &str = "1002";

fn stub_script() -> &'static Path {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kith-discord-stub-{}", uuid::Uuid::new_v4()));
        // a fresh, unguessable folder that must not exist yet: nothing planted there is followed
        std::fs::create_dir(&dir).unwrap();
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

fn now_ms() -> u64 {
    u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

struct Rig {
    dir: PathBuf,
    fd: Arc<Fd>,
    core: Arc<Core>,
    ad: Arc<Discord>,
    cfg: Config,
    sup: Option<JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(s) = self.sup.take() {
            s.abort();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn paths(dir: &Path) -> Paths {
    let bin = dir.join("bin");
    Paths {
        hq_bin: bin.join("kith"),
        auth_bin: bin.join("kith-auth"),
        machine: dir.join("machine"),
        agent_roots: vec![dir.join("agents")],
        data: dir.join("data"),
    }
}

/// A core over the rig's folders: a new one reads the state file the last one saved, like a host restart.
fn make_core(dir: &Path) -> Arc<Core> {
    let settings = Settings {
        debounce: Duration::from_millis(100),
        hold: Duration::from_millis(400),
        poll: Duration::from_millis(40),
        ..Settings::default()
    };
    let paths = paths(dir);
    let gate = Gate::new(GateSettings::unlimited(), Box::new(memory::System));
    Core::new(core::Deps {
        paths: paths.clone(),
        settings,
        gate,
        backups: Arc::new(crate::agent::backups::Backups::new(paths.data.join("backups.json"))),
        judge: Judge::fixed(None, paths),
    })
}

impl Rig {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    async fn with(tune: impl FnOnce(&mut Config)) -> Self {
        let dir = std::env::temp_dir().join(format!("kith-discord-{}", uuid::Uuid::new_v4()));
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
        ] {
            std::fs::create_dir_all(dir.join("agents").join(name)).unwrap();
            std::fs::write(dir.join("agents").join(name).join("agent.md"), card).unwrap();
        }
        let files = [
            (
                "people",
                json!({format!("discord:{OWNER}"): {"person": "kai", "owner": true, "was": []},
                       format!("discord:{GUEST}"): {"person": "guest", "owner": false, "was": []}}),
            ),
            ("access", json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": ["shop"]}})),
        ];
        for (name, v) in files {
            std::fs::write(stub.join(format!("{name}.json")), v.to_string()).unwrap();
        }
        let fd = Fd::start().await;
        let mut cfg = Config {
            api_base: fd.api_base(),
            gateway_url: fd.gateway_url(),
            quiet: Duration::from_secs(600),
            rescan: Duration::from_secs(600),
            guild: None,
        };
        tune(&mut cfg);
        let core = make_core(&dir);
        let ad = Discord::new(CONN, "test-token", core.clone(), cfg.clone());
        Self { dir, fd, core, ad, cfg, sup: None }
    }

    fn start(&mut self) {
        let backoff = Backoff {
            first: Duration::from_millis(100),
            max: Duration::from_millis(300),
            healthy: Duration::from_secs(600),
        };
        let ad: Arc<dyn Adapter> = self.ad.clone();
        self.sup = Some(tokio::spawn(supervise(ad, backoff)));
    }

    fn stop(&mut self) {
        if let Some(s) = self.sup.take() {
            s.abort();
        }
    }

    /// A host restart: the connection ends, and a new core and adapter read the saved state.
    fn restart(&mut self) {
        self.stop();
        self.core = make_core(&self.dir);
        self.ad = Discord::new(CONN, "test-token", self.core.clone(), self.cfg.clone());
        self.start();
    }

    async fn ready(&self) {
        let ad = self.ad.clone();
        until("the adapter to be ready", move || *ad.ready.borrow()).await;
    }

    async fn started(mut self) -> Self {
        self.start();
        self.ready().await;
        self
    }

    fn channel(&self, name: &str) -> String {
        self.fd.st.locked().id_of_channel(name).unwrap_or_else(|| panic!("no channel {name}"))
    }

    fn hq_calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("stub/calls.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|c| c["tool"] == "kith").collect()
    }

    /// The agents' answers (what the stub echoes), not the working lines.
    fn answers(&self) -> Vec<Post> {
        self.fd.st.locked().posts.iter().filter(|p| p.content.contains("echo:") && !p.deleted).cloned().collect()
    }

    fn answers_with(&self, needle: &str) -> Vec<Post> {
        self.answers().into_iter().filter(|p| p.content.contains(needle)).collect()
    }

    async fn answered(&self, needle: &str) -> Post {
        until(&format!("an answer to {needle}"), || !self.answers_with(needle).is_empty()).await;
        self.answers_with(needle).remove(0)
    }

    fn say(&self, channel: &str, text: &str) -> String {
        self.fd.say(channel, OWNER, text, &json!({}), true, None)
    }

    fn threads(&self) -> Vec<Value> {
        self.fd.st.locked().channels.iter().filter(|c| c["type"] == 11).cloned().collect()
    }

    async fn idle(&self) {
        let core = self.core.clone();
        until("the queue to empty", move || core.with(|i| i.busy.is_empty())).await;
    }
}

fn footer() -> &'static str {
    "\n\n(From Discord: this thread is your task. Your final answer is posted here automatically; use kith-send for images and files. Helpers you call post status with `kith-send --as <their name>`.)"
}

fn custom_ids(components: &Value) -> Vec<String> {
    components
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row["components"].as_array().cloned().unwrap_or_default())
        .filter_map(|b| b["custom_id"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_gets_a_category_a_channel_a_webhook_and_a_role_per_agent() {
    let r = Rig::new().await.started().await;
    let st = r.fd.st.locked();
    let category = st.channels.iter().find(|c| c["type"] == 4 && c["name"] == "kith").expect("the kith category");
    for (agent, topic) in [("house", "Keeps the house"), ("personal", "Personal"), ("shop", "Shopping")] {
        let c = st.channels.iter().find(|c| c["name"] == agent).unwrap_or_else(|| panic!("no #{agent}"));
        assert_eq!((c["type"].as_u64(), &c["parent_id"], c["topic"].as_str()), (Some(0), &category["id"], Some(topic)));
        assert_eq!(st.hooks_of(c["id"].as_str().unwrap()).len(), 1, "one webhook for #{agent}");
    }
    let roles = st.requests("POST", "/roles");
    assert_eq!(roles.len(), 3);
    assert!(roles.iter().all(|r| r["mentionable"] == true));
    let hooks = r.core.conn_state(CONN)["hooks"].clone();
    assert_eq!(hooks.as_object().unwrap().len(), 3, "webhooks are kept in the connection's state: {hooks}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_message_gets_one_answer_in_a_thread_under_the_agents_name() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    // History from before the first run is not read back.
    r.fd.say(&house, OWNER, "before the first run", &json!({}), false, Some(now_ms() - 60_000));
    let id = r.say(&house, "hello");
    let answer = r.answered("hello").await;
    assert_eq!(answer.content, format!("echo:hello{}", footer()), "not marked late");
    assert_eq!(answer.username.as_deref(), Some("Home"), "the agent's display name");
    assert!(answer.via_webhook.is_some(), "through the channel's webhook");

    let threads = r.threads();
    assert_eq!(threads.len(), 1);
    assert_eq!(
        (threads[0]["name"].as_str(), threads[0]["parent_id"].as_str(), threads[0]["from"].as_str()),
        (Some("hello"), Some(house.as_str()), Some(id.as_str()))
    );
    assert_eq!(answer.channel, threads[0]["id"].as_str().unwrap(), "the answer is in the thread");

    // The working line has a Stop button while the agent works, and goes away when it is done.
    let working: Vec<Post> = r.fd.st.locked().posts.iter().filter(|p| p.content == "-# working…").cloned().collect();
    assert_eq!(working.len(), 1);
    assert!(custom_ids(&working[0].components)[0].starts_with("kith-stop:"));
    until("the working line to go", || r.fd.st.locked().posts.iter().all(|p| p.content != "-# working…" || p.deleted))
        .await;
    assert_eq!(r.answers().len(), 1);
    r.idle().await;
    // kith-send posts into the thread through the same webhook.
    let call = &r.hq_calls()[0];
    assert_eq!(
        call["withheld"],
        json!(["HQ_DISCORD_THREAD_ID", "HQ_DISCORD_WEBHOOK_URL", "KITH_DISCORD_THREAD_ID", "KITH_DISCORD_WEBHOOK_URL"])
    );
    let conv = json!({"thread": threads[0]["id"], "channel": house, "key": "k"});
    let env = r.ad.env(&conv, "house");
    let hook = r.fd.st.locked().hooks_of(&house)[0].clone();
    assert_eq!(
        env,
        vec![
            ("KITH_DISCORD_WEBHOOK_URL".to_owned(), hook.clone()),
            ("HQ_DISCORD_WEBHOOK_URL".to_owned(), hook),
            ("KITH_DISCORD_THREAD_ID".to_owned(), threads[0]["id"].as_str().unwrap().to_owned()),
            ("HQ_DISCORD_THREAD_ID".to_owned(), threads[0]["id"].as_str().unwrap().to_owned())
        ]
    );
    assert!(r.ad.env(&json!({"dm": DM}), "house").is_empty(), "no webhooks in a DM");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn at_agent_in_another_channel_talks_to_that_agent_and_other_chatter_is_left_alone() {
    let r = Rig::new().await.started().await;
    r.say(GENERAL, "lunch anyone?");
    r.say(GENERAL, "@shop buy eggs");
    let a = r.answered("buy eggs").await;
    assert_eq!(a.content, format!("echo:buy eggs{}", footer()));
    assert_eq!(a.username.as_deref(), Some("shop"));
    assert_eq!(r.threads().len(), 1, "the chatter opened no thread");
    assert_eq!(r.fd.st.locked().hooks_of(GENERAL).len(), 1, "the channel got its webhook for hq-send");

    // The agent's role is the same as typing its name, and the bot's own mention is taken out.
    let role_id = r.fd.st.locked().role_id("house");
    r.say(GENERAL, &format!("<@&{role_id}> water the plants"));
    assert_eq!(r.answered("water the plants").await.username.as_deref(), Some("Home"));
    r.say(GENERAL, &format!("<@{BOT}> @personal call mum"));
    assert_eq!(r.answered("call mum").await.username.as_deref(), Some("personal"));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bots_webhooks_and_threads_that_are_not_hqs_are_ignored() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.fd.say(&house, "4000", "from another bot", &json!({"author": {"id": "4000", "bot": true}}), true, None);
    r.fd.say(&house, "4001", "from a webhook", &json!({"webhook_id": "77"}), true, None);
    r.fd.add_thread("900", GENERAL, "someone else's");
    r.say("900", "just people talking");
    // The thread is only talked into when addressed.
    r.say("900", "@house are you there");
    assert_eq!(r.answered("are you there").await.channel, "900", "answered inside their thread as it is");
    assert_eq!(r.answers().len(), 1);
    assert!(r.threads().len() == 1, "no thread was opened");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_delivered_twice_is_handled_once() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    let id = r.say(&house, "only once");
    r.answered("only once").await;
    r.fd.replay(&house, &id);
    r.say(&house, "marker");
    r.answered("marker").await;
    r.idle().await;
    assert_eq!(r.answers_with("only once").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dm_is_answered_in_the_dm_and_a_reply_to_the_answer_continues_the_task() {
    let r = Rig::new().await.started().await;
    r.say(DM, "what is for dinner");
    let a = r.answered("what is for dinner").await;
    assert_eq!(a.channel, DM);
    assert!(a.content.contains("echo:what is for dinner"));
    // No webhook in a DM: the bot posts, with the agent's name on top.
    let posted = r.fd.st.locked().posts_in(DM);
    assert!(posted.iter().any(|p| p.content.starts_with("**personal**\n")), "{posted:?}");
    assert!(posted.iter().all(|p| p.via_webhook.is_none()));
    assert!(r.threads().is_empty(), "a DM has no threads");
    r.idle().await;
    // A reply to that answer is the same task: the personal agent resumes its session.
    let answer_id = posted.iter().find(|p| p.content.starts_with("**personal**")).unwrap().id.clone();
    let reply = json!({"message_reference": {"message_id": answer_id}, "type": 19});
    r.fd.say(DM, OWNER, "and tomorrow?", &reply, true, None);
    r.answered("and tomorrow?").await;
    r.idle().await;
    let calls = r.hq_calls();
    assert_eq!(calls[1]["resume"], "sess-personal-1", "{calls:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachments_are_downloaded_and_passed_on() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    let host = r.fd.st.locked().host.clone();
    let files = json!({"attachments": [{"id": "1", "filename": "../note.txt", "url": format!("{host}/cdn/note.txt")}]});
    let id = r.fd.say(&house, OWNER, "read this", &files, true, None);
    r.answered("read this").await;
    let saved = r.core.attachments_dir(CONN).join(&id).join("note.txt");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "attachment-bytes");
    assert!(r.hq_calls()[0]["task"].as_str().unwrap().contains(&saved.to_string_lossy().into_owned()));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_button_stops_the_work_for_its_owner_only() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "SLOW job");
    until("the working line", || r.fd.st.locked().posts.iter().any(|p| p.content == "-# working…" && !p.deleted)).await;
    let line = r.fd.st.locked().posts.iter().find(|p| p.content == "-# working…").cloned().unwrap();
    let button = custom_ids(&line.components).remove(0);
    until("the agent to run", || !r.hq_calls().is_empty()).await;

    // Someone else's press is refused, privately.
    let token = r.fd.press(GUEST, &button, &line.id);
    let path = format!("/webhooks/{}/{token}", fake::APP);
    until("the refusal", || !r.fd.st.locked().requests("POST", &path).is_empty()).await;
    let refusal = r.fd.st.locked().requests("POST", &path).remove(0);
    assert_eq!(
        (refusal["content"].as_str(), refusal["flags"].as_u64()),
        (Some("Only the person who started this can stop it."), Some(64))
    );
    assert!(r.fd.st.locked().requests("POST", "/interactions/").iter().any(|b| b["type"] == 6), "acknowledged at once");

    let token = r.fd.press(OWNER, &button, &line.id);
    let edit = format!("/webhooks/{}/{token}/messages/@original", fake::APP);
    until("the line to say stopped", || {
        r.fd.st.locked().posts.iter().any(|p| p.id == line.id && p.content == "-# stopped")
    })
    .await;
    // The run ending and the press race: "stopping…" may be skipped, but never lands after "stopped".
    until("the press to be acknowledged", || {
        r.fd.st.locked().requests("POST", "/interactions/").iter().filter(|b| b["type"] == 6).count() == 2
    })
    .await;
    for said in r.fd.st.locked().requests("PATCH", &edit) {
        assert_eq!((said["content"].as_str(), &said["components"]), (Some("-# stopping…"), &json!([])));
    }
    let stopped = r.fd.st.locked().posts.iter().find(|p| p.id == line.id).cloned().unwrap();
    assert_eq!(stopped.components, json!([]), "the button is gone");
    assert!(r.answers().is_empty(), "stopped work answers nothing");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn who_should_take_this_asks_with_buttons_and_a_double_press_runs_once() {
    let r = Rig::new().await.started().await;
    let id = r.say(GENERAL, &format!("<@{BOT}> remind me about the bins"));
    until("the question", || r.fd.st.locked().posts_in(GENERAL).iter().any(|p| p.content == "Who should take this?"))
        .await;
    let ask = r.fd.st.locked().posts_in(GENERAL).into_iter().find(|p| p.content == "Who should take this?").unwrap();
    let buttons = custom_ids(&ask.components);
    assert_eq!(buttons.len(), 3, "a button per agent: {buttons:?}");
    let reply_to =
        r.fd.st.locked().messages_json(GENERAL).iter().find(|m| m["id"] == ask.id).map(|m| m["reply"].clone()).unwrap();
    assert_eq!(reply_to, json!(id), "asked as a reply to the message");
    assert!(r.answers().is_empty(), "nothing is answered before a button is pressed");

    let shop = buttons.iter().find(|b| b.ends_with(":shop")).unwrap().clone();
    let first = r.fd.press(OWNER, &shop, &ask.id);
    let a = r.answered("remind me about the bins").await;
    assert_eq!(a.username.as_deref(), Some("shop"));
    let edit = format!("/webhooks/{}/{first}/messages/@original", fake::APP);
    until("the question to say who took it", || !r.fd.st.locked().requests("PATCH", &edit).is_empty()).await;
    assert_eq!(r.fd.st.locked().requests("PATCH", &edit)[0]["content"], "→ shop");

    let second = r.fd.press(OWNER, &shop, &ask.id);
    let again = format!("/webhooks/{}/{second}", fake::APP);
    until("the second press to be answered", || !r.fd.st.locked().requests("POST", &again).is_empty()).await;
    assert_eq!(r.fd.st.locked().requests("POST", &again)[0]["flags"], 64);
    r.idle().await;
    assert_eq!(r.answers().len(), 1, "run once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_sent_while_down_are_answered_once_and_marked_late() {
    let mut r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "msg-live");
    r.answered("msg-live").await;
    r.idle().await;
    r.stop();

    // Python's check: the last message seen is 30 h old; one waiting message is 26 h old (skipped), one 10 minutes.
    let last = time_snowflake(now_ms() - 30 * 3_600_000);
    r.core.update_conn(CONN, |cs| cs["last"] = json!(last));
    r.fd.say(&house, OWNER, "msg-too-old", &json!({}), false, Some(now_ms() - 26 * 3_600_000));
    r.fd.say(&house, OWNER, "msg-while-down", &json!({}), false, Some(now_ms() - 600_000));
    r.restart();
    r.ready().await;
    let late = r.answered("msg-while-down").await;
    assert!(late.content.contains("picked it up late"), "marked late: {}", late.content);
    r.idle().await;
    assert!(r.answers_with("msg-too-old").is_empty(), "older than 24 h is skipped");
    assert_eq!(r.answers_with("msg-live").len(), 1, "answered before the restart: not again");

    // A second restart answers nothing again.
    r.restart();
    r.ready().await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    r.idle().await;
    assert_eq!(
        (r.answers_with("msg-while-down").len(), r.answers_with("msg-live").len(), r.answers().len()),
        (1, 1, 2)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_gateway_is_replaced_by_a_new_connection_that_works() {
    let r = Rig::with(|c| c.quiet = Duration::from_millis(1500)).await;
    r.fd.hello_ms.store(5000, Ordering::SeqCst); // no heartbeat of ours to notice it first
    let mut r = r.started().await;
    let house = r.channel("house");
    assert_eq!(r.fd.conns.load(Ordering::SeqCst), 1);
    r.fd.mute();
    until("a second connection", || r.fd.conns.load(Ordering::SeqCst) >= 2).await;
    r.ready().await;
    r.say(&house, "msg-after-reconnect");
    r.answered("msg-after-reconnect").await;
    r.idle().await;
    assert_eq!(r.answers_with("msg-after-reconnect").len(), 1);
    r.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_gateway_resumes_and_nothing_is_answered_twice() {
    let mut r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "before the drop");
    r.answered("before the drop").await;
    r.fd.kick();
    r.fd.say(&house, OWNER, "during the drop", &json!({}), true, None);
    r.answered("during the drop").await;
    r.idle().await;
    until("the gateway to be resumed", || r.fd.resumes.load(Ordering::SeqCst) >= 1).await;
    assert_eq!(
        (r.fd.identifies.load(Ordering::SeqCst), r.fd.resumes.load(Ordering::SeqCst)),
        (1, 1),
        "resumed, not started over"
    );
    assert_eq!((r.answers_with("before the drop").len(), r.answers_with("during the drop").len()), (1, 1));
    r.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threads_and_webhooks_from_the_bridge_keep_working() {
    let mut r = Rig::new().await;
    let host = r.fd.st.locked().host.clone();
    r.fd.register_hook("777", GENERAL, "oldtoken");
    r.fd.add_thread("300", GENERAL, "an old task");
    let bridge = json!({
        "version": 2,
        "conns": {CONN: {"hooks": {GENERAL: format!("{host}/api/v10/webhooks/777/oldtoken")}, "last": 5}},
        "threads": {"discord/hq:300": {"person": "kai", "owner": "house", "last": "house", "room": "discord/hq:200",
                                        "sessions": {"house": "bridge-session"},
                                        "conv": {"thread": 300, "channel": 200, "key": "discord/hq:300"}}},
        "rooms": {}, "replies": {}, "inbox": {}, "pending": {}
    });
    let file = r.dir.join("machine/bridge/state.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, bridge.to_string()).unwrap();
    r.core.import_bridge(CONN).unwrap();
    r.start();
    r.ready().await;
    r.say("300", "carry on");
    let a = r.answered("carry on").await;
    assert_eq!((a.via_webhook.as_deref(), a.channel.as_str()), (Some("777"), "300"), "the imported webhook posted it");
    assert_eq!(r.hq_calls()[0]["resume"], "bridge-session");
    assert!(r.fd.st.locked().requests("POST", "/channels/200/webhooks").is_empty(), "no second webhook for #general");
    r.idle().await;
    r.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_webhook_is_made_again_once_and_a_rate_limit_is_waited_out() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "first");
    r.answered("first").await;
    r.idle().await;
    let before = r.fd.st.locked().requests("POST", &format!("/channels/{house}/webhooks")).len();
    r.fd.forget_hooks();
    r.fd.fail_once("POST /webhooks/", 429, json!({"retry_after": 0.05, "global": false}));
    r.say(&house, "second");
    let a = r.answered("second").await;
    r.idle().await;
    assert_eq!(r.answers_with("second").len(), 1);
    assert_eq!(r.fd.st.locked().requests("POST", &format!("/channels/{house}/webhooks")).len(), before + 1);
    assert!(a.via_webhook.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archived_thread_is_opened_again_to_post() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "one");
    r.answered("one").await;
    r.idle().await;
    let thread = r.threads()[0]["id"].as_str().unwrap().to_owned();
    r.fd.fail_once("POST /webhooks/", 400, json!({"code": 50083, "message": "Thread is archived"}));
    r.say(&thread, "two");
    r.answered("two").await;
    r.idle().await;
    assert!(r.fd.st.locked().requests("PATCH", &format!("/channels/{thread}")).iter().any(|b| b["archived"] == false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_go_through_the_webhook_as_multipart_under_the_agents_name() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "make a picture");
    r.answered("make a picture").await;
    r.idle().await;
    let thread = r.threads()[0]["id"].as_str().unwrap().to_owned();
    let file = r.dir.join("pic.txt");
    std::fs::write(&file, "PICTURE").unwrap();
    let conv = json!({"thread": thread, "channel": house, "key": "k"});
    let files = [file.to_string_lossy().into_owned(), "/no/such/file".to_owned()];
    r.ad.send_files(&conv, "house", &files, "here it is").await.unwrap();
    let post = r.fd.st.locked().posts.iter().find(|p| !p.files.is_empty()).cloned().unwrap();
    assert_eq!(
        (post.username.as_deref(), post.content.as_str(), post.channel.as_str()),
        (Some("Home"), "here it is", thread.as_str())
    );
    assert_eq!(post.files, vec![("pic.txt".to_owned(), "PICTURE".to_owned())]);
    // A DM gets the files from the bot itself.
    r.ad.send_files(&json!({"dm": DM, "key": "k"}), "house", &files[..1], "").await.unwrap();
    let dm = r.fd.st.locked().posts_in(DM).into_iter().find(|p| !p.files.is_empty()).unwrap();
    assert_eq!((dm.content.as_str(), dm.via_webhook), ("**Home**", None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_answer_is_split_and_the_context_line_is_added() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "start");
    r.answered("start").await;
    r.idle().await;
    let thread = r.threads()[0]["id"].as_str().unwrap().to_owned();
    let before = r.fd.st.locked().posts_in(&thread).len();
    let long = format!("{}\n{}", "a".repeat(1500), "b".repeat(1500));
    let conv = json!({"thread": thread, "channel": house, "key": "k"});
    r.ad.post(&conv, "house", &long, Some(41.6)).await.unwrap();
    let posts = r.fd.st.locked().posts_in(&thread);
    let new = &posts[before..];
    assert_eq!(new.len(), 2);
    assert!(new[0].content.starts_with('a') && new[1].content.starts_with('b'));
    assert!(new[1].content.ends_with("\n-# context 42%"));
    assert!(new.iter().all(|p| p.content.chars().count() <= 1900));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_agents_channel_is_made_and_a_deleted_one_comes_back_by_the_rescan() {
    let mut r = Rig::with(|c| c.rescan = Duration::from_millis(200)).await.started().await;
    let shop = r.channel("shop");
    {
        let mut st = r.fd.st.locked();
        st.channels.retain(|c| c["name"] != "shop"); // deleted by hand
        let house = st.channels.iter_mut().find(|c| c["name"] == "house").unwrap();
        house["topic"] = json!("someone changed it");
    }
    until("the channel to come back", || r.fd.st.locked().id_of_channel("shop").is_some()).await;
    assert_ne!(r.channel("shop"), shop);
    let topic =
        |name: &str| r.fd.st.locked().channels.iter().find(|c| c["name"] == name).map(|c| c["topic"].clone()).unwrap();
    // Someone else's edit of an existing channel's topic stays: only channels hq makes get one.
    assert_eq!(topic("house"), json!("someone changed it"));
    assert_eq!(topic("shop"), json!("Shopping"));
    assert!(r.fd.st.locked().requests("PATCH", "/channels/").is_empty(), "no channel was edited");
    r.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_manage_roles_there_are_no_roles_and_plain_at_agent_works() {
    let r = Rig::new().await;
    r.fd.st.locked().perms = "0".into();
    let r = r.started().await;
    assert!(r.fd.st.locked().requests("POST", "/roles").is_empty(), "no roles without the permission");
    assert_eq!(r.core.conn_state(CONN)["roles"], "none");
    r.say(GENERAL, "@house hi there");
    r.answered("hi there").await;
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn several_servers_need_the_setting_and_a_matching_name_or_id_picks_one() {
    // No setting: the connection stops with a message that says what to do.
    let r = Rig::new().await;
    r.fd.st.locked().guilds.push(("101".into(), "Work Place".into()));
    let ad: Arc<dyn Adapter> = r.ad.clone();
    let err = format!("{:#}", ad.start().await.unwrap_err());
    assert!(err.contains("expected one server") && err.contains("discord_guild"), "{err}");

    // By name (case and spaces don't matter) and by id.
    for want in ["workplace", "101"] {
        let guild = want.to_owned();
        let mut r2 = Rig::with(move |c| c.guild = Some(guild)).await;
        r2.fd.st.locked().guilds.push(("101".into(), "Work Place".into()));
        r2.start();
        r2.ready().await;
        assert_eq!(r2.ad.rt.locked().guild.as_ref().map(|g| g.id.clone()).as_deref(), Some("101"));
        r2.stop();
    }
}

/// A rig for connection `conn` with `channels.discord_guild` at `setting`; `linked`: whether kith knows the owner's
/// Discord account (otherwise kith-auth knows kai as the owner and nobody's handle). The server is "Home", `OWNER`'s.
async fn owner_rig(conn: &str, setting: Option<&str>, linked: bool) -> Rig {
    let guild = setting.map(str::to_owned);
    let mut r = Rig::with(move |c| c.guild = guild).await;
    if !linked {
        let people =
            json!({"*": {"person": null, "owner": false, "was": [], "owner_id": "kai", "owner_linked": false}});
        std::fs::write(r.dir.join("stub/people.json"), people.to_string()).unwrap();
    }
    r.ad = Discord::new(conn, "test-token", r.core.clone(), r.cfg.clone());
    r.started().await
}

/// Whether the server's owner is taken as kith's owner: what the adapter says about their message, and who kith
/// says they are when they ask in a DM.
async fn owner_stands_in(r: &Rig) -> (bool, String) {
    let msg = json!({"id": "9001", "channel_id": GENERAL, "guild_id": GUILD, "author": {"id": OWNER}, "content": "hi"});
    let admin = r.ad.incoming(&msg).await.unwrap().expect("a message in the server").admin;
    r.fd.say(DM, OWNER, "/whoami", &json!({}), true, None);
    let said = || r.fd.st.locked().posts_in(DM).into_iter().map(|p| p.content).find(|c| !c.contains("working"));
    until("an answer in the DM", || said().is_some()).await;
    (admin, said().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_named_servers_owner_is_taken_as_the_owner_before_linking() {
    for (conn, setting, stands_in) in [
        // The server channels.discord_guild names (by name or id): its owner is kith's owner until linked.
        (CONN, Some("home"), true),
        (CONN, Some(GUILD), true),
        ("discord/work", Some("Home"), true),
        // Another server than the one named: its owner is no one to kith.
        ("discord/work", Some("Work Place"), false),
        // Nothing named: no server's owner is.
        (CONN, None, false),
        (CONN, Some(""), false),
    ] {
        let r = owner_rig(conn, setting, false).await;
        let (admin, said) = owner_stands_in(&r).await;
        assert_eq!(admin, stands_in, "{conn} {setting:?}");
        if stands_in {
            assert!(said.starts_with("You are kai in kith."), "{conn} {setting:?}: {said}");
        } else {
            assert!(said.contains("not linked to kith"), "{conn} {setting:?}: {said}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_owner_is_the_owner_whatever_server_is_named() {
    for (conn, setting) in [(CONN, Some("home")), ("discord/work", Some("Work Place")), (CONN, None)] {
        let r = owner_rig(conn, setting, true).await;
        let (_, said) = owner_stands_in(&r).await;
        assert!(said.starts_with("You are kai in kith."), "{conn} {setting:?}: {said}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_renamed_to_an_agents_name_is_that_agents_room() {
    let r = Rig::new().await.started().await;
    r.say(GENERAL, "chatter, nobody answers");
    tokio::time::sleep(Duration::from_millis(300)).await; // handled (and ignored) under its old name
    r.fd.rename(GENERAL, "shop");
    // The rename reaches the adapter as an event; a message in the room is then the shop agent's without a mention.
    tokio::time::sleep(Duration::from_millis(200)).await;
    r.say(GENERAL, "two tins of beans");
    assert_eq!(r.answered("two tins of beans").await.username.as_deref(), Some("shop"));
    assert!(r.answers_with("chatter").is_empty(), "the first message was not for hq");
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_is_marked_seen_before_the_core_gets_it_and_not_taken_twice() {
    let r = Rig::new().await;
    let seen =
        |id: u64| r.core.conn_state(CONN)["seen"].as_array().is_some_and(|s| s.iter().any(|v| v.as_u64() == Some(id)));
    let first = r.ad.claim(5).expect("a new message is taken");
    assert!(r.ad.claim(5).is_none(), "not twice at once");
    r.ad.mark_seen(5);
    drop(first);
    assert!(r.ad.claim(5).is_none() && seen(5), "marked: a crash cannot run it twice");
    assert_eq!(r.core.conn_state(CONN)["last"], 5);

    // Through the whole path: while a long task runs, its message is already remembered.
    let r = Rig::new().await.started().await;
    let id = r.say(&r.channel("house"), "SLOW job");
    until("the agent to run", || !r.hq_calls().is_empty()).await;
    assert!(
        r.core.conn_state(CONN)["seen"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some(id.as_str()) || v.as_u64() == id.parse().ok())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_message_right_after_a_restart_does_not_hide_what_came_before() {
    let mut r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "msg-first");
    r.answered("msg-first").await;
    r.idle().await;
    r.stop();
    r.core.update_conn(CONN, |cs| cs["last"] = json!(time_snowflake(now_ms() - 30 * 3_600_000)));
    r.fd.say(&house, OWNER, "msg-while-down", &json!({}), false, Some(now_ms() - 600_000));
    *r.fd.after_ready.locked() = Some((house.clone(), "msg-live-at-once".into()));
    r.restart();
    r.ready().await;
    r.answered("msg-live-at-once").await;
    r.answered("msg-while-down").await;
    r.idle().await;
    assert_eq!((r.answers_with("msg-while-down").len(), r.answers_with("msg-live-at-once").len()), (1, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_text_cannot_ping_everyone_or_a_role() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    r.say(&house, "hello");
    r.answered("hello").await;
    r.idle().await;
    let thread = r.threads()[0]["id"].as_str().unwrap().to_owned();
    let conv = json!({"thread": thread, "channel": house, "key": "k"});
    r.ad.post(&conv, "house", "@everyone look", None).await.unwrap();
    r.ad.reply(&Incoming { origin: json!({"channel": house, "message": "1"}), ..Incoming::default() }, "hi")
        .await
        .unwrap();
    let st = r.fd.st.locked();
    let sent: Vec<Value> = st
        .log
        .iter()
        .filter(|q| q.method == "POST" && (q.path.starts_with("/webhooks/") || q.path.ends_with("/messages")))
        .map(|q| q.body.clone())
        .collect();
    assert!(sent.len() >= 3, "{sent:?}");
    assert!(sent.iter().all(|b| b["allowed_mentions"] == json!({"parse": ["users"]})), "{sent:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn big_attachments_are_left_out_with_a_note_and_same_named_ones_both_stay() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    let host = r.fd.st.locked().host.clone();
    let url = format!("{host}/cdn/x");
    let files = json!({"attachments": [
        {"id": "1", "filename": "a.txt", "url": url},
        {"id": "2", "filename": "a.txt", "url": url},
        {"id": "3", "filename": "big.bin", "size": 30_000_000u64, "url": url}]});
    let id = r.fd.say(&house, OWNER, "three files", &files, true, None);
    r.answered("three files").await;
    let dir = r.core.attachments_dir(CONN).join(&id);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "attachment-bytes");
    assert_eq!(std::fs::read_to_string(dir.join("1-a.txt")).unwrap(), "attachment-bytes");
    assert!(!dir.join("big.bin").exists());
    assert!(r.hq_calls()[0]["task"].as_str().unwrap().contains("(file too big: big.bin)"));
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strangers_files_are_not_downloaded() {
    let r = Rig::new().await.started().await;
    let house = r.channel("house");
    let host = r.fd.st.locked().host.clone();
    let files = json!({"attachments": [{"id": "1", "filename": "note.txt", "url": format!("{host}/cdn/note.txt")}]});
    let id = r.fd.say(&house, "424242", "read this", &files, true, None);
    tokio::time::sleep(Duration::from_millis(800)).await; // time to (not) download
    assert!(!r.core.attachments_dir(CONN).join(&id).exists());
}
