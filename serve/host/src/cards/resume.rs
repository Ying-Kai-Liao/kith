//! The resume half of approval cards (C3-C8, A7, L2): an approved card whose asker was gone owes its session a turn.
//!
//! The card store is the record of an owed resume (`Store::cards_awaiting_resume`: approved or used-by-a-later-ask,
//! granted, not resumed, before the deadline). The doors below only deliver it, and a restart re-derives the whole list
//! from the store (`cards::start`), never from a door's queue: the channel inbox is emptied before a run starts, so a
//! crash or a refused claim would drop what it held (C5).
//!
//! One resume per card: [`Store::card_resume_claim`] is a compare-and-set on `resumed_at`, taken just before the turn is
//! handed to a door, so a door's copy and a restart's re-derive can't start two turns. A refused hand-over (the session's
//! writer claim is held, the bot's actor is gone) gives the claim back and the resume stays owed; it is retried with a
//! doubling pause (5 s up to 10 min) until it happens or the card expires (silence is no).
//!
//! Doors, by where the card came from (its `session`):
//! - `acp:<chat>:<thread>`: a bot chat's turn. A turn is queued on the bot's actor (`agent/bot/queue.rs`), after a short
//!   notice in the thread. A held writer claim fails that turn's start; the actor calls [`refused`].
//! - `run:..:<thread>#<agent>`: a channel thread. The note joins the channel inbox (`Core::wake`).
//! - `run:..:<claude session>`: a `claude -p` run the kernel started (`kith ask`, `kith-sched`). The host runs
//!   `kith <agent> --resume <session> "<note>"` with `KITH_PERSON` set to the card's person (absent for the owner).
//!
//! A card used by a later message before its resume still gets the wake, without a grant (C7, A9): the run asks again
//! if it still needs it.

use super::Card;
use crate::channels::Wake;
use crate::hub::Hub;
use crate::store::{CardState, EntryKind, Lane, now_ms};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(600);
/// How long the kernel's one-shot waits for a held session before it exits 1 with "held by ..." (`KITH_CLAIM_WAIT`, #244,
/// default 5). The spawn gets it explicitly so the rule below doesn't depend on the kernel's default.
const CLAIM_WAIT_S: u64 = 5;
/// The (c) delivered rule: a spawn still alive this long after start has the session, because a held one would have
/// exited by `CLAIM_WAIT_S`. The two numbers are one rule: raise `CLAIM_WAIT_S` and this follows.
#[cfg(not(test))]
const EARLY_EXIT: Duration = Duration::from_secs(CLAIM_WAIT_S + 3);

/// Cards with a retry loop running: an answer, a restart and a refused claim all offer the same card.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// The entry of a queued wake turn -> the cards it answers, so a refused start can give their claims back.
static WAKES: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What one attempt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Delivered, taken by someone else, dropped or expired: nothing more is owed from this card.
    Done,
    /// Still owed: try again after the pause.
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

/// A resume is owed for `card`: attempt it now, and again with a doubling pause while it stays owed.
pub fn offer(hub: &Arc<Hub>, card: &Card) {
    if !lock(&IN_FLIGHT).insert(card.id.clone()) {
        return;
    }
    let (weak, id): (Weak<Hub>, String) = (Arc::downgrade(hub), card.id.clone());
    tokio::spawn(async move {
        let mut pause = FIRST_RETRY;
        loop {
            let Some(hub) = weak.upgrade() else { break };
            if attempt(&hub, &id).await == Step::Done {
                break;
            }
            drop(hub);
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(MAX_RETRY);
        }
        lock(&IN_FLIGHT).remove(&id);
    });
}

/// Claims the resume of every card in `group` (the owed cards of one session); returns the ones this caller got.
fn claim_all(hub: &Hub, group: &[Card]) -> Vec<Card> {
    let now = now_ms();
    group.iter().filter(|c| hub.store.card_resume_claim(&c.id, now).unwrap_or(false)).cloned().collect()
}

fn release_all(hub: &Hub, cards: &[Card]) {
    for c in cards {
        let _ = hub.store.card_resume_release(&c.id);
    }
}

fn end_owed(hub: &Hub, card: &Card) {
    let _ = hub.store.card_resume_drop(&card.id);
}

