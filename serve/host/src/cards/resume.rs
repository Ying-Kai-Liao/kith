//! The resume half of approval cards (C3-C8, A7, L2): an approved card whose asker was gone owes its session a turn.
//!
//! The card store is the record of an owed resume (`Store::cards_awaiting_resume`: approved or used-by-a-later-ask,
//! granted, not resumed, before the deadline), and the kernel's wake queue (`kith-wake`, [`crate::wake`]) is the one
//! owner of the wake itself (lifecycle F12): a card answer is `enqueue`d as request `card:<id>` and the host keeps no
//! queue or retry list of its own. What the store keeps is only whether the request reached the queue
//! ([`Store::card_resume_claim`], a compare-and-set on `resumed_at`) and whether a turn carrying it has started
//! (`applied_at`, M5): a restart gives back every claim that wasn't applied (`Store::cards_restart`) and offers those
//! cards again, which is safe because the queue drops an id it already holds or has answered (rule 13).
//!
//! Doors, by where the card came from (its `session`):
//! - `acp:<chat>:<thread>`: a bot chat's turn. The host takes the item (`kith-wake take`, [`drive`]) and queues the turn
//!   on the bot's actor (`agent/bot/queue.rs`), after a short notice in the thread. The actor says when the turn
//!   started ([`turn_started`]) or could not ([`refused`], [`not_started`]).
//! - `run:..:<thread>#<agent>`: a channel thread. The note joins the channel inbox (`Core::wake`), and the run says
//!   the same ([`channel_started`], [`channel_failed`]).
//! - `run:..:<claude session>`: a `claude -p` run the kernel started (`kith ask`, `kith-sched`). The owner's own session
//!   is the kernel's run door (`kith-wake` resumes it once nobody holds it); a person's, and a card a `kith ask` run
//!   parked on (`reply_to`, resumed as `kith ask --reply <id> ...`: the turn's words are that ask's answer, rule 7), go
//!   through the host door: the host runs `kith <agent> --resume <session> "<note>"` with `KITH_PERSON` set, because
//!   only it can give a person's run its token.
//!
//! Stop wins (R8): [`stopped`] withdraws what the session is owed from the queue and closes the approved cards whose
//! turn had not started, so neither a restart nor another yes brings the stopped turn back.
//!
//! A card used by a later message before its resume still gets the wake, without a grant (C7, A9): the run asks again
//! if it still needs it.

use super::Card;
use crate::channels::Wake;
use crate::hub::Hub;
use crate::store::{CardState, EntryKind, Lane, now_ms};
use crate::wake::{self, Queued, Request, Turn};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long the kernel's one-shot waits for a held session before it exits 1 with "held by ..." (`KITH_CLAIM_WAIT`, #244,
/// default 5). The spawn gets it explicitly so the rule below doesn't depend on the kernel's default.
const CLAIM_WAIT_S: u64 = 5;
/// The (c) delivered rule: a spawn still alive this long after start has the session, because a held one would have
/// exited by `CLAIM_WAIT_S`. The two numbers are one rule: raise `CLAIM_WAIT_S` and this follows.
#[cfg(not(test))]
const EARLY_EXIT: Duration = Duration::from_secs(CLAIM_WAIT_S + 3);
/// A turn the host took and heard nothing of this long is given back (an actor that shut down, a lost callback).
const TAKEN_TTL: Duration = Duration::from_secs(30 * 60);
/// How soon the taker looks after a request arrives, so cards answered together share one turn.
const SETTLE: Duration = Duration::from_millis(300);
/// How often the taker looks for host-door turns on its own (a `kith ask` answer comes from outside the host).
const LOOK_EVERY: Duration = Duration::from_secs(5);

/// A bot's wake turn on its actor: the notice entry that carries it -> the turn it belongs to, until that turn starts
/// ([`turn_started`]) or doesn't ([`refused`], [`not_started`]). Only the link between the actor's callback and the
/// queue's run; the queue is the record.
static ENTRIES: LazyLock<Mutex<HashMap<String, Carried>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The turns this host holds -> when it took them, so one nobody settles is given back ([`TAKEN_TTL`]).
static TAKEN: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static KICK: tokio::sync::Notify = tokio::sync::Notify::const_new();

struct Carried {
    run: String,
    cards: Vec<String>,
}

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What one attempt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Queued, taken by someone else, dropped or expired: nothing more is owed from this card.
    Done,
    /// Still owed: tried again at the next look.
    Retry,
}

