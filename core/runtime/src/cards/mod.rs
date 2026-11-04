//! The card store in the core (M2-11, lane E): the owner's approval cards over core.db, so a card raised while no app
//! runs can be answered from a terminal (runtime-design section 5).
//!
//! The rules are the host's (`serve/host/src/store/cards.rs`, which still decides until lane H switches its callers),
//! one `BEGIN IMMEDIATE` transaction per change through [`Core::claim_tx`], so two threads of this process never see
//! half a change and a second answer finds the first one's result:
//!
//! ```text
//! pending -> live -> parked            (parked when the asking process is gone)
//!    open (pending|live|parked) -> approved | denied   (an answer; approved is a once grant bound to the session)
//!    open -> used                       (saved: the secret card's write is done, the resume it owes stays owed)
//!    open -> expired | withdrawn        (the clock; the asker cancelled, the person's access ended)
//!    approved -> used | expired | withdrawn
//! ```
//!
//! Only an open card takes an answer, once, and only before its deadline: an answer to a closed card, a second answer
//! and an answer after the deadline change nothing (A1) and say why. An answer that finds the deadline passed closes the
//! card as expired, the same step.
//!
//! The claim is the one thing the host did with memory: a secret card's answer first takes the right to answer
//! (`secret.rs` `Slot`), then pushes the deadline out so the clock cannot close the card mid-write. Here it is one row
//! state: [`claim`] records who holds it and until when, a second holder is refused, a plain [`answer`] is refused
//! while it is held, and the sweep leaves the card alone until the claim's time (or its process) runs out. The holder
//! ends it with [`saved`] (the write is done) or [`release`] (it failed; the card stays open). A holder that gave its
//! pid is checked on every call as the gate checks its holders: a pid that is gone, or now has another start time,
//! drops the claim; a failed probe never does.
//!
//! A claim with a pid is not a timer. While that process is alive the claim holds past its `claim_until`: a second
//! claimer, a plain answer, the sweep and a single-card withdrawal all leave the card alone (a revoke's [`withdraw_people`] is the exception and
//! takes a claimed card too, the holder's `saved` then finds it closed; M2-11 review, precondition 1: a claim
//! that ran out by time must not be taken over under a writer that is still going). Only the holder's own end, or its
//! death (start time and ESRCH-only, as above), frees it. A claim with no pid is the caller's own timer: it ends at
//! `claim_until`. The secret path ([`secret`]) claims with the daemon's own pid, so the writer is the claimer and no
//! child outlives it: the key store's tools are waited for and killed at their timeout inside the same call.
//!
//! What differs from the host, on purpose:
//! - Who may answer is the owner alone. The host also lets a card's `answerable_by` person answer (`approver_of`,
//!   needs the member's devices); that and the bot approvers come with lane A's tables.
//! - A secret card (a Key card, [`SECRET_TOOLS`]) is never answered by the `cards.answer` row: its yes writes a key, which
//!   [`secret`] does on the envelope (`cards.provide`, claim, write and save, at most once). The row refuses and points
//!   to the app. An escape card (`request_access`) stays refused here too: its yes changes a guest's access (M2-3).
//! - Times are milliseconds, as the host's `now_ms`; every call takes `now` so tests (and a replay) set the clock.
//!
//! Not here yet: the host-side effects of an answer (waking the asker, closing the entry, the auto-allow and edge
//! cards' keeping) stay with the host until lane H; the owed-resume bookkeeping (`resumed_at`, `applied_at`) is stored
//! but its claims move with the host's resume code.

use rusqlite::{OptionalExtension as _, Row, Transaction, params};
use serde_json::{Value, json};

use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::{Liveness, holder_alive};

/// The tools whose card writes a secret or an access grant on yes. `cards.answer` never takes their yes: a key card is
/// provided through [`secret`], and an escape card's yes waits for M2-3. Fail safe: it is held back, since its yes
/// changes what a guest may reach.
pub(crate) const SECRET_TOOLS: &[&str] = &["request_secret", "request_access"];

/// Where the person is sent when a secret card is answered from a terminal.
const SECRET_HINT: &str = "this card asks for a key or access; answer it in the Kith app (a terminal never types a secret)";

const OPEN_STATES: &str = "'pending','live','parked'";
const CLEAR_CLAIM: &str =
    "claim_by = NULL, claim_until = NULL, claim_pid = NULL, claim_started = ''";
