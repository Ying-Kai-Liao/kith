//! The host's runner for the turns the core queued and no host began (design 2026-11-16-core-queued-runner, rule R;
//! `HostRuns.tla`). Once the core owns the schedules its tick fires a job as a turn on `schedule:<agent>:<job>:<engine>`
//! with no `enq_key`, and the core starts no processes: in Writer mode the host pulls those turns (`hostclaim.queued`)
//! after each `schedule.tick` and every [`PULL_EVERY_MS`], and runs each as a routine run, by its id.
//!
//! - **A turn whose earlier claim was redeemed** (`redeemedBefore`: a restart cut it off, its prompt may have gone) is
//!   cancelled "interrupted" with a notice where the run shows, and never begun again (`SkipRedeemed`, `PromptOnce`).
//! - **Otherwise** it is an item here, held in memory (a host restart forgets it, as the model's `HostDown` does; the
//!   next host start's adopt puts a redeemed claim's turn back, and the rule above cancels it). The item goes to the bot's
//!   actor as a routine run (`Cmd::Routine` with the item's id): a thread of its own on a fresh session, the routine's
//!   timeout, and an `Accept` with the turn's id and no key (`writer.rs`), so it begins by id (`claim_next`'s CAS) and is
//!   never enqueued. A second pull of the same id while the item lives is dropped; one after it ended loses the CAS.
//! - **Where it shows**: an imported routine (its job is one the import made from a routine of this host) in that
//!   routine's bot, as before; a `kith-sched` job in its agent's own 1:1 chat. The thread opens with a notice in the
//!   chat, and the result is posted there (the run card) when it ends.
//! - **A cron job of `kith-sched`** (any job that is not an imported routine; M4-5 package C) is not run through a bot at all: the
//!   host claims its turn and starts `kith-sched run-turn` detached, for the owner's jobs and a guest's alike (`core_cron.rs`).
//! - **Only the owner's routine turns**: a routine's run goes to an owner's bot (no `person`). A turn the host cannot place
//!   right away (a routine whose bot is gone or not the owner's, a turn that names no job, a run that would not start) is
//!   cancelled with a reason and a notice where the run would have shown, so `kith schedule` says how it ended instead of
//!   `fired` and nothing waits silently for its deadline. A keyed turn is never listed (the host actor that enqueued it owns
//!   it; the strays sweep cancels an orphan).
//!
//! An item that ends without its turn having finished (its start failed) cancels the turn: nothing pulls it again.

use super::runner::Prepared;
use super::{EntryKind, Hub, Lane, LockExt, Result, Routines, Status, Value, default_timeout, json};
use crate::agent::bot::Pulled;
use crate::terminal::core_writer::Mode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the runner pulls between ticks (the writer's slow tick).
pub(super) const PULL_EVERY_MS: i64 = 2_000;

/// What a cancelled interrupted run says.
pub(crate) const INTERRUPTED: &str = "interrupted: a restart cut this run off";

/// One pulled turn on its way through a bot's actor.
#[derive(Clone)]
pub(super) struct Item {
    turn: i64,
    bot: String,
    conversation: String,
    person: String,
    prompt: String,
    name: String,
    timeout: Duration,
    pub(super) status: Status,
}

/// The item id the actor is sent: `core-<turn>`.
fn item_id(turn: i64) -> String {
    format!("core-{turn}")
}

/// The thread a run writes in.
fn root_of(turn: i64) -> String {
    format!("schedule-{turn}")
}

/// What the runner asks of the core and the system, so the rule below is tested without either.
pub(super) trait Ops {
    /// Cancels a turn at the core with a reason (true: it answered).
    fn cancel(&self, turn: i64, why: &str) -> bool;
    /// Claims a cron job's turn and starts its run (`core_cron.rs`). `job` is the job's id.
    fn cron(&self, turn: &Value, job: &str) -> Started;
}

/// What [`Ops::cron`] came to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Started {
    /// Claimed and running as a process of its own.
    Ran,
    /// Not now (a writer holds the conversation, the core did not answer): the turn stays queued and is pulled again.
    Later,
    /// The turn is gone (the core cancelled it, it was claimed elsewhere, its person was revoked): nothing to do.
    Gone,
    /// It cannot run and the claim is given back: the turn is cancelled with this reason.
    Failed(String),
}

