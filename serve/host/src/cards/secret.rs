//! Secret cards (designs/2026-10-25-secret-cards.md): an agent that needs a service's key asks the owner, in the
//! owner's chat with it, and learns only "saved as NAME". Two cards, both Permission entries (`data.card`):
//!
//! - **account**: the owner already has a key for this service. Pick which account the agent may use
//!   (`respondPermission` `profile:<name>` -> `kith-auth grant`), or "another" (the card becomes a key card in place).
//! - **key**: type the key into the app's secure field (`provideSecret`). The host writes it with `kith-auth set` and
//!   grants it to the asking agent.
//!
//! The value is the only thing here that must never leak. It travels app -> `provideSecret` body -> this module ->
//! the stdin of one `kith-auth set` child, and nowhere else: not argv, not env, not the card, entry, event, audit,
//! analytics, push, log or the resume note (those carry names only). It is held in [`Zeroizing`] buffers and dropped
//! the moment the child has read it. The write comes BEFORE the card is answered; a failed write leaves the card open.
//!
//! One writer per card, in order (verification/secrets/Secrets.tla): every answer of a secret card first takes the
//! card's [`Slot`] (in memory, so a second device finds it taken), then the card store's write claim
//! ([`crate::store::Store::card_claim`], which also keeps the clock from expiring the card mid-write). Only then does
//! the Keychain change; the claim ends in `card_saved` (the card is spent and its resume owed) or is released.
//!
//! "Secrets are never carded" (`resource::classify`) is the other direction: no card lets an agent READ a secret.

use super::{Card, chat_for, follow_up, settings};
use crate::api::UserError;
use crate::hub::Hub;
use crate::runauth::RunIdentity;
use crate::store::{CardState, EntryKind, Lane, NewCard, Opened, now_ms};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;
use zeroize::Zeroizing;

/// The tool name a secret card carries (the permission card kind the apps already draw).
pub const TOOL: &str = "request_secret";

/// What an old app, which draws only the card's options, shows for a key card.
pub const OLD_APP_DETAIL: &str = "Update Kith to add the key here.";

const CLOSED: &str = "That request is closed";
const BAD_ACCOUNT: &str = "That account name can't be used";
const BAD_VALUES: &str = "Enter every field of the key";
const TOO_LONG: &str = "That key is too long";
const NOT_SAVED: &str = "Couldn't save it to the Keychain";
const TOLD: &str = "Asked {owner} in the Kith app; you won't see the key. End your turn.";

/// The longest value a field takes, in bytes: kith-auth set caps each at 1536 (the `security -i` line limit).
const MAX_VALUE: usize = 1536;
const MAX_PURPOSE: usize = 200;
/// A write has this long to finish before the card's own deadline matters again.
const CLAIM_MS: i64 = 5 * 60 * 1000;
/// A saved card keeps its owed resume alive at least this long.
const RESUME_KEEP_MS: i64 = 60 * 60 * 1000;
const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

// ---- where an ask came from

/// Who is asking and where the answer goes back to: the run socket's token, or a chat's built-in tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub agent: String,
    pub session: String,
    pub source: &'static str,
    pub bot_id: Option<String>,
    pub run_id: Option<String>,
}

impl Origin {
    /// An owner `claude -p` run (`requestSecret` on the run socket).
    pub fn run(who: &RunIdentity) -> Self {
        Self {
            agent: who.agent.clone(),
            session: who.session.clone(),
            source: "run",
            bot_id: None,
            run_id: Some(who.run_id.clone()),
        }
    }

    /// A turn of the owner's chat `bot` with `agent`, in `lane` (the chat's built-in `request_secret` tool).
    pub fn chat(bot: &str, agent: &str, lane: &Lane) -> Self {
        Self {
            agent: agent.to_owned(),
            session: format!("acp:{}:{}", lane.chat, lane.thread.as_deref().unwrap_or_default()),
            source: "acp",
            bot_id: Some(bot.to_owned()),
            run_id: None,
        }
    }
}

// ---- names

