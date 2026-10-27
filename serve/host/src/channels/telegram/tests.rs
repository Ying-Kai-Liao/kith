//! The Telegram adapter against a fake Bot API (axum), the real core and the stub hq
//! (`tests/fixtures/channels_stub.py`). Ported from the retired Python bridge's checks.

#![cfg(unix)]

use super::api::{Api, TgError};
use super::format::{escape, tg_html};
use super::{Telegram, command_target, without_mention};
use crate::LockExt;
use crate::agent::gate::{Gate, Settings as GateSettings, memory};
use crate::channels::adapter::{Adapter, Reaction};
use crate::channels::hq::Paths;
use crate::channels::judge::Judge;
use crate::channels::manager::{Backoff, supervise};
use crate::channels::settings::Settings;
use crate::channels::{Core, core};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const STUB: &str = include_str!("../../../tests/fixtures/channels_stub.py");
const TOKEN: &str = "123456:TESTTOKENVALUE";
const BOT: &str = "hqbot";
const BOT_ID: i64 = 999;
const KAI: i64 = 1;
const GUEST: i64 = 2;
const DM: i64 = 100;
const GROUP: i64 = -100_200;

fn stub_script() -> &'static std::path::Path {
    static SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    SCRIPT.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kith-telegram-stub-{}", std::process::id()));
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

// ---- the fake Bot API ----

#[derive(Default)]
struct BotState {
    updates: Vec<Value>,
    next_id: i64,
    calls: Vec<(String, Value)>,
    /// The offset of every getUpdates.
    offsets: Vec<i64>,
    logins: usize,
    hang: bool,
    /// How many getUpdates still answer 409.
    conflict: usize,
    /// How many sends (messages or files) still answer 429.
    rate_limit: usize,
    reject_html: bool,
    files: HashMap<String, Vec<u8>>,
    /// How long a file download takes, and how many started.
    file_delay: Duration,
    file_hits: usize,
}

struct Bot {
    url: String,
    state: Mutex<BotState>,
}

type Shared = State<Arc<Bot>>;

fn failure(code: u16, description: &str, extra: &Value) -> (StatusCode, Json<Value>) {
    let body = json!({"ok": false, "error_code": code, "description": description, "parameters": extra});
    (StatusCode::from_u16(code).unwrap(), Json(body))
}