/// The agent a bot's claims name (`Actor::writer_agent`): its hq agent's, or `bot:<id>` for a native bot.
fn agent_of(cfg: &crate::store::BotConfig) -> String {
    cfg.hq_agent.clone().unwrap_or_else(|| format!("bot:{}", cfg.id))
}

/// The imported routine a core job id names, if any: the import names a routine's jobs `<id>` (one trigger) or `<id>-<n>`.
/// `(bot id, name, timeout)`, whether or not that bot is still here.
fn routine_of(routines: &Routines, job: &str) -> Option<(String, String, Duration)> {
    let s = routines.state.locked();
    s.routines
        .iter()
        .find(|r| {
            !r.deleted
                && (if r.triggers.len() == 1 {
                    r.id == job
                } else {
                    (1..=r.triggers.len()).any(|n| format!("{}-{n}", r.id) == job)
                })
        })
        .map(|r| (r.bot_id.clone(), r.name.clone(), Duration::from_secs(r.timeout_seconds)))
}

/// Where a pulled turn shows, if anywhere here: `(bot, name, timeout)`. Only an owner's bot (no `person`) of the agent.
fn place(hub: &Hub, routines: &Routines, agent: &str, job: Option<&str>) -> Option<(String, String, Duration)> {
    let mine = |b: &crate::store::BotRow| !b.deleted && b.config.person.is_none() && agent_of(&b.config) == agent;
    let owners = |bot: &str| hub.store.bot(bot).ok().flatten().is_some_and(|b| mine(&b));
    if let Some(routine) = job.and_then(|job| routine_of(routines, job))
        && owners(&routine.0)
    {
        return Some(routine);
    }
    let bots = hub.store.bots().ok()?;
    let own = bots.iter().find(|b| mine(b))?;
    Some((own.config.id.clone(), job.unwrap_or("scheduled job").to_owned(), Duration::from_secs(default_timeout())))
}

impl Routines {
    /// Pulls once when due (every [`PULL_EVERY_MS`], or `now` right after a tick). Blocking; one pull at a time.
    pub(crate) fn pull_due(&self, hub: &Arc<Hub>, now: i64, after_tick: bool) {
        {
            let mut last = self.last_pull.locked();
            if !after_tick && now - *last < PULL_EVERY_MS {
                return;
            }
            *last = now;
        }
        if self.pulling.swap(true, Ordering::AcqRel) {
            return;
        }
        self.pull(hub);
        self.pulling.store(false, Ordering::Release);
    }

    /// One pull: only in Writer mode (the runner's turns run on the core's claims and nowhere else).
    fn pull(&self, hub: &Arc<Hub>) {
        if self.stopping() {
            return;
        }
        let Some(core) = crate::agent::bot::writer_core(hub) else { return };
        if core.mode() != Mode::Writer {
            return;
        }
        let turns = match core.queued() {
            Ok(t) => t,
            Err(why) => {
                tracing::debug!(why, "routines: the core's queued turns can't be read");
                return;
            }
        };
        self.take(hub, &turns, &super::core_cron::Live(core));
        self.dispatch_pulled(hub);
    }

