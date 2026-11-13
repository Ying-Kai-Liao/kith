//! The host's Writer mode (designs/2026-11-14-m3-cutover sections 1.2 to 1.4): each warm turn runs on a core claim plus the
//! per-turn writer lock, which a resident `kith-stub` holds for the host. Machinery only: nothing calls it until the wiring
//! package (P7), and nothing turns it on.
//!
//! **The mode** ([`Mode`]) is picked once at host start. `Files` is today's path and the default. `Shadow` is
//! `KITH_HOST_CORE_CLAIMS=1` (`core_claims`). `Writer` only when the core's `claims` module reads switched (its switch file
//! plus the marker, through `cutover.mayServe`); no env var turns it on. A Writer host whose core does not answer waits
//! ([`Begin::CoreDown`]); it never runs a turn unclaimed.
//!
//! **One turn** ([`CoreWriter::begin`]), each step one call:
//! 1. `hostclaim.enqueue` (refused as revoked: dropped), 2. `hostclaim.turn` (`claim_next`, the only way to claim a turn; the
//!    answers are the table in [`Begin`]), 3. the warm process follows the account the core picked, 4. the stub's `take`
//!    (`busy` reports `busy` and the turn parks `lock_busy`), 5. `hostclaim.redeem` on the stub's pid. The caller then syncs the
//!    transcript under the lock and prompts, and ends with [`CoreWriter::finish`].
//!
//! **Why the lock is the stub's.** A host that held the flock itself would free it in a crash while the engine still writes
//! (`WriterLock4` "holder holds warm locks itself"). The stub outlives the host, so the lock does too, and the next host start
//! ([`CoreWriter::adopt`]) ends what the old one left.
//!
//! Rules this keeps (Claims4, `.claude/skills/writer-lock`):
//! - **A refused turn is not leaked.** A revoked, exhausted or failed claim is cancelled by the caller (`c4-fencecancel`,
//!   `c4-epochcancel`); `held`, `waiting` and `busy` park or stay queued, unlike Shadow mode.
//! - **Never `take` after `release` for the same claim**, and never release a lock whose redeem is unknown: a redeem whose reply
//!   was lost is checked with `show` first, and a claim `running` on our stub's pid stands. A begin repeated for the same turn
//!   answers the same thing (the retried `take` is `locked` again and the redeem `again`).
//! - **`LockStuck`** (`c4-lockstuck-*`): a redeem the core refused whose lock cannot be given back stops the claim and kills the
//!   process group; a lock must not outlive a claim that is not running.
//! - **One host instance per start** ([`host_instance`]): the old one's claims are not "ours" (no stale writer read as ours).
//!
//! Blocking: every call is a core row or a stub line, so an async caller runs them on a blocking thread.
//!
//! Open: a `hostclaim.enqueue` whose reply was lost leaves a queued turn nobody knows the id of (no row lists turns); the next
//! begin enqueues again and the first one waits for its deadline. A `Down` is told apart from a refusal, so nothing else is
//! guessed.

// Nothing calls this until the wiring package (P7).
#![allow(dead_code, reason = "machinery with no caller yet")]

use super::core_claims::row_answer;
use super::registry::{pid_runs, proc_start};
use crate::core_front::Answer;
use kith_acp::{Release, Take};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};
use std::time::Duration;

/// The core module whose switch turns Writer mode on.
pub const MODULE: &str = "claims";

/// Every host instance name starts with this (`core_claims::HOST_INSTANCE`'s name; the core tells a host's by it).
const INSTANCE_PREFIX: &str = "kith-serve-host";

/// How many times a release is tried before the lock is called stuck (`give_lock_back`).
const RELEASE_TRIES: usize = 3;

/// How long a `waiting` (reconcile) answer is waited out before the next try.
pub const RECONCILE_RETRY: Duration = Duration::from_secs(1);

