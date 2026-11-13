//! The host's writer claims through the core (M3-10f), behind `KITH_HOST_CORE_CLAIMS=1` (off by default, unswitched: the
//! claim files of `claim.rs` stay the writer authority until the S2 switch).
//!
//! With the flag on, for a state folder that [`enable`] registered, `claim::take_as`, `claim::holder` and `claim::release`,
//! the terminal take-over (`sync::take_over`) and the bot turn claim (`agent/bot/turn.rs`) also write the core's `hostclaim.*`
//! rows (`core/runtime/src/hostclaim.rs`, host principal only). The core's `conversation` is the Claude session id the claim
//! files are named by.
//!
//! **The core claim is a shadow of the claim file.** `claim::take_for_turn` takes `<sid>.claim` first, under its lock (the
//! one writer the kernel's takers see: `kith-sessions take_claim`, kith-lifecycle's compact claim and kith-wake never ask the
//! core), and only then makes the core claim. A core claim that is refused as held gives the file back and reports `Held`.
//! `claim::release` gives the core claim back first, then the file. So flipping the flag with a live claim, or a
//! `kith --resume` during a host turn, never gives two writers: the file is the gate for both, and the core's claim only
//! makes the turn visible to the core (for its own claims, wake and the M3 import).
//!
//! | host kind | core claim |
//! |---|---|
//! | `Terminal`, `Oneshot` | `external` claim on the claimer's pid and start stamp |
//! | `App`, `Bot` | a `turn` claim made by `claim_next` (the turn is queued first, claimed, redeemed on the agent's pid) |
//!
//! A turn the host holds for a person other than the owner reads back as a `Bot` holder; the owner's reads back as nothing
//! (it only keeps a terminal out, as `Kind::App` does), so `holder` answers what it answers with the files.
//!
//! When the core claim cannot be made, the file claim is still the writer, so the turn is not left without one:
//! - **`revoked`** (the person's revoke epoch moved): the file claim is given back and the turn is refused ([`Revoked`]).
//! - **Every other failure** (core down or lost, `waiting`, `notWaiting`, `other`, `exhausted`, a failed redeem): the turn
//!   goes on under the file claim alone, as it does with the flag off, and the failure is logged. Its turn row is cancelled.
//!
//! Rules this keeps (the writer-lock models `Claims4.tla`, `results4.md`):
//! - **One writer.** The file claim is taken first, so a live claim (written while the flag was off, or by the kernel)
//!   refuses the turn as `Held` before the core is asked, and a core claim that holds refuses it the same way. Nothing is
//!   imported; the run finishes under the claim it began with.
//! - **A take-over waits for `ended`** before the host writes: the take-over file claim is taken first (so the
//!   `holder()` checks of `cards/resume.rs` and a second take-over see it), the core claim is stopped (`ending`, still the
//!   writer), the terminal is signalled, and only a claim the core shows `ended` (the process confirmed gone) lets the host
//!   go on.
//! - **A claim of ours whose end the core never heard is ended, not read as another writer.** Ours is what the core stores:
//!   every claim the host makes names [`HOST_INSTANCE`] (`holderInstance` in `hostclaim.show`), so it holds across a host
//!   restart or crash, when nothing is left in memory. If `release` failed or the host died, the claim stays live
//!   (`running` on the warm agent's pid, or `ending`); the next take, which holds the claim file under its lock (so no live
//!   turn of this host can own it), finds the core claim held, sees its instance is ours, reports it `done` (`exit` for an
//!   external claim) and makes the claim again, once. A claim of another instance, or the kernel's (no instance), still
//!   refuses, on any pid. Two takes after a restart are serialized by the file lock: the second never reaches the core while
//!   the first holds the file. The in-memory list only names a claim of ours that shows no instance.
//! - **A refused turn is cancelled by the caller.** The turn row is the claim's vehicle; whatever refuses its claim
//!   (held, revoked, a stale epoch, a failed redeem, no account left), the adapter cancels it with `hostclaim.cancel`
//!   instead of leaving it queued. (A stopped ask's turn the core cancels itself.) A redeem whose reply was lost is checked
//!   with `show` first: a claim that is `running` on the agent's pid stands.
//! - **Every turn claim is `claim_next`'s**: `hostclaim.turn` is the only way, and it picks the account (the host ignores it).
//! - **Release reports what the claim is**: `done` for a turn claim (a host turn's normal end; it never requeues, the host
//!   retries on its own), `exit` for an external claim. A claim whose process is gone is ended by `hostclaim.settle`
//!   (a crashed holder), never by the host guessing.
//!
//! Open, and not new in this module (the models and the code differ here today):
//! - **Revoke epoch order (Claims4 MayCheck/TurnArrive).** The epoch is read in [`take_as`], after the host's own gate, not at
//!   the gate and carried through. Until S2 the host's gate (and `revoke_person`'s sweep of the holders) is the fence.
//! - **K2.** "Gone" means the ACP adapter's pid is dead (`settle` calls `end_gone`), but the transcript is written by its
//!   `claude` child; gone should mean the process group is empty. Open: fix before M3-5.
//! - **The holder's hello (done).** The turn claim names [`HOST_INSTANCE`], which is never the holder's. The core's hello
//!   (`holder_link::reconcile`) runs `different_holder`, which ends the unlisted claims of an old holder instance but skips
//!   any claim naming `HOST_INSTANCE` (`release_unlisted`), so a holder hello during a host turn leaves the claim and its
//!   warm agent process alone; a holder that says hello under that name is refused. The core's own restart (step 5,
//!   `claims::reconcile`) still reconciles the host's claims: a new generation makes them stale.
//! - **Writer lock.** Host turns take no writer lock, but `WriterLock4`'s P1 rests on the stub holding one.
//! - **Park.** The host never calls `park`/`unpark`.
//! - **Kill idle.** The warm agent process is kept after a turn, though P7/killidle decided to kill it first.
//! - **The session row.** `claim_next` writes the turn's session row with the cwd and agent the host passes; a non-hq
//!   bot has no agent name and the conversation's agent stays `"kith"` (`INSERT OR IGNORE` keeps the first).