/// A service or account label as kith-auth takes it: lowercase letters and digits, then `.` `_` `-`, at most 40.
pub fn label_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'))
}

fn var_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// An agent name is passed as an argument: no flag, no path, no whitespace.
fn agent_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.starts_with('-') && s.bytes().all(|c| c.is_ascii_graphic() && c != b'/')
}

/// `service/account` split, both parts well formed.
fn split_profile(p: &str) -> Option<(&str, &str)> {
    p.split_once('/').filter(|(s, a)| label_ok(s) && label_ok(a))
}

fn clean_purpose(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_PURPOSE).collect::<String>().trim().to_owned()
}

fn refused(why: &str) -> Value {
    json!({"ok": false, "carded": false, "error": why, "message": why})
}

// ---- kith-auth

/// What a `kith-auth` child came to. Its stderr is never read: nothing it says reaches a person, a log or an agent.
struct Ran {
    ok: bool,
    stdout: Vec<u8>,
}

/// Runs `kith-auth args`, with `stdin` (when given) as its whole standard input. Nothing secret is ever an argument.
/// None when kith-auth can't be found or started or ran out of time.
async fn kith_auth(args: &[&str], stdin: Option<Zeroizing<Vec<u8>>>) -> Option<Ran> {
    let bin = crate::remote::account::bin().await?;
    let mut cmd = tokio::process::Command::new(bin);
    #[cfg(test)]
    cmd.envs(
        crate::channels::hq::TEST_ENV
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(k, v)| (*k, v)),
    );
    let mut child = cmd
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A child that exits before reading is judged by its exit code, not by a broken pipe.
        let _ = pipe.write_all(&input).await;
        drop(pipe);
    }
    let out = tokio::time::timeout(TOOL_TIMEOUT, child.wait_with_output()).await.ok()?.ok()?;
    Some(Ran { ok: out.status.success(), stdout: out.stdout })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct State {
    declared: bool,
    secrets: Vec<String>,
    granted: Vec<String>,
    missing: Vec<String>,
    profiles: Vec<ProfileRow>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct ProfileRow {
    profile: String,
    account: String,
    agents: Vec<String>,
}

async fn account_state(agent: &str, service: &str) -> Option<State> {
    let ran = kith_auth(&["account-state", agent, service, "--json"], None).await?;
    ran.ok.then_some(())?;
    serde_json::from_slice(&ran.stdout).ok()
}

// ---- the ask

/// What card an ask comes to, from kith-auth's account state.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    card: &'static str,
    vars: Vec<String>,
    profile: Option<String>,
    account: Option<String>,
    replaces: bool,
    accounts: Vec<Value>,
}

fn plan_for(s: &State) -> Plan {
    let vars = if s.missing.is_empty() { s.secrets.clone() } else { s.missing.clone() };
    // A grant already: a new key goes into that account (a rotation), the account is not asked again.
    if let Some(profile) = s.granted.iter().min() {
        let account = s
            .profiles
            .iter()
            .find(|p| &p.profile == profile)
            .map(|p| p.account.clone())
            .or_else(|| split_profile(profile).map(|(_, a)| a.to_owned()));
        return Plan {
            card: "key",
            vars,
            profile: Some(profile.clone()),
            account,
            replaces: true,
            accounts: Vec::new(),
        };
    }
    if s.profiles.is_empty() {
        return Plan { card: "key", vars, profile: None, account: None, replaces: false, accounts: Vec::new() };
    }
    let accounts =
        s.profiles.iter().map(|p| json!({"profile": p.profile, "account": p.account, "agents": p.agents})).collect();
    Plan { card: "account", vars, profile: None, account: None, replaces: false, accounts }
}

