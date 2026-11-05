//! The ask tree in the core (M3-8a, lane "children"): delegation records as rows of `asks` (migration 100) instead of
//! `live/ask/*.reg` files. Not switched: `kernel/bin/kith-ask` stays the writer of the files until the M3-10 switch; the
//! import (`import/asks.rs`) copies them in, and nothing here runs on its own (no sweep).
//!
//! The caps are the kernel's own rules, not a copy: `kith_kernel::delegation` (`lineage_with`, `chain_costs`, `decide`,
//! `chain_cost_cap`) is fed records built from rows instead of files. What the file world did with the box lock is ONE
//! `BEGIN IMMEDIATE` transaction here: [`open`] reads every record, decides and inserts under it, so no second ask can pass
//! the cap between the check and the open. Chain cost counts every record of the chain, a crashed child's too (a child that
//! died keeps its `cost` and its row until it is forgotten), as `chain_costs` does.
//!
//! An open adds no cost, so two opens can never take a chain over its cap between them; what the transaction excludes is a
//! second open of the same id, and an open racing a settle that tips the cap.
//!
//! Stopping (M3-8b): [`end_tree`] settles a tree of asks `stopped`, stops what each child's conversation runs through the
//! claims API and withdraws the wakes the callers are owed through the wake lane. It kills no process: it returns the kills.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use kith_kernel::delegation::{self as d, Reg};
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde_json::{Map, Value, json};

use crate::conversations::claims::{self, Kill};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::wake;

pub(crate) mod call;
#[cfg(test)]
mod tests;

pub(crate) use call::{call, serves};

/// Characters of an ask's text a record keeps (`BRIEF_MAX` in kith-ask).
const BRIEF_MAX: usize = 2000;

const COLS: &str = "id, chain, parent, reply_session, hop, cross_hops, crossed, asked, caller_agent, caller_person, \
caller_via, conversation, cost, cost_base, cap_usd, state, at_ms, answered_ms, capped_ms, cap_card_ms, ended_ms, pid, \
pid_at_ms, brief, done, delivered, woke_answer, extra, claim_id";

/// One row of `asks`: the record of one `kith ask`.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "the record's own flags")]
pub(crate) struct Ask {
    pub id: String,
    pub chain: String,
    pub parent: Option<String>,
    pub reply_session: Option<String>,
    pub hop: i64,
    pub cross_hops: i64,
    pub crossed: bool,
    pub asked: Option<String>,
    pub caller_agent: Option<String>,
    pub caller_person: Option<String>,
    pub caller_via: Option<String>,
    pub conversation: Option<String>,
    pub cost: Option<f64>,
    pub cost_base: Option<f64>,
    pub cap_usd: Option<f64>,
    pub state: String,
    pub at_ms: Option<i64>,
    pub answered_ms: Option<i64>,
    pub capped_ms: Option<i64>,
    pub cap_card_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub pid: Option<i64>,
    pub pid_at_ms: Option<i64>,
    pub brief: Option<String>,
    pub done: bool,
    pub delivered: bool,
    pub woke_answer: bool,
    /// Every other key of an imported record, so nothing it held is lost.
    pub extra: Option<Map<String, Value>>,
    /// The claim the ask's run started (code-only link to the conversations lane, no foreign key). Not part of the
    /// record `kith ask` prints. NULL for a row from before the column: stop then matches the run by `pid`.
    pub claim_id: Option<i64>,
}

const STATES: [&str; 4] = ["open", "answered", "capped", "stopped"];

fn take_str(rest: &mut Reg, key: &str) -> Option<String> {
    match rest.get(key) {
        Some(Value::String(_)) => rest.remove(key).and_then(|v| v.as_str().map(str::to_owned)),
        Some(Value::Null) => {
            rest.remove(key);
            None
        }
        _ => None, // a value of the wrong type stays in `extra`
    }
}

fn take_num(rest: &mut Reg, key: &str) -> Option<f64> {
    match rest.get(key) {
        Some(Value::Number(n)) => {
            let n = n.as_f64();
            rest.remove(key);
            n
        }
        Some(Value::Null) => {
            rest.remove(key);
            None
        }
        _ => None,
    }
}

