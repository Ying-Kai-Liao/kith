//! The channel door by turn id (design 2026-11-21-wake-s4, `WK4c`, amendment 2026-11-26 O3). A wake turn of a channel thread is
//! claimed at the pull on the thread's work key (`<conn>:<thread>#<agent>`, the turn's own conversation), like a run door
//! turn, and the claim rides with the thread's next run: the launch takes its own writer claim on the thread's session
//! first, then attaches the wake claim naming the work key (`KITH_TURN_CLAIM`, `KITH_TURN_CONVERSATION`).
//!
//! After each attempt the claim's own state is read, not the exit code: attached by this attempt (report its end, the core
//! answers the wake); still `claimed` with no process (the launch stopped before the attach: requeued when a writer holds the
//! session, withdrawn after a Stop, freed otherwise). The wake batch lives in memory only: a batch on disk would replay the
//! note after a restart and send the prompt twice.

use super::{Card, TAKEN, applied, cards_of, clear_held, end_claimed, give_up, lock, set_held, settled, wake};
use crate::channels::Wake;
use crate::hub::Hub;
use crate::routines::run_door::{self, Pace};
use crate::terminal::core_holders::{Reader, Reading};
use crate::terminal::core_writer::{CoreWriter, RowSaid, RunClaim};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A wake turn the core queued, handed to a door of the old kind by its id: the core to claim and begin it on.
pub(super) struct Pull<'a> {
    pub core: &'a Arc<CoreWriter>,
    pub turn: i64,
    pub person: String,
}

/// How many times each run's launch refused the claim: the pause between its tries grows with it.
static SKEW: std::sync::LazyLock<Mutex<std::collections::HashMap<String, u32>>> =
    std::sync::LazyLock::new(Mutex::default);

/// The claimed wake turn a channel thread's next run carries.
pub struct ChannelWake {
    pub run: String,
    pub turn: i64,
    pub claim: i64,
    pub wk: String,
    pub cards: Vec<String>,
    /// The wake queue's request ids of the run, for a Stop's withdraw.
    pub ids: Vec<String>,
    /// An attempt attached the claim, or it ended: no later attempt carries it.
    settled: AtomicBool,
    /// An attempt exited 10 (the launch refused the attach) with the claim still unattached: an older launch that attaches with the
    /// session's id meets the core's refusal of a wake that names the work key. The wake is requeued, never dropped.
    refused: AtomicBool,
    /// The session the attempts run, for the writer check when none attached.
    sid: Mutex<Option<String>>,
}

impl ChannelWake {
    /// A claimed wake for a test of the channel's batch.
    #[cfg(test)]
    pub fn for_test(run: &str, turn: i64, claim: i64, wk: &str) -> Arc<Self> {
        Arc::new(Self {
            run: run.to_owned(),
            turn,
            claim,
            wk: wk.to_owned(),
            cards: Vec::new(),
            ids: Vec::new(),
            settled: AtomicBool::new(false),
            refused: AtomicBool::new(false),
            sid: Mutex::new(None),
        })
    }

    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    pub fn set_sid(&self, sid: Option<&str>) {
        *self.sid.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = sid.map(str::to_owned);
    }

