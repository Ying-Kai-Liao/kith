//! What an invited person (a device with a `person_id`) may do: deny by default. A method must be on
//! [`MEMBER_METHODS`] and, when it names a chat, the chat must be theirs and its agent still granted to them.
//! Everything an allowed method returns is shaped here too, so no handler has to know who is asking.
//! The owner's devices and Local never come through here (see `dispatch`).

use super::devices::NotAllowed;
use crate::hub::Hub;
use crate::remote::account;
use crate::store::Store;
use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Arc;

/// The whole of what an invited person can call, from the host RPC contract (serve/contract/host/contract.json, each
/// method with its reason). A new method is refused for them until the contract lists it for `member`;
/// `every_router_method_is_denied_to_members_unless_listed` walks the router to keep it that way.
pub use kith_wire::rpc::MEMBER_METHODS;

/// The whole of what a bot guest can call (docs/kith-bot-guests.md 5.7), deny by default like [`MEMBER_METHODS`], and
/// what one that has not been allowed in yet may call: from the host RPC contract, as `bot` and `botPending`, each
/// method with its reason. `every_router_method_is_denied_to_bots_unless_listed` walks the router to keep it that way.
/// Not there, on purpose: `respondPermission` (a bot never answers the prompts of its own run: they go to its owner and
/// the Mac owner), `startChat`, `send`, `thread`, `history` (too wide for a client that can't be upgraded in lockstep;
/// `botAsk` and `botThread` are the narrow contract), files, projects, and everything the owner or a member has.
pub use kith_wire::rpc::{BOT_METHODS, BOT_PENDING_METHODS};

pub fn bot_may(method: &str) -> bool {
    BOT_METHODS.contains(&method)
}

/// All an unproven device of an invited person may call: showing the invite token (see `people::prove_invite`). From
/// the contract, as `unproven`.
pub use kith_wire::rpc::UNPROVEN_METHODS;

pub fn member_may(method: &str) -> bool {
    MEMBER_METHODS.contains(&method)
}

fn denied(why: &str) -> anyhow::Error {
    NotAllowed(why.to_owned()).into()
}

