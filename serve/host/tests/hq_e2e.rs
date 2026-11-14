//! hq agents as bots, against a stub `hq` and a scripted ACP agent: the bot appears from `hq ls --json`, its first
//! message starts the agent from `hq <agent> --launch-json`, and the session carries hq's prompt and servers and
//! none of Kith's.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Setup {
    host: Host,
    /// The agent's folder.
    home: PathBuf,
    /// Folders the tests pick from (alpha, beta, plain): a browsable root of the host.
    area: PathBuf,
}

async fn start() -> Setup {
    start_with("instructions_agent.py claude").await
}

/// [`start`] with another scripted agent (`fixture [args]`).
async fn start_with(agent: &str) -> Setup {
    let home = std::env::temp_dir().join(format!("kith-hq-e2e-{}", uuid::Uuid::new_v4()));
    start_in(agent, home, &[]).await
}

/// [`start`] for a test that makes `house` an invited person's: kith-auth and the kernel's home are folders of the
/// agent's, never this computer's.
async fn start_for_a_person() -> Setup {
    let home = std::env::temp_dir().join(format!("kith-hq-e2e-{}", uuid::Uuid::new_v4()));
    let (auth, kith) = (home.join("auth.json"), home.join("kith-home"));
    let (auth, kith) = (auth.to_string_lossy(), kith.to_string_lossy());
    start_in("instructions_agent.py claude", home.clone(), &[("KITH_AUTH", &auth), ("KITH_HOME", &kith)]).await
}

async fn start_in(agent: &str, home: PathBuf, env: &[(&str, &str)]) -> Setup {
    let (script, args) = agent.split_once(' ').unwrap_or((agent, ""));
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&home).unwrap();
    let stub = fixtures.join("stub_hq.py");
    let acp = format!("{} {args}", common::python_agent(&fixtures.join(script)));
    let home_text = home.to_string_lossy().into_owned();
    let area = std::env::temp_dir().join(format!("kith-hq-e2e-area-{}", uuid::Uuid::new_v4()));
    for dir in ["alpha", "beta", "plain", ".dot"] {
        std::fs::create_dir_all(area.join(dir)).unwrap();
    }
    let area = area.canonicalize().unwrap();
    let (stub, area_text) = (stub.to_string_lossy(), area.to_string_lossy());
    let mut vars = vec![
        ("KITH_KERNEL_BIN", stub.as_ref()),
        ("STUB_HQ_HOME", home_text.as_str()),
        ("KITH_SERVE_HQ_ACP_COMMAND", acp.as_str()),
        ("KITH_SERVE_FOLDER_ROOTS", area_text.as_ref()),
    ];
    vars.extend(env.iter().copied());
    let host = Host::start_env(&vars).await;
    Setup { host, home, area }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.area);
    }
}

impl Setup {
    /// The roster's `house` bot, once the startup sync made it.
    async fn house(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(bot) = self.host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
                return bot;
            }
            assert!(Instant::now() < deadline, "house never appeared");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn lines(&self, file: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.home.join(file)).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        self.lines("requests.jsonl").into_iter().filter(|r| r["method"] == method).collect()
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.area.join(name)
    }

    /// Registers `alpha` and `beta` (folders of `area`) as hq projects.
    fn register_projects(&self) {
        let one = |id: &str| {
            let path = self.dir(id).to_string_lossy().into_owned();
            json!({"id": id, "paths": [path], "home": path, "remotes": [], "parent": null, "memory": "/state/projects", "exists": true})
        };
        let list = json!({"projects": [one("alpha"), one("beta")]});
        std::fs::write(self.home.join("projects.json"), list.to_string()).unwrap();
    }

    /// The launches of `hq house --launch-json` that ran in `dir`.
    fn launches_in(&self, dir: &Path) -> Vec<Value> {
        self.launches().into_iter().filter(|c| same_dir(c["cwd"].as_str().unwrap(), dir)).collect()
    }

    /// A message in the thread on `root` and the agent's final answer to it.
    async fn thread_reply(&self, bot: &str, root: &str, text: &str) {
        let finals = |entries: &Value| {
            let all = entries["entries"].as_array().unwrap();
            all.iter().filter(|e| e["kind"] == "agent" && e["data"]["final"] == true).count()
        };
        let before = finals(&self.host.call("thread", json!({"botId": bot, "rootId": root})).await);
        let nonce = uuid::Uuid::new_v4().to_string();
        self.host.call("send", json!({"botId": bot, "text": text, "threadId": root, "clientNonce": nonce})).await;
        let deadline = Instant::now() + Duration::from_secs(30);
        while finals(&self.host.call("thread", json!({"botId": bot, "rootId": root})).await) <= before {
            assert!(Instant::now() < deadline, "no answer in the thread to {text:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The `house` bot with its chat as one conversation (no tasks) that has run once, and the message that opens
    /// a thread.
    async fn house_chat(&self) -> (String, String) {
        let id = self.house().await["id"].as_str().unwrap().to_owned();
        self.host.call("updateBot", json!({"id": id, "tasks": false})).await;
        self.host.reply(&id, "hello").await;
        let history = self.host.call("history", json!({"botId": id})).await;
        let user = history["entries"].as_array().unwrap().iter().find(|e| e["kind"] == "user").unwrap().clone();
        (id, user["id"].as_str().unwrap().to_owned())
    }

    fn launches(&self) -> Vec<Value> {
        self.lines("calls.jsonl").into_iter().filter(|c| c["args"][1] == "--launch-json").collect()
    }
}

/// The ACP requests the agent started in `dir` got (the fixture logs into its working folder).
fn requests_in(dir: &Path, method: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join("requests.jsonl")).unwrap_or_default();
    let all = text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap());
    all.filter(|r| r["method"] == method).collect()
}

