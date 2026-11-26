//! The takers of `kernel/bin/kith-sessions` in both modes: [`Taker`] takes, gives back and sweeps the writer claims as the
//! Python does, in Files mode against `<state>/live/terminal/*.claim` and in Writer mode (the claims are the core's,
//! [`claims_switched`]) through `claim.external`, `claim.release`, `claim.sweepPerson` and `claim.epoch`. Which mode is read
//! at every call, from the files, as the Python's `writer_mode()` does; the mode is never an env var and a core that does not
//! answer is never answered with the files.
//!
//! The errors are the lines `kith-sessions` dies with, after the `kith: ` its callers put in front.
//!
//! The step's race (thread-writers, `take_claim`): a taker that looked before the claims marker went down and links after it
//! takes its file back and claims in the core ([`super::take_claim_checked`]); the refusal of the claims step re-reads the
//! files only once the marker is down, so one of the two sees the other.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::core::{Core, CoreErr, claims_switched, lockable};
use super::resolve::truthy;
use super::{
    Claim, Ctx, Outcome, Take, claim_path, claim_wait, intent, os, proc_claim, read_claim,
    release_claims, sweep_persons, take_claim_checked,
};

/// Who holds a session the taker could not claim.
#[derive(Debug, PartialEq, Eq)]
pub struct Holder {
    /// `None`: a claim file with no usable pid (printed `None`, as the Python does).
    pub pid: Option<i64>,
    pub kind: String,
    /// An idle agent of the app is being stopped: worth the usual wait even for a terminal.
    pub waiting: bool,
}

impl Holder {
    pub fn pid_text(&self) -> String {
        self.pid
            .map_or_else(|| "None".to_owned(), |p| p.to_string())
    }
}

/// The line a launch dies with when its host-made turn claim cannot be attached (`KITH_TURN_CLAIM`); the CLI exits
/// [`TURN_CLAIM_EXIT`] for it, so the host can tell "the turn is no longer this run's" from any other refusal.
pub const TURN_CLAIM_REFUSED: &str = "the turn claim was refused";
pub const TURN_CLAIM_EXIT: i32 = 10;

/// The exit code of a refused claim: [`TURN_CLAIM_EXIT`] for a refused turn claim, else 1 (a held writer), as `kith threads claim`
/// and `terminal` exit. One place for every launch path, so they cannot drift apart.
pub fn claim_exit(refusal: &str) -> i32 {
    if refusal.starts_with(TURN_CLAIM_REFUSED) {
        TURN_CLAIM_EXIT
    } else {
        1
    }
}

/// Why [`Taker::attach_run`] did not attach: the claim is no longer this run's (cancelled, adopted, attached by another), or the
/// core could not say.
#[derive(Debug, PartialEq, Eq)]
pub enum AttachErr {
    Gone(String),
    Down(String),
}

/// Why a `proc` claim was not made. `Core` is exit 9: the launcher stops a person's run for it.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcErr {
    /// The core cannot take it (the line the Python prints to stderr before exit 9).
    Core(String),
    Io(String),
}

pub struct Taker<'a> {
    c: &'a Ctx<'a>,
    core: Option<&'a dyn Core>,
    /// The owner's run goes on under the conversation lock alone (the core gave no answer): `claim` and `terminal` print
    /// `lock-alone`, `resolve` adds `r_alone=1`.
    alone: Cell<bool>,
    /// What the Python prints to stderr on the way and carries on (a run that cannot be claimed, a core not answering).
    said: RefCell<String>,
}

impl<'a> Taker<'a> {
    pub fn new(c: &'a Ctx<'a>, core: Option<&'a dyn Core>) -> Self {
        Self {
            c,
            core,
            alone: Cell::new(false),
            said: RefCell::new(String::new()),
        }
    }

    pub fn alone(&self) -> bool {
        self.alone.get()
    }

    /// The lines said so far, each ending in a newline.
    pub fn said(&self) -> String {
        self.said.borrow().clone()
    }

    fn say(&self, line: &str) {
        let mut s = self.said.borrow_mut();
        s.push_str(line);
        s.push('\n');
    }