/// Where a card's turn goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Door {
    Bot { bot: String, lane: Lane },
    Channel(String),
    Run { session: String },
}

/// The door for a card, from where it came from.
pub fn door_of(card: &Card) -> Option<Door> {
    if let Some(rest) = card.session.strip_prefix("acp:") {
        let (chat, thread) = rest.split_once(':')?;
        let bot = card.bot_id.clone()?;
        let lane = Lane { chat: chat.to_owned(), thread: (!thread.is_empty()).then(|| thread.to_owned()) };
        return Some(Door::Bot { bot, lane });
    }
    let inner = if let Some(s) = card.session.strip_prefix("run:o:") {
        s
    } else {
        // `run:g:<len>:<person>:<session>` (runauth::session_of): the person's length says where the session starts.
        let tail = card.session.strip_prefix("run:g:")?;
        let (len, tail) = tail.split_once(':')?;
        let len: usize = len.parse().ok()?;
        tail.get(len + 1..).filter(|_| tail.get(..len) == Some(card.person.as_str()))?
    };
    if inner.is_empty() {
        return None;
    }
    Some(if inner.contains('#') { Door::Channel(inner.to_owned()) } else { Door::Run { session: inner.to_owned() } })
}

/// The wake queue's key for a card's session: the bot's `acp:<chat>:<thread>`, a channel thread's work key or the
/// Claude session. (`kith ask` answers for a person's session use the Claude session too, so they merge.)
fn key_of(door: &Door, card: &Card) -> String {
    match door {
        Door::Bot { .. } => card.session.clone(),
        Door::Channel(wk) => wk.clone(),
        Door::Run { session } => session.clone(),
    }
}

fn request_id(card: &Card) -> String {
    format!("card:{}", card.id)
}

fn state_of(hub: &Hub) -> PathBuf {
    hub.terminal.env().state
}

/// A resume is owed for `card`: put it in the wake queue now. A refusal leaves it owed for the next look ([`run_taker`]).
pub fn offer(hub: &Arc<Hub>, card: &Card) {
    let (hub, id) = (hub.clone(), card.id.clone());
    tokio::spawn(async move {
        attempt(&hub, &id).await;
    });
}

/// Claims the resume of every card in `group` (the owed cards of one session); returns the ones this caller got.
fn claim_all(hub: &Hub, group: &[Card]) -> Vec<Card> {
    let now = now_ms();
    group.iter().filter(|c| hub.store.card_resume_claim(&c.id, now).unwrap_or(false)).cloned().collect()
}

/// The turn that carries these cards' resumes started: they are delivered (M5).
fn applied(hub: &Hub, ids: &[String]) {
    let now = now_ms();
    for id in ids {
        let _ = hub.store.card_resume_applied(id, now);
    }
}

fn end_all(hub: &Hub, cards: &[Card]) {
    for c in cards {
        end_owed(hub, c);
    }
}

fn end_owed(hub: &Hub, card: &Card) {
    let _ = hub.store.card_resume_drop(&card.id);
}

/// Whether a card's wake is still wanted: approved (or used by a later ask) and no turn carrying it started.
/// Withdrawn, expired and Stopped cards are not.
fn wanted(c: &Card) -> bool {
    matches!(c.state, CardState::Approved | CardState::Used) && c.granted_at.is_some() && c.applied_at.is_none()
}

