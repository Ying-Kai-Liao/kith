//! The host's routines after the core takes the schedules (M4-5b; `kith kernel cutover step schedules`).
//!
//! Until that step the host runs routines from serve.db and nothing here changes anything: [`Gate::Own`]. The step
//! imports every routine into its agent's jobs.json (job id = routine id, or `<id>-<n>` for the n-th of several
//! triggers) and the core fires them: [`Gate::Core`]. Then, and only then:
//!
//! - the host's own scheduler fires nothing; once a minute it calls `schedule.tick` and the core does the firing (one tick
//!   at a time, a slow one is skipped not queued, a failed one is logged and waits for the next minute);
//! - `POST /hooks/routines/:id` (and the relay's delivery, which reaches the same `receive`) goes to `schedule.fire` for the
//!   job the import made from that routine, as the Host principal. The core checks the key and the signature with its
//!   own copy of the key; the host logs and returns no secret;
//! - the app's and the agent's "run now" goes to `schedule.runNow`;
//! - creating, editing, pausing and deleting a routine is refused: jobs.json is the store now and a host edit would
//!   silently diverge from it (change it with `kith schedule`);
//! - a routine the import refused (a host-native bot with no kith agent, `hq_agent` empty) has no job: its hook answers
//!   `unauthorized` (told apart from nothing else, as an unknown routine is) and it never fires;
//! - runs the host started before the step (rows in serve.db) still finish and publish; serve.db's routines stay as
//!   they were (a rollback writes them back) and are never read as live.
//!
//! **The gate.** The host asks `cutover.mayServe {module: "schedules"}` at start and on every runner tick.
//!
//! - `ok: true`: the core owns them ([`Gate::Core`]).
//! - `ok: false` with no warning: clearly not switched ([`Gate::Own`]); the host keeps its own runner.
//! - anything else (a warning, a refusal, no daemon, a connection that broke): the host cannot tell. If the switch file for
//!   schedules exists (stepping, rolling back, a marker that does not match, or a core that is down after the step) it
//!   might be the core's turn, so it fires nothing, tells nobody anything and logs ([`Gate::Held`]); a delivery waiting
//!   in the cloud queue is answered "try again". If there is no switch file the module was never switched, and an
//!   unreachable core is just how a Mac without a daemon (or before it is up) looks: [`Gate::Own`].

use super::{Hub, LockExt, Refused, Result, Routines, Trigger, Value, bail, json};
use crate::core_front::Answer;
use std::sync::atomic::Ordering;

/// The module's name in the cutover framework.
const MODULE: &str = "schedules";
/// One core tick a minute, as `kith-sched`'s launchd job ticked.
const TICK_EVERY_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Gate {
    /// Not switched: the host's own runner (today's behaviour).
    #[default]
    Own,
    /// Switched: the core fires, the host forwards.
    Core,
    /// Cannot tell, and the switch file exists: nothing fires from the host and nothing is forwarded.
    Held(String),
}

/// The host's way to the core, so a test can stand in for it.
pub trait Link: Send + Sync {
    fn gate(&self) -> Gate;
    /// An owner call (`schedule.tick`, `schedule.runNow`); never sent twice.
    fn owner(&self, method: &str, params: &Value) -> Answer;
    /// A Host-principal call (`schedule.fire`); never sent twice.
    fn host(&self, method: &str, params: &Value) -> Answer;
}

/// The real thing: the daemon beside this host.
pub struct CoreLink;

impl Link for CoreLink {
    fn gate(&self) -> Gate {
        let folders = crate::core_front::process_folders();
        let said = crate::core_front::call_owner(&folders, "cutover.mayServe", &json!({"module": MODULE}), true);
        decide(&said, crate::core_front::switch_file_exists(&folders, MODULE))
    }

    fn owner(&self, method: &str, params: &Value) -> Answer {
        crate::core_front::call_owner(&crate::core_front::process_folders(), method, params, false)
    }

    fn host(&self, method: &str, params: &Value) -> Answer {
        crate::core_front::call_host(&crate::core_front::process_folders(), method, params)
    }
}

