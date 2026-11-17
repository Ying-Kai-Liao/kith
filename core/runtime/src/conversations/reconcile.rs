//! Steps 5 and 7 of the start protocol (`claims.rs` has the others): what a core restart (5, [`reconcile`]) and a holder
//! instance the core did not record (7, [`different_holder`]) do to the claims the holder does not list.
//!
//! **Rule K** (design 2026-11-16-core-queued-runner, `HostRuns.tla` `KeepLiveHost`): step 5 leaves the serve host's own claims
//! alone while they can still be the host's: a `claimed` one (no process yet: the host's redeem or stop decides it, and a dead
//! host's goes at the next host start's adopt) and a `running` one whose recorded process is alive on this Mac (the probe
//! [`end_gone`](super::claims::end_gone) uses). The host is not a holder and lists nothing at a hello, so without K every core
//! restart (every update) killed the person's in-flight turns. A `running` host claim whose process is gone is reconciled as
//! before. External and holder claims are unchanged.

use std::collections::HashSet;

use rusqlite::{Transaction, params};

use super::claims::{
    HOST_INSTANCE, Kill, RECONCILED, Released, is_host_instance, kill_of, reconciled,
    recorded_gone, requeue_turn, row,
};
use crate::db::{Core, RecordedProc};
use crate::error::{Error, Result};
use crate::gate::{Liveness, System};

/// A claim and the process its row records, as step 5 probed it.
type Probed = (i64, i64, String);

/// The host's `running` claims (at or below `high_water`) whose recorded process the system says is gone: the only host
/// claims step 5 reconciles (rule K). Read and probed outside any transaction, as [`end_gone`](super::claims::end_gone)
/// does; the transaction then acts only on a row that still names the same process. A running host claim with no
/// recorded process cannot be probed and counts as gone.
fn host_gone(core: &Core, live: &dyn Liveness, high_water: i64) -> Result<HashSet<Probed>> {
    let mut rows: Vec<(i64, Option<i64>, Option<String>, String)> = Vec::new();
    {
        let mut st = core.conn().prepare(
            "SELECT claim_id, pid, pid_started, holder_instance FROM claims
             WHERE state = 'running' AND kind <> 'external' AND holder_instance IS NOT NULL AND claim_id <= ?1",
        )?;
        for r in st.query_map([high_water], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })? {
            rows.push(r?);
        }
    }
    Ok(rows
        .into_iter()
        .filter(|(_, _, _, holder)| is_host_instance(holder))
        .filter_map(|(id, pid, started, _)| {
            let gone = match (pid, started.as_deref()) {
                (Some(p), Some(s)) => u32::try_from(p).is_ok_and(|p| recorded_gone(live, p, s)),
                _ => true,
            };
            gone.then(|| (id, pid.unwrap_or(-1), started.unwrap_or_default()))
        })
        .collect())
}