async fn bot_call(
    State(bot): Shared,
    Path((token, method)): Path<(String, String)>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    if token != format!("bot{TOKEN}") {
        return failure(401, "Unauthorized", &json!({}));
    }
    let body = serde_json::from_slice::<Value>(&body)
        .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&body).into_owned()}));
    let ok = |result: Value| (StatusCode::OK, Json(json!({"ok": true, "result": result})));
    if method == "getUpdates" {
        let offset = body["offset"].as_i64().unwrap_or(0);
        {
            let mut st = bot.state.locked();
            st.offsets.push(offset);
            if st.conflict > 0 {
                st.conflict -= 1;
                return failure(409, "Conflict: terminated by other getUpdates request", &json!({}));
            }
        }
        for _ in 0..8 {
            let (hang, found) = {
                let st = bot.state.locked();
                let found: Vec<Value> =
                    st.updates.iter().filter(|u| u["update_id"].as_i64().unwrap_or(0) >= offset).cloned().collect();
                (st.hang, found)
            };
            if hang {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            if !found.is_empty() {
                return ok(Value::Array(found));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        return ok(json!([]));
    }
    let mut st = bot.state.locked();
    st.calls.push((method.clone(), body.clone()));
    match method.as_str() {
        "getMe" => {
            st.logins += 1;
            ok(json!({"id": BOT_ID, "is_bot": true, "username": BOT}))
        }
        "sendMessage" | "sendPhoto" | "sendDocument" => {
            if st.rate_limit > 0 {
                st.rate_limit -= 1;
                if let Some((_, call)) = st.calls.last_mut() {
                    call["rejected"] = json!(true);
                }
                return failure(429, "Too Many Requests: retry after 1", &json!({"retry_after": 1}));
            }
            if method == "sendMessage" && st.reject_html && body.get("parse_mode").is_some() {
                if let Some((_, call)) = st.calls.last_mut() {
                    call["rejected"] = json!(true);
                }
                return failure(400, "Bad Request: can't parse entities", &json!({}));
            }
            st.next_id += 1;
            let id = st.next_id;
            if let Some((_, call)) = st.calls.last_mut() {
                call["sent_id"] = json!(id); // so a test can press the button of the line it was sent as
            }
            ok(json!({"message_id": id, "chat": {"id": body["chat_id"]}}))
        }
        "getFile" => {
            let id = body["file_id"].as_str().unwrap_or("").to_owned();
            if st.files.contains_key(&id) {
                ok(json!({"file_id": id, "file_path": format!("docs/{id}.txt")}))
            } else {
                failure(400, "Bad Request: file is too big", &json!({}))
            }
        }
        _ => ok(json!(true)),
    }
}

async fn bot_file(State(bot): Shared, Path((token, name)): Path<(String, String)>) -> (StatusCode, Vec<u8>) {
    if token != format!("bot{TOKEN}") {
        return (StatusCode::NOT_FOUND, Vec::new());
    }
    let id = name.trim_start_matches("docs/").trim_end_matches(".txt");
    let delay = {
        let mut st = bot.state.locked();
        st.file_hits += 1;
        st.file_delay
    };
    tokio::time::sleep(delay).await;
    let found = bot.state.locked().files.get(id).cloned();
    found.map_or((StatusCode::NOT_FOUND, Vec::new()), |bytes| (StatusCode::OK, bytes))
}

impl Bot {
    async fn start() -> Arc<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bot = Arc::new(Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            state: Mutex::new(BotState { next_id: 1000, ..BotState::default() }),
        });
        let app = Router::new()
            .route("/{token}/{method}", post(bot_call))
            .route("/file/{token}/{*name}", get(bot_file))
            .with_state(bot.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        bot
    }

    /// A user's message, sent `ago` seconds ago -> (update id, message id).
    fn say(&self, from: i64, chat: i64, text: &str, ago: u64, extra: &Value) -> (i64, i64) {
        let mut st = self.state.locked();
        st.next_id += 1;
        let mid = st.next_id;
        let date = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - ago;
        let kind = if chat > 0 { "private" } else { "supergroup" };
        let mut message = json!({"message_id": mid, "date": date, "from": {"id": from, "is_bot": false},
                                 "chat": {"id": chat, "type": kind}, "text": text});
        for (k, v) in extra.as_object().into_iter().flatten() {
            message[k] = v.clone();
        }
        let uid = i64::try_from(st.updates.len()).unwrap() + 40;
        st.updates.push(json!({"update_id": uid, "message": message}));
        (uid, mid)
    }

    fn push_update(&self, update: &Value) -> i64 {
        let mut st = self.state.locked();
        let uid = i64::try_from(st.updates.len()).unwrap() + 40;
        let mut update = update.clone();
        update["update_id"] = json!(uid);
        st.updates.push(update);
        uid
    }

    fn press(&self, from: i64, chat: i64, message: i64, data: &str) {
        self.push_update(&json!({"callback_query": {"id": format!("cb{data}"), "from": {"id": from, "is_bot": false},
            "data": data, "message": {"message_id": message, "chat": {"id": chat}}}}));
    }

    /// The calls Telegram did not refuse.
    fn accepted(&self, method: &str) -> Vec<Value> {
        self.calls(method).into_iter().filter(|c| c["rejected"].is_null()).collect()
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.state.locked().calls.iter().filter(|(m, _)| m == method).map(|(_, b)| b.clone()).collect()
    }

    /// The text of every sendMessage that answered (not the working line).
    fn texts(&self) -> Vec<String> {
        self.calls("sendMessage")
            .iter()
            .filter(|c| c["rejected"].is_null())
            .filter_map(|c| c["text"].as_str())
            .filter(|t| *t != "working…")
            .map(str::to_owned)
            .collect()
    }

    fn answers(&self, token: &str) -> usize {
        self.texts().iter().filter(|t| t.contains("echo:") && t.contains(token)).count()
    }

    /// Every call so far, in order: (method, body).
    fn order(&self) -> Vec<(String, Value)> {
        self.state.locked().calls.clone()
    }

    fn logins(&self) -> usize {
        self.state.locked().logins
    }
}

// ---- the rig ----

struct Rig {
    dir: PathBuf,
    paths: Paths,
    core: Arc<Core>,
    bot: Arc<Bot>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn new_core(paths: &Paths) -> Arc<Core> {
    let settings = Settings {
        debounce: Duration::from_millis(100),
        hold: Duration::from_millis(400),
        poll: Duration::from_millis(40),
        ..Settings::default()
    };
    let gate = Gate::new(GateSettings::unlimited(), Box::new(memory::System));
    Core::new(core::Deps {
        paths: paths.clone(),
        settings,
        gate,
        judge: Judge::fixed(None, paths.clone()),
        backups: Arc::new(crate::agent::backups::Backups::new(paths.data.join("backups.json"))),
    })
}

impl Rig {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-telegram-{}", uuid::Uuid::new_v4()));
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
            ("my-agent", "description: Hyphenated\n"),
        ] {
            std::fs::create_dir_all(dir.join("agents").join(name)).unwrap();
            std::fs::write(dir.join("agents").join(name).join("agent.md"), card).unwrap();
        }
        let files = [
            (
                "people",
                json!({format!("telegram:{KAI}"): {"person": "kai", "owner": true, "was": []},
                       format!("telegram:{GUEST}"): {"person": "guest", "owner": false, "was": []}}),
            ),
            ("access", json!({"kai": {"owner": true}, "guest": {"owner": false, "agents": ["shop"]}})),
        ];
        for (name, v) in files {
            std::fs::write(stub.join(format!("{name}.json")), v.to_string()).unwrap();
        }
        let paths = Paths {
            hq_bin: bin.join("kith"),
            auth_bin: bin.join("kith-auth"),
            machine: dir.join("machine"),
            agent_roots: vec![dir.join("agents")],
            data: dir.join("data"),
        };
        let core = new_core(&paths);
        Self { dir, paths, core, bot: Bot::start().await }
    }

    fn telegram(&self, core: &Arc<Core>) -> Telegram {
        let mut tg = Telegram::new("telegram/hq", TOKEN, &self.bot.url, core.clone());
        tg.pace.conflict = Duration::from_millis(60);
        tg.pace.error = Duration::from_millis(60);
        tg.pace.long_poll_s = 1;
        tg
    }

    /// Runs the adapter's `start` on its own.
    fn run(&self) -> (Arc<Telegram>, JoinHandle<anyhow::Result<()>>) {
        self.run_with(|_| {})
    }

    fn run_with(&self, tune: impl FnOnce(&mut Telegram)) -> (Arc<Telegram>, JoinHandle<anyhow::Result<()>>) {
        let mut tg = self.telegram(&self.core);
        tune(&mut tg);
        let tg = Arc::new(tg);
        let task = tokio::spawn(tg.clone().start());
        (tg, task)
    }

    fn hq_calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.join("stub/calls.jsonl")).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|c| c["tool"] == "kith").collect()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.paths.data.join("state.json")).unwrap()).unwrap()
    }
}