use super::claim::{Claim, Held, Kind};
use super::registry::{pid_runs, proc_start};
use crate::core_front::{Answer, call_host_claims};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The env switch. Anything but `1` is off.
pub const FLAG: &str = "KITH_HOST_CORE_CLAIMS";

/// The holder instance every host turn claim is redeemed under: one name for the host, not one per process.
pub const HOST_INSTANCE: &str = "kith-serve-host";

/// The core refused a claim because the person was revoked after the caller's check: the turn must not run.
#[derive(Debug)]
pub struct Revoked(pub String);

impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "core claims: revoked: {}", self.0)
    }
}

impl std::error::Error for Revoked {}

/// Whether `e` is a [`Revoked`] refusal.
pub fn is_revoked(e: &std::io::Error) -> bool {
    e.get_ref().is_some_and(<dyn std::error::Error + Send + Sync>::is::<Revoked>)
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Paths>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Paths>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// The core claim each (state folder, session) of this process holds, until its end report went through. A release the core
/// never heard (down, lost reply) leaves its claim `running` on the warm agent's pid; the next take of the session finds it
/// here and ends it, instead of reading it as another writer.
fn mine() -> &'static Mutex<HashMap<(PathBuf, String), i64>> {
    static M: OnceLock<Mutex<HashMap<(PathBuf, String), i64>>> = OnceLock::new();
    M.get_or_init(Mutex::default)
}

fn remember(f: &Paths, sid: &str, id: i64) {
    mine().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((f.state.clone(), sid.to_owned()), id);
}

fn forget(f: &Paths, sid: &str) {
    mine().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(f.state.clone(), sid.to_owned()));
}

fn is_mine(f: &Paths, sid: &str, id: i64) -> bool {
    mine().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(f.state.clone(), sid.to_owned()))
        == Some(&id)
}

/// Whether the process was started with the flag on.
pub fn flag_on() -> bool {
    std::env::var(FLAG).is_ok_and(|v| v == "1")
}

/// Registers `state` as a state folder whose claims go through the core at `folders`. Called once at startup when the flag
/// is on (and by tests).
pub fn enable(state: &Path, folders: Paths) {
    registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(state.to_path_buf(), folders);
}

