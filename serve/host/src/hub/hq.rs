//! The roster of hq agents: each agent `hq ls --json` lists is a bot (see `agent::hq`). The host looks at startup,
//! every [`EVERY`] and when the app asks (`syncHqBots`).

use super::Hub;
use crate::agent::hq::{self, AgentExt as _, HqAgent};
use crate::store::BotConfig;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How often the host looks again. An agent made with `hq` shows up within this, or at once on `syncHqBots`.
const EVERY: Duration = Duration::from_secs(300);

/// Kv key: the bot was hidden because its agent left `hq ls` (a bot the user hid is not shown again).
pub(crate) fn gone_key(bot: &str) -> String {
    format!("hq.gone.{bot}")
}

/// Kv key: the bot's `tasks: false`, which every hq bot was created with until tasks became the default, was turned on once.
fn tasks_key(bot: &str) -> String {
    format!("hq.tasks.{bot}")
}

/// What one sync did, by agent name.
#[derive(Debug, Default, PartialEq)]
pub struct Synced {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub hidden: Vec<String>,
    pub unhidden: Vec<String>,
}

impl Synced {
    pub fn json(&self) -> Value {
        json!({"created": self.created, "updated": self.updated, "hidden": self.hidden, "unhidden": self.unhidden})
    }
}