/// A claim whose holder gave a pid: it holds while that process lives, whatever `claim_until` says (`void_dead_claims`
/// has already dropped the dead ones in the same transaction).
const PID_HELD: &str = "(claim_by IS NOT NULL AND claim_pid IS NOT NULL)";
/// The deadline the clock goes by: the card's own, or the claim's time while one is held.
const EFFECTIVE: &str = "MAX(deadline, CASE WHEN claim_by IS NULL THEN 0 ELSE claim_until END)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Pending,
    Live,
    Parked,
    Approved,
    Denied,
    Expired,
    Withdrawn,
    Used,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Live => "live",
            Self::Parked => "parked",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Withdrawn => "withdrawn",
            Self::Used => "used",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "live" => Self::Live,
            "parked" => Self::Parked,
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "used" => Self::Used,
            "withdrawn" => Self::Withdrawn,
            // An unknown word never reads as open or as access.
            _ => Self::Expired,
        }
    }

    /// Waiting for an answer.
    pub(crate) fn open(self) -> bool {
        matches!(self, Self::Pending | Self::Live | Self::Parked)
    }
}

/// A card as stored. `person` is empty for the owner's own run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Card {
    pub id: String,
    pub session: String,
    pub source: String,
    pub bot_id: Option<String>,
    pub run_id: Option<String>,
    pub entry_id: Option<String>,
    pub person: String,
    pub agent: String,
    pub approver: String,
    pub tool: String,
    pub input: String,
    pub resource: String,
    pub shown: String,
    pub scope: String,
    pub state: State,
    pub created_at: i64,
    pub deadline: i64,
    pub answer: Option<String>,
    pub allowed: Option<bool>,
    pub answered_by: Option<String>,
    pub answered_at: Option<i64>,
    pub answered_parked: bool,
    pub granted_at: Option<i64>,
    pub resumed_at: Option<i64>,
    pub applied_at: Option<i64>,
    pub used_by_ask: bool,
    pub reply_to: Option<String>,
    pub asked_by: Option<String>,
    pub answerable_by: Option<String>,
    pub in_grants: bool,
    /// JSON text of the offered answers; None is the plain allow / deny / cancel.
    pub options: Option<String>,
    pub claim_by: Option<String>,
    pub claim_until: Option<i64>,
    pub claim_pid: Option<i64>,
}

impl Card {
    /// The `camelCase` form the host serialises a card in, plus the claim.
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id, "session": self.session, "source": self.source, "botId": self.bot_id,
            "runId": self.run_id, "entryId": self.entry_id, "person": self.person, "agent": self.agent,
            "approver": self.approver, "tool": self.tool, "input": self.input, "resource": self.resource,
            "shown": self.shown, "scope": self.scope, "state": self.state.as_str(),
            "createdAt": self.created_at, "deadline": self.deadline, "answer": self.answer,
            "allowed": self.allowed, "answeredBy": self.answered_by, "answeredAt": self.answered_at,
            "answeredParked": self.answered_parked, "grantedAt": self.granted_at,
            "resumedAt": self.resumed_at, "appliedAt": self.applied_at, "replyTo": self.reply_to,
            "askedBy": self.asked_by, "answerableBy": self.answerable_by, "inGrants": self.in_grants,
            "options": self.options.as_deref().and_then(|o| serde_json::from_str::<Value>(o).ok()),
            "claimedBy": self.claim_by, "claimedUntil": self.claim_until,
            "secret": SECRET_TOOLS.contains(&self.tool.as_str()),
        })
    }

    /// The short form `cards.list` shows.
    fn summary(&self) -> Value {
        json!({
            "id": self.id, "state": self.state.as_str(), "agent": self.agent, "person": self.person,
            "tool": self.tool, "resource": self.resource, "createdAt": self.created_at,
            "deadline": self.deadline, "answer": self.answer, "claimedBy": self.claim_by,
            "secret": SECRET_TOOLS.contains(&self.tool.as_str()),
        })
    }

    /// The holder of a claim that is still in force at `now`.
    fn holder(&self, now: i64) -> Option<&str> {
        match (&self.claim_by, self.claim_until) {
            // A claim with a pid is still in force after its time: its process was found alive in this transaction.
            (Some(h), Some(until)) if until >= now || self.claim_pid.is_some() => Some(h.as_str()),
            _ => None,
        }
    }
}