/// How the host's turns are claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// The claim files are the writer; no core claims.
    #[default]
    Files,
    /// The claim file is the writer and the core claim shadows it (`core_claims`).
    Shadow,
    /// The core claim and the stub's lock are the writer.
    Writer,
}

impl Mode {
    /// The mode for what the core's gate said. `said` is the answer to `cutover.mayServe {module: "claims"}`; `switch_file` is
    /// whether the module's switch file exists (the one thing readable when the core is down); `shadow_flag` is
    /// `KITH_HOST_CORE_CLAIMS=1`. Writer needs the gate's yes, or no answer at all with the switch file there (a switched Mac
    /// whose core is down waits, it does not fall back to files). A clear no, or no switch file, is never Writer.
    pub fn decide(said: &Answer, switch_file: bool, shadow_flag: bool) -> Self {
        let writer = match said {
            Answer::Done(v) => match (v.get("ok").and_then(Value::as_bool), v.get("warning").and_then(Value::as_str)) {
                (Some(true), _) => true,
                (Some(false), None) => false,
                _ => switch_file,
            },
            Answer::Refused(_) | Answer::Down { .. } => switch_file,
        };
        match (writer, shadow_flag) {
            (true, _) => Self::Writer,
            (false, true) => Self::Shadow,
            (false, false) => Self::Files,
        }
    }

    /// The mode of this host start: asks the core's gate once.
    pub fn select(folders: &Paths) -> Self {
        let said = row_answer(folders, "cutover.mayServe", &json!({"module": MODULE}));
        Self::decide(&said, crate::core_front::switch_file_exists(folders, MODULE), super::core_claims::flag_on())
    }
}

/// This start's holder instance: `kith-serve-host/<pid>-<start stamp>`. The core ends the claims and processes of every
/// other host instance at [`CoreWriter::adopt`].
pub fn host_instance() -> String {
    let pid = std::process::id();
    format!("{INSTANCE_PREFIX}/{pid}-{}", proc_start(pid).unwrap_or_else(|| "0".into()).replace(' ', "_"))
}

/// The warm process a turn runs on: its stub, its control channel and the account it is connected as. The wiring package
/// implements it over `kith_acp::spawn_resident`; the tests over a real stub.
pub trait WarmStub {
    /// The stub's pid (`Acp::pid`): the process group leader the lock outlives the host with.
    fn pid(&self) -> u32;
    /// The account the process is connected as, when the host knows it.
    fn account(&self) -> Option<String>;
    /// `take <key> <claim>` on the control channel. `Err`: no answer (the stub is gone or wedged).
    fn take(&self, key: &str, claim: i64) -> std::io::Result<Take>;
    /// `release <key> <claim>`.
    fn release(&self, key: &str, claim: i64) -> std::io::Result<Release>;
    /// Starts the process again (a new stub, new pid), on `account` when given. The old one is gone when this returns.
    fn restart(&mut self, account: Option<&str>) -> std::io::Result<()>;
    /// Kills the stub's process group (engine and children). The lock goes with the stub.
    fn kill_group(&self);
}

/// One turn to begin. `id` is set when the same turn begins again (a retry, an unpark): it is never enqueued twice.
#[derive(Clone, Debug)]
pub struct Turn<'a> {
    pub id: Option<i64>,
    /// The Claude session id: the conversation, and the stub's lock key.
    pub conversation: &'a str,
    pub agent: &'a str,
    pub engine: &'a str,
    /// `app`, `bot`, `wake`, `routine`, `group`...
    pub source: &'a str,
    /// The person the turn runs as; empty is the owner.
    pub person: &'a str,
    pub cwd: &'a str,
    /// `hostclaim.epoch` of the person, read at accept (before the host's may check).
    pub revoke_seen: i64,
    pub deadline: Option<i64>,
    /// The room fields of the enqueue (`basis`, `epochs`, `room`, `gen`, `level`, `joinEpoch`, `author`, `budget`...), a JSON
    /// object merged in as it is.
    pub room: Option<&'a Value>,
}

