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
use crate::store::{CardState, Entry, EntryKind, Lane, now_ms};
use crate::wake::{self, Queued, Request, Turn};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
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
/// Even a turn the bot's actor or a channel run still seems to hold is given back after this long: its callback was lost.
const TRACKED_TTL: Duration = Duration::from_secs(6 * 3600);
/// How soon the taker looks after a request arrives, so cards answered together share one turn.
const SETTLE: Duration = Duration::from_millis(300);
/// How often the taker looks at the queue's folder (a `kith ask` answer comes from outside the host); it starts a
/// process only when the folder holds something.
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// Every this many looks, the cards the queue hasn't got (a kith-wake that failed) are offered again.
const OFFER_EVERY: u32 = 12;

/// A bot's wake turn on its actor: the notice entry that carries it -> the turn it belongs to, until that turn starts
/// ([`turn_started`]) or doesn't ([`refused`], [`not_started`]). Only the link between the actor's callback and the
/// queue's run; the queue is the record.
static ENTRIES: LazyLock<Mutex<HashMap<String, Carried>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The turns this host holds -> when it took them, so one nobody settles is given back ([`TAKEN_TTL`]).
static TAKEN: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The runs a channel thread holds, until its turn starts or doesn't: a turn queued behind a long channel run is not lost.
static CHANNEL_RUNS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
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

/// The cards whose request the queue holds right now (queued or in a turn), for a restart to keep their claims.
pub(crate) fn queued_cards(hub: &Hub) -> Vec<String> {
    wake::statuses(&state_of(hub))
        .into_iter()
        .filter(|(_, how)| matches!(how.as_str(), "queued" | "running"))
        .filter_map(|(id, _)| id.strip_prefix("card:").map(str::to_owned))
        .collect()
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

/// Cards a taken turn had re-claimed (`handle`) end with it: `end_owed` only acts on a card nobody holds.
fn end_claimed(hub: &Hub, cards: &[Card]) {
    for c in cards {
        end_for_stop(hub, c);
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
        Door::Bot { bot, lane } => {
            if hub.store.bot(bot).ok().flatten().is_none_or(|b| b.deleted) || left_room(hub, bot, lane) {
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
        }
    }
    // Every card of the group is marked before any is claimed: one claimed but not yet queued is not "forgotten".
    let state = state_of(hub);
    let _marks: Vec<Enqueuing> = group.iter().map(|c| Enqueuing::mark(&state, &c.id)).collect();
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
pub(crate) async fn enqueue(hub: &Arc<Hub>, card: &Card, door: &Door) -> anyhow::Result<()> {
    let _marker = Enqueuing::mark(&state_of(hub), &card.id);
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
    if kernel_door && queued == Queued::New {
        tests_seen::record("run", &card.session, &text, None);
    }
    if queued == Queued::Duplicate {
        // The queue has this id. Still queued or in a turn: it is the queue's. Finished or withdrawn: over. The call
        // failing says nothing, so the card stays owed for the next look (a flaky status must not lose a wake).
        let (st, rid) = (state.clone(), id.clone());
        match wake::blocking(move || wake::status(&st, &rid)).await.flatten().as_deref() {
            Some("answered" | "recovered" | "withdrawn") => applied(hub, std::slice::from_ref(&card.id)),
            // It failed after its tries: not delivered, so not applied, and not owed any more either.
            Some("failed") => failed(hub, card),
            Some("queued" | "running") => {}
            other => anyhow::bail!("the queue's answer about {id} was {other:?}"),
        }
    }
    // Stop may have come while this was on its way: the request is withdrawn again (the card is closed, R8).
    if hub.store.card(&card.id).is_none_or(|c| !wanted(&c)) {
        let (st, key, rid) = (state, key, id);
        wake::spawn(move || wake::withdraw(&st, &key, Some(&rid)));
    } else if kernel_door {
        show_holder(hub, card).await;
    } else {
        KICK.notify_one();
    }
    Ok(())
}

/// Requests (cards, waiting messages) on their way into the queue, each with how many callers mark it.
/// [`settle_queue_at`] must not read "not in the queue" as "forgotten" for these: the item is written a moment after
/// the claim. `ENQUEUED` counts finished enqueues per state folder (a host's queue), so a settle whose read overlapped
/// one can tell, and a second host in the same process (a test) never makes it unsure.
static ENQUEUING: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Mutex::default);
static ENQUEUED: LazyLock<Mutex<HashMap<PathBuf, usize>>> = LazyLock::new(Mutex::default);
/// Requests the queue's files did not show at the last settle (neither queued nor answered). A commit writes the answer
/// before it removes the item, but a read of the files is not atomic across them, so one miss is not yet an answer.
static MISSED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

struct Enqueuing {
    id: String,
    state: PathBuf,
}

impl Enqueuing {
    fn mark(state: &Path, id: &str) -> Self {
        *lock(&ENQUEUING).entry(id.to_owned()).or_default() += 1;
        Self { id: id.to_owned(), state: state.to_owned() }
    }
}

impl Drop for Enqueuing {
    fn drop(&mut self) {
        let mut marks = lock(&ENQUEUING);
        if let Some(n) = marks.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                marks.remove(&self.id);
            }
        }
        *lock(&ENQUEUED).entry(self.state.clone()).or_default() += 1;
    }
}