fn dm(rig: &Rig, text: &str) -> (i64, i64) {
    rig.bot.say(KAI, DM, text, 0, &json!({}))
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_gets_one_answer_and_is_not_late() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    let (_, mid) = dm(&rig, "@house msg-live");
    until("the answer", || rig.bot.answers("msg-live") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.bot.answers("msg-live"), 1);
    let answer = rig.bot.calls("sendMessage").into_iter().find(|c| c["parse_mode"] == "HTML").unwrap();
    assert!(answer["text"].as_str().unwrap().starts_with("<b>Home</b>\necho:msg-live"), "{answer}");
    assert_eq!(answer["reply_parameters"]["message_id"], mid);
    assert_eq!(answer["chat_id"], DM);
    let tasks: Vec<String> = rig.hq_calls().iter().map(|c| c["task"].as_str().unwrap().to_owned()).collect();
    assert!(tasks.iter().all(|t| !t.contains("picked it up late")), "{tasks:?}");
    // The working line came and went, and the / menu was set with the agents.
    until("the working line to go", || !rig.bot.calls("deleteMessage").is_empty()).await;
    let menu = &rig.bot.calls("setMyCommands")[0]["commands"];
    let names: Vec<&str> = menu.as_array().unwrap().iter().map(|c| c["command"].as_str().unwrap()).collect();
    assert_eq!(names, ["house", "my_agent", "personal", "shop", "agents", "default", "stop", "whoami"]);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_sent_while_down_are_answered_once_late_and_confirmed() {
    let rig = Rig::new().await;
    let (_, _) = rig.bot.say(KAI, DM, "@house msg-down-1", 600, &json!({}));
    let (last, _) = rig.bot.say(KAI, DM, "@house msg-down-2", 300, &json!({}));
    let (_tg, task) = rig.run();
    until("both answers", || rig.bot.answers("msg-down-1") == 1 && rig.bot.answers("msg-down-2") == 1).await;
    let late = rig.hq_calls().iter().filter(|c| c["task"].as_str().unwrap().contains("picked it up late")).count();
    assert_eq!(late, 2, "{:?}", rig.hq_calls());
    assert_eq!(rig.state()["conns"]["telegram/hq"]["offset"], last + 1, "the offset is saved");
    task.abort();

    // A new host reading the same state confirms the offset and answers nothing again.
    let again = Arc::new(rig.telegram(&new_core(&rig.paths)));
    let task = tokio::spawn(again.start());
    until("the poll with the saved offset", || rig.bot.state.locked().offsets.contains(&(last + 1))).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!((rig.bot.answers("msg-down-1"), rig.bot.answers("msg-down-2")), (1, 1));
    assert_eq!(rig.hq_calls().len(), 2);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_poll_is_replaced_by_a_new_connection() {
    let rig = Rig::new().await;
    let mut tg = rig.telegram(&rig.core);
    tg.quiet = Duration::from_secs(3);
    let backoff =
        Backoff { first: Duration::from_millis(50), max: Duration::from_millis(200), healthy: Duration::from_secs(60) };
    let supervised = tokio::spawn(supervise(Arc::new(tg), backoff));
    until("the first login", || rig.bot.logins() == 1).await;
    dm(&rig, "@house msg-before");
    until("the first answer", || rig.bot.answers("msg-before") == 1).await;
    rig.bot.state.locked().hang = true; // a poll that never comes back
    until("a second login", || rig.bot.logins() >= 2).await;
    rig.bot.state.locked().hang = false;
    dm(&rig, "@house msg-after");
    until("the answer after", || rig.bot.answers("msg-after") == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((rig.bot.answers("msg-after"), rig.bot.answers("msg-before")), (1, 1));
    supervised.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_to_a_tasks_message_continues_it() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    let (_, first) = dm(&rig, "@house start");
    until("the first answer", || rig.bot.answers("start") == 1).await;
    until("the answer to be linked", || rig.state()["replies"].as_object().is_some_and(|r| r.len() >= 2)).await;
    // Reply to the answer (a message hq posted), with no @agent.
    let reply_key = rig.state()["replies"]
        .as_object()
        .unwrap()
        .keys()
        .find(|k| !k.ends_with(&format!(":{first}")))
        .unwrap()
        .clone();
    let answer_mid: i64 = reply_key.rsplit(':').next().unwrap().parse().unwrap();
    rig.bot.say(
        KAI,
        DM,
        "msg-more",
        0,
        &json!({"reply_to_message": {"message_id": answer_mid, "from": {"id": BOT_ID, "is_bot": true}, "text": "x"}}),
    );
    until("the second answer", || rig.bot.answers("msg-more") == 1).await;
    // And to the user's own first message.
    rig.bot.say(
        KAI,
        DM,
        "msg-own",
        0,
        &json!({"reply_to_message": {"message_id": first, "from": {"id": KAI}, "text": "@house start"}}),
    );
    until("the third answer", || rig.bot.answers("msg-own") == 1).await;
    let calls = rig.hq_calls();
    assert!(calls[0]["resume"].is_null());
    assert_eq!(calls[1]["resume"], "sess-house-1");
    assert_eq!(calls[2]["resume"], "sess-house-2");
    assert_eq!(calls[1]["agent"], "house");
    // The answer to the newest message replies to it.
    let last = rig.bot.calls("sendMessage").into_iter().rfind(|c| c["parse_mode"] == "HTML").unwrap();
    assert!(last["reply_parameters"]["message_id"].as_i64().unwrap() > answer_mid);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_and_mentions_address_agents_in_groups() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    rig.bot.say(KAI, GROUP, "/house@hqbot msg-cmd", 0, &json!({}));
    until("the command answer", || rig.bot.answers("msg-cmd") == 1).await;
    // Hyphenated agents are commands with an underscore.
    rig.bot.say(KAI, GROUP, "/my_agent msg-hyphen", 0, &json!({}));
    until("the hyphen answer", || rig.bot.answers("msg-hyphen") == 1).await;
    assert_eq!(rig.hq_calls()[1]["agent"], "my-agent");
    // The bot's mention, anywhere in the text, with an @agent.
    rig.bot.say(KAI, GROUP, "hey @HQBOT @shop msg-mention", 0, &json!({}));
    until("the mention answer", || rig.bot.answers("msg-mention") == 1).await;
    let tasks: Vec<String> = rig.hq_calls().iter().map(|c| c["task"].as_str().unwrap().to_owned()).collect();
    assert!(tasks[0].starts_with("msg-cmd"), "the command's @bot is stripped: {tasks:?}");
    assert!(
        tasks[2].starts_with("hey  @shop msg-mention") || tasks[2].starts_with("hey @shop msg-mention"),
        "{tasks:?}"
    );
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_not_meant_for_the_bot_are_ignored() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    let b = &rig.bot;
    b.say(KAI, GROUP, "/house@otherbot msg-other", 0, &json!({}));
    b.say(KAI, GROUP, "just talking, msg-chatter", 0, &json!({}));
    b.say(KAI, GROUP, "@house msg-nomention", 0, &json!({}));
    b.say(7, DM, "msg-bot", 0, &json!({"from": {"id": 7, "is_bot": true}}));
    b.say(KAI, GROUP, "@hqbot @house msg-anon", 0, &json!({"sender_chat": {"id": -5}}));
    b.push_update(&json!({"edited_message": {"message_id": 5, "from": {"id": KAI}, "chat": {"id": DM, "type": "private"}, "text": "@house msg-edit"}}));
    // A reply to somebody else's message is quoted to the agent.
    b.say(
        KAI,
        GROUP,
        "@hqbot @house msg-quote",
        0,
        &json!({"reply_to_message": {"message_id": 3, "from": {"id": 55}, "text": "what the neighbour said"}}),
    );
    until("the real one", || b.answers("msg-quote") == 1).await;
    let calls = rig.hq_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0]["task"].as_str().unwrap().starts_with("(Replying to: \"what the neighbour said\")"), "{calls:?}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_who_takes_it_and_pressing_a_button_twice() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    // A group has no default agent: the bot asks.
    let (_, mid) = rig.bot.say(KAI, GROUP, "@hqbot msg-ask", 0, &json!({}));
    until("the question", || !rig.bot.calls("sendMessage").is_empty()).await;
    let ask = rig.bot.calls("sendMessage").remove(0);
    assert_eq!(ask["text"], "Who should take this?");
    assert_eq!(ask["reply_parameters"]["message_id"], mid);
    let rows = ask["reply_markup"]["inline_keyboard"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "three buttons to a row: {ask}");
    let data = rows[0][0]["callback_data"].as_str().unwrap().to_owned();
    let (pid, agent) = data.strip_prefix("pick:").unwrap().split_once(':').unwrap();
    assert_eq!(agent, "house");
    // Somebody else's press is refused.
    rig.bot.press(GUEST, GROUP, 4, &data);
    until("the refusal", || rig.bot.calls("answerCallbackQuery").len() == 1).await;
    assert_eq!(rig.bot.calls("answerCallbackQuery")[0]["text"], "Only the person who asked can do that.");
    assert_eq!(rig.hq_calls().len(), 0);
    // The asker's press runs the task, once.
    rig.bot.press(KAI, GROUP, 4, &data);
    until("the answer", || rig.bot.answers("msg-ask") == 1).await;
    assert_eq!(rig.hq_calls()[0]["agent"], "house");
    assert_eq!(rig.bot.calls("editMessageText")[0]["text"], "→ house");
    // Pressed again: nothing runs, and it is refused.
    rig.bot.press(KAI, GROUP, 4, &format!("pick:{pid}:house"));
    until("the second refusal", || rig.bot.calls("answerCallbackQuery").len() == 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.hq_calls().len(), 1);
    assert_eq!(rig.bot.calls("answerCallbackQuery")[2]["text"], "Only the person who asked can do that.");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stop_button_stops_the_work() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    let (_, mid) = dm(&rig, "@house SLOW work");
    until("the working line", || rig.bot.calls("sendMessage").iter().any(|c| c["text"] == "working…")).await;
    let line = rig.bot.calls("sendMessage").into_iter().find(|c| c["text"] == "working…").unwrap();
    assert_eq!(line["disable_notification"], true);
    assert_eq!(line["reply_parameters"]["message_id"], mid);
    let button = &line["reply_markup"]["inline_keyboard"][0][0];
    assert_eq!(button["text"], "Stop");
    let data = button["callback_data"].as_str().unwrap().to_owned();
    until("the run", || !rig.hq_calls().is_empty()).await;
    let line_id = line["sent_id"].as_i64().unwrap();
    // Another person's press is refused and stops nothing.
    rig.bot.press(GUEST, DM, line_id, &data);
    until("the refusal", || rig.bot.calls("answerCallbackQuery").len() == 1).await;
    assert_eq!(rig.bot.calls("answerCallbackQuery")[0]["text"], "Only the person who asked can do that.");
    // Kai's press stops it: "stopping…", then "stopped" on the same line.
    rig.bot.press(KAI, DM, line_id, &data);
    until("stopped", || rig.bot.calls("editMessageText").iter().any(|c| c["text"] == "<i>stopped</i>")).await;
    until("the press answer", || rig.bot.calls("answerCallbackQuery").len() == 2).await;
    let edits: Vec<String> =
        rig.bot.calls("editMessageText").iter().map(|c| c["text"].as_str().unwrap().to_owned()).collect();
    // The run ending races the press: "stopping…" may be skipped, but never lands after "stopped".
    assert_eq!(edits.last().map(String::as_str), Some("<i>stopped</i>"), "{edits:?}");
    assert_eq!(rig.bot.calls("answerCallbackQuery")[1].get("text"), None);
    // Telegram is answered before the slower edit.
    let order = rig.bot.order();
    let answered =
        order.iter().enumerate().filter(|(_, (m, _))| m == "answerCallbackQuery").map(|(i, _)| i).nth(1).unwrap();
    let stopping = order.iter().position(|(m, b)| m == "editMessageText" && b["text"] == "<i>stopping…</i>");
    if let Some(stopping) = stopping {
        assert!(answered < stopping, "{order:?}");
    }
    // Pressed after it ended: nothing to stop, still no error.
    rig.bot.press(KAI, DM, line_id, &data);
    until("the last answer", || rig.bot.calls("answerCallbackQuery").len() == 3).await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn html_telegram_refuses_is_sent_plain() {
    let rig = Rig::new().await;
    rig.bot.state.locked().reject_html = true;
    let (_tg, task) = rig.run();
    dm(&rig, "@house msg-plain **bold**");
    until("the plain answer", || rig.bot.answers("msg-plain") == 1).await;
    let sent = rig.bot.calls("sendMessage");
    let tried = sent.iter().find(|c| c["parse_mode"] == "HTML").unwrap();
    assert!(tried["text"].as_str().unwrap().contains("<b>Home</b>"));
    let plain =
        sent.iter().find(|c| c["parse_mode"].is_null() && c["text"].as_str().unwrap().contains("echo:")).unwrap();
    assert!(plain["text"].as_str().unwrap().starts_with("Home\necho:msg-plain **bold**"), "{plain}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_come_in_and_go_out() {
    let rig = Rig::new().await;
    rig.bot.state.locked().files.insert("doc1".into(), b"the notes".to_vec());
    rig.bot.state.locked().files.insert("pic9".into(), b"png bytes".to_vec());
    let (tg, task) = rig.run();
    rig.bot.say(
        KAI,
        DM,
        "",
        0,
        &json!({"text": null, "caption": "@house msg-file", "document": {"file_id": "doc1", "file_name": "../notes.txt"},
                "photo": [{"file_id": "small"}, {"file_id": "pic9"}]}),
    );
    until("the answer", || rig.bot.answers("msg-file") == 1).await;
    let task_text = rig
        .hq_calls()
        .into_iter()
        .map(|c| c["task"].as_str().unwrap().to_owned())
        .find(|t| t.contains("msg-file"))
        .unwrap();
    let paths: Vec<&str> =
        task_text.split("Attached files: ").nth(1).unwrap().split("\n\n(From").next().unwrap().split(", ").collect();
    let folder = rig.core.attachments_dir("telegram/hq");
    assert_eq!(paths.len(), 2, "{task_text}");
    for p in &paths {
        assert!(std::path::Path::new(p).starts_with(&folder), "inside the attachments folder: {p}");
    }
    let doc = paths.iter().find(|p| p.ends_with("/notes.txt")).unwrap();
    assert_eq!(std::fs::read(doc).unwrap(), b"the notes");
    assert!(paths.iter().any(|p| p.ends_with("/photo.txt")), "named after its kind: {paths:?}");

    // Files out: a picture as a photo, the rest as documents; the caption on the first only.
    let outdir = rig.dir.join("out");
    std::fs::create_dir_all(&outdir).unwrap();
    for (n, c) in [("a.png", "image"), ("b.pdf", "doc")] {
        std::fs::write(outdir.join(n), c).unwrap();
    }
    let files: Vec<String> =
        ["a.png", "missing.png", "b.pdf"].iter().map(|n| outdir.join(n).to_string_lossy().into_owned()).collect();
    let conv = json!({"chat": DM, "reply_to": 7, "key": "telegram/hq:100:7"});
    rig.bot.state.locked().rate_limit = 1; // the first upload is told to wait, then goes through
    tg.send_files(&conv, "house", &files, "see this").await.unwrap();
    assert_eq!(rig.bot.calls("sendPhoto").len(), 2, "the photo was sent again after the 429");
    let (photos, docs) = (rig.bot.accepted("sendPhoto"), rig.bot.accepted("sendDocument"));
    assert_eq!((photos.len(), docs.len()), (1, 1));
    let raw = photos[0]["raw"].as_str().unwrap();
    assert!(
        raw.contains("name=\"photo\"; filename=\"a.png\"") && raw.contains("Home: see this") && raw.contains("image"),
        "{raw}"
    );
    assert!(raw.contains("name=\"chat_id\"\r\n\r\n100") && raw.contains("allow_sending_without_reply"));
    let raw = docs[0]["raw"].as_str().unwrap();
    assert!(raw.contains("name=\"document\"; filename=\"b.pdf\""), "{raw}");
    assert!(!raw.contains("see this"), "the caption is on the first file only");
    // What it sent belongs to the thread: replying to it continues the task.
    let linked = rig.state()["replies"].as_object().unwrap().values().filter(|k| *k == "telegram/hq:100:7").count();
    assert!(linked >= 2, "{}", rig.state()["replies"]);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_state_continues_where_the_bridge_stopped() {
    let rig = Rig::new().await;
    // The bridge's file, as the retired Python bridge wrote it: offset 41, a task in chat 100 started by message 5, answered by 7.
    let bridge = json!({
        "version": 2,
        "conns": {"telegram/hq": {"offset": 41}},
        "rooms": {"telegram/hq:100": {"default": "house"}},
        "threads": {"telegram/hq:100:5": {"person": "kai", "owner": "house", "last": "house", "room": "telegram/hq:100",
            "sessions": {"house": "bridge-session"}, "conv": {"chat": 100, "reply_to": 5, "key": "telegram/hq:100:5"}}},
        "replies": {"telegram/hq:100:5": "telegram/hq:100:5", "telegram/hq:100:7": "telegram/hq:100:5"}
    });
    std::fs::create_dir_all(rig.paths.bridge_state().parent().unwrap()).unwrap();
    std::fs::write(rig.paths.bridge_state(), bridge.to_string()).unwrap();
    assert!(rig.core.import_bridge("telegram/hq").is_some());
    // Update 40 was handled by the bridge; 41 is a reply to its answer 7.
    rig.bot.say(KAI, DM, "msg-handled-by-the-bridge", 0, &json!({}));
    rig.bot.say(
        KAI,
        DM,
        "msg-after-cutover",
        0,
        &json!({"reply_to_message": {"message_id": 7, "from": {"id": BOT_ID, "is_bot": true}, "text": "old answer"}}),
    );
    let (_tg, task) = rig.run();
    until("the answer", || rig.bot.answers("msg-after-cutover") == 1).await;
    assert_eq!(rig.bot.state.locked().offsets[0], 41, "polling starts at the bridge's offset");
    assert_eq!(rig.bot.answers("msg-handled-by-the-bridge"), 0);
    let calls = rig.hq_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0]["agent"].as_str(), calls[0]["resume"].as_str()), (Some("house"), Some("bridge-session")));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_poller_and_rate_limits_are_waited_out() {
    let rig = Rig::new().await;
    {
        let mut st = rig.bot.state.locked();
        st.conflict = 3;
        st.rate_limit = 1;
    }
    let (_tg, task) = rig.run();
    dm(&rig, "@house msg-conflict");
    until("the answer after the 409s and the 429", || rig.bot.answers("msg-conflict") == 1).await;
    let polls = rig.bot.state.locked().offsets.len();
    assert!(polls >= 4, "it kept trying: {polls}");
    assert!(!task.is_finished(), "a 409 does not end the connection");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_token_ends_the_connection() {
    let rig = Rig::new().await;
    let tg = Arc::new(Telegram::new("telegram/hq", "other:token", &rig.bot.url, rig.core.clone()));
    // The fake only answers its own token, with 401.
    let ended = tokio::time::timeout(Duration::from_secs(10), tg.start()).await.unwrap();
    assert!(ended.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reactions_typing_and_the_menu_refresh() {
    let rig = Rig::new().await;
    let (tg, task) = rig.run();
    until("login", || rig.bot.logins() == 1).await;
    let m = crate::channels::Incoming { reference: json!({"chat": DM, "message": 12}), ..Default::default() };
    tg.react(&m, Reaction::Stop).await;
    tg.react(&m, Reaction::Redo).await;
    let reactions = rig.bot.calls("setMessageReaction");
    assert_eq!(reactions[0]["reaction"], json!([{"type": "emoji", "emoji": "🫡"}]));
    assert_eq!(reactions[1]["reaction"], json!([{"type": "emoji", "emoji": "✍"}]));
    assert_eq!(reactions[0]["message_id"], 12);

    let conv = json!({"chat": DM});
    let typing = tokio::spawn({
        let tg = tg.clone();
        async move { tg.typing(&conv).await }
    });
    until("typing", || !rig.bot.calls("sendChatAction").is_empty()).await;
    typing.abort();
    assert_eq!(rig.bot.calls("sendChatAction")[0]["action"], "typing");

    // The menu is set again only when the agents changed.
    let sets = rig.bot.calls("setMyCommands").len();
    tg.set_menu().await;
    assert_eq!(rig.bot.calls("setMyCommands").len(), sets);
    *tg.menu.locked() = json!([]);
    tg.set_menu().await;
    assert_eq!(rig.bot.calls("setMyCommands").len(), sets + 1);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_token_never_shows_in_errors() {
    let api = Api::new("http://127.0.0.1:1", TOKEN);
    let err = api.call("getMe", &json!({}), Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(err, TgError::Net(_)));
    assert!(!err.to_string().contains("TESTTOKENVALUE"), "{err}");
    let dest = std::env::temp_dir().join(format!("kith-tg-{}", uuid::Uuid::new_v4()));
    let err = api.download("docs/x.txt", &dest, 10).await.unwrap_err();
    assert!(!dest.exists());
    assert!(!err.to_string().contains("TESTTOKENVALUE"), "{err}");
}

// ---- plain functions ----

#[test]
fn mentions_and_command_targets() {
    assert_eq!(command_target("/house@HqBot do it").as_deref(), Some("HqBot"));
    assert_eq!(command_target("  /house@x").as_deref(), Some("x"));
    assert_eq!(command_target("/house do it"), None);
    assert_eq!(command_target("hello /house@x"), None);
    assert_eq!(command_target("/@x"), None);
    assert_eq!(without_mention("hey @HqBot, @hqbotter and @hqbot", "hqbot"), "hey , @hqbotter and");
    assert_eq!(without_mention("/house@hqbot go", "hqbot"), "/house go");
}

#[test]
fn escaping_follows_python() {
    assert_eq!(escape("a<b>&\"c'", true), "a&lt;b&gt;&amp;&quot;c&#x27;");
    assert_eq!(escape("a<b>&\"c'", false), "a&lt;b&gt;&amp;\"c'");
}

/// `tests/fixtures/telegram_html.json`: what `tg_html` in the bridge's `telegram_chat.py` makes of each text.
#[test]
fn markdown_becomes_the_same_html_as_the_bridge_made() {
    let golden: Vec<Value> = serde_json::from_str(include_str!("../../../tests/fixtures/telegram_html.json")).unwrap();
    assert!(golden.len() > 20);
    for case in golden {
        let md = case["md"].as_str().unwrap();
        assert_eq!(tg_html(md), case["html"].as_str().unwrap(), "markdown: {md:?}");
    }
}

#[test]
fn code_inside_a_link_text_is_restored() {
    assert_eq!(tg_html("[`code`](https://example.com)"), "<a href=\"https://example.com\"><code>code</code></a>");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chats_with_the_same_message_id_and_file_name_keep_their_own_files() {
    let rig = Rig::new().await;
    {
        let mut st = rig.bot.state.locked();
        st.files.insert("doc_a".into(), b"from chat a".to_vec());
        st.files.insert("doc_b".into(), b"from chat b".to_vec());
    }
    let (_tg, task) = rig.run();
    for (chat, id, tag) in [(DM, "doc_a", "msg-chat-a"), (101, "doc_b", "msg-chat-b")] {
        let extra = json!({"message_id": 777, "document": {"file_id": id, "file_name": "notes.txt"}});
        rig.bot.say(KAI, chat, &format!("@house {tag}"), 0, &extra);
        until("the answer", || rig.bot.answers(tag) == 1).await;
    }
    let mut seen = Vec::new();
    for call in rig.hq_calls() {
        let task = call["task"].as_str().unwrap().to_owned();
        let path = task.split("Attached files: ").nth(1).unwrap().split("\n\n(From").next().unwrap().to_owned();
        seen.push((task.contains("msg-chat-a"), std::fs::read_to_string(&path).unwrap()));
    }
    assert_eq!(seen, [(true, "from chat a".to_owned()), (false, "from chat b".to_owned())]);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_offset_is_confirmed_only_once_the_message_is_queued() {
    let rig = Rig::new().await;
    {
        let mut st = rig.bot.state.locked();
        st.files.insert("slow".into(), b"x".to_vec());
        st.file_delay = Duration::from_millis(700);
    }
    let (_tg, task) = rig.run();
    let extra = json!({"document": {"file_id": "slow", "file_name": "a.txt"}});
    let (uid, _) = rig.bot.say(KAI, DM, "@house msg-slow-file", 0, &extra);
    until("the download to start", || rig.bot.state.locked().file_hits == 1).await;
    // Something else saves the shared state while this message is still being fetched.
    rig.core.with(core::Inner::save);
    let offset = rig.state()["conns"]["telegram/hq"]["offset"].as_i64().unwrap_or(0);
    assert!(offset <= uid, "the offset ran ahead of the unqueued message: {offset}");
    until("the answer", || rig.bot.answers("msg-slow-file") == 1).await;
    until("the offset", || rig.state()["conns"]["telegram/hq"]["offset"] == uid + 1).await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_update_that_hangs_is_given_up_without_restarting_the_connection() {
    let rig = Rig::new().await;
    {
        let mut st = rig.bot.state.locked();
        st.files.insert("slow".into(), b"x".to_vec());
        st.file_delay = Duration::from_secs(30);
    }
    // quiet 2 s: an update may take 1 s.
    let (_tg, task) = rig.run_with(|tg| tg.quiet = Duration::from_secs(2));
    let extra = json!({"document": {"file_id": "slow", "file_name": "a.txt"}});
    let (stuck, _) = rig.bot.say(KAI, DM, "@house msg-stuck", 0, &extra);
    rig.bot.say(KAI, DM, "@house msg-next", 0, &json!({}));
    until("the next message", || rig.bot.answers("msg-next") == 1).await;
    until("the offset past the stuck one", || rig.bot.state.locked().offsets.iter().any(|o| *o > stuck + 1)).await;
    assert_eq!(rig.bot.logins(), 1, "the connection was not restarted");
    assert_eq!(rig.bot.answers("msg-stuck"), 0);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replying_to_the_working_line_goes_on_with_the_task() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run();
    let (_, first) = rig.bot.say(KAI, GROUP, "@hqbot @house SLEEP first", 0, &json!({}));
    until("the working line", || rig.bot.calls("sendMessage").iter().any(|c| c["text"] == "working…")).await;
    let line = loop {
        let keys: Vec<String> = rig.state()["replies"].as_object().unwrap().keys().cloned().collect();
        if let Some(k) = keys.iter().find(|k| !k.ends_with(&format!(":{first}"))) {
            break k.rsplit(':').next().unwrap().parse::<i64>().unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // In a group, a reply to a message of the bot is for it; it must continue the task, not ask who takes it.
    rig.bot.say(
        KAI,
        GROUP,
        "msg-follow",
        0,
        &json!({"reply_to_message": {"message_id": line, "from": {"id": BOT_ID, "is_bot": true}, "text": "working…"}}),
    );
    until("the answer", || rig.bot.answers("msg-follow") >= 1).await;
    assert!(!rig.bot.texts().iter().any(|t| t == "Who should take this?"), "{:?}", rig.bot.texts());
    assert!(rig.hq_calls().iter().all(|c| !c["task"].as_str().unwrap().contains("Replying to")));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_pick_still_answers_the_button_and_runs_the_task() {
    let rig = Rig::new().await;
    let (_tg, task) = rig.run_with(|tg| tg.pace.pick_wait = Duration::ZERO);
    rig.bot.say(KAI, GROUP, "@hqbot msg-slow-pick", 0, &json!({}));
    until("the question", || !rig.bot.calls("sendMessage").is_empty()).await;
    let ask = rig.bot.calls("sendMessage").remove(0);
    let data = ask["reply_markup"]["inline_keyboard"][0][0]["callback_data"].as_str().unwrap().to_owned();
    rig.bot.press(KAI, GROUP, 4, &data);
    until("the answer to the press", || !rig.bot.calls("answerCallbackQuery").is_empty()).await;
    assert_eq!(rig.bot.calls("answerCallbackQuery")[0].get("text"), None);
    until("the task", || rig.bot.answers("msg-slow-pick") == 1).await;
    until("the edit", || rig.bot.calls("editMessageText").iter().any(|c| c["text"] == "→ house")).await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_over_the_limit_are_not_downloaded() {
    let rig = Rig::new().await;
    rig.bot.state.locked().files.insert("unsized".into(), vec![b'x'; 64]);
    let (_tg, task) = rig.run_with(|tg| tg.pace.max_file = 32);
    // Telegram says it is too big: nothing is asked for.
    let big = json!({"document": {"file_id": "huge", "file_name": "big.bin", "file_size": 33}});
    rig.bot.say(KAI, DM, "@house msg-big", 0, &big);
    until("the answer", || rig.bot.answers("msg-big") == 1).await;
    assert_eq!(rig.bot.calls("getFile").len(), 0);
    // It doesn't say, and the download goes over the limit: stopped, nothing kept.
    let unsized_file = json!({"document": {"file_id": "unsized", "file_name": "grows.bin"}});
    rig.bot.say(KAI, DM, "@house msg-grows", 0, &unsized_file);
    until("the second answer", || rig.bot.answers("msg-grows") == 1).await;
    let tasks: Vec<String> = rig.hq_calls().iter().map(|c| c["task"].as_str().unwrap().to_owned()).collect();
    assert!(tasks[0].contains("(file too big: big.bin)") && !tasks[0].contains("Attached files"), "{tasks:?}");
    assert!(tasks[1].contains("(file too big: grows.bin)") && !tasks[1].contains("Attached files"), "{tasks:?}");
    let kept: Vec<_> = walk(&rig.core.attachments_dir("telegram/hq"));
    assert!(kept.is_empty(), "files: {kept:?}");
    task.abort();
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strangers_files_are_not_downloaded() {
    let rig = Rig::new().await;
    rig.bot.state.locked().files.insert("pic".into(), b"x".to_vec());
    let (_tg, task) = rig.run();
    let doc = json!({"document": {"file_id": "pic", "file_name": "a.txt", "file_size": 1}});
    rig.bot.say(777, DM, "/link ABCD1234", 0, &doc);
    rig.bot.say(777, DM, "hello", 0, &doc);
    until("the not-linked reply", || !rig.bot.calls("sendMessage").is_empty()).await;
    assert_eq!(rig.bot.calls("getFile").len(), 0);
    let kept = walk(&rig.core.attachments_dir("telegram/hq"));
    assert!(kept.is_empty(), "{kept:?}");
    task.abort();
}