fn take_int(rest: &mut Reg, key: &str) -> Option<i64> {
    if let Some(n) = rest.get(key).and_then(Value::as_i64) {
        rest.remove(key);
        return Some(n);
    }
    if rest.get(key).is_some_and(Value::is_null) {
        rest.remove(key);
    }
    None
}

/// A time in seconds (float) as ms, the way the view prints it (`int(t * 1000)`); zero and null are no time.
#[allow(clippy::cast_possible_truncation)]
fn take_ms(rest: &mut Reg, key: &str) -> Option<i64> {
    take_num(rest, key)
        .filter(|t| *t != 0.0)
        .map(|t| (t * 1000.0) as i64)
}

fn take_flag(rest: &mut Reg, key: &str) -> bool {
    d::truthy(rest.remove(key).as_ref())
}

impl Ask {
    /// The row of a record as the code reads it (`upgrade`d). `None` when its state is none the table can hold.
    pub(crate) fn from_reg(id: &str, reg: &Reg) -> Option<Self> {
        let mut rest = reg.clone();
        let state = rest.get("state").and_then(Value::as_str)?.to_owned();
        if !STATES.contains(&state.as_str()) {
            return None;
        }
        rest.remove("state");
        let (mut parent, mut reply_session) = (None, None);
        if let Some(Value::Object(mut rt)) = rest.remove("reply_to") {
            reply_session = take_str(&mut rt, "session");
            parent = take_str(&mut rt, "ask");
        }
        let chain = take_str(&mut rest, "chain").unwrap_or_else(|| id.to_owned());
        let crossed = take_flag(&mut rest, "crossed");
        let hop = take_int(&mut rest, "hop").unwrap_or(0);
        let cross_hops = take_int(&mut rest, "cross_hops").unwrap_or(0);
        let ask = Self {
            id: id.to_owned(),
            chain,
            parent,
            reply_session,
            hop,
            cross_hops,
            crossed,
            asked: take_str(&mut rest, "asked"),
            caller_agent: take_str(&mut rest, "agent"),
            caller_person: take_str(&mut rest, "person"),
            caller_via: take_str(&mut rest, "via"),
            conversation: None,
            cost: take_num(&mut rest, "cost"),
            cost_base: take_num(&mut rest, "cost_base"),
            cap_usd: take_num(&mut rest, "cap_usd"),
            state,
            at_ms: take_ms(&mut rest, "at"),
            answered_ms: take_ms(&mut rest, "answered_at"),
            capped_ms: take_ms(&mut rest, "capped_at"),
            cap_card_ms: take_ms(&mut rest, "cap_card_at"),
            ended_ms: take_ms(&mut rest, "ended_at"),
            pid: take_int(&mut rest, "pid"),
            pid_at_ms: take_ms(&mut rest, "pid_at"),
            brief: take_str(&mut rest, "brief"),
            done: take_flag(&mut rest, "done"),
            delivered: take_flag(&mut rest, "delivered"),
            woke_answer: take_flag(&mut rest, "woke_answer"),
            extra: None,
            claim_id: None,
        };
        Some(Self {
            extra: (!rest.is_empty()).then_some(rest),
            ..ask
        })
    }