/// A turn that holds its claim and the lock: the caller syncs the transcript and prompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub turn: i64,
    pub claim: i64,
    /// The stub the claim was redeemed on.
    pub pid: u32,
    pub started: String,
    /// The account the core picked (`None` when this is a repeated begin and the pick is not known again).
    pub account: Option<String>,
    /// The core had redeemed it already (a repeated begin).
    pub again: bool,
}

/// Why a turn is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parked {
    /// A terminal or external claim writes: the item shows "waiting for your terminal session".
    TerminalLive,
    /// The stub's lock is held by something else (a `--direct` run): the claim ended `busy`.
    LockBusy,
}

/// Why a turn is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The person was revoked: "Access was revoked."
    Revoked,
    /// The turn was cancelled or expired.
    NotWaiting,
    /// The core had already cancelled it (the conversation's ask was stopped).
    Stopped,
    /// The chain's turn budget is spent.
    Budget,
    /// The core refused for another reason (`said`).
    Refused,
}

/// What [`CoreWriter::begin`] came to. The table of design 1.3 step 3:
///
/// | core answer | outcome |
/// |---|---|
/// | `claimed` | [`Self::Running`] |
/// | `held`, writer is a terminal or external claim | [`Self::Parked`] `TerminalLive` |
/// | `held`, another turn's claim | [`Self::Queued`]: retry on the next tick |
/// | `waiting` | [`Self::Retry`] after 1 s |
/// | `notWaiting` | [`Self::Dropped`] |
/// | `revoked` | turn cancelled, [`Self::Dropped`] |
/// | `stopped` | [`Self::Dropped`] (the core cancelled it) |
/// | `exhausted` | turn cancelled, [`Self::Exhausted`] |
#[derive(Debug, PartialEq, Eq)]
pub enum Begin {
    Running(Running),
    /// Another turn's claim holds the conversation: stay queued, begin again (same turn id) on the next tick.
    Queued {
        turn: i64,
    },
    Retry {
        turn: i64,
        after: Duration,
    },
    Parked {
        turn: i64,
        why: Parked,
    },
    Dropped {
        turn: Option<i64>,
        why: Dropped,
        said: String,
    },
    /// No account is left: post `text`, fail the item.
    Exhausted {
        text: String,
    },
    /// The core does not answer. Nothing runs; begin again (same turn id when there is one) when it does.
    CoreDown {
        turn: Option<i64>,
        why: String,
    },
    /// The turn could not start (`why`); the claim is ended and the turn put back or cancelled.
    Failed {
        turn: Option<i64>,
        why: String,
    },
    /// The lock could not be given back: the claim is stopped and the process group killed.
    LockStuck {
        why: String,
    },
}

/// How a turn ended, for [`CoreWriter::finish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The prompt finished (or the user stopped it through `session/cancel`): release, then `done`.
    Done,
    /// The account's usage ran out: release, then `exit {out_of_usage}`; the core requeues the turn.
    OutOfUsage,
    /// A room-level turn: stop the claim, kill the process group. Room processes are never warm.
    Room,
    /// The host killed the process (the 15 s stop): `exit`.
    Killed,
}

/// What [`CoreWriter::finish`] came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Finished {
    /// The lock is released and the end reported.
    Reported,
    /// The lock is released but the core did not take the report (`why`): the claim stays `running` and the next begin of the
    /// conversation ends it.
    ReportLost(String),
    /// A room turn's claim is stopped and its process group killed; `gone` says whether the process is confirmed gone and its
    /// `warm_procs` row forgotten.
    RoomEnded { gone: bool },
    /// The lock could not be released: the claim is stopped and the process group killed.
    LockStuck(String),
}

/// What [`CoreWriter::adopt`] ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Adopted {
    /// Claims ended at once (never redeemed).
    pub ended: Vec<i64>,
    /// Processes the core signals until they are gone (`kill_resend`).
    pub kill: Vec<i64>,
}

/// The host's writer over the core's `hostclaim.*` rows.
pub struct CoreWriter {
    folders: Paths,
    instance: String,
}