    /// The rule of the module doc over what `hostclaim.queued` listed. `ops` is the core and the system.
    pub(super) fn take(&self, hub: &Hub, turns: &[Value], ops: &dyn Ops) {
        let listed: Vec<i64> = turns.iter().filter_map(|t| t["turn"].as_i64()).collect();
        // A pending item whose turn is no longer waiting (expired, cancelled) is gone; one that started is the actor's.
        self.pulled.locked().retain(|_, i| i.status != Status::Pending || listed.contains(&i.turn));
        // One that started on an actor that is gone has no one to end it: it would hold the bot's routine slot until a host restart.
        // Dropped, the slot freed; its turn, if the core put it back, is pulled again.
        let orphans: Vec<String> = self
            .pulled
            .locked()
            .extract_if(|_, i| i.status != Status::Pending && !hub.has_actor(&i.bot))
            .map(|(_, i)| i.bot)
            .collect();
        for bot in orphans {
            tracing::warn!(bot, "routines: a scheduled run's actor is gone; its slot is freed");
            self.release(&bot);
        }
        for t in turns {
            let Some(turn) = t["turn"].as_i64() else { continue };
            if self.pulled.locked().contains_key(&item_id(turn)) {
                // Ours (a requeue after a usage limit, a park): the actor begins it again by its id.
                continue;
            }
            let agent = t["agent"].as_str().unwrap_or_default();
            let job = t["job"].as_str();
            let where_ = place(hub, self, agent, job);
            if t["redeemedBefore"] == true {
                if ops.cancel(turn, INTERRUPTED)
                    && let Some((bot, name, _)) = where_
                {
                    let text =
                        format!("{name} was interrupted: a restart cut this run off. It runs again at its next time.");
                    let data = json!({"text": text, "style": "error", "coreTurn": turn});
                    if let Err(error) =
                        hub.add_entry_once(&format!("schedule-cut-{turn}"), &Lane::main(&bot), EntryKind::Notice, &data)
                    {
                        tracing::warn!(%error, "routines: couldn't post an interrupted run's notice");
                    }
                }
                continue;
            }
            let Some(job) = job else {
                Self::give_up(hub, ops, turn, where_.as_ref(), "the fire names no job");
                continue;
            };
            if routine_of(self, job).is_none() {
                // A cron job of kith-sched: its own process, for the owner and a guest alike.
                match ops.cron(t, job) {
                    Started::Ran | Started::Later | Started::Gone => {}
                    Started::Failed(why) => Self::give_up(hub, ops, turn, where_.as_ref(), &why),
                }
                continue;
            }
            if t["owner"].is_null() {
                tracing::warn!(
                    turn,
                    agent,
                    "routines: the core can't read the owner; a scheduled run waits and expires at its deadline"
                );
                continue;
            }
            if t["owner"] != true {
                Self::give_up(hub, ops, turn, where_.as_ref(), "a routine runs for the owner only");
                continue;
            }
            // The routine's own bot; one that is gone or not the owner's falls back to the agent's chat (`place`), and with no
            // bot at all there is nothing here to run it.
            let Some((bot, name, timeout)) = where_ else {
                Self::give_up(hub, ops, turn, None, "no bot of the owner's here to run it");
                continue;
            };
            let item = Item {
                turn,
                bot,
                conversation: t["conversation"].as_str().unwrap_or_default().to_owned(),
                person: t["person"].as_str().unwrap_or_default().to_owned(),
                prompt: t["prompt"].as_str().unwrap_or_default().to_owned(),
                name,
                timeout,
                status: Status::Pending,
            };
            self.pulled.locked().insert(item_id(turn), item);
        }
    }

    /// Cancels a turn the host cannot place, with the reason, and says so where its run would have shown (the notice keeps
    /// `kith schedule` from showing only `fired`).
    fn give_up(hub: &Hub, ops: &dyn Ops, turn: i64, at: Option<&(String, String, Duration)>, why: &str) {
        tracing::warn!(turn, why, "routines: a scheduled run can't be placed; cancelled");
        if ops.cancel(turn, why)
            && let Some((bot, name, _)) = at
        {
            let data = json!({"text": format!("{name} did not run: {why}."), "style": "error", "coreTurn": turn});
            if let Err(error) =
                hub.add_entry_once(&format!("schedule-cut-{turn}"), &Lane::main(bot), EntryKind::Notice, &data)
            {
                tracing::warn!(%error, "routines: couldn't post a cancelled run's notice");
            }
        }
    }

    /// Hands each pending item to its bot's actor, one routine run per bot at a time.
    fn dispatch_pulled(&self, hub: &Arc<Hub>) {
        let pending: Vec<(String, String)> = self
            .pulled
            .locked()
            .iter()
            .filter(|(_, i)| i.status == Status::Pending)
            .map(|(id, i)| (id.clone(), i.bot.clone()))
            .collect();
        for (id, bot) in pending {
            if !self.active.locked().insert(bot.clone()) {
                continue;
            }
            if let Some(i) = self.pulled.locked().get_mut(&id) {
                i.status = Status::Starting;
            }
            if let Err(error) = hub.send_cmd(&bot, crate::agent::bot::Cmd::Routine(id.clone())) {
                tracing::warn!(error = format!("{error:#}"), "routines: couldn't hand a scheduled run to its bot");
                // Pulled again on the next pass.
                self.pulled.locked().remove(&id);
                self.release(&bot);
            }
        }
    }