/// What a new ask is, before it has an id.
#[derive(Clone, Debug)]
pub(crate) struct NewCard {
    pub session: String,
    /// `acp` (a turn of a bot chat) or `run` (a `claude -p` run asking over the run socket).
    pub source: &'static str,
    pub bot_id: Option<String>,
    pub run_id: Option<String>,
    pub person: String,
    pub agent: String,
    pub answerable_by: Option<String>,
    pub in_grants: bool,
    pub tool: String,
    pub input: String,
    pub resource: String,
    pub shown: String,
    pub deadline: i64,
    pub reply_to: Option<String>,
    pub asked_by: Option<String>,
    /// JSON text of the offered answers (`[{"optionId": .., "kind": ..}]`), or None for allow / deny / cancel.
    pub options: Option<String>,
}

/// What opening a card came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Raised {
    /// A new card, `pending` until its entry is posted.
    New(Card),
    /// The same ask was already open; this one waits on it (A6).
    Joined(Card),
    /// An earlier "yes, once" for this session and resource was waiting: used now (C7, C8).
    Granted(Card),
}

/// One answer to a card.
pub(crate) struct Reply<'a> {
    pub id: &'a str,
    pub allow: bool,
    pub answer: &'a str,
    pub by: &'a str,
    /// A yes to a card whose asker is up is spent in the same step: the caller hands it to that asker, and a parallel
    /// ask cannot pick the grant up in between (A4, C3).
    pub spend: bool,
}

/// What an answer came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answered {
    /// The card moved to `approved`, `used` or `denied`; `was_live` is whether the asker's process was up.
    Applied { card: Box<Card>, was_live: bool },
    /// Nothing changed (A1): no such card, already closed, or past its deadline (now `Expired`).
    Nothing(Option<State>),
    /// Someone holds the claim: only they end it.
    Claimed(String),
}

/// One request for the right to answer a card.
pub(crate) struct Claim<'a> {
    pub holder: &'a str,
    /// The holder's process, so a crash drops the claim; None for a caller that releases it itself.
    pub pid: Option<u32>,
    /// How long the claim keeps the clock off the card (ms since the epoch).
    pub until: i64,
}

const COLS: &str = "id, session, source, bot_id, run_id, entry_id, person, agent, approver, tool, input, resource, shown, \
    scope, state, created_at, deadline, answer, allowed, answered_by, answered_at, answered_parked, granted_at, resumed_at, \
    applied_at, used_by_ask, reply_to, asked_by, answerable_by, in_grants, options, claim_by, claim_until, claim_pid";

fn row(r: &Row<'_>) -> rusqlite::Result<Card> {
    Ok(Card {
        id: r.get(0)?,
        session: r.get(1)?,
        source: r.get(2)?,
        bot_id: r.get(3)?,
        run_id: r.get(4)?,
        entry_id: r.get(5)?,
        person: r.get(6)?,
        agent: r.get(7)?,
        approver: r.get(8)?,
        tool: r.get(9)?,
        input: r.get(10)?,
        resource: r.get(11)?,
        shown: r.get(12)?,
        scope: r.get(13)?,
        state: State::parse(&r.get::<_, String>(14)?),
        created_at: r.get(15)?,
        deadline: r.get(16)?,
        answer: r.get(17)?,
        allowed: r.get::<_, Option<i64>>(18)?.map(|v| v != 0),
        answered_by: r.get(19)?,
        answered_at: r.get(20)?,
        answered_parked: r.get::<_, i64>(21)? != 0,
        granted_at: r.get(22)?,
        resumed_at: r.get(23)?,
        applied_at: r.get(24)?,
        used_by_ask: r.get::<_, i64>(25)? != 0,
        reply_to: r.get(26)?,
        asked_by: r.get(27)?,
        answerable_by: r.get(28)?,
        in_grants: r.get::<_, i64>(29)? != 0,
        options: r.get(30)?,
        claim_by: r.get(31)?,
        claim_until: r.get(32)?,
        claim_pid: r.get(33)?,
    })
}

fn get(tx: &Transaction<'_>, id: &str) -> Result<Option<Card>> {
    Ok(tx
        .query_row(
            &format!("SELECT {COLS} FROM cards WHERE id = ?1"),
            [id],
            row,
        )
        .optional()?)
}