/// The core folders for `state`, when its claims go through the core.
pub fn folders_for(state: &Path) -> Option<Paths> {
    registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(state).cloned()
}

fn failed(what: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("core claims: {what}"))
}

/// One host row. A refusal and a core that is not there are both errors here; the rows that refuse as data answer `Ok`.
fn row(f: &Paths, method: &str, params: &Value) -> std::io::Result<Value> {
    #[cfg(test)]
    let answer = match tests::fake_for(f) {
        Some(fake) => fake(method, params),
        None => call_host_claims(f, method, params),
    };
    #[cfg(not(test))]
    let answer = call_host_claims(f, method, params);
    match answer {
        Answer::Done(v) => Ok(v),
        Answer::Refused(said) => Err(failed(said)),
        Answer::Down { why, .. } => Err(failed(why)),
    }
}

fn owner_or(person: Option<&str>) -> &str {
    person.filter(|p| !p.is_empty()).unwrap_or("owner")
}

/// A claim the core shows, as the host's [`Claim`].
fn claim_of(v: &Value) -> Claim {
    let person = v["person"].as_str().filter(|p| !p.is_empty() && *p != "owner").map(str::to_owned);
    let kind = match v["kind"].as_str() {
        Some("terminal") => Kind::Terminal,
        Some("external") => Kind::Oneshot,
        // A turn: a person's reads as a bot guest's holder; the owner's is the host's own (App).
        _ if person.is_some() => Kind::Bot,
        _ => Kind::App,
    };
    Claim {
        // A claim not redeemed yet has no pid: this process is the one starting it.
        pid: v["pid"].as_u64().and_then(|p| u32::try_from(p).ok()).unwrap_or_else(std::process::id),
        proc_start: v["started"].as_str().map(str::to_owned),
        at: String::new(),
        kind,
        takeover: false,
        person,
        agent: None,
    }
}

/// The core's live claim on `sid` (claimed, running or ending), as the row shows it.
fn live(f: &Paths, sid: &str) -> std::io::Result<Option<Value>> {
    let shown = row(f, "hostclaim.show", &json!({"conversation": sid}))?;
    Ok(Some(shown["claim"].clone()).filter(Value::is_object))
}

/// [`live`] after `settle`: a holder that crashed with its claim still `running` is ended by the core once its pid is
/// confirmed gone, so it does not keep the session for ever.
fn live_settled(f: &Paths, sid: &str) -> std::io::Result<Option<Value>> {
    let Some(c) = live(f, sid)? else { return Ok(None) };
    let gone = c["pid"].as_u64().and_then(|p| u32::try_from(p).ok()).is_some_and(|p| !pid_runs(p));
    if !gone {
        return Ok(Some(c));
    }
    row(f, "hostclaim.settle", &json!({}))?;
    live(f, sid)
}

/// What the core's claim means for [`super::claim::holder`]: a terminal's or another writer's claim, not the owner's own turn.
pub(super) fn holder(f: &Paths, sid: &str) -> Option<Claim> {
    let c = live_settled(f, sid).ok().flatten()?;
    Some(claim_of(&c)).filter(|c| c.kind != Kind::App)
}