    fn sid(&self) -> Option<String> {
        self.sid.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

/// What one attempt of a run left, as the run saw it.
#[derive(Default)]
pub struct Seen {
    pub pid: Option<u32>,
    pub code: Option<i32>,
}

/// Claims the wake turn and hands it to the channel's worker. The turn stays queued (pulled again after a pause) when the
/// thread's key is held by an earlier wake or the core cannot say.
pub(super) async fn door(hub: &Arc<Hub>, pull: &Pull<'_>, turn: &wake::Turn, group: &[Card], wk: &str, note: &str) {
    let run = turn.run.as_str();
    // A writer on the thread's session: nothing is claimed or spawned, the card shows the holder and the turn waits (as the run door).
    if let Some(sid) = crate::channels::session_of(wk) {
        let state = super::state_of(hub);
        let held = wake::blocking(move || (Reader::new(&state).read(&sid), sid)).await;
        if let Some((Reading::By(h, _), sid)) = held {
            for card in group {
                set_held(hub, card, Some((&sid, &h)));
            }
            return later(run);
        }
    }
    let (core, id, person, key) = (pull.core.clone(), pull.turn, pull.person.clone(), wk.to_owned());
    let claimed = wake::blocking(move || core.claim_run_as("wakeRun", &key, &person, id)).await;
    let claim = match claimed {
        Some(RunClaim::Claimed(c)) => c,
        Some(RunClaim::Later | RunClaim::Down(_)) | None => return later(run),
        Some(RunClaim::Gone) => {
            end_claimed(hub, group);
            return give_up(hub, run);
        }
        Some(RunClaim::Refused(why)) => {
            end_claimed(hub, group);
            settled(run);
            let core = pull.core.clone();
            wake::spawn(move || run_door::free(&core, id, &why));
            return;
        }
    };
    // The core's turn is the record from here: nothing of the host's keeps it.
    settled(run);
    let w = Arc::new(ChannelWake {
        run: run.to_owned(),
        turn: id,
        claim,
        wk: wk.to_owned(),
        cards: group.iter().map(|c| c.id.clone()).collect(),
        ids: turn.ids.clone(),
        settled: AtomicBool::new(false),
        refused: AtomicBool::new(false),
        sid: Mutex::new(None),
    });
    match crate::channels::wake_claimed(wk, note, w) {
        Wake::Queued | Wake::Claimed => clear_held(hub, group),
        Wake::NoThread => {
            end_claimed(hub, group);
            let core = pull.core.clone();
            wake::spawn(move || run_door::free(&core, id, "the thread is gone"));
        }
        // The connection isn't up (yet): the claim goes back and the turn is pulled again.
        Wake::Offline => {
            requeue(pull.core.clone(), claim, "the connection is not up");
            later(run);
        }
    }
}

/// The turn stays queued; it is begun again after the pause.
fn later(run: &str) {
    settled(run);
    super::wake_clock::later(run);
}

/// Gives a claim with no process back, asking again until the core answers.
fn requeue(core: Arc<CoreWriter>, claim: i64, why: &'static str) {
    std::thread::spawn(move || {
        run_door::until_told("requeue", claim, &Pace::LIVE, || core.stop_requeue(claim, why));
    });
}

/// The run that carries the wake has its process up. The cards are not applied yet: that is the attach's (`attempt_over`), because a
/// launch that never attaches gives the turn back and the next pull must still find its cards wanted.
pub fn started(hub: &Hub, w: &ChannelWake) {
    clear_held(hub, &cards_of(hub, &w.cards));
}

fn attached_by(row: &Value, pid: u32) -> bool {
    row["pid"].as_u64() == Some(u64::from(pid))
}

/// After an attempt: reads the claim and reports the end of the process when this attempt attached it. `true` when no later
/// attempt should carry the claim (it attached, or it is ended or taken).
pub async fn attempt_over(hub: &Arc<Hub>, w: &ChannelWake, seen: &Seen) -> bool {
    let Some(pid) = seen.pid else { return false };
    let Some(core) = writer(hub).await else {
        tracing::warn!(run = %w.run, "channel wake: this host is not the claims writer any more; the claim is left for the core's own end");
        return w.settle();
    };
    // Asked until the core answers: a claim nobody settled stays the host's while it lives, so a guess here would hold the
    // thread's next wake until a restart.
    let row = loop {
        let (c, claim) = (core.clone(), w.claim);
        match wake::blocking(move || c.claim_row_said(claim)).await {
            Some(RowSaid::Row(r)) => break r,
            // The core ran its rules and will not say: asking again would hold the thread's worker for ever.
            Some(RowSaid::Refused(why)) => {
                tracing::warn!(run = %w.run, why, "channel wake: the core refused to say whether the launch attached");
                return w.settle();
            }
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };
    let state = row.as_ref().and_then(|r| r["state"].as_str()).unwrap_or("none");
    match (&row, state) {
        (Some(r), "claimed") if r["pid"].is_null() => {
            w.refused.store(seen.code == Some(10), Ordering::Release);
            false
        }
        (Some(r), "running" | "ending") if attached_by(r, pid) => {
            applied(hub, &w.cards);
            let clean = seen.code == Some(0);
            let why =
                format!("the channel run ended with exit {}", seen.code.map_or("signal".into(), |c| c.to_string()));
            let (claim, turn) = (w.claim, w.turn);
            std::thread::spawn(move || {
                run_door::until_told("report", turn, &Pace::LIVE, || core.run_over(claim, pid, clean, &why));
            });
            w.settle()
        }
        // Ended, cancelled, another process's: nothing to report.
        _ => w.settle(),
    }
}

impl ChannelWake {
    fn settle(&self) -> bool {
        self.settled.store(true, Ordering::Release);
        true
    }
}

/// The run is over and no attempt attached the claim: after a Stop the wake is withdrawn; a session a writer holds requeues the
/// turn (no failed try, the card shows the holder); anything else frees it (cancelled, never redeemed: a failed try).
pub async fn unattached(hub: &Arc<Hub>, w: &ChannelWake, stopped: bool) {
    if w.settled() {
        return;
    }
    w.settle();
    lock(&TAKEN).remove(&w.run);
    let state = super::state_of(hub);
    if stopped {
        let (state, wk, ids) = (state, w.wk.clone(), w.ids.clone());
        wake::spawn(move || {
            for id in &ids {
                wake::withdraw(&state, &wk, Some(id));
            }
        });
        return;
    }
    // The claim goes back (or is freed) and the core's turn is the one record of the wake: the note leaves the inbox too, or a
    // correction's redo would send it with no claim and the turn would send it again.
    crate::channels::forget_wake(w);
    let Some(core) = writer(hub).await else { return };
    let sid = w.sid();
    let st = state.clone();
    let held = wake::blocking(move || sid.map(|sid| (Reader::new(&st).read(&sid), sid))).await.flatten();
    if let Some((Reading::By(h, _), sid)) = held {
        for card in cards_of(hub, &w.cards) {
            set_held(hub, &card, Some((&sid, &h)));
        }
        requeue(core, w.claim, "a writer holds the session");
        super::wake_clock::later(&w.run);
        return;
    }
    // The launch refused the claim (exit 10): a launch too old for `KITH_TURN_CONVERSATION` attaches with the session's id and the
    // core refuses the mismatch. The wake is not dropped: the claim goes back and the turn waits for the program to be updated.
    if w.refused.load(Ordering::Acquire) {
        requeue(core, w.claim, "the launch refused the claim");
        let tries = {
            let mut skew = lock(&SKEW);
            let n = skew.entry(w.run.clone()).or_insert(0);
            *n += 1;
            *n
        };
        if tries == 1 {
            tracing::warn!(run = %w.run, "channel wake: the launch refused the claim (an older kith program?): the wake waits; update kith");
        }
        super::wake_clock::later_extra(&w.run, Duration::from_secs(15u64 << tries.min(5)));
        return;
    }
    let (turn, why) = (w.turn, "the channel run did not start the wake");
    wake::spawn(move || run_door::free(&core, turn, why));
}

/// A Stop or a revoke dropped a wake batch the thread had not run: its requests are withdrawn (which cancels the unredeemed turn).
pub fn dropped(hub: &Hub, w: &ChannelWake) {
    w.settle();
    let (state, wk, ids) = (super::state_of(hub), w.wk.clone(), w.ids.clone());
    wake::spawn(move || {
        for id in &ids {
            wake::withdraw(&state, &wk, Some(id));
        }
    });
}

async fn writer(hub: &Arc<Hub>) -> Option<Arc<CoreWriter>> {
    let hub = hub.clone();
    wake::blocking(move || crate::agent::bot::writer_core(&hub)).await.flatten()
}

// Unix-only: the scripted core and the claim reads (writer mode) are Unix.
#[cfg(all(test, unix))]
#[path = "channel_wake_tests.rs"]
mod tests;
