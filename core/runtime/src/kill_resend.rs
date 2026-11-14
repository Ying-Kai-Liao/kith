//! The level-triggered re-send of a revoke's kill (M2-23 gate).
//!
//! A revoke commits a person's claims to `ending` and answers a `kill` list once. The host sends the signal on a detached
//! task (`end_kills` -> `end_claimed`); a host restart, a failed spawn or a stamp that could not be compared at that moment
//! loses it, and nothing else would signal that process again: `replay_open` finds no row, `end_gone` only waits for the
//! pid to leave, and the holder link ends only its own strays. So the daemon's own tick (it runs with no holder link at
//! all) reads the claims table and signals every `ending` claim whose process positively is the recorded one.
//!
//! The rules:
//! - Which claims: `state = 'ending'` with a pid, whose `end_reason` is a kill's (not `done` / `exit` / `out_of_usage`: a
//!   report that names no process also writes `ending`, a nameless exit that ran out of usage is rewritten to
//!   `out_of_usage`, and that process is the claimer's own business), and whose holder instance is this
//!   Mac's: the serve host's ([`is_host_instance`]), none (a claim made before the switch, a terminal's own), or the
//!   connected local holder's. A claim with no instance counts only for a kill the core decided (a revoke, a stop), never
//!   for `holder_changed`: no holder started that process (H1), so a holder's hello is no reason to signal it. Claims of an older local holder instance are skipped and left to the next hello's
//!   `reconcile`; a remote holder's pids are not this Mac's, so its instance is never an owner.
//! - Which process: the pid must be alive with a stamp that [`procstart::comparable`] and [`procstart::same_process`]
//!   say is the recorded one. A missing, empty or incomparable stamp is logged once per claim and never signalled; a gone
//!   or reused pid is left alone. Where the leader is gone but its group is still held and the stamp is from this boot,
//!   the dead leader's group is signalled (the rule of #919's `end_groups`).
//! - A `warm_procs` row with `kill_at` set (a revoke or a host change decided it must die) is signalled the same way, under
//!   the negative of its pid as its claim id; the row goes when `hostclaim.settle` finds the process gone.
//! - What is signalled: [`procstart::claim_victims`], the same set the host's `end_claimed` uses: descendants first, the
//!   claimer, and its process group when it leads one.
//! - How: SIGTERM on the first pass that finds it, remembered as (pid, stamp, when). SIGKILL on a later pass, once `grace`
//!   has gone, if it is still the same process; "survived KILL" is logged once per claim. Nothing sleeps, a pass repeated
//!   finds a gone process and does nothing, and `end_gone` ends the claim once the pid is gone.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use kith_kernel::procstart::{self, Sig};

use crate::conversations::claims::is_host_instance;
use crate::daemon::Shared;
use crate::log;

/// The end reason `release_unlisted` writes when a holder's hello did not list a claim.
const HOLDER_CHANGED: &str = "holder_changed";

/// How often the daemon's tick looks.
pub(crate) const EVERY: Duration = Duration::from_secs(5);
/// How long a SIGTERM gets before the SIGKILL.
pub(crate) const GRACE: Duration = Duration::from_secs(5);

/// What the pass remembers between ticks.
#[derive(Default)]
pub(crate) struct Resend {
    /// (pid, stamp) -> when SIGTERM went; a KILL has gone when `killed`.
    sent: HashMap<(u32, String), Sent>,
    /// Claims already logged as unstamped / not comparable.
    told: HashSet<i64>,
    /// Claims already logged as having survived the KILL.
    survived: HashSet<i64>,
}

struct Sent {
    termed: Instant,
    killed: bool,
}

/// The local holder instance whose claims this pass owns: only while a holder on this machine is connected.
pub(crate) fn connected_instance(shared: &Shared) -> Option<String> {
    let h = shared.holder_snapshot();
    (h.state == "connected" && !shared.terminals.is_remote())
        .then_some(h.instance)
        .flatten()
}