/// One try at the resume of card `id`: it goes into the kernel's queue, once. Safe to call from anywhere, any number of
/// times: the claim decides, and the queue drops an id it has.
pub async fn attempt(hub: &Arc<Hub>, id: &str) -> Step {
    let now = now_ms();
    let Some(card) = hub.store.cards_awaiting_resume(now).into_iter().find(|c| c.id == id) else {
        return Step::Done;
    };
    // A7: a person who lost the agent since the yes gets no resume, and their cards end.
    if !card.person.is_empty() {
        match crate::api::roles::may_use_checked(&card.person, &card.agent).await {
            Some(false) => {
                end_owed(hub, &card);
                super::withdraw_person(hub, &card.person, Some(&card.agent));
                return Step::Done;
            }
            None => return Step::Retry, // kith-auth can't be asked right now
            Some(true) => {}
        }
    }
    // Requests for one session merge: one turn answers every card owed there (L5).
    let mut group = hub.store.cards_owed_in(&card.session, now);
    if !group.iter().any(|c| c.id == card.id) {
        group.push(card.clone());
    }
    let Some(door) = door_of(&card) else {
        end_owed(hub, &card);
        return Step::Done;
    };
    match &door {
        Door::Bot { bot, .. } => {
            if hub.store.bot(bot).ok().flatten().is_none_or(|b| b.deleted) {
                end_all(hub, &group);
                return Step::Done;
            }
        }
        Door::Channel(_) => {}
        Door::Run { session } => {
            if !crate::terminal::claim::safe_session(session) {
                end_all(hub, &group);
                return Step::Done;
            }
            // A live writer on the session keeps the resume owed, and the card shows who holds it (L2).
            let (state, sid) = (state_of(hub), session.clone());
            let held = wake::blocking(move || crate::terminal::claim::holder(&state, &sid)).await.flatten();
            if let Some(h) = held {
                set_held(hub, &card, Some((session, &h)));
                return Step::Retry;
            }
        }
    }
    let claimed = claim_all(hub, &group);
    let mut step = Step::Done;
    for c in &claimed {
        if let Err(error) = enqueue(hub, c, &door).await {
            tracing::warn!(card = %c.id, error = format!("{error:#}"), "couldn't queue a card's resume");
            let _ = hub.store.card_resume_release(&c.id);
            step = Step::Retry;
        }
    }
    step
}

/// One card's request into the queue. The owner's plain session wakes through the kernel's run door (it resumes the
/// session itself once nobody holds it); everything the host must start (a person's run, a `kith ask` reply, a bot's
/// chat, a channel thread) is a host-door item.
async fn enqueue(hub: &Arc<Hub>, card: &Card, door: &Door) -> anyhow::Result<()> {
    let kernel_door = matches!(door, Door::Run { .. }) && card.person.is_empty() && card.reply_to.is_none();
    let text = note_of(hub, card).await;
    let (state, key, id) = (state_of(hub), key_of(door, card), request_id(card));
    let (agent, person) = (card.agent.clone(), Some(card.person.clone()).filter(|p| !p.is_empty()));
    let room = person.as_deref().map(|p| crate::agent::hq::room_of(&card.agent, Some(p), None));
    let queued = {
        let (state, text, key, id) = (state.clone(), text.clone(), key.clone(), id.clone());
        wake::blocking(move || {
            wake::enqueue(
                &state,
                &Request {
                    session: &key,
                    id: &id,
                    agent: &agent,
                    person: person.as_deref(),
                    room: room.as_deref(),
                    host_door: !kernel_door,
                    why: "",
                    text: &text,
                },
            )
        })
        .await
    };
    let queued = queued.ok_or_else(|| anyhow::anyhow!("the wake call didn't finish"))??;
    #[cfg(test)]
    if kernel_door {
        tests_seen::record("run", &card.session, &text, None);
    }
    if queued == Queued::Duplicate {
        // The queue has this id: still queued or in a turn (it is the queue's now), or finished or withdrawn (over).
        let state = state.clone();
        let how = wake::blocking(move || wake::status(&state, &id)).await.unwrap_or_default();
        if !matches!(how.as_str(), "queued" | "running") {
            applied(hub, std::slice::from_ref(&card.id));
        }
    } else if kernel_door {
        // From here the kernel's queue delivers it exactly once, with no turn of the host's to wait for.
        applied(hub, std::slice::from_ref(&card.id));
        clear_held(hub, std::slice::from_ref(card));
    }
    if !kernel_door {
        KICK.notify_one();
    }
    Ok(())
}

/// One card's words: who allowed what, and that the agent can go on. A used grant is said so (C7).
async fn note_of(hub: &Hub, card: &Card) -> String {
    let who = super::approver_label(hub, card).await;
    if card.tool == super::secret::TOOL {
        // A saved key is told by name only (never a value); its note stands on its own.
        return super::secret::note(hub, &who, card);
    }
    let what = super::short(&card.shown, 80);
    if card.state == CardState::Used {
        format!(
            "{who} allowed {what} once, but a later request already used that; ask again if you still need it; go on."
        )
    } else {
        format!("{who} allowed {what} once; go on.")
    }
}

// ---- the host takes its doors' turns