/// One try at the resume of card `id`. Safe to call from anywhere, any number of times: the claim decides.
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
    let note = note_for(hub, &group).await;
    let Some(door) = door_of(&card) else {
        end_owed(hub, &card);
        return Step::Done;
    };
    match door {
        Door::Bot { bot, lane } => bot_door(hub, &group, &bot, &lane, &note),
        Door::Channel(wk) => channel_door(hub, &group, &wk, &note),
        Door::Run { session } => run_door(hub, &card, &group, &session, &note).await,
    }
}

/// The words the agent is told: who allowed what, and that it can go on. A used grant is said so (C7).
async fn note_for(hub: &Hub, group: &[Card]) -> String {
    let who = super::approver_label(hub, &group[0]).await;
    let (keys, asks): (Vec<&Card>, Vec<&Card>) = group.iter().partition(|c| c.tool == super::secret::TOOL);
    let parts: Vec<String> = asks
        .iter()
        .map(|c| {
            let what = super::short(&c.shown, 80);
            if c.state == CardState::Used {
                format!(
                    "{who} allowed {what} once, but a later request already used that; ask again if you still need it"
                )
            } else {
                format!("{who} allowed {what} once")
            }
        })
        .collect();
    // A saved key is told by name only (never a value); its note stands on its own.
    let mut note = String::from("[Kith]");
    if !parts.is_empty() {
        note.push(' ');
        note.push_str(&parts.join("; "));
        note.push_str("; go on.");
    }
    for c in keys {
        note.push(' ');
        note.push_str(&super::secret::note(hub, &who, c));
    }
    note
}

// ---- door (a): a bot's chat

fn bot_door(hub: &Arc<Hub>, group: &[Card], bot: &str, lane: &Lane, note: &str) -> Step {
    let alive = hub.store.bot(bot).ok().flatten().is_some_and(|b| !b.deleted);
    if !alive {
        for c in group {
            end_owed(hub, c);
        }
        return Step::Done;
    }
    let claimed = claim_all(hub, group);
    if claimed.is_empty() {
        return Step::Done;
    }
    let turn = hub.store.max_turn(&lane.chat);
    let shown = note.trim_start_matches("[Kith] ");
    let Some(entry) = hub.add_entry(lane, EntryKind::Notice, turn, &json!({"text": shown, "style": "info"})) else {
        release_all(hub, &claimed);
        return Step::Retry;
    };
    lock(&WAKES).insert(entry.id.clone(), claimed.iter().map(|c| c.id.clone()).collect());
    clear_held(hub, &claimed);
    #[cfg(test)]
    {
        tests_seen::record("bot", &claimed[0].session, note, &[]);
        Step::Done
    }
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
            lock(&WAKES).remove(&entry.id);
            release_all(hub, &claimed);
            return Step::Retry;
        }
        Step::Done
    }
}

/// A wake turn on a bot's actor could not start because the session's writer is held: the claims go back and the
/// resume is offered again, with the holder shown on the card. Called from the actor's `start_failed`.
pub fn refused(hub: &Arc<Hub>, entry_ids: &[String], session: Option<&str>) {
    let holder =
        session.and_then(|sid| crate::terminal::claim::holder(&hub.terminal.env().state, sid).map(|c| (sid, c)));
    for entry in entry_ids {
        let Some(ids) = lock(&WAKES).remove(entry) else { continue };
        for id in ids {
            let Some(card) = hub.store.card(&id) else { continue };
            let _ = hub.store.card_resume_release(&id);
            set_held(hub, &card, holder.as_ref().map(|(sid, c)| (*sid, c)));
            offer(hub, &card);
        }
    }
}

// ---- door (b): a channel thread

fn channel_door(hub: &Arc<Hub>, group: &[Card], wk: &str, note: &str) -> Step {
    let got = Mutex::new(Vec::new());
    let result = crate::channels::wake(wk, note, || {
        let claimed = claim_all(hub, group);
        let ids = claimed.iter().map(|c| c.id.clone()).collect();
        *got.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = claimed;
        ids
    });
    let claimed = got.into_inner().unwrap_or_default();
    match result {
        Wake::Queued => {
            clear_held(hub, &claimed);
            Step::Done
        }
        Wake::Claimed => Step::Done,
        Wake::NoThread => {
            for c in group {
                end_owed(hub, c);
            }
            Step::Done
        }
        Wake::Offline => Step::Retry,
    }
}