/// Looks every [`EVERY`], starting now. Runs on the host's runtime for as long as it lives.
pub fn start(hub: &Arc<Hub>) {
    // Unit tests build hubs of their own and must never reach the hq on this computer.
    if cfg!(test) {
        return;
    }
    let hub = hub.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = hub.sync_hq_bots().await {
                tracing::info!(error = format!("{error:#}"), "hq bots not synced");
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

impl Hub {
    /// Finds hq (see [`hq::bin`]) and syncs the bots with `hq ls --json`. Fails, with one line to log, when there is
    /// no hq or it can't list: the bots already there stay as they are.
    pub async fn sync_hq_bots(self: &Arc<Self>) -> Result<Synced> {
        let Some(bin) = tokio::task::spawn_blocking(hq::bin).await? else {
            anyhow::bail!("no kith found (KITH_KERNEL_BIN, PATH or ~/.local/bin/kith)");
        };
        self.sync_hq_bots_with(&bin).await
    }

    pub async fn sync_hq_bots_with(self: &Arc<Self>, bin: &Path) -> Result<Synced> {
        // Startup and the API can ask at once; one at a time, so an agent never gets two bots, and the lock is
        // taken before listing so an older answer can't be applied after a newer one.
        let _one = self.hq_sync.lock().await;
        let agents = hq::list(bin).await?;
        // Rosters carry each place's folder; knowing the projects now lets them name a project's path.
        tokio::spawn(async { drop(hq::projects(true).await) });
        let hub = self.clone();
        tokio::task::spawn_blocking(move || hub.apply_hq_agents(&agents)).await?
    }

    /// Brings the bots to `agents`: a new agent gets a bot; a known one keeps what the user set and takes hq's
    /// description and folder; one that left `hq ls` is hidden, and shown again when it comes back.
    fn apply_hq_agents(self: &Arc<Self>, agents: &[HqAgent]) -> Result<Synced> {
        let mut done = Synced::default();
        let rows = self.store.bots()?;
        for agent in agents {
            if !agent.runnable() {
                tracing::debug!(agent = %agent.name, harness = ?agent.harness, "hq agent is not a Claude agent; no bot");
                continue;
            }
            if !Path::new(&agent.home).is_dir() {
                tracing::info!(agent = %agent.name, home = %agent.home, "hq agent folder missing; no bot");
                continue;
            }
            // An invitee's chat with the agent is not the agent's own bot.
            let mine = |b: &&crate::store::BotRow| {
                b.config.hq_agent.as_deref() == Some(agent.name.as_str()) && b.config.person.is_none()
            };
            let Some(row) = rows.iter().find(mine) else {
                let made = self.create_bot(new_bot(agent))?;
                // Born with tasks on: the one-time turn-on is done, so a later "off" from the user stays off.
                if let Some(id) = made["id"].as_str() {
                    self.store.kv_set(&tasks_key(id), "1")?;
                }
                done.created.push(agent.name.clone());
                continue;
            };
            // A bot the user deleted stays deleted.
            if row.deleted {
                continue;
            }
            let mut patch = serde_json::Map::new();
            if row.config.description != agent.description {
                patch.insert("description".into(), agent.description.clone().into());
            }
            if row.config.cwd != agent.home {
                patch.insert("cwd".into(), agent.home.clone().into());
            }
            // Chats with hq bots auto-thread (a task per burst) like any bot's; older bots were created with it off.
            if self.store.kv_get(&tasks_key(&row.config.id)).is_none() {
                if row.config.tasks == Some(false) {
                    patch.insert("tasks".into(), true.into());
                }
                self.store.kv_set(&tasks_key(&row.config.id), "1")?;
            }
            let came_back = row.config.hidden && self.store.kv_get(&gone_key(&row.config.id)).is_some();
            if came_back {
                patch.insert("hidden".into(), false.into());
            }
            if patch.is_empty() {
                continue;
            }
            patch.insert("id".into(), row.config.id.clone().into());
            self.update_bot(&Value::Object(patch))?;
            if came_back {
                self.store.kv_delete(&gone_key(&row.config.id))?;
                done.unhidden.push(agent.name.clone());
            } else {
                done.updated.push(agent.name.clone());
            }
        }
        for row in &rows {
            let Some(name) = row.config.hq_agent.as_deref() else { continue };
            let listed = agents.iter().any(|a| a.name == name && a.runnable() && Path::new(&a.home).is_dir());
            if listed || row.deleted || row.config.hidden {
                continue;
            }
            // A running turn goes on: hiding only takes the bot off the roster.
            self.store.kv_set(&gone_key(&row.config.id), "1")?;
            self.update_bot(&json!({"id": row.config.id, "hidden": true}))?;
            done.hidden.push(name.to_owned());
        }
        Ok(done)
    }
}

/// An agent's first bot. Permission is Ask: the agent's requests become approval cards until the user says otherwise.
/// Its chat auto-threads: each burst of messages is a task in a thread of its own, as in any bot's chat.
fn new_bot(agent: &HqAgent) -> BotConfig {
    serde_json::from_value(json!({
        "id": "", "name": agent.name, "description": agent.description, "backend": "claude", "cwd": agent.home,
        "hqAgent": agent.name, "permission": "default", "connectors": [], "skills": [],
    }))
    .expect("a valid bot")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::identity::Identity;
    use crate::store::Store;

    fn hub() -> (Arc<Hub>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kith-hq-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "hq-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        (hub, dir)
    }

    fn agent(dir: &Path, name: &str, description: &str) -> HqAgent {
        let home = dir.join(name);
        std::fs::create_dir_all(&home).unwrap();
        HqAgent {
            name: name.into(),
            description: description.into(),
            home: home.to_string_lossy().into_owned(),
            harness: Some(hq::Harness::Claude),
            ..HqAgent::default()
        }
    }

    fn bots(hub: &Hub) -> Vec<BotConfig> {
        hub.store.bots().unwrap().into_iter().filter(|b| !b.deleted).map(|b| b.config).collect()
    }

    #[tokio::test]
    async fn an_agent_gets_one_bot_whose_edits_a_resync_keeps() {
        let (hub, dir) = hub();
        let house = agent(&dir, "house", "keeps the house");
        let done = hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        assert_eq!(done.created, ["house"]);
        let bot = bots(&hub).remove(0);
        assert_eq!(
            (bot.hq_agent.as_deref(), bot.backend.as_str(), bot.cwd.as_str()),
            (Some("house"), "claude", house.home.as_str())
        );
        assert_eq!(bot.permission, crate::store::Permission::Ask, "approval cards until the user says otherwise");
        assert!(bot.model.is_none() && bot.connectors.is_empty() && bot.skills.is_empty());

        hub.update_bot(&json!({"id": bot.id, "name": "Home", "pinned": true, "notify": false, "permission": "auto",
            "model": "opus", "hqAgent": "other", "backend": "codex"}))
            .unwrap();
        let done = hub.apply_hq_agents(&[agent(&dir, "house", "keeps the house tidy")]).unwrap();
        assert!(done.created.is_empty(), "never a second bot: {done:?}");
        let all = bots(&hub);
        assert_eq!(all.len(), 1);
        let kept = &all[0];
        assert_eq!(
            (kept.name.as_str(), kept.pinned, kept.notify, kept.model.as_deref()),
            ("Home", true, Some(false), None)
        );
        // Model, effort, Fast mode and permission are hq's: an app's edit of them on an hq bot is not stored.
        assert_eq!(kept.permission, crate::store::Permission::Ask);
        assert_eq!((kept.hq_agent.as_deref(), kept.backend.as_str()), (Some("house"), "claude"), "not editable");
        assert_eq!(kept.description, "keeps the house tidy", "hq's description");
    }

    #[tokio::test]
    async fn hq_bots_auto_thread_and_an_older_one_is_turned_on_once() {
        let (hub, dir) = hub();
        let house = agent(&dir, "house", "");
        hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        let bot = bots(&hub).remove(0);
        assert!(bot.uses_tasks(), "a new hq bot starts a task per burst");

        // A new bot is born with the turn-on done: the user's "off" survives the next sync.
        hub.update_bot(&json!({"id": bot.id, "tasks": false})).unwrap();
        hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        assert!(!bots(&hub)[0].uses_tasks());

        // A bot made before tasks were on by default has tasks off; the first sync turns it on, and the user's choice after that stays.
        hub.store.kv_delete(&tasks_key(&bot.id)).unwrap();
        hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        assert!(bots(&hub)[0].uses_tasks());
        hub.update_bot(&json!({"id": bot.id, "tasks": false})).unwrap();
        hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        assert!(!bots(&hub)[0].uses_tasks());
    }

    #[tokio::test]
    async fn a_gone_agent_is_hidden_and_comes_back_but_a_users_hide_stays() {
        let (hub, dir) = hub();
        let (house, money) = (agent(&dir, "house", ""), agent(&dir, "money", ""));
        hub.apply_hq_agents(&[house.clone(), money.clone()]).unwrap();
        let id = |name: &str| bots(&hub).into_iter().find(|b| b.name == name).unwrap().id;
        hub.update_bot(&json!({"id": id("money"), "hidden": true})).unwrap();

        let done = hub.apply_hq_agents(&[]).unwrap();
        assert_eq!(done.hidden, ["house"], "money was hidden already");
        assert!(bots(&hub).iter().all(|b| b.hidden), "hidden, not deleted");

        let done = hub.apply_hq_agents(&[house, money]).unwrap();
        assert_eq!(done.unhidden, ["house"]);
        let hidden: Vec<_> = bots(&hub).into_iter().filter(|b| b.hidden).map(|b| b.name).collect();
        assert_eq!(hidden, ["money"], "the user's own hide is theirs");
    }

    #[tokio::test]
    async fn only_claude_agents_get_bots() {
        // What `hq ls --json` prints: an agent from an older hq has no harness, which means Claude; one this version
        // doesn't know is never started.
        let listed: Vec<HqAgent> = serde_json::from_str(
            r#"[{"name":"a","description":"","home":"/x"},{"name":"b","description":"","home":"/x","harness":"claude"},
               {"name":"c","description":"","home":"/x","harness":"codex"},{"name":"d","description":"","home":"/x","harness":"gemini"}]"#,
        )
        .unwrap();
        assert_eq!(listed.iter().map(HqAgent::runnable).collect::<Vec<_>>(), [true, true, false, false]);

        let (hub, dir) = hub();
        let mut codex = agent(&dir, "coder", "");
        codex.harness = Some(hq::Harness::Codex);
        let house = agent(&dir, "house", "");
        let done = hub.apply_hq_agents(&[codex.clone(), house.clone()]).unwrap();
        assert_eq!(done.created, ["house"], "no bot that would fail on every message");
        // An agent that switches to another harness is taken off the roster like one that left hq.
        let mut moved = house;
        moved.harness = Some(hq::Harness::Codex);
        let done = hub.apply_hq_agents(&[codex, moved]).unwrap();
        assert_eq!(done.hidden, ["house"]);
    }

    #[tokio::test]
    async fn a_deleted_bot_and_a_missing_folder_make_no_bot() {
        let (hub, dir) = hub();
        let house = agent(&dir, "house", "");
        hub.apply_hq_agents(std::slice::from_ref(&house)).unwrap();
        hub.delete_bot(&bots(&hub)[0].id).unwrap();
        let ghost = HqAgent {
            name: "ghost".into(),
            description: String::new(),
            home: "/no/such/folder".into(),
            ..HqAgent::default()
        };
        let done = hub.apply_hq_agents(&[house, ghost]).unwrap();
        assert_eq!(done, Synced::default());
        assert!(bots(&hub).is_empty());
    }
}