/// One tick. `holder` is [`connected_instance`]. Returns the claimer pids signalled on this pass.
pub(crate) fn pass(shared: &Shared, holder: Option<&str>, grace: Duration) -> Vec<u32> {
    type Row = (i64, i64, Option<String>, Option<String>, String);
    let rows: Vec<Row> = match shared.with_core(|c| {
        let mut st = c.conn().prepare(
            "SELECT claim_id, pid, pid_started, holder_instance, end_reason FROM claims
             WHERE state = 'ending' AND pid IS NOT NULL
               AND end_reason IS NOT NULL AND end_reason NOT IN ('done', 'exit', ?1)
             UNION ALL
             SELECT DISTINCT -pid, pid, started, instance, 'warm' FROM warm_procs WHERE kill_at IS NOT NULL",
        )?;
        let rows = st
            .query_map([crate::accounts::failover::OUT_OF_USAGE], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }) {
        Ok(rows) => rows,
        Err(e) => {
            log::note(&shared.logs, &format!("daemon: kill re-send: {e}"));
            return Vec::new();
        }
    };
    let mut guard = shared
        .resend
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let st = &mut *guard;
    let mut live: HashSet<(u32, String)> = HashSet::new();
    let mut touched = Vec::new();
    let mut claims: HashSet<i64> = HashSet::new();
    for (claim, pid, started, holder_instance, reason) in rows {
        claims.insert(claim);
        let mine = match holder_instance.as_deref() {
            None => reason != HOLDER_CHANGED,
            Some(h) => is_host_instance(h) || Some(h) == holder,
        };
        let Ok(pid) = u32::try_from(pid) else {
            continue;
        };
        if !mine || pid <= 1 || pid == std::process::id() {
            continue;
        }
        let recorded = started.unwrap_or_default();
        let key = (pid, recorded.clone());
        let victims = match procstart::probe(pid) {
            procstart::Probe::Gone => match procstart::orphan_group(pid, &recorded) {
                Some(group) => procstart::Victims {
                    pids: Vec::new(),
                    group: Some(group),
                },
                None => continue,
            },
            procstart::Probe::Alive(Some(now)) if procstart::comparable(&recorded, &now) => {
                if !procstart::same_process(&recorded, &now) {
                    continue;
                }
                procstart::claim_victims(pid)
            }
            procstart::Probe::Alive(_) | procstart::Probe::Unknown => {
                if st.told.insert(claim) {
                    log::note(
                        &shared.logs,
                        &format!(
                            "daemon: claim {claim} is ending but pid {pid} cannot be told from its recorded start ({recorded:?}); not signalled"
                        ),
                    );
                }
                continue;
            }
        };
        live.insert(key.clone());
        let new = !st.sent.contains_key(&key);
        let sent = st.sent.entry(key).or_insert_with(|| Sent {
            termed: Instant::now(),
            killed: false,
        });
        let sig = if new {
            Sig::Term
        } else if sent.termed.elapsed() >= grace {
            if sent.killed && st.survived.insert(claim) {
                log::note(
                    &shared.logs,
                    &format!(
                        "daemon: the process of claim {claim} (pid {pid}) survived SIGKILL; still sending it"
                    ),
                );
            }
            sent.killed = true;
            Sig::Kill
        } else {
            continue;
        };
        procstart::signal(&victims.pids, sig);
        if let Some(g) = victims.group {
            procstart::signal_group(g, sig);
        }
        touched.push(pid);
    }
    st.sent.retain(|k, _| live.contains(k));
    st.told.retain(|c| claims.contains(c));
    st.survived.retain(|c| claims.contains(c));
    drop(guard);
    if !touched.is_empty() {
        log::note(
            &shared.logs,
            &format!(
                "daemon: re-sent the kill of ending claims to {} process(es): {touched:?}",
                touched.len()
            ),
        );
    }
    touched
}