fn enqueued(state: &Path) -> usize {
    lock(&ENQUEUED).get(state).copied().unwrap_or_default()
}

/// The card of a kernel run-door wake says who holds its session while the wake waits for the writer (L2).
async fn show_holder(hub: &Arc<Hub>, card: &Card) {
    let Some(Door::Run { session }) = door_of(card) else { return };
    let (state, sid) = (state_of(hub), session.clone());
    let held = wake::blocking(move || crate::terminal::claim::holder(&state, &sid)).await.flatten();
    match held {
        Some(h) => set_held(hub, card, Some((&session, &h))),
        None => clear_held(hub, std::slice::from_ref(card)),
    }
}

/// The queue gave up on this card's wake (it failed after its tries) or no longer knows it: it is not delivered, so the
/// card ends instead of staying approved past its deadline ("silence is no").
fn failed(hub: &Hub, card: &Card) {
    tracing::warn!(card = %card.id, "a card's wake is not delivered; the card ends");
    end_for_stop(hub, card);
}

/// Settles the cards the queue holds: delivered ones are applied; a failed one, or one the queue has forgotten after its
/// deadline, ends (it would stay approved forever otherwise); a kernel run-door one shows who holds its session. Reads
/// the queue's files, so it starts no process.
pub(crate) async fn settle_queue(hub: &Arc<Hub>) {
    settle_queue_at(hub, now_ms()).await;
}

pub(crate) async fn settle_queue_at(hub: &Arc<Hub>, now: i64) {
    let cards: Vec<Card> = hub.store.cards_unapplied().into_iter().filter(|c| c.resumed_at.is_some()).collect();
    let waiting = hub.store.waiting_for_terminal().unwrap_or_default();
    if cards.is_empty() && waiting.is_empty() {
        lock(&MISSED).clear();
        return;
    }
    let state = state_of(hub);
    let (before, count) = (lock(&ENQUEUING).keys().cloned().collect::<HashSet<_>>(), enqueued(&state));
    let known = {
        let state = state.clone();
        wake::blocking(move || wake::statuses(&state)).await.unwrap_or_default()
    };
    // An enqueue that began, ran or ended during the read may have written its item after the read saw the folder.
    let racing = count != enqueued(&state);
    for card in &cards {
        let how = known.get(&request_id(card)).map(String::as_str);
        let unsure = racing || before.contains(&card.id) || lock(&ENQUEUING).contains_key(&card.id);
        if how.is_some() || unsure {
            lock(&MISSED).remove(&card.id);
        }
        // Failed, or forgotten by the queue (its memory is a week) once the card's deadline has passed: nothing can come.
        // "Forgotten" must be seen on two passes in a row: a commit in progress is in neither file for a moment.
        let gone = how.is_none() && !unsure && now > card.deadline && !lock(&MISSED).insert(card.id.clone());
        if how == Some("failed") || gone {
            lock(&MISSED).remove(&card.id);
            failed(hub, card);
            continue;
        }
        match how {
            Some("answered" | "recovered" | "withdrawn") => {
                applied(hub, std::slice::from_ref(&card.id));
                clear_held(hub, std::slice::from_ref(card));
            }
            Some(_) if kernel_door(card) => show_holder(hub, card).await,
            _ => {}
        }
    }
    for entry in &waiting {
        let rid = message_id(&entry.id);
        let how = known.get(&rid).map(String::as_str);
        let unsure = racing || before.contains(&rid) || lock(&ENQUEUING).contains_key(&rid);
        if how.is_some() || unsure {
            lock(&MISSED).remove(&rid);
        }
        let gone = how.is_none() && !unsure && !lock(&MISSED).insert(rid.clone());
        match how {
            Some("failed") => end_message(hub, entry, MessageEnd::Failed),
            None if gone => end_message(hub, entry, MessageEnd::Failed),
            Some("withdrawn") => end_message(hub, entry, MessageEnd::Cancelled),
            // A turn carried it: the actor's start marks it sent, a recovered one already ran.
            Some("answered" | "recovered") => end_message(hub, entry, MessageEnd::Sent),
            _ => {}
        }
    }
    // A card or message that left the set takes its miss with it.
    lock(&MISSED).retain(|id| {
        cards.iter().any(|c| c.id == *id) || id.strip_prefix("msg:").is_some_and(|e| waiting.iter().any(|w| w.id == e))
    });
}

