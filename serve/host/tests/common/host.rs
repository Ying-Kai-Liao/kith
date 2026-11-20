//! A real host on disposable data, and what the end-to-end tests ask of it.

// Each test binary compiles `common` and uses only part of this.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};

pub struct Host {
    child: Child,
    /// The host's `KITH_SERVE_HOME`.
    pub home: PathBuf,
    base: String,
    token: String,
    /// Passed again when the host restarts.
    env: Vec<(String, String)>,
}

impl Drop for Host {
    fn drop(&mut self) {
        super::stop(&mut self.child);
        let _ = std::fs::remove_dir_all(&self.home);
        super::forget_home(&self.home);
    }
}

impl Host {
    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn start() -> Self {
        Self::start_env(&[]).await
    }

    /// [`Self::start`] with extra environment variables for the host (a later value wins). The host's kernel is
    /// `kernel_agents.py`: it lists the folders of `agents_dir(home)` as kith agents and starts each on the scripted
    /// ACP agent its `acp` file names (see [`Self::agent`]). One-bot-model C5: every bot of a test is one.
    pub async fn start_env(env: &[(&str, &str)]) -> Self {
        let home = std::env::temp_dir().join(format!("kith-memory-e2e-{}", uuid::Uuid::new_v4()));
        let agents = super::agents_env(&home);
        let mut vars: Vec<(&str, &str)> = agents.iter().map(|(k, v)| (*k, v.as_str())).collect();
        vars.extend(env.iter().copied());
        Self::start_in(home, &vars).await
    }

    async fn start_in(home: PathBuf, env: &[(&str, &str)]) -> Self {
        let (child, base) = super::serve_env(&home, env).await;
        let token = super::read_token(&home).trim().to_owned();
        let env = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Self { child, home, base, token, env }
    }

    /// Stops the host, lets `between` change its files, and starts it again on the same data.
    pub async fn restart(&mut self, between: impl FnOnce(&std::path::Path)) {
        super::stop(&mut self.child);
        between(&self.home);
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let (child, base) = super::serve_env(&self.home, &env).await;
        self.child = child;
        self.base = base;
    }

    /// Stops the host and starts it again on the same data with `env` added to what it had (a later value wins).
    pub async fn restart_env(&mut self, env: &[(&str, &str)]) {
        super::stop(&mut self.child);
        self.env.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let all: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let (child, base) = super::serve_env(&self.home, &all).await;
        self.child = child;
        self.base = base;
    }

    pub async fn post(&self, method: &str, body: &Value) -> (bool, Value) {
        let response = reqwest::Client::new()
            .post(format!("{}/api/{method}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .unwrap();
        let ok = response.status().is_success();
        (ok, response.json().await.unwrap())
    }

    /// The roster as a client holds it after the event stream's catch-up.
    pub async fn roster(&self) -> Vec<Value> {
        super::snapshot(&self.base, &self.token).await["bots"].as_array().cloned().unwrap_or_default()
    }

    pub async fn call(&self, method: &str, body: Value) -> Value {
        let (ok, body) = self.post(method, &body).await;
        assert!(ok, "{method}: {body}");
        body
    }

    /// A call that must be refused: its error text.
    pub async fn refused(&self, method: &str, body: Value) -> String {
        let (ok, body) = self.post(method, &body).await;
        assert!(!ok, "{method} should have been refused: {body}");
        body["error"].as_str().unwrap_or_default().to_owned()
    }

    pub async fn bot(&self, name: &str, permission: &str) -> String {
        let bot = json!({"name": name, "backend": "custom", "tasks": false, "command": "unused", "connectors": [], "permission": permission});
        self.call("createBot", bot).await["bot"]["id"].as_str().unwrap().to_owned()
    }

    /// A host-native row, as an old release left one in serve.db (nothing can make one now): the host restarts with it
    /// listed, flagged and refused. Its id.
    pub async fn native_row(&mut self, name: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let config = json!({"id": id, "name": name, "backend": "custom", "command": "unused", "cwd": "", "tasks": false});
        self.restart(|home| {
            let db = rusqlite::Connection::open(home.join("serve.db")).unwrap();
            db.execute("INSERT INTO bots(id, rev, deleted, config) VALUES (?1, 1, 0, ?2)", (&id, config.to_string()))
                .unwrap();
        })
        .await;
        id
    }

    pub async fn memory(&self, bot: &str) -> Value {
        self.call("memory", json!({"botId": bot})).await
    }

    pub async fn tool(&self, bot: &str, name: &str, arguments: Value) -> Value {
        self.call("memoryCall", json!({"botId": bot, "name": name, "arguments": arguments})).await
    }

    pub async fn tool_refused(&self, bot: &str, name: &str, arguments: Value) -> String {
        self.refused("memoryCall", json!({"botId": bot, "name": name, "arguments": arguments})).await
    }

    /// The folder of the kith agent `name` ([`Self::start_agents`]).
    pub fn agent_dir(&self, name: &str) -> PathBuf {
        super::agents_dir(&self.home).join(name)
    }

    /// A kith agent `name` on the scripted ACP agent `tests/fixtures/<fixture>` with `args`, as the old suites made a
    /// bot: its messages are not tasks. Its folder is its working folder, where a fixture keeps its files.
    pub async fn agent(&self, name: &str, fixture: &str, args: &str) -> String {
        let id = self.task_agent(name, fixture, args).await;
        self.call("updateBot", json!({"id": id, "tasks": false})).await;
        id
    }

    /// [`Self::agent`] as the apps make one: its messages become tasks.
    pub async fn task_agent(&self, name: &str, fixture: &str, args: &str) -> String {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(fixture);
        let dir = self.agent_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("acp"), format!("{} {args}", super::python_agent(&script))).unwrap();
        self.call("syncHqBots", json!({})).await;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(bot) = self.roster().await.into_iter().find(|b| b["hqAgent"] == name) {
                return bot["id"].as_str().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "the agent {name} never got a bot");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A kith agent named `name` running `tests/fixtures/<fixture>` with `args` (messages are not tasks): its bot's id.
    pub async fn fixture_bot(&self, name: &str, fixture: &str, args: &str) -> String {
        self.agent(name, fixture, args).await
    }

    /// Sends `text` and waits for the bot's reply.
    pub async fn reply(&self, bot: &str, text: &str) {
        let finals = |history: &Value| {
            history["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "agent" && e["data"]["final"] == true)
                .count()
        };
        let before = finals(&self.call("history", json!({"botId": bot})).await);
        let nonce = uuid::Uuid::new_v4().to_string();
        self.call("send", json!({"botId": bot, "text": text, "clientNonce": nonce})).await;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let history = self.call("history", json!({"botId": bot})).await;
            if finals(&history) > before {
                return;
            }
            assert!(Instant::now() < deadline, "{bot} didn't answer {text:?}: {history}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The requests `instructions_agent.py` logged in a bot's workspace.
    pub fn requests(&self, bot: &str, method: &str) -> Vec<Value> {
        let log = std::fs::read_to_string(self.home.join("bots").join(bot).join("workspace/requests.jsonl")).unwrap();
        log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).filter(|r| r["method"] == method).collect()
    }

    /// The text of every prompt a bot's agent got, oldest first.
    pub fn prompts(&self, bot: &str) -> Vec<String> {
        self.requests(bot, "session/prompt")
            .iter()
            .map(|r| r["params"]["prompt"][0]["text"].as_str().unwrap().to_owned())
            .collect()
    }
}