    /// Whether the writer claims are the core's now (`writer_mode()`: needs `KITH_STATE`).
    pub fn writer(&self) -> bool {
        self.c.state.is_some() && claims_switched(self.c)
    }

    fn ask(&self, method: &str, params: &Value) -> Result<Value, CoreErr> {
        match self.core {
            Some(core) => core.call(method, params, None),
            None => Err(CoreErr::NoAnswer("no kith program to ask".to_owned())),
        }
    }

    /// `take_claim`: `Ok(None)` taken, `Ok(Some(holder))` a live writer holds it.
    pub fn take(&self, sid: &str, pid: i64, how: &Take) -> Result<Option<Holder>, String> {
        if self.writer() {
            return self.core_take(sid, pid, how, None, how.lifecycle_owner);
        }
        // The host's claim is a core row: in Files mode there is nothing to attach it to, and a run that started anyway would
        // leave it `claimed` with no process, for a host restart to requeue and prompt twice.
        let claimed = self.turn_claim()?.is_some();
        self.turn_conversation(claimed)?;
        if claimed {
            return Err(format!(
                "{TURN_CLAIM_REFUSED}: the claims are not the core's here, so the host's claim cannot be attached; nothing was started"
            ));
        }
        match take_claim_checked(self.c, sid, pid, how, &|| self.writer()) {
            Err(e) => Err(e.to_string()),
            Ok(Outcome::Taken) => Ok(None),
            Ok(Outcome::Held(h)) => Ok(Some(Holder {
                pid: h.0.pid,
                kind: h.0.kind,
                waiting: false,
            })),
            // The step put its marker down after the mode was read: the file just linked was taken back, and the core
            // refuses until the switch has finished.
            Ok(Outcome::Switched) => self.core_take(sid, pid, how, None, how.lifecycle_owner),
        }
    }

