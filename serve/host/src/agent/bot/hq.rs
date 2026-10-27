//! The sessions of an hq agent's bot: `hq <agent> --launch-json` says how to start it (see `agent::hq`), and
//! nothing of Kith's goes in with it — no instructions of its own, no `chat`/`memory`/`team`/`routines` servers,
//! no memory snapshot, no tidying. The agent's reply is its turn's text.

use super::fast_mode::Listing;
use super::launch::{launch_commands, start_agent};
use super::{Actor, NoticeStyle, Profile};
use crate::agent::acp::Incoming;
use crate::agent::backups::{Backups, ChainEntry, NoAccount, Pick};
use crate::agent::{folder, hq, project};
use crate::cards::access::Reservation;
use crate::hub::Hub;
use crate::store::{EntryKind, Lane};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Asks hq for `agent`'s chain now and remembers the answer; a failure is logged and leaves what was known.
pub(super) async fn fetch_chain(hub: &Hub, agent: &str) -> Vec<ChainEntry> {
    let got = match tokio::task::spawn_blocking(hq::bin).await {
        Ok(Some(bin)) => hq::backups(&bin, agent)
            .await
            .inspect_err(|error| tracing::warn!(agent, error = format!("{error:#}"), "couldn't read the backup chain"))
            .ok(),
        _ => None,
    };
    hub.backups.end_refresh(agent, got);
    hub.backups.known_chain(agent).unwrap_or_default()
}

/// [`fetch_chain`] in the background, one at a time per agent.
fn refresh_chain_later(hub: &Arc<Hub>, agent: &str) {
    if hub.backups.begin_refresh(agent) {
        let (hub, agent) = (hub.clone(), agent.to_owned());
        tokio::spawn(async move { drop(fetch_chain(&hub, &agent).await) });
    }
}

/// What a Codex backup's `permission_mode` (the agent's, in Claude's words) means to codex-acp's session modes.
fn codex_mode(permission: &str) -> Option<&'static str> {
    match permission {
        "plan" | "default" => Some("read-only"),
        "acceptEdits" | "auto" => Some("auto"),
        "bypassPermissions" => Some("full-access"),
        _ => None,
    }
}

/// The environment of the agent process. The keys the host holds for Claude are the default login's and win over
/// `CLAUDE_CONFIG_DIR` and `apiKeyHelper`, so a run on a login of its own gets none of them: a Claude backup, and any
/// run on a named account or API profile (`KITH_ACCOUNT`), the agent's main account included.
fn process_env(saved: Vec<(String, String)>, launch: &hq::Launch, backup: bool) -> Vec<(String, String)> {
    let named = launch.env.get("KITH_ACCOUNT").is_some_and(|a| !a.is_empty());
    let own_login = named || (backup && !launch.is_codex());
    let mut env = if own_login { Vec::new() } else { saved };
    env.extend(launch.env());
    env
}

/// The project set of a turn and what changed with it.
#[derive(Default)]
pub(super) struct ProjectTurn {
    pub projects: Vec<String>,
    /// What the agent is to be told with this turn: the set that became current (empty: no project any more).
    pub say: Option<Vec<String>>,
    pub change: Option<project::Change>,
}

impl Actor {
    pub(super) fn is_hq(&self) -> bool {
        self.cfg.is_hq()
    }

    /// The room an hq run is for: the group chat of the turn, else the 1:1 of this bot's person (or the owner).
    pub(super) fn run_room(&self) -> String {
        let agent = self.cfg.hq_agent.as_deref().unwrap_or_default();
        hq::room_of(agent, self.cfg.person.as_deref(), self.memory_room().as_deref())
    }

    /// Who is in a group room's run, read now so a change between turns counts: the person each member bot runs as (an
    /// invitee, or a bot guest). Only a Kith app group is ever known; a 1:1, and anything the store fails to read (the
    /// group, or any member), is `Unknown`, which the kernel reads as shared only. `OwnerOnly` needs this very read to
    /// succeed and find nobody but the owner's own bots, and a run that is itself a person's never gets it.
    pub(super) fn run_members(&self) -> hq::Members {
        match self.active_group.as_ref() {
            Some(g) => room_members(&self.hub.store, &g.lane.chat, self.cfg.person.as_deref()),
            None => hq::Members::Unknown,
        }
    }

