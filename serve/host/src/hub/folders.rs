//! The folder each place of a bot works in (see `agent::folder`): checking a client's pick, applying it, and
//! ending the sessions it makes stale.

use super::Hub;
use crate::agent::bot::{self, Cmd};
use crate::agent::folder::{self, Change, Pick};
use crate::agent::hq::{self, ProjectExt as _};
use crate::store::{BotConfig, Lane, lane_key};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;

/// A pick that passed [`Hub::check_change`]: what to store (None: forget the place's own pick) and the folder it
/// is, for the recent list.
pub struct Checked {
    pick: Option<Pick>,
    path: String,
}

impl Hub {
    /// Whether `change` names something real: a project hq knows, a folder inside the browsable roots (stored
    /// resolved). None when the client asked for nothing.
    pub async fn check_change(&self, change: Change) -> Result<Option<Checked>> {
        Ok(Some(match change {
            Change::Nothing => return Ok(None),
            Change::Clear => Checked { pick: None, path: String::new() },
            Change::Set(Pick::Project(id)) => {
                if !folder::valid_project_id(&id) {
                    bail!("`{id}` isn't a project id (lowercase words joined by dashes)");
                }
                let listed = hq::projects(true).await?;
                let project = listed.iter().find(|p| p.id == id).ok_or_else(|| anyhow!("unknown project {id}"))?;
                Checked { path: project.folder().unwrap_or_default(), pick: Some(Pick::Project(id)) }
            }
            Change::Set(Pick::Folder(path)) => {
                let canon = tokio::task::spawn_blocking(move || folder::check(Path::new(&path))).await??;
                let path = canon.to_string_lossy().into_owned();
                Checked { pick: Some(Pick::Folder(path.clone())), path }
            }
        }))
    }

    /// The bot a folder can be set for, in a place it talks: its own chat or one of its threads, or one of its
    /// group lanes. A group has no agent of its own.
    fn folder_place(&self, bot_id: &str, lane: &Lane) -> Result<BotConfig> {
        let cfg = self.store.bot(bot_id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?.config;
        if cfg.is_group() {
            bail!("A group has no folder of its own; set the folder of a member in it");
        }
        if lane.chat == bot_id {
            if let Some(root) = &lane.thread {
                let ok = self.store.entry(root).is_some_and(|e| e.bot_id == bot_id && e.thread_id.is_none());
                if !ok {
                    bail!("unknown thread");
                }
            }
        } else {
            let group = self.store.bot(&lane.chat)?.filter(|r| !r.deleted && r.config.is_group());
            if lane.thread.is_some() || !group.is_some_and(|g| g.config.members.iter().any(|m| m == bot_id)) {
                bail!("{bot_id} isn't in that group");
            }
        }
        Ok(cfg)
    }

    /// Makes `checked` the folder of `bot_id`'s `lane`. When that changes where the place works, its session ends
    /// (see [`Hub::reset_place`]); setting what it already works in changes nothing. Whether the place moved.
    pub fn apply_change(&self, bot_id: &str, lane: &Lane, checked: &Checked) -> Result<bool> {
        let cfg = self.folder_place(bot_id, lane)?;
        let before = folder::effective(&self.store, &cfg, lane);
        if Pick::load(&self.store, bot_id, lane) == checked.pick {
            return Ok(false);
        }
        Pick::save(&self.store, bot_id, lane, checked.pick.as_ref())?;
        if let Some(pick) = &checked.pick
            && let Err(error) = folder::record_recent(&self.store, pick, &checked.path)
        {
            tracing::warn!(error = format!("{error:#}"), "couldn't note the recent folder");
        }
        let moved = folder::effective(&self.store, &cfg, lane).pick != before.pick;
        if moved {
            self.reset_place(bot_id, lane);
        }
        // Clients read the place again: the bot's event is the one they already follow.
        self.emit_bot(bot_id);
        if lane.chat != bot_id {
            self.emit_bot(&lane.chat);
        }
        Ok(moved)
    }

    /// `setPlaceFolder`: the place's new folder, as clients read it.
    pub async fn set_place_folder(&self, bot_id: &str, lane: &Lane, change: Change) -> Result<Value> {
        let Some(checked) = self.check_change(change).await? else {
            bail!("give one of `project`, `folder` or `clear`");
        };
        self.apply_change(bot_id, lane, &checked)?;
        self.place_folder(bot_id, lane).await
    }

    /// The folder of `bot_id`'s `lane` as clients read it; null for a group.
    pub async fn place_folder(&self, bot_id: &str, lane: &Lane) -> Result<Value> {
        let Some(row) = self.store.bot(bot_id)?.filter(|r| !r.deleted && !r.config.is_group()) else {
            return Ok(Value::Null);
        };
        let eff = folder::effective(&self.store, &row.config, lane);
        let projects = match eff.pick {
            Some(Pick::Project(_)) => hq::projects_soon().await,
            _ => Vec::new(),
        };
        Ok(folder::describe(&eff, &row.config, &projects))
    }

    /// The place's session is over: forget it, drop a turn the last host process cut off there, and tell the actor
    /// that runs it to stop its turn and restart its agent.
    pub(crate) fn reset_place(&self, bot_id: &str, lane: &Lane) {
        self.forget_stored(bot_id, lane);
        bot::forget_inflight(&self.store, bot_id, lane);
        if let Err(error) = self.send_cmd(bot_id, Cmd::ResetPlace { lane: lane.clone() }) {
            tracing::warn!(bot = bot_id, error = format!("{error:#}"), "couldn't reset a place");
        }
    }

    fn forget_stored(&self, bot_id: &str, lane: &Lane) {
        let forgotten = if *lane == Lane::main(bot_id) {
            self.store.set_session(bot_id, None)
        } else {
            self.store.kv_set(&lane_key("session", bot_id, lane), "")
        };
        if let Err(error) = forgotten {
            tracing::warn!(bot = bot_id, error = format!("{error:#}"), "couldn't forget the session of a place");
        }
    }

    /// The bot's default folder changed: the stored sessions of its places without a folder of their own end, so a
    /// place without a running actor starts afresh too. (Running actors do the rest on `Reconfigure`.)
    pub(super) fn reset_inheriting_stored(&self, cfg: &BotConfig) {
        let bot_id = cfg.id.as_str();
        let inherits = |lane: &Lane| Pick::load(&self.store, bot_id, lane).is_none();
        let main = Lane::main(bot_id);
        if inherits(&main) {
            self.forget_stored(bot_id, &main);
            bot::forget_inflight(&self.store, bot_id, &main);
        }
        // Places that were cut off mid-turn but never stored a session.
        for key in self.store.kv_prefix(&format!("turn.inflight.{bot_id}|")) {
            let lane = bot::place_of(key.trim_start_matches("turn.inflight."));
            if inherits(&lane) {
                bot::forget_inflight(&self.store, bot_id, &lane);
            }
        }
        let prefix = format!("lane.session.{bot_id}@");
        for key in self.store.kv_prefix(&prefix) {
            let Some(place) = key.strip_prefix(&prefix) else { continue };
            let lane = match place.split_once('/') {
                Some((chat, root)) => Lane::in_thread(chat, root),
                None => Lane::main(place),
            };
            if self.store.kv_get(&key).is_some_and(|s| !s.is_empty()) && inherits(&lane) {
                self.forget_stored(bot_id, &lane);
                bot::forget_inflight(&self.store, bot_id, &lane);
            }
        }
    }
}
