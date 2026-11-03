//! Which account a run starts on: `account_status`, the policies and `pick` of `kernel/lib/kith_accounts.py`. The settings
//! order the set (the pin, then the set by policy), the state file says which accounts are out, and the harness's own status
//! command says which are signed out.

use super::state::{Mark, usage_level};
use super::{
    Ctx, Map, agent_harness, agent_md_of, check, fix, layout, parse, present, settings,
    static_order,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Where an account stands: `ready`, `out`, `signed_out`, `unsupported` or `invalid`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // `status` is the word the JSON view uses
pub struct Status {
    pub status: &'static str,
    pub why: Option<String>,
    pub fix: Option<String>,
    pub out_until: Option<i64>,
    pub reason: Option<String>,
}

impl Status {
    fn of(status: &'static str, why: Option<String>) -> Status {
        Status {
            status,
            why,
            fix: None,
            out_until: None,
            reason: None,
        }
    }
}

/// `account_status`. `deep` asks the harness whether the login is good; not deep only looks for the folder or the granted
/// key. `assume_default`: the default login counts as signed in (the host may hold its keys where `auth status` can't see
/// them), as the main account always did.
pub fn account_status(
    ctx: &Ctx<'_>,
    entry: &str,
    agent: &str,
    out: &HashMap<String, Mark>,
    deep: bool,
    assume_default: bool,
) -> Status {
    let (harness, name) = match parse(entry) {
        Ok(p) => p,
        Err(e) => return Status::of("invalid", Some(e)),
    };
    if !["claude", "claude-api", "codex"].contains(&harness.as_str()) {
        return Status::of(
            "unsupported",
            Some(format!("{harness} is not supported yet")),
        );
    }
    if let Some(o) = out.get(entry) {
        return Status {
            status: "out",
            why: Some("out of usage".to_owned()),
            fix: None,
            out_until: Some(o.until),
            reason: Some(o.reason.clone()),
        };
    }
    let (ready, why) = if assume_default && name.is_none() && harness != "claude-api" {
        (true, None)
    } else if deep {
        check(ctx, entry, agent).unwrap_or((false, None))
    } else {
        present(ctx, entry, agent).unwrap_or((false, None))
    };
    if ready {
        return Status::of("ready", None);
    }
    Status {
        fix: fix(entry, agent).ok().flatten(),
        ..Status::of("signed_out", why)
    }
}

/// `account_status` for each id (once each). Python runs the harness status commands side by side; here they go one after
/// the other, which asks the same questions.
pub fn statuses(
    ctx: &Ctx<'_>,
    ids: &[String],
    agent: &str,
    out: &HashMap<String, Mark>,
    deep: bool,
    assume_default: bool,
) -> HashMap<String, Status> {
    let mut res = HashMap::new();
    for i in ids {
        if !res.contains_key(i) {
            res.insert(
                i.clone(),
                account_status(ctx, i, agent, out, deep, assume_default),
            );
        }
    }
    res
}

fn started(state: &Map, id: &str) -> f64 {
    state
        .get(id)
        .and_then(|e| e.get("last_started"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

/// The account with the most room first: the lowest fullest-window percent of a fresh reading (no reading counts as empty),
/// ties by the one started least recently, then the set's order.
fn spread(ids: &[String], state: &Map, now: f64) -> Vec<String> {
    let key = |n: usize| {
        let level = usage_level(state.get(&ids[n]), now).unwrap_or(0.0);
        (level, started(state, &ids[n]), n)
    };
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by(|a, b| {
        let (ka, kb) = (key(*a), key(*b));
        ka.0.total_cmp(&kb.0)
            .then(ka.1.total_cmp(&kb.1))
            .then(ka.2.cmp(&kb.2))
    });
    order.into_iter().map(|n| ids[n].clone()).collect()
}

/// An account that was left out of the candidates, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub id: String,
    pub status: Status,
}

/// The ordered candidate list for a run of the agent (`kith account pick AGENT --json`).
#[derive(Debug)]
pub struct Pick {
    pub harness: String,
    pub policy: String,
    pub pin: Option<settings::Pin>,
    pub candidates: Vec<String>,
    pub skipped: Vec<Skipped>,
    pub all_out: bool,
}

/// The pin (preferred, or only) first, then the agent's set in policy order; accounts that are out or signed out are skipped.
/// When every candidate is skipped the first of the static order is the only candidate (`all_out`): it runs and the harness
/// says what is wrong, as the main account always did.
pub fn pick(
    ctx: &Ctx<'_>,
    s: &Map,
    agent: &str,
    agent_md: Option<&Path>,
    deep: bool,
    now: f64,
    state: &Map,
) -> Pick {
    let harness = match agent_md {
        Some(p) if !p.as_os_str().is_empty() => agent_harness(Some(p)),
        _ => agent_harness(agent_md_of(ctx, agent).as_deref()),
    };
    let out = super::state::out_marks(&ctx.machine, state, now);
    let (order, _) = static_order(s, agent, &harness);
    let policy = settings::policy(s);
    let l = layout(s, agent, &harness);
    let mut ordered: Vec<String> = if l.pin.is_some() {
        order.iter().take(1).cloned().collect()
    } else {
        vec![]
    };
    if !l.pin.as_ref().is_some_and(|p| p.only) {
        if policy == "spread" {
            ordered.extend(spread(&l.own, state, now));
        } else {
            ordered.extend(l.own.iter().cloned());
        }
        ordered.extend(l.foreign.iter().cloned());
    }
    let status = statuses(ctx, &ordered, agent, &out, deep, !deep);
    let mut candidates: Vec<String> = ordered
        .iter()
        .filter(|i| status[*i].status == "ready")
        .cloned()
        .collect();
    let skipped: Vec<Skipped> = ordered
        .iter()
        .filter(|i| status[*i].status != "ready")
        .map(|i| Skipped {
            id: i.clone(),
            status: status[i].clone(),
        })
        .collect();
    let all_out = candidates.is_empty();
    if all_out {
        candidates = order.into_iter().take(1).collect();
    }
    Pick {
        harness,
        policy,
        pin: l.pin,
        candidates,
        skipped,
        all_out,
    }
}