/// Whether the kernel's run door carries this card's wake.
fn kernel_door(card: &Card) -> bool {
    matches!(door_of(card), Some(Door::Run { .. })) && card.person.is_empty() && card.reply_to.is_none()
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
    if !wake::has_items(&state) {
        return;
    }
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

/// A turn the host took and lost track of (a callback that never came) goes back to the queue after [`TAKEN_TTL`]. A
/// turn that really waits with a bot's actor or a channel run behind a long turn is not one of these: giving that back
/// would deliver it twice.
fn reap(hub: &Hub) {
    reap_after(hub, TAKEN_TTL);
}

fn reap_after(hub: &Hub, ttl: Duration) {
    reap_with(hub, ttl, TRACKED_TTL);
}

fn reap_with(hub: &Hub, ttl: Duration, tracked: Duration) {
    let stale: Vec<String> = lock(&TAKEN)
        .iter()
        .filter(|(_, at)| at.elapsed() >= ttl)
        .map(|(run, at)| (run.clone(), at.elapsed() >= tracked))
        .filter(|(run, long)| {
            *long || (!lock(&ENTRIES).values().any(|c| c.run == *run) && !lock(&CHANNEL_RUNS).contains(run))
        })
        .map(|(run, _)| run)
        .collect();
    for run in stale {
        lock(&TAKEN).remove(&run);
        lock(&ENTRIES).retain(|_, c| c.run != run);
        lock(&CHANNEL_RUNS).remove(&run);
        let st = state_of(hub);
        wake::spawn(move || wake::back(&st, &run, true));
    }
}

/// The host's taker: looks at the queue soon after a request arrives and every few seconds otherwise (a look at a
/// folder, not a process, unless something is queued), offers again the cards the queue hasn't got now and then, and
/// settles the cards the kernel's run door carried.
pub fn run_taker(hub: &Arc<Hub>) {
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        let mut looks = 0u32;
        loop {
            tokio::select! {
                () = KICK.notified() => tokio::time::sleep(SETTLE).await,
                () = tokio::time::sleep(LOOK_EVERY) => looks += 1,
            }
            let Some(hub) = weak.upgrade() else { return };
            if looks.is_multiple_of(OFFER_EVERY) && super::settings(&hub.store).resume {
                for card in hub.store.cards_awaiting_resume(now_ms()) {
                    attempt(&hub, &card.id).await;
                }
            }
            settle_queue(&hub).await;
            drive(&hub).await;
        }
    });
}