/// The chat or agent a call names, for the audit log.
pub fn target_of(b: &Value) -> Option<String> {
    ["botId", "entryId", "agent", "inviteId", "conversation"]
        .iter()
        .find_map(|k| b[*k].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
}

/// Whether `person` may use `agent` right now, asked of kith-auth (the truth) on every call so that taking an agent
/// away cuts access at the next request. Fails closed.
pub async fn may_use(person: &str, agent: &str) -> bool {
    resolve(person, agent).await.is_some_and(|v| v["allowed"].as_bool() == Some(true))
}

/// [`may_use`], but `None` when kith-auth could not be asked (or named the owner): a caller that would drop work on a
/// no (a card's owed resume) tries again instead of dropping it on a hiccup.
pub async fn may_use_checked(person: &str, agent: &str) -> Option<bool> {
    resolve(person, agent).await.map(|v| v["allowed"].as_bool() == Some(true))
}

/// What kith-auth says `person` gets of `agent` (see `kith-auth resolve`); None when it can't be asked or the person
/// is the owner, who never comes through here.
pub(crate) async fn resolve(person: &str, agent: &str) -> Option<Value> {
    let bin = account::bin().await?;
    let out = crate::channels::hq::output(&bin, &["resolve", agent, "--person", person]).await?;
    serde_json::from_slice::<Value>(&out.stdout).ok().filter(|v| v["owner"].as_bool() != Some(true))
}

/// The agents `person` holds, for the apps' "New chat": every agent set up on this computer that kith-auth lets them
/// use, with the role they have in it (`member` only for the member preset, else `guest`). Never one they don't hold.
pub async fn granted_agents(hub: &Hub, person: &str) -> Vec<Value> {
    let mut seen = std::collections::BTreeMap::new();
    for row in hub.store.bots().unwrap_or_default() {
        let c = row.config;
        if let (false, None, Some(agent)) = (row.deleted, &c.person, c.hq_agent) {
            seen.entry(agent).or_insert(c.name);
        }
    }
    let mut out = Vec::new();
    for (agent, name) in seen {
        let Some(v) = resolve(person, &agent).await.filter(|v| v["allowed"].as_bool() == Some(true)) else {
            continue;
        };
        let role = if v["preset"].as_str() == Some("member") { "member" } else { "guest" };
        out.push(json!({"agent": agent, "name": name, "role": role}));
    }
    out
}

/// The owner's name in kith-auth, for "Waiting for <owner>"; None when it has none (or can't be asked).
pub async fn owner_name(agent: &str) -> Option<String> {
    let bin = account::bin().await?;
    // No person: kith-auth answers for the owner.
    let out = crate::channels::hq::output(&bin, &["resolve", agent]).await?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["name"].as_str().filter(|n| !n.is_empty()).map(str::to_owned)
}

/// Whether an entry's `data` is a key or account card (cards/secret.rs): it names an agent, a service and an account,
/// and is the owner's alone. No other principal is told it exists.
pub fn secret_card(data: &Value) -> bool {
    matches!(data["card"].as_str(), Some("key" | "account"))
}

/// Whether the permission card `data` offered `option` to its chat.
pub fn offered(data: &Value, option: &str) -> bool {
    data["options"].as_array().is_some_and(|all| all.iter().any(|o| o["optionId"].as_str() == Some(option)))
}

/// Whether a tool runs code (or something the host can't see into): its card goes to the owner whoever asked. A
/// member's own run executes as the owner's macOS user, so they can't approve their own shell (F11). Shell tools of any
/// harness (ACP kind `execute`) and every `mcp__*` tool count; `BashOutput` and `KillShell` only act on a shell that is
/// already running, so a member may answer those.
pub fn runs_code(tool: &str, kind: &str) -> bool {
    if matches!(tool, "BashOutput" | "KillShell") {
        return false;
    }
    // No tool name (an ACP request without claudeCode's toolName: its title is display text and may be an MCP tool's
    // label): only a kind that clearly runs no code is the member's; an unknown kind goes to the owner too.
    if tool.is_empty() {
        return !matches!(kind, "read" | "edit" | "search" | "fetch" | "think");
    }
    kind == "execute" || tool == "Bash" || tool.starts_with("mcp__")
}

/// Whether every file a file tool names lies inside `roots` (real paths, links followed, `..` applied). A path that can't be
/// resolved, or a relative one, is outside, and so is a file tool that names none (Grep with no path searches the working
/// directory, which may be anywhere). A tool that isn't a file tool names no files and is inside.
pub fn within(roots: &[std::path::PathBuf], tool: &str, input: &Value) -> bool {
    let named = crate::cards::resource::named_paths(tool, input);
    if named.is_empty() {
        return !crate::cards::resource::is_file_tool(tool);
    }
    named.into_iter().all(|raw| inside(roots, raw))
}

/// `within` for an app chat's (ACP) tool call. One with no tool name that reads, edits or searches is judged by the files
/// its `locations` name, and with none it is outside.
pub fn acp_within(roots: &[std::path::PathBuf], name: &str, kind: &str, call: &Value) -> bool {
    if !name.is_empty() || !matches!(kind, "read" | "edit" | "search") {
        return within(roots, name, &call["rawInput"]);
    }
    let paths: Vec<&str> =
        call["locations"].as_array().into_iter().flatten().filter_map(|l| l["path"].as_str()).collect();
    !paths.is_empty() && paths.into_iter().all(|raw| inside(roots, raw))
}

fn inside(roots: &[std::path::PathBuf], raw: &str) -> bool {
    crate::cards::resource::resolve(raw).is_some_and(|real| roots.iter().any(|root| real.starts_with(root)))
}

/// What a run for someone but the owner holds grants to, as real paths: its person's memory and files folders for
/// `agent` and the agent's shared memory (F11 step 2a: the kernel's sandbox profile lets Bash and the file tools write in
/// the same folders). A file card for anything else is the owner's to answer, a member's own run included.
pub fn grant_roots(person: &str, agent: &str) -> Vec<std::path::PathBuf> {
    let home = crate::kernel::home_dir(&dirs::home_dir().unwrap_or_default());
    let agents = crate::kernel::var("AGENTS").map_or_else(|| home.join("agents"), std::path::PathBuf::from);
    let base = agents.join(agent);
    [base.join("memory/people").join(person), base.join("files/people").join(person), base.join("memory/shared")]
        .iter()
        .filter_map(|p| crate::cards::resource::resolve(&p.to_string_lossy()))
        .collect()
}

/// The person's chat `bot`, when it is theirs and its agent is still granted to them.
pub async fn own_bot(hub: &Hub, person: &str, bot: &str) -> Result<()> {
    let row = hub
        .store
        .bot(bot)?
        .filter(|r| !r.deleted && r.config.person.as_deref() == Some(person))
        .ok_or_else(|| denied("That chat isn't yours."))?;
    let agent = row.config.hq_agent.as_deref().ok_or_else(|| denied("That chat isn't yours."))?;
    if !may_use(person, agent).await {
        return Err(denied("You no longer have access to that agent."));
    }
    Ok(())
}

/// The agent of `chat` when it is the chat of a bot guest that `person` owns.
fn owned_bot_chat_agent(hub: &Hub, person: &str, chat: &str) -> Option<String> {
    let row = hub.store.bot(chat).ok().flatten().filter(|r| !r.deleted)?;
    let bot = hub.store.bot_guest_of_person(row.config.person.as_deref()?)?;
    (bot.owner_person.as_deref() == Some(person)).then_some(())?;
    row.config.hq_agent
}

/// Checks a call from `person` before it runs. May tighten the body (`b`).
pub async fn guard(hub: &Arc<Hub>, person: &str, proven: bool, method: &str, b: &Value) -> Result<()> {
    if !proven {
        return if UNPROVEN_METHODS.contains(&method) {
            Ok(())
        } else {
            Err(denied("This device hasn't shown its invite yet."))
        };
    }
    // A proven device shows its kept invite again on every connection; it is told it has, in the words the apps wait for.
    if method == "proveInvite" {
        return Err(denied("This device has already shown its invite."));
    }
    if !member_may(method) {
        return Err(denied("This isn't available to guests and members on this computer."));
    }
    match method {
        // Checked by their own handlers, or about no chat at all.
        "hello" | "projects" | "linkCode" | "scheduleMessages" | "startChat" | "callAccept" | "callDecline"
        | "callGreet" | "createBotInvite" | "revokeBotInvite" | "revokeBot" | "myBots" | "setBotLimits"
        | "resolveCitation" => {}
        // A card or message names its chat only through the entry.
        "respondPermission" | "react" | "editHandoff" | "keepConversation" => {
            let id = b["entryId"].as_str().unwrap_or_default();
            let entry = hub.store.entry(id).ok_or_else(|| denied("That chat isn't yours."))?;
            // A key or account card is the owner's whatever else is true of it (it is never in a member's chat).
            if secret_card(&entry.data)
                || hub.store.card_by_entry(id).is_some_and(|c| c.tool == crate::cards::secret::TOOL)
            {
                return Err(denied(crate::cards::NOT_YOURS));
            }
            // Who answers is stored on the card when it opens (`access::approver`, the one rule for every producer);
            // kith-auth is asked again now, so a revoke applies (A7). What the card names isn't judged again.
            if method == "respondPermission" {
                let card = hub.store.card_by_entry(id).filter(|_| entry.kind == "permission");
                return match card {
                    Some(card) if crate::access::may_answer(Some(person), &card).await => Ok(()),
                    _ => Err(denied(crate::cards::NOT_YOURS)),
                };
            }
            own_bot(hub, person, &entry.bot_id).await?;
        }
        // Reading the chat of a bot guest they own (docs/kith-bot-guests.md 6.1); every other method, writes included,
        // falls to `own_bot`, which refuses a chat that is not the person's own.
        "history" | "thread" | "kithInfo" | "threadProjects"
            if owned_bot_chat_agent(hub, person, b["botId"].as_str().unwrap_or_default()).is_some() => {}
        _ => own_bot(hub, person, b["botId"].as_str().unwrap_or_default()).await?,
    }
    if method == "setThreadProjects" {
        let shared = hub.store.person(person).map(|p| p.projects).unwrap_or_default();
        let ids = b["projects"].as_array().into_iter().flatten().filter_map(Value::as_str);
        if ids.into_iter().any(|id| !shared.iter().any(|s| s == id)) {
            return Err(denied("That project isn't shared with you."));
        }
    }
    if method == "send" {
        // A folder on this Mac is the owner's to pick; a project must be one shared with them.
        let text = |k: &str| b[k].as_str().map(str::trim).filter(|s| !s.is_empty());
        if text("folder").is_some() || b["clear"] == true {
            return Err(denied("Folders on this computer can't be picked from this account."));
        }
        if let Some(project) = text("project")
            && !hub.store.person(person).is_some_and(|p| p.projects.iter().any(|x| x == project))
        {
            return Err(denied("That project isn't shared with you."));
        }
    }
    Ok(())
}

/// What an allowed method returns to `person`.
pub fn shape(hub: &Hub, person: &str, method: &str, mut v: Value) -> Value {
    match method {
        "hello" => {
            // Names and keys the apps need to talk to this computer; none of its paths, addresses or screens.
            let keep = [
                "hostId",
                "computerId",
                "signKey",
                "boxKey",
                "name",
                "version",
                "protocol",
                "minProtocol",
                "os",
                "rev",
                "store",
            ];
            if let Some(o) = v.as_object_mut() {
                o.retain(|k, _| keep.contains(&k.as_str()));
                // The error text can name this computer's folders.
                if let Some(store) = o.get_mut("store").and_then(Value::as_object_mut) {
                    store.remove("error");
                }
            }
        }
        "projects" => {
            let shared = hub.store.person(person).map(|p| p.projects).unwrap_or_default();
            if let Some(list) = v["projects"].as_array_mut() {
                list.retain(|p| p["id"].as_str().is_some_and(|id| shared.iter().any(|s| s == id)));
                // An id and a name; the folders behind a project are this computer's.
                for p in list.iter_mut() {
                    *p = json!({"id": p["id"], "name": p["id"]});
                }
            }
            // The error text can name this computer's folders.
            if let Some(o) = v.as_object_mut() {
                o.remove("error");
            }
        }
        // Where a chat works on this computer is not theirs to see.
        "history" | "thread" | "resolveCitation" => {
            if let Some(o) = v.as_object_mut() {
                o.remove("folder");
            }
            // `get_mut`: indexing would add `entries: null` to a `not_shared` answer.
            if let Some(list) = v.get_mut("entries").and_then(Value::as_array_mut) {
                list.retain(|e| !secret_card(&e["data"]));
                for e in list {
                    hide_foreign_summary(person, e);
                    mark_can_answer(&hub.store, Some(person), e);
                }
            }
        }
        "kithInfo" => {
            let info = if v["info"].is_object() {
                let pick = ["name", "description", "avatar"].map(|k| (k, v["info"][k].clone()));
                Value::Object(pick.into_iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.to_owned(), x)).collect())
            } else {
                Value::Null
            };
            v = json!({"info": info, "fetchedAt": v["fetchedAt"]});
        }
        _ => {}
    }
    v
}