/// Starts the host-door turns that are owed and due, one after another. Called by the taker's loop and by tests.
pub async fn drive(hub: &Arc<Hub>) {
    let state = state_of(hub);
    reap(hub);
    loop {
        let st = state.clone();
        let taken = match wake::blocking(move || wake::take(&st)).await {
            Some(Ok(Some(turn))) => turn,
            Some(Ok(None)) | None => break,
            Some(Err(error)) => {
                tracing::warn!(error = format!("{error:#}"), "couldn't take a wake");
                break;
            }
        };
        handle(hub, taken).await;
    }
}

/// Turns nobody settled for [`TAKEN_TTL`] go back to the queue.
fn reap(hub: &Hub) {
    let stale: Vec<String> =
        lock(&TAKEN).iter().filter(|(_, at)| at.elapsed() > TAKEN_TTL).map(|(run, _)| run.clone()).collect();
    for run in stale {
        lock(&TAKEN).remove(&run);
        lock(&ENTRIES).retain(|_, c| c.run != run);
        let st = state_of(hub);
        wake::spawn(move || wake::back(&st, &run, true));
    }
}

/// The host's taker: looks for owed host-door turns soon after a request arrives and every few seconds otherwise, and
/// offers again the cards the queue has not got (a kith-wake that failed, a session held by a terminal).
pub fn run_taker(hub: &Arc<Hub>) {
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = KICK.notified() => tokio::time::sleep(SETTLE).await,
                () = tokio::time::sleep(LOOK_EVERY) => {}
            }
            let Some(hub) = weak.upgrade() else { return };
            if super::settings(&hub.store).resume {
                for card in hub.store.cards_awaiting_resume(now_ms()) {
                    attempt(&hub, &card.id).await;
                }
            }
            drive(&hub).await;
        }
    });
}

/// One taken turn goes through its door.
async fn handle(hub: &Arc<Hub>, turn: Turn) {
    lock(&TAKEN).insert(turn.run.clone(), Instant::now());
    let cards: Vec<Card> =
        turn.ids.iter().filter_map(|i| i.strip_prefix("card:")).filter_map(|id| hub.store.card(id)).collect();
    let live: Vec<Card> = cards.into_iter().filter(wanted).collect();
    let others = turn.ids.iter().filter(|i| !i.starts_with("card:")).count();
    // Every card was withdrawn, expired, Stopped or already carried by a turn that started: nothing to run.
    if live.is_empty() && others == 0 {
        return give_up(hub, &turn.run);
    }
    let person = turn.person.clone().filter(|p| !p.is_empty());
    // A7 again, at the door: a person's access can end between the yes and the turn.
    if let Some(person) = &person {
        match crate::api::roles::may_use_checked(person, &turn.agent).await {
            Some(false) => {
                super::withdraw_person(hub, person, Some(&turn.agent));
                return give_up(hub, &turn.run);
            }
            None => return unstarted(hub, &turn.run, &card_ids(&live), true, true),
            Some(true) => {}
        }
    }
    // What a card already carried is not said again; the rest of the turn's words stand.
    let note =
        if live.len() + others == turn.ids.len() { turn.note.clone() } else { rebuilt(hub, &turn.run, &live).await };
    for c in &live {
        let _ = hub.store.card_resume_claim(&c.id, now_ms()); // a restart gave the claim back: it is held again
    }
    let ids = card_ids(&live);
    let session = turn.session.as_str();
    if session.starts_with("acp:") {
        match live.first().and_then(door_of) {
            Some(Door::Bot { bot, lane }) => bot_door(hub, &turn.run, &live, &bot, &lane, &note),
            _ => give_up(hub, &turn.run),
        }
    } else if session.contains('#') {
        channel_door(hub, &turn.run, &live, &ids, session, &note);
    } else {
        run_door(hub, &turn, &live, person.as_deref(), &note).await;
    }
}

fn card_ids(cards: &[Card]) -> Vec<String> {
    cards.iter().map(|c| c.id.clone()).collect()
}

/// The turn's words from the cards still wanted, in the shape kith-wake writes them.
async fn rebuilt(hub: &Hub, run: &str, live: &[Card]) -> String {
    let mut texts = Vec::new();
    for c in live {
        texts.push(note_of(hub, c).await);
    }
    let head = format!("[kith wake {run}]");
    if let [one] = texts.as_slice() {
        return format!("{head} {one}");
    }
    let mut note = format!("{head} {} things happened while this session was idle:", texts.len());
    for (n, t) in texts.iter().enumerate() {
        let _ = write!(note, "\n{}. {t}", n + 1);
    }
    note
}