    /// The turn and its begin for an item the actor was sent (`writer::Accept::pulled`); `None` for a serve.db run.
    pub(crate) fn pulled_of(&self, id: &str) -> Option<(i64, Pulled)> {
        self.pulled
            .locked()
            .get(id)
            .map(|i| (i.turn, Pulled { conversation: i.conversation.clone(), person: i.person.clone() }))
    }

    /// [`Routines::begin`] for an item: `None` when `id` is not one; `Some(None)` when it is no longer runnable.
    pub(super) fn begin_pulled(&self, hub: &Hub, bot: &str, id: &str) -> Option<Result<Option<Prepared>>> {
        let item = self.pulled.locked().get(id).cloned()?;
        if self.stopping() || item.bot != bot {
            self.pulled.locked().remove(id);
            self.release(bot);
            return Some(Ok(None));
        }
        let root = root_of(item.turn);
        let prompt = format!(
            "Scheduled routine: {}\n\n{}\n\nThis is an autonomous routine run. Follow its saved instruction. If there is nothing to report and the instruction permits silence, return exactly (pass). Otherwise give the user your final result. Do not recreate this routine.",
            item.name, item.prompt
        );
        let notice =
            json!({"text": format!("Scheduled · {}", item.name), "coreTurn": item.turn, "runId": id, "style": "info"});
        if let Err(e) = hub.add_entry_once(&root, &Lane::main(bot), EntryKind::Notice, &notice) {
            return Some(Err(e));
        }
        Some(Ok(Some(Prepared { lane: Lane::in_thread(bot, &root), prompt, timeout: item.timeout })))
    }

    /// [`Routines::mark_started`] for an item: `None` when `id` is not one.
    pub(super) fn mark_pulled(&self, id: &str) -> Option<Result<()>> {
        let mut all = self.pulled.locked();
        let item = all.get_mut(id)?;
        Some(if matches!(item.status, Status::Starting | Status::Running) {
            item.status = Status::Running;
            Ok(())
        } else {
            Err(anyhow::anyhow!("routine is no longer runnable"))
        })
    }

    /// The timeout of an item, if `id` is one.
    pub(super) fn pulled_timeout(&self, id: &str) -> Option<Duration> {
        self.pulled.locked().get(id).map(|i| i.timeout)
    }

    /// [`Routines::complete`] for an item: the result goes to the chat, the item is forgotten, and a turn that did not
    /// finish is cancelled (nothing pulls it again). `None` when `id` is not one.
    pub(super) fn complete_pulled(
        &self,
        hub: &Hub,
        id: &str,
        status: Status,
        detail: Option<&str>,
        text: Option<String>,
    ) -> Option<Result<()>> {
        let item = self.pulled.locked().remove(id)?;
        self.release(&item.bot);
        if status != Status::Succeeded
            && let Some(core) = crate::agent::bot::writer_core(hub)
        {
            // Done already, or running on a process: the core leaves it alone (`cancel_waiting_in`).
            core.cancel_turn(item.turn, "the scheduled run did not finish");
        }
        let text = text.filter(|t| !crate::chat::group::is_pass(t));
        let (kind, data) = match (&text, status) {
            (Some(t), _) => (EntryKind::Agent, json!({"text": t, "final": true, "coreTurn": item.turn, "runId": id})),
            (None, Status::Succeeded) => return Some(Ok(())),
            (None, _) => (
                EntryKind::Notice,
                json!({"text": format!("{} could not finish: {}", item.name, detail.unwrap_or("inspect its thread")),
                       "style": "error", "coreTurn": item.turn, "runId": id}),
            ),
        };
        Some(
            hub.add_entry_once(&format!("schedule-result-{}", item.turn), &Lane::main(&item.bot), kind, &data)
                .map(drop),
        )
    }

    /// Tests: the items held now, `(turn, bot, pending)`.
    #[cfg(test)]
    pub(super) fn pulled_items(&self) -> Vec<(i64, String, bool)> {
        let mut v: Vec<_> =
            self.pulled.locked().values().map(|i| (i.turn, i.bot.clone(), i.status == Status::Pending)).collect();
        v.sort_by_key(|i| i.0);
        v
    }
}