/// Whether the thread summary of `audience` may be seen by `person` (None: the owner, who sees all). Group rooms and
/// terminal threads are the owner's until members of a room can be resolved.
pub fn may_see_summary(person: Option<&str>, audience: &str) -> bool {
    person.is_none_or(|p| audience == "shared" || audience.strip_prefix("person:") == Some(p))
}

/// Drops the thread summary from the row of entry `e` (an entry as JSON) unless `person` may see it; the audience
/// itself is never sent to an invited person.
fn hide_foreign_summary(person: &str, e: &mut Value) {
    let Some(thread) = e["data"]["thread"].as_object_mut() else { return };
    let audience = thread.remove("summaryAudience");
    if !audience.as_ref().and_then(Value::as_str).is_some_and(|a| may_see_summary(Some(person), a)) {
        thread.remove("summary");
    }
}

/// Whose chat a bot is: `Some(None)` is the owner's, `None` an unknown bot.
#[allow(clippy::option_option, reason = "an unknown bot and the owner's chat are different answers")]
fn bot_person(store: &Store, id: &str) -> Option<Option<String>> {
    store.bot(id).ok().flatten().map(|r| r.config.person)
}

/// One live or catch-up event, for the owner (`who` None) or for `person`: the owner never sees an invitee's chats,
/// an invitee sees only their own, and no invitee sees usage, screens or the cloud. A permission card says whether
/// they may answer it (`canAnswer`).
pub fn event_for(store: &Store, who: Option<&str>, v: Value) -> Option<Value> {
    let mut v = route(store, who, v)?;
    if v["type"] == "entry" {
        mark_can_answer(store, who, &mut v["entry"]);
    }
    Some(v)
}