/// Nothing of this turn will run (every request is withdrawn, or the thread is gone): the queue is told, ids kept.
fn give_up(hub: &Hub, run: &str) {
    lock(&TAKEN).remove(run);
    let (state, run) = (state_of(hub), run.to_owned());
    wake::spawn(move || wake::done(&state, &run, "withdrawn"));
}

/// The turn `run` started: what it carried is delivered (M5).
fn started(hub: &Hub, run: &str, cards: &[String]) {
    lock(&TAKEN).remove(run);
    applied(hub, cards);
    let (state, run) = (state_of(hub), run.to_owned());
    wake::spawn(move || wake::done(&state, &run, "answered"));
}

/// The turn `run` did not start. `again`: it is owed again (`held`: a writer had the session, which is not a failed
/// try); otherwise (a Stop) it is dropped, ids kept, and its cards end: Stop wins over a yes already given (R8).
fn unstarted(hub: &Hub, run: &str, cards: &[String], again: bool, held: bool) {
    lock(&TAKEN).remove(run);
    let still = cards.is_empty() || cards.iter().any(|id| hub.store.card(id).is_some_and(|c| wanted(&c)));
    let (state, run_id) = (state_of(hub), run.to_owned());
    if again && still {
        wake::spawn(move || wake::back(&state, &run_id, held));
        return;
    }
    if !again {
        // A saved key's card keeps its record (secret cards close only through their own steps): only its wake goes.
        for card in cards_of(hub, cards).iter().filter(|c| c.tool != super::secret::TOOL) {
            let _ = hub.store.card_resume_close(&card.id);
        }
    }
    wake::spawn(move || wake::done(&state, &run_id, "withdrawn"));
}

// ---- door (a): a bot's chat

fn bot_door(hub: &Arc<Hub>, run: &str, group: &[Card], bot: &str, lane: &Lane, note: &str) {
    let alive = hub.store.bot(bot).ok().flatten().is_some_and(|b| !b.deleted);
    if !alive {
        end_all(hub, group);
        return give_up(hub, run);
    }
    let turn = hub.store.max_turn(&lane.chat);
    let shown = note.split_once("] ").map_or(note, |(_, rest)| rest);
    let Some(entry) = hub.add_entry(lane, EntryKind::Notice, turn, &json!({"text": shown, "style": "info"})) else {
        return unstarted(hub, run, &card_ids(group), true, false);
    };
    lock(&ENTRIES).insert(entry.id.clone(), Carried { run: run.to_owned(), cards: card_ids(group) });
    #[cfg(test)]
    tests_seen::record("bot", group.first().map_or(run, |c| c.session.as_str()), note, Some(&entry.id));
    #[cfg(not(test))]
    {
        let cmd = crate::agent::bot::Cmd::Send {
            lane: lane.clone(),
            entry_id: entry.id.clone(),
            text: note.to_owned(),
            route: false,
        };
        if let Err(error) = hub.send_cmd(bot, cmd) {
            tracing::warn!(error = format!("{error:#}"), "couldn't queue a card's resume");
            lock(&ENTRIES).remove(&entry.id);
            unstarted(hub, run, &card_ids(group), true, false);
        }
    }
}

/// The bot's turn for these entries started (its prompt went to the agent): a wake among them is delivered. Called
/// from the actor once the prompt is sent.
pub fn turn_started(hub: &Hub, entry_ids: &[String]) {
    for entry in entry_ids {
        let Some(c) = lock(&ENTRIES).remove(entry) else { continue };
        clear_held(hub, &cards_of(hub, &c.cards));
        started(hub, &c.run, &c.cards);
    }
}

fn cards_of(hub: &Hub, ids: &[String]) -> Vec<Card> {
    ids.iter().filter_map(|id| hub.store.card(id)).collect()
}

/// A wake turn on a bot's actor could not start because the session's writer is held: it is owed again, with the
/// holder shown on the card. Called from the actor's `start_failed`.
pub fn refused(hub: &Arc<Hub>, entry_ids: &[String], session: Option<&str>) {
    let holder =
        session.and_then(|sid| crate::terminal::claim::holder(&hub.terminal.env().state, sid).map(|c| (sid, c)));
    for entry in entry_ids {
        let Some(c) = lock(&ENTRIES).remove(entry) else { continue };
        for card in cards_of(hub, &c.cards) {
            set_held(hub, &card, holder.as_ref().map(|(sid, c)| (*sid, c)));
        }
        unstarted(hub, &c.run, &c.cards, true, true);
    }
}