    /// The notes to put before the first user message of a thread. Once per thread: the session is new only for its
    /// first message, and a resumed one is not.
    pub(super) async fn first_message_recall(&self, text: &str) -> Option<String> {
        if !self.session_fresh || self.reuse_prompt || self.turn_text.is_none() {
            return None;
        }
        let agent = self.cfg.hq_agent.clone()?;
        let bin = tokio::task::spawn_blocking(hq::bin).await.ok().flatten()?;
        let project = self.project_turn.as_ref().and_then(|t| t.projects.first()).map(String::as_str);
        hq::recall_for_first_message(
            &bin,
            &agent,
            self.cfg.person.as_deref(),
            &self.run_room(),
            &self.run_members(),
            text,
            project,
        )
        .await
    }

    /// Decides the project set of a person's message (see `agent::project`) and saves it. A pinned project
    /// (`BotConfig.project`) wins and is never a change; so does nothing for a group, whose lane has more than one
    /// person's words in it.
    pub(super) async fn infer_project(&self, lane: &Lane, entry_ids: &[String], text: &str) -> Option<ProjectTurn> {
        if let Some(pin) = &self.cfg.project {
            return Some(ProjectTurn { projects: vec![pin.clone()], ..ProjectTurn::default() });
        }
        if self.cfg.is_group() {
            return None;
        }
        // Only projects the asker may know of: an invitee's are those shared with them, a bot guest's none.
        let person = self.cfg.person.clone();
        let shared = person.as_deref().and_then(|p| self.hub.store.person(p)).map(|p| p.projects).unwrap_or_default();
        let known = project::allowed(hq::projects_soon().await, person.as_deref(), &shared);
        let recent = self.recent_user_texts(lane, entry_ids);
        let (hub, lane, text) = (self.hub.clone(), lane.clone(), text.to_owned());
        let entry = entry_ids.last().cloned().unwrap_or_default();
        let d = tokio::task::spawn_blocking(move || project::infer(&hub.store, &lane, &known, &entry, &text, &recent))
            .await
            .ok()?;
        Some(ProjectTurn { projects: d.state.projects, say: d.say, change: d.change })
    }

    /// The person's last two messages in `lane` before this turn's, oldest first.
    fn recent_user_texts(&self, lane: &Lane, this_turn: &[String]) -> Vec<String> {
        let rows = match &lane.thread {
            Some(root) => self.hub.store.thread(&lane.chat, root, 8),
            None => self.hub.store.history(&lane.chat, i64::MAX, 8),
        }
        .unwrap_or_default();
        let mut texts: Vec<String> = rows
            .iter()
            .filter(|e| e.kind == EntryKind::User.as_str() && !this_turn.contains(&e.id))
            .filter_map(|e| e.data["text"].as_str().map(str::to_owned))
            .collect();
        texts.drain(..texts.len().saturating_sub(2));
        texts
    }

    /// The divider for a change of this turn's project set: "Now on <project>", in the lane, with the move spelled out
    /// for the apps in `projectChange`. The first inference of a thread is a label, not a change.
    pub(super) fn project_divider(&self) {
        let Some(change) = self.project_turn.as_ref().and_then(|t| t.change.as_ref()) else { return };
        let data = json!({
            "text": project::divider_text(&change.to),
            "style": NoticeStyle::Divider,
            "projectChange": project::change_data(&change.from, &change.to, "inferred"),
        });
        self.add(EntryKind::Notice, self.turn.unwrap_or_default(), data);
    }

    /// What the turn carries about its project when it just became current: the line and the new project's notes. Once:
    /// the turn's retries reuse the prompt they were given.
    pub(super) async fn project_context(&mut self) -> Option<String> {
        if self.reuse_prompt {
            return None;
        }
        let say = self.project_turn.as_mut()?.say.take()?;
        let agent = self.cfg.hq_agent.clone()?;
        let bin = tokio::task::spawn_blocking(hq::bin).await.ok().flatten()?;
        Some(
            hq::project_context(&bin, &agent, self.cfg.person.as_deref(), &self.run_room(), &self.run_members(), &say)
                .await,
        )
    }

    /// What the prompt of an hq bot's turn is: the user's words, with the notes the host owes the agent (an earlier
    /// conversation's handoff, where a thread branches off). Nothing about Kith.
    pub(super) fn compose_hq(&mut self, text: &str, recalled: Option<String>) -> String {
        self.session_fresh = false;
        // The turn goes to the next account as the first one got it, handoff and thread notes included.
        let mut prompt = if self.reuse_prompt {
            self.thread_intro = None;
            self.attempt_prompt.clone().unwrap_or_else(|| text.to_owned())
        } else {
            // Notes recalled for a thread's first message go with the turn, never into the system prefix, so the cached
            // prompt stays as it is.
            let mut prompt = recalled.map_or_else(|| text.to_owned(), |r| format!("{r}\n\n{text}"));
            if let Some(handoff) = self.handoff.take() {
                prompt = format!("{handoff}\n\n{prompt}");
            }
            if let Some(intro) = self.thread_intro.take() {
                prompt = format!("{intro}\n\n{prompt}");
            }
            self.attempt_prompt = Some(prompt.clone());
            prompt
        };
        if let Some(preface) = self.preface.take() {
            prompt = format!("{preface}\n\n{prompt}");
        }
        prompt
    }