/// The core claim that shadows the file claim `claim::take_for_turn` holds. `cwd` is the run's folder for a turn's session row.
#[allow(clippy::too_many_arguments, reason = "take_as's inputs and the folder")]
pub(super) fn take_as(
    f: &Paths,
    sid: &str,
    kind: Kind,
    pid: u32,
    person: Option<&str>,
    agent: Option<&str>,
    cwd: Option<&str>,
) -> std::io::Result<Result<(), Held>> {
    if !super::claim::safe_session(sid) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a session id"));
    }
    let started = proc_start(pid).ok_or_else(|| failed("the claimer's start time can't be read"))?;
    let person = owner_or(person);
    let agent = agent.unwrap_or("kith");
    row(f, "hostclaim.ensure", &json!({"conversation": sid, "agent": agent, "engine": "claude"}))?;
    let mut reclaimed = false;
    for attempt in 0..3 {
        let seen = row(f, "hostclaim.epoch", &json!({"person": person}))?["epoch"].clone();
        let refused = match kind {
            Kind::Terminal | Kind::Oneshot => {
                let v = row(
                    f,
                    "hostclaim.claim",
                    &json!({"conversation": sid, "kind": "external", "person": person, "revokeSeen": seen,
                            "pid": pid, "started": started, "holderInstance": HOST_INSTANCE}),
                )?;
                if let Some(id) = v["claimId"].as_i64() {
                    remember(f, sid, id);
                    return Ok(Ok(()));
                }
                v
            }
            Kind::App | Kind::Bot => match turn_claim(f, sid, kind, pid, &started, person, agent, cwd, &seen)? {
                Ok(id) => {
                    remember(f, sid, id);
                    return Ok(Ok(()));
                }
                Err(refused) => refused,
            },
            Kind::Starting | Kind::Other => return Err(failed("this kind of claim is the core's own")),
        };
        if refused["refused"] == "held" {
            // A holder that crashed with its claim still live is ended by the core (its pid confirmed gone); once.
            if attempt == 0 && live_settled(f, sid)?.is_none() {
                continue;
            }
            let shown = live(f, sid)?;
            // Our own claim whose end report never reached the core (the file claim is ours, taken under its lock, so no live
            // turn of this host can own it): ended as `release` would have, and the claim made again, once. "Ours" is what the
            // core stores (the holder instance this host redeems under), so it holds across a host restart, when the list in
            // memory is empty; the list still names a claim of ours that shows no instance. Any other core claim (another
            // instance's, the kernel's) still refuses, on any pid: the core is not guessed at.
            if let Some(c) = &shown
                && !reclaimed
                && (c["holderInstance"] == HOST_INSTANCE || c["claimId"].as_i64().is_some_and(|id| is_mine(f, sid, id)))
            {
                tracing::warn!(
                    sid,
                    "ending a claim of ours that was never reported: the last turn's end didn't reach the core"
                );
                if end_report(f, sid, c).is_ok() {
                    reclaimed = true;
                    continue;
                }
            }
            let c = shown.map_or_else(
                || Claim {
                    pid: 0,
                    proc_start: None,
                    at: String::new(),
                    kind,
                    takeover: false,
                    person: None,
                    agent: None,
                },
                |c| claim_of(&c),
            );
            return Ok(Err(Held(c)));
        }
        let why = refused["why"].as_str().unwrap_or("refused");
        if refused["refused"] == "revoked" {
            return Err(std::io::Error::other(Revoked(why.to_owned())));
        }
        return Err(failed(why));
    }
    Err(failed("held and not settled"))
}

/// A turn's claim: queue the turn, claim it through `claim_next`, redeem it on `pid`. `Err(refused)` is the core's refusal
/// as data, after the turn was cancelled.
#[allow(clippy::too_many_arguments, reason = "one claim's inputs")]
fn turn_claim(
    f: &Paths,
    sid: &str,
    kind: Kind,
    pid: u32,
    started: &str,
    person: &str,
    agent: &str,
    cwd: Option<&str>,
    seen: &Value,
) -> std::io::Result<Result<i64, Value>> {
    let source = if kind == Kind::Bot { "bot" } else { "app" };
    let q = row(
        f,
        "hostclaim.enqueue",
        &json!({"conversation": sid, "agent": agent, "engine": "claude", "source": source, "person": person,
                "prompt": "", "revokeSeen": seen}),
    )?;
    let Some(turn) = q["turn"].as_i64() else { return Ok(Err(q)) };
    let holder_instance = HOST_INSTANCE;
    let cancel = |why: &str| {
        // A refused turn is not left queued for a drain to retry for ever.
        let _ = row(f, "hostclaim.cancel", &json!({"turn": turn, "reason": why}));
    };
    let mut ask = json!({"conversation": sid, "turn": turn, "person": person, "revokeSeen": seen,
                         "cwd": cwd.unwrap_or(""), "sessionId": sid, "holderInstance": holder_instance});
    for (k, v) in crate::core_front::caller_env() {
        ask["env"][k] = v;
    }
    let got = match row(f, "hostclaim.turn", &ask) {
        Ok(v) => v,
        Err(e) => {
            cancel("claim failed");
            return Err(e);
        }
    };
    if got["status"] == "exhausted" {
        cancel("no account left");
        return Err(failed(got["text"].as_str().unwrap_or("no account left")));
    }
    let Some(id) = got["claimId"].as_i64() else {
        cancel(got["refused"].as_str().unwrap_or("refused"));
        return Ok(Err(got));
    };
    let red = match row(
        f,
        "hostclaim.redeem",
        &json!({"claimId": id, "pid": pid, "started": started, "holderInstance": holder_instance}),
    ) {
        Ok(v) => v,
        Err(e) => {
            // The reply may have been lost after the core redeemed: a claim that is running on this pid stands.
            let shown = row(f, "hostclaim.show", &json!({"claimId": id})).ok();
            if shown
                .is_some_and(|s| s["claim"]["state"] == "running" && s["claim"]["pid"].as_u64() == Some(u64::from(pid)))
            {
                return Ok(Ok(id));
            }
            // A claimed claim with no pid is never settled (gone needs a pid): cancelling the turn ends it.
            cancel("redeem failed");
            return Err(e);
        }
    };
    if red["running"] == true {
        return Ok(Ok(id));
    }
    // Not redeemable (the claim was revoked or ended in between): the claim goes and so does the turn.
    let _ = row(f, "hostclaim.stop", &json!({"claimId": id, "reason": "not redeemed"}));
    cancel("not redeemed");
    Err(failed(format!("the claim could not be redeemed: {red}")))
}