/// One taken turn goes through its door.
async fn handle(hub: &Arc<Hub>, turn: Turn) {
    lock(&TAKEN).insert(turn.run.clone(), Instant::now());
    let carried: Vec<Card> =
        turn.ids.iter().filter_map(|i| i.strip_prefix("card:")).filter_map(|id| hub.store.card(id)).collect();
    // The door comes from any card of the turn: a request that isn't a card (a `ci:` result, a kith ask answer) still
    // goes where the session is, even if the cards beside it died.
    let door_card = carried.first().cloned();
    let others = turn.ids.iter().filter(|i| !i.starts_with("card:") && !i.starts_with("msg:")).count();
    let person = turn.person.clone().filter(|p| !p.is_empty());
    // A7 again, at the door: a person's access can end between the yes and the turn.
    if let Some(person) = &person {
        match crate::api::roles::may_use_checked(person, &turn.agent).await {
            Some(false) => {
                super::withdraw_person(hub, person, Some(&turn.agent));
                return give_up(hub, &turn.run);
            }
            None => return unstarted(hub, &turn.run, true, true),
            Some(true) => {}
        }
    }
    // The terminal's hold is asked before anything is read below: the last await, so a Stop or an unsend that raced
    // the take is seen by what follows, which does not wait again before the actor has the messages.
    let first_message =
        turn.ids.iter().filter_map(|i| i.strip_prefix("msg:")).filter_map(|id| hub.store.entry(id)).find(waits);
    if let Some(e) = first_message
        && terminal_holds(hub, &lane_of_message(&e)).await
    {
        // The terminal still has it: back to the queue, which asks again after its pause.
        return unstarted(hub, &turn.run, true, true);
    }
    // Looked at last, after the awaits: a Stop that raced the take has closed its cards by now (R8).
    let live: Vec<Card> = carried.iter().filter_map(|c| hub.store.card(&c.id)).filter(wanted).collect();
    // The app messages that still wait (one Stopped or taken back is no longer wanted), with what each asked.
    let messages = waiting_messages(hub, &turn);
    // Every card was withdrawn, expired, Stopped or already carried by a turn that started, and nothing else was asked.
    if live.is_empty() && others == 0 && messages.is_empty() {
        return give_up(hub, &turn.run);
    }
    // What a card already carried is not said again; every other request's words stand. The messages are not in the
    // note at all: they go to the agent as the user's own messages, one each.
    let sent = turn.ids.iter().filter(|i| i.starts_with("msg:")).count();
    let note =
        if sent == 0 && live.len() + others == turn.ids.len() { turn.note.clone() } else { rebuilt(&turn, &live) };
    for c in &live {
        let _ = hub.store.card_resume_claim(&c.id, now_ms()); // a restart gave the claim back: it is held again
    }
    let ids = card_ids(&live);
    let session = turn.session.as_str();
    if session.starts_with("acp:") {
        let door = match door_card.as_ref().and_then(door_of) {
            Some(door) => Some(door),
            None => messages.first().map(|(e, _)| Door::Bot { bot: e.bot_id.clone(), lane: lane_of_message(e) }),
        };
        match door {
            Some(Door::Bot { bot, lane }) => {
                bot_door(hub, &turn.run, &live, &bot, &lane, &note, &messages);
            }
            _ => unstarted(hub, &turn.run, true, false),
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

/// The turn's words without the cards that are no longer wanted, in the shape kith-wake writes them.
fn rebuilt(turn: &Turn, live: &[Card]) -> String {
    let keep: Vec<&wake::Asked> = turn
        .requests
        .iter()
        .filter(|r| r.id.strip_prefix("card:").is_none_or(|id| live.iter().any(|c| c.id == id)))
        .filter(|r| !r.id.starts_with("msg:"))
        .collect();
    if keep.is_empty() {
        return String::new();
    }
    let words = |r: &wake::Asked| if r.why.is_empty() { r.text.clone() } else { format!("{}: {}", r.why, r.text) };
    let head = format!("[kith wake {}]", turn.run);
    if let [one] = keep.as_slice() {
        return format!("{head} {}", words(one));
    }
    let mut note = format!("{head} {} things happened while this session was idle:", keep.len());
    for (n, r) in keep.iter().enumerate() {
        let _ = write!(note, "\n{}. {}", n + 1, words(r));
    }
    note
}

/// Nothing of this turn will run (every request is withdrawn, or the thread is gone): the queue is told, ids kept.
fn give_up(hub: &Hub, run: &str) {
    settled(run);
    let (state, run) = (state_of(hub), run.to_owned());
    wake::spawn(move || wake::done(&state, &run, "withdrawn"));
}

fn settled(run: &str) {
    lock(&TAKEN).remove(run);
    lock(&CHANNEL_RUNS).remove(run);
}

/// The turn `run` started: what it carried is delivered (M5).
fn started(hub: &Hub, run: &str, cards: &[String]) {
    settled(run);
    applied(hub, cards);
    let (state, run) = (state_of(hub), run.to_owned());
    wake::spawn(move || wake::done(&state, &run, "answered"));
}

/// The turn `run` did not start. `again`: it goes back to the queue (`held`: a writer had the session, which is not a
/// failed try); the next take drops what is no longer wanted and keeps every other request. Otherwise (a Stop) it is
/// dropped, ids kept: Stop wins over a yes already given (R8), and the closers of [`stopped`] end its cards.
fn unstarted(hub: &Hub, run: &str, again: bool, held: bool) {
    settled(run);
    let (state, run_id) = (state_of(hub), run.to_owned());
    if again {
        wake::spawn(move || wake::back(&state, &run_id, held));
    } else {
        wake::spawn(move || wake::done(&state, &run_id, "withdrawn"));
    }
}

/// A Stop ends the wake of cards its turn carried (those not yet delivered), whatever their kind.
fn stop_cards(hub: &Hub, ids: &[String]) {
    for card in cards_of(hub, ids) {
        end_for_stop(hub, &card);
    }
}

/// An approved card whose turn had not started ends: withdrawn, except a saved key's card, which keeps its record and
/// is marked as not owed any more (secret cards close only through their own steps).
fn end_for_stop(hub: &Hub, card: &Card) {
    if card.tool == super::secret::TOOL {
        let _ = hub.store.card_resume_stop(&card.id, now_ms());
    } else {
        let _ = hub.store.card_resume_close(&card.id);
    }
}

// ---- door (a): a bot's chat

/// Whether `bot` has left (or never was in) the group the lane is in: sending there would start a fresh actor in a room
/// the bot no longer belongs to. A chat that is not a group (the bot's own chat, anything unknown) is never judged.
fn left_room(hub: &Hub, bot: &str, lane: &Lane) -> bool {
    if lane.chat == bot {
        return false;
    }
    match hub.store.bot(&lane.chat).ok().flatten() {
        Some(row) if row.config.is_group() => row.deleted || !row.config.members.iter().any(|m| m == bot),
        _ => false,
    }
}

/// The turn goes to the bot's actor: a notice for the cards (when the turn carries any), then the app messages that
/// waited, each as the entry it is. The actor merges them into the one turn, in this order.
fn bot_door(hub: &Arc<Hub>, run: &str, group: &[Card], bot: &str, lane: &Lane, note: &str, messages: &[Message]) {
    let alive = hub.store.bot(bot).ok().flatten().is_some_and(|b| !b.deleted);
    if !alive || left_room(hub, bot, lane) {
        // The cards were re-claimed by `handle`: only a close ends a claimed card.
        end_claimed(hub, group);
        for (entry, _) in messages {
            end_message(hub, entry, MessageEnd::Cancelled);
        }
        return give_up(hub, run);
    }
    let mut sends: Vec<(String, String)> = Vec::new();
    if !note.is_empty() {
        let turn = hub.store.max_turn(&lane.chat);
        let shown = note.split_once("] ").map_or(note, |(_, rest)| rest);
        let Some(entry) = hub.add_entry(lane, EntryKind::Notice, turn, &json!({"text": shown, "style": "info"})) else {
            return unstarted(hub, run, true, false);
        };
        lock(&ENTRIES).insert(entry.id.clone(), Carried { run: run.to_owned(), cards: card_ids(group) });
        #[cfg(test)]
        tests_seen::record("bot", group.first().map_or(run, |c| c.session.as_str()), note, Some(&entry.id));
        sends.push((entry.id, note.to_owned()));
    }
    for (entry, prompt) in messages {
        lock(&ENTRIES).insert(entry.id.clone(), Carried { run: run.to_owned(), cards: Vec::new() });
        // With the actor now: still queued until its prompt is sent, no longer waiting for the terminal.
        set_waits(hub, &entry.id, "turn");
        #[cfg(test)]
        tests_seen::record("bot", &session_key(lane), prompt, Some(&entry.id));
        sends.push((entry.id.clone(), prompt.clone()));
    }
    // One command, so an idle actor starts the notice and the messages as one turn.
    #[cfg(not(test))]
    {
        let ids: Vec<String> = sends.iter().map(|(id, _)| id.clone()).collect();
        if let Err(error) = hub.send_cmd(bot, crate::agent::bot::Cmd::WakeBatch { lane: lane.clone(), items: sends }) {
            tracing::warn!(error = format!("{error:#}"), "couldn't queue a wake turn");
            for id in &ids {
                lock(&ENTRIES).remove(id);
                set_waits(hub, id, "terminal");
            }
            unstarted(hub, run, true, false);
        }
    }
    #[cfg(test)]
    drop(sends);
}

/// Whether a wake's entry is with a bot's actor right now.
pub fn wake_carries(entry_id: &str) -> bool {
    lock(&ENTRIES).contains_key(entry_id)
}

/// What a waiting message waits for, on its entry: `terminal` (the thread is held) or `turn` (the actor has it).
fn set_waits(hub: &Hub, entry_id: &str, what: &str) {
    if let Some(mut e) = hub.store.entry(entry_id).filter(waits) {
        e.data["waitsFor"] = what.into();
        hub.set_entry(entry_id, &e.data);
    }
}

/// An actor took a message back (unsend) that a wake carried: the wake is no longer waiting on it, and the queue is told
/// when it was the last of its turn.
pub fn unsent(hub: &Hub, entry_id: &str) {
    let Some(c) = lock(&ENTRIES).remove(entry_id) else { return };
    if !lock(&ENTRIES).values().any(|other| other.run == c.run) {
        give_up(hub, &c.run);
    }
}

/// The bot's turn for these entries started (its prompt went to the agent): a wake among them is delivered. Called
/// from the actor once the prompt is sent.
pub fn turn_started(hub: &Hub, entry_ids: &[String]) {
    let mut runs: Vec<String> = Vec::new();
    for entry in entry_ids {
        let Some(c) = lock(&ENTRIES).remove(entry) else { continue };
        clear_held(hub, &cards_of(hub, &c.cards));
        applied(hub, &c.cards);
        if let Some(e) = hub.store.entry(entry) {
            end_message(hub, &e, MessageEnd::Sent);
        }
        if !runs.contains(&c.run) {
            runs.push(c.run);
        }
    }
    // A run is answered when the last of its entries has started: messages the actor kept for a later turn still wait.
    for run in runs {
        if !lock(&ENTRIES).values().any(|c| c.run == run) {
            started(hub, &run, &[]);
        }
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
        // A message of the wake waits for the terminal again.
        set_waits(hub, entry, "terminal");
        unstarted(hub, &c.run, true, true);
    }
}

/// A bot's actor shut down with these entries still queued: their wakes go back to the queue (it keeps them for the
/// next actor or a restart), instead of staying "taken" by a host that never starts them.
pub fn actor_gone(hub: &Arc<Hub>, entry_ids: &[String]) {
    not_started(hub, entry_ids, true);
}

/// A wake turn on a bot's actor failed to start for another reason. `again`: it is owed again (the queue pauses
/// before it comes back); after a Stop it is dropped, and the stopped yes does not come back.
pub fn not_started(hub: &Arc<Hub>, entry_ids: &[String], again: bool) {
    for entry in entry_ids {
        let Some(c) = lock(&ENTRIES).remove(entry) else { continue };
        if !again {
            stop_cards(hub, &c.cards);
        }
        unstarted(hub, &c.run, again, false);
    }
}

// ---- door (b): a channel thread

fn channel_door(hub: &Arc<Hub>, run: &str, group: &[Card], card_ids: &[String], wk: &str, note: &str) {
    // The channel carries the cards and the turn's run in one list: its run settles both ([`channel_started`]).
    let mut carried = card_ids.to_vec();
    carried.push(format!("run:{run}"));
    lock(&CHANNEL_RUNS).insert(run.to_owned());
    match crate::channels::wake(wk, note, || carried) {
        Wake::Queued => clear_held(hub, group),
        Wake::Claimed => {}
        Wake::NoThread => {
            end_claimed(hub, group);
            give_up(hub, run);
        }
        // The connection isn't up (yet): the queue keeps it and asks again after its pause.
        Wake::Offline => unstarted(hub, run, true, true),
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
        if !again {
            stop_cards(hub, &cards);
        }
        unstarted(hub, &run, again, false);
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
    // The owner's session is the kernel's own kind of run (a wake), so its turn can still set waiters.
    set("VIA", if person.is_empty() { "wake" } else { "host" });
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
        end_claimed(hub, live);
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
        return unstarted(hub, &turn.run, true, true);
    }
    #[cfg(not(test))]
    let Some(bin) = crate::agent::hq::bin() else { return unstarted(hub, &turn.run, true, true) };
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
                unstarted(hub, &turn.run, true, true);
            }
            Exec::Failed(why) => {
                tracing::warn!(run = %turn.run, why, "couldn't resume a session for an approved card");
                unstarted(hub, &turn.run, true, false);
            }
        }
    }
}