    /// The session `slot` has on the account that answers now: the agent's stored one, or the one a backup entry
    /// opened in this process.
    fn hq_session(&mut self, slot: Option<&str>) -> Option<String> {
        match self.serving.clone() {
            None => self.session_for(slot),
            Some(entry) => self.backup_sessions.get(&(entry, slot.unwrap_or_default().to_owned())).cloned(),
        }
    }

    /// The account answering now, for its usage numbers: the backup entry, else the main account when it is one of
    /// its own (`claude:work`); `None` is the harness's default login.
    pub(super) fn answering_account(&self) -> Option<&str> {
        self.serving.as_deref().or_else(|| self.conn_account.as_deref().filter(|a| a.contains(':')))
    }

    /// Whether the live process runs on the account the chain now puts first. Only the agent's own sessions ask: a
    /// backup entry's process is its entry's. A chain not yet known, or a kernel that doesn't say, changes nothing.
    fn conn_on_main(&self, chain: &[ChainEntry]) -> bool {
        self.serving.is_some()
            || chain.first().is_none_or(|main| self.conn_account.as_ref().is_none_or(|a| *a == main.id))
    }

    fn hq_set_session(&mut self, slot: Option<&str>, sid: &str) {
        match self.serving.clone() {
            None => self.set_session(slot, Some(sid)),
            Some(entry) => {
                self.backup_sessions.insert((entry, slot.unwrap_or_default().to_owned()), sid.to_owned());
            }
        }
    }

    /// The agent's backup chain as last known. Never makes a turn wait for hq: a chain gone stale is fetched again in
    /// the background, and one never fetched is too, unless backups are in use (something is out), when this turn
    /// needs it to pick. No chain, or any failure, is an empty chain.
    async fn hq_chain(&self, agent: &str) -> Vec<ChainEntry> {
        match self.hub.backups.chain_state(agent) {
            Some((age, chain)) => {
                if Backups::chain_stale(age, &chain) {
                    refresh_chain_later(&self.hub, agent);
                }
                chain
            }
            None if self.hub.backups.any_out() => fetch_chain(&self.hub, agent).await,
            None => {
                refresh_chain_later(&self.hub, agent);
                Vec::new()
            }
        }
    }

    /// A Codex backup's new session starts in the mode that matches the agent's permission mode, when the adapter lists
    /// it. Left as it is when it doesn't, or when the call fails.
    pub(super) async fn set_codex_mode(&mut self, sid: &str, res: &Value, permission: Option<&str>) {
        let Some(mode) = permission.and_then(codex_mode) else { return };
        let listed = res["modes"]["availableModes"].as_array();
        if listed.is_some_and(|modes| !modes.iter().any(|m| m["id"] == mode)) {
            return;
        }
        if let Err(error) = self.session_request("session/set_mode", json!({"sessionId": sid, "modeId": mode})).await {
            tracing::warn!(mode, error = format!("{error:#}"), "couldn't set the backup's permission mode");
        }
    }

    /// A Claude session starts in the mode kith launched the agent with. The adapter starts in "default" (or in the
    /// mode its transcript ended in) and ignores the `defaultMode` the launch carries as a setting, so the host asks
    /// for the mode once the session exists. An invited person's launch has already been confined, so this is never
    /// bypass for them. Left as it is when the adapter doesn't offer the mode, or when the call fails.
    pub(super) async fn set_claude_mode(&mut self, sid: &str, res: &Value, permission: Option<&str>) {
        let Some(mode) = permission.filter(|m| !m.is_empty()) else { return };
        if res["modes"]["currentModeId"] == mode {
            return;
        }
        let offered = res["modes"]["availableModes"].as_array().is_some_and(|ms| ms.iter().any(|m| m["id"] == mode));
        if !offered {
            tracing::warn!(mode, "the agent doesn't offer the permission mode kith launched it with");
            return;
        }
        if let Err(error) = self.session_request("session/set_mode", json!({"sessionId": sid, "modeId": mode})).await {
            tracing::warn!(mode, error = format!("{error:#}"), "couldn't set the session's permission mode");
        }
    }