/// `requestSecret` / the chat tool: makes the card and answers at once. Never waits for the owner (the answer is
/// applied by a resume), never carries a value.
pub async fn request(hub: &Arc<Hub>, o: &Origin, service: &str, purpose: &str) -> Value {
    let service = service.trim();
    if !label_ok(service) {
        return refused("Name the service in lowercase, like github.");
    }
    if !agent_ok(&o.agent) {
        return refused("Keys aren't available for this agent.");
    }
    let Some(state) = account_state(&o.agent, service).await else {
        return refused("Keys aren't available right now.");
    };
    if !state.declared || state.secrets.is_empty() || !state.secrets.iter().all(|v| var_ok(v)) {
        return refused("That service doesn't take a key here. Don't ask for it again.");
    }
    let plan = plan_for(&state);
    let purpose = clean_purpose(purpose);
    let Some(chat) = chat_for(hub, "", &o.agent) else {
        return refused("There is nowhere to ask: no chat with this agent on this computer.");
    };
    let cfg = settings(&hub.store);
    let now = now_ms();
    let ask = crate::access::Request { person: "", agent: &o.agent, owner_run: true, ask: crate::access::Ask::Secret };
    let answerable_by = crate::access::approver(&hub.store, &ask).await;
    let new = NewCard {
        session: o.session.clone(),
        source: o.source,
        bot_id: o.bot_id.clone(),
        run_id: o.run_id.clone(),
        person: String::new(),
        agent: o.agent.clone(),
        answerable_by,
        in_grants: false,
        tool: TOOL.to_owned(),
        input: json!({"service": service, "purpose": purpose}).to_string(),
        resource: format!("secret:{service}"),
        shown: format!("{service}\n\nWhy: {purpose}"),
        deadline: now.saturating_add(cfg.expiry_ms),
        reply_to: None,
        asked_by: None,
    };
    let card = match hub.store.card_open(&new, now) {
        Ok(Opened::New(c) | Opened::Joined(c) | Opened::Granted(c)) => c,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "couldn't store a secret card");
            return refused("Keys aren't available right now.");
        }
    };
    if !show(hub, &chat, &card, service, &purpose, &plan) {
        return refused("There is nowhere to ask: no chat with this agent on this computer.");
    }
    let owner = crate::api::roles::owner_name(&o.agent).await.unwrap_or_else(|| "the owner".into());
    json!({"ok": true, "carded": true, "message": TOLD.replace("{owner}", &owner)})
}

fn options_of(plan: &Plan) -> Value {
    let deny = json!({"optionId": "deny", "name": "Not now", "kind": "reject_once"});
    if plan.card != "account" {
        // An old app draws only these; the new ones draw the secure field from `data.secret`.
        return json!([deny]);
    }
    let mut all: Vec<Value> = plan
        .accounts
        .iter()
        .map(|a| {
            json!({"optionId": format!("profile:{}", a["profile"].as_str().unwrap_or_default()),
                   "name": format!("Use {}", a["account"].as_str().unwrap_or_default()), "kind": "allow_once"})
        })
        .collect();
    all.push(json!({"optionId": "another", "name": "Another account", "kind": "allow_once"}));
    all.push(deny);
    Value::Array(all)
}

/// Puts the card's entry in the owner's chat with the agent (a repeat reuses the entry it has) and tells the owner.
/// False when it could not be shown.
fn show(
    hub: &Arc<Hub>,
    chat: &crate::store::BotConfig,
    card: &Card,
    service: &str,
    purpose: &str,
    plan: &Plan,
) -> bool {
    if let Some(entry_id) = &card.entry_id
        && let Some(mut e) = hub.store.entry(entry_id)
    {
        e.data["status"] = "pending".into();
        e.data["parked"] = false.into();
        hub.set_entry(entry_id, &e.data);
        return hub.store.card_live(&card.id, None, card.run_id.as_deref()).ok().flatten().is_some();
    }
    let title = match (plan.card, plan.replaces) {
        ("account", _) => format!("{} wants access to {service}", chat.name),
        (_, true) => format!("{} needs a new key for {service}", chat.name),
        _ => format!("{} needs a key for {service}", chat.name),
    };
    let detail = if plan.card == "account" {
        format!("Choose which {service} account {} may use.", chat.name)
    } else {
        OLD_APP_DETAIL.to_owned()
    };
    let data = json!({
        "title": title,
        "toolKind": "secret",
        "command": Value::Null,
        "detail": detail,
        "diffs": [],
        "cwd": Value::Null,
        "options": options_of(plan),
        "status": "pending",
        "selected": Value::Null,
        "author": chat.id,
        "approver": "owner",
        "agent": card.agent,
        "cardId": card.id,
        "source": card.source,
        "card": plan.card,
        "accounts": plan.accounts,
        "secret": {
            "service": service,
            "vars": plan.vars,
            "profile": plan.profile,
            "account": plan.account,
            "purpose": purpose,
            "replaces": plan.replaces,
            "state": "pending",
        },
    });
    let lane = Lane::main(&chat.id);
    let turn = hub.store.max_turn(&chat.id);
    let Some(entry) = hub.add_entry(&lane, EntryKind::Permission, turn, &data) else { return false };
    if hub.store.card_live(&card.id, Some(&entry.id), card.run_id.as_deref()).ok().flatten().is_none() {
        return false;
    }
    // The owner's devices; the text names the agent and the service only.
    let mut target = chat.clone();
    target.person = None;
    crate::remote::push::notify(
        hub,
        &target,
        Some(&chat.id),
        &format!("{} needs you", chat.name),
        &title,
        crate::remote::push::AlertKind::NeedsInput,
    );
    true
}