enum Step {
    Go(Begin),
    /// The stub failed: the claim was reported `exit`, the process restarted; claim again.
    Again,
}

fn failed(why: impl std::fmt::Display) -> Begin {
    Begin::Failed { turn: None, why: why.to_string() }
}

impl CoreWriter {
    /// A writer for this host start.
    pub fn new(folders: Paths) -> Self {
        Self::with_instance(folders, host_instance())
    }

    pub fn with_instance(folders: Paths, instance: String) -> Self {
        Self { folders, instance }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    fn call(&self, method: &str, params: &Value) -> Answer {
        row_answer(&self.folders, method, params)
    }

    /// A row that answers data: `Err(why)` when the core did not answer (down, or it refused the call itself).
    fn data(&self, method: &str, params: &Value) -> Result<Value, String> {
        match self.call(method, params) {
            Answer::Done(v) => Ok(v),
            Answer::Refused(said) => Err(said),
            Answer::Down { why, .. } => Err(why),
        }
    }

    /// `hostclaim.epoch` of a person, to be read at accept, before the host's may check, and carried on the queued item.
    pub fn epoch(&self, person: &str) -> std::io::Result<i64> {
        let v = self.data("hostclaim.epoch", &json!({"person": owner_or(person)})).map_err(std::io::Error::other)?;
        v["epoch"].as_i64().ok_or_else(|| std::io::Error::other("the core gave no epoch"))
    }

    /// The live claim on a conversation (`None`: none), or the reason the core could not say.
    fn live(&self, conversation: &str) -> Result<Option<Value>, String> {
        Ok(Some(self.data("hostclaim.show", &json!({"conversation": conversation}))?["claim"].clone())
            .filter(Value::is_object))
    }

    fn cancel(&self, turn: i64, why: &str) {
        let _ = self.call("hostclaim.cancel", &json!({"turn": turn, "reason": why}));
    }

    fn report(&self, claim: i64, kind: &str, pid: u32, extra: &Value) -> Result<(), String> {
        let mut body = json!({"out_of_usage": false, "pid": pid});
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        self.data("hostclaim.report", &json!({"claimId": claim, "kind": kind, "body": body.to_string()})).map(drop)
    }

    /// Begins `t` on the warm process `stub`: steps 2 to 6 of design 1.3. Call it again with the same turn id after
    /// [`Begin::Queued`], [`Begin::Retry`], [`Begin::CoreDown`] or an unpark.
    pub fn begin(&self, t: &Turn<'_>, stub: &mut dyn WarmStub) -> Begin {
        let mut turn = t.id;
        let mut claim = None;
        // What the core shows for the conversation: a claim of ours that a repeated begin resumes (the reply was lost, or
        // the redeem did not finish), or one an earlier turn never ended.
        match self.live(t.conversation) {
            Err(why) => return Begin::CoreDown { turn, why },
            Ok(Some(c)) if c["holderInstance"] == self.instance.as_str() && c["kind"] == "turn" => {
                let id = c["claimId"].as_i64();
                // Another process of this host that runs a turn on the conversation is a writer like any other: held.
                let same = c["pid"].is_null() || c["pid"].as_u64() == Some(u64::from(stub.pid()));
                match (id, c["state"].as_str()) {
                    (Some(id), Some("claimed" | "running")) if t.id.is_some() && same => claim = Some(id),
                    (Some(id), Some("claimed")) if same => {
                        let _ = self.call(
                            "hostclaim.stop",
                            &json!({"claimId": id, "reason": "a turn never started",
                                                                    "requeue": false}),
                        );
                    }
                    (Some(id), Some("running")) if same => {
                        // A claim of this process whose end the core never heard, begun again as a new turn: its lock is
                        // given back (a repeated release is `not-held`) and it ends as `finish` would have.
                        let _ = stub.release(t.conversation, id);
                        let _ = self.report(id, "done", stub.pid(), &json!({}));
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
        }
        for attempt in 0..2 {
            let step = self.attempt(t, stub, &mut turn, &mut claim, attempt == 0);
            match step {
                Step::Go(b) => return b,
                Step::Again => claim = None,
            }
        }
        Begin::Failed { turn, why: "the stub failed twice".into() }
    }

    fn attempt(
        &self,
        t: &Turn<'_>,
        stub: &mut dyn WarmStub,
        turn: &mut Option<i64>,
        claim: &mut Option<i64>,
        retry: bool,
    ) -> Step {
        let person = owner_or(t.person);
        if turn.is_none() {
            let mut ask = json!({"conversation": t.conversation, "agent": t.agent, "engine": t.engine, "source": t.source,
                                 "person": person, "prompt": "", "revokeSeen": t.revoke_seen});
            if let Some(d) = t.deadline {
                ask["deadline"] = json!(d);
            }
            if let (Some(a), Some(Value::Object(room))) = (ask.as_object_mut(), t.room) {
                a.extend(room.clone());
            }
            match self.call("hostclaim.enqueue", &ask) {
                Answer::Done(v) => match v["turn"].as_i64() {
                    Some(id) => *turn = Some(id),
                    None => return Step::Go(refusal(None, &v)),
                },
                Answer::Refused(said) => return Step::Go(failed(said)),
                Answer::Down { why, .. } => return Step::Go(Begin::CoreDown { turn: None, why }),
            }
        }
        let Some(turn_id) = *turn else { return Step::Go(failed("no turn")) };
        let (claim_id, account) = match *claim {
            Some(id) => (id, None),
            None => match self.claim_turn(t, turn_id, person) {
                Ok(got) => got,
                Err(b) => return Step::Go(b),
            },
        };
        // 4. The core picks the account and the host follows (a claim not redeemed has no pid: a stop ends it by CAS).
        if let Some(want) = &account
            && stub.account().as_ref() != Some(want)
            && let Err(e) = stub.restart(Some(want))
        {
            let _ = self.call(
                "hostclaim.stop",
                &json!({"claimId": claim_id, "reason": "account switch failed", "requeue": true}),
            );
            return Step::Go(Begin::Failed { turn: Some(turn_id), why: format!("restarting on {want}: {e}") });
        }
        // 5. The lock.
        let pid = stub.pid();
        match stub.take(t.conversation, claim_id) {
            Ok(Take::Locked) => {}
            Ok(Take::Busy) => {
                return Step::Go(match self.busy(claim_id, pid) {
                    Ok(()) => Begin::Parked { turn: turn_id, why: Parked::LockBusy },
                    Err(why) => Begin::CoreDown { turn: Some(turn_id), why },
                });
            }
            Ok(Take::Error(why)) => return self.stub_failed(stub, claim_id, pid, retry, turn_id, &why),
            Err(e) => return self.stub_failed(stub, claim_id, pid, retry, turn_id, &e.to_string()),
        }
        // 6. The redeem.
        Step::Go(self.redeem(t, stub, turn_id, claim_id, account.as_ref()))
    }

    /// Step 3: `hostclaim.turn`. `Ok` is the claim and the account the core picked; `Err` is what the answer comes to.
    fn claim_turn(&self, t: &Turn<'_>, turn_id: i64, person: &str) -> Result<(i64, Option<String>), Begin> {
        let mut ask = json!({"conversation": t.conversation, "turn": turn_id, "person": person,
                             "revokeSeen": t.revoke_seen, "cwd": t.cwd, "sessionId": t.conversation,
                             "holderInstance": self.instance});
        for (k, v) in crate::core_front::caller_env() {
            ask["env"][k] = v;
        }
        match self.call("hostclaim.turn", &ask) {
            Answer::Down { why, .. } => Err(Begin::CoreDown { turn: Some(turn_id), why }),
            Answer::Refused(said) => Err(Begin::Failed { turn: Some(turn_id), why: said }),
            Answer::Done(v) => match (v["status"].as_str(), v["claimId"].as_i64()) {
                (Some("claimed"), Some(id)) => Ok((id, v["account"].as_str().map(str::to_owned))),
                (Some("exhausted"), _) => {
                    self.cancel(turn_id, "no account left");
                    let text = v["text"].as_str().unwrap_or("no account left").to_owned();
                    Err(Begin::Exhausted { text })
                }
                _ => Err(self.not_claimed(t, turn_id, &v)),
            },
        }
    }

    /// A claim the core would not give (`v` is its refusal as data).
    fn not_claimed(&self, t: &Turn<'_>, turn: i64, v: &Value) -> Begin {
        let said = v["why"].as_str().unwrap_or_default().to_owned();
        let drop = |why| Begin::Dropped { turn: Some(turn), why, said: said.clone() };
        match v["refused"].as_str() {
            Some("held") => match self.live(t.conversation) {
                Err(why) => Begin::CoreDown { turn: Some(turn), why },
                Ok(Some(c)) if matches!(c["kind"].as_str(), Some("terminal" | "external")) => {
                    match self.data("hostclaim.park", &json!({"turn": turn, "reason": "terminal_live"})) {
                        Ok(p) if p["parked"] == true => Begin::Parked { turn, why: Parked::TerminalLive },
                        // Not queued any more (cancelled meanwhile): the next begin finds out.
                        _ => Begin::Queued { turn },
                    }
                }
                Ok(_) => Begin::Queued { turn },
            },
            Some("waiting") => Begin::Retry { turn, after: RECONCILE_RETRY },
            Some("notWaiting") => drop(Dropped::NotWaiting),
            Some("stopped") => drop(Dropped::Stopped),
            Some("revoked") => {
                self.cancel(turn, "revoked");
                drop(Dropped::Revoked)
            }
            Some("budget") => {
                self.cancel(turn, "budget");
                drop(Dropped::Budget)
            }
            _ => {
                self.cancel(turn, "refused");
                Begin::Failed { turn: Some(turn), why: format!("the core refused the claim: {v}") }
            }
        }
    }

    /// `take` got an error or no answer: the claim is reported `exit` (the core puts the turn back), the process restarted
    /// and the claim made again, once.
    fn stub_failed(&self, stub: &mut dyn WarmStub, claim: i64, pid: u32, retry: bool, turn: i64, why: &str) -> Step {
        let _ = self.report(claim, "exit", pid, &json!({"why": why}));
        if !retry {
            return Step::Go(Begin::Failed { turn: Some(turn), why: format!("the stub failed again: {why}") });
        }
        match stub.restart(None) {
            Ok(()) => Step::Again,
            Err(e) => Step::Go(Begin::Failed { turn: Some(turn), why: format!("restarting the process: {e}") }),
        }
    }

    fn redeem(&self, t: &Turn<'_>, stub: &mut dyn WarmStub, turn: i64, claim: i64, account: Option<&String>) -> Begin {
        let pid = stub.pid();
        let Some(started) = proc_start(pid) else {
            return if give_lock_back(stub, t.conversation, claim) {
                Begin::Failed { turn: Some(turn), why: "the stub's start time can't be read".into() }
            } else {
                self.lock_stuck(stub, claim, "no start time")
            };
        };
        let ask = json!({"claimId": claim, "pid": pid, "started": started, "holderInstance": self.instance});
        let running = |again| {
            Begin::Running(Running { turn, claim, pid, started: started.clone(), account: account.cloned(), again })
        };
        for _ in 0..2 {
            match self.call("hostclaim.redeem", &ask) {
                Answer::Done(v) if v["running"] == true => return running(v["again"] == true),
                Answer::Done(v) if v["refused"].is_string() => {
                    // Revoked, stopped or ended between the claim and the redeem: the lock goes back, the claim is not running.
                    let state = v["refused"].as_str().unwrap_or_default().to_owned();
                    if !give_lock_back(stub, t.conversation, claim) {
                        return self.lock_stuck(stub, claim, &format!("redeem refused ({state})"));
                    }
                    self.cancel(turn, "not redeemed");
                    return Begin::Dropped {
                        turn: Some(turn),
                        why: Dropped::Stopped,
                        said: format!("redeem refused ({state})"),
                    };
                }
                // The reply may have been lost after the core redeemed: a claim running on our pid stands.
                _ => match self.data("hostclaim.show", &json!({"claimId": claim})) {
                    Ok(s) if s["claim"]["state"] == "running" && s["claim"]["pid"].as_u64() == Some(u64::from(pid)) => {
                        return running(true);
                    }
                    Ok(s) if s["claim"]["state"] == "claimed" => {}
                    Ok(s) => {
                        // Ended meanwhile (a stop, a revoke): nothing is running, so the lock goes back.
                        if !give_lock_back(stub, t.conversation, claim) {
                            return self.lock_stuck(stub, claim, "claim ended during the redeem");
                        }
                        self.cancel(turn, "not redeemed");
                        return Begin::Dropped {
                            turn: Some(turn),
                            why: Dropped::Stopped,
                            said: s["claim"].to_string(),
                        };
                    }
                    Err(why) => return Begin::CoreDown { turn: Some(turn), why },
                },
            }
        }
        // The lock stays: a repeated begin takes it again (`locked`) and redeems, or finds the claim running.
        Begin::CoreDown { turn: Some(turn), why: "the redeem got no answer".into() }
    }

    /// The stub found the lock taken: the claim ends `busy` and the turn parks `lock_busy`.
    pub fn busy(&self, claim: i64, pid: u32) -> Result<(), String> {
        self.report(claim, "busy", pid, &json!({}))
    }

    /// A lock that cannot be given back must not outlive a claim that is not running.
    fn lock_stuck(&self, stub: &dyn WarmStub, claim: i64, why: &str) -> Begin {
        tracing::error!(
            claim,
            why,
            "core writer: the lock could not be released; stopping the claim and killing the process"
        );
        let _ = self.call("hostclaim.stop", &json!({"claimId": claim, "reason": "lock stuck"}));
        stub.kill_group();
        Begin::LockStuck { why: why.to_owned() }
    }

    /// Ends the turn `run` began (design 1.3, "At end").
    pub fn finish(&self, run: &Running, conversation: &str, stub: &mut dyn WarmStub, end: End) -> Finished {
        match end {
            End::Room => {
                let _ = self.call("hostclaim.stop", &json!({"claimId": run.claim, "reason": "room-turn-end"}));
                stub.kill_group();
                // Forgotten once confirmed gone: `end_gone` then ends the claim; until then the core keeps the row to kill.
                let until = std::time::Instant::now() + Duration::from_secs(3);
                while pid_runs(run.pid) && std::time::Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(25));
                }
                let gone = !pid_runs(run.pid);
                if gone {
                    let _ = self.call("hostclaim.settle", &json!({}));
                    let _ = self.proc_gone(run.pid, &run.started);
                }
                Finished::RoomEnded { gone }
            }
            End::Killed => match self.report(run.claim, "exit", run.pid, &json!({})) {
                Ok(()) => Finished::Reported,
                Err(why) => Finished::ReportLost(why),
            },
            End::Done | End::OutOfUsage => {
                if !give_lock_back(stub, conversation, run.claim) {
                    let Begin::LockStuck { why } = self.lock_stuck(stub, run.claim, "release failed") else {
                        unreachable!()
                    };
                    return Finished::LockStuck(why);
                }
                let sent = if end == End::Done {
                    self.report(run.claim, "done", run.pid, &json!({}))
                } else {
                    self.report(run.claim, "exit", run.pid, &json!({"out_of_usage": true}))
                };
                match sent {
                    Ok(()) => Finished::Reported,
                    Err(why) => Finished::ReportLost(why),
                }
            }
        }
    }

    /// Gives a begun turn up before it ran (the item was cancelled between `begin` and the prompt, or a begin was left
    /// unresolved): the lock back, the claim stopped, the turn cancelled.
    pub fn abandon(&self, run_turn: i64, claim: i64, conversation: &str, stub: &mut dyn WarmStub) {
        if !give_lock_back(stub, conversation, claim) {
            let _ = self.lock_stuck(stub, claim, "release failed");
            return;
        }
        let _ = self.call("hostclaim.stop", &json!({"claimId": claim, "reason": "abandoned"}));
        self.cancel(run_turn, "abandoned");
    }

    /// The slow tick, or the kick when the core reports an external claim ended: parked turns of the conversation go back to
    /// queued once nothing holds it. No lock probe (the retried `take` is the probe), so `lockFree` is always true. The turns
    /// woken; a second call finds none.
    pub fn unpark(&self, conversation: &str, terminal_live: bool) -> Result<Vec<i64>, String> {
        let v = self.data(
            "hostclaim.unpark",
            &json!({"conversation": conversation, "lockFree": true, "terminalLive": terminal_live}),
        )?;
        Ok(v["turns"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect())
    }

    /// First thing at host start: ends every claim and marks every process of another host instance (design 1.3, "Host
    /// restart"). A claim never redeemed ends at once; one with a process stays the writer until the process is gone.
    pub fn adopt(&self) -> Result<Adopted, String> {
        let v = self.data("hostclaim.adopt", &json!({"instance": self.instance}))?;
        let ids = |k: &str, field: &str| -> Vec<i64> {
            v[k].as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| if field.is_empty() { e.as_i64() } else { e[field].as_i64() })
                .collect()
        };
        Ok(Adopted { ended: ids("ended", ""), kill: ids("kill", "claimId") })
    }

    /// Registers a warm process before `session/load` (`warm_procs`). `revoke_seen` is read before the process starts.
    pub fn register_proc(
        &self,
        pid: u32,
        started: &str,
        person: &str,
        agent: &str,
        conversation: Option<&str>,
        revoke_seen: Option<i64>,
    ) -> Result<(), String> {
        let mut ask =
            json!({"pid": pid, "started": started, "instance": self.instance, "person": person, "agent": agent});
        if let Some(c) = conversation {
            ask["conversation"] = json!(c);
        }
        if let Some(e) = revoke_seen {
            ask["revokeSeen"] = json!(e);
        }
        let v = self.data("hostclaim.proc", &ask)?;
        match v["refused"].as_str() {
            Some(kind) => Err(format!("{kind}: {}", v["why"].as_str().unwrap_or_default())),
            None => Ok(()),
        }
    }

    /// Forgets a warm process once it is gone.
    pub fn proc_gone(&self, pid: u32, started: &str) -> Result<bool, String> {
        Ok(self.data("hostclaim.procGone", &json!({"pid": pid, "started": started}))?["gone"] == true)
    }
}

/// A refusal of `hostclaim.enqueue` (no turn exists yet).
fn refusal(turn: Option<i64>, v: &Value) -> Begin {
    let said = v["why"].as_str().unwrap_or_default().to_owned();
    let why = match v["refused"].as_str() {
        Some("revoked") => Dropped::Revoked,
        Some("budget") => Dropped::Budget,
        Some("stopped") => Dropped::Stopped,
        Some("notWaiting") => Dropped::NotWaiting,
        _ => Dropped::Refused,
    };
    Begin::Dropped { turn, why, said }
}

/// Up to three releases (a repeated one is `not-held`, which is fine). False: the lock is still ours.
fn give_lock_back(stub: &dyn WarmStub, key: &str, claim: i64) -> bool {
    (0..RELEASE_TRIES).any(|_| matches!(stub.release(key, claim), Ok(Release::Released | Release::NotHeld)))
}

fn owner_or(person: &str) -> &str {
    if person.is_empty() { "owner" } else { person }
}

#[cfg(test)]
#[path = "core_writer_tests.rs"]
mod tests;