/// Gives back what the host took and lost track of, as if `ttl` had passed.
#[cfg(test)]
pub(crate) fn left_room_for_test(hub: &Hub, bot: &str, lane: &Lane) -> bool {
    left_room(hub, bot, lane)
}

#[cfg(test)]
pub(crate) fn reap_for_test(hub: &Hub, ttl: Duration) {
    reap_after(hub, ttl);
}

/// Gives back even the turns a door seems to hold, as if the upper bound had passed.
#[cfg(test)]
pub(crate) fn reap_long_for_test(hub: &Hub) {
    reap_with(hub, Duration::ZERO, Duration::ZERO);
}

/// Whether the settle remembers a miss for this card or message request.
#[cfg(test)]
pub(crate) fn missed_for_test(id: &str) -> bool {
    lock(&MISSED).contains(id)
}

/// How many enqueues finished against this host's queue.
#[cfg(test)]
pub(crate) fn enqueued_for_test(hub: &Hub) -> usize {
    enqueued(&state_of(hub))
}

/// One enqueue finishing against another queue.
#[cfg(test)]
pub(crate) fn enqueue_elsewhere_for_test(state: &Path) {
    drop(Enqueuing::mark(state, "elsewhere"));
}

/// Whether any request is marked as on its way into the queue.
#[cfg(test)]
pub(crate) fn enqueuing_for_test(id: &str) -> bool {
    lock(&ENQUEUING).contains_key(id)
}