    /// The record as `kith ask` reads it (the shape `read_reg` returns after `upgrade`).
    pub(crate) fn to_reg(&self) -> Reg {
        #[allow(clippy::cast_precision_loss)]
        let secs = |ms: i64| json!(ms as f64 / 1000.0);
        let mut r = self.extra.clone().unwrap_or_default();
        let mut put = |k: &str, v: Value| {
            r.insert(k.to_owned(), v);
        };
        // `open_record` always writes agent, person and via (null when unset); a record without a via is an old one.
        let opened = self.caller_via.is_some();
        for (k, v) in [
            ("asked", &self.asked),
            ("agent", &self.caller_agent),
            ("person", &self.caller_person),
            ("via", &self.caller_via),
            ("brief", &self.brief),
        ] {
            if let Some(v) = v {
                put(k, json!(v));
            } else if opened && matches!(k, "agent" | "person") {
                put(k, Value::Null);
            }
        }
        put(
            "reply_to",
            json!({"session": self.reply_session, "ask": self.parent}),
        );
        put("chain", json!(self.chain));
        put("hop", json!(self.hop));
        put("cross_hops", json!(self.cross_hops));
        put("crossed", json!(self.crossed));
        put("cost", json!(self.cost));
        if let Some(b) = self.cost_base {
            put("cost_base", json!(b));
        }
        if let Some(c) = self.cap_usd {
            put("cap_usd", json!(c));
        }
        put("state", json!(self.state));
        for (k, v) in [
            ("at", self.at_ms),
            ("answered_at", self.answered_ms),
            ("capped_at", self.capped_ms),
            ("cap_card_at", self.cap_card_ms),
            ("ended_at", self.ended_ms),
            ("pid_at", self.pid_at_ms),
        ] {
            if let Some(ms) = v {
                put(k, secs(ms));
            }
        }
        if let Some(p) = self.pid {
            put("pid", json!(p));
        }
        for (k, on) in [
            ("done", self.done),
            ("delivered", self.delivered),
            ("woke_answer", self.woke_answer),
        ] {
            if on {
                put(k, json!(true));
            }
        }
        r
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let extra: Option<String> = r.get(27)?;
        Ok(Self {
            id: r.get(0)?,
            chain: r.get(1)?,
            parent: r.get(2)?,
            reply_session: r.get(3)?,
            hop: r.get(4)?,
            cross_hops: r.get(5)?,
            crossed: r.get::<_, i64>(6)? != 0,
            asked: r.get(7)?,
            caller_agent: r.get(8)?,
            caller_person: r.get(9)?,
            caller_via: r.get(10)?,
            conversation: r.get(11)?,
            cost: r.get(12)?,
            cost_base: r.get(13)?,
            cap_usd: r.get(14)?,
            state: r.get(15)?,
            at_ms: r.get(16)?,
            answered_ms: r.get(17)?,
            capped_ms: r.get(18)?,
            cap_card_ms: r.get(19)?,
            ended_ms: r.get(20)?,
            pid: r.get(21)?,
            pid_at_ms: r.get(22)?,
            brief: r.get(23)?,
            done: r.get::<_, i64>(24)? != 0,
            delivered: r.get::<_, i64>(25)? != 0,
            woke_answer: r.get::<_, i64>(26)? != 0,
            extra: extra.and_then(|t| serde_json::from_str(&t).ok()),
            claim_id: r.get(28)?,
        })
    }

    /// Writes the row, replacing one with the same id.
    pub(crate) fn put(&self, conn: &Connection) -> Result<()> {
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO asks ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)"
            ),
            params![
                self.id,
                self.chain,
                self.parent,
                self.reply_session,
                self.hop,
                self.cross_hops,
                i64::from(self.crossed),
                self.asked,
                self.caller_agent,
                self.caller_person,
                self.caller_via,
                self.conversation,
                self.cost,
                self.cost_base,
                self.cap_usd,
                self.state,
                self.at_ms,
                self.answered_ms,
                self.capped_ms,
                self.cap_card_ms,
                self.ended_ms,
                self.pid,
                self.pid_at_ms,
                self.brief,
                i64::from(self.done),
                i64::from(self.delivered),
                i64::from(self.woke_answer),
                self.extra
                    .as_ref()
                    .map(|e| Value::Object(e.clone()).to_string()),
                self.claim_id,
            ],
        )?;
        Ok(())
    }
}

/// Every ask row, in id order (the order `records()` lists files in).
pub(crate) fn load_all(conn: &Connection) -> Result<Vec<Ask>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM asks ORDER BY id"))?;
    let rows = stmt.query_map([], Ask::from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// One ask row.
pub(crate) fn get(conn: &Connection, id: &str) -> Result<Option<Ask>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM asks WHERE id = ?1"))?;
    let mut rows = stmt.query_map([id], Ask::from_row)?;
    Ok(rows.next().transpose()?)
}

fn regs(asks: &[Ask]) -> BTreeMap<String, Reg> {
    asks.iter().map(|a| (a.id.clone(), a.to_reg())).collect()
}