/// The card behind the permission entry `e` (an entry as JSON).
fn card_of(store: &Store, e: &Value) -> Option<crate::store::Card> {
    (e["kind"] == "permission").then(|| store.card_by_entry(e["id"].as_str()?)).flatten()
}

/// Sets `canAnswer` on the permission entry `e` for `who` (None: the owner, who answers every one), from who the card
/// names (`access::can_answer`): the apps show its buttons only when it is true. A card with no row (a limit card) is
/// the owner's. Who could answer it before is the owner's to see only.
pub fn mark_can_answer(store: &Store, who: Option<&str>, e: &mut Value) {
    if e["kind"] != "permission" || !e["data"].is_object() {
        return;
    }
    let can = who.is_none() || card_of(store, e).is_some_and(|c| crate::access::can_answer(who, &c));
    e["data"]["canAnswer"] = can.into();
    if who.is_some()
        && let Some(data) = e["data"].as_object_mut()
    {
        data.remove("formerApprovers");
    }
}

/// Whether `me` could answer the permission entry `e` before its approver changed (`cards::approver_changed`).
fn was_approver(e: &Value, me: &str) -> bool {
    e["kind"] == "permission" && e["data"]["formerApprovers"].as_array().is_some_and(|l| l.iter().any(|p| p == me))
}