/// A channel turn carrying a wake never ran (the session was held, the spawn failed): the claims go back, the cards stay
/// owed and are offered again with the usual backoff (C5: the marker means the turn started).
pub fn channel_failed(hub: &Arc<Hub>, card_ids: &[String]) {
    for id in card_ids {
        let Some(card) = hub.store.card(id) else { continue };
        let _ = hub.store.card_resume_release(id);
        offer(hub, &card);
    }
}

// ---- door (c): a -p run the kernel started

/// What the host starts for a resumed -p run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawn {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// `kith <agent> --resume <session> "<note>"` as the card's person, with the env the host gives its own runs: a person's
/// run gets `KITH_PERSON` (the owner's none) so the session doesn't resume under the owner's profile. The run token is
/// added by the caller.
pub fn spawn_for(card: &Card, session: &str, note: &str) -> Spawn {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| crate::channels::run::inherited(k)).collect();
    let mut set = |name: &str, v: &str| env.extend(crate::kernel::both(name, v));
    set("VIA", "host");
    set("CLAIM_WAIT", &CLAIM_WAIT_S.to_string());
    if card.person.is_empty() {
        // The owner: no person, no room set here.
    } else {
        set("PERSON", &card.person);
        set("ROOM", &crate::agent::hq::room_of(&card.agent, Some(&card.person), None));
    }
    Spawn { args: vec![card.agent.clone(), "--resume".into(), session.to_owned(), note.to_owned()], env }
}

async fn run_door(hub: &Arc<Hub>, card: &Card, group: &[Card], session: &str, note: &str) -> Step {
    if !crate::terminal::claim::safe_session(session) {
        for c in group {
            end_owed(hub, c);
        }
        return Step::Done;
    }
    // A live writer on the session keeps the resume owed (the kernel's one-shot would wait and then refuse).
    let state = hub.terminal.env().state;
    let sid = session.to_owned();
    let held = tokio::task::spawn_blocking({
        let (state, sid) = (state.clone(), sid.clone());
        move || crate::terminal::claim::holder(&state, &sid)
    })
    .await
    .ok()
    .flatten();
    if let Some(h) = held {
        set_held(hub, card, Some((session, &h)));
        return Step::Retry;
    }
    #[cfg(not(test))]
    let Some(bin) = crate::agent::hq::bin() else { return Step::Retry };
    let claimed = claim_all(hub, group);
    if claimed.is_empty() {
        return Step::Done;
    }
    let mut spawn = spawn_for(card, session, note);
    // L3, step 4: a file or network card's once grant would be consumed HERE, after the claim and before exec, and
    // the profile built from the consume result. A tool-prompt card consumes it when the resumed run asks (card_open),
    // so no profile carries the grant and the stale-profile race can't happen.
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
        spawn.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    let grant = crate::runauth::grant_for_env(&mut env, &card.agent, session);
    spawn.env = env.iter().filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v.to_str()?.to_owned()))).collect();
    #[cfg(test)]
    {
        let _ = grant;
        tests_seen::record("run", &card.session, note, &spawn.env);
        tests_seen::spawned(spawn);
        clear_held(hub, &claimed);
        Step::Done
    }
    #[cfg(not(test))]
    {
        match exec(&bin, &spawn, grant).await {
            Exec::Running => {
                clear_held(hub, &claimed);
                Step::Done
            }
            Exec::Held => {
                release_all(hub, &claimed);
                let h = crate::terminal::claim::holder(&state, &sid);
                set_held(hub, card, h.as_ref().map(|h| (session, h)));
                Step::Retry
            }
            Exec::Failed(why) => {
                tracing::warn!(card = %card.id, why, "couldn't resume a session for an approved card");
                release_all(hub, &claimed);
                Step::Retry
            }
        }
    }
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
    }

    static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
    static SPAWNS: Mutex<Vec<(Spawn, String)>> = Mutex::new(Vec::new());

    pub fn record(door: &'static str, session: &str, note: &str, _env: &[(String, String)]) {
        SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Seen {
            door,
            session: session.to_owned(),
            note: note.to_owned(),
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
            .filter(|(s, _)| s.args.get(2).map(String::as_str) == Some(session_id))
            .map(|(s, _)| s.clone())
            .collect()
    }
}
