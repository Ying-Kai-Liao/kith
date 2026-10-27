//! Approval cards: a limit that is not a secret or a login becomes a card for the approver, instead of a refusal
//! (designs/2026-10-24-approval-cards.md). The store (`store/cards.rs`) holds the states; this module is what the host
//! does around them: the `requestPermission` call of a `claude -p` run (the run socket, `runauth.rs`), answers from the
//! approver's device (`respondPermission`), and the clock, restart and revoke edges.
//!
//! A run asks while its process is up and waits a short live window (`cards.live_window_s`, default 90 s) for an
//! answer. No answer in the window: it is told to end its turn (A9, no process waits on a parked card) and the card
//! stays live until the process is gone, then parks (C6). An answer while the process is up goes through the live
//! channel and uses the once grant (C3); an answer while it is gone is recorded, as an approved card with its grant.
//!
//! The resume after such an answer, and on restart "grant without resume -> owed" (C4's second half), is `resume`.
//! `follow_up` is its one call site; the setting `cards.resume` (default on) is the kill switch.

pub mod access;
pub mod resource;
pub mod resume;
pub mod secret;

use crate::hub::Hub;
use crate::runauth::{RunIdentity, TokenScope};
use crate::store::{Answered, Card, CardState, EntryKind, Lane, NewCard, Opened, Store, now_ms};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

const WINDOW_KEY: &str = "cards.live_window_s";
const EXPIRY_KEY: &str = "cards.expiry_hours";
const RESUME_KEY: &str = "cards.resume";
const DEFAULT_WINDOW: Duration = Duration::from_secs(90);
const DEFAULT_EXPIRY_MS: i64 = 24 * 60 * 60 * 1000;
const SWEEP_EVERY: Duration = Duration::from_secs(60);
const MAX_INPUT: usize = 32 * 1024;

/// The host's settings for cards, read each time they are needed (kv keys above; none set is the default).
pub struct Settings {
    pub window: Duration,
    pub expiry_ms: i64,
    pub resume: bool,
}

pub fn settings(store: &Store) -> Settings {
    let num = |k: &str| store.kv_get(k).and_then(|v| v.trim().parse::<u64>().ok());
    Settings {
        window: num(WINDOW_KEY).map_or(DEFAULT_WINDOW, |s| Duration::from_secs(s.min(600))),
        expiry_ms: num(EXPIRY_KEY)
            .map_or(DEFAULT_EXPIRY_MS, |h| i64::try_from(h.clamp(1, 24 * 30) * 3_600_000).unwrap_or(DEFAULT_EXPIRY_MS)),
        resume: !matches!(store.kv_get(RESUME_KEY).as_deref(), Some("off" | "false" | "0")),
    }
}

// ---- the live channel of a -p run

/// What a waiting `requestPermission` call is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

type Waiters = HashMap<String, Vec<oneshot::Sender<Verdict>>>;
/// The waiting calls by card id. In memory only: a restart loses them and the cards park (C4).
static LIVE: LazyLock<Mutex<Waiters>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_live() -> std::sync::MutexGuard<'static, Waiters> {
    LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Tells every call waiting on `card_id`. True when at least one was still there to hear it.
///
/// A yes is one use (A4): the first call waiting hears it; a repeat that joined the card is told to ask again.
fn deliver(card_id: &str, verdict: &Verdict) -> bool {
    let senders = lock_live().remove(card_id).unwrap_or_default();
    let mut heard = false;
    for tx in senders {
        let v = match verdict {
            Verdict::Allow if heard => {
                Verdict::Deny("That approval was used by an identical request; ask again.".into())
            }
            v => v.clone(),
        };
        heard |= tx.send(v).is_ok();
    }
    heard
}

/// A waiting call's place in [`LIVE`], taken out when it stops waiting however it ends.
struct Waiting(String);

impl Waiting {
    fn new(card_id: &str) -> (Self, oneshot::Receiver<Verdict>) {
        let (tx, rx) = oneshot::channel();
        lock_live().entry(card_id.to_owned()).or_default().push(tx);
        (Self(card_id.to_owned()), rx)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut live = lock_live();
        if let Some(list) = live.get_mut(&self.0) {
            list.retain(|tx| !tx.is_closed());
            if list.is_empty() {
                live.remove(&self.0);
            }
        }
    }
}