/// The rule in the module doc.
pub fn decide(said: &Answer, switch_file: bool) -> Gate {
    let unclear = |why: String| if switch_file { Gate::Held(why) } else { Gate::Own };
    match said {
        Answer::Done(v) => match (v.get("ok").and_then(Value::as_bool), v.get("warning").and_then(Value::as_str)) {
            (Some(true), _) => Gate::Core,
            (Some(false), None) => Gate::Own,
            (Some(false), Some(warning)) => unclear(warning.to_owned()),
            (None, _) => unclear("the core's gate answered in a shape the host does not know".into()),
        },
        Answer::Refused(why) => unclear(format!("the core refused the gate question: {why}")),
        Answer::Down { why, .. } => unclear(format!("the core cannot be asked: {why}")),
    }
}

impl Routines {
    pub fn gate(&self) -> Gate {
        self.gate.locked().clone()
    }

    /// Ask the core and remember the answer; a change is logged once.
    pub fn refresh_gate(&self) -> Gate {
        let link = self.link.locked().clone();
        let now = link.gate();
        let mut held = self.gate.locked();
        if *held != now {
            match &now {
                Gate::Own => tracing::info!("routines: the host runs its own routines"),
                Gate::Core => tracing::info!("routines: the core runs the schedules; the host forwards"),
                Gate::Held(why) => tracing::warn!(
                    why,
                    "routines: cannot tell whether the core owns the schedules; the host fires nothing"
                ),
            }
            held.clone_from(&now);
        }
        now
    }

    /// Use another way to the core (a test's).
    #[cfg(test)]
    pub fn use_link(&self, link: std::sync::Arc<dyn Link>) {
        *self.link.locked() = link;
    }

    /// Whether a core tick is due now; true claims it (one at a time, a minute apart). The caller must call
    /// [`Self::forward_tick`], which releases the claim.
    pub(super) fn core_tick_due(&self, now: i64) -> bool {
        if self.core_ticking.swap(true, Ordering::AcqRel) {
            return false;
        }
        let mut last = self.last_core_tick.locked();
        if now - *last < TICK_EVERY_MS {
            self.core_ticking.store(false, Ordering::Release);
            return false;
        }
        // Stamped when claimed, not when done: a failing tick waits for the next minute instead of looping.
        *last = now;
        true
    }

    /// One `schedule.tick`. Blocking; the runner calls it off the async threads.
    pub(super) fn forward_tick(&self) {
        let link = self.link.locked().clone();
        match link.owner("schedule.tick", &json!({})) {
            Answer::Done(v) => tracing::debug!(fired = %v, "routines: the core ticked"),
            Answer::Refused(why) => tracing::warn!(why, "routines: the core refused a schedule tick"),
            Answer::Down { why, .. } => tracing::warn!(why, "routines: could not reach the core for a schedule tick"),
        }
        self.core_ticking.store(false, Ordering::Release);
    }

    /// Edits are the core's now.
    pub(super) fn refuse_edit(&self) -> Result<()> {
        match self.gate() {
            Gate::Own => Ok(()),
            Gate::Core => bail!(
                "Routines are run by the Kith core now: their jobs file is the store, and a change made here would not reach it. Change it with `kith schedule` (add, pause, resume, rm)."
            ),
            Gate::Held(_) => bail!(
                "Routines are being moved to the Kith core and cannot be changed until that is settled (check `kith migrate status`)."
            ),
        }
    }

