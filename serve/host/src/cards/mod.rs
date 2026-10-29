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
pub mod edge;
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
/// What a person is told for a card that isn't theirs to answer.
pub const NOT_YOURS: &str = "Only the owner can answer this agent's permission requests for you.";

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
    /// No answer in the live window: the run ends its turn on the card (a `kith ask` that started it waits for it).
    pub waiting: bool,
}

fn deny(message: impl Into<String>) -> Decision {
    Decision { allow: false, message: message.into(), waiting: false }
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

/// The lane of an agent's cards that have no chat on this computer (`cards:<agent>`). It has no bot row: no roster
/// entry, no sends, no unread count. The apps show its pending cards in the approver's Needs-you list (rule 5).
const LANE: &str = "cards:";

/// The card lane of `agent`, as the chat a card and its push name (never stored as a bot).
fn lane_of(agent: &str) -> crate::store::BotConfig {
    serde_json::from_value(json!({"id": format!("{LANE}{agent}"), "name": agent, "hqAgent": agent}))
        .expect("a bot config from an id and a name")
}

/// Where a -p run's card appears: the person's own chat with the agent, else (the owner's run, or a person with no chat
/// yet) the owner's chat with it, else the agent's card lane. Cards are Permission entries, so the apps already show
/// them in a chat, and the lane's in Needs-you.
fn chat_for(hub: &Hub, person: &str, agent: &str) -> crate::store::BotConfig {
    let rows: Vec<_> =
        hub.store.bots().unwrap_or_default().into_iter().filter(|r| !r.deleted).map(|r| r.config).collect();
    let of_agent = |c: &&crate::store::BotConfig| c.hq_agent.as_deref() == Some(agent);
    if !person.is_empty()
        && let Some(c) = rows.iter().filter(of_agent).find(|c| c.person.as_deref() == Some(person))
    {
        return c.clone();
    }
    rows.iter().filter(of_agent).find(|c| c.person.is_none()).cloned().unwrap_or_else(|| lane_of(agent))
}

/// The chat `id` names in an entry's `author`: a bot's, or an agent's card lane.
fn chat_of(hub: &Hub, id: &str) -> Option<crate::store::BotConfig> {
    match id.strip_prefix(LANE) {
        Some(agent) => Some(lane_of(agent)),
        None => hub.store.bot(id).ok().flatten().map(|r| r.config),
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
    // A file outside the run's grants is the owner's to allow, whoever the run is for (F11 step 2a).
    let in_grants = crate::access::tool_in_grants(&who.person, &who.agent, tool, tool_kind(tool), input);
    let req = crate::access::Request {
        person: &who.person,
        agent: &who.agent,
        owner_run: who.scope == TokenScope::OwnerAsk,
        ask: crate::access::Ask::Tool { in_grants },
    };
    let answerable_by = crate::access::approver(&hub.store, &req).await;
    let now = now_ms();
    let new = NewCard {
        session: who.session.clone(),
        source: "run",
        bot_id: None,
        run_id: Some(who.run_id.clone()),
        person: who.person.clone(),
        agent: who.agent.clone(),
        answerable_by,
        in_grants: req.in_grants(),
        tool: tool.to_owned(),
        input: input.to_string(),
        resource: resource.to_owned(),
        shown: shown.to_owned(),
        deadline: now.saturating_add(cfg.expiry_ms),
        reply_to: who.reply.as_ref().map(|r| r.to.clone()),
        asked_by: who.reply.as_ref().and_then(|r| r.asked_by.clone()),
    };
    let card = match hub.store.card_open(&new, now) {
        Ok(Opened::Granted(_)) => return Decision { allow: true, message: "Allowed once.".into(), waiting: false },
        Ok(Opened::New(c) | Opened::Joined(c)) => c,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "couldn't store an approval card");
            return deny("Approvals aren't available right now.");
        }
    };
    // Listening comes first: an answer that lands the moment the card is shown must find this call.
    let (_waiting, rx) = Waiting::new(&card.id);
    let extra = edge_extras(hub, who, &card).await;
    let Some(card) = show(hub, who, &card, extra) else {
        return deny("Approvals aren't available right now.");
    };
    let label = approver_label(hub, &card).await;
    match tokio::time::timeout(cfg.window, rx).await {
        Ok(Ok(Verdict::Allow)) => Decision { allow: true, message: "Approved.".into(), waiting: false },
        Ok(Ok(Verdict::Deny(why))) => deny(why),
        Ok(Err(_)) | Err(_) => Decision {
            waiting: true,
            ..deny(format!(
                "This is waiting on {label} to approve ({}). Don't retry it or try another way around. End your turn now \
                 and tell the user the request is waiting for approval.",
                short(&card.shown, 80)
            ))
        },
    }
}