// ---- requestPermission

#[derive(Debug, PartialEq, Eq)]
pub struct Decision {
    pub allow: bool,
    pub message: String,
}

fn deny(message: impl Into<String>) -> Decision {
    Decision { allow: false, message: message.into() }
}

/// The agent-visible tool kind the apps already know how to draw.
fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "Bash" | "BashOutput" | "KillShell" => "execute",
        "Read" | "Glob" | "Grep" | "LS" | "NotebookRead" => "read",
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => "edit",
        "WebFetch" | "WebSearch" => "fetch",
        _ => "other",
    }
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.lines().next().unwrap_or_default().chars().take(n).collect();
    if one.len() < s.trim().len() { format!("{one}…") } else { one }
}

fn title_of(tool: &str, shown: &str) -> String {
    match tool_kind(tool) {
        "execute" => format!("Run {}", short(shown, 80)),
        "other" => format!("Use {tool}"),
        _ => format!("{tool} {}", short(shown, 80)),
    }
}

/// The chat a -p run's card appears in: the person's own chat with the agent, else (the owner's run, or a person with
/// no chat yet) the owner's chat with it. Cards are Permission entries, so the apps already show them there.
fn chat_for(hub: &Hub, person: &str, agent: &str) -> Option<crate::store::BotConfig> {
    let rows: Vec<_> =
        hub.store.bots().unwrap_or_default().into_iter().filter(|r| !r.deleted).map(|r| r.config).collect();
    let of_agent = |c: &&crate::store::BotConfig| c.hq_agent.as_deref() == Some(agent);
    if !person.is_empty()
        && let Some(c) = rows.iter().filter(of_agent).find(|c| c.person.as_deref() == Some(person))
    {
        return Some(c.clone());
    }
    rows.iter().filter(of_agent).find(|c| c.person.is_none()).cloned()
}

/// Whom a card goes to for a run of `who`: the owner for the owner's own run, else whoever kith-auth names now, and always the owner for a tool that runs code.
async fn approver_of(who: &RunIdentity, tool: &str, input: &Value) -> &'static str {
    if who.scope == TokenScope::OwnerAsk || who.person.is_empty() {
        return "owner";
    }
    // A file outside the run's grants is the owner's to allow, whoever the run is for (F11 step 2a).
    let inside = crate::api::roles::within(&crate::api::roles::grant_roots(&who.person, &who.agent), tool, input);
    if inside && crate::api::roles::approves_own(&who.person, &who.agent, tool, tool_kind(tool)).await {
        "member"
    } else {
        "owner"
    }
}

/// The host half of `--permission-prompt-tool` for a -p run (A5, A6, A9). Never panics, never blocks past the window.
pub async fn request_permission(hub: &Arc<Hub>, who: &RunIdentity, tool: &str, input: &Value) -> Decision {
    if input.to_string().len() > MAX_INPUT {
        return deny("That request is too large to show for approval.");
    }
    let (resource, shown) = match resource::classify(tool, input, &resource::roots()) {
        resource::Classified::Card { key, shown } => (key, shown),
        resource::Classified::Never => {
            return deny("That is a secret or a login. It can't be approved; don't try another way to reach it.");
        }
        resource::Classified::Malformed(why) => return deny(format!("Can't ask about that: {why}.")),
    };
    request_card(hub, who, tool, input, &resource, &shown).await
}