    /// The agent and the jobs the import made from this routine: `(job, takes deliveries)` in trigger order. None for a
    /// routine the host does not have, or one the import refused (no kith agent behind its bot).
    #[allow(clippy::type_complexity, reason = "(bot, agent, jobs) read once by two callers")]
    fn jobs_of(&self, hub: &Hub, id: &str) -> Option<(String, String, Vec<(String, bool)>)> {
        let r = self.state.locked().routines.iter().find(|r| r.id == id && !r.deleted).cloned()?;
        let agent = hub
            .store
            .bot(&r.bot_id)
            .ok()
            .flatten()
            .filter(|b| !b.deleted)?
            .config
            .hq_agent
            .filter(|a| !a.is_empty())?;
        let n = r.triggers.len();
        let jobs = r
            .triggers
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let job = if n == 1 { r.id.clone() } else { format!("{}-{}", r.id, i + 1) };
                (job, matches!(t, Trigger::Webhook | Trigger::Event { .. }))
            })
            .collect();
        Some((r.bot_id, agent, jobs))
    }

    /// A delivery while the core owns the schedules: handed over whole, the core decides. A routine with several
    /// delivery triggers has one job each; the first one that takes the delivery (or answers busy or paused) answers, and
    /// when none matches it is `ignored`, as for a routine with one trigger.
    pub(super) fn receive_via_core(
        &self,
        hub: &Hub,
        id: &str,
        headers: &super::hooks::Headers,
        body: &[u8],
    ) -> Result<Value, Refused> {
        let hooked: Vec<(String, String)> = match self.jobs_of(hub, id) {
            Some((_, agent, jobs)) => {
                jobs.into_iter().filter(|(_, hooked)| *hooked).map(|(job, _)| (agent.clone(), job)).collect()
            }
            None => Vec::new(),
        };
        if hooked.is_empty() {
            return Err(Refused::Unauthorized);
        }
        let Ok(text) = std::str::from_utf8(body) else {
            return Err(Refused::Invalid("event body must be text".into()));
        };
        let link = self.link.locked().clone();
        for (agent, job) in hooked {
            let params = json!({"agent": agent, "id": job, "headers": headers, "body": text, "caller": "webhook"});
            let v = match link.host("schedule.fire", &params) {
                Answer::Done(v) => v,
                Answer::Refused(why) => return Err(Refused::Invalid(why)),
                Answer::Down { why, .. } => {
                    // The reason carries no header or body: only the core's words about its own state.
                    tracing::warn!(why, "routines: a webhook delivery could not reach the core");
                    return Err(Refused::Held);
                }
            };
            let reason = || v["reason"].as_str().unwrap_or("refused").to_owned();
            match v["outcome"].as_str().unwrap_or_default() {
                "ignored" => {}
                "ping" => return Ok(json!({"ping": true})),
                "busy" | "stale" => return Err(Refused::Busy),
                "paused" => return Err(Refused::Paused),
                "refused" => {
                    return Err(match v["code"].as_str().unwrap_or_default() {
                        "unauthorized" => Refused::Unauthorized,
                        "paused" => Refused::Paused,
                        _ => Refused::Invalid(reason()),
                    });
                }
                _ => return Ok(v),
            }
        }
        Ok(json!({"ignored": "event does not match this routine"}))
    }

    /// "Run now" while the core owns the schedules: the routine's first job, through the same fire as a tick.
    pub(super) fn run_now_via_core(&self, hub: &Hub, bot: &str, id: &str) -> Result<Value> {
        let Some((owner, agent, jobs)) = self.jobs_of(hub, id).filter(|(owner, ..)| owner == bot) else {
            bail!("routine not found");
        };
        let Some((job, _)) = jobs.first() else { bail!("routine not found") };
        let key = format!("host-{}", uuid::Uuid::new_v4().simple());
        let link = self.link.locked().clone();
        match link.owner("schedule.runNow", &json!({"agent": agent, "id": job, "key": key})) {
            Answer::Done(v) => match v["outcome"].as_str().unwrap_or_default() {
                "fired" => {
                    Ok(json!({"run": {"id": key, "routineId": id, "botId": owner, "status": "pending"}, "core": v}))
                }
                "busy" => bail!(Refused::Busy),
                other => bail!("the core did not start the routine ({other})"),
            },
            Answer::Refused(why) => bail!("the core refused to run the routine: {why}"),
            Answer::Down { why, .. } => bail!("Kith's core isn't answering ({why}); the routine was not started"),
        }
    }

    /// The test-run entry both the app and the agent's tool come through.
    pub fn run_now(&self, hub: &Hub, bot: &str, id: &str) -> Result<Value> {
        match self.gate() {
            Gate::Own => self.enqueue(&hub.store, bot, id, json!({"source": "test"}), None, true),
            Gate::Core => self.run_now_via_core(hub, bot, id),
            Gate::Held(_) => self.refuse_edit().map(|()| Value::Null),
        }
    }
}