fn refuse(r: d::Refusal) -> Error {
    Error::Refused(r.0)
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// What an ask opens with.
pub(crate) struct Open<'a> {
    pub id: &'a str,
    pub asked: &'a str,
    pub crossing: bool,
    pub brief: Option<&'a str>,
    pub pid: Option<i64>,
    /// The asked agent's session of a `--bg` run, when it already has one.
    pub conversation: Option<&'a str>,
    /// The claim the run started, when it already has one (stop and kill then match by it, not by `pid`).
    pub claim_id: Option<i64>,
}

/// Opens ask `o.id`: the cap check and the insert as one transaction (`open_record` under the box lock). `env` is the
/// caller's view (`KITH_ASK_*` for the lineage, `KITH_AGENT`, `KITH_PERSON`, `KITH_VIA`, `CLAUDE_CODE_SESSION_ID` for the
/// record). A cap that refuses it is [`Error::Refused`] with the words `kith ask` prints, and writes nothing. With
/// `check_only` the decision is made and nothing is written. Answers `{decision, record}` (`record` null on a check).
pub(crate) fn open(
    core: &mut Core,
    machine: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    o: &Open<'_>,
    check_only: bool,
    now: i64,
) -> Result<Value> {
    let tx = core.claim_tx()?;
    let out = open_in(&tx, machine, env, o, check_only, now)?;
    tx.commit()?;
    Ok(out)
}