fn route(store: &Store, who: Option<&str>, v: Value) -> Option<Value> {
    let kind = v["type"].as_str().unwrap_or_default().to_owned();
    // A key or account card goes to the owner's devices and nobody else's.
    if who.is_some() && kind == "entry" && secret_card(&v["entry"]["data"]) {
        return None;
    }
    // A card reaches the person it names to answer it, wherever it was raised, and still reaches one it named before
    // (who then can't answer it, so it leaves their Needs-you list).
    if let Some(me) = who
        && kind == "entry"
        && (was_approver(&v["entry"], me)
            || card_of(store, &v["entry"]).is_some_and(|c| c.answerable_by.as_deref() == Some(me)))
    {
        let mut v = shape_bot(v);
        hide_foreign_summary(me, &mut v["entry"]);
        return Some(v);
    }
    let bot = match kind.as_str() {
        // A bot guest changed state: the owner of the computer and the bot's own owner hear of it.
        "botUpdated" => {
            return match who {
                None => Some(v),
                Some(me) => (v["bot"]["ownerPerson"].as_str() == Some(me)).then_some(v),
            };
        }
        "bot" => v["bot"]["id"].as_str().map(str::to_owned),
        "entry" => v["entry"]["botId"].as_str().or_else(|| v["entry"]["bot_id"].as_str()).map(str::to_owned),
        "resync" => return Some(v),
        // A call goes to whoever the chat belongs to, never to the owner for an invitee's chat.
        "call" => v["call"]["botId"].as_str().map(str::to_owned),
        _ => return who.is_none().then_some(v),
    };
    let owner = bot.as_deref().and_then(|b| bot_person(store, b));
    match (who, owner) {
        // The owner answers the permission prompts of an invitee's runs (a guest's always, a member's when they like);
        // nothing else of the chat reaches them.
        // A bot guest's conversation is the owner's to see whole (6.2); only an invitee's chat is held back.
        (None, Some(Some(p))) => (store.bot_guest_of_person(&p).is_some()
            || (kind == "entry" && v["entry"]["kind"] == "permission"))
            .then_some(v),
        (None, _) => Some(v),
        (Some(me), Some(Some(p))) if p == me => {
            let agents = store.person(me).map(|p| p.agents).unwrap_or_default();
            let granted =
                store.bot(bot.as_deref()?).ok().flatten()?.config.hq_agent.is_some_and(|a| agents.contains_key(&a));
            if !granted {
                return None;
            }
            let mut v = shape_bot(v);
            if kind == "entry" {
                hide_foreign_summary(me, &mut v["entry"]);
            }
            Some(v)
        }
        // The chat of a bot guest of theirs, read-only for them; its prompts they answer when they are a member of the
        // agent (checked in `guard`). Nothing of other people's bots.
        (Some(me), Some(Some(p))) => {
            let guest = store.bot_guest_of_person(&p)?;
            (guest.owner_person.as_deref() == Some(me)).then_some(())?;
            let mut v = shape_bot(v);
            if kind == "entry" {
                hide_foreign_summary(me, &mut v["entry"]);
            }
            Some(v)
        }
        (Some(_), _) => None,
    }
}