/// The part after classification, split out so tests can name the resource.
async fn request_card(
    hub: &Arc<Hub>,
    who: &RunIdentity,
    tool: &str,
    input: &Value,
    resource: &str,
    shown: &str,
) -> Decision {
    let cfg = settings(&hub.store);
    let approver = approver_of(who, tool, input).await;
    let now = now_ms();
    let new = NewCard {
        session: who.session.clone(),
        source: "run",
        bot_id: None,
        run_id: Some(who.run_id.clone()),
        person: who.person.clone(),
        agent: who.agent.clone(),
        approver,
        tool: tool.to_owned(),
        input: input.to_string(),
        resource: resource.to_owned(),
        shown: shown.to_owned(),
        deadline: now.saturating_add(cfg.expiry_ms),
    };
    let card = match hub.store.card_open(&new, now) {
        Ok(Opened::Granted(_)) => return Decision { allow: true, message: "Allowed once.".into() },
        Ok(Opened::New(c) | Opened::Joined(c)) => c,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "couldn't store an approval card");
            return deny("Approvals aren't available right now.");
        }
    };
    // Listening comes first: an answer that lands the moment the card is shown must find this call.
    let (_waiting, rx) = Waiting::new(&card.id);
    let Some(card) = show(hub, who, &card) else {
        return deny("There is nowhere to ask about this: no chat with this agent on this computer.");
    };
    let label = approver_label(hub, &card).await;
    match tokio::time::timeout(cfg.window, rx).await {
        Ok(Ok(Verdict::Allow)) => Decision { allow: true, message: "Approved.".into() },
        Ok(Ok(Verdict::Deny(why))) => deny(why),
        Ok(Err(_)) | Err(_) => deny(format!(
            "This is waiting on {label} to approve ({}). Don't retry it or try another way around. End your turn now and \
             tell the user the request is waiting for approval.",
            short(&card.shown, 80)
        )),
    }
}

async fn approver_label(hub: &Hub, card: &Card) -> String {
    if card.approver == "owner" {
        return crate::api::roles::owner_name(&card.agent).await.unwrap_or_else(|| "the owner".into());
    }
    hub.store.person(&card.person).map(|p| p.name).filter(|n| !n.is_empty()).unwrap_or_else(|| "the member".into())
}

/// Makes the card live and puts its Permission entry where the approver sees it (a repeat reuses the entry it has).
fn show(hub: &Arc<Hub>, who: &RunIdentity, card: &Card) -> Option<Card> {
    if let Some(entry_id) = &card.entry_id
        && let Some(mut e) = hub.store.entry(entry_id)
    {
        e.data["status"] = "pending".into();
        e.data["parked"] = false.into();
        hub.set_entry(entry_id, &e.data);
        return hub.store.card_live(&card.id, None, Some(&who.run_id)).ok().flatten();
    }
    let chat = chat_for(hub, &who.person, &who.agent)?;
    let escape = card.tool == access::TOOL;
    let (title, kind) = (title_of(&card.tool, &card.shown), tool_kind(&card.tool));
    let (title, detail, options) = if escape {
        access::entry_view(card)
    } else {
        (
            title,
            crate::agent::acp::truncate(&card.shown, 6000),
            json!([
                {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
            ]),
        )
    };
    let command = (kind == "execute").then(|| card.shown.clone());
    let mut data = json!({
        "title": title,
        "toolKind": kind,
        "command": command,
        "detail": detail,
        "diffs": [],
        "cwd": Value::Null,
        "options": options,
        "status": "pending",
        "selected": Value::Null,
        "author": chat.id,
        "approver": card.approver,
        "agent": who.agent,
        "cardId": card.id,
        "source": "run",
    });
    if !who.person.is_empty() {
        let name = hub.store.person(&who.person).map(|p| p.name).filter(|n| !n.is_empty());
        data["person"] = who.person.as_str().into();
        data["personName"] = name.unwrap_or_else(|| who.person.clone()).into();
    }
    let lane = Lane::main(&chat.id);
    let turn = hub.store.max_turn(&chat.id);
    let entry = hub.add_entry(&lane, EntryKind::Permission, turn, &data)?;
    let live = hub.store.card_live(&card.id, Some(&entry.id), Some(&who.run_id)).ok().flatten()?;
    // Tell the approver: the owner's devices for an owner card, the member's own for theirs.
    let mut target = chat.clone();
    let mut heading = format!("{} needs you", chat.name);
    if live.approver == "owner" {
        target.person = None;
        if !who.person.is_empty() {
            heading = format!("{} needs your OK", data["personName"].as_str().unwrap_or_default());
        }
    }
    crate::remote::push::notify(
        hub,
        &target,
        Some(&chat.id),
        &heading,
        &title,
        crate::remote::push::AlertKind::NeedsInput,
    );
    Some(live)
}

// ---- answers

/// How an answer on a card came out.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No card, a closed one, or one past its deadline: nothing happened (A1). Old entries with no row read this way.
    Closed,
    /// The answer was recorded. `forward` is set for a bot chat's card whose process was up: the caller hands the
    /// answer to the bot's actor, which holds the request id, and the actor reports back through [`delivered`].
    Recorded { forward: bool },
}