/// [`open`] inside a transaction the caller began (`BEGIN IMMEDIATE`) and commits: the check and the insert are one step
/// because the transaction is. (A test runs it on two connections to prove that.)
pub(crate) fn open_in(
    tx: &Connection,
    machine: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    o: &Open<'_>,
    check_only: bool,
    now: i64,
) -> Result<Value> {
    if !d::safe_id(o.id) {
        return Err(Error::BadParams(format!(
            "kith-core: asks: {:?} is not an ask id",
            o.id
        )));
    }
    let asks = load_all(tx)?;
    let by_id = regs(&asks);
    let place = d::lineage_with(&|id| by_id.get(id).cloned(), env, o.crossing).map_err(refuse)?;
    // Under a stopped ask, at any depth, nothing new may open: its tree is ended (an open that won the race is ended with
    // it). The check answers what an open would.
    let mut up = place.ask.as_deref();
    for _ in 0..=asks.len() {
        let Some(p) = up.and_then(|p| asks.iter().find(|a| a.id == p)) else {
            break;
        };
        if p.state == "stopped" || p.ended_ms.is_some() {
            return Err(Error::Refused(format!(
                "kith-core: asks: ask {} was stopped; it cannot open another",
                p.id
            )));
        }
        up = p.parent.as_deref();
    }
    let costs = if place.chain.is_some() {
        d::chain_costs(&by_id)
    } else {
        HashMap::new()
    };
    d::decide(&place, &costs, d::chain_cost_cap(machine)).map_err(refuse)?;
    let decision = d::decision_json(&place, o.id);
    if check_only {
        return Ok(json!({"decision": decision, "record": null}));
    }
    if by_id.contains_key(o.id) {
        return Err(Error::Refused(format!(
            "kith-core: asks: ask {} already has a record",
            o.id
        )));
    }
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    // The id decides which external claim `ask --end` stops, so it must be the run's own: an `external` claim on the ask's
    // conversation with the ask's pid and person. A wrong or stale id could otherwise stop another person's `--direct`
    // writer.
    if let Some(claim) = o.claim_id {
        let ours: Option<(String, String, Option<i64>, String)> = tx
            .query_row(
                "SELECT kind, conversation, pid, person FROM claims WHERE claim_id = ?1",
                [claim],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let person = get("KITH_PERSON").unwrap_or_else(|| "owner".to_owned());
        let fits = ours.is_some_and(|(kind, conv, pid, who)| {
            kind == "external"
                && o.conversation == Some(conv.as_str())
                && o.pid.is_some()
                && pid == o.pid
                && who == person
        });
        if !fits {
            return Err(Error::Refused(format!(
                "kith-core: asks: claim {claim} is not this ask's own run (an external claim on its conversation, with its pid and person)"
            )));
        }
    }
    let ask = Ask {
        id: o.id.to_owned(),
        chain: place.chain.clone().unwrap_or_else(|| o.id.to_owned()),
        parent: place.ask.clone(),
        reply_session: get("CLAUDE_CODE_SESSION_ID"),
        hop: place.hop,
        cross_hops: place.cross_hops,
        crossed: place.crossed,
        asked: Some(o.asked.to_owned()),
        caller_agent: get("KITH_AGENT"),
        caller_person: get("KITH_PERSON"),
        caller_via: Some(get("KITH_VIA").unwrap_or_else(|| "terminal".to_owned())),
        conversation: o.conversation.map(str::to_owned),
        cost: Some(0.0),
        cost_base: Some(place.cost_base),
        cap_usd: None,
        state: "open".to_owned(),
        at_ms: Some(now),
        answered_ms: None,
        capped_ms: None,
        cap_card_ms: None,
        ended_ms: None,
        pid: o.pid,
        pid_at_ms: o.pid.map(|_| now),
        brief: o
            .brief
            .filter(|b| !b.is_empty())
            .map(|b| b.chars().take(BRIEF_MAX).collect()),
        done: false,
        delivered: false,
        woke_answer: false,
        extra: None,
        claim_id: o.claim_id,
    };
    ask.put(tx)?;
    Ok(json!({"decision": decision, "record": Value::Object(ask.to_reg())}))
}

/// The run behind `id` ended a turn at `cost` US$: it joins its chain's cost, and a chain that reaches its cap with this
/// turn is marked (`capped_ms`, once per cap). `state` is `answered`, or `stopped` when the run gave no answer; None while
/// it still waits on a card. Answers `None` when there is no such ask, else `{id, state, cost, tipped}`.
pub(crate) fn settle(
    core: &mut Core,
    machine: &Path,
    id: &str,
    cost: f64,
    state: Option<&str>,
    now: i64,
) -> Result<Option<Value>> {
    let tx = core.claim_tx()?;
    let out = settle_in(&tx, machine, id, cost, state, now)?;
    tx.commit()?;
    Ok(out)
}

/// [`settle`] inside a transaction the caller began and commits.
pub(crate) fn settle_in(
    tx: &Connection,
    machine: &Path,
    id: &str,
    cost: f64,
    state: Option<&str>,
    now: i64,
) -> Result<Option<Value>> {
    let mut asks = load_all(tx)?;
    let Some(i) = asks.iter().position(|a| a.id == id) else {
        return Ok(None);
    };
    asks[i].cost = Some(round6(asks[i].cost.unwrap_or(0.0) + cost));
    let cap = d::chain_cost_cap(machine);
    let chain = asks[i].chain.clone();
    let spent = d::chain_costs(&regs(&asks))
        .get(&chain)
        .copied()
        .unwrap_or(0.0);
    // A new cap reached is a new card: the old one's mark does not stand for it.
    let tipped = cap.is_some_and(|cap| {
        spent >= cap
            && !asks.iter().any(|r| {
                r.chain == chain && r.capped_ms.is_some() && r.cap_usd.unwrap_or(0.0) >= cap
            })
    });
    let a = &mut asks[i];
    if tipped {
        a.capped_ms = Some(now);
        a.cap_usd = cap;
        a.cap_card_ms = None;
    }
    if let Some(s) = state
        && a.state == "open"
    {
        if tipped { "capped" } else { s }.clone_into(&mut a.state);
        a.answered_ms = Some(now);
    }
    a.put(tx)?;
    Ok(Some(
        json!({"id": a.id, "state": a.state, "cost": a.cost, "tipped": tipped}),
    ))
}

/// `kith ask --cap-told CHAIN`: the host raised the chain's cap card; it is not raised again for this cap. Answers how
/// many records were marked.
pub(crate) fn cap_told(core: &mut Core, chain: &str, now: i64) -> Result<usize> {
    let tx = core.claim_tx()?;
    let n = tx.execute(
        "UPDATE asks SET cap_card_ms = ?2 WHERE chain = ?1 AND capped_ms IS NOT NULL",
        params![chain, now],
    )?;
    tx.commit()?;
    Ok(n)
}

/// What [`end_tree`] did.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Ended {
    /// The asks settled `stopped` by this call.
    pub ids: Vec<String>,
    /// The sessions whose wakes and waiters were withdrawn: the ones named, the callers' sessions and the `--bg` runs' own.
    pub sessions: Vec<String>,
    /// Writers to kill, from the claims of the children's conversations (to be re-sent until gone).
    pub kill: Vec<Kill>,
}