/// One live or catch-up event for the bot guest `bot_id`: only the entries of its own conversations, shaped to what a
/// bot may know (see [`bot_entry`]). Nothing else reaches it, not even other bots' status.
pub fn event_for_bot(store: &Store, bot_id: &str, v: Value) -> Option<Value> {
    match v["type"].as_str()? {
        "resync" => Some(v),
        "entry" => {
            let e = &v["entry"];
            let (chat, root) = (e["botId"].as_str()?, e["threadId"].as_str()?);
            let conv = store.bot_conversation_of_thread(chat, root).filter(|c| c.bot_id == bot_id)?;
            let bot = store.bot_guest(bot_id).filter(|b| b.state == crate::store::BotState::Active)?;
            let shaped = bot_entry(e, &conv.conversation_id, &bot_waits_on(store, &bot))?;
            Some(json!({"type": "entry", "rev": v["rev"], "conversation": conv.conversation_id, "entry": shaped}))
        }
        _ => None,
    }
}

/// Who a bot's turn waits on when it asks for a tool: its owner (the owner of the computer for their own bots).
fn bot_waits_on(store: &Store, bot: &crate::store::BotGuest) -> String {
    bot.owner_person.as_deref().and_then(|p| store.person(p)).map_or_else(|| "the owner".to_owned(), |p| p.name)
}

/// A transcript entry as a bot guest sees it (docs/kith-bot-guests.md 5.8): the user's and the agent's words, an error
/// notice, and a bare "waiting" for a permission card (never its tool or arguments). Thoughts, tool calls, plans,
/// attachments and every other notice are dropped. `e` is an [`crate::store::Entry`] as JSON.
pub fn bot_entry(e: &Value, conversation: &str, waits_on: &str) -> Option<Value> {
    use kith_wire::rpc::BotEntryKind as Kind;
    let data = &e["data"];
    if secret_card(data) {
        return None;
    }
    let text = |k: &str| data[k].as_str().unwrap_or_default().to_owned();
    let mut out = json!({
        "id": e["id"], "seq": e["rev"], "turn": e["turn"], "at": e["createdAt"], "conversation": conversation,
    });
    match e["kind"].as_str()? {
        "user" => {
            out["kind"] = Kind::User.as_str().into();
            out["text"] = text("text").into();
            if data["sender"].is_object() {
                let s = &data["sender"];
                out["sender"] = json!({"botId": s["botId"], "name": s["name"], "speaker": s["speaker"]});
            }
        }
        "agent" => {
            out["kind"] = Kind::Agent.as_str().into();
            out["text"] = text("text").into();
            out["final"] = (data["final"] == true).into();
        }
        "notice" if data["style"] == "error" => {
            out["kind"] = Kind::Notice.as_str().into();
            out["text"] = text("text").into();
            // The client ends the turn on a notice marked as an error.
            out["error"] = true.into();
        }
        "permission" if data["status"] == "pending" => {
            out["kind"] = Kind::Waiting.as_str().into();
            out["text"] = format!("Waiting for {waits_on} to approve a step").into();
        }
        _ => return None,
    }
    Some(out)
}

/// A bot as an invitee sees it: the agent's folder on this computer is not theirs to see.
pub fn shape_bot(mut v: Value) -> Value {
    if let Some(b) = v["bot"].as_object_mut() {
        b.remove("cwd");
        b.remove("command");
        b.remove("folder");
    }
    v
}

/// The first event of a stream, trimmed for an invitee.
pub fn hello_for(v: &Value) -> Value {
    json!({"type": "hello", "hostId": v["hostId"], "computerId": v["computerId"], "rev": v["rev"]})
}
