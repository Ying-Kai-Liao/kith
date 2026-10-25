//! Bot snapshots shared by full, incremental and live synchronization.

use super::Hub;
use crate::store::{BotRow, Lane, ReadScope};
use anyhow::Result;
use serde_json::{Value, json};

impl Hub {
    pub fn bot_json(&self, row: &BotRow) -> Value {
        let cfg = &row.config;
        let mut v = serde_json::to_value(cfg).expect("BotConfig is plain data and always serializes");
        v["managedWorkspace"] = crate::agent::workspace::is_managed(cfg).into();
        // Null where there is no switch to show.
        v["harnessMemory"] =
            (!cfg.is_group() && crate::agent::own_memory::switchable(cfg)).then_some(cfg.harness_memory).into();
        if !cfg.is_group() {
            // The bot's own chat; a thread's comes with the thread (`thread`).
            v["folder"] = crate::agent::folder::describe_cached(&self.store, cfg, &Lane::main(&cfg.id));
            v["sharedMemory"] = crate::chat::memory::shared(cfg).into();
            v["memoryRev"] = self.store.memory_rev().into();
        }
        let rt = if cfg.is_group() {
            // A group has no agent: Fast mode belongs to its members.
            crate::hub::Runtime {
                fast_mode: crate::agent::config::FastModeSupport::default(),
                ..self.group_runtime(cfg)
            }
        } else {
            self.bot_runtime(&cfg.id)
        };
        // Every place it works in at once; `workingChat`/`workingThread` name one of them.
        let lanes: Vec<Value> =
            self.working_lanes(cfg).iter().map(|l| json!({"chat": l.chat, "thread": l.thread})).collect();
        v["workingLanes"] = lanes.into();
        let last = self.store.last_message(&cfg.id);
        v["rev"] = row.rev.into();
        v["deleted"] = row.deleted.into();
        v["status"] = json!(rt.status);
        v["activity"] = rt.activity.into();
        v["startedAt"] = rt.started_at.into();
        v["workingChat"] = rt.lane.as_ref().map(|l| l.chat.clone()).into();
        v["fastModeSupport"] = rt.fast_mode.as_str().into();
        v["fastModeIssue"] = rt.fast_mode.issue().into();
        v["workingThread"] = rt.lane.and_then(|l| l.thread).into();
        v["unread"] = self.store.unread(&cfg.id, row.read_rev, ReadScope::All).into();
        let preview = last.as_ref().map(|l| match &l.author {
            // A group names who spoke.
            Some(author) if cfg.is_group() => format!("{}: {}", self.bot_name(author), l.text),
            Some(_) => l.text.clone(),
            None => format!("You: {}", l.text),
        });
        v["lastMessage"] = preview.map(|p| crate::agent::acp::truncate(&p, 280)).into();
        v["lastAt"] = last.map_or(cfg.created_at, |l| l.at).into();
        // The chat of a bot guest: the apps tag it and show it read-only. Absent for every other row.
        if let Some(g) = cfg.person.as_deref().and_then(|p| self.store.bot_guest_of_person(p)) {
            let owner_name = g.owner_person.as_deref().and_then(|p| self.store.person(p)).map(|p| p.name);
            v["botGuest"] = json!({
                "botId": g.bot_id, "name": g.name, "provider": g.provider,
                "ownerPerson": g.owner_person, "ownerName": owner_name,
            });
        }
        v
    }

    /// Full refreshes retain cached client state, so they need deletion records too.
    pub fn bots_json(&self, since: i64) -> Result<Vec<Value>> {
        Ok(self.store.bots()?.iter().filter(|b| b.rev > since).map(|b| self.bot_json(b)).collect())
    }
}