// ---- one writer per card

/// Cards with an answer in flight. In memory: one host process; a restart loses it and the card's claim deadline
/// runs out on its own.
static SLOTS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The right to answer one card, held until it is dropped however the answer ends.
struct Slot(String);

impl Slot {
    fn take(card_id: &str) -> Option<Self> {
        let mut held = SLOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(card_id.to_owned()).then(|| Self(card_id.to_owned()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        SLOTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.0);
    }
}

fn closed() -> anyhow::Error {
    UserError(CLOSED.into()).into()
}

/// The secret card behind `entry_id`, when it is one and still open.
fn open_card(hub: &Hub, entry_id: &str) -> Option<(Card, Value)> {
    let card = hub.store.card_by_entry(entry_id).filter(|c| c.tool == TOOL && c.state.open())?;
    let data = hub.store.entry(entry_id)?.data;
    data["secret"].is_object().then_some((card, data))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_owned)).collect()
}

/// Writes the card's entry after an answer.
fn settle_entry(hub: &Hub, entry_id: &str, state: &str, selected: Option<&str>, profile: Option<(&str, &str)>) {
    let Some(mut e) = hub.store.entry(entry_id) else { return };
    e.data["status"] = (if selected.is_some() { "answered" } else { "cancelled" }).into();
    e.data["selected"] = selected.into();
    e.data["parked"] = false.into();
    e.data["secret"]["state"] = state.into();
    if let Some((profile, account)) = profile {
        e.data["secret"]["profile"] = profile.into();
        e.data["secret"]["account"] = account.into();
    }
    hub.set_entry(entry_id, &e.data);
}

/// The card is saved: closes its entry, offers the resume, and says whether one was offered.
fn finish(hub: &Arc<Hub>, card: &Card, state: &str, profile: &str) -> bool {
    let account = split_profile(profile).map_or("", |(_, a)| a);
    if let Some(id) = &card.entry_id {
        settle_entry(hub, id, state, Some(state), Some((profile, account)));
    }
    let resumed = settings(&hub.store).resume && super::resume::door_of(card).is_some();
    follow_up(hub, card);
    resumed
}

// ---- provideSecret