/// A wake turn on a bot's actor failed to start for another reason. `again`: it is owed again (the queue pauses
/// before it comes back); after a Stop it is dropped, and the stopped yes does not come back.
pub fn not_started(hub: &Arc<Hub>, entry_ids: &[String], again: bool) {
    for entry in entry_ids {
        let Some(c) = lock(&ENTRIES).remove(entry) else { continue };
        unstarted(hub, &c.run, &c.cards, again, false);
    }
}

// ---- door (b): a channel thread

fn channel_door(hub: &Arc<Hub>, run: &str, group: &[Card], card_ids: &[String], wk: &str, note: &str) {
    // The channel carries the cards and the turn's run in one list: its run settles both ([`channel_started`]).
    let mut carried = card_ids.to_vec();
    carried.push(format!("run:{run}"));
    match crate::channels::wake(wk, note, || carried) {
        Wake::Queued => clear_held(hub, group),
        Wake::Claimed => {}
        Wake::NoThread => {
            end_all(hub, group);
            give_up(hub, run);
        }
        // The connection isn't up (yet): the queue keeps it and asks again after its pause.
        Wake::Offline => unstarted(hub, run, card_ids, true, true),
    }
}

/// Splits what a channel carries into the cards and the turn's run.
fn split_run(ids: &[String]) -> (Vec<String>, Option<String>) {
    let run = ids.iter().find_map(|i| i.strip_prefix("run:")).map(str::to_owned);
    (ids.iter().filter(|i| !i.starts_with("run:")).cloned().collect(), run)
}

/// The channel run that carries these wakes started (its process is up).
pub fn channel_started(hub: &Hub, ids: &[String]) {
    let (cards, run) = split_run(ids);
    match run {
        Some(run) => started(hub, &run, &cards),
        None => applied(hub, &cards),
    }
}

/// A channel turn carrying a wake never ran (the session was held, the spawn failed, Stop). `again`: it is owed again
/// (with the queue's pause); after a Stop it is dropped.
pub fn channel_failed(hub: &Arc<Hub>, ids: &[String], again: bool) {
    let (cards, run) = split_run(ids);
    if let Some(run) = run {
        unstarted(hub, &run, &cards, again, false);
    }
}

// ---- door (c): a -p run the host starts (a person's session, a `kith ask` reply)

/// What the host starts for a resumed -p run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawn {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// `kith <agent> --resume <session> "<note>"` as the card's person, with the env the host gives its own runs: a person's
/// run gets `KITH_PERSON` (the owner's none) so the session doesn't resume under the owner's profile. The run token is
/// added by the caller. A card with `reply_to` goes through `kith ask --reply`, which hands the turn's answer to the asker.
#[cfg(test)]
pub fn spawn_for(card: &Card, session: &str, note: &str) -> Spawn {
    spawn_with(&card.agent, &card.person, card.reply_to.as_deref(), session, note)
}

fn spawn_with(agent: &str, person: &str, reply_to: Option<&str>, session: &str, note: &str) -> Spawn {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| crate::channels::run::inherited(k)).collect();
    let mut set = |name: &str, v: &str| env.extend(crate::kernel::both(name, v));
    set("VIA", "host");
    set("CLAIM_WAIT", &CLAIM_WAIT_S.to_string());
    if person.is_empty() {
        // The owner: no person, no room set here.
    } else {
        set("PERSON", person);
        set("ROOM", &crate::agent::hq::room_of(agent, Some(person), None));
    }
    let resume = [agent, "--resume", session];
    let args = match reply_to {
        Some(id) => ["ask", "--reply", id].into_iter().chain(resume).chain(["--", note]).map(str::to_owned).collect(),
        None => resume.into_iter().chain([note]).map(str::to_owned).collect(),
    };
    Spawn { args, env }
}