/// "<agent>, asked by <caller>" for a card a `kith ask` run opened (rule 7): whose request it is reaches the approver.
fn chain_of(card: &Card) -> Option<String> {
    card.reply_to.as_ref()?;
    Some(format!("{}, asked by {}", card.agent, card.asked_by.as_deref().unwrap_or("a terminal")))
}

/// A card a `kith ask` run parked on ended without a yes: that is the ask's answer (rule 7). The kernel's mailbox keeps it
/// for the asker's `kith ask --wait`, or wakes the asker's session (`kith-ask --answer`).
fn tell_asker(card: &Card, how: &str) {
    let Some(id) = &card.reply_to else { return };
    let text = format!("{}'s request ({}) {how}.", card.agent, short(&card.shown, 80));
    let args = ["ask", "--answer", id.as_str(), "--as", card.agent.as_str(), "--", text.as_str()].map(str::to_owned);
    #[cfg(test)]
    resume::tests_seen::asked(args.to_vec());
    #[cfg(not(test))]
    {
        let Some(bin) = crate::agent::hq::bin() else {
            tracing::warn!(card = %card.id, "no kernel to answer a kith ask with");
            return;
        };
        let env = std::env::vars().filter(|(k, _)| crate::channels::run::inherited(k));
        let spawned = std::process::Command::new(bin)
            .args(&args)
            .env_clear()
            .envs(env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            // Reaped off the runtime: a short write to the mailbox, or a wake request.
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(error) => tracing::warn!(card = %card.id, error = %error, "couldn't answer a kith ask"),
        }
    }
}

async fn approver_label(hub: &Hub, card: &Card) -> String {
    let Some(p) = &card.answerable_by else {
        return crate::api::roles::owner_name(&card.agent).await.unwrap_or_else(|| "the owner".into());
    };
    hub.store.person(p).map(|p| p.name).filter(|n| !n.is_empty()).unwrap_or_else(|| "the member".into())
}

/// What the owner's card for an edge ask of an invitee's run adds (see `edge`): none for a `kith ask` run's card, whose
/// grants would land on the helper's row, not the caller's.
pub(super) async fn edge_extras(hub: &Hub, who: &RunIdentity, card: &Card) -> Option<edge::Extra> {
    if card.approver != "owner" || who.person.is_empty() || card.reply_to.is_some() {
        return None;
    }
    let e = edge::of(&card.tool, &serde_json::from_str(&card.input).unwrap_or(Value::Null), &card.resource)?;
    let name =
        hub.store.person(&who.person).map(|p| p.name).filter(|n| !n.is_empty()).unwrap_or_else(|| who.person.clone());
    Some(edge::extras(&who.person, &name, &who.agent, &e, card.tool != access::TOOL).await)
}

/// Makes the card live and puts its Permission entry where the approver sees it (a repeat reuses the entry it has).
fn show(hub: &Arc<Hub>, who: &RunIdentity, card: &Card, extra: Option<edge::Extra>) -> Option<Card> {
    if let Some(entry_id) = &card.entry_id
        && let Some(mut e) = hub.store.entry(entry_id)
    {
        e.data["status"] = "pending".into();
        e.data["parked"] = false.into();
        hub.set_entry(entry_id, &e.data);
        return hub.store.card_live(&card.id, None, Some(&who.run_id)).ok().flatten();
    }
    let chat = chat_for(hub, &who.person, &who.agent);
    let person_name = hub.store.person(&who.person).map(|p| p.name).filter(|n| !n.is_empty());
    let escape = card.tool == access::TOOL;
    let (title, kind) = (title_of(&card.tool, &card.shown), tool_kind(&card.tool));
    let (title, mut detail, mut options) = if escape {
        let name = hub.store.person(&card.person).map(|p| p.name).filter(|n| !n.is_empty());
        access::entry_view(card, &name.unwrap_or_else(|| card.person.clone()))
    } else {
        let (yes, no) = match &extra {
            Some((_, _, kept)) => {
                let name = person_name.clone().unwrap_or_else(|| who.person.clone());
                (edge::once_label(kept, &name, &who.agent), "No")
            }
            None => ("Allow once".into(), "Deny"),
        };
        let mut options = vec![
            json!({"optionId": "allow", "name": yes, "kind": "allow_once"}),
            json!({"optionId": "deny", "name": no, "kind": "reject_once"}),
        ];
        // A door starts only on its job, ticked: a yes to this one run would be refused at its start.
        if card.tool == edge::DOOR_TOOL {
            options.remove(0);
        }
        (title, crate::agent::acp::truncate(&card.shown, 6000), Value::Array(options))
    };
    let kept = extra.map(|(text, more, kept)| {
        if !text.is_empty() {
            detail = format!("{detail}\n\n{text}");
        }
        if let Some(all) = options.as_array_mut() {
            edge::insert(all, more);
        }
        kept
    });
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
    if let Some(kept) = kept {
        data["edge"] = kept;
    }
    if let Some(chain) = chain_of(card) {
        data["chain"] = chain.into();
    }
    if !who.person.is_empty() {
        data["person"] = who.person.as_str().into();
        data["personName"] = person_name.unwrap_or_else(|| who.person.clone()).into();
    }
    let lane = Lane::main(&chat.id);
    let turn = hub.store.max_turn(&chat.id);
    let entry = hub.add_entry(&lane, EntryKind::Permission, turn, &data)?;
    let live = hub.store.card_live(&card.id, Some(&entry.id), Some(&who.run_id)).ok().flatten()?;
    let heading = match data["chain"].as_str() {
        Some(chain) => format!("{chain} needs you"),
        None => format!("{} needs you", chat.name),
    };
    tell_approver(hub, &chat, &live, &data, heading, &title);
    Some(live)
}