/// A taken turn the host has no door callback for.
#[cfg(test)]
pub(crate) fn lose_track_of(run: &str) {
    lock(&TAKEN).insert(run.to_owned(), Instant::now());
}

/// A host that stopped and started again knows nothing of the turns it had taken (the queue recovers them).
#[cfg(test)]
pub(crate) fn forget_in_flight() {
    lock(&ENTRIES).clear();
    lock(&TAKEN).clear();
}

// ---- app messages that wait for a terminal

/// A user message of an app chat with the words the agent is told for it.
type Message = (Entry, String);

/// The wake request of a user message that waits for the terminal holding its thread.
fn message_id(entry_id: &str) -> String {
    format!("msg:{entry_id}")
}

/// The place a message's turn runs in: a task's first message starts its own thread.
fn lane_of_message(e: &Entry) -> Lane {
    match &e.thread_id {
        Some(root) => Lane::in_thread(&e.bot_id, root),
        None if e.data["task"] == true => Lane::in_thread(&e.bot_id, &e.id),
        None => Lane::main(&e.bot_id),
    }
}

/// The wake queue's key for a bot's place: the one cards use too, so a card answered meanwhile shares the turn.
fn session_key(lane: &Lane) -> String {
    format!("acp:{}:{}", lane.chat, lane.thread.as_deref().unwrap_or_default())
}

/// A message waits while it is queued and flagged: a Stop, an unsend or a failure has changed its status otherwise.
fn waits(e: &Entry) -> bool {
    e.kind == EntryKind::User.as_str() && e.data["status"] == "queued" && !e.data["waitsFor"].is_null()
}

/// The messages of a taken turn that are still wanted, oldest first, with what the agent is told for each.
fn waiting_messages(hub: &Hub, turn: &Turn) -> Vec<Message> {
    turn.ids
        .iter()
        .filter_map(|i| i.strip_prefix("msg:"))
        .filter_map(|id| hub.store.entry(id))
        .filter(waits)
        .filter_map(|e| {
            let cfg = hub.store.bot(&e.bot_id).ok().flatten().filter(|r| !r.deleted)?.config;
            let prompt = crate::chat::prompt::of_entry(&hub.store, &cfg, &e.data);
            Some((e, prompt))
        })
        .collect()
}