/// `provideSecret {entryId, account, values}`. The owner types a key into a key card.
///
/// Order, and why: the card is looked at, the fields are checked, the slot and the store's claim are taken (a second
/// device, a late retry or the clock now finds the card closed), THEN the key is written, THEN the card is answered. A
/// failed write releases the claim and leaves the card open. `values` is taken out of the body by the caller so the
/// only copies are the ones zeroed here.
pub async fn provide(
    hub: &Arc<Hub>,
    entry_id: &str,
    account: Option<&str>,
    values: BTreeMap<String, Zeroizing<String>>,
) -> anyhow::Result<Value> {
    let (card, data) = open_card(hub, entry_id).ok_or_else(closed)?;
    if data["card"] != "key" || card.deadline < now_ms() {
        return Err(closed());
    }
    let secret = &data["secret"];
    let service = secret["service"].as_str().unwrap_or_default().to_owned();
    let vars = strings(&secret["vars"]);
    let replaces = secret["replaces"] == true;
    let profile = if replaces {
        secret["profile"].as_str().filter(|p| split_profile(p).is_some_and(|(s, _)| s == service)).map(str::to_owned)
    } else {
        account.filter(|a| label_ok(a)).map(|a| format!("{service}/{a}"))
    };
    let Some(profile) = profile else { return Err(UserError(BAD_ACCOUNT.into()).into()) };
    let well_formed = !vars.is_empty()
        && vars.iter().all(|v| var_ok(v))
        && values.len() == vars.len()
        && vars
            .iter()
            .all(|v| values.get(v).is_some_and(|x| !x.is_empty() && x.len() <= MAX_VALUE && !x.contains('\0')));
    if values.values().any(|x| x.len() > MAX_VALUE) {
        return Err(UserError(TOO_LONG.into()).into());
    }
    if !well_formed {
        return Err(UserError(BAD_VALUES.into()).into());
    }
    if !replaces {
        // A new account never silently replaces a key the owner already has under that name.
        let rows = list_keys().await.map_err(|_| UserError(NOT_SAVED.into()))?;
        if rows["keys"].as_array().into_iter().flatten().any(|k| k["profile"] == profile.as_str()) {
            return Err(UserError(BAD_ACCOUNT.into()).into());
        }
    }
    let slot = Slot::take(&card.id).ok_or_else(closed)?;
    let started = now_ms();
    let claimed = hub.store.card_claim(&card.id, started, started + CLAIM_MS).ok().flatten().ok_or_else(closed)?;
    let written = write_key(&card.agent, &profile, &service, &vars, &values).await;
    drop(values);
    if !written {
        let _ = hub.store.card_claim_release(&card.id, claimed.deadline);
        return Err(UserError(NOT_SAVED.into()).into());
    }
    let saved = hub.store.card_saved(&card.id, "owner", started, now_ms() + RESUME_KEEP_MS).ok().flatten();
    let Some(saved) = saved else {
        // The claim kept every other closer out, so this is a store failure after the key was written. The key is in the
        // Keychain and granted; the card is left to expire.
        tracing::error!(card = %card.id, "a key was saved but its card couldn't be closed");
        return Err(UserError(NOT_SAVED.into()).into());
    };
    drop(slot);
    let resumed = finish(hub, &saved, "saved", &profile);
    Ok(json!({"saved": {"service": service, "vars": vars, "profile": profile}, "resumed": resumed}))
}

/// `kith-auth set <profile> --stdin-json --grant <agent>` with the values on stdin. True when it exited 0 and said it
/// wrote this profile for this service with these names.
async fn write_key(
    agent: &str,
    profile: &str,
    service: &str,
    vars: &[String],
    values: &BTreeMap<String, Zeroizing<String>>,
) -> bool {
    if !agent_ok(agent) || split_profile(profile).is_none_or(|(s, _)| s != service) {
        return false;
    }
    // Room for every byte to become a six-byte escape, so the buffer never grows (a growth would leave a copy behind).
    let room: usize = values.iter().map(|(k, v)| (k.len() + v.len()) * 6 + 8).sum::<usize>() + 8;
    let mut body = Zeroizing::new(Vec::with_capacity(room));
    body.push(b'{');
    for (i, name) in vars.iter().enumerate() {
        let Some(value) = values.get(name) else { return false };
        if i > 0 {
            body.push(b',');
        }
        if serde_json::to_writer(&mut *body, name).is_err() || {
            body.push(b':');
            serde_json::to_writer(&mut *body, value.as_str()).is_err()
        } {
            return false;
        }
    }
    body.push(b'}');
    let Some(ran) = kith_auth(&["set", profile, "--stdin-json", "--grant", agent], Some(body)).await else {
        return false;
    };
    if !ran.ok {
        return false;
    }
    let Ok(said) = serde_json::from_slice::<Value>(ran.stdout.split(|b| *b == b'\n').next().unwrap_or_default()) else {
        return false;
    };
    said["profile"] == profile && said["service"] == service
}