    /// Writer mode's take: `claim.external` for `pid` (the launcher's shell). `Ok(None)` taken, else who holds it (`waiting`
    /// for an idle agent of the app that is being stopped, retried by [`Self::claim_waiting`]). A core that gives no
    /// answer: background work counts the session as held (`held_when_down`); a person's run refuses (a revoke could not
    /// end it without a core claim); the owner's goes on under the stub's lock alone, with one line saying so.
    fn core_take(
        &self,
        sid: &str,
        pid: i64,
        how: &Take,
        engine: Option<&str>,
        held_when_down: bool,
    ) -> Result<Option<Holder>, String> {
        let person = how.person.filter(|p| !p.is_empty());
        let agent = how.agent.filter(|a| !a.is_empty());
        let mut unclaimable = None;
        let mut started = None;
        if lockable(sid) {
            started = os::proc_start(pid);
            if started.is_none() {
                unclaimable = Some(format!(
                    "cannot read the start time of pid {pid}, so its claim cannot be made"
                ));
            }
        } else {
            unclaimable = Some(format!(
                "session id \"{sid}\" cannot be locked (only a-z 0-9 . _ - are allowed), so it cannot be claimed"
            ));
        }
        let turn_claim = self.turn_claim()?;
        // The conversation the host's claim is for, when it is not `sid` (a channel thread's wake: the turn is on the thread's
        // work key, the session is another conversation).
        let turn_conversation = self
            .turn_conversation(turn_claim.is_some())?
            .filter(|c| c != sid);
        if let Some(why) = unclaimable {
            // A host-claimed turn is never run unclaimed: the host reads its claim's state to know the run is alive.
            if person.is_some()
                || turn_claim.is_some()
                || (self.c.env)("KITH_REQUIRE_STUB").as_deref() == Some("1")
            {
                if turn_claim.is_some() {
                    return Err(format!("{TURN_CLAIM_REFUSED}: {why}"));
                }
                return Err(why);
            }
            self.say(&format!("kith: {why}; this run is not claimed"));
            return Ok(None);
        }
        let mut params = json!({
            "conversation": sid,
            "pid": pid,
            "started": started,
            "person": person.unwrap_or(""),
        });
        if let Some(a) = agent {
            params["agent"] = json!(a);
        }
        if let Some(e) = engine {
            params["engine"] = json!(e);
        }
        if let Some(p) = person {
            // The person's revoke epoch as bin/kith read it BEFORE kith-auth decided they may run (`epoch`): a revoke after
            // it refuses this claim. Without it the core refuses a person's claim.
            let seen = (self.c.env)("KITH_REVOKE_SEEN").unwrap_or_default();
            if let Some((who, n)) = seen.rsplit_once(':')
                && who == p
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
                && let Ok(n) = n.parse::<u64>()
            {
                params["revokeSeen"] = json!(n);
            }
        }
        // A claim for the run's own session is attached first (the `claim.external` after it by the same process answers
        // `again`). One for another conversation is attached after the session's claim, so a session a writer holds never
        // redeems a turn that did not run: held or refused, the launch exits before the attach.
        if let (Some(claim), None) = (turn_claim, &turn_conversation) {
            self.attach_turn(
                claim,
                sid,
                None,
                pid,
                started.as_deref().unwrap_or_default(),
            )?;
        }
        let ans = match self.ask("claim.external", &params) {
            Ok(a) => a,
            Err(CoreErr::Refused(e)) => {
                return Err(format!("the Kith core refused the claim: {e}"));
            }
            Err(CoreErr::NoAnswer(why)) => {
                // Background work (compact, end) is held and tried again at the next sweep; only the owner's interactive
                // run goes on.
                if held_when_down {
                    return Ok(Some(Holder {
                        pid: Some(0),
                        kind: "the Kith core (not answering)".to_owned(),
                        waiting: false,
                    }));
                }
                if turn_conversation.is_some() {
                    // The wake's claim is attached after this one: with no core there is nothing to attach it to.
                    return Err(format!(
                        "{TURN_CLAIM_REFUSED}: the Kith core is not answering ({why}); nothing was started"
                    ));
                }
                if let Some(p) = person {
                    return Err(format!(
                        "the session is busy until the Kith core answers: the Kith core is not answering, so {p}'s run cannot be claimed ({why}); try again when kith serve is running"
                    ));
                }
                self.say(&format!(
                    "kith: the Kith core is not answering ({why}); this run is held by the conversation lock alone"
                ));
                self.alone.set(true);
                return Ok(None);
            }
        };
        let refused = ans.get("refused").filter(|w| truthy(w));
        match refused.and_then(Value::as_str) {
            Some("held") => {
                let h = ans.get("holder").filter(|h| truthy(h));
                let field = |k: &str| h.and_then(|h| h.get(k)).filter(|v| truthy(v));
                return Ok(Some(Holder {
                    pid: Some(field("pid").and_then(Value::as_i64).unwrap_or(0)),
                    kind: field("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("terminal")
                        .to_owned(),
                    waiting: false,
                }));
            }
            Some("waiting") => {
                return Ok(Some(Holder {
                    pid: Some(0),
                    kind: "the app's idle agent".to_owned(),
                    waiting: true,
                }));
            }
            Some("takeover") => {
                return Err(
                    "that session is being taken over in the app right now; try again in a moment"
                        .to_owned(),
                );
            }
            _ => {}
        }
        if let Some(why) = refused {
            let text = why.as_str().map_or_else(|| why.to_string(), str::to_owned);
            let more = ans.get("why").and_then(Value::as_str).unwrap_or("");
            return Err(format!("the Kith core refused the claim ({text}): {more}"));
        }
        if ans.get("claimId").is_none_or(Value::is_null) {
            return Err("the Kith core gave no claim for that session".to_owned());
        }
        if let (Some(claim), Some(wk)) = (turn_claim, &turn_conversation)
            && let Err(why) = self.attach_turn(
                claim,
                wk,
                Some(sid),
                pid,
                started.as_deref().unwrap_or_default(),
            )
        {
            // The session's claim has no turn to run: given back, so a refused launch leaves nothing held.
            self.core_release(pid, Some(sid));
            return Err(why);
        }
        // A taker ends the intent and the 54-minute decision of the period before it (rules 12, 14).
        if !how.lifecycle_owner
            && let Some(state) = self.c.state.as_deref()
        {
            intent::taken(state, sid);
        }
        Ok(None)
    }

    /// The claim the host made for the turn this launch runs (`KITH_TURN_CLAIM`, which `bin/kith` hands over from its own
    /// environment so the engine and a nested `kith` never see it). Empty is none.
    fn turn_claim(&self) -> Result<Option<i64>, String> {
        let Some(raw) = (self.c.env)("KITH_TURN_CLAIM").filter(|v| !v.trim().is_empty()) else {
            return Ok(None);
        };
        raw.trim()
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or_else(|| {
                format!("{TURN_CLAIM_REFUSED}: KITH_TURN_CLAIM \"{raw}\" is not a claim id")
            })
    }

    /// The conversation the host's claim is for (`KITH_TURN_CONVERSATION`), when the turn is not on the session the launch runs.
    /// It is read only with a claim: alone it is refused, nothing is started.
    fn turn_conversation(&self, has_claim: bool) -> Result<Option<String>, String> {
        let Some(raw) = (self.c.env)("KITH_TURN_CONVERSATION").filter(|v| !v.trim().is_empty())
        else {
            return Ok(None);
        };
        if !has_claim {
            return Err(format!(
                "{TURN_CLAIM_REFUSED}: KITH_TURN_CONVERSATION \"{raw}\" names no turn claim; nothing was started"
            ));
        }
        Ok(Some(raw.trim().to_owned()))
    }

    /// `claim.attach`: the launch's own process takes the host's claim for this turn before anything starts. The core's
    /// check-and-set refuses a claim that was cancelled, adopted by a host restart, another conversation's, or already
    /// attached by another process; the same process asking again (once per claim step) is told `again`.
    fn attach_turn(
        &self,
        claim: i64,
        sid: &str,
        session: Option<&str>,
        pid: i64,
        started: &str,
    ) -> Result<(), String> {
        let mut params =
            json!({"claimId": claim, "conversation": sid, "pid": pid, "started": started});
        // The engine's real session, when the turn is on another conversation (`ask:<id>`): the core keeps it for `-c`.
        if let Some(s) = session {
            params["session"] = json!(s);
        }
        match self.ask("claim.attach", &params) {
            Ok(a) if a.get("attached").is_some_and(truthy) => Ok(()),
            Ok(a) => {
                let state = a.get("state").and_then(Value::as_str).unwrap_or("unknown");
                Err(format!(
                    "{TURN_CLAIM_REFUSED}: claim {claim} is no longer this run's to start in {sid} (it is {state}); nothing was started"
                ))
            }
            Err(CoreErr::Refused(e)) => Err(format!("{TURN_CLAIM_REFUSED}: {e}")),
            Err(CoreErr::NoAnswer(why)) => Err(format!(
                "{TURN_CLAIM_REFUSED}: the Kith core is not answering ({why}); nothing was started"
            )),
        }
    }

    /// `claim.attach` for a run the host claimed without a conversation of its own to name (a schedule's turn, a non-Claude
    /// one-shot): this process takes the claim before anything starts, as `kith-sched run-turn` did. The same check-and-set.
    pub fn attach_run(&self, claim: i64, pid: i64) -> Result<(), AttachErr> {
        self.attach_run_in(claim, pid, None)
    }

    /// [`Self::attach_run`] naming the conversation the claim's turn is on, when the host said (`KITH_TURN_CONVERSATION`): a claim
    /// of another conversation is refused by the core's check-and-set.
    pub fn attach_run_in(
        &self,
        claim: i64,
        pid: i64,
        conversation: Option<&str>,
    ) -> Result<(), AttachErr> {
        let Some(started) = os::proc_start(pid) else {
            return Err(AttachErr::Down(format!(
                "cannot read the start time of pid {pid}"
            )));
        };
        let mut params = json!({"claimId": claim, "pid": pid, "started": started});
        if let Some(c) = conversation {
            params["conversation"] = json!(c);
        }
        match self.ask("claim.attach", &params) {
            Ok(a) if a.get("attached").is_some_and(truthy) => Ok(()),
            Ok(a) => {
                let why = a
                    .get("state")
                    .or_else(|| a.get("refused"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Err(AttachErr::Gone(why.to_owned()))
            }
            Err(CoreErr::Refused(e)) => Err(AttachErr::Down(format!("the core refused: {e}"))),
            Err(CoreErr::NoAnswer(why)) => {
                Err(AttachErr::Down(format!("the core did not answer: {why}")))
            }
        }
    }

    /// [`Self::take`], retried for up to `wait` while a live writer holds the session. `None` when taken (also when `pid`
    /// already holds it), else the live claim that still holds it.
    pub fn claim_waiting(
        &self,
        sid: &str,
        pid: i64,
        how: &Take,
        wait: Duration,
    ) -> Result<Option<Holder>, String> {
        let began = Instant::now();
        let mut end = began + wait;
        loop {
            let Some(held) = self.take(sid, pid, how)? else {
                return Ok(None);
            };
            if held.pid == Some(pid) {
                return Ok(None);
            }
            // The app's idle agent is being stopped: worth the usual wait even for a terminal.
            if held.waiting {
                end = end.max(began + claim_wait(self.c.env));
            }
            if Instant::now() >= end {
                return Ok(Some(held));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// `claim` / `terminal`: takes the claim for a launcher's shell, and refuses while a take-over runs.
    pub fn claim_start(
        &self,
        sid: &str,
        pid: i64,
        kind: &str,
        wait: Duration,
        person: Option<&str>,
        agent: Option<&str>,
    ) -> Result<(), String> {
        let Some(dir) = self.c.reg_dir() else {
            return Err("KITH_STATE is not set, so the session cannot be claimed".to_owned());
        };
        let how = Take {
            person,
            agent,
            ..Take::new(kind)
        };
        if let Some(held) = self.claim_waiting(sid, pid, &how, wait)? {
            return Err(if held.waiting {
                "that session is busy: the app's idle agent is still stopping; try again in a moment".to_owned()
            } else {
                format!(
                    "that session is busy: held by {} (pid {}); try again in a moment",
                    held.kind,
                    held.pid_text()
                )
            });
        }
        // Writer mode: a take-over is a queued claim, answered above.
        if !self.writer()
            && read_claim(&claim_path(&dir, sid, true))
                .as_ref()
                .is_some_and(Claim::is_live)
        {
            self.release_claims(pid);
            return Err(
                "that session is being taken over in the app right now; try again in a moment"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// `claim.release` for the claims `pid` holds (only `sid`'s when given). True when one was given back. A core that does
    /// not answer leaves the claim to end with its process (the core ends a claim whose pid is gone), so this never fails.
    fn core_release(&self, pid: i64, sid: Option<&str>) -> bool {
        let Some(started) = os::proc_start(pid) else {
            return false;
        };
        let mut params = json!({"pid": pid, "started": started});
        if let Some(s) = sid.filter(|s| !s.is_empty()) {
            params["conversation"] = json!(s);
        }
        self.ask("claim.release", &params)
            .is_ok_and(|a| a.get("ended").is_some_and(truthy))
    }

    /// Gives back every claim `pid` holds (the terminal's own run ended).
    pub fn release_claims(&self, pid: i64) {
        if self.writer() {
            self.core_release(pid, None);
        } else {
            release_claims(self.c, pid);
        }
    }

    /// `release SID`: gives back SID's claim, only if it is the caller's parent's (a launcher shell that took it in
    /// `resolve`) and that process is the one that took it (same start time). True when it was given back.
    pub fn release_own(&self, sid: &str) -> bool {
        self.release_for(sid, os::parent_pid())
    }

    /// [`Self::release_own`] for a claim named by its pid: the launch that runs in the process that took the claim (`--held`)
    /// gives back its own, not its parent's.
    pub fn release_for(&self, sid: &str, ppid: i64) -> bool {
        if self.writer() {
            return self.core_release(ppid, Some(sid));
        }
        let Some(dir) = self.c.reg_dir() else {
            return false;
        };
        let f = claim_path(&dir, sid, false);
        if !f.exists() {
            return false;
        }
        let Ok(_lock) = super::Lock::exclusive(&super::lock_path(&f)) else {
            return false;
        };
        match read_claim(&f) {
            Some(c) if c.pid == Some(ppid) && c.identity() != Some(false) => {
                let _ = std::fs::remove_file(&f);
                true
            }
            _ => false,
        }
    }

    /// `proc PID PERSON AGENT`: a run that has a person's profile is findable by the revoke sweep for its whole life.
    pub fn proc(&self, pid: i64, person: &str, agent: &str) -> Result<(), ProcErr> {
        if self.c.reg_dir().is_none() {
            return Ok(());
        }
        if !self.writer() {
            proc_claim(self.c, pid, person, agent).map_err(|e| ProcErr::Io(e.to_string()))?;
            if !self.writer() {
                return Ok(());
            }
            // The marker went down after the mode was read: the file is a run nobody reads. Take it back and claim in the core.
            if let Some(dir) = self.c.reg_dir() {
                let _ = std::fs::remove_file(dir.join(format!("proc-{pid}.claim")));
            }
        }
        // A conversation of its own per process (`proc-<pid>-<start stamp>`: a reused pid is another key), so a run with no
        // session (-c) is still a claim the core's revoke finds. Its engine is `proc`: readers of conversations skip those.
        // A person's run the core cannot claim must not go on: exit 9 tells the launcher.
        let started = os::proc_start(pid).unwrap_or_default();
        let key = format!("proc-{pid}-{started}")
            .to_lowercase()
            .replace(':', ".");
        let how = Take {
            person: Some(person),
            agent: Some(agent),
            lifecycle_owner: true,
            ..Take::new("app")
        };
        self.core_take(&key, pid, &how, Some("proc"), false)
            .map(|_| ())
            .map_err(|m| ProcErr::Core(format!("kith: {m}")))
    }

    /// `epoch PERSON`: what to print, `PERSON:N`, how many times the core revoked them (`KITH_REVOKE_SEEN` for the claim);
    /// nothing in Files mode. A core that does not answer is an error with the reason.
    pub fn epoch(&self, person: &str) -> Result<String, String> {
        if !self.writer() {
            return Ok(String::new());
        }
        let ans = self
            .ask("claim.epoch", &json!({"person": person}))
            .map_err(|e| {
                format!(
                    "the session is busy until the Kith core answers: the Kith core is not answering, so {person}'s run cannot be claimed ({}); try again when kith serve is running",
                    e.why()
                )
            })?;
        match ans.get("epoch") {
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Ok(format!("{person}:{n}\n")),
            _ => Err("the Kith core gave no revoke epoch".to_owned()),
        }
    }

    /// `sweep PERSON...`: the pids ended. Writer mode asks the core whose claims these are and ends the rows; the signals
    /// are ours. The error is the line printed before exit 3: the files are not the claims any more, and a sweep that
    /// guessed from them would miss every run.
    pub fn sweep(&self, persons: &[String], grace: Duration) -> Result<Vec<i64>, String> {
        if !self.writer() {
            return Ok(sweep_persons(self.c, persons, grace));
        }
        let mut sorted: Vec<&String> = persons.iter().collect();
        sorted.sort();
        sorted.dedup();
        let ans = self
            .ask("claim.sweepPerson", &json!({"persons": sorted}))
            .map_err(|e| {
                format!(
                    "kith-sessions: the claims moved to the core, and it gave no answer ({}); start kith serve and sweep again",
                    e.why()
                )
            })?;
        let mut holders = Vec::new();
        for kill in ans
            .get("kill")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let pid = kill.get("pid").and_then(Value::as_i64);
            let started = kill
                .get("started")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if let (Some(pid), Some(started)) = (pid, started)
                && pid > 1
                && (Claim {
                    pid: Some(pid),
                    proc_start: Some(started.to_owned()),
                    at: String::new(),
                    kind: String::new(),
                    person: None,
                    agent: None,
                    takeover: false,
                })
                .is_claimer()
            {
                holders.push(pid);
            }
        }
        Ok(super::end_holders(holders, grace))
    }
}
