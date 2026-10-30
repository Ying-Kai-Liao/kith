//! Agent → client: `session/update`s, permission requests and streamed text mapped onto entries.

use super::fast_mode::Listing;
use super::{Actor, Seg, SegKind};
use crate::access::{self, Ask, Request};
use crate::agent::acp::{self, Incoming};
use crate::chat::tasks::TaskState;
use crate::hub::BotStatus;
use crate::remote::push::{self, AlertKind};
use crate::store::{EntryKind, NewCard, Opened};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// What an invited person may pick on a card: allow or reject this once. Everything else the adapter offers changes
/// more than the one call: "don't ask again" writes a rule into the agent's shared folder, and the exit-plan options
/// switch the session to bypass, auto or accept-edits.
fn once_only(options: Vec<Value>) -> Vec<Value> {
    options.into_iter().filter(|o| matches!(o["kind"].as_str(), Some("allow_once" | "reject_once"))).collect()
}

impl Actor {
    pub(super) async fn on_incoming(&mut self, inc: Option<Incoming>) {
        match inc {
            None | Some(Incoming::Closed { .. }) => {
                let tail = match inc {
                    Some(Incoming::Closed { stderr_tail }) => stderr_tail,
                    _ => String::new(),
                };
                self.conn = None;
                // The in-flight prompt request resolves with an error; finish_turn reports it with this output.
                self.exit_tail = Some(tail);
                // Questions this process still had open can't be answered through it any more: their cards park, and
                // an answer later is recorded (it used to be dropped, leaving the card pending for good).
                self.perms.clear();
                self.parked_prompts.clear();
                crate::cards::bot_exited(&self.hub, &self.cfg.id);
            }
            Some(Incoming::Notification { method, params }) => {
                // Any session's, turn or not: the agent changed an option itself.
                if method == "session/update"
                    && params["update"]["sessionUpdate"] == "config_option_update"
                    && let Some(sid) = params["sessionId"].as_str()
                {
                    self.note_options(sid, &params["update"], Listing::Update);
                } else if method == "session/update"
                    && params["sessionId"].as_str() == self.turn_session.as_deref()
                    && self.turn.is_some()
                {
                    self.on_update(&params["update"]);
                }
            }
            Some(Incoming::Request { id, method, params }) => {
                let Some(conn) = &self.conn else { return };
                let acp = conn.acp.clone();
                // Out of a turn too: a question nobody is waiting on is a card, not an error.
                if method == "session/request_permission" {
                    self.on_permission(id, params).await;
                } else {
                    let _ = acp.respond_error(id, -32601, &format!("{method} is not supported by Kith")).await;
                }
            }
        }
    }