/// The claims the holder's listing does not cover, ended or moved to `ending` (steps 5 and 7). `foreign_to` limits it
/// to claims of another holder instance (step 7), never the host's ([`HOST_INSTANCE`]); `None` is step 5, which takes
/// every live claim but the host's that may still be live (rule K, the module doc): `host_gone` names the host's
/// `running` claims whose process is gone, the only host claims step 5 takes; a host claim that is `claimed` stays.
fn release_unlisted(
    tx: &Transaction<'_>,
    listed: &HashSet<i64>,
    foreign_to: Option<&str>,
    host_gone: &HashSet<Probed>,
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let mut out = Released::default();
    #[allow(
        clippy::type_complexity,
        reason = "one row of the query below, used once"
    )]
    let mut live: Vec<(
        i64,
        String,
        bool,
        Option<i64>,
        Option<String>,
        Option<String>,
    )> = Vec::new();
    {
        // Only claims at or below the high-water mark read before the holder's list: a newer one may simply be missing
        // from a list that was read before it was taken.
        let mut st = tx.prepare(
            "SELECT claim_id, state, kind = 'external', pid, pid_started, holder_instance FROM claims
             WHERE state IN ('claimed', 'running', 'ending') AND claim_id <= ?2
               AND (?1 IS NULL OR holder_instance IS NULL
                    OR (holder_instance <> ?1
                        AND NOT (holder_instance = ?3 OR substr(holder_instance, 1, length(?3) + 1) = ?3 || '/')))",
        )?;
        for r in st.query_map(params![foreign_to, high_water, HOST_INSTANCE], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })? {
            live.push(r?);
        }
    }
    for (id, state, external, pid, started, holder) in live {
        if listed.contains(&id) {
            continue;
        }
        // A `--direct` run has no stub or holder entry: the holder not listing it says nothing. `end_gone` ends it
        // when its recorded pid is gone; a stop's kill is still re-sent.
        if external {
            if state == "ending" {
                out.kill.push(kill_of(id, pid, started));
            }
            continue;
        }
        // Rule K. Step 7 never lists a host claim here (the query leaves them out).
        if foreign_to.is_none() && holder.as_deref().is_some_and(is_host_instance) {
            let probed = (id, pid.unwrap_or(-1), started.clone().unwrap_or_default());
            let keep = match state.as_str() {
                "claimed" => true,
                // Redeemed after the probe read it: a process that redeemed just now is alive.
                "running" => !host_gone.contains(&probed),
                _ => false,
            };
            if keep {
                continue;
            }
        }
        match state.as_str() {
            // Never redeemed: end it by CAS so a late redeem fails, and give the turn back.
            "claimed" => {
                let n = tx.execute(
                    "UPDATE claims SET state = 'ended', ended_at = ?2, end_reason = 'holder_changed'
                     WHERE claim_id = ?1 AND state = 'claimed'",
                    params![id, now],
                )?;
                if n == 1 {
                    requeue_turn(tx, id)?;
                    out.ended.push(id);
                }
            }
            // Redeemed: the process may still write. It stays the writer, as `ending`, until its pid is gone.
            "running" => {
                let n = tx.execute(
                    "UPDATE claims SET state = 'ending', end_reason = 'holder_changed'
                     WHERE claim_id = ?1 AND state = 'running'",
                    [id],
                )?;
                if n == 1 {
                    requeue_turn(tx, id)?;
                }
                let k = kill_of(id, pid, started);
                out.holder_kill.push((k.clone(), holder));
                out.kill.push(k);
            }
            // Already ending: with a process, wait for it; without one it was never redeemed and now cannot be.
            _ => {
                if pid.is_some() {
                    let k = kill_of(id, pid, started);
                    out.holder_kill.push((k.clone(), holder));
                    out.kill.push(k);
                } else {
                    let n = tx.execute(
                        "UPDATE claims SET state = 'ended', ended_at = ?2
                         WHERE claim_id = ?1 AND state = 'ending'",
                        params![id, now],
                    )?;
                    if n == 1 {
                        out.ended.push(id);
                    }
                }
            }
        }
    }
    let mut known: HashSet<(i64, String)> = HashSet::new();
    {
        // Only live claims count: an old-instance process whose claim ended must still be killed (step 7).
        let mut st = tx.prepare(
            "SELECT pid, pid_started FROM claims
             WHERE state IN ('claimed', 'running', 'ending') AND pid IS NOT NULL AND pid_started IS NOT NULL",
        )?;
        for r in st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            known.insert(r?);
        }
    }
    for p in procs {
        if !known.contains(&(i64::from(p.pid), p.started.clone())) {
            out.kill_procs.push(Kill {
                claim_id: None,
                pid: Some(p.pid),
                started: Some(p.started.clone()),
            });
        }
    }
    Ok(out)
}

/// Step 5: after the generation bump and hello, one transaction releases the claims the holder does not list, lists the
/// holder entries to kill (their claim is ended or unknown), and marks this generation reconciled, so claims open.
/// `listed` is the holder's claim ids; `procs` the old instance's recorded processes with no redeem (step 7), if any;
/// `high_water` is [`claim_high_water`](super::claims::claim_high_water) read before the holder was asked. The host's
/// claims are probed on this system (rule K). A second reconcile in one generation is refused.
pub(crate) fn reconcile(
    core: &mut Core,
    listed: &[i64],
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    reconcile_with(core, &System, listed, procs, high_water, now)
}

/// [`reconcile`] with the host's processes probed through `live`.
pub(crate) fn reconcile_with(
    core: &mut Core,
    live: &dyn Liveness,
    listed: &[i64],
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let generation = core.generation();
    let gone = host_gone(core, live, high_water)?;
    let tx = core.claim_tx()?;
    if reconciled(&tx)? == Some(generation) {
        return Err(Error::Refused(format!(
            "kith-core: generation {generation} is already reconciled"
        )));
    }
    let set: HashSet<i64> = listed.iter().copied().collect();
    let mut out = release_unlisted(&tx, &set, None, &gone, procs, high_water, now)?;
    for id in listed {
        let state = row(&tx, *id)?.map(|r| r.state);
        if !matches!(state.as_deref(), Some("claimed" | "running" | "ending")) {
            out.kill_listed.push(*id);
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![RECONCILED, generation.to_string()],
    )?;
    tx.commit()?;
    Ok(out)
}

/// Step 7: the core's hello met a holder instance other than the one it recorded. Claims of any other instance that the
/// new one does not list: `claimed` ones are ended by CAS (a late redeem fails), `running` ones go to `ending` and are
/// killed by their recorded pid until confirmed gone ([`end_gone`](super::claims::end_gone)), never straight to
/// `ended`. The host's claims are never another instance's here. `procs` are the old instance's recorded processes with
/// no redeem. Does not open claims; [`reconcile`] does.
pub(crate) fn different_holder(
    core: &mut Core,
    new_instance: &str,
    listed: &[i64],
    procs: &[RecordedProc],
    high_water: i64,
    now: i64,
) -> Result<Released> {
    let tx = core.claim_tx()?;
    let set: HashSet<i64> = listed.iter().copied().collect();
    let out = release_unlisted(
        &tx,
        &set,
        Some(new_instance),
        &HashSet::new(),
        procs,
        high_water,
        now,
    )?;
    tx.commit()?;
    Ok(out)
}