fn same_dir(a: &str, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

#[tokio::test]
async fn an_hq_agent_appears_and_starts_from_launch_json() {
    let s = start().await;
    let bot = s.house().await;
    assert_eq!(bot["description"], "Keeps the house");
    assert_eq!(bot["backend"], "claude");
    assert_eq!(bot["permission"], "default");
    assert_eq!(s.launches().len(), 0, "nothing starts before the first message");
    let id = bot["id"].as_str().unwrap();
    // These read the main chat's answer; tasks (the default) answer in a thread.
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;

    s.host.reply(id, "what do you keep track of?").await;

    let launches = s.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["via"], "serve");
    assert!(same_dir(launches[0]["cwd"].as_str().unwrap(), &s.home), "run in the bot's folder");

    let new = s.requests("session/new");
    assert_eq!(new.len(), 1);
    let params = &new[0]["params"];
    assert!(same_dir(params["cwd"].as_str().unwrap(), &s.home));
    assert_eq!(params["mcpServers"], json!([]), "no chat, memory, team, routines or connectors servers");
    assert_eq!(params["_meta"]["systemPrompt"], json!({"append": "You are house, the household keeper."}));
    let options = &params["_meta"]["claudeCode"]["options"];
    assert_eq!(options["strictMcpConfig"], true);
    assert_eq!(options["mcpServers"]["pantry"]["command"], "pantry-mcp");
    assert_eq!(options["allowedTools"], json!(["Read", "mcp__kith-files"]), "plus the host's file sharing");
    assert_eq!(options["mcpServers"]["kith-files"]["args"][1], "files");
    assert_eq!(options["extraArgs"], json!({"disable-slash-commands": null}));
    assert_eq!(options["model"], "haiku");
    assert!(options["settings"]["hooks"].is_object());
    assert!(!params.to_string().contains("Kith"), "nothing of the host's own instructions: {params}");

    let prompts = s.requests("session/prompt");
    assert_eq!(prompts[0]["params"]["prompt"][0]["text"], "what do you keep track of?", "no bot profile around it");

    // A second message uses the live session: hq is not asked again.
    s.host.reply(id, "and the shopping list?").await;
    assert_eq!(s.launches().len(), 1);
    assert_eq!(s.requests("session/new").len(), 1);
}

#[tokio::test]
async fn the_host_restart_loads_the_session_with_a_fresh_launch() {
    let mut s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    s.host.reply(&id, "hello").await;
    s.host.restart(|_| {}).await;
    s.house().await;
    // The user's "off" survives the restart's sync (see `hub::hq`): this test reads the main chat.
    assert_eq!(s.house().await["tasks"], false);
    s.host.reply(&id, "again").await;
    assert_eq!(s.launches().len(), 2, "the launch is fetched again");
    let load = s.requests("session/load");
    assert_eq!(load.len(), 1);
    assert_eq!(load[0]["params"]["sessionId"], "s1");
    assert_eq!(load[0]["params"]["_meta"]["systemPrompt"]["append"], "You are house, the household keeper.");
    assert_eq!(s.requests("session/new").len(), 1);
}

/// An invited person's turn compares what their session runs with against what it was loaded with. Unchanged, the warm
/// session takes the turn as it is; changed (a yes on an escape card), that session alone is loaded again with the new
/// grant, in the same agent process, and the other thread's session is left alone.
#[tokio::test]
async fn a_persons_new_grant_loads_their_session_again_in_the_same_process() {
    let mut s = start_for_a_person().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host
        .restart(|dir| {
            let db = rusqlite::Connection::open(dir.join("serve.db")).unwrap();
            db.execute("UPDATE bots SET config = json_set(config, '$.person', 'sam')", []).unwrap();
        })
        .await;
    s.house().await;
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    s.host.reply(&id, "hello").await;
    let history = s.host.call("history", json!({"botId": id})).await;
    let root = history["entries"].as_array().unwrap().iter().find(|e| e["kind"] == "user").unwrap()["id"].clone();
    let root = root.as_str().unwrap().to_owned();
    s.thread_reply(&id, &root, "in the thread").await;
    assert_eq!(s.requests("session/new").len(), 2, "the chat's session and the thread's");

    // Nothing changed: the warm session answers without loading again.
    s.host.reply(&id, "again").await;
    assert!(s.requests("session/load").is_empty(), "{:?}", s.requests("session/load"));

    // A yes on an escape card: the kernel's next launch for sam's conversation carries the folder.
    let granted = s.dir("alpha").to_string_lossy().into_owned();
    std::fs::write(s.home.join("grants.json"), json!({"read": [granted]}).to_string()).unwrap();
    s.host.reply(&id, "now with the folder").await;
    let loads = s.requests("session/load");
    assert_eq!(loads.len(), 1, "{loads:?}");
    assert_eq!(loads[0]["params"]["sessionId"], "s1");
    let settings = &loads[0]["params"]["_meta"]["claudeCode"]["options"]["settings"];
    assert_eq!(settings["sandbox"]["filesystem"]["allowRead"], json!([granted]));
    // The turn ran in the session loaded with it: the load came before the prompt.
    let order: Vec<Value> = s.lines("requests.jsonl");
    let load_at = order.iter().position(|r| r["method"] == "session/load").unwrap();
    let last_prompt = order.iter().rposition(|r| r["method"] == "session/prompt").unwrap();
    assert!(load_at < last_prompt);
    assert_eq!(order[last_prompt]["params"]["sessionId"], "s1");
    // The chat kept its one process all along (a thread lane has its own actor, so its own process), and the thread's
    // process neither loaded nor closed a session.
    let inits: Vec<&Value> = order.iter().filter(|r| r["method"] == "initialize").collect();
    assert_eq!(inits.len(), 2, "the chat's process and the thread's");
    let (chat, thread) = (&inits[0]["pid"], &inits[1]["pid"]);
    assert_eq!(&order[load_at]["pid"], chat);
    assert_eq!(&order[last_prompt]["pid"], chat);
    let in_thread: Vec<&Value> = order.iter().filter(|r| &r["pid"] == thread).collect();
    assert!(in_thread.iter().all(|r| r["method"] != "session/load" && r["method"] != "session/close"), "{in_thread:?}");

    // The grant is the session's now: the next turn doesn't load it again.
    s.host.reply(&id, "and again").await;
    assert_eq!(s.requests("session/load").len(), 1);
}

#[tokio::test]
async fn a_failing_launch_says_why_in_the_chat() {
    let s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    std::fs::write(s.home.join("fail"), "house is not allowed here").unwrap();
    s.host.call("send", json!({"botId": id, "text": "hi", "clientNonce": "n1"})).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let history = s.host.call("history", json!({"botId": id})).await;
        let notice = history["entries"].as_array().unwrap().iter().find(|e| {
            e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("house is not allowed here"))
        });
        if notice.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "no readable error: {history}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(s.requests("session/new").len(), 0, "no fallback to a plain claude");
}

#[tokio::test]
async fn syncing_again_makes_no_second_bot() {
    let s = start().await;
    s.house().await;
    let done = s.host.call("syncHqBots", json!({})).await;
    assert_eq!(done["created"], json!([]));
    let houses = s.host.roster().await.into_iter().filter(|b| b["hqAgent"] == "house").count();
    assert_eq!(houses, 1);
}