fn list_where(
    tx: &rusqlite::Connection,
    tail: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<Card>> {
    let mut st = tx.prepare(&format!("SELECT {COLS} FROM cards {tail}"))?;
    let rows = st.query_map(args, row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Moves every card matching `where_` to `to`; returns them as they were before. A move that closes the card drops its
/// claim; `keep_claim` is for a move that leaves the card open (parking), whose claim holder is still writing.
fn move_where(
    tx: &rusqlite::Connection,
    where_: &str,
    args: impl rusqlite::Params + Copy,
    to: State,
    keep_claim: bool,
) -> Result<Vec<Card>> {
    let before = list_where(tx, &format!("WHERE {where_}"), args)?;
    let claim = if keep_claim {
        String::new()
    } else {
        format!(", {CLEAR_CLAIM}")
    };
    tx.execute(
        &format!(
            "UPDATE cards SET state = '{}'{claim} WHERE {where_}",
            to.as_str()
        ),
        args,
    )?;
    Ok(before)
}

fn bad(what: &str) -> Error {
    Error::BadParams(format!("kith-core: cards: {what}"))
}

/// The claims whose holder's process is gone or is another process now, judged with no lock held (a probe reads the
/// system; the write lock is shared with the conversation claims). One rule, as the gate's ([`holder_alive`]):
/// - the start time reads and equals the recorded one: alive;
/// - it reads and differs: the pid was reused, dead;
/// - it cannot be read (or none was recorded, or old `lstart` text): dead only when the system says there is no such
///   process. A failed probe must never drop a live holder's claim, which would let a second answerer in.
///
/// `(card id, pid, started)` as the row was read; [`void_dead_claims`] frees the ones that still read so.
pub(super) fn dead_claims(core: &Core, live: &dyn Liveness) -> Result<Vec<(String, i64, String)>> {
    let mut st = core.conn().prepare(
        "SELECT id, claim_pid, claim_started FROM cards WHERE claim_by IS NOT NULL AND claim_pid IS NOT NULL",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut dead = Vec::new();
    for r in rows {
        let (id, pid, started) = r?;
        let Ok(p) = u32::try_from(pid) else {
            continue;
        };
        if !holder_alive(live, p, &started) {
            dead.push((id, pid, started));
        }
    }
    Ok(dead)
}

/// Drops the claims [`dead_claims`] found, inside the transaction. A claim that was taken again in between (another pid
/// or start time) is left alone.
pub(super) fn void_dead_claims(tx: &Transaction<'_>, dead: &[(String, i64, String)]) -> Result<()> {
    for (id, pid, started) in dead {
        tx.execute(
            &format!(
                "UPDATE cards SET {CLEAR_CLAIM} WHERE id = ?1 AND claim_pid = ?2 AND claim_started = ?3"
            ),
            params![id, pid, started],
        )?;
    }
    Ok(())
}

/// A random card id, 32 hex digits.
fn new_id() -> Result<String> {
    use std::fmt::Write as _;
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw)
        .map_err(|e| Error::Io(format!("kith-core: no random id for a card: {e}")))?;
    Ok(raw.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Opens a card for an ask, or joins or uses what is already there, in one step (A6, C7, C8).
pub(crate) fn raise(core: &mut Core, n: &NewCard, now: i64, live: &dyn Liveness) -> Result<Raised> {
    if n.session.is_empty() || n.resource.is_empty() || n.tool.is_empty() {
        return Err(bad("a card names its session, resource and tool"));
    }
    if !matches!(n.source, "acp" | "run") {
        return Err(bad("source is acp or run"));
    }
    if n.deadline <= now {
        return Err(bad("a card's deadline is in the future"));
    }
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    // Whatever is past its deadline is closed first: an expired card never joins or grants.
    tx.execute(
        &format!(
            "UPDATE cards SET state = 'expired', {CLEAR_CLAIM} WHERE session = ?1 AND resource = ?2 AND {EFFECTIVE} < ?3
               AND NOT {PID_HELD} AND state IN ({OPEN_STATES},'approved')"
        ),
        params![n.session, n.resource, now],
    )?;
    if let Some(open) = list_where(
        &tx,
        &format!("WHERE session = ?1 AND resource = ?2 AND state IN ({OPEN_STATES}) LIMIT 1"),
        params![n.session, n.resource],
    )?
    .pop()
    {
        tx.commit()?;
        return Ok(Raised::Joined(open));
    }
    // A yes for this session and exactly this resource that nobody used yet: once, then closed (A4).
    let grant = list_where(
        &tx,
        "WHERE session = ?1 AND resource = ?2 AND state = 'approved' AND granted_at IS NOT NULL AND scope = 'once'
           AND tool NOT IN ('request_access', 'request_secret') ORDER BY answered_at LIMIT 1",
        params![n.session, n.resource],
    )?
    .pop();
    if let Some(mut g) = grant {
        tx.execute(
            "UPDATE cards SET state = 'used', used_by_ask = 1 WHERE id = ?1 AND state = 'approved'",
            [&g.id],
        )?;
        tx.commit()?;
        g.state = State::Used;
        g.used_by_ask = true;
        return Ok(Raised::Granted(g));
    }
    let id = new_id()?;
    tx.execute(
        "INSERT INTO cards (id, session, source, bot_id, run_id, person, agent, approver, tool, input, resource, shown,
                            scope, state, created_at, deadline, reply_to, asked_by, answerable_by, in_grants, options)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'once', 'pending', ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            id,
            n.session,
            n.source,
            n.bot_id,
            n.run_id,
            n.person,
            n.agent,
            if n.answerable_by.is_some() {
                "member"
            } else {
                "owner"
            },
            n.tool,
            n.input,
            n.resource,
            n.shown,
            now,
            n.deadline,
            n.reply_to,
            n.asked_by,
            n.answerable_by,
            i64::from(n.in_grants),
            n.options,
        ],
    )?;
    let card = get(&tx, &id)?.ok_or_else(|| Error::Io("kith-core: a new card vanished".into()))?;
    tx.commit()?;
    Ok(Raised::New(card))
}

/// One card, as stored.
pub(crate) fn card(core: &mut Core, id: &str) -> Result<Option<Card>> {
    let tx = core.claim_tx()?;
    let c = get(&tx, id)?;
    tx.commit()?;
    Ok(c)
}

/// The open cards, oldest first, and the newest `recent` closed cards of the owner's own (never an invited person's or
/// a bot guest's), newest first.
pub(crate) fn listing(core: &mut Core, recent: usize) -> Result<(Vec<Card>, Vec<Card>)> {
    let tx = core.claim_tx()?;
    let open = list_where(
        &tx,
        &format!("WHERE state IN ({OPEN_STATES}) ORDER BY created_at, id"),
        [],
    )?;
    let limit = i64::try_from(recent).unwrap_or(i64::MAX);
    let closed = list_where(
        &tx,
        &format!(
            "WHERE state NOT IN ({OPEN_STATES}) AND approver = 'owner' AND person = '' ORDER BY created_at DESC, id LIMIT ?1"
        ),
        [limit],
    )?;
    tx.commit()?;
    Ok((open, closed))
}

/// The card's entry is posted and its asker is up: `live`. A parked card joined by a new process comes back here too.
/// Does nothing to a closed card.
pub(crate) fn mark_live(
    core: &mut Core,
    id: &str,
    entry_id: Option<&str>,
    run_id: Option<&str>,
) -> Result<Option<Card>> {
    let tx = core.claim_tx()?;
    tx.execute(
        &format!(
            "UPDATE cards SET state = 'live', entry_id = COALESCE(?2, entry_id), run_id = COALESCE(?3, run_id)
              WHERE id = ?1 AND state IN ({OPEN_STATES})"
        ),
        params![id, entry_id, run_id],
    )?;
    let c = get(&tx, id)?;
    tx.commit()?;
    Ok(c)
}

/// The run's process is gone: its live cards park (C6). Returns them as they were. A parked card is still open, so a
/// held claim stays: its holder is mid-write and the card must not be answered or swept under it.
pub(crate) fn park_run(core: &mut Core, run_id: &str) -> Result<Vec<Card>> {
    let tx = core.claim_tx()?;
    let moved = move_where(
        &tx,
        "run_id = ?1 AND state IN ('pending','live')",
        params![run_id],
        State::Parked,
        true,
    )?;
    tx.commit()?;
    Ok(moved)
}

/// An answer. Only an open card before its deadline takes one (A1); the first answer wins, so a second device or a late
/// retry finds it closed. An allow records the once grant in the same step. While another holds the claim nothing is
/// recorded.
pub(crate) fn answer(
    core: &mut Core,
    r: &Reply<'_>,
    now: i64,
    live: &dyn Liveness,
) -> Result<Answered> {
    if r.by.is_empty() || r.answer.is_empty() {
        return Err(bad("an answer says who gave it and what it is"));
    }
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let Some(card) = get(&tx, r.id)? else {
        return Ok(Answered::Nothing(None));
    };
    if !card.state.open() {
        return Ok(Answered::Nothing(Some(card.state)));
    }
    // A secret card's yes is a write to the Keychain (M2-12, on the envelope). Recording it here would leave an approved
    // card with nothing written, which a later ask would be handed as a grant. A no needs no write.
    if r.allow && SECRET_TOOLS.contains(&card.tool.as_str()) {
        return Err(Error::NotAllowed(format!(
            "kith-core: cards: {SECRET_HINT}"
        )));
    }
    if let Some(h) = card.holder(now) {
        return Ok(Answered::Claimed(h.to_owned()));
    }
    if card.deadline < now {
        tx.execute(
            &format!("UPDATE cards SET state = 'expired', {CLEAR_CLAIM} WHERE id = ?1"),
            [r.id],
        )?;
        tx.commit()?;
        return Ok(Answered::Nothing(Some(State::Expired)));
    }
    // A card still `pending` was only just made: its asker is up.
    let was_live = matches!(card.state, State::Live | State::Pending);
    let to = if !r.allow {
        State::Denied
    } else if r.spend && was_live {
        State::Used
    } else {
        State::Approved
    };
    let done = tx.execute(
        &format!(
            "UPDATE cards SET state = ?2, answer = ?3, allowed = ?4, answered_by = ?5, answered_at = ?6,
                              answered_parked = ?7, granted_at = ?8, {CLEAR_CLAIM}
              WHERE id = ?1 AND state IN ({OPEN_STATES})"
        ),
        params![
            r.id,
            to.as_str(),
            r.answer,
            i64::from(r.allow),
            r.by,
            now,
            i64::from(card.state == State::Parked),
            r.allow.then_some(now)
        ],
    )?;
    if done != 1 {
        return Ok(Answered::Nothing(Some(card.state)));
    }
    let card = Box::new(
        get(&tx, r.id)?.ok_or_else(|| Error::Io("kith-core: an answered card vanished".into()))?,
    );
    tx.commit()?;
    Ok(Answered::Applied { card, was_live })
}

/// Takes the right to answer a card: an open card before its deadline, with no other holder, is held until `until`, so
/// the sweep and a second answer cannot close it while the key is being written. Returns the card (with the claim);
/// None when it is closed, past its deadline, or someone else holds it. The same holder claiming again extends its
/// claim. Whoever holds it ends it with [`release`] (the write failed) or [`saved`].
pub(crate) fn claim(
    core: &mut Core,
    id: &str,
    c: &Claim<'_>,
    now: i64,
    live: &dyn Liveness,
) -> Result<Option<Card>> {
    if c.holder.is_empty() {
        return Err(bad("a claim names its holder"));
    }
    if c.until <= now {
        return Err(bad("a claim lasts past now"));
    }
    let dead = dead_claims(core, live)?;
    let started = c.pid.and_then(|p| live.start(p)).unwrap_or_default();
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let Some(card) = get(&tx, id)?.filter(|k| k.state.open() && k.deadline >= now) else {
        return Ok(None);
    };
    if card.holder(now).is_some_and(|h| h != c.holder) {
        return Ok(None);
    }
    tx.execute(
        "UPDATE cards SET claim_by = ?2, claim_until = ?3, claim_pid = ?4, claim_started = ?5 WHERE id = ?1",
        params![id, c.holder, c.until, c.pid.map(i64::from), started],
    )?;
    let held = get(&tx, id)?;
    tx.commit()?;
    Ok(held)
}

/// The write failed: the claim ends and the card stays open on its own deadline. False when `holder` did not hold it
/// (it ran out, was taken over, or the card closed).
pub(crate) fn release(
    core: &mut Core,
    id: &str,
    holder: &str,
    live: &dyn Liveness,
) -> Result<bool> {
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let n = tx.execute(
        &format!(
            "UPDATE cards SET {CLEAR_CLAIM} WHERE id = ?1 AND claim_by = ?2 AND state IN ({OPEN_STATES})"
        ),
        params![id, holder],
    )?;
    tx.commit()?;
    Ok(n == 1)
}

/// The key is in the Keychain (or the grant is written): the card is answered and spent, and the resume it owes stays
/// owed whether or not the asker was still up (`answered_parked`, `used_by_ask`). Only the holder of the claim, and only
/// while the card is open: a card the clock, a withdrawal or a no closed is never saved. `keep_until` keeps the owed
/// resume alive past a deadline that passed during the write.
///
/// The claim's own time is not checked here, on purpose: a write that finished after `claim_until` is still the
/// holder's, as long as nobody took the claim over and the card is still open.
pub(crate) fn saved(
    core: &mut Core,
    id: &str,
    holder: &str,
    by: &str,
    now: i64,
    keep_until: i64,
    live: &dyn Liveness,
) -> Result<Option<Card>> {
    if by.is_empty() {
        return Err(bad("a save says who gave it"));
    }
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let done = saved_in(&tx, id, holder, by, now, keep_until)?;
    let card = if done { get(&tx, id)? } else { None };
    tx.commit()?;
    Ok(card)
}

/// [`saved`] inside a transaction of the caller's, so a secret card's write and its save are one commit. True when the
/// holder's claim ended the card.
fn saved_in(
    tx: &Transaction<'_>,
    id: &str,
    holder: &str,
    by: &str,
    now: i64,
    keep_until: i64,
) -> Result<bool> {
    let done = tx.execute(
        &format!(
            "UPDATE cards SET state = 'used', used_by_ask = 1, answer = 'saved', allowed = 1, answered_by = ?3,
                              answered_at = ?4, answered_parked = 1, granted_at = ?4, deadline = MAX(deadline, ?5),
                              {CLEAR_CLAIM}
              WHERE id = ?1 AND claim_by = ?2 AND state IN ({OPEN_STATES})"
        ),
        params![id, holder, by, now, keep_until],
    )?;
    Ok(done == 1)
}

/// An open card is withdrawn (the asker cancelled, the person's access ended). A card whose claim is held is not: its
/// holder is mid-write, and closing the card under it would let the write land on a closed card, so this answers false
/// as the sweep leaves it alone, and the caller asks again once the claim has ended. (The one exception is a revoke,
/// [`withdraw_people`], which takes a claimed card too.) Returns whether it was withdrawn.
pub(crate) fn withdraw(core: &mut Core, id: &str, now: i64, live: &dyn Liveness) -> Result<bool> {
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let n = tx.execute(
        &format!(
            "UPDATE cards SET state = 'withdrawn', {CLEAR_CLAIM} WHERE id = ?1 AND state IN ({OPEN_STATES})
                AND NOT (claim_by IS NOT NULL AND (claim_until >= ?2 OR claim_pid IS NOT NULL))"
        ),
        params![id, now],
    )?;
    tx.commit()?;
    Ok(n == 1)
}

/// The people's access ended (A7, M2-11b): every open card and unused once-grant asked by one of `people` (a person
/// and their bots) is withdrawn, inside the caller's transaction so it commits with the revoke. Like the host's
/// `cards_withdraw_person` it takes a card whose claim is held too, and drops the claim: after a revoke nothing of
/// theirs may stay answerable, and the holder's `saved` then finds a closed card. Returns the ids that moved. The owner
/// (an empty person) is never named here.
pub(crate) fn withdraw_people(tx: &rusqlite::Connection, people: &[String]) -> Result<Vec<String>> {
    // the caller passes each person's old ids too (`people.was`, see `revoke::was_ids`): a run that began before a
    // rename can raise under the old id after it
    let mut named: Vec<String> = people.to_vec();
    named.sort();
    named.dedup();
    let people = &named[..];
    let mut gone = Vec::new();
    for p in people.iter().filter(|p| !p.is_empty()) {
        let moved = move_where(
            tx,
            &format!("person = ?1 AND state IN ({OPEN_STATES},'approved')"),
            params![p],
            State::Withdrawn,
            false,
        )?;
        gone.extend(moved.into_iter().map(|c| c.id));
    }
    Ok(gone)
}

/// A person's new id (`people.renamePerson`): their cards follow in the rename's own transaction, so a later revoke
/// of the new id finds them (like `doors::renamed`). A renamed person's bots keep their ids.
pub(crate) fn renamed(tx: &rusqlite::Connection, old: &str, new: &str) -> Result<()> {
    tx.execute(
        "UPDATE cards SET person = ?2 WHERE person = ?1",
        params![old, new],
    )?;
    Ok(())
}

/// Past its deadline (or its claim's time): open cards and unused grants expire, silence is no. Returns the cards as
/// they were. A claim that is held keeps its card.
pub(crate) fn sweep(core: &mut Core, now: i64, live: &dyn Liveness) -> Result<Vec<Card>> {
    let dead = dead_claims(core, live)?;
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, &dead)?;
    let moved = move_where(
        &tx,
        // An approved card whose wake is in the queue stays until a turn carried it: expiring it would lose the yes.
        &format!(
            "{EFFECTIVE} < ?1 AND NOT {PID_HELD} AND state IN ({OPEN_STATES},'approved')
                AND NOT (state = 'approved' AND resumed_at IS NOT NULL AND applied_at IS NULL)"
        ),
        params![now],
        State::Expired,
        false,
    )?;
    tx.commit()?;
    Ok(moved)
}

fn text<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(&format!("{key} is required")))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// How many closed cards `cards.list` shows after the open ones.
const RECENT: usize = 20;

/// Whether the answer `word` allows. The card's own options when it has them (an option allows when its kind starts
/// with `allow`, as the host reads an entry's options); the plain `allow` / `deny` / `cancel` when it has none. Anything
/// else is refused, naming what is valid. A card whose options cannot be read answers nothing: fail safe.
fn allows(card: &Card, word: &str) -> Result<bool> {
    let Some(raw) = &card.options else {
        return match word {
            "allow" => Ok(true),
            "deny" | "cancel" => Ok(false),
            _ => Err(bad("this card takes the answers allow, deny or cancel")),
        };
    };
    let options: Value = serde_json::from_str(raw).map_err(|_| {
        Error::Refused(format!(
            "kith-core: cards: the options of card {} cannot be read, so it is not answered here",
            card.id
        ))
    })?;
    let list = options.as_array().ok_or_else(|| {
        Error::Refused(format!(
            "kith-core: cards: the options of card {} cannot be read, so it is not answered here",
            card.id
        ))
    })?;
    let ids: Vec<&str> = list
        .iter()
        .filter_map(|o| o.get("optionId").and_then(Value::as_str))
        .collect();
    let Some(o) = list
        .iter()
        .find(|o| o.get("optionId").and_then(Value::as_str) == Some(word))
    else {
        return Err(bad(&format!(
            "this card takes the answers {}",
            ids.join(", ")
        )));
    };
    Ok(o.get("kind")
        .and_then(Value::as_str)
        .is_some_and(|k| k.starts_with("allow")))
}

fn answer_row(core: &mut Core, params: &Value, live: &dyn Liveness) -> Result<Value> {
    let id = text(params, "id")?;
    let word = text(params, "answer")?;
    let found = card(core, id)?.ok_or_else(|| bad(&format!("no card {id}")))?;
    // Before anything is read of the answer: a secret card's yes writes the Keychain, which the app does.
    if SECRET_TOOLS.contains(&found.tool.as_str()) {
        return Err(Error::NotAllowed(format!(
            "kith-core: cards.answer: {SECRET_HINT}"
        )));
    }
    let allow = allows(&found, word)?;
    let reply = Reply {
        id,
        allow,
        answer: word,
        by: "owner",
        spend: false,
    };
    Ok(match answer(core, &reply, now_ms(), live)? {
        Answered::Applied { card, .. } => {
            json!({"result": "answered", "id": card.id, "state": card.state.as_str(), "allowed": allow})
        }
        Answered::Nothing(Some(State::Expired)) => {
            json!({"result": "expired", "id": id, "state": "expired"})
        }
        Answered::Nothing(Some(s)) => json!({"result": "closed", "id": id, "state": s.as_str()}),
        Answered::Nothing(None) => json!({"result": "closed", "id": id, "state": null}),
        Answered::Claimed(by) => json!({"result": "claimed", "id": id, "by": by}),
    })
}

/// The `cards.*` rows of the table. The params were checked against the row already.
pub(crate) fn call(
    core: &mut Core,
    method: &str,
    params: &Value,
    live: &dyn Liveness,
) -> Result<Value> {
    // A key write that could not release its claim leaves it under this live pid: free it before anything reads cards.
    secret::forget_own_claims(core)?;
    match method {
        "cards.list" => {
            let (open, recent) = listing(core, RECENT)?;
            Ok(json!({
                "open": open.iter().map(Card::summary).collect::<Vec<_>>(),
                "recent": recent.iter().map(Card::summary).collect::<Vec<_>>(),
            }))
        }
        "cards.show" => {
            let id = text(params, "id")?;
            card(core, id)?
                .as_ref()
                .map(Card::json)
                .ok_or_else(|| bad(&format!("no card {id}")))
        }
        "cards.answer" => answer_row(core, params, live),
        "cards.provide" => secret::provide_row(core, params, live),
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

mod secret;
#[cfg(test)]
mod tests;