// ---- respondPermission on a secret card

/// The owner's choice on a secret card (`respondPermission`). Who may answer was decided before this (roles.rs).
pub async fn respond(hub: &Arc<Hub>, entry_id: &str, option: Option<&str>) -> anyhow::Result<Value> {
    let Some((card, data)) = open_card(hub, entry_id) else { return Ok(json!({"closed": true})) };
    let offered = |o: &str| crate::api::roles::offered(&data, o);
    match option {
        None | Some("deny") => {
            deny(hub, &card, option).map(|closed| if closed { json!({"closed": true}) } else { json!({}) })
        }
        Some(o) if !offered(o) => Err(UserError("That isn't one of the choices on this request.".into()).into()),
        Some("another") => {
            if data["card"] != "account" {
                return Err(UserError("That isn't one of the choices on this request.".into()).into());
            }
            let Some(_slot) = Slot::take(&card.id) else { return Ok(json!({"closed": true})) };
            let Some(mut e) = hub.store.entry(entry_id) else { return Ok(json!({"closed": true})) };
            if hub.store.card(&card.id).is_none_or(|c| !c.state.open()) {
                return Ok(json!({"closed": true}));
            }
            e.data["card"] = "key".into();
            e.data["detail"] = OLD_APP_DETAIL.into();
            e.data["options"] = json!([{"optionId": "deny", "name": "Not now", "kind": "reject_once"}]);
            e.data["secret"]["replaces"] = false.into();
            e.data["secret"]["profile"] = Value::Null;
            e.data["secret"]["account"] = Value::Null;
            hub.set_entry(entry_id, &e.data);
            Ok(json!({}))
        }
        Some(o) => {
            let Some(profile) = o.strip_prefix("profile:").filter(|_| data["card"] == "account") else {
                return Err(UserError("That isn't one of the choices on this request.".into()).into());
            };
            grant(hub, &card, &data, profile).await
        }
    }
}

/// A deny (or cancel). True when the card turned out to be closed already.
fn deny(hub: &Arc<Hub>, card: &Card, option: Option<&str>) -> anyhow::Result<bool> {
    let Some(_slot) = Slot::take(&card.id) else { return Ok(true) };
    let applied = hub.store.card_answer(&card.id, false, option.unwrap_or("cancel"), "owner", None, now_ms(), false)?;
    let crate::store::Answered::Applied { card, .. } = applied else {
        if hub.store.card(&card.id).is_some_and(|c| c.state == CardState::Expired) {
            super::close_entry(hub, card, "expired");
        }
        return Ok(true);
    };
    if let Some(id) = &card.entry_id {
        settle_entry(hub, id, "denied", option, None);
    }
    Ok(false)
}

/// "Use this account": the grant goes to kith-auth under the same claim as a key write, then the card is saved.
async fn grant(hub: &Arc<Hub>, card: &Card, data: &Value, profile: &str) -> anyhow::Result<Value> {
    let service = data["secret"]["service"].as_str().unwrap_or_default();
    let listed = data["accounts"].as_array().into_iter().flatten().any(|a| a["profile"] == profile);
    if !listed || split_profile(profile).is_none_or(|(s, _)| s != service) || !agent_ok(&card.agent) {
        return Err(UserError("That isn't one of the choices on this request.".into()).into());
    }
    let slot = Slot::take(&card.id).ok_or_else(closed)?;
    let started = now_ms();
    let claimed = hub.store.card_claim(&card.id, started, started + CLAIM_MS).ok().flatten().ok_or_else(closed)?;
    let ran = kith_auth(&["grant", &card.agent, service, profile], None).await;
    if !ran.is_some_and(|r| r.ok) {
        let _ = hub.store.card_claim_release(&card.id, claimed.deadline);
        return Err(UserError("Couldn't give access".into()).into());
    }
    let Some(saved) = hub.store.card_saved(&card.id, "owner", started, now_ms() + RESUME_KEEP_MS).ok().flatten() else {
        tracing::error!(card = %card.id, "access was granted but its card couldn't be closed");
        return Err(UserError("Couldn't give access".into()).into());
    };
    drop(slot);
    finish(hub, &saved, "saved", profile);
    Ok(json!({}))
}