async fn run_door(hub: &Arc<Hub>, turn: &Turn, live: &[Card], person: Option<&str>, note: &str) {
    let session = turn.session.as_str();
    let ids = card_ids(live);
    if !crate::terminal::claim::safe_session(session) {
        end_all(hub, live);
        return give_up(hub, &turn.run);
    }
    // A live writer on the session keeps the resume owed (the kernel's one-shot would wait and then refuse).
    let state = hub.terminal.env().state;
    let sid = session.to_owned();
    let held = {
        let (state, sid) = (state.clone(), sid.clone());
        wake::blocking(move || crate::terminal::claim::holder(&state, &sid)).await.flatten()
    };
    if let Some(h) = held {
        for c in live {
            set_held(hub, c, Some((session, &h)));
        }
        return unstarted(hub, &turn.run, &ids, true, true);
    }
    #[cfg(not(test))]
    let Some(bin) = crate::agent::hq::bin() else { return unstarted(hub, &turn.run, &ids, true, true) };
    let reply = live.iter().find(|c| c.reply_to.is_some());
    let mut spawn =
        spawn_with(&turn.agent, person.unwrap_or_default(), reply.and_then(|c| c.reply_to.as_deref()), session, note);
    // L3, step 4: a file or network card's once grant would be consumed HERE, after the claim and before exec, and
    // the profile built from the consume result. A tool-prompt card consumes it when the resumed run asks (card_open),
    // so no profile carries the grant and the stale-profile race can't happen.
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        spawn.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let asked = reply.and_then(|c| crate::runauth::AskReply::parse(c.reply_to.as_deref()?, c.asked_by.as_deref()));
    let grant = crate::runauth::grant_for_env(&mut env, &turn.agent, session, asked);
    spawn.env = env.iter().filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v.to_str()?.to_owned()))).collect();
    #[cfg(test)]
    {
        let _ = grant;
        tests_seen::record("run", live.first().map_or(session, |c| c.session.as_str()), note, None);
        tests_seen::spawned(spawn);
        clear_held(hub, live);
        started(hub, &turn.run, &ids);
    }
    #[cfg(not(test))]
    {
        match exec(&bin, &spawn, grant).await {
            Exec::Running => {
                clear_held(hub, live);
                started(hub, &turn.run, &ids);
            }
            Exec::Held => {
                let h = crate::terminal::claim::holder(&state, &sid);
                for c in live {
                    set_held(hub, c, h.as_ref().map(|h| (session, h)));
                }
                unstarted(hub, &turn.run, &ids, true, true);
            }
            Exec::Failed(why) => {
                tracing::warn!(run = %turn.run, why, "couldn't resume a session for an approved card");
                unstarted(hub, &turn.run, &ids, true, false);
            }
        }
    }
}

/// A host that stopped and started again knows nothing of the turns it had taken (the queue recovers them).
#[cfg(test)]
pub(crate) fn forget_in_flight() {
    lock(&ENTRIES).clear();
    lock(&TAKEN).clear();
}

// ---- Stop

/// Stop wins over a yes already given (R8): what the stopped places are owed is withdrawn from the queue (their ids
/// kept, so a late copy doesn't come back), and the approved cards whose turn had not started end, so neither a restart
/// nor another yes in the session brings the stopped turn back. `keys` are the queue sessions the caller knows it
/// stops; the cards of `matches` add theirs. A turn the queue already gave to a door is settled by that door's own
/// Stop ([`not_started`], [`channel_failed`]).
pub fn stopped(hub: &Hub, keys: &[String], matches: impl Fn(&Door) -> bool) {
    let mut keys: BTreeSet<String> = keys.iter().cloned().collect();
    for card in hub.store.cards_unapplied() {
        let Some(door) = door_of(&card).filter(&matches) else { continue };
        keys.insert(key_of(&door, &card));
        // A saved key's card keeps its record: only its wake goes (its id is withdrawn below).
        if card.tool != super::secret::TOOL {
            let _ = hub.store.card_resume_close(&card.id);
        }
    }
    let state = state_of(hub);
    wake::spawn(move || {
        for k in &keys {
            wake::withdraw(&state, k);
        }
    });
}

/// Stop in one place of a bot (its chat or one thread).
pub fn stopped_lane(hub: &Hub, lane: &Lane) {
    let key = format!("acp:{}:{}", lane.chat, lane.thread.as_deref().unwrap_or_default());
    stopped(hub, &[key], |d| matches!(d, Door::Bot { lane: l, .. } if l == lane));
}

/// Stop everywhere in a chat (a group stops as a whole).
pub fn stopped_chat(hub: &Hub, chat: &str) {
    stopped(hub, &[], |d| matches!(d, Door::Bot { lane, .. } if lane.chat == chat));
}

/// Stop everywhere a bot talks.
pub fn stopped_bot(hub: &Hub, bot_id: &str) {
    stopped(hub, &[], |d| matches!(d, Door::Bot { bot, .. } if bot == bot_id));
}