#[tokio::test]
async fn a_place_in_a_project_launches_hq_for_it_and_runs_in_its_folder() {
    let s = start().await;
    s.register_projects();
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    let set = s.host.call("setPlaceFolder", json!({"botId": id, "project": "alpha"})).await;
    let alpha = s.dir("alpha");
    assert_eq!(set["folder"]["kind"], "project");
    assert_eq!((set["folder"]["project"].as_str(), set["folder"]["inherited"].as_bool()), (Some("alpha"), Some(false)));
    assert!(same_dir(set["folder"]["path"].as_str().unwrap(), &alpha));

    s.host.reply(&id, "work on alpha").await;

    let launches = s.launches_in(&alpha);
    assert_eq!(launches.len(), 1, "hq runs in the project's folder: {:?}", s.launches());
    let args = launches[0]["args"].as_array().unwrap();
    assert_eq!(&args[..4], [json!("house"), json!("--launch-json"), json!("-p"), json!("alpha")]);
    let new = requests_in(&alpha, "session/new");
    assert_eq!(new.len(), 1);
    assert!(
        same_dir(new[0]["params"]["cwd"].as_str().unwrap(), &alpha),
        "the session works in the folder, not the agent's home"
    );
    // The history read says where the place works.
    let history = s.host.call("history", json!({"botId": id})).await;
    assert_eq!(history["folder"]["project"], "alpha");
}

#[tokio::test]
async fn a_plain_folder_is_the_cwd_and_asks_hq_for_no_project() {
    let s = start().await;
    let (id, _) = s.house_chat().await;
    let plain = s.dir("plain");
    s.host.call("setPlaceFolder", json!({"botId": id, "folder": plain})).await;
    s.host.reply(&id, "in the plain folder").await;

    let launches = s.launches_in(&plain);
    assert_eq!(launches.len(), 1, "{:?}", s.launches());
    assert_eq!(launches[0]["args"], json!(["house", "--launch-json"]), "no -p: hq finds a project from the cwd");
    let new = requests_in(&plain, "session/new");
    assert!(same_dir(new[0]["params"]["cwd"].as_str().unwrap(), &plain));
    assert_eq!(requests_in(&plain, "session/load").len(), 0);
}

#[tokio::test]
async fn changing_a_places_folder_starts_a_fresh_session_for_that_place_only() {
    let s = start().await;
    s.register_projects();
    let (id, root) = s.house_chat().await;
    let (alpha, beta) = (s.dir("alpha"), s.dir("beta"));
    // `house_chat` already ran the chat once in the agent's folder. The chat now works in alpha, the thread keeps
    // the agent's own folder.
    s.host.call("setPlaceFolder", json!({"botId": id, "project": "alpha"})).await;
    s.host.reply(&id, "in alpha").await;
    s.thread_reply(&id, &root, "in the thread").await;
    assert_eq!(requests_in(&s.home, "session/new").len(), 2, "the chat's first session, and the thread's");
    assert_eq!(requests_in(&alpha, "session/new").len(), 1);

    // Same value again: nothing restarts.
    s.host.call("setPlaceFolder", json!({"botId": id, "project": "alpha"})).await;
    s.host.reply(&id, "still alpha").await;
    assert_eq!(s.launches_in(&alpha).len(), 1);
    assert_eq!(requests_in(&alpha, "session/new").len(), 1);

    // Another project: a fresh session in beta, the old one never loaded.
    let moved = s.host.call("setPlaceFolder", json!({"botId": id, "project": "beta"})).await;
    assert_eq!(moved["folder"]["project"], "beta");
    s.host.reply(&id, "now beta").await;
    assert_eq!(requests_in(&beta, "session/new").len(), 1);
    assert_eq!(requests_in(&beta, "session/load").len() + requests_in(&alpha, "session/load").len(), 0);
    assert_eq!(s.launches_in(&beta)[0]["args"], json!(["house", "--launch-json", "-p", "beta"]));

    // The thread was left alone: same agent process, same session.
    s.thread_reply(&id, &root, "thread again").await;
    assert_eq!(requests_in(&s.home, "session/new").len(), 2);
    assert_eq!(requests_in(&s.home, "session/load").len(), 0);

    // Clearing the pick goes back to the agent's folder, and starts afresh there too.
    let cleared = s.host.call("setPlaceFolder", json!({"botId": id, "clear": true})).await;
    assert_eq!(cleared["folder"]["inherited"], true);
    s.host.reply(&id, "back home").await;
    assert_eq!(requests_in(&s.home, "session/new").len(), 3, "the chat's new session");
    assert_eq!(requests_in(&s.home, "session/load").len(), 0);
}

#[tokio::test]
async fn the_bots_default_project_resets_only_the_places_that_follow_it() {
    let s = start().await;
    s.register_projects();
    let (id, root) = s.house_chat().await;
    let (alpha, beta) = (s.dir("alpha"), s.dir("beta"));
    s.thread_reply(&id, &root, "thread").await;
    // The chat picked beta itself; the thread follows the bot.
    s.host.call("setPlaceFolder", json!({"botId": id, "project": "beta"})).await;
    s.host.reply(&id, "beta").await;

    s.host.call("updateBot", json!({"id": id, "project": "alpha"})).await;
    s.thread_reply(&id, &root, "thread in alpha").await;
    assert_eq!(requests_in(&alpha, "session/new").len(), 1, "the thread follows the new default");
    assert_eq!(requests_in(&alpha, "session/load").len(), 0);
    assert_eq!(s.launches_in(&alpha)[0]["args"], json!(["house", "--launch-json", "-p", "alpha"]));
    let folder = s.host.call("thread", json!({"botId": id, "rootId": root})).await["folder"].clone();
    assert_eq!((folder["project"].as_str(), folder["inherited"].as_bool()), (Some("alpha"), Some(true)));
    s.host.reply(&id, "still beta").await;
    assert_eq!(requests_in(&beta, "session/new").len(), 1, "the chat kept its session");
    assert_eq!(requests_in(&beta, "session/load").len(), 0);
}

#[tokio::test]
async fn a_project_without_a_folder_here_falls_back_with_a_notice_and_still_passes_p() {
    let s = start().await;
    let gone = s.dir("gone");
    let entry = json!({"id": "lost", "paths": [gone], "home": null, "remotes": [], "parent": null, "memory": "/state/projects/lost", "exists": false});
    std::fs::write(s.home.join("projects.json"), json!({"projects": [entry]}).to_string()).unwrap();
    let (id, _) = s.house_chat().await;
    s.host.call("setPlaceFolder", json!({"botId": id, "project": "lost"})).await;
    s.host.reply(&id, "somewhere").await;
    let launch = s.launches().into_iter().rfind(|c| c["args"].as_array().unwrap().contains(&json!("lost"))).unwrap();
    assert_eq!(&launch["args"].as_array().unwrap()[2..], [json!("-p"), json!("lost")]);
    assert!(same_dir(launch["cwd"].as_str().unwrap(), &s.home), "the bot's own folder");
    let history = s.host.call("history", json!({"botId": id})).await;
    let said = |e: &Value| {
        e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("no folder on this computer"))
    };
    assert!(history["entries"].as_array().unwrap().iter().any(said), "{history}");
}