// ---- the resume note

/// What the agent is told when a secret card was answered: names only.
pub fn note(hub: &Hub, who: &str, card: &Card) -> String {
    let data = card.entry_id.as_deref().and_then(|id| hub.store.entry(id)).map(|e| e.data).unwrap_or_default();
    let vars = strings(&data["secret"]["vars"]).join(", ");
    let profile = data["secret"]["profile"].as_str().unwrap_or_default();
    let service = data["secret"]["service"].as_str().unwrap_or("the service");
    let what = if vars.is_empty() { service.to_owned() } else { vars };
    format!("{who} saved {what} ({profile}) for you. Use it through kith-auth exec; never print it.")
}

// ---- the owner's list of keys

fn keys_unavailable() -> anyhow::Error {
    UserError("Couldn't read the keys right now".into()).into()
}

/// `listKeys`: kith-auth's rows (names, accounts, who holds each; never a value).
pub async fn list_keys() -> anyhow::Result<Value> {
    let ran = kith_auth(&["keys", "--json"], None).await.filter(|r| r.ok).ok_or_else(keys_unavailable)?;
    let keys: Value = serde_json::from_slice(&ran.stdout).map_err(|_| keys_unavailable())?;
    anyhow::ensure!(keys.is_array(), keys_unavailable());
    Ok(json!({"keys": keys}))
}

/// `revokeKey {agent, service, profile}`: takes one agent's access to one account away.
pub async fn revoke_key(agent: &str, service: &str, profile: &str) -> anyhow::Result<Value> {
    if !agent_ok(agent) || !label_ok(service) || split_profile(profile).is_none_or(|(s, _)| s != service) {
        return Err(UserError("That key can't be removed".into()).into());
    }
    let ran = kith_auth(&["revoke", agent, service, "--profile", profile], None).await;
    if !ran.is_some_and(|r| r.ok) {
        return Err(UserError("Couldn't remove access right now".into()).into());
    }
    Ok(json!({"ok": true}))
}

// ---- the chat tool

/// The `request_secret` tool of the built-in chat servers.
pub fn tool() -> Value {
    json!({
        "name": TOOL,
        "description": "Ask the owner for a service's key. The owner gets a card in this chat and picks an account or types the key into a private field. You never see the key: it is saved to the Keychain and you are told its name, then use it through `kith-auth exec`. Ask once, then end your turn.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "service": {"type": "string", "description": "The service, lowercase, as kith-auth declares it (github, notion)."},
                "purpose": {"type": "string", "description": "Why you need it, in a few words."},
            },
            "required": ["service"],
        },
        "annotations": {"readOnlyHint": false, "idempotentHint": false},
    })
}

/// A chat's `request_secret` call. Only the owner's own chat with an hq agent can ask (v1): a chat with a person set
/// (an invitee, a bot guest) is refused.
pub async fn chat_request(hub: &Arc<Hub>, bot: &str, actor: &str, args: &Value) -> anyhow::Result<Value> {
    let row = hub.store.bot(bot)?.filter(|r| !r.deleted);
    let Some(config) = row.map(|r| r.config).filter(|c| c.person.is_none() && !c.is_group()) else {
        return Ok(refused("Only the owner's own chat can ask for a key."));
    };
    let Some(agent) = config.hq_agent.as_deref() else {
        return Ok(refused("Only agents set up in Kith can ask for a key."));
    };
    let lane = crate::agent::bot::place_of(actor);
    let origin = Origin::chat(bot, agent, &lane);
    let service = args["service"].as_str().unwrap_or_default();
    Ok(request(hub, &origin, service, args["purpose"].as_str().unwrap_or_default()).await)
}

#[cfg(test)]
mod tests;