    pub(super) fn on_update(&mut self, u: &Value) {
        let turn = self.turn.unwrap_or(0);
        if matches!(u["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update")) && u["status"] == "completed" {
            self.attempt_effects = true;
        }
        match u["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                let t = acp::content_text(&u["content"]);
                self.append(SegKind::Text, &t);
                self.set_activity("Writing a reply…");
            }
            "agent_thought_chunk" => {
                let t = acp::content_text(&u["content"]);
                self.append(SegKind::Thought, &t);
                self.set_activity("Thinking…");
            }
            "tool_call" => {
                self.close_seg();
                let tool_id = u["toolCallId"].as_str().unwrap_or_default().to_owned();
                let mut data = json!({
                    "toolCallId": tool_id,
                    "title": u["title"].as_str().unwrap_or("Tool"),
                    "toolKind": u["kind"].as_str().unwrap_or("other"),
                    "status": u["status"].as_str().unwrap_or("pending"),
                    "output": "",
                    "diffs": [],
                    "locations": u["locations"].clone(),
                });
                merge_tool_content(&mut data, &u["content"]);
                crate::chat::memory::files::touched(&self.hub.store, &self.cfg, &data);
                let title = data["title"].as_str().unwrap_or("Tool").to_owned();
                if let Some(e) = self.add(EntryKind::Tool, turn, data) {
                    self.tools.insert(tool_id, e.id);
                }
                self.set_activity(super::activity(&title));
            }
            "tool_call_update" => {
                let tool_id = u["toolCallId"].as_str().unwrap_or_default();
                let Some(entry_id) = self.tools.get(tool_id).cloned() else { return };
                let Some(mut e) = self.hub.store.entry(&entry_id) else { return };
                for (k, key) in [("title", "title"), ("kind", "toolKind"), ("status", "status")] {
                    if let Some(v) = u[k].as_str() {
                        e.data[key] = v.into();
                    }
                }
                if !u["locations"].is_null() {
                    e.data["locations"] = u["locations"].clone();
                }
                merge_tool_content(&mut e.data, &u["content"]);
                crate::chat::memory::files::touched(&self.hub.store, &self.cfg, &e.data);
                let status = e.data["status"].as_str().unwrap_or_default().to_owned();
                let title = e.data["title"].as_str().unwrap_or_default().to_owned();
                self.hub.set_entry(&entry_id, &e.data);
                if status == "in_progress" {
                    self.set_activity(super::activity(&title));
                }
            }
            "compaction_update" => {
                if let Some(conn) = self.conn.as_mut() {
                    conn.reports_compaction = true;
                }
                // Claude's adapter carries the summary on one of its completed updates.
                if u["status"] == "completed" {
                    let summary = acp::content_text(&u["summary"]);
                    self.compacted(
                        u["compactionId"].as_str().unwrap_or_default(),
                        (!summary.trim().is_empty()).then_some(summary),
                    );
                }
            }
            "usage_update" => {
                let info = &u["_meta"]["_claude/rateLimit"];
                if info["status"] == "rejected" {
                    // Which turn ended on it is for `failover`; the reset time is what it needs to mark the entry.
                    let resets_at = info["resetsAt"].as_i64().filter(|t| *t > 0).map(acp::epoch_secs);
                    self.limit_seen = Some(acp::Depleted { resets_at });
                }
                if info.is_object() {
                    crate::usage::ingest_claude_rate_limit(&self.hub, info, self.answering_account());
                }
                if let Some(used) = u["used"].as_u64() {
                    self.track_usage(used, u["size"].as_u64().unwrap_or(0));
                }
            }
            "plan" => {
                let data = json!({"entries": u["entries"].clone(), "author": self.cfg.id});
                match &self.plan_entry {
                    Some(id) => {
                        self.hub.set_entry(id, &data);
                    }
                    None => {
                        if let Some(e) = self.add(EntryKind::Plan, turn, data) {
                            self.plan_entry = Some(e.id);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) async fn on_permission(&mut self, rpc_id: Value, params: Value) {
        let mut options = params["options"].as_array().cloned().unwrap_or_default();
        if self.cfg.person.is_some() {
            options = once_only(options);
        }
        let tool = &params["toolCall"];
        let title = tool["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let eid = self.tools.get(tool["toolCallId"].as_str()?)?;
                self.hub.store.entry(eid)?.data["title"].as_str().map(str::to_owned)
            })
            .unwrap_or_else(|| "Use a tool".into());
        // An invitee's run asks its approver (`access::approver`). The card says whose run it is, since the approver
        // sees it outside the invitee's chat.
        let whose = match (self.cfg.person.clone(), self.cfg.hq_agent.clone()) {
            (Some(person), Some(agent)) => Some((person, agent)),
            _ => None,
        };
        let mut in_grants = false;
        let mut answerable_by = None;
        if let Some((person, agent)) = &whose {
            let name = tool["_meta"]["claudeCode"]["toolName"].as_str().unwrap_or_default();
            let kind = tool["kind"].as_str().unwrap_or("other");
            let ask = Request {
                person,
                agent,
                owner_run: false,
                ask: Ask::Tool { in_grants: access::acp_in_grants(person, agent, name, kind, tool) },
            };
            in_grants = ask.in_grants();
            answerable_by = access::approver(&self.hub.store, &ask).await;
        }
        let raw = &tool["rawInput"];
        let tool_name = crate::cards::edge::acp_tool(tool, &title).to_owned();
        // A host or a helper the owner is asked about can be kept: "Always for <person>", or the job that holds it.
        let mut kept = None;
        if let (Some((person, agent)), None) = (&whose, &answerable_by) {
            let who = self.hub.store.person(person).map(|p| p.name).filter(|n| !n.is_empty());
            let who = who.unwrap_or_else(|| person.clone());
            kept = crate::cards::edge::for_acp(person, &who, agent, &tool_name, raw, &mut options).await;
        }
        let mut detail = json!({"output": "", "diffs": []});
        merge_tool_content(&mut detail, &tool["content"]);
        if let Some((text, _)) = &kept
            && !text.is_empty()
        {
            let shown = detail["output"].as_str().unwrap_or_default();
            detail["output"] = if shown.is_empty() { text.clone() } else { format!("{shown}\n\n{text}") }.into();
        }
        let command = raw["command"].as_str().map(str::to_owned).or_else(|| {
            raw["command"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
        });
        let data = json!({
            "title": title,
            "toolKind": tool["kind"].as_str().unwrap_or("other"),
            "command": command,
            "detail": detail["output"],
            "diffs": detail["diffs"],
            "cwd": self.cfg.cwd,
            "options": options.iter().map(|o| json!({"optionId": o["optionId"], "name": o["name"], "kind": o["kind"]})).collect::<Vec<_>>(),
            "status": "pending",
            "selected": Value::Null,
        });
        let mut data = data;
        if let Some((_, edge)) = kept {
            data["edge"] = edge;
        }
        // Who the run waits for when it isn't the asker: the owner, or a bot's owner person.
        let mut waiting_for = None;
        if let Some((person, agent)) = &whose {
            let name = self.hub.store.person(person).map(|p| p.name).filter(|n| !n.is_empty());
            data["cwd"] = Value::Null;
            data["approver"] = if answerable_by.is_some() { "member" } else { "owner" }.into();
            data["person"] = person.as_str().into();
            data["personName"] = name.unwrap_or_else(|| person.clone()).into();
            data["agent"] = agent.as_str().into();
            waiting_for = match &answerable_by {
                Some(p) if p == person => None,
                Some(p) => Some(
                    self.hub.store.person(p).map(|o| o.name).filter(|n| !n.is_empty()).unwrap_or_else(|| p.clone()),
                ),
                None => Some(crate::api::roles::owner_name(agent).await.unwrap_or_else(|| "the owner".into())),
            };
        }
        let in_turn = self.turn.is_some();
        let turn = self.turn.unwrap_or_else(|| self.hub.store.max_turn(&self.lane.chat));
        // The same question again joins the card that is open for it (A6), and a yes already given for it is used.
        let Some((resource, shown)) = Self::card_resource(tool, &title) else {
            // A secret or a login (A5): no card, the agent is told no.
            let no = params["options"].as_array().and_then(|all| all.iter().find(|o| o["kind"] == "reject_once"));
            let outcome = match no {
                Some(o) => json!({"outcome": {"outcome": "selected", "optionId": o["optionId"]}}),
                None => json!({"outcome": {"outcome": "cancelled"}}),
            };
            if let Some(conn) = &self.conn {
                let _ = conn.acp.respond(rpc_id, outcome).await;
            }
            return;
        };
        let now = crate::store::now_ms();
        let card = NewCard {
            session: format!("acp:{}:{}", self.lane.chat, self.lane.thread.as_deref().unwrap_or_default()),
            source: "acp",
            bot_id: Some(self.cfg.id.clone()),
            run_id: None,
            person: whose.as_ref().map(|w| w.0.clone()).unwrap_or_default(),
            agent: whose.as_ref().map(|w| w.1.clone()).or_else(|| self.cfg.hq_agent.clone()).unwrap_or_default(),
            answerable_by: answerable_by.clone(),
            in_grants,
            tool: tool_name,
            input: raw.to_string(),
            resource,
            shown,
            deadline: now.saturating_add(crate::cards::settings(&self.hub.store).expiry_ms),
            reply_to: None,
            asked_by: None,
        };
        let opened = self.hub.store.card_open(&card, now);
        if let Ok(Opened::Granted(_)) = &opened
            && let Some(o) = params["options"].as_array().and_then(|all| all.iter().find(|o| o["kind"] == "allow_once"))
        {
            if let Some(conn) = &self.conn {
                let _ = conn
                    .acp
                    .respond(rpc_id, json!({"outcome": {"outcome": "selected", "optionId": o["optionId"]}}))
                    .await;
            }
            return;
        }
        if let Ok(Opened::Joined(c)) = &opened
            && let Some(entry_id) = c.entry_id.clone()
            && let Some(mut e) = self.hub.store.entry(&entry_id)
        {
            e.data["status"] = "pending".into();
            e.data["parked"] = false.into();
            self.hub.set_entry(&entry_id, &e.data);
            let _ = self.hub.store.card_live(&c.id, None, None);
            self.perms.entry(entry_id).or_default().push(rpc_id);
            return;
        }
        self.close_seg();
        if let Some(e) = self.add(EntryKind::Permission, turn, data.clone()) {
            if let Ok(Opened::New(c) | Opened::Joined(c)) = &opened {
                let _ = self.hub.store.card_live(&c.id, Some(&e.id), None);
            }
            // The owner may be a while: the slot goes back to the gate so other people's turns aren't stuck behind it.
            if waiting_for.is_some() && in_turn {
                self.parked_prompts.insert(e.id.clone(), self.hub.gate.park(&self.rt));
            }
            self.perms.entry(e.id).or_default().push(rpc_id);
            if in_turn {
                self.hub.set_task_status(&self.lane, TaskState::NeedsYou);
            }
        }
        if in_turn {
            self.hub.set_runtime(&self.rt, |r| {
                r.status = BotStatus::NeedsInput;
                r.activity = match &waiting_for {
                    Some(owner) => format!("Waiting for {owner}"),
                    None => format!("Needs approval: {title}"),
                };
            });
        }
        // Tapping it opens where the card is: the group, for a room turn.
        let mut target =
            self.hub.store.bot(&self.lane.chat).ok().flatten().map_or_else(|| self.cfg.clone(), |r| r.config);
        let mut heading = format!("{} needs you", self.cfg.name);
        if waiting_for.is_some() {
            // The asker waits; the approver's devices are the ones to tell.
            target.person.clone_from(&answerable_by);
            heading = format!("{} needs your OK", data["personName"].as_str().unwrap_or_default());
        }
        push::notify(&self.hub, &target, Some(&self.cfg.id), &heading, &title, AlertKind::NeedsInput);
    }

    /// The resource key and the text of what a bot's question is about: the real path for a file, else a digest of
    /// its input. A secret or a login is not carded; the agent is told no (A5).
    fn card_resource(tool: &Value, title: &str) -> Option<(String, String)> {
        let name = crate::cards::edge::acp_tool(tool, title);
        let raw = &tool["rawInput"];
        match crate::cards::resource::classify(name, raw, &crate::cards::resource::roots()) {
            crate::cards::resource::Classified::Card { key, shown } => Some((key, shown)),
            crate::cards::resource::Classified::Never => None,
            crate::cards::resource::Classified::Malformed(_) => {
                Some((format!("{name}:{}", crate::cards::resource::digest(raw)), title.to_owned()))
            }
        }
    }

    pub(super) async fn answer_permission(&mut self, entry_id: &str, option_id: Option<String>) {
        // In an invited person's chat only an option the card offered counts (see `once_only`): nothing a client
        // makes up can switch the session's mode or write a rule that outlives the run.
        if let (Some(_), Some(o)) = (&self.cfg.person, &option_id)
            && !self.hub.store.entry(entry_id).is_some_and(|e| crate::api::roles::offered(&e.data, o))
        {
            tracing::warn!(bot = %self.cfg.id, option = %o, "refused an answer the card didn't offer");
            return;
        }
        let Some(rpc_ids) = self.perms.remove(entry_id) else {
            // Nothing here to answer (the process is gone): a yes spent for it goes back to the card.
            crate::cards::undelivered(&self.hub.store, entry_id);
            return;
        };
        // A prompt that waited for the owner gave its slot up; an answer takes it back before the agent goes on.
        if let Some(parked) = self.parked_prompts.remove(entry_id)
            && option_id.is_some()
        {
            parked.resume().await;
        }
        // The host's own "always" and job options reach the agent as its allow_once (cards/edge.rs).
        let offered = self.hub.store.entry(entry_id).map(|e| e.data["options"].clone()).unwrap_or_default();
        let outcome = match option_id.as_deref().and_then(|o| crate::cards::edge::adapter_option(&offered, o)) {
            Some(o) => json!({"outcome": {"outcome": "selected", "optionId": o}}),
            None => json!({"outcome": {"outcome": "cancelled"}}),
        };
        let allowed =
            self.hub.store.entry(entry_id).is_some_and(|e| crate::cards::allows(&e.data, option_id.as_deref()));
        if let Some(conn) = &self.conn {
            // A yes is one use (A4): the first request that asked hears it, a repeat that joined is told no.
            let no = self.hub.store.entry(entry_id).and_then(|e| {
                e.data["options"].as_array()?.iter().find(|o| o["kind"] == "reject_once").map(|o| o["optionId"].clone())
            });
            for (i, rpc_id) in rpc_ids.into_iter().enumerate() {
                let outcome = match (allowed && i > 0, &no) {
                    (true, Some(no)) => json!({"outcome": {"outcome": "selected", "optionId": no}}),
                    (true, None) => json!({"outcome": {"outcome": "cancelled"}}),
                    _ => outcome.clone(),
                };
                let _ = conn.acp.respond(rpc_id, outcome).await;
            }
            // The yes was heard by the agent: the once grant stays spent (C3), or the question is closed for good.
            crate::cards::delivered(&self.hub.store, entry_id, allowed);
        } else {
            crate::cards::undelivered(&self.hub.store, entry_id);
        }
        if let Some(mut e) = self.hub.store.entry(entry_id) {
            e.data["status"] = if option_id.is_some() { "answered" } else { "cancelled" }.into();
            e.data["selected"] = option_id.into();
            self.hub.set_entry(entry_id, &e.data);
        }
        if self.perms.is_empty() && self.turn.is_some() {
            self.hub.set_task_status(&self.lane, TaskState::Working);
            self.hub.set_runtime(&self.rt, |r| {
                r.status = BotStatus::Working;
                r.activity = "Continuing…".into();
            });
        }
    }

    /// A `send_message`: the user's next bubble, in the turn's lane, right away. Only in the
    /// bot's own chat and threads, including independent messages; a room, an ask or a
    /// routine reads the turn's reply.
    pub(super) fn send_to_user(&mut self, text: String) -> anyhow::Result<()> {
        let Some(turn) = self.turn else { anyhow::bail!("no turn is running") };
        if self.active_request.as_ref().is_some_and(crate::chat::team::BotRequest::expects_reply) {
            anyhow::bail!(
                "send_message isn't available in an ask: return your answer to the requesting bot as your final reply"
            );
        }
        if self.active_group.is_some() || self.active_routine.is_some() {
            anyhow::bail!("send_message isn't available in this turn: write your answer as your reply");
        }
        self.close_seg();
        let entry = self
            .add(EntryKind::Agent, turn, json!({"text": text, "final": true}))
            .ok_or_else(|| anyhow::anyhow!("couldn't save the message"))?;
        self.last_agent = Some(entry.id);
        self.sent.push(text);
        self.set_activity("Working…");
        Ok(())
    }

    // MARK: streaming text segments

    pub(super) fn append(&mut self, kind: SegKind, text: &str) {
        if text.is_empty() {
            return;
        }
        let same = matches!(&self.seg, Seg::Open { kind: k, .. } if *k == kind);
        if !same {
            self.close_seg();
            let (k, data) = match kind {
                SegKind::Text => (EntryKind::Agent, json!({"text": text, "final": false})),
                SegKind::Thought => (EntryKind::Thought, json!({"text": text})),
            };
            if let Some(e) = self.add(k, self.turn.unwrap_or(0), data) {
                if kind == SegKind::Text {
                    self.last_agent = Some(e.id.clone());
                    self.last_text = Some(e.id.clone());
                }
                self.seg =
                    Seg::Open { kind, entry_id: e.id, buf: text.to_owned(), flushed: Instant::now(), dirty: false };
            }
            return;
        }
        if let Seg::Open { buf, dirty, .. } = &mut self.seg {
            buf.push_str(text);
            *dirty = true;
        }
    }

    /// Persists streamed text at most every 300 ms (or immediately with `force`).
    pub(super) fn flush(&mut self, force: bool) {
        let (hq, serving) = (self.is_hq(), self.serving.clone());
        if let Seg::Open { kind, entry_id, buf, flushed, dirty } = &mut self.seg
            && *dirty
            && (force || flushed.elapsed() >= Duration::from_millis(300))
        {
            let data = match kind {
                SegKind::Text if hq => {
                    json!({"text": buf, "final": false, "author": self.cfg.id, "answeredBy": serving})
                }
                SegKind::Text => json!({"text": buf, "final": false, "author": self.cfg.id}),
                SegKind::Thought => json!({"text": buf, "author": self.cfg.id}),
            };
            self.hub.set_entry(entry_id, &data);
            *flushed = Instant::now();
            *dirty = false;
        }
    }

    pub(super) fn close_seg(&mut self) {
        self.flush(true);
        self.seg = Seg::None;
    }
}

/// Folds ACP tool-call `content` into `{output, diffs}` on an entry's data.
pub fn merge_tool_content(data: &mut Value, content: &Value) {
    let Some(items) = content.as_array() else { return };
    let mut out = String::new();
    let mut diffs = vec![];
    for item in items {
        match item["type"].as_str() {
            Some("diff") => {
                let path = item["path"].as_str().unwrap_or_default();
                let old = item["oldText"].as_str().unwrap_or_default();
                let new = item["newText"].as_str().unwrap_or_default();
                diffs.push(diff_summary(path, old, new));
            }
            Some("terminal") => {}
            _ => {
                let t = acp::content_text(item);
                if !t.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&t);
                }
            }
        }
    }
    if !out.is_empty() {
        data["output"] = acp::truncate(&out, 6000).into();
    }
    if !diffs.is_empty() {
        data["diffs"] = diffs.into();
    }
}

/// Changed-region summary: trims the common prefix/suffix and reports the middle.
pub fn diff_summary(path: &str, old: &str, new: &str) -> Value {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let removed = &a[prefix..a.len() - suffix];
    let added = &b[prefix..b.len() - suffix];
    let mut unified = String::new();
    for (sign, lines) in [('-', removed), ('+', added)] {
        for l in lines {
            unified.push(sign);
            unified.push_str(l);
            unified.push('\n');
        }
    }
    json!({
        "path": path,
        "added": added.len(),
        "removed": removed.len(),
        "isNew": old.is_empty(),
        "startLine": prefix + 1,
        "patch": acp::truncate(&unified, 6000),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invitee_card_keeps_only_the_once_choices() {
        // What the Claude adapter offers on an exit-plan card and on an ordinary tool card.
        let offered = vec![
            json!({"optionId": "exit-plan-bypass", "name": "Yes, and bypass permissions", "kind": "allow_always"}),
            json!({"optionId": "exit-plan-auto", "name": "Yes, and use auto mode", "kind": "allow_always"}),
            json!({"optionId": "exit-plan-accept-edits", "name": "Yes, auto-accept edits", "kind": "allow_always"}),
            json!({"optionId": "exit-plan-default", "name": "Yes, manually approve edits", "kind": "allow_once"}),
            json!({"optionId": "allow-always", "name": "Yes, and don't ask again", "kind": "allow_always"}),
            json!({"optionId": "reject", "name": "No", "kind": "reject_once"}),
            json!({"optionId": "reject-always", "name": "Never", "kind": "reject_always"}),
        ];
        let kept: Vec<String> = once_only(offered).iter().map(|o| o["optionId"].as_str().unwrap().to_owned()).collect();
        assert_eq!(kept, ["exit-plan-default", "reject"]);
        let card = json!({"options": [{"optionId": "reject"}]});
        assert!(crate::api::roles::offered(&card, "reject") && !crate::api::roles::offered(&card, "exit-plan-bypass"));
    }

    #[test]
    fn diff_counts_changed_middle() {
        let d = diff_summary("f", "a\nb\nc\n", "a\nB\nB2\nc\n");
        assert_eq!(d["added"], 2);
        assert_eq!(d["removed"], 1);
        assert_eq!(d["startLine"], 2);
        assert_eq!(d["patch"], "-b\n+B\n+B2\n");
    }

    #[test]
    fn tool_content_merges_text_and_diffs() {
        let mut data = json!({"output": "", "diffs": []});
        merge_tool_content(
            &mut data,
            &json!([
                {"type": "content", "content": {"type": "text", "text": "ok"}},
                {"type": "diff", "path": "/a.rs", "oldText": "", "newText": "x\n"}
            ]),
        );
        assert_eq!(data["output"], "ok");
        assert_eq!(data["diffs"][0]["isNew"], true);
    }
}