/// What to end: the roots of trees, the callers' sessions, the people. Any of them may be empty.
#[derive(Debug, Default)]
pub(crate) struct EndReq<'a> {
    /// Asks whose whole subtree ends (the ask itself and every descendant by `parent`).
    pub roots: &'a [String],
    pub sessions: &'a [String],
    pub persons: &'a [String],
}

/// Stop, revoke and removal (R5, R8), M3-8b: every ask whose caller is one of `sessions` (`reply_to.session`) or `persons`
/// (the person it was asked for), or that is one of `roots`, is settled `stopped` and marked ended, with every descendant
/// of those (a child's own children). The same transaction stops what each of them runs through the claims API: its
/// conversation's queued and parked turns are cancelled and its live claim goes to `ending` (an unspawned one ends), and the
/// kills come back. Then the wakes and waiters the callers are owed are withdrawn through the wake lane. One that was already
/// delivered or answered to its caller is history and stays; so does its cost: chain cost keeps counting every record.
/// A record with no session belongs to no lane, unless its person is named.
///
/// An answer, report or settle of a stopped ask is recorded (its cost joins the chain) but changes no state, so nothing
/// wakes the caller afterwards ([`settle_in`] only moves an `open` ask). An [`open`] under a stopped ask is refused, so an
/// open racing the end either lands first (and the end takes it with the tree) or is refused.
///
/// Repeating the call is safe: asks already ended are not settled again, but their conversations are stopped again and their
/// callers' wakes withdrawn again. The stop is one transaction; a stopped ask's conversation also takes no new ask or wake
/// turn ([`claims::claim`] refuses it), so a wake given back afterwards cannot restart it.
///
/// Persons match the record's own `person` only: the `--bg` job's person (`ask-bg/ID.json`) is always the same value, since
/// the child inherits `KITH_PERSON` from the run that opened the record, so nothing is lost by not storing it.
pub(crate) fn end_tree(core: &mut Core, req: &EndReq<'_>, now: i64) -> Result<Ended> {
    let tx = core.claim_tx()?;
    let mut out = Ended::default();
    let mut owed: std::collections::BTreeSet<String> = req.sessions.iter().cloned().collect();
    let asks = load_all(&tx)?;
    let mut hit: std::collections::BTreeSet<&str> = asks
        .iter()
        .filter(|a| {
            req.roots.contains(&a.id)
                || a.reply_session
                    .as_ref()
                    .is_some_and(|s| req.sessions.contains(s))
                || a.caller_person
                    .as_ref()
                    .is_some_and(|p| req.persons.contains(p))
        })
        .map(|a| a.id.as_str())
        .collect();
    // The subtree: whatever hangs off a hit, to any depth.
    loop {
        let more: Vec<&str> = asks
            .iter()
            .filter(|a| {
                !hit.contains(a.id.as_str()) && a.parent.as_deref().is_some_and(|p| hit.contains(p))
            })
            .map(|a| a.id.as_str())
            .collect();
        if more.is_empty() {
            break;
        }
        hit.extend(more);
    }
    let mut conversations: Vec<(String, claims::AskRun)> = Vec::new();
    for mut a in asks.iter().filter(|a| hit.contains(a.id.as_str())).cloned() {
        // A delivered or answered ask is history: its record and its caller's wakes stay, but its run stops and the
        // record is marked ended, so nothing opens under it or starts its conversation again.
        let history = a.delivered || a.woke_answer;
        if a.ended_ms.is_none() {
            if !history {
                "stopped".clone_into(&mut a.state);
                a.done = true;
                a.answered_ms = a.answered_ms.or(Some(now));
                out.ids.push(a.id.clone());
            }
            a.ended_ms = Some(now);
            a.put(&tx)?;
        }
        if !history {
            owed.extend(a.reply_session.clone());
        }
        owed.extend(a.conversation.clone());
        if let Some(c) = a.conversation {
            conversations.push((
                c,
                claims::AskRun {
                    pid: a.pid,
                    claim_id: a.claim_id,
                },
            ));
        }
    }
    for (c, run) in conversations.iter().filter(|(c, _)| !c.is_empty()) {
        out.kill.extend(claims::stop_conversation_in(
            &tx,
            c,
            *run,
            "ask stopped",
            now,
        )?);
    }
    out.ids.sort();
    owed.remove("");
    out.sessions = owed.into_iter().collect();
    // The wakes go in the same step as the records and the claims: no window where a stopped ask's answer wakes its caller.
    wake::withdraw_owed_in(&tx, now, &out.sessions, req.persons)?;
    tx.commit()?;
    Ok(out)
}