/// `claim::release` through the core: the end report of the claim that names `pid`. A claim of another pid is left alone.
pub(super) fn release(f: &Paths, sid: &str, pid: u32) {
    let Ok(Some(c)) = live(f, sid) else { return };
    if c["pid"].as_u64() != Some(u64::from(pid)) {
        return;
    }
    if let Err(error) = end_report(f, sid, &c) {
        tracing::warn!(%error, sid, "couldn't report a claim's end to the core");
    }
}

/// The end report of the live claim `c`. A host turn ends `done` (the core never requeues it: the host retries on its own);
/// `exit` is an external claim's end. The claim stays remembered as ours until the report went through.
fn end_report(f: &Paths, sid: &str, c: &Value) -> std::io::Result<()> {
    let Some(id) = c["claimId"].as_i64() else { return Ok(()) };
    let kind = if c["kind"] == "turn" { "done" } else { "exit" };
    row(f, "hostclaim.report", &json!({"claimId": id, "kind": kind, "body": json!({"pid": c["pid"]}).to_string()}))?;
    forget(f, sid);
    Ok(())
}

/// The core's terminal or external claim on `sid` that a take-over would stop, if any.
pub(super) fn terminal_claim(f: &Paths, sid: &str) -> Option<i64> {
    let c = live_settled(f, sid).ok().flatten()?;
    matches!(c["kind"].as_str(), Some("terminal" | "external")).then(|| c["claimId"].as_i64()).flatten()
}

/// What a core take-over came to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Taken {
    /// The claim is `ended`: the host may write.
    Ended,
    /// The claim is still `ending` after the wait (its process did not leave): nothing is promoted.
    StillEnding,
}

/// Stops the core claim `id` for a take-over (it goes to `ending`, still the writer) and waits, up to `wait`, for the core to
/// show it `ended`. The caller has signalled the claimer already (an idle terminal, checked just before), so the claim only
/// leaves `ending` once the core confirms the process gone (`settle` each round) or the holder reports its exit. Blocking.
pub(super) fn take_over(f: &Paths, id: i64, wait: std::time::Duration) -> std::io::Result<Taken> {
    row(f, "hostclaim.stop", &json!({"claimId": id, "reason": "taken over in the app"}))?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        row(f, "hostclaim.settle", &json!({}))?;
        let shown = row(f, "hostclaim.show", &json!({"claimId": id}))?;
        match shown["claim"]["state"].as_str() {
            Some("ended") | None => return Ok(Taken::Ended),
            Some(_) if std::time::Instant::now() >= deadline => return Ok(Taken::StillEnding),
            Some(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
#[path = "core_claims_tests.rs"]
pub(super) mod tests;