/// Whether the option `option_id` of the entry's card allows. No option is a cancel.
pub fn allows(entry_data: &Value, option_id: Option<&str>) -> bool {
    let Some(o) = option_id else { return false };
    entry_data["options"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|x| x["optionId"].as_str() == Some(o) && x["kind"].as_str().is_some_and(|k| k.starts_with("allow")))
}

/// An approver's answer to the card behind `entry_id` (`respondPermission`). Who may answer was decided before this
/// (roles.rs); here only the card decides whether the answer counts.
pub fn answer(hub: &Arc<Hub>, entry_id: &str, option_id: Option<&str>, by: &str) -> anyhow::Result<Outcome> {
    let Some(entry) = hub.store.entry(entry_id) else { return Ok(Outcome::Closed) };
    let Some(card) = hub.store.card_by_entry(entry_id) else { return Ok(Outcome::Closed) };
    let allow = allows(&entry.data, option_id);
    let Answered::Applied { card, was_live } =
        hub.store.card_answer(&card.id, allow, option_id.unwrap_or("cancel"), by, now_ms(), allow)?
    else {
        // An expired one shows as expired now, whatever the sweep was doing.
        if let Some(c) = hub.store.card(&card.id).filter(|c| c.state == CardState::Expired) {
            close_entry(hub, &c, "expired");
        }
        return Ok(Outcome::Closed);
    };
    let status = if option_id.is_some() { "answered" } else { "cancelled" };
    let mut forward = false;
    if card.tool == access::TOOL {
        // No process waits on an escape card: the yes is applied at the session's next activation (see `access`).
        access::answered(hub, &card, allow, option_id);
        // A process of that conversation that is kept warm (an app chat's) starts again before its next turn (F2).
        if allow {
            crate::runauth::access_granted(&card.session);
        }
    } else if was_live && card.source == "run" {
        let verdict = if allow { Verdict::Allow } else { Verdict::Deny("The approver said no.".into()) };
        // A yes to a live asker was spent in the answer itself (no parallel ask can take it too); if nobody was
        // there to hear it, it goes back as the answer to a parked card.
        if !deliver(&card.id, &verdict) && allow && hub.store.card_undelivered(&card.id)? {
            follow_up(hub, &card);
        }
    } else if was_live {
        forward = true;
    } else if allow {
        follow_up(hub, &card);
    }
    if let Some(mut e) = hub.store.entry(entry_id) {
        e.data["status"] = status.into();
        e.data["selected"] = option_id.into();
        e.data["parked"] = false.into();
        hub.set_entry(entry_id, &e.data);
    }
    Ok(Outcome::Recorded { forward })
}

/// The bot actor could not hand a yes to its agent (it had no request to answer): the grant goes back.
pub fn undelivered(store: &Store, entry_id: &str) {
    if let Some(card) = store.card_by_entry(entry_id)
        && let Err(error) = store.card_undelivered(&card.id)
    {
        tracing::warn!(error = format!("{error:#}"), "couldn't return a grant");
    }
}

/// The bot actor handed an answer to its agent (or cancelled the question itself): the once grant stays spent (C3,
/// at the answer), or the open card is withdrawn.
pub fn delivered(store: &Store, entry_id: &str, allow: bool) {
    let Some(card) = store.card_by_entry(entry_id) else { return };
    let done = match card.state {
        CardState::Approved if allow => store.card_use(&card.id).map(drop),
        s if s.open() => store.card_withdraw(&card.id).map(drop),
        _ => Ok(()),
    };
    if let Err(error) = done {
        tracing::warn!(error = format!("{error:#}"), "couldn't settle a card");
    }
}

// ---- the edges: process exit, revoke, clock, restart

fn close_entry(hub: &Hub, card: &Card, status: &str) {
    let Some(id) = &card.entry_id else { return };
    if let Some(mut e) = hub.store.entry(id)
        && e.data["status"] == "pending"
    {
        e.data["status"] = status.into();
        e.data["parked"] = false.into();
        // A secret card says how it ended in its own words (the apps draw that, not the status).
        if e.data["secret"].is_object() {
            e.data["secret"]["state"] = (if status == "expired" { "expired" } else { "denied" }).into();
        }
        hub.set_entry(id, &e.data);
    }
}