/// Pushes a card to its approver's devices: the owner's for an owner card, else those of the person it names (a member, or
/// a bot guest's owner), whoever's chat it is in.
fn tell_approver(hub: &Hub, chat: &crate::store::BotConfig, card: &Card, data: &Value, heading: String, title: &str) {
    let mut target = chat.clone();
    target.person.clone_from(&card.answerable_by);
    let heading = if !card.person.is_empty() && card.answerable_by.as_deref() != Some(card.person.as_str()) {
        format!("{} needs your OK", data["personName"].as_str().unwrap_or_default())
    } else {
        heading
    };
    crate::remote::push::notify(
        hub,
        &target,
        Some(&chat.id),
        &heading,
        title,
        crate::remote::push::AlertKind::NeedsInput,
    );
}

/// Who may answer `card` changed from `before` (`crate::access::rederive`): its entry says so, which reaches the apps
/// with `canAnswer` for each viewer, and the new approver is told. A member who could answer it before is named in
/// `formerApprovers`, so the entry still reaches them, with `canAnswer` false, and leaves their Needs-you list.
pub fn approver_changed(hub: &Hub, card: &Card, before: Option<&str>) {
    let Some(entry_id) = &card.entry_id else { return };
    let Some(mut e) = hub.store.entry(entry_id) else { return };
    e.data["approver"] = card.approver.as_str().into();
    if let Some(p) = before.filter(|p| card.answerable_by.as_deref() != Some(*p)) {
        let mut former = e.data["formerApprovers"].as_array().cloned().unwrap_or_default();
        if !former.iter().any(|v| v == p) {
            former.push(p.into());
            e.data["formerApprovers"] = former.into();
        }
    }
    hub.set_entry(entry_id, &e.data);
    let chat = e.data["author"].as_str().unwrap_or(&e.bot_id).to_owned();
    let Some(chat) = chat_of(hub, &chat) else { return };
    let title = e.data["title"].as_str().unwrap_or_default().to_owned();
    tell_approver(hub, &chat, card, &e.data, format!("{} needs you", chat.name), &title);
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

/// An approver's answer to the card behind `entry_id` (`respondPermission`): from the owner (`person` None) or an
/// invited person. Who may answer was checked before this (roles.rs, `crate::access::may_answer`); the card store checks
/// it again as it records the answer (M2), and decides whether the answer counts.
pub fn answer(
    hub: &Arc<Hub>,
    entry_id: &str,
    option_id: Option<&str>,
    person: Option<&str>,
) -> anyhow::Result<Outcome> {
    let Some(entry) = hub.store.entry(entry_id) else { return Ok(Outcome::Closed) };
    let Some(card) = hub.store.card_by_entry(entry_id) else { return Ok(Outcome::Closed) };
    let allow = allows(&entry.data, option_id);
    let by = person.unwrap_or("owner");
    let answered =
        hub.store.card_answer(&card.id, allow, option_id.unwrap_or("cancel"), by, person, now_ms(), allow)?;
    if answered == Answered::Refused {
        return Err(crate::api::devices::NotAllowed(NOT_YOURS.into()).into());
    }
    let Answered::Applied { card, was_live } = answered else {
        // An expired one shows as expired now, whatever the sweep was doing.
        if let Some(c) = hub.store.card(&card.id).filter(|c| c.state == CardState::Expired) {
            close_entry(hub, &c, "expired");
        }
        return Ok(Outcome::Closed);
    };
    let status = if option_id.is_some() { "answered" } else { "cancelled" };
    // The entry shows the answer first, so what follows from it (an "always" that stays once) is written on top.
    if let Some(mut e) = hub.store.entry(entry_id) {
        e.data["status"] = status.into();
        e.data["selected"] = option_id.into();
        e.data["parked"] = false.into();
        hub.set_entry(entry_id, &e.data);
    }
    let kept = entry.data.get("edge").filter(|e| e.is_object() && card.approver == "owner").cloned();
    if let Some(kept) = kept {
        let (hub, option, person) = (hub.clone(), option_id.map(str::to_owned), person.map(str::to_owned));
        let door = card.tool == edge::DOOR_TOOL;
        let helper = (card.tool == edge::HELPER_TOOL || door) && card.source == "run";
        let card = card.clone();
        // What the answer keeps is written first; a helper's or door's run is told only then, since kith ask checks it
        // at launch and a door at its start.
        tokio::spawn(async move {
            let mut allow = allow;
            if let Err(why) = edge::apply(&card, &kept, option.as_deref(), allow).await {
                tracing::warn!(card = card.id, why, "kith-auth refused what an edge card keeps");
                if allow && door {
                    allow = false;
                    access::not_kept(&hub, &card, &why);
                } else if allow && matches!(option.as_deref(), Some(edge::ALWAYS | edge::JOB)) {
                    if helper {
                        let _ = edge::once_instead(&card).await;
                    }
                    access::kept_once(&hub, &card, &why);
                }
            }
            if helper && let Err(error) = settle(&hub, &card, was_live, allow, option.as_deref(), person.as_deref()) {
                tracing::warn!(error = format!("{error:#}"), card = card.id, "couldn't tell a helper card's answer");
            }
        });
        if helper {
            return Ok(Outcome::Recorded { forward: false });
        }
    }
    Ok(Outcome::Recorded { forward: settle(hub, &card, was_live, allow, option_id, person)? })
}

/// Tells whoever waits on an answered card, or owes its session the answer. True for a bot chat's card whose process
/// was up: the caller forwards the answer to the bot's actor.
fn settle(
    hub: &Arc<Hub>,
    card: &Card,
    was_live: bool,
    allow: bool,
    option_id: Option<&str>,
    person: Option<&str>,
) -> anyhow::Result<bool> {
    let mut forward = false;
    let said_no = if option_id.is_some() { "was denied" } else { "was cancelled" };
    if card.tool == access::TOOL {
        // No process waits on an escape card: the yes is applied at the session's next activation (see `access`).
        access::answered(hub, card, allow, option_id, person);
    } else if was_live && card.source == "run" {
        let verdict = if allow { Verdict::Allow } else { Verdict::Deny("The approver said no.".into()) };
        // A yes to a live asker was spent in the answer itself (no parallel ask can take it too); if nobody was
        // there to hear it, it goes back as the answer to a parked card.
        let heard = deliver(&card.id, &verdict);
        if !heard && allow && hub.store.card_undelivered(&card.id)? {
            follow_up(hub, card);
        } else if !heard && !allow {
            tell_asker(card, said_no);
        }
    } else if was_live {
        forward = true;
    } else if allow {
        follow_up(hub, card);
    } else {
        tell_asker(card, said_no);
    }
    Ok(forward)
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
    expire(hub, now_ms());
}

fn expire(hub: &Hub, at: i64) {
    match hub.store.cards_sweep(at) {
        Ok(expired) => {
            for card in &expired {
                if !deliver(&card.id, &Verdict::Deny("The request expired without an answer.".into())) {
                    tell_asker(card, "expired without an answer");
                }
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
            crate::access::rederive(&hub).await;
            // An owner token handed out over the API ends with its process or its time.
            let gone = tokio::task::spawn_blocking(|| crate::runauth::table().reap(crate::runauth::pid_alive))
                .await
                .unwrap_or_default();
            for run in gone {
                run_exited(&hub, &run);
            }
        }
    });
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        // kith-auth writes its file from outside the host (a revoke, a new grant): who answers follows within seconds.
        let mut seen = auth_stamp();
        let mut tick = tokio::time::interval(AUTH_POLL);
        loop {
            tick.tick().await;
            let now = auth_stamp();
            if now == seen {
                continue;
            }
            seen = now;
            let Some(hub) = weak.upgrade() else { return };
            crate::access::rederive(&hub).await;
        }
    });
}

/// How often kith-auth's file is looked at for a change.
const AUTH_POLL: Duration = Duration::from_secs(3);

/// When kith-auth's file last changed: `KITH_AUTH`, else every place `kernel/lib/kith_paths.py` may put it.
fn auth_stamp() -> Vec<Option<std::time::SystemTime>> {
    let home = dirs::home_dir().unwrap_or_default();
    let files = match std::env::var_os("KITH_AUTH").filter(|v| !v.is_empty()) {
        Some(given) => vec![std::path::PathBuf::from(given)],
        None => vec![
            crate::kernel::machine_dir(&home).join("secrets/auth.json"),
            home.join(".config/kith/auth.json"),
            home.join(".config/hq/auth.json"),
        ],
    };
    files.iter().map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok()).collect()
}

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod edge_tests;
#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod resume_tests;