    /// A non-hq bot's session starts in the bot's own permission mode, passed to the agent unchanged. Codex has its
    /// own mode names, so it gets [`codex_mode`]'s translation. Left as it is when the agent doesn't offer the mode.
    /// A person's bot never starts in bypass, whatever its field says.
    pub(super) async fn set_bot_mode(&mut self, sid: &str, res: &Value) {
        let mode = self.cfg.permission.mode_id();
        if self.cfg.person.is_some() && mode == "bypassPermissions" {
            return;
        }
        if self.cfg.backend == "codex" {
            self.set_codex_mode(sid, res, Some(mode)).await;
        } else {
            self.set_claude_mode(sid, res, Some(mode)).await;
        }
    }

    /// `session/new` and `session/load` for `launch`: Claude's `_meta` for Claude, ACP's own parameters for a Codex
    /// backup (its servers as a list, the instructions as `systemPrompt` when it takes them).
    fn hq_params(&self, launch: &hq::Launch, session: Option<&str>) -> Value {
        if !launch.is_codex() {
            return launch.session_params(&self.cfg, session);
        }
        let servers: Vec<Value> = launch
            .mcp_servers
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, s)| {
                let env: Vec<Value> =
                    s["env"].as_object().into_iter().flatten().map(|(k, v)| json!({"name": k, "value": v})).collect();
                json!({"name": name, "command": s["command"], "args": s["args"], "env": env})
            })
            .collect();
        let mut params = json!({"cwd": launch.cwd(&self.cfg), "mcpServers": servers});
        if let Some(sid) = session {
            params["sessionId"] = sid.into();
        }
        let caps = self.conn.as_ref().map(|c| c.sessions);
        if caps.is_some_and(|c| c.additional_directories) && !launch.add_dirs.is_empty() {
            params["additionalDirectories"] = json!(launch.add_dirs);
        }
        if caps.is_some_and(|c| c.system_prompt) && !launch.system_prompt.is_empty() {
            params["systemPrompt"] = super::system_prompt(&launch.system_prompt);
        }
        params
    }

    /// [`Actor::ensure_session`] for an hq agent. Whenever a session has to be created or loaded (a first message, a
    /// restart, an idle release, a new session) hq is asked again how to start the agent, so it runs as hq says now.
    ///
    /// Which account answers is decided here, once per turn (see `agent::backups`): the first entry of the agent's
    /// chain that is ready and not out of usage. A turn already running is never moved.
    pub(super) async fn ensure_hq_session(&mut self, slot: Option<&str>) -> Result<()> {
        let agent = self.cfg.hq_agent.clone().ok_or_else(|| anyhow!("not an hq agent"))?;
        let chain = self.hq_chain(&agent).await;
        let mut skip: HashSet<String> = self.turn_tried.clone();
        let mut prepared = None;
        let person = self.cfg.person.clone();
        // The conversation this place's process serves: its run token's session, which an escape card is raised under.
        let conversation = format!("acp:{}:{}", self.lane.chat, self.lane.thread.as_deref().unwrap_or_default());
        let lane = self.lane.clone();
        let mut warm;
        let (mut launch, backup, mut reservation) = loop {
            let backup = match self.hub.backups.pick(&agent, &chain, &skip) {
                Pick::Primary => None,
                Pick::Backup(entry) => Some(entry.id),
                Pick::AllOut(outs) => return Err(NoAccount(self.hub.backups.all_out_text(&outs)).into()),
            };
            self.serving.clone_from(&backup);
            let current = self.hq_session(slot);
            // An invited person's turn asks the kernel again even when a session is warm: their membership may have
            // been taken away, their budget used up or their mode changed since the last turn.
            warm = matches!((&current, &self.conn), (Some(sid), Some(conn)) if conn.loaded.contains_key(sid))
                && self.conn_serving == self.serving
                && self.conn_room == self.run_room();
            // A process on another account than the main one is started again below: its launch says so.
            if warm && person.is_none() && self.conn_on_main(&chain) {
                self.turn_session = current;
                return Ok(());
            }
            // A person's activation takes the session's writer claim first: the once grants are reserved under it and
            // the session loads under it (access-arch rule 4). A process that isn't running has no pid to claim for
            // yet: its claim is taken before the load below.
            if person.is_some()
                && self.conn.is_some()
                && let Some(sid) = &current
            {
                self.claim_for_turn(sid, &lane).await?;
            }
            // The once grants the loaded session was given before stay with it.
            let carried = match (&current, &self.conn) {
                (Some(sid), Some(conn)) if warm => conn.profiles.get(sid).map(|p| p.holds.clone()).unwrap_or_default(),
                _ => Vec::new(),
            };
            if prepared.is_none() {
                if self.hub.store.kv_read(&format!("agent-env:{}", self.cfg.backend))?.is_some() {
                    crate::market::vault::unlock(self.hub.clone()).await?;
                }
                self.hub.set_runtime(&self.rt, |r| r.activity = "Starting agent…".into());
                let Some(bin) = tokio::task::spawn_blocking(hq::bin).await? else {
                    bail!("kith isn't installed on this computer (set KITH_KERNEL_BIN, or put `kith` on PATH)");
                };
                // The place's own folder or project, else the bot's default; hq runs in it so the kernel finds the project.
                let target = folder::resolve(&self.hub.store, &self.cfg, &self.lane_of(slot)).await;
                if let Some(notice) = &target.notice {
                    self.notice(notice, NoticeStyle::Divider);
                }
                prepared = Some((bin, target));
            }
            let Some((bin, target)) = &prepared else { unreachable!("prepared above") };
            let room = self.run_room();
            // A person's launch carries what their conversation was given beyond the box (a yes on an escape card, an
            // "always"), asked for with a token of its own: one per launch, since a run takes its access once. Its once
            // grants are reserved, and taken when the session started or loaded with them.
            let access = person.as_deref().and_then(|p| {
                crate::runauth::grant_for(p, &agent, Some(room.as_str()), &conversation, target.project.as_deref())
            });
            let reservation = access.as_ref().map(|(grant, _)| Reservation::open(grant.run_id(), carried));
            match hq::launch(
                bin,
                &agent,
                &target.cwd,
                target.project.as_deref(),
                backup.as_deref(),
                person.as_deref(),
                &room,
                &self.run_members(),
                access.as_ref(),
            )
            .await
            {
                Ok(mut launch) => {
                    // What lets `kith-call` ring this chat's person, and nobody else (see `calls::key`).
                    launch.env.extend(crate::calls::env_for(&self.hub, &self.cfg.id));
                    if target.explicit {
                        launch.cwd.clone_from(&target.cwd);
                    }
                    // Not for an invited person: send_file copies any path the Mac user can read, and the kernel
                    // confines that person's run to folders this tool doesn't check.
                    if person.is_none()
                        && let Ok(exe) = std::env::current_exe()
                    {
                        launch.with_files_server(&exe.to_string_lossy(), self.hub.port, &self.rt);
                    }
                    break (launch, backup, reservation);
                }
                // hq won't run this entry (not signed in, not allowed for this person): the next one answers.
                // The kernel said no to this person (not a member, over budget): no other account changes that.
                Err(error) if error.is::<hq::Refused>() => return Err(error),
                Err(error) if backup.is_some() => {
                    let id = backup.unwrap_or_default();
                    tracing::info!(entry = %id, error = format!("{error:#}"), "backup account skipped");
                    // What one person may not run says nothing about the entry for the owner and everyone else.
                    if person.is_none() {
                        self.hub.backups.note_unavailable(&agent, &id);
                    }
                    skip.insert(id);
                }
                Err(error) => return Err(error),
            }
        };
        // Only the agent's own Claude run on the owner's behalf switches models for a call.
        self.call_models = (backup.is_none() && person.is_none() && !launch.is_codex()).then(|| {
            super::call_model::CallModels { own: launch.model.clone(), launch_call: launch.call_model.clone() }
        });
        // What a person's session runs with (M6): compared with what each loaded session was given at every turn.
        let digest = person.as_ref().map(|_| launch.profile_digest());
        // The process carries kith's env (KITH_PROJECT, the cost tag) and its folder: a place that needs others gets a
        // process of its own, started again here (the sessions it held load again on their next turn).
        let mut started_as = (launch.env(), launch.cwd(&self.cfg).to_owned());
        if person.is_some() {
            // The membership's mode is set when a session starts: a changed one starts the process again.
            started_as.0.push(("permission_mode".into(), launch.permission_mode.clone().unwrap_or_default()));
        }
        if self.conn.is_some()
            && (self.conn_started_as.as_ref() != Some(&started_as) || self.conn_serving != self.serving)
            && let Some(c) = self.conn.take()
        {
            c.acp.kill().await;
        }
        // A warm session whose profile changed (a yes on an escape card, a grant taken away, a demotion) is loaded again
        // with this launch before its turn: that session alone, in the same process, so the other threads' sessions
        // and their background work go on. A narrowing never waits: the turn after it runs on the new profile (M7).
        let mut reload = None;
        let sid = if warm && self.conn.is_some() { self.hq_session(slot) } else { None };
        if warm && let Some(conn) = self.conn.as_mut() {
            let changed = match (&digest, &sid) {
                (Some(digest), Some(sid)) => conn.profiles.get(sid).is_none_or(|p| &p.digest != digest),
                _ => false,
            };
            if !changed {
                if let (Some(reservation), Some(sid)) = (reservation.take(), &sid) {
                    let holds = reservation.commit(&self.hub.store);
                    if let Some(profile) = conn.profiles.get_mut(sid) {
                        profile.holds = holds;
                    }
                }
                self.turn_session = sid;
                return Ok(());
            }
            if let Some(sid) = &sid {
                tracing::info!(bot = %self.cfg.id, session = %sid, "the session's profile changed; loading it again");
                conn.loaded.remove(sid);
            }
            reload = sid;
        }
        // A backup has no session of the agent's to load: it starts one and is told the conversation. A reload is of the
        // session this process holds, on the account it runs on.
        let mut current = reload.clone().or_else(|| self.hq_session(slot).filter(|_| backup.is_none()));
        // A session is loaded only under the account that made it: after a change of the main account the agent starts
        // a new one, told the conversation, rather than fail to load (or load) another login's.
        let mut moved = false;
        if reload.is_none()
            && let Some(sid) = &current
        {
            let made = hq::session_account(&self.hub.store, sid);
            if !made.as_ref().is_ok_and(|made| launch.may_load(made.as_deref())) {
                tracing::info!(session = %sid, made = ?made.ok().flatten(), now = ?launch.account_id, "session made under another account; starting a new one");
                current = None;
                moved = true;
            }
        }
        if self.conn.is_none() {
            self.conn_started_as = Some(started_as);
            self.conn_serving.clone_from(&self.serving);
            self.conn_account.clone_from(&launch.account_id);
            self.conn_room = self.run_room();
            let provider = if launch.is_codex() { "codex" } else { self.cfg.backend.as_str() };
            let mut env = process_env(crate::agent::auth::env(&self.hub.store, provider)?, &launch, backup.is_some());
            // Anyone but the owner asks the host who they are (`kith recall` over the run socket), with a token that
            // is theirs alone; the owner's process gets none.
            let mut grant = person.as_deref().and_then(|p| {
                let room = launch.env.get("KITH_ROOM").map(String::as_str);
                let project = launch.env.get("KITH_PROJECT").map(String::as_str);
                crate::runauth::grant_for(p, &agent, room, &conversation, project)
            });
            if let Some((_, vars)) = &grant {
                env.extend(vars.iter().cloned());
            }
            let candidates = match std::env::var(hq::ACP_COMMAND_ENV) {
                Ok(command) if !command.is_empty() => vec![command],
                _ => {
                    let (hub, rt) = (self.hub.clone(), self.rt.clone());
                    // A Codex backup starts Codex's adapter, not the bot's own.
                    let mut cfg = self.cfg.clone();
                    if launch.is_codex() {
                        cfg.backend = "codex".into();
                        cfg.command = None;
                    }
                    launch_commands(&cfg, move |msg: &str| {
                        let msg = msg.to_owned();
                        hub.set_runtime(&rt, |r| r.activity = msg);
                    })
                    .await?
                }
            };
            let mut last_err = None;
            for (i, command) in candidates.iter().enumerate() {
                // A local CLI too old for ACP just hangs; don't make the user wait long before the fallback.
                let budget = Duration::from_secs(if i + 1 < candidates.len() { 20 } else { 180 });
                match start_agent(command, launch.cwd(&self.cfg), &env, budget).await {
                    Ok(mut conn) => {
                        if let Some((grant, _)) = grant.take() {
                            if let Some(pid) = conn.acp.pid() {
                                grant.bind_pid(pid);
                            }
                            conn.run_grant = Some(grant);
                        }
                        self.conn = Some(conn);
                        break;
                    }
                    Err(e) => {
                        tracing::info!(command, error = format!("{e:#}"), "agent failed to start");
                        last_err = Some(e);
                    }
                }
            }
            if self.conn.is_none() {
                return Err(last_err.unwrap_or_else(|| anyhow!("agent failed to start")));
            }
        }
        if let Some(sid) = current {
            if self.conn.as_ref().is_some_and(|c| c.sessions.load) {
                self.hub.set_runtime(&self.rt, |r| r.activity = "Resuming session…".into());
                // The writer claim comes before the load (access-arch rule 4).
                self.claim_for_turn(&sid, &lane).await?;
                let params = self.hq_params(&launch, Some(&sid));
                let res = self.session_request("session/load", params).await;
                let Some(conn) = self.conn.as_mut() else { return res.map(drop) };
                // The agent replays history as session/update before answering; drop it.
                while let Ok(inc) = conn.rx.try_recv() {
                    if let Incoming::Request { id, .. } = inc {
                        let _ = conn.acp.respond_error(id, -32601, "not supported").await;
                    }
                }
                match res {
                    Ok(res) => {
                        conn.loaded.insert(sid.clone(), Instant::now());
                        conn.session_started(&sid, &res);
                        // Loaded with them: the once grants are taken, before the first turn (M4).
                        if let Some(digest) = &digest {
                            let holds = reservation.take().map(|r| r.commit(&self.hub.store)).unwrap_or_default();
                            conn.profiles.insert(sid.clone(), Profile { digest: digest.clone(), holds });
                        }
                        self.note_options(&sid, &res, Listing::Session);
                        if !launch.is_codex() {
                            self.set_claude_mode(&sid, &res, launch.permission_mode.as_deref()).await;
                        }
                        self.turn_session = Some(sid);
                        // The agent's own account came back to a session that missed what a backup answered.
                        self.preface = self.recovery_preface();
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(error = format!("{e:#}"), "session/load failed; starting fresh");
                        self.notice(
                            "Couldn't resume the previous session — starting a fresh one.",
                            NoticeStyle::Divider,
                        );
                    }
                }
            } else {
                self.notice(
                    "This agent can't resume sessions after a restart — starting a fresh one.",
                    NoticeStyle::Divider,
                );
                self.set_session(slot, None);
            }
        }
        if moved {
            self.notice("The agent's main account changed — starting a fresh session.", NoticeStyle::Divider);
        }
        let params = self.hq_params(&launch, None);
        let res = self.session_request("session/new", params).await?;
        let Some(conn) = self.conn.as_mut() else { bail!("agent not running") };
        let sid = res["sessionId"].as_str().ok_or_else(|| anyhow!("agent returned no sessionId"))?.to_owned();
        conn.loaded.insert(sid.clone(), Instant::now());
        conn.session_started(&sid, &res);
        if let Some(digest) = &digest {
            let holds = reservation.take().map(|r| r.commit(&self.hub.store)).unwrap_or_default();
            conn.profiles.insert(sid.clone(), Profile { digest: digest.clone(), holds });
        }
        // The model and effort are hq's (launch-json): the host sets neither again. The permission mode is set once,
        // below, because the adapter starts every session in "default".
        self.note_options(&sid, &res, Listing::Session);
        if backup.is_none() {
            hq::tag_session(&self.hub.store, &sid, launch.account_id.as_deref());
        }
        self.hq_set_session(slot, &sid);
        if launch.is_codex() {
            self.set_codex_mode(&sid, &res, launch.permission_mode.as_deref()).await;
        } else {
            self.set_claude_mode(&sid, &res, launch.permission_mode.as_deref()).await;
        }
        self.session_fresh = true;
        self.turn_session = Some(sid);
        if backup.is_some() {
            self.preface = self.backup_preface();
            // Codex takes the agent's instructions as ACP's `systemPrompt`; one that can't gets them first.
            if launch.is_codex()
                && !self.conn.as_ref().is_some_and(|c| c.sessions.system_prompt)
                && !launch.system_prompt.is_empty()
            {
                let prompt = std::mem::take(&mut launch.system_prompt);
                let profile = format!("<agent-profile>\n{prompt}\n</agent-profile>");
                self.preface = Some(match self.preface.take() {
                    Some(preface) => format!("{profile}\n\n{preface}"),
                    None => profile,
                });
            }
        } else if moved {
            self.preface = self.account_preface();
        } else {
            self.preface = self.recovery_preface();
        }
        if let Some(root) = slot {
            self.thread_intro = self.thread_intro(root, false);
        }
        Ok(())
    }
}