fn mark_parked(hub: &Hub, cards: &[Card]) {
    for id in cards.iter().filter_map(|c| c.entry_id.as_ref()) {
        if let Some(mut e) = hub.store.entry(id)
            && e.data["status"] == "pending"
        {
            e.data["parked"] = true.into();
            hub.set_entry(id, &e.data);
        }
    }
}

/// A -p run's process is gone: its cards park (C6). The entry stays answerable; the answer is recorded.
pub fn run_exited(hub: &Hub, run_id: &str) {
    match hub.store.cards_park_run(run_id) {
        Ok(parked) => mark_parked(hub, &parked),
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't park cards"),
    }
}

/// A bot chat's agent process is gone with a question open: its cards park.
pub fn bot_exited(hub: &Hub, bot_id: &str) {
    match hub.store.cards_park_bot(bot_id) {
        Ok(parked) => mark_parked(hub, &parked),
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't park cards"),
    }
}

/// A turn ended with a question still open: its asker is gone, so the card parks (C6) and the entry stays
/// answerable. False when the entry has no open card (an old one).
pub fn park_entry(hub: &Hub, entry_id: &str) -> bool {
    let Some(card) = hub.store.card_by_entry(entry_id).filter(|c| c.state.open()) else { return false };
    if hub.store.card_park(&card.id).unwrap_or(false) {
        mark_parked(hub, &[card]);
    }
    true
}

/// A failed attempt's question can never be answered usefully: withdrawn, shown as expired like before.
pub fn withdraw_entry(hub: &Hub, entry_id: &str) {
    if let Some(card) = hub.store.card_by_entry(entry_id)
        && hub.store.card_withdraw(&card.id).unwrap_or(false)
    {
        close_entry(hub, &card, "expired");
    }
}

/// The person's access ended (A7): their open cards and unused grants are withdrawn, and a waiting call is told so.
/// `agent` narrows it to one agent they lost.
pub fn withdraw_person(hub: &Hub, person: &str, agent: Option<&str>) {
    let gone = match hub.store.cards_withdraw_person(person, agent) {
        Ok(gone) => gone,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), person, "couldn't withdraw cards");
            return;
        }
    };
    for card in &gone {
        deliver(&card.id, &Verdict::Deny("That access was withdrawn.".into()));
        close_entry(hub, card, "cancelled");
    }
}

/// Past their deadline, cards expire: silence is no (A1).
pub fn sweep(hub: &Hub) {
    match hub.store.cards_sweep(now_ms()) {
        Ok(expired) => {
            for card in &expired {
                deliver(&card.id, &Verdict::Deny("The request expired without an answer.".into()));
                close_entry(hub, card, "expired");
            }
        }
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't expire cards"),
    }
}

/// The resume half (see `resume`): an approved card whose asker was gone owes its session a turn. On unless
/// `cards.resume` is set to `off` (a kill switch); the store keeps the card owed either way, so switching it back on
/// (and the next start) delivers what waited.
pub fn follow_up(hub: &Arc<Hub>, card: &Card) {
    if settings(&hub.store).resume {
        resume::offer(hub, card);
    }
}

/// At startup: live waits are lost, so live cards park (C4); the grant of an approved card is re-derived and its
/// follow-up offered again. Then the clock runs.
pub fn start(hub: &Arc<Hub>) {
    match hub.store.cards_restart(now_ms()) {
        Ok(parked) => mark_parked(hub, &parked),
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't restore cards"),
    }
    for card in hub.store.cards_awaiting_resume(now_ms()) {
        follow_up(hub, &card);
    }
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            let Some(hub) = weak.upgrade() else { return };
            sweep(&hub);
            // An owner token handed out over the API ends with its process or its time.
            let gone = tokio::task::spawn_blocking(|| crate::runauth::table().reap(crate::runauth::pid_alive))
                .await
                .unwrap_or_default();
            for run in gone {
                run_exited(&hub, &run);
            }
        }
    });
}

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod resume_tests;