/// Whether a terminal (or a take-over) still holds the thread `lane` runs in. When nobody does, the terminal's last
/// lines are imported first, so the turn carries on from the whole conversation (the same step as a send's).
async fn terminal_holds(hub: &Arc<Hub>, lane: &Lane) -> bool {
    let Some(root) = &lane.thread else { return false };
    // A bot guest's writer is an error here: not ours to run into, so it waits like a terminal.
    crate::terminal::send_gate(hub, &lane.chat, root, true)
        .await
        .map_or(true, |g| g == crate::terminal::SendGate::Waits)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageEnd {
    /// A turn carried it.
    Sent,
    /// Stop, or unsend: the user does not want it.
    Cancelled,
    /// The queue gave up on it, or lost it.
    Failed,
}

/// A waiting message is over: its entry says how, and no longer says it waits.
fn end_message(hub: &Hub, entry: &Entry, how: MessageEnd) {
    let Some(mut e) = hub.store.entry(&entry.id).filter(waits) else { return };
    e.data["status"] = match how {
        MessageEnd::Sent => "sent",
        MessageEnd::Cancelled => "cancelled",
        MessageEnd::Failed => "failed",
    }
    .into();
    if let Some(data) = e.data.as_object_mut() {
        data.remove("waitsFor");
    }
    hub.set_entry(&e.id, &e.data);
}

/// Wakes the taker now (a message was queued, a take-over ended).
pub fn kick() {
    KICK.notify_one();
}

/// A user message to a thread a terminal holds: it waits as a wake until the terminal lets go, is never typed into the
/// terminal and never starts a second writer. Door: the host's, because only the host can run the thread's turn (its
/// bot, its session and the entries the apps show); the kernel's run door would resume the Claude session on its own and
/// leave the entry behind. Several messages merge into the thread's one next turn, in order, and share it with a card
/// answered meanwhile. An error leaves the message not queued at all: the caller fails it.
pub async fn wait_for_terminal(hub: &Arc<Hub>, agent: &str, entry_id: &str) -> anyhow::Result<()> {
    // Already carried by a wake (a turn of its own start was refused): the queue has it.
    if lock(&ENTRIES).contains_key(entry_id) {
        return Ok(());
    }
    let entry = hub.store.entry(entry_id).ok_or_else(|| anyhow::anyhow!("unknown message"))?;
    let (state, id) = (state_of(hub), message_id(entry_id));
    let _marker = Enqueuing::mark(&state, &id);
    let lane = lane_of_message(&entry);
    let cfg =
        hub.store.bot(&entry.bot_id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow::anyhow!("unknown bot"))?.config;
    let (key, prompt, agent) =
        (session_key(&lane), crate::chat::prompt::of_entry(&hub.store, &cfg, &entry.data), agent.to_owned());
    let mut data = entry.data.clone();
    data["waitsFor"] = "terminal".into();
    hub.set_entry(entry_id, &data);
    let queued = wake::blocking(move || {
        wake::enqueue(
            &state,
            &Request {
                session: &key,
                id: &id,
                agent: &agent,
                person: None,
                room: None,
                host_door: true,
                why: "",
                text: &prompt,
            },
        )
    })
    .await
    .ok_or_else(|| anyhow::anyhow!("the wake call didn't finish"));
    if let Err(error) = queued.and_then(|r| r) {
        let mut data = entry.data;
        if let Some(d) = data.as_object_mut() {
            d.remove("waitsFor");
        }
        hub.set_entry(entry_id, &data);
        return Err(error);
    }
    kick();
    Ok(())
}

/// Takes back a message that waits in the queue: whether it did. One a turn already carries is the actor's to take back.
pub async fn unsend_waiting(hub: &Arc<Hub>, entry: &Entry) -> bool {
    if !waits(entry) {
        return false;
    }
    let (state, key, id) = (state_of(hub), session_key(&lane_of_message(entry)), message_id(&entry.id));
    let gone = wake::blocking(move || {
        wake::withdraw(&state, &key, Some(&id));
        wake::status(&state, &id)
    })
    .await
    .flatten();
    if gone.as_deref() != Some("withdrawn") {
        return false;
    }
    end_message(hub, entry, MessageEnd::Cancelled);
    true
}

/// Stop: the messages that wait in `matches` places are dropped; their wakes are withdrawn with the rest of the place's
/// (the caller's key list), and the entries say they were not sent.
fn cancel_waiting(hub: &Hub, matches: impl Fn(&Lane) -> bool) {
    for e in hub.store.waiting_for_terminal().unwrap_or_default() {
        if matches(&lane_of_message(&e)) {
            end_message(hub, &e, MessageEnd::Cancelled);
            let (state, key, id) = (state_of(hub), session_key(&lane_of_message(&e)), message_id(&e.id));
            wake::spawn(move || wake::withdraw(&state, &key, Some(&id)));
        }
    }
}

// ---- Stop

/// Stop wins over a yes already given (R8): what the stopped places are owed is withdrawn from the queue (their ids
/// kept, so a late copy doesn't come back), and the approved cards whose turn had not started end, so neither a restart
/// nor another yes in the session brings the stopped turn back. `keys` are the queue sessions the caller knows it
/// stops; the cards of `matches` add theirs. A turn the queue already gave to a door is settled by that door's own
/// Stop ([`not_started`], [`channel_failed`]).
pub fn stopped(hub: &Hub, keys: &[String], ending: Vec<String>, matches: impl Fn(&Door) -> bool) {
    let mut keys: BTreeSet<String> = keys.iter().cloned().collect();
    for card in hub.store.cards_unapplied() {
        let Some(door) = door_of(&card).filter(&matches) else { continue };
        keys.insert(key_of(&door, &card));
        // Closed first: a request still on its way into the queue finds the card closed and is withdrawn again.
        end_for_stop(hub, &card);
    }
    let state = state_of(hub);
    let withdraw = {
        let (state, keys) = (state.clone(), keys.clone());
        move || {
            for k in &keys {
                wake::withdraw(&state, k, None);
            }
        }
    };
    wake::spawn(withdraw.clone());
    // The delegations those places started end too (R5, R8): their runs stop and their records settle. An answer landing
    // between the two steps could still enqueue a wake, so the queue is withdrawn once more after they have ended.
    if !ending.is_empty()
        && let Ok(rt) = tokio::runtime::Handle::try_current()
    {
        drop(rt.spawn(async move {
            crate::delegation::end(&ending, &[]).await;
            drop(tokio::task::spawn_blocking(withdraw).await);
        }));
    }
}

/// The Claude sessions of the lanes whose key (`chat` or `chat/root`) `at` accepts (of one `bot`, or of every bot in the
/// place), and the main session of `main_of` when that bot is stopped in its own main chat. The `kith ask` jobs these sessions started are the stopped lane's.
fn lane_sessions(hub: &Hub, bot: Option<&str>, at: impl Fn(&str) -> bool, main_of: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = hub
        .store
        .kv_prefix(&bot.map_or_else(|| "lane.session.".to_owned(), |b| format!("lane.session.{b}@")))
        .into_iter()
        .filter(|k| k.split_once('@').is_some_and(|(_, lane)| at(lane)))
        .filter_map(|k| hub.store.kv_get(&k))
        .collect();
    out.extend(main_of.and_then(|bot| hub.store.bot(bot).ok().flatten()).and_then(|b| b.session_id));
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    out
}

/// Every session a bot has: its main one and those of its lanes.
fn bot_sessions(hub: &Hub, bot_id: &str) -> Vec<String> {
    let prefix = format!("lane.session.{bot_id}@");
    let mut out: Vec<String> = hub
        .store
        .kv_prefix(&prefix)
        .into_iter()
        .filter_map(|k| hub.store.kv_get(&k))
        .chain(hub.store.bot(bot_id).ok().flatten().and_then(|b| b.session_id))
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Stop in one place of a bot (its chat or one thread).
pub fn stopped_lane(hub: &Hub, bot: &str, lane: &Lane) {
    cancel_waiting(hub, |l| l == lane);
    let key = lane.key();
    let ending = lane_sessions(hub, Some(bot), |at| at == key, lane.thread.is_none().then_some(bot));
    stopped(hub, &[session_key(lane)], ending, |d| matches!(d, Door::Bot { lane: l, .. } if l == lane));
}

/// Stop everywhere in a chat (a group stops as a whole).
pub fn stopped_chat(hub: &Hub, chat: &str) {
    cancel_waiting(hub, |l| l.chat == chat);
    let ending = lane_sessions(
        hub,
        None,
        |at| at == chat || at.strip_prefix(chat).is_some_and(|r| r.starts_with('/')),
        Some(chat),
    );
    stopped(hub, &[], ending, |d| matches!(d, Door::Bot { lane, .. } if lane.chat == chat));
}

/// Stop everywhere a bot talks.
pub fn stopped_bot(hub: &Hub, bot_id: &str) {
    cancel_waiting(hub, |l| l.chat == bot_id);
    stopped(hub, &[], bot_sessions(hub, bot_id), |d| matches!(d, Door::Bot { bot, .. } if bot == bot_id));
}

/// Stop in a channel thread (its work key).
pub fn stopped_thread(hub: &Hub, wk: &str) {
    stopped_thread_of(hub, wk, crate::channels::thread_sessions(wk));
}

/// [`stopped_thread`] with the thread's Claude sessions given: their delegations end with it (R8), as a lane's do.
pub fn stopped_thread_of(hub: &Hub, wk: &str, sessions: Vec<String>) {
    stopped(hub, &[wk.to_owned()], sessions, |d| matches!(d, Door::Channel(w) if w == wk));
}

#[cfg(not(test))]
enum Exec {
    Running,
    Held,
    Failed(String),
}

#[cfg(not(test))]
async fn exec(bin: &Path, spawn: &Spawn, grant: Option<crate::runauth::RunGrant>) -> Exec {
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
