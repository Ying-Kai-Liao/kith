//! Bot and chat methods: roster, messages, uploads, memory, routines and the bots' own MCP calls.

use super::marketplace::connector_ids;
use super::{actor_arg, str_arg};
use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::market;
use crate::store::{BotConfig, EntryKind, Lane, ReadScope, now_ms};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

/// The lane's newest conversation as the apps draw it, `null` when it never had one.
fn lane_conversation(hub: &Hub, lane: &Lane) -> Value {
    hub.store.newest_conversation(lane).map_or(Value::Null, |c| crate::chat::group::conversation_view(hub, &c))
}

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "routineSchedule" => crate::routines::schedule_preview(&b, now_ms())?,
        "routines" => hub.routines.list(str_arg(&b, "botId")?),
        "saveRoutine" => hub.routines.save(hub, str_arg(&b, "botId")?, &b)?,
        "setRoutineEnabled" => hub.routines.set_enabled(
            &hub.store,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            b["enabled"].as_bool().context("enabled is required")?,
        )?,
        "deleteRoutine" => hub.routines.remove(&hub.store, str_arg(&b, "botId")?, str_arg(&b, "id")?)?,
        "runRoutine" => hub.routines.enqueue(
            &hub.store,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            json!({"source":"test"}),
            None,
            true,
        )?,
        "routineWebhook" => hub.routines.credentials(
            hub,
            str_arg(&b, "botId")?,
            str_arg(&b, "id")?,
            b["rotate"].as_bool().unwrap_or(false),
        )?,
        "routineCall" => crate::routines::call(hub, str_arg(&b, "botId")?, str_arg(&b, "name")?, &b["arguments"])?,
        "memoryCall" => {
            let (bot, name) = (str_arg(&b, "botId")?, str_arg(&b, "name")?);
            crate::chat::memory::tool_call(hub, bot, actor_arg(&b, bot), name, &b["arguments"]).await?
        }
        "chatCall" => {
            let (bot, name) = (str_arg(&b, "botId")?, str_arg(&b, "name")?);
            if name == "call_person" {
                let reason = b["arguments"]["reason"].as_str().unwrap_or_default();
                crate::calls::ring_and_wait(hub, bot, reason, false).await?
            } else if name == crate::cards::secret::TOOL {
                crate::cards::secret::chat_request(hub, bot, actor_arg(&b, bot), &b["arguments"]).await?
            } else if crate::chat::profile::TOOLS.contains(&name) {
                crate::chat::profile::call(hub, bot, name, &b["arguments"]).await?
            } else {
                crate::chat::outbox::call(hub, actor_arg(&b, bot), name, &b["arguments"]).await?
            }
        }
        "teamCall" => {
            let (bot, name) = (str_arg(&b, "botId")?, str_arg(&b, "name")?);
            crate::chat::team::call(hub, actor_arg(&b, bot), name, &b["arguments"]).await?
        }
        "history" => {
            let bot = str_arg(&b, "botId")?;
            let before = b["beforeSeq"].as_i64().unwrap_or(i64::MAX);
            let limit = b["limit"].as_i64().unwrap_or(100).clamp(1, 500);
            let mut entries = hub.store.history(bot, before, limit)?;
            crate::chat::memory::summary::hide_forgotten(&hub.store, &mut entries);
            let conversation = lane_conversation(hub, &Lane::main(bot));
            json!({"entries": entries, "folder": hub.place_folder(bot, &Lane::main(bot)).await?, "conversation": conversation})
        }
        // A thread's replies (its newest 500); the root is in the main chat.
        "thread" => {
            let (bot, root) = (str_arg(&b, "botId")?, str_arg(&b, "rootId")?);
            let lane = Lane::in_thread(bot, root);
            let mut entries = hub.store.thread(bot, root, 500)?;
            crate::chat::memory::summary::hide_forgotten(&hub.store, &mut entries);
            let conversation = lane_conversation(hub, &lane);
            json!({"entries": entries, "folder": hub.place_folder(bot, &lane).await?, "conversation": conversation})
        }
        // Notices between a bot and one peer (the newest 200), for the read-only bot conversation sheet.
        "botConversation" => {
            let bot = str_arg(&b, "botId")?;
            let entries = hub.store.bot_conversation(bot, str_arg(&b, "peerId")?, 200)?;
            json!({"entries": entries, "folder": hub.place_folder(bot, &Lane::main(bot)).await?})
        }
        "createBot" => {
            let mut b = b;
            b["id"] = "".into();
            // Only the hq sync makes a bot of an hq agent.
            b["hqAgent"] = Value::Null;
            // Nor can a client make a chat that belongs to an invited person.
            b["person"] = Value::Null;
            // Connectors are on for a new bot unless the client picked them.
            // Without a credential store (headless Linux) no connector can run, so the bot starts with none.
            if b["connectors"].is_null() && b["kind"] != "group" {
                b["connectors"] = match market::vault::unlock(hub.clone()).await {
                    Ok(()) => json!(connector_ids(&hub.store)?),
                    Err(e) => {
                        tracing::warn!(
                            error = format!("{e:#}"),
                            "credential store unavailable; new bot starts without connectors"
                        );
                        json!([])
                    }
                };
            }
            // Skills too: every installed one is on unless the client picked them.
            if b["skills"].is_null() && b["kind"] != "group" {
                b["skills"] = json!(market::skills(&hub.store).into_iter().map(|s| s.id).collect::<Vec<_>>());
            }
            let cfg: BotConfig = serde_json::from_value(b).context("invalid bot")?;
            let hub = hub.clone();
            json!({"bot": tokio::task::spawn_blocking(move || hub.create_bot(cfg)).await??})
        }
        // Looks for hq agents now; the answer says what changed (the roster events carry the bots).
        "syncHqBots" => {
            let hub = hub.clone();
            json!(hub.sync_hq_bots().await?.json())
        }
        "updateBot" => {
            let hub = hub.clone();
            json!({"bot": tokio::task::spawn_blocking(move || hub.update_bot(&b)).await??})
        }
        // The roster as the user dragged it, top first.
        "reorderBots" => {
            let ids: Vec<String> = serde_json::from_value(b["ids"].clone()).context("ids is required")?;
            let hub = hub.clone();
            tokio::task::spawn_blocking(move || hub.reorder_bots(&ids)).await??;
            json!({})
        }
        "deleteBot" => {
            hub.delete_bot(str_arg(&b, "botId")?)?;
            json!({})
        }
        // The main chat, one thread (`threadId`), or everything (`all`, the roster's "Mark as read").
        "markRead" => {
            let scope = match b["threadId"].as_str().filter(|t| !t.is_empty()) {
                Some(root) => ReadScope::Thread(root),
                None if b["all"] == true => ReadScope::All,
                None => ReadScope::Chat,
            };
            hub.mark_read(str_arg(&b, "botId")?, scope)?;
            json!({})
        }
        // One chunk of a file for a later `send` (`attachments`); base64 `data` at `offset`.
        "upload" => {
            use base64::Engine as _;
            let row = hub.store.bot(str_arg(&b, "botId")?)?.filter(|r| !r.deleted);
            let root = crate::chat::uploads::root(&row.ok_or_else(|| anyhow!("unknown bot"))?.config);
            let (id, name) = (str_arg(&b, "uploadId")?.to_owned(), str_arg(&b, "name")?.to_owned());
            let data =
                base64::engine::general_purpose::STANDARD.decode(str_arg(&b, "data")?).context("invalid data")?;
            let (offset, done) = (b["offset"].as_u64().unwrap_or(0), b["done"] == true);
            // Set by `member_call` only, so an invited person's bytes count against their quota.
            let guest = b["asMember"].as_str().map(str::to_owned);
            let attachment = tokio::task::spawn_blocking(move || {
                let who = match &guest {
                    Some(p) => crate::chat::uploads::Uploader::Guest(p),
                    None => crate::chat::uploads::Uploader::Owner,
                };
                crate::chat::uploads::append(&root, &id, &name, offset, &data, done, who)
            })
            .await??;
            json!({"attachment": attachment})
        }
        // A sent file, back in chunks (chat previews on other devices).
        "readUpload" => {
            use base64::Engine as _;
            let row = hub.store.bot(str_arg(&b, "botId")?)?.filter(|r| !r.deleted);
            let root = crate::chat::uploads::root(&row.ok_or_else(|| anyhow!("unknown bot"))?.config);
            let id = str_arg(&b, "uploadId")?.to_owned();
            let offset = b["offset"].as_u64().unwrap_or(0);
            let (data, size) =
                tokio::task::spawn_blocking(move || crate::chat::uploads::read(&root, &id, offset, 384 * 1024))
                    .await??;
            json!({"data": base64::engine::general_purpose::STANDARD.encode(data), "size": size})
        }
        "send" => {
            let bot = str_arg(&b, "botId")?;
            let text = b["text"].as_str().unwrap_or_default().trim();
            let uploads: Vec<&str> =
                b["attachments"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if text.is_empty() && uploads.is_empty() {
                bail!("empty message");
            }
            let nonce = b["clientNonce"].as_str().unwrap_or_default();
            if !nonce.is_empty()
                && let Some(e) = hub.store.find_by_nonce(bot, nonce)
            {
                return Ok(ControlFlow::Break(json!({"entry": e})));
            }
            let row = hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            // `project` / `folder`: where the place this message runs in works, set before the turn starts, so a
            // new chat, thread or task picks its folder without a reset round trip. Checked before anything is saved.
            let start_in = hub.check_change(crate::agent::folder::Change::from_args(&b)?).await?;
            if start_in.is_some() && row.config.is_group() {
                bail!("A group has no folder; set it on a member");
            }
            // `threadId`: reply in the thread on that message of the main chat (threads are flat).
            let thread = match b["threadId"].as_str().filter(|t| !t.is_empty()) {
                Some(root) => {
                    let r = hub.store.entry(root).ok_or_else(|| anyhow!("unknown message"))?;
                    if r.bot_id != bot || r.thread_id.is_some() {
                        bail!("threads start from a message in the main chat");
                    }
                    Some(root.to_owned())
                }
                // In a bot's chat, a message goes on with the current task, or starts one (see `tasks`).
                None if row.config.uses_tasks() => {
                    crate::chat::tasks::continued(hub, &row.config, b["newTask"] == true, now_ms())
                }
                None => None,
            };
            // A terminal thread belongs to the terminal while it lives there. The owner's own message to an agent's
            // thread then waits as a wake until the terminal lets go (never typed in, never a second writer); anyone
            // else's is refused, and nothing is saved.
            let mut waits = false;
            if let Some(root) = &thread {
                let may_wait = row.config.person.is_none()
                    && !row.config.is_group()
                    && row.config.hq_agent.is_some()
                    && b["asMember"].is_null()
                    && hub.store.bot_conversation_of_thread(bot, root).is_none();
                waits = crate::terminal::send_gate(hub, bot, root, may_wait).await? == crate::terminal::SendGate::Waits;
            }
            let starts_task = thread.is_none() && row.config.uses_tasks();
            let lane = Lane { chat: bot.to_owned(), thread };
            let mut attachments = Vec::new();
            if !uploads.is_empty() {
                let root = crate::chat::uploads::root(&row.config);
                for id in uploads {
                    attachments.push(crate::chat::uploads::resolve(&root, id)?.1);
                }
            }
            // `/skill …` reaches the agent as "read this skill first"; the chat keeps what was typed.
            // `via: "call"`: spoken in a voice call. Its turn gets the call note and the call model; the chat keeps
            // only what was said. Any other value (or none, from an older app) is a typed message.
            let call = b["via"] == "call";
            let prompt = crate::chat::prompt::of_message(&hub.store, &row.config, text, &attachments, call);
            let turn = hub.store.max_turn(bot) + 1;
            // A group has no agent of its own to queue behind: its message is simply sent.
            let status = if row.config.is_group() { "sent" } else { "queued" };
            let mut data = json!({"text": text, "clientNonce": nonce, "status": status, "attachments": attachments});
            if call {
                data["via"] = "call".into();
            }
            // What the user pasted rather than typed: memory never takes it as the user's own words.
            let pasted: Vec<&str> = b["pasted"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|p| !p.is_empty() && text.contains(p))
                .collect();
            if !pasted.is_empty() {
                data["pasted"] = json!(pasted);
            }
            if starts_task {
                crate::chat::tasks::mark(&mut data);
            }
            // Before the message is saved, so a failure leaves nothing behind; a new task's place exists only once its
            // first message does.
            if let Some(checked) = start_in.as_ref().filter(|_| !starts_task) {
                hub.apply_change(bot, &lane, checked)?;
            }
            let e = hub
                .add_entry(&lane, EntryKind::User, turn, &data)
                .ok_or_else(|| anyhow!("couldn't save the message"))?;
            // A new task's turn runs in its own thread.
            let turn_lane = if starts_task { Lane::in_thread(bot, &e.id) } else { lane.clone() };
            if let Some(checked) = start_in.as_ref().filter(|_| starts_task)
                && let Err(error) = hub.apply_change(bot, &turn_lane, checked)
            {
                // The task never starts: take the message back.
                let mut data = e.data.clone();
                data["status"] = "cancelled".into();
                let _ = hub.store.update_entry(&e.id, &data);
                return Err(error);
            }
            // Before the turn starts, so its reply lands unread.
            let scope = lane.thread.as_deref().map_or(ReadScope::Chat, ReadScope::Thread);
            if let Err(error) = hub.mark_read(bot, scope) {
                tracing::warn!(%error, bot, "couldn't mark bot read after send");
            }
            if waits {
                let agent = row.config.hq_agent.as_deref().unwrap_or_default();
                if let Err(error) = crate::cards::resume::wait_for_terminal(hub, agent, &e.id).await {
                    // Nothing will run it: say so rather than leave it queued for good.
                    let mut data = e.data.clone();
                    data["status"] = "failed".into();
                    hub.set_entry(&e.id, &data);
                    return Err(error.context("couldn't queue the message for the terminal's thread"));
                }
                return Ok(ControlFlow::Break(json!({"entry": hub.store.entry(&e.id).unwrap_or(e)})));
            }
            if row.config.is_group() {
                // The sender grants the turn budget (rule C1); a plain message gets today's ten.
                let budget = crate::chat::group::budget_of(b["budget"].as_i64());
                crate::chat::group::start(hub, &lane, budget, b["asMember"].as_str().unwrap_or("owner"), &e.id)?;
            } else {
                hub.send_cmd(bot, Cmd::Send { lane: turn_lane, entry_id: e.id.clone(), text: prompt, route: true })?;
            }
            json!({"entry": e})
        }
        // A finished voice call, as a line in the chat ("Voice chat · 0:16").
        "logCall" => {
            let bot = str_arg(&b, "botId")?;
            hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            hub.calls.ended(bot);
            let seconds = b["seconds"].as_u64().unwrap_or(0);
            let data = json!({"text": "Voice chat", "callSeconds": seconds});
            let e = hub.add_entry(&Lane::main(bot), EntryKind::Notice, hub.store.max_turn(bot), &data);
            json!({"entry": e})
        }
        // A voice call that just connected: its line goes in the chat now, so it sits where the call began; `endCall`
        // fills in the length. `callLive` is the "in progress" mark; the apps stop showing it after a few hours.
        "startCall" => {
            let bot = str_arg(&b, "botId")?;
            hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            let data = json!({"text": "Voice chat", "callLive": true});
            // None means the line wasn't saved: an error, so the app falls back to `logCall` instead of waiting on a null.
            let e = hub
                .add_entry(&Lane::main(bot), EntryKind::Notice, hub.store.max_turn(bot), &data)
                .ok_or_else(|| anyhow!("couldn't save the call"))?;
            json!({"entry": e})
        }
        // The call's end: the length goes on its line. Sending it twice (a retry) changes nothing the second time.
        // Without `entryId` (a call that never connected) it only frees the person for the next call.
        "endCall" => {
            let bot = str_arg(&b, "botId")?;
            hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
            hub.calls.ended(bot);
            if let Some(id) = b["entryId"].as_str() {
                let e = hub.store.entry(id).filter(|e| e.bot_id == bot && e.kind == EntryKind::Notice.as_str());
                // Only a call line (live, or already filled in by an earlier `endCall`): members may call this, and
                // must not be able to stamp a length on any other notice of the chat.
                let is_call = |e: &crate::store::Entry| e.data["callLive"] == true || !e.data["callSeconds"].is_null();
                let mut e = e.filter(is_call).ok_or_else(|| anyhow!("unknown call"))?;
                if e.data["callSeconds"].is_null() {
                    if let Some(data) = e.data.as_object_mut() {
                        data.remove("callLive");
                        data.insert("callSeconds".into(), b["seconds"].as_u64().unwrap_or(0).into());
                    }
                    e = hub.set_entry(id, &e.data).ok_or_else(|| anyhow!("couldn't save the call"))?;
                }
                json!({"entry": e})
            } else {
                json!({})
            }
        }
        // Stops where the user pressed it: the bot's chat, or the thread `threadId`; `all` stops the bot everywhere.
        // A group stops as a whole.
        "stop" => {
            let bot = str_arg(&b, "botId")?;
            match hub.store.bot(bot)? {
                Some(row) if row.config.is_group() => crate::chat::group::stop(hub, bot),
                _ if b["all"] == true => hub.send_cmd(bot, Cmd::Stop)?,
                _ => {
                    let lane =
                        b["threadId"].as_str().map_or_else(|| Lane::main(bot), |root| Lane::in_thread(bot, root));
                    hub.stop_place(bot, &lane)?;
                }
            }
            json!({})
        }
        // Takes back a message of the user's that still waits for its turn; taking it back twice is fine.
        "unsend" => {
            let (bot, entry_id) = (str_arg(&b, "botId")?, str_arg(&b, "entryId")?);
            let e = hub.store.entry(entry_id).filter(|e| e.bot_id == bot && e.kind == EntryKind::User.as_str());
            let e = e.ok_or_else(|| anyhow!("unknown message"))?;
            let unsent = e.data["status"] == "cancelled"
                || (e.data["status"] == "queued"
                    && (hub.unsend(bot, entry_id).await? || crate::cards::resume::unsend_waiting(hub, &e).await));
            if !unsent {
                bail!("This message already went to the bot. Stop ends its turn.");
            }
            json!({"entry": hub.store.entry(entry_id)})
        }
        "newSession" => {
            hub.send_cmd(str_arg(&b, "botId")?, Cmd::NewSession)?;
            json!({})
        }
        "memory" | "saveMemory" | "pinMemory" | "archiveMemory" | "forgetMemory" | "clearMemory" | "undoTidy"
        | "undoDream" | "answerMemory" | "undoMemory" | "memoryFile" | "saveMemoryFile" | "deleteMemoryFile"
        | "restoreMemoryFile" => crate::chat::memory::call(hub, method, &b).await?,
        m if crate::chat::memory::HQ_METHODS.contains(&m) => {
            let (hub, method, b) = (hub.clone(), m.to_owned(), b.clone());
            tokio::task::spawn_blocking(move || crate::chat::memory::hq_call(&hub, &method, &b)).await??
        }
        "editHandoff" | "keepConversation" => crate::chat::handoff::call(hub, method, &b)?,
        "react" => {
            let e = hub.react(str_arg(&b, "entryId")?, str_arg(&b, "emoji")?)?;
            json!({"entry": e})
        }
        "respondPermission" => {
            let entry_id = str_arg(&b, "entryId")?;
            let e = hub.store.entry(entry_id).ok_or_else(|| anyhow!("unknown card"))?;
            let option_id = b["optionId"].as_str().map(str::to_owned);
            // A key or account card has its own answers (a grant or a deny), and no process waits on it.
            if hub.store.card_by_entry(entry_id).is_some_and(|c| c.tool == crate::cards::secret::TOOL) {
                return Ok(ControlFlow::Break(
                    crate::cards::secret::respond(hub, entry_id, option_id.as_deref()).await?,
                ));
            }
            // A delegation chain's cap card (delegation.rs) has no process or messages either: raise the cap, or leave it.
            if e.data["delegation"].is_object() {
                let by = b["asPerson"].as_str().unwrap_or("owner");
                return Ok(ControlFlow::Break(crate::delegation::respond(hub, &e, option_id.as_deref(), by).await?));
            }
            // A limit card holds messages, not a process: its answer runs them, raises the limit, or keeps them.
            if e.data["limit"].is_object() {
                let by = b["asPerson"].as_str().unwrap_or("owner");
                return Ok(ControlFlow::Break(crate::limits::respond(hub, &e, option_id.as_deref(), by).await?));
            }
            // A card in a group belongs to the bot that asked.
            let bot = e.data["author"].as_str().unwrap_or(&e.bot_id);
            // In an invited person's chat only what the card offered can be picked, by anyone (the owner included).
            let invitee_chat = hub.store.bot(bot).ok().flatten().is_some_and(|r| r.config.person.is_some());
            if invitee_chat && option_id.as_deref().is_some_and(|o| !super::roles::offered(&e.data, o)) {
                bail!("That isn't one of the choices on this request.");
            }
            // The card store decides whether the answer counts (A1) and records it; an answer for a process that is up
            // goes on to the bot's actor, which holds the request id.
            match crate::cards::answer(hub, entry_id, option_id.as_deref(), b["asPerson"].as_str())? {
                crate::cards::Outcome::Closed => json!({"closed": true}),
                crate::cards::Outcome::Recorded { forward } => {
                    if forward
                        && let Err(e) = hub.send_cmd(bot, Cmd::Permission { entry_id: entry_id.to_owned(), option_id })
                    {
                        tracing::warn!(error = format!("{e:#}"), bot, "the bot was gone before it heard the answer");
                    }
                    json!({})
                }
            }
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