/// Stop in a channel thread (its work key).
pub fn stopped_thread(hub: &Hub, wk: &str) {
    stopped(hub, &[wk.to_owned()], |d| matches!(d, Door::Channel(w) if w == wk));
}

#[cfg(not(test))]
enum Exec {
    Running,
    Held,
    Failed(String),
}

#[cfg(not(test))]
async fn exec(bin: &std::path::Path, spawn: &Spawn, grant: Option<crate::runauth::RunGrant>) -> Exec {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt as _;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&spawn.args)
        .env_clear()
        .envs(spawn.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => return Exec::Failed(format!("{error}")),
    };
    if let (Some(g), Some(pid)) = (&grant, child.id()) {
        g.bind_pid(pid);
    }
    // The tail of stderr, kept while the run goes on so a full pipe never stalls it.
    let tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut t = tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                let excess = t.len().saturating_sub(4096);
                if excess > 0 {
                    let cut = (excess..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(t.len());
                    t.drain(..cut);
                }
            }
        });
    }
    match tokio::time::timeout(EARLY_EXIT, child.wait()).await {
        Err(_) => {
            // Still running: it has the session. The token lives as long as the process.
            tokio::spawn(async move {
                let _ = child.wait().await;
                drop(grant);
            });
            Exec::Running
        }
        Ok(Ok(status)) if status.success() => Exec::Running,
        Ok(done) => {
            let said = tail.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if said.contains("held by") { Exec::Held } else { Exec::Failed(format!("{done:?}: {}", said.trim())) }
        }
    }
}

// ---- what the apps see

/// "Held by an old run": the writer claim a waiting resume is stuck behind, on the card's entry, for the apps to show
/// next to a Stop (`stopHolder`).
fn set_held(hub: &Hub, card: &Card, holder: Option<(&str, &crate::terminal::claim::Claim)>) {
    let Some(id) = &card.entry_id else { return };
    let Some(mut e) = hub.store.entry(id) else { return };
    e.data["held"] =
        holder.map_or(Value::Null, |(sid, c)| json!({"session": sid, "kind": c.kind, "pid": c.pid, "since": c.at}));
    hub.set_entry(id, &e.data);
}

fn clear_held(hub: &Hub, cards: &[Card]) {
    for c in cards {
        let Some(id) = &c.entry_id else { continue };
        if let Some(mut e) = hub.store.entry(id)
            && !e.data["held"].is_null()
        {
            e.data["held"] = Value::Null;
            hub.set_entry(id, &e.data);
        }
    }
}

/// What the doors would have done, for tests (no process, no actor).
#[cfg(test)]
pub(crate) mod tests_seen {
    use super::Spawn;
    use std::sync::Mutex;

    #[derive(Debug, Clone)]
    pub struct Seen {
        pub door: &'static str,
        pub session: String,
        pub note: String,
        /// The notice entry a bot door queued the turn under.
        pub entry: Option<String>,
    }

    static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
    static SPAWNS: Mutex<Vec<(Spawn, String)>> = Mutex::new(Vec::new());
    static ASKED: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

    /// A `kith ask --answer` the host would have run (`cards::tell_asker`).
    pub fn asked(args: Vec<String>) {
        ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(args);
    }

    /// The answers sent to the `kith ask` with this id.
    pub fn answers_to(id: &str) -> Vec<Vec<String>> {
        ASKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|a| a.get(2).map(String::as_str) == Some(id))
            .cloned()
            .collect()
    }

    pub fn record(door: &'static str, session: &str, note: &str, entry: Option<&str>) {
        SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Seen {
            door,
            session: session.to_owned(),
            note: note.to_owned(),
            entry: entry.map(str::to_owned),
        });
    }

    pub fn spawned(s: Spawn) {
        let note = s.args.last().cloned().unwrap_or_default();
        SPAWNS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((s, note));
    }

    /// Everything seen for a session.
    pub fn of(session: &str) -> Vec<Seen> {
        SEEN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|s| s.session == session)
            .cloned()
            .collect()
    }

    pub fn spawns_for(session_id: &str) -> Vec<Spawn> {
        SPAWNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(s, _)| {
                let at = s.args.iter().position(|a| a == "--resume");
                at.and_then(|i| s.args.get(i + 1)).map(String::as_str) == Some(session_id)
            })
            .map(|(s, _)| s.clone())
            .collect()
    }
}