/// [`Actor::run_members`] for the group `chat`, run by the bot of `run_person` (`None` is the owner's).
fn room_members(store: &crate::store::Store, chat: &str, run_person: Option<&str>) -> hq::Members {
    use hq::Members::{OwnerOnly, People, Unknown};
    let Ok(Some(group)) = store.bot(chat) else { return Unknown };
    if group.deleted {
        return Unknown;
    }
    match group_people(store, &group.config.members) {
        None => Unknown,
        Some(people) if !people.is_empty() => People(people),
        Some(_) if run_person.is_some() => Unknown,
        Some(_) => OwnerOnly,
    }
}

/// The person each of these bots runs as (an invitee's, or a bot guest's), sorted and without repeats; a deleted bot or
/// one the store does not have counts for nobody. `None` when the store failed to answer for any of them: that is not
/// "nobody", and the caller must not read it as owner only.
fn group_people(store: &crate::store::Store, members: &[String]) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    for id in members {
        match store.bot(id) {
            Err(_) => return None,
            Ok(Some(r)) if !r.deleted => ids.extend(r.config.person),
            Ok(_) => {}
        }
    }
    ids.sort();
    ids.dedup();
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(provider: &str, account: &str) -> hq::Launch {
        serde_json::from_value(json!({"harness": provider, "env": {"KITH_ACCOUNT": account}})).unwrap()
    }

    fn keys() -> Vec<(String, String)> {
        vec![("ANTHROPIC_API_KEY".into(), "k".into()), ("CLAUDE_CODE_OAUTH_TOKEN".into(), "t".into())]
    }

    fn names(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn a_backup_never_runs_on_the_primarys_saved_keys() {
        let primary = process_env(keys(), &launch("claude", ""), false);
        assert!(names(&primary).contains(&"ANTHROPIC_API_KEY"), "the agent's own login keeps its keys");
        for (harness, account) in [("claude", "work"), ("claude", ""), ("codex", "work")] {
            let env = process_env(keys(), &launch(harness, account), true);
            assert!(
                !names(&env).iter().any(|k| ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"].contains(k)),
                "{harness}:{account} kept a host key: {:?}",
                names(&env)
            );
            assert!(names(&env).contains(&"KITH_ACCOUNT"), "kith's own env still goes");
        }
        // The ambient Codex login uses the keys saved for Codex.
        assert!(
            names(&process_env(vec![("OPENAI_API_KEY".into(), "k".into())], &launch("codex", ""), true))
                .contains(&"OPENAI_API_KEY")
        );
    }

    #[test]
    fn a_main_account_of_its_own_runs_without_the_default_logins_saved_keys() {
        // The agent's main account is `claude:work` (or an API profile): its own login, not the host's keys.
        for account in ["work", "main"] {
            let env = process_env(keys(), &launch("claude", account), false);
            assert!(
                !names(&env).iter().any(|k| ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"].contains(k)),
                "the main account {account} kept a host key: {:?}",
                names(&env)
            );
        }
        // The default login as the main account keeps them, as it always did.
        assert!(names(&process_env(keys(), &launch("claude", ""), false)).contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn claudes_permission_modes_map_to_codexs() {
        assert_eq!(codex_mode("default"), Some("read-only"));
        assert_eq!(codex_mode("plan"), Some("read-only"));
        assert_eq!(codex_mode("acceptEdits"), Some("auto"));
        assert_eq!(codex_mode("auto"), Some("auto"));
        assert_eq!(codex_mode("bypassPermissions"), Some("full-access"));
        assert_eq!(codex_mode("surprise"), None);
    }

    #[test]
    fn a_group_rooms_people_are_the_persons_its_member_bots_run_as() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        for (id, person) in
            [("m1", json!("sam")), ("m2", json!("sam")), ("m3", json!(null)), ("m4", json!("bot-1234abcd"))]
        {
            let cfg = json!({"id": id, "name": id, "backend": "claude", "cwd": "/tmp", "person": person});
            store.save_bot(&serde_json::from_value(cfg).unwrap()).unwrap();
        }
        let all: Vec<String> = ["m1", "m2", "m3", "m4", "gone"].map(String::from).to_vec();
        // The owner's bot (no person) and an unknown bot add nobody; a person in two bots counts once.
        assert_eq!(group_people(&store, &all).unwrap(), ["bot-1234abcd", "sam"]);
        assert_eq!(group_people(&store, &["m3".to_owned()]), Some(vec![]), "read fine, owner only");
    }

    #[test]
    fn only_a_fresh_read_that_finds_the_owner_alone_is_owner_only() {
        use hq::Members::{OwnerOnly, People, Unknown};
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let save = |id: &str, person: Value, members: Value, deleted: bool| {
            let cfg =
                json!({"id": id, "name": id, "backend": "claude", "cwd": "/tmp", "person": person, "members": members});
            store.save_bot(&serde_json::from_value(cfg).unwrap()).unwrap();
            if deleted {
                store.delete_bot(id).unwrap();
            }
        };
        save("mine", json!(null), json!([]), false);
        save("mine2", json!(null), json!([]), false);
        save("theirs", json!("sam"), json!([]), false);
        save("gone", json!("sam"), json!([]), true);
        save("solo", json!(null), json!(["mine", "mine2", "gone"]), false);
        save("mixed", json!(null), json!(["mine", "theirs"]), false);
        save("dead-group", json!(null), json!(["mine"]), true);
        assert_eq!(room_members(&store, "solo", None), OwnerOnly, "owner's bots and a deleted guest");
        assert_eq!(room_members(&store, "mixed", None), People(vec!["sam".to_owned()]));
        assert_eq!(room_members(&store, "nope", None), Unknown, "group not found is a failed read");
        assert_eq!(room_members(&store, "dead-group", None), Unknown, "a group deleted mid-run");
        assert_eq!(room_members(&store, "solo", Some("sam")), Unknown, "a person's own run is never owner only");
    }
}
