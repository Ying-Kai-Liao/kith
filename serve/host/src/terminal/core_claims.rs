//! The host's writer claims through the core (M3-10f), behind `KITH_HOST_CORE_CLAIMS=1` (off by default, unswitched: the
//! claim files of `claim.rs` stay the writer authority until the S2 switch).
//!
//! With the flag on, for a state folder that [`enable`] registered, `claim::take_as`, `claim::holder` and `claim::release`,
//! the terminal take-over (`sync::take_over`) and the bot turn claim (`agent/bot/turn.rs`) go through the core's `hostclaim.*`
//! rows (`core/runtime/src/hostclaim.rs`, host principal only) instead of writing `<sid>.claim`. The core's `conversation`
//! is the Claude session id the claim files are named by.
//!
//! | host kind | core claim |
//! |---|---|
//! | `Terminal`, `Oneshot` | `external` claim on the claimer's pid and start stamp |
//! | `App`, `Bot` | a `turn` claim made by `claim_next` (the turn is queued first, claimed, redeemed on the agent's pid) |
//!
//! A turn the host holds for a person other than the owner reads back as a `Bot` holder; the owner's reads back as nothing
//! (it only keeps a terminal out, as `Kind::App` does), so `holder` answers what it answers with the files.
//!
//! Rules this keeps (the writer-lock models `Claims4.tla`, `results4.md`):
//! - **One writer.** A live claim file for the session (a claim written while the flag was off, or by the kernel) refuses the
//!   core claim as `Held`, and a core claim that holds refuses a claim file's taker the same way: flipping the flag with a
//!   live claim never gives two writers. Nothing is imported; the run finishes under the claim it began with.
//! - **A take-over waits for `ended`** before the host writes: the core claim is stopped (`ending`, still the writer), the
//!   terminal is signalled, and only a claim the core shows `ended` (the process confirmed gone) lets the host go on.
//! - **A refused turn is cancelled by the caller.** The turn row is the claim's vehicle; whatever refuses its claim
//!   (held, revoked, a stale epoch), the adapter cancels it with `hostclaim.cancel` instead of leaving it queued. (A stopped
//!   ask's turn the core cancels itself.)
//! - **Every turn claim is `claim_next`'s**: `hostclaim.turn` is the only way, and it picks the account.
//! - **Release is the holder's exit report** for the claim that names its pid; a claim whose process is gone is ended by
//!   `hostclaim.settle` (a crashed holder), never by the host guessing.

use super::claim::{Claim, Held, Kind};
use super::registry::{pid_runs, proc_start};
use crate::core_front::{Answer, call_host};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The env switch. Anything but `1` is off.
pub const FLAG: &str = "KITH_HOST_CORE_CLAIMS";

fn registry() -> &'static Mutex<HashMap<PathBuf, Paths>> {
    static R: OnceLock<Mutex<HashMap<PathBuf, Paths>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
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
        None => call_host(f, method, params),
    };
    #[cfg(not(test))]
    let answer = call_host(f, method, params);
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
    Ok(Some(shown["claim"].clone()).filter(|c| c.is_object()))
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

/// A live claim file for `sid` (either name, any kind): what refuses a core claim while the flag was off when it was taken.
pub(super) fn legacy_holder(state: &Path, sid: &str) -> Option<Claim> {
    [false, true]
        .into_iter()
        .filter_map(|t| super::claim::read(&super::claim::path(state, sid, t)))
        .find(Claim::is_live)
}

/// What the core's claim means for [`super::claim::holder`]: a terminal's or another writer's claim, not the owner's own turn.
pub(super) fn holder(f: &Paths, sid: &str) -> Option<Claim> {
    let c = live_settled(f, sid).ok().flatten()?;
    Some(claim_of(&c)).filter(|c| c.kind != Kind::App)
}

/// `claim::take_as` through the core. `cwd` is the run's folder for a turn's session row.
pub(super) fn take_as(
    f: &Paths,
    state: &Path,
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
    if let Some(old) = legacy_holder(state, sid) {
        // Taken before the flag was on: it holds until its holder is done. No import, no second writer.
        return Ok(Err(Held(old)));
    }
    let started = proc_start(pid).ok_or_else(|| failed("the claimer's start time can't be read"))?;
    let person = owner_or(person);
    let agent = agent.unwrap_or("kith");
    row(f, "hostclaim.ensure", &json!({"conversation": sid, "agent": agent, "engine": "claude"}))?;
    for attempt in 0..2 {
        let seen = row(f, "hostclaim.epoch", &json!({"person": person}))?["epoch"].clone();
        let refused = match kind {
            Kind::Terminal | Kind::Oneshot => {
                let v = row(
                    f,
                    "hostclaim.claim",
                    &json!({"conversation": sid, "kind": "external", "person": person, "revokeSeen": seen,
                            "pid": pid, "started": started}),
                )?;
                if v["claimId"].is_i64() {
                    return Ok(Ok(()));
                }
                v
            }
            Kind::App | Kind::Bot => match turn_claim(f, sid, kind, pid, &started, person, agent, cwd, &seen)? {
                Ok(()) => return Ok(Ok(())),
                Err(refused) => refused,
            },
            Kind::Starting | Kind::Other => return Err(failed("this kind of claim is the core's own")),
        };
        if refused["refused"] == "held" {
            // A holder that crashed with its claim still live is ended by the core (its pid confirmed gone); once.
            if attempt == 0 && live_settled(f, sid)?.is_none() {
                continue;
            }
            let c = live(f, sid)?.map_or_else(
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
        return Err(failed(refused["why"].as_str().unwrap_or("refused")));
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
) -> std::io::Result<Result<(), Value>> {
    let source = if kind == Kind::Bot { "bot" } else { "app" };
    let q = row(
        f,
        "hostclaim.enqueue",
        &json!({"conversation": sid, "agent": agent, "engine": "claude", "source": source, "person": person,
                "prompt": "", "revokeSeen": seen}),
    )?;
    let Some(turn) = q["turn"].as_i64() else { return Ok(Err(q)) };
    let holder_instance = format!("host-{}", std::process::id());
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
        return Err(failed(got["text"].as_str().unwrap_or("no account left")));
    }
    let Some(id) = got["claimId"].as_i64() else {
        cancel(got["refused"].as_str().unwrap_or("refused"));
        return Ok(Err(got));
    };
    let red = row(
        f,
        "hostclaim.redeem",
        &json!({"claimId": id, "pid": pid, "started": started, "holderInstance": holder_instance}),
    )?;
    if red["running"] == true {
        return Ok(Ok(()));
    }
    // Not redeemable (the claim was revoked or ended in between): the claim goes and so does the turn.
    let _ = row(f, "hostclaim.stop", &json!({"claimId": id, "reason": "not redeemed"}));
    cancel("not redeemed");
    Err(failed(format!("the claim could not be redeemed: {red}")))
}

/// `claim::release` through the core: the exit report of the claim that names `pid`. A claim of another pid is left alone.
pub(super) fn release(f: &Paths, sid: &str, pid: u32) {
    let Ok(Some(c)) = live(f, sid) else { return };
    if c["pid"].as_u64() != Some(u64::from(pid)) {
        return;
    }
    let Some(id) = c["claimId"].as_i64() else { return };
    if let Err(error) =
        row(f, "hostclaim.report", &json!({"claimId": id, "kind": "exit", "body": json!({"pid": pid}).to_string()}))
    {
        tracing::warn!(%error, sid, "couldn't report a claim's exit to the core");
    }
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