#[tokio::test]
async fn pickers_list_projects_recent_folders_and_browsable_directories() {
    let s = start().await;
    s.register_projects();
    let (id, _) = s.house_chat().await;

    // The startup sync lists the projects in the background and the host reuses that list for a few seconds, so
    // whether it ran before `register_projects` is a race; a refresh is what the picker asks for on opening.
    let projects = s.host.call("projects", json!({"refresh": true})).await;
    let ids: Vec<&str> = projects["projects"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["alpha", "beta"]);
    assert_eq!(projects["projects"][0]["exists"], true);
    assert!(same_dir(projects["projects"][0]["path"].as_str().unwrap(), &s.dir("alpha")));

    s.host.call("setPlaceFolder", json!({"botId": id, "folder": s.dir("plain")})).await;
    s.host.call("setPlaceFolder", json!({"botId": id, "project": "beta"})).await;
    let recent = s.host.call("recentFolders", json!({})).await["folders"].clone();
    assert_eq!((recent[0]["name"].as_str(), recent[0]["project"].as_str()), (Some("beta"), Some("beta")));
    assert_eq!((recent[1]["name"].as_str(), recent[1]["project"].as_str()), (Some("plain"), None));
    std::fs::remove_dir_all(s.dir("plain")).unwrap();
    let after = s.host.call("recentFolders", json!({})).await;
    assert_eq!(after["folders"].as_array().unwrap().len(), 1, "a deleted folder drops out");

    let listing = s.host.call("listDirs", json!({"path": s.area})).await;
    let names: Vec<&str> = listing["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["alpha", "beta"], "dot folders stay out: {listing}");
    assert_eq!(listing["dirs"][0]["project"], "alpha");
    assert_eq!(listing["dirs"][1]["project"], "beta");
    assert!(listing["roots"].as_array().unwrap().iter().any(|r| same_dir(r["path"].as_str().unwrap(), &s.area)));
    let hidden = s.host.call("listDirs", json!({"path": s.area, "showHidden": true})).await;
    assert!(hidden["dirs"].as_array().unwrap().iter().any(|d| d["name"] == ".dot"));

    let outside = s.host.refused("listDirs", json!({"path": "/etc"})).await;
    assert!(outside.contains("outside"), "{outside}");
    let climb = s.dir("alpha").join("..").join("..").join("..");
    let escape = s.host.refused("listDirs", json!({"path": climb})).await;
    assert!(escape.contains("outside"), "{escape}");
    let refused = s.host.refused("setPlaceFolder", json!({"botId": id, "folder": "/etc"})).await;
    assert!(refused.contains("outside"), "{refused}");
    let unknown = s.host.refused("setPlaceFolder", json!({"botId": id, "project": "nope"})).await;
    assert!(unknown.contains("unknown project"), "{unknown}");
}

#[tokio::test]
async fn registering_a_project_runs_hq_project_add_and_passes_its_error_through() {
    let s = start().await;
    let added = s.host.call("registerProject", json!({"id": "gamma", "path": s.dir("plain")})).await;
    assert_eq!(added["project"]["id"], "gamma");
    assert!(same_dir(added["project"]["path"].as_str().unwrap(), &s.dir("plain")));
    let again = s.host.refused("registerProject", json!({"id": "gamma", "path": s.dir("plain")})).await;
    assert!(again.contains("already exists"), "hq's own words: {again}");
    let bad_id = s.host.refused("registerProject", json!({"id": "Not Kebab", "path": s.dir("plain")})).await;
    assert!(bad_id.contains("project id"), "{bad_id}");
    let outside = s.host.refused("registerProject", json!({"id": "etc", "path": "/etc"})).await;
    assert!(outside.contains("outside"), "{outside}");
}

#[tokio::test]
async fn a_new_task_starts_in_the_folder_the_message_picked() {
    let s = start().await;
    s.register_projects();
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    // Tasks are on by default for hq bots too: the message opens a thread, which runs in alpha from its first turn.
    let sent =
        s.host.call("send", json!({"botId": id, "text": "a task", "clientNonce": "t1", "project": "alpha"})).await;
    assert_eq!(sent["entry"]["data"]["task"], true, "an hq bot's message starts a task: {sent}");
    let task = sent["entry"]["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    while requests_in(&s.dir("alpha"), "session/prompt").is_empty() {
        assert!(Instant::now() < deadline, "the task never ran in alpha");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(requests_in(&s.dir("alpha"), "session/new").len(), 1);
    let thread = s.host.call("thread", json!({"botId": id, "rootId": task})).await;
    assert_eq!(thread["folder"]["project"], "alpha", "{thread}");
    let main = s.host.call("history", json!({"botId": id})).await;
    assert_eq!(main["folder"]["inherited"], true, "the chat itself keeps the agent's folder");
}

#[tokio::test]
async fn without_hq_projects_is_empty_and_says_why() {
    let host = Host::start().await;
    let answer = host.call("projects", json!({})).await;
    assert_eq!(answer["projects"], json!([]));
    assert!(answer["error"].as_str().is_some_and(|e| e.contains("kith")), "{answer}");
}

#[tokio::test]
async fn moving_a_task_thread_to_another_project_after_its_turn_never_stalls_the_host() {
    let s = start().await;
    s.register_projects();
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": true})).await;
    let sent =
        s.host.call("send", json!({"botId": id, "text": "a task", "clientNonce": "t1", "project": "alpha"})).await;
    let task = sent["entry"]["id"].as_str().unwrap().to_owned();
    // The task's first turn is over: the folder moves after a turn, not during one.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let thread = s.host.call("thread", json!({"botId": id, "rootId": task})).await;
        let answered =
            thread["entries"].as_array().unwrap().iter().any(|e| e["kind"] == "agent" && e["data"]["final"] == true);
        if answered {
            break;
        }
        assert!(Instant::now() < deadline, "the task never answered in alpha");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // /health must answer through all of it.
    let health = format!("{}/health", s.host.base());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let watch = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut stopped = stopped;
        while stopped.try_recv().is_err() {
            let answered = client.get(&health).timeout(Duration::from_secs(5)).send().await;
            assert!(answered.is_ok_and(|r| r.status().is_success()), "/health stopped answering");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let work = async {
        s.host.call("setPlaceFolder", json!({"botId": id, "threadId": task, "project": "beta"})).await;
        s.thread_reply(&id, &task, "now in beta").await;
        s.host.call("setPlaceFolder", json!({"botId": id, "threadId": task, "project": "alpha"})).await;
        s.host.call("setPlaceFolder", json!({"botId": id, "threadId": task, "project": "beta"})).await;
        s.thread_reply(&id, &task, "beta again").await;
    };
    tokio::time::timeout(Duration::from_secs(60), work).await.expect("the host stalled");
    let _ = stop.send(());
    watch.await.unwrap();
}

/// What an app does for an hq bot's permission: `kithSetPermission` goes through to hq, and hq's value is read back.
#[tokio::test]
async fn permission_is_written_through_to_hq_and_read_back() {
    let s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    // hq bots start with tasks on; this test reads the main chat.
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    let before = s.host.call("kithInfo", json!({"botId": id})).await;
    assert_eq!(before["info"]["permission_mode"], "auto");

    let got = s.host.call("kithSetPermission", json!({"botId": id, "mode": "plan"})).await;
    assert_eq!(got["info"]["permission_mode"], "plan", "the answer is hq's info, read back");
    assert_eq!(std::fs::read_to_string(s.home.join("permission")).unwrap(), "plan", "hq holds it");
    let again = s.host.call("kithInfo", json!({"botId": id})).await;
    assert_eq!(again["info"]["permission_mode"], "plan", "another device's next read sees hq's value, not a cache");

    // The next turn starts the agent from hq's launch, which carries that mode and nothing the host chose.
    s.host.reply(&id, "hello").await;
    assert_eq!(s.launches().len(), 1);
    let new = s.requests("session/new");
    assert_eq!(new[0]["params"]["_meta"]["claudeCode"]["options"]["settings"]["permissions"]["defaultMode"], "plan");

    // Changed by hand in hq: the app shows it on its next fetch.
    std::fs::write(s.home.join("permission"), "default").unwrap();
    let hand = s.host.call("kithInfo", json!({"botId": id, "refresh": true})).await;
    assert_eq!(hand["info"]["permission_mode"], "default");

    // An empty mode forgets this Mac's choice; a mode hq refuses is an error, and hq keeps what it had.
    s.host.call("kithSetPermission", json!({"botId": id, "mode": ""})).await;
    assert!(!s.home.join("permission").exists());
    std::fs::write(s.home.join("permission"), "plan").unwrap();
    let why = s.host.refused("kithSetPermission", json!({"botId": id, "mode": "nonsense"})).await;
    assert!(why.contains("not one of"), "{why}");
    assert_eq!(std::fs::read_to_string(s.home.join("permission")).unwrap(), "plan");
}

/// What the editor does for an hq agent's own model, effort and harness: `kithSetSetting` goes through `kith … --json`,
/// kith's refusal comes back as the error, and the answer is the info read back.
#[tokio::test]
async fn model_effort_and_harness_are_written_through_to_kith_and_read_back() {
    let s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    let set = |setting: &str, value: &str| json!({"botId": id, "setting": setting, "value": value});
    let sets = || -> Vec<Value> {
        std::fs::read_to_string(s.home.join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["args"].clone())
            .filter(|a| a[1] == "set")
            .collect()
    };

    let got = s.host.call("kithSetSetting", set("model", "opus")).await;
    assert_eq!(got["info"]["model"], "opus", "the answer is kith's info, read back");
    let got = s.host.call("kithSetSetting", set("effort", "high")).await;
    assert_eq!(got["info"]["effort"], "high");
    assert_eq!(got["info"]["model"], "opus");
    let again = s.host.call("kithSetSetting", set("effort", "high")).await;
    assert_eq!(again["info"]["effort"], "high", "the same value again is not an error");
    assert_eq!(sets()[0], json!(["house", "set", "model", "opus", "--json"]));

    // kith's refusal text, not the command line around it.
    let why = s.host.refused("kithSetSetting", set("effort", "loud")).await;
    assert!(why.contains("is not one of low, medium"), "{why}");
    assert!(!why.contains("failed ("), "{why}");
    let why = s.host.refused("kithSetSetting", set("harness", "codex")).await;
    assert!(why.contains("a Claude model needs a Claude harness"), "{why}");
    let moved = s
        .host
        .call("kithSetSetting", json!({"botId": id, "setting": "harness", "value": "codex", "model": "default"}))
        .await;
    assert_eq!(moved["info"]["harness"], "codex");
    assert!(moved["info"]["model"].is_null());
    assert_eq!(sets().last().unwrap(), &json!(["house", "set", "harness", "codex", "--model", "default", "--json"]));

    s.host.call("kithSetSetting", set("effort", "default")).await;
    let info = s.host.call("kithInfo", json!({"botId": id, "refresh": true})).await;
    assert!(info["info"]["effort"].is_null(), "default forgets it");

    for bad in [set("permission", "plan"), set("model", "--bad"), set("model", "two words"), set("model", "")] {
        s.host.refused("kithSetSetting", bad).await;
    }
}

/// "New bot": `newKithAgent` makes the agent through `kith new … --json` and the settings through `set`, and the answer
/// carries its bot with the look the app picked; `kernelStatus` says the kernel runs.
#[tokio::test]
async fn a_new_bot_is_a_kith_agent_made_through_the_cli() {
    let s = start().await;
    let status = s.host.call("kernelStatus", json!({})).await;
    assert_eq!(status["ok"], true, "{status}");
    assert!(status["problem"].is_null());

    let made = s
        .host
        .call(
            "newKithAgent",
            json!({"name": "trip-pal", "description": "Plans trips", "model": "opus", "effort": "high",
                   "pinned": true, "avatarColor": "#112233"}),
        )
        .await;
    assert_eq!(made["agent"], "trip-pal");
    let id = made["botId"].as_str().expect("the roster has the new agent's bot");
    assert_eq!(made["bot"]["hqAgent"], "trip-pal");
    assert_eq!(made["bot"]["pinned"], true);
    assert_eq!(made["bot"]["avatarColor"], "#112233");
    assert!(s.host.roster().await.iter().any(|b| b["id"] == id), "the roster has the new bot");

    let calls: Vec<Value> = s.lines("calls.jsonl").into_iter().map(|c| c["args"].clone()).collect();
    let pos = |args: Value| calls.iter().position(|c| *c == args).unwrap_or_else(|| panic!("no call {args}"));
    let new = pos(json!(["new", "trip-pal", "Plans trips", "--json"]));
    let model = pos(json!(["trip-pal", "set", "model", "opus", "--json"]));
    let effort = pos(json!(["trip-pal", "set", "effort", "high", "--json"]));
    assert!(new < model && model < effort);

    let why = s.host.refused("newKithAgent", json!({"name": "trip-pal", "description": "again"})).await;
    assert!(why.contains("there is already an agent called"), "{why}");
}

/// A message spoken in a call: its turn gets the voice note and runs on the call model, the chat keeps the spoken
/// words only, and a typed message after it is back on the agent's own model, in the same session.
#[tokio::test]
async fn a_call_turn_gets_the_voice_note_and_the_call_model() {
    let s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    let finals = |entries: &Value| entries.as_array().unwrap().iter().filter(|e| e["data"]["final"] == true).count();
    let say = |text: &'static str, via: Option<&'static str>| {
        let (host, id) = (&s.host, id.clone());
        async move {
            let mut body = json!({"botId": id, "text": text, "clientNonce": uuid::Uuid::new_v4().to_string()});
            if let Some(via) = via {
                body["via"] = via.into();
            }
            let before = finals(&host.call("history", json!({"botId": id})).await["entries"]);
            host.call("send", body).await;
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let entries = host.call("history", json!({"botId": id})).await["entries"].clone();
                if finals(&entries) > before {
                    return entries;
                }
                assert!(Instant::now() < deadline, "no answer to {text:?}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    say("first", None).await;
    let prompts = |s: &Setup| -> Vec<String> {
        s.requests("session/prompt")
            .iter()
            .map(|r| r["params"]["prompt"][0]["text"].as_str().unwrap().to_owned())
            .collect()
    };
    let models = |s: &Setup| -> Vec<String> {
        s.requests("session/set_config_option")
            .iter()
            .filter(|r| r["params"]["configId"] == "model")
            .map(|r| r["params"]["value"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(models(&s).is_empty(), "a typed turn runs on the launch model: {:?}", models(&s));

    let entries = say("what is on today", Some("call")).await;
    let sent = prompts(&s);
    assert!(sent[1].starts_with("what is on today\n\n[Kith: You are on a live voice call"), "{}", sent[1]);
    assert_eq!(models(&s), ["sonnet"], "the call turn runs on the default call model, via the model config option");
    assert!(s.requests("session/set_model").is_empty(), "the adapter has no session/set_model");
    let user = entries.as_array().unwrap().iter().find(|e| e["data"]["text"] == "what is on today").unwrap();
    assert_eq!(user["data"]["text"], "what is on today", "the chat keeps only what was said");

    say("and a typed one", None).await;
    assert_eq!(prompts(&s)[2], "and a typed one", "no note on a typed message");
    assert_eq!(models(&s), ["sonnet", "haiku"], "back on the agent's own model (hq's launch model)");
    assert_eq!(s.launches().len(), 1, "same process and session: nothing relaunched");
    assert_eq!(s.requests("session/new").len(), 1);

    // The app's pick reaches the next call at once; `own` keeps the agent's model, so nothing is switched.
    let got = s.host.call("kithSetCallModel", json!({"botId": id, "model": "opus"})).await;
    assert_eq!(got["info"]["call_model"], "opus");
    say("a spoken one", Some("call")).await;
    assert_eq!(models(&s), ["sonnet", "haiku", "opus"]);
    s.host.call("kithSetCallModel", json!({"botId": id, "model": "own"})).await;
    say("another typed", None).await;
    say("spoken, own model", Some("call")).await;
    assert_eq!(models(&s), ["sonnet", "haiku", "opus", "haiku"], "a call on the agent's own model switches nothing");
    let why = s.host.refused("kithSetCallModel", json!({"botId": id, "model": "--bad"})).await;
    assert!(why.contains("model must be"), "{why}");
}

#[tokio::test]
async fn a_plain_bot_has_no_hq_permission_to_set() {
    let s = start().await;
    let plain = s.host.bot("plain", "ask").await;
    let why = s.host.refused("kithSetPermission", json!({"botId": plain, "mode": "plan"})).await;
    assert!(why.contains("isn't a kith agent"), "{why}");
}

/// The host's own copy of model, effort, Fast mode and permission is not hq's, so it never reaches the launch and an
/// app's edit of it is not stored.
#[tokio::test]
async fn the_hosts_own_copy_never_overrides_hq() {
    let s = start().await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    // hq bots start with tasks on; this test reads the main chat.
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    // Model, effort and Fast mode are refused with the kith command that sets them, not dropped.
    for (patch, command) in [
        (json!({"model": "opus"}), "kith house set model"),
        (json!({"modelOptions": {"effort": "high"}}), "kith house set effort"),
        (json!({"fastMode": true}), "no Fast mode"),
    ] {
        let mut body = patch.clone();
        body["id"] = json!(id);
        let why = s.host.refused("updateBot", body).await;
        assert!(why.contains(command), "{patch}: {why}");
    }
    // The permission is not the host's either, but an edit of it is only left unstored.
    let edited = s.host.call("updateBot", json!({"id": id, "permission": "auto"})).await;
    assert_eq!(edited["bot"]["permission"], "default", "the edit of an hq bot's permission isn't stored");
    assert_eq!(edited["bot"]["fastMode"], false);
    assert!(edited["bot"]["model"].is_null());
    // A full-row echo of what the host shows still goes through.
    let echo = s.host.call("updateBot", edited["bot"].clone()).await;
    assert_eq!(echo["bot"]["permission"], "default");
    assert!(echo["bot"]["model"].is_null());

    s.host.reply(&id, "hello").await;
    let launch = &s.launches()[0];
    let args: Vec<&str> = launch["args"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(args, ["house", "--launch-json"], "no -m, -e or --permission-mode of the host's: {args:?}");
    let options = &s.requests("session/new")[0]["params"]["_meta"]["claudeCode"]["options"];
    assert_eq!(options["model"], "haiku", "hq's model");
}

#[tokio::test]
async fn an_hq_agent_shares_an_image_with_the_tool_it_is_given() {
    let s = start_with("hq_file_agent.py").await;
    let id = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": id, "tasks": false})).await;
    std::fs::write(s.home.join("pic.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    s.host.reply(&id, "send me the picture").await;

    let results: Value =
        serde_json::from_str(&std::fs::read_to_string(s.home.join("file-results.json")).unwrap()).unwrap();
    assert_eq!(
        results["tools"],
        json!(["send_file", "call_person", "request_secret"]),
        "file sharing, ringing the person and asking for a key: its words already are its reply"
    );
    assert!(results["sent"]["isError"].is_null(), "{results}");
    let history = s.host.call("history", json!({"botId": id})).await;
    let shared =
        history["entries"].as_array().unwrap().iter().find(|e| e["data"]["files"].is_array()).expect("a file entry");
    assert_eq!(shared["data"]["files"][0]["name"], "pic.png");
}

/// The history's entries of `kind` that `pick` keeps.
async fn entries(s: &Setup, bot: &str, pick: impl Fn(&Value) -> bool) -> Vec<Value> {
    let history = s.host.call("history", json!({"botId": bot})).await;
    history["entries"].as_array().unwrap().iter().filter(|e| pick(e)).cloned().collect()
}

/// Waits for an entry `pick` keeps.
async fn wait_for(s: &Setup, bot: &str, what: &str, pick: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(e) = entries(s, bot, &pick).await.into_iter().last() {
            return e;
        }
        assert!(Instant::now() < deadline, "no {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn pending_card(e: &Value) -> bool {
    e["kind"] == "permission"
        && e["data"]["status"] == "pending"
        && e["data"]["limit"]["kind"] == "agent"
        && e["data"]["limit"]["entryIds"].as_array().is_some_and(|ids| !ids.is_empty())
}

fn finals(e: &Value) -> bool {
    e["kind"] == "agent" && e["data"]["final"] == true
}

#[tokio::test]
async fn an_owner_message_over_the_limit_waits_under_a_card_and_runs_when_answered() {
    let s = start().await;
    let (bot, _) = s.house_chat().await;
    let over = |at: &str| {
        let state = json!({"agent": "house", "limit": 300, "at": at, "spent": 300.4, "over": true, "month": "October",
            "runs": 99, "perRun": 3.03, "raiseTo": 400, "resets": "2026-11-01",
            "text": "house has used $300.40 of its $300 for October (at API prices)"});
        std::fs::write(s.home.join("limit.json"), state.to_string()).unwrap();
    };
    let send = |text: &str| {
        let nonce = uuid::Uuid::new_v4().to_string();
        s.host.call("send", json!({"botId": bot, "text": text, "clientNonce": nonce}))
    };
    let replies = entries(&s, &bot, finals).await.len();

    // Ask: the message waits under the card, nothing runs; Allow runs it.
    over("ask");
    send("over the limit, ask").await;
    let card = wait_for(&s, &bot, "limit card", pending_card).await;
    assert_eq!(card["data"]["title"], "This run is waiting: house has used its $300 for October.");
    let names: Vec<&str> =
        card["data"]["options"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Allow this run", "Raise to $400", "Stop"]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(entries(&s, &bot, finals).await.len(), replies, "nothing ran while the card waits");
    let held = card["data"]["limit"]["entryIds"][0].as_str().unwrap().to_owned();
    s.host.call("respondPermission", json!({"entryId": card["id"], "optionId": "allow"})).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    while entries(&s, &bot, finals).await.len() <= replies {
        assert!(Instant::now() < deadline, "the allowed message never ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let message = entries(&s, &bot, |e| e["id"] == held.as_str()).await.remove(0);
    assert_eq!(message["data"]["limitAllowed"], true, "the message is kept and marked allowed");
    let answered = entries(&s, &bot, |e| e["id"] == card["id"]).await.remove(0);
    assert_eq!(answered["data"]["status"], "answered");

    // Raise: the kernel is told the new limit, then the message runs.
    let replies = entries(&s, &bot, finals).await.len();
    send("over the limit, raise").await;
    let card = wait_for(&s, &bot, "second limit card", pending_card).await;
    s.host.call("respondPermission", json!({"entryId": card["id"], "optionId": "raise"})).await;
    assert_eq!(std::fs::read_to_string(s.home.join("limit_set")).unwrap(), "400");
    let deadline = Instant::now() + Duration::from_secs(30);
    while entries(&s, &bot, finals).await.len() <= replies {
        assert!(Instant::now() < deadline, "the message never ran after the raise");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Stop: the message is kept, not run.
    let replies = entries(&s, &bot, finals).await.len();
    over("ask");
    send("over the limit, stop").await;
    let card = wait_for(&s, &bot, "third limit card", pending_card).await;
    let held = card["data"]["limit"]["entryIds"][0].as_str().unwrap().to_owned();
    s.host.call("respondPermission", json!({"entryId": card["id"], "optionId": "stop"})).await;
    let message = entries(&s, &bot, |e| e["id"] == held.as_str()).await.remove(0);
    assert_eq!(message["data"]["status"], "cancelled");
    assert_eq!(message["data"]["text"], "over the limit, stop", "the message is kept");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(entries(&s, &bot, finals).await.len(), replies);

    // Hold: no Allow on the card.
    over("hold");
    send("over the limit, hold").await;
    let card = wait_for(&s, &bot, "hold card", pending_card).await;
    let ids: Vec<&str> =
        card["data"]["options"].as_array().unwrap().iter().map(|o| o["optionId"].as_str().unwrap()).collect();
    assert_eq!(ids, ["raise", "stop"]);
    let refused = s.host.refused("respondPermission", json!({"entryId": card["id"], "optionId": "allow"})).await;
    assert!(refused.contains("choices"), "{refused}");
    s.host.call("respondPermission", json!({"entryId": card["id"], "optionId": "stop"})).await;

    // Tell: it runs, with a line once.
    over("tell");
    s.host.reply(&bot, "over the limit, tell").await;
    let told = entries(&s, &bot, |e| {
        e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("running anyway"))
    })
    .await;
    assert_eq!(told.len(), 1);
    s.host.reply(&bot, "over the limit, tell again").await;
    let told = entries(&s, &bot, |e| {
        e["kind"] == "notice" && e["data"]["text"].as_str().is_some_and(|t| t.contains("running anyway"))
    })
    .await;
    assert_eq!(told.len(), 1, "the line shows once a month");
}

/// The latest `--launch-json` call's room env, as the stub logged it: (`KITH_ROOM_MEMBERS`, `KITH_ROOM_OWNER_ONLY`).
fn last_room_env(s: &Setup) -> (Option<String>, Option<String>) {
    let last = s.launches().pop().expect("a launch");
    let text = |k: &str| last[k].as_str().map(str::to_owned);
    (text("room_members"), text("room_owner_only"))
}

/// Waits until the agent has been initialized `n` times. The room's other (plain) member bots answer a room message too,
/// so the first final reply is not always the agent's.
async fn initialized(s: &Setup, n: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(30);
    while s.requests("initialize").len() < n && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    s.requests("initialize").len()
}

#[tokio::test]
async fn a_warm_owner_process_in_a_room_starts_again_when_the_rooms_members_change() {
    let mut s = start().await;
    let house = s.house().await["id"].as_str().unwrap().to_owned();
    let guest = s.host.bot("Guest", "default").await;
    // A bot guest's row: its person is set while the host is stopped (the rig has no invite path).
    s.host
        .restart(|dir| {
            let db = rusqlite::Connection::open(dir.join("serve.db")).unwrap();
            db.execute("UPDATE bots SET config = json_set(config, '$.person', 'sam') WHERE id = ?1", [&guest]).unwrap();
        })
        .await;
    s.house().await;
    s.host.call("updateBot", json!({"id": house, "tasks": false})).await;
    let group = s.host.call("createBot", json!({"name": "Room", "kind": "group", "members": [house]})).await;
    let group = group["bot"]["id"].as_str().unwrap().to_owned();
    let inits = || s.requests("initialize").len();

    s.host.reply(&group, "first").await;
    assert_eq!(s.launches().len(), 1);
    assert_eq!(last_room_env(&s), (None, Some("1".to_owned())), "the owner's bots alone: owner only");
    assert_eq!(inits(), 1);

    // Nothing changed: the warm process answers, no new launch.
    s.host.reply(&group, "second").await;
    assert_eq!((s.launches().len(), inits()), (1, 1), "unchanged members reuse the warm process");

    // A bot guest joins the room: its person is a member now, so the process starts again with that env.
    s.host.call("updateBot", json!({"id": group, "members": [house, guest]})).await;
    s.host.reply(&group, "third").await;
    assert_eq!(initialized(&s, 2).await, 2, "a new member starts the process again");
    assert_eq!(s.launches().len(), 2);
    assert_eq!(last_room_env(&s), (Some("sam".to_owned()), None), "the guest's person, and no longer owner only");

    // And back to the owner's bots alone: a new process again, owner only.
    s.host.call("updateBot", json!({"id": group, "members": [house]})).await;
    s.host.reply(&group, "fourth").await;
    assert_eq!(initialized(&s, 3).await, 3);
    assert_eq!(s.launches().len(), 3);
    assert_eq!(last_room_env(&s), (None, Some("1".to_owned())));
}

/// A stay of `person` in `room` as the host writes it, written straight into the host's database (the rig has no
/// authority to grant `addRoomPerson` against): `left` closes it. The host reads the store on every turn.
fn stay(s: &Setup, room: &str, person: &str, joined: i64, left: Option<i64>) {
    let db = rusqlite::Connection::open(s.host.home.join("serve.db")).unwrap();
    db.execute(
        "INSERT INTO room_people(room, person, joined_gen, left_gen) VALUES(?1, ?2, ?3, ?4)",
        rusqlite::params![room, person, joined, left],
    )
    .unwrap();
}

#[tokio::test]
async fn a_warm_owner_process_in_a_room_is_not_reused_after_a_join_then_a_leave_or_while_anyone_is_in() {
    let s = start().await;
    let house = s.house().await["id"].as_str().unwrap().to_owned();
    s.host.call("updateBot", json!({"id": house, "tasks": false})).await;
    let group = s.host.call("createBot", json!({"name": "Room", "kind": "group", "members": [house]})).await;
    let group = group["bot"]["id"].as_str().unwrap().to_owned();
    let inits = || s.requests("initialize").len();

    s.host.reply(&group, "first").await;
    s.host.reply(&group, "second").await;
    assert_eq!((s.launches().len(), inits()), (1, 1), "nobody in the room: the warm process answers");

    // Someone joined and left between two turns: the members are the owner's bots alone again, but the room's gen
    // moved, so the process that started before is not trusted with what happened in between.
    stay(&s, &group, "sam", 1_000_000, Some(1_000_001));
    s.host.reply(&group, "third").await;
    assert_eq!(initialized(&s, 2).await, 2, "a join then a leave starts the process again");
    assert_eq!(last_room_env(&s), (None, Some("1".to_owned())), "and the room reads as owner only again");

    // The gen is the same now and nobody is in: warm again.
    s.host.reply(&group, "fourth").await;
    assert_eq!((s.launches().len(), inits()), (2, 2), "the new process is reused while nothing changes");

    // Someone is in the room: a process for it is never reused, though the members are the same as last turn.
    stay(&s, &group, "ann", 1_000_002, None);
    s.host.reply(&group, "fifth").await;
    // A person's activation may ask the kernel for a launch twice (see the guest test below), so a turn with a person in the
    // room starts one process or two: these counts are lower bounds. A reply returns only once its process answered.
    assert!(initialized(&s, 3).await >= 3);
    assert_eq!(last_room_env(&s), (Some("ann".to_owned()), None), "the room's person is in the room's members");
    s.host.reply(&group, "sixth").await;
    let after = initialized(&s, 4).await;
    assert!(after >= 4, "with a person in the room no process is warm: {after} initializes");
}

#[tokio::test]
async fn a_bot_guests_warm_run_in_a_room_starts_again_when_the_rooms_members_change() {
    let mut s = start_for_a_person().await;
    let house = s.house().await["id"].as_str().unwrap().to_owned();
    let other = s.host.bot("Other", "default").await;
    s.host
        .restart(|dir| {
            let db = rusqlite::Connection::open(dir.join("serve.db")).unwrap();
            db.execute("UPDATE bots SET config = json_set(config, '$.person', 'kim') WHERE id = ?1", [&other]).unwrap();
            db.execute("UPDATE bots SET config = json_set(config, '$.person', 'sam') WHERE id != ?1", [&other])
                .unwrap();
        })
        .await;
    s.house().await;
    s.host.call("updateBot", json!({"id": house, "tasks": false})).await;
    let group = s.host.call("createBot", json!({"name": "Room", "kind": "group", "members": [house]})).await;
    let group = group["bot"]["id"].as_str().unwrap().to_owned();
    let inits = || s.requests("initialize").len();

    s.host.reply(&group, "first").await;
    // A person's activation asks the kernel for a launch each time it runs (the count is 1 or 2 depending on whether
    // the turn's prepare step runs twice before the reply lands), so the one process is counted by its initialize.
    assert_eq!(inits(), 1);
    assert_eq!(last_room_env(&s), (Some("sam".to_owned()), None), "the room holds sam's bot");

    // A person's turn asks the kernel for a launch every time, but the unchanged room keeps the one process.
    s.host.reply(&group, "second").await;
    assert_eq!(inits(), 1, "unchanged members reuse the guest's warm process");

    // Another person's bot joins: the guest's own process starts again and sees both.
    s.host.call("updateBot", json!({"id": group, "members": [house, other]})).await;
    s.host.reply(&group, "third").await;
    assert_eq!(initialized(&s, 2).await, 2, "a changed room starts the guest's process again");
    assert_eq!(last_room_env(&s), (Some("kim,sam".to_owned()), None));
}