/// `kith ask --to ID`: whether a follow-up may be sent to the ask. The stopped fence never lifts, so an ended or stopped ask
/// refuses at send ("ask anew"): the Python `send_to` queued the message and let the wake fence drop it silently, which
/// the caller could not tell from a delivery. Reads only.
pub(crate) fn check_to(conn: &Connection, id: &str) -> Result<()> {
    let Some(a) = get(conn, id)? else {
        return Err(Error::Refused(format!(
            "kith-core: asks: no ask {id} on this Mac"
        )));
    };
    if a.state == "stopped" || a.ended_ms.is_some() {
        return Err(Error::Stopped(format!(
            "kith-core: ask {id} was stopped; ask anew"
        )));
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn micro(usd: f64) -> i64 {
    (usd * 1e6).round_ties_even() as i64
}

fn view(a: &Ask, costs: &HashMap<String, f64>, cap: Option<f64>) -> Value {
    json!({
        "id": a.id,
        "agent": a.asked.clone().unwrap_or_default(),
        "chain": a.chain,
        "hop": a.hop,
        "cross_hops": a.cross_hops,
        "crossed": a.crossed,
        "caller": {"agent": a.caller_agent, "person": a.caller_person, "via": a.caller_via},
        "reply_to": {"session": a.reply_session, "ask": a.parent},
        "state": a.state,
        "cost_micro_usd": a.cost.map(micro),
        "chain_cost_micro_usd": micro(costs.get(&a.chain).copied().unwrap_or(0.0)),
        "cap_micro_usd": cap.map(micro),
        "started_ms": a.at_ms.unwrap_or(0),
        "answered_ms": a.answered_ms,
        "capped_ms": a.capped_ms,
        "cap_card_ms": a.cap_card_ms,
        "brief": a.brief.as_ref().filter(|b| !b.is_empty()),
        "session": a.conversation.as_ref().filter(|s| !s.is_empty()),
    })
}

/// `kith ask delegations --json`: every record, newest first (equal starts keep id order), in the shape of the contract's
/// `Delegation`: money in micro-dollars, times in ms.
pub(crate) fn delegations(core: &Core, machine: &Path) -> Result<Value> {
    let asks = load_all(core.conn())?;
    let costs = d::chain_costs(&regs(&asks));
    let cap = d::chain_cost_cap(machine);
    let mut rows: Vec<&Ask> = asks.iter().collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.at_ms.unwrap_or(0)));
    Ok(json!({"delegations": rows.into_iter().map(|a| view(a, &costs, cap)).collect::<Vec<_>>()}))
}

/// The record of `id` as `kith ask` reads it, or None.
pub(crate) fn record(core: &Core, id: &str) -> Result<Option<Value>> {
    if !d::safe_id(id) {
        return Ok(None);
    }
    Ok(get(core.conn(), id)?.map(|a| Value::Object(a.to_reg())))
}

/// `kith kernel delegation limits`, and the chain cap in force (`cap_usd`, null for none).
pub(crate) fn limits(machine: &Path) -> Value {
    let mut v = d::limits_json();
    v["cap_usd"] = json!(d::chain_cost_cap(machine));
    v
}
