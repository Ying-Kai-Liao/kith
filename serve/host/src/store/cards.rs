//! The card store: what a run asked for beyond its limits, and what became of it (docs: designs/2026-10-24-approval-cards.md,
//! A1-A9 and C1-C8). One writer: the host, through the methods here; each one is a single transaction, so the states
//! below never disagree with each other.
//!
//! ```text
//! pending -> live -> parked            (parked when the asking process is gone, C6)
//!    open (pending|live|parked) -> approved | denied   (an answer; approved is a once grant bound to the session)
//!    open -> expired | withdrawn       (the clock; the person's access ended)
//!    approved -> used | expired | withdrawn
//! ```
//!
//! Only the open states are unique per `(session, resource)` (A6, a unique index): a repeat of the same ask joins the
//! open card. Nothing here knows who may answer; the callers (roles.rs for people, the run socket for runs) decide that.

use super::Store;
use crate::LockExt;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CardState {
    Pending,
    Live,
    Parked,
    Approved,
    Denied,
    Expired,
    Withdrawn,
    Used,
}

impl CardState {
    pub fn as_str(self) -> &'static str {
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
            // An unknown word never reads as access.
            _ => Self::Expired,
        }
    }

    /// Waiting for an answer.
    pub fn open(self) -> bool {
        matches!(self, Self::Pending | Self::Live | Self::Parked)
    }
}

/// A card as stored. `person` is empty for the owner's own run.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub session: String,
    /// `acp` (a turn of a bot chat) or `run` (a `claude -p` run asking over the run socket).
    pub source: String,
    pub bot_id: Option<String>,
    pub run_id: Option<String>,
    pub entry_id: Option<String>,
    pub person: String,
    pub agent: String,
    /// `owner` or `member` (the person themself).
    pub approver: String,
    pub tool: String,
    pub input: String,
    pub resource: String,
    /// The resource as the approver saw it (for a file, the resolved real path): what a grant is exactly (A3).
    pub shown: String,
    pub scope: String,
    pub state: CardState,
    pub created_at: i64,
    pub deadline: i64,
    pub answer: Option<String>,
    pub allowed: Option<bool>,
    pub answered_by: Option<String>,
    pub answered_at: Option<i64>,
    /// The answer arrived while the process was gone (nothing to continue; the grant waits for a resume).
    pub answered_parked: bool,
    pub granted_at: Option<i64>,
    pub resumed_at: Option<i64>,
    /// The `kith ask` id the run was started by: the card's outcome is that ask's answer (rule 7).
    pub reply_to: Option<String>,
    /// The agent that asked it, for the card's chain; none for a terminal.
    pub asked_by: Option<String>,
}

/// What a new ask is, before it has an id.
#[derive(Clone, Debug)]
pub struct NewCard {
    pub session: String,
    pub source: &'static str,
    pub bot_id: Option<String>,
    pub run_id: Option<String>,
    pub person: String,
    pub agent: String,
    pub approver: &'static str,
    pub tool: String,
    pub input: String,
    pub resource: String,
    pub shown: String,
    pub deadline: i64,
    pub reply_to: Option<String>,
    pub asked_by: Option<String>,
}

/// What opening a card came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Opened {
    /// A new card, `pending` until its entry is posted.
    New(Card),
    /// The same ask was already open; this one waits on it (A6).
    Joined(Card),
    /// An earlier answer of "yes, once" for this session and resource was waiting: used now (C7, C8).
    Granted(Card),
}

/// What an answer came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Answered {
    /// The card moved to `approved` or `denied`; `was_live` is whether the asker's process was up.
    Applied { card: Box<Card>, was_live: bool },
    /// Nothing changed (A1): no such card, already closed, or past its deadline.
    Nothing(Option<CardState>),
}

const COLS: &str = "id, session, source, bot_id, run_id, entry_id, person, agent, approver, tool, input, resource, shown, \
    scope, state, created_at, deadline, answer, allowed, answered_by, answered_at, answered_parked, granted_at, resumed_at, \
    reply_to, asked_by";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Card> {
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
        state: CardState::parse(&r.get::<_, String>(14)?),
        created_at: r.get(15)?,
        deadline: r.get(16)?,
        answer: r.get(17)?,
        allowed: r.get::<_, Option<i64>>(18)?.map(|v| v != 0),
        answered_by: r.get(19)?,
        answered_at: r.get(20)?,
        answered_parked: r.get::<_, i64>(21)? != 0,
        granted_at: r.get(22)?,
        resumed_at: r.get(23)?,
        reply_to: r.get(24)?,
        asked_by: r.get(25)?,
    })
}

fn get(c: &Connection, id: &str) -> rusqlite::Result<Option<Card>> {
    c.query_row(&format!("SELECT {COLS} FROM cards WHERE id = ?1"), [id], row).optional()
}

fn list(c: &Connection, tail: &str, args: impl rusqlite::Params) -> rusqlite::Result<Vec<Card>> {
    c.prepare(&format!("SELECT {COLS} FROM cards {tail}"))?.query_map(args, row)?.collect()
}

/// Moves every card matching `where_` to `to`; returns them as they were before.
fn move_where(c: &Connection, where_: &str, args: impl rusqlite::Params + Copy, to: CardState) -> Result<Vec<Card>> {
    let before = list(c, &format!("WHERE {where_}"), args)?;
    c.execute(&format!("UPDATE cards SET state = '{}' WHERE {where_}", to.as_str()), args)?;
    Ok(before)
}

impl Store {
    /// Opens a card for an ask, or joins/uses what is already there, in one step (A6, C7, C8).
    pub fn card_open(&self, n: &NewCard, now: i64) -> Result<Opened> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        // Whatever is past its deadline is closed first: an expired card never joins or grants.
        tx.execute(
            "UPDATE cards SET state = 'expired' WHERE session = ?1 AND resource = ?2 AND deadline < ?3
               AND state IN ('pending','live','parked','approved')",
            params![n.session, n.resource, now],
        )?;
        if let Some(open) = list(
            &tx,
            "WHERE session = ?1 AND resource = ?2 AND state IN ('pending','live','parked') LIMIT 1",
            params![n.session, n.resource],
        )?
        .pop()
        {
            tx.commit()?;
            return Ok(Opened::Joined(open));
        }
        // A yes for this session and exactly this resource that nobody used yet: once, then closed (A4).
        let grant = list(
            &tx,
            "WHERE session = ?1 AND resource = ?2 AND state = 'approved' AND granted_at IS NOT NULL AND scope = 'once'
              AND tool != 'request_access' ORDER BY answered_at LIMIT 1",
            params![n.session, n.resource],
        )?
        .pop();
        if let Some(mut g) = grant {
            tx.execute(
                "UPDATE cards SET state = 'used', used_by_ask = 1 WHERE id = ?1 AND state = 'approved'",
                [&g.id],
            )?;
            tx.commit()?;
            g.state = CardState::Used;
            return Ok(Opened::Granted(g));
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO cards(id, session, source, bot_id, run_id, person, agent, approver, tool, input, resource, shown,
                               scope, state, created_at, deadline, reply_to, asked_by)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'once', 'pending', ?13, ?14, ?15, ?16)",
            params![
                id,
                n.session,
                n.source,
                n.bot_id,
                n.run_id,
                n.person,
                n.agent,
                n.approver,
                n.tool,
                n.input,
                n.resource,
                n.shown,
                now,
                n.deadline,
                n.reply_to,
                n.asked_by
            ],
        )?;
        let card = get(&tx, &id)?.ok_or_else(|| anyhow::anyhow!("card vanished"))?;
        tx.commit()?;
        Ok(Opened::New(card))
    }

    pub fn card(&self, id: &str) -> Option<Card> {
        super::logged("card", get(&self.db.locked(), id)).flatten()
    }

    pub fn card_by_entry(&self, entry_id: &str) -> Option<Card> {
        let c = self.db.locked();
        super::logged("card by entry", list(&c, "WHERE entry_id = ?1 ORDER BY created_at DESC LIMIT 1", [entry_id]))?
            .pop()
    }

    /// Open cards (a person's, when `person` is set), newest first.
    #[cfg(test)]
    pub fn cards_open(&self, person: Option<&str>) -> Vec<Card> {
        let c = self.db.locked();
        let rows = match person {
            Some(p) => {
                list(&c, "WHERE person = ?1 AND state IN ('pending','live','parked') ORDER BY created_at DESC", [p])
            }
            None => list(&c, "WHERE state IN ('pending','live','parked') ORDER BY created_at DESC", []),
        };
        super::logged("open cards", rows).unwrap_or_default()
    }

    /// The card's entry is posted and its asker is up: `live`. A parked card joined by a new process comes back
    /// here too. Does nothing to a closed card.
    pub fn card_live(&self, id: &str, entry_id: Option<&str>, run_id: Option<&str>) -> Result<Option<Card>> {
        let c = self.db.locked();
        c.execute(
            "UPDATE cards SET state = 'live', entry_id = COALESCE(?2, entry_id), run_id = COALESCE(?3, run_id)
              WHERE id = ?1 AND state IN ('pending','live','parked')",
            params![id, entry_id, run_id],
        )?;
        Ok(get(&c, id)?)
    }

    /// An answer. Only an open card before its deadline takes one (A1); the first answer wins, so a second device
    /// or a late retry finds it closed. An allow records the once grant in the same step. With `spend`, a yes to a
    /// card whose asker is up is spent in that same step (`used`): the caller is about to hand it to that asker, and
    /// a parallel ask can't pick the grant up in between (A4, C3). If nobody hears it, [`Self::card_undelivered`].
    pub fn card_answer(
        &self,
        id: &str,
        allow: bool,
        answer: &str,
        by: &str,
        now: i64,
        spend: bool,
    ) -> Result<Answered> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let Some(card) = get(&tx, id)? else { return Ok(Answered::Nothing(None)) };
        if !card.state.open() {
            return Ok(Answered::Nothing(Some(card.state)));
        }
        if card.deadline < now {
            tx.execute("UPDATE cards SET state = 'expired' WHERE id = ?1", [id])?;
            tx.commit()?;
            return Ok(Answered::Nothing(Some(CardState::Expired)));
        }
        // A card still `pending` was only just made: its asker is up.
        let was_live = matches!(card.state, CardState::Live | CardState::Pending);
        tx.execute(
            "UPDATE cards SET state = ?2, answer = ?3, allowed = ?4, answered_by = ?5, answered_at = ?6,
                              answered_parked = ?7, granted_at = ?8
              WHERE id = ?1",
            params![
                id,
                if !allow {
                    "denied"
                } else if spend && was_live {
                    "used"
                } else {
                    "approved"
                },
                answer,
                i64::from(allow),
                by,
                now,
                i64::from(card.state == CardState::Parked),
                allow.then_some(now)
            ],
        )?;
        let card = Box::new(get(&tx, id)?.ok_or_else(|| anyhow::anyhow!("card vanished"))?);
        tx.commit()?;
        Ok(Answered::Applied { card, was_live })
    }

    /// Takes the write claim on a secret card: an open card before its deadline has its deadline pushed to `until`, so
    /// the sweep and a second answer cannot close it while the key is being written. Returns the card as it was (its own
    /// deadline); None when it is closed or past its deadline. Whoever holds the claim ends it with
    /// [`Self::card_claim_release`] (the write failed) or [`Self::card_saved`].
    pub fn card_claim(&self, id: &str, now: i64, until: i64) -> Result<Option<Card>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let Some(card) = get(&tx, id)?.filter(|c| c.state.open() && c.deadline >= now) else { return Ok(None) };
        tx.execute("UPDATE cards SET deadline = MAX(deadline, ?2) WHERE id = ?1", params![id, until])?;
        tx.commit()?;
        Ok(Some(card))
    }

    /// The write failed: the card goes back to its own deadline and stays open.
    pub fn card_claim_release(&self, id: &str, deadline: i64) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE cards SET deadline = ?2 WHERE id = ?1 AND state IN ('pending','live','parked')",
            params![id, deadline],
        )? == 1)
    }

    /// The key is in the Keychain (or the grant is written): the card is answered and spent, and the resume it owes
    /// stays owed whether or not the asker was still up (`answered_parked`, `used_by_ask`). `keep_until` keeps the owed
    /// resume alive past a deadline that passed during the write.
    pub fn card_saved(&self, id: &str, by: &str, now: i64, keep_until: i64) -> Result<Option<Card>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let done = tx.execute(
            "UPDATE cards SET state = 'used', used_by_ask = 1, answer = 'saved', allowed = 1, answered_by = ?2,
                              answered_at = ?3, answered_parked = 1, granted_at = ?3, deadline = MAX(deadline, ?4)
              WHERE id = ?1 AND state IN ('pending','live','parked')",
            params![id, by, now, keep_until],
        )?;
        let card = if done == 1 { get(&tx, id)? } else { None };
        tx.commit()?;
        Ok(card)
    }

    /// A yes spent for a live asker that did not hear it (it was gone, or its wait had ended): the grant is back, as an
    /// answer to a parked card, so the next ask of the session (or the resume) finds it.
    pub fn card_undelivered(&self, id: &str) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE cards SET state = 'approved', answered_parked = 1 WHERE id = ?1 AND state = 'used' AND allowed = 1 AND used_by_ask = 0",
            [id],
        )? == 1)
    }

    /// One card's asker is gone with it open: it parks (C6).
    pub fn card_park(&self, id: &str) -> Result<bool> {
        Ok(self
            .db
            .locked()
            .execute("UPDATE cards SET state = 'parked' WHERE id = ?1 AND state IN ('pending','live')", [id])?
            == 1)
    }

    /// An access card's answer was "always": the scope is kept on the card (what the approver chose).
    pub fn card_scope(&self, id: &str, scope: &str) -> Result<bool> {
        Ok(self
            .db
            .locked()
            .execute("UPDATE cards SET scope = ?2 WHERE id = ?1 AND state = 'approved'", params![id, scope])?
            == 1)
    }

    /// An "always" is written to kith-auth: the card is spent, and the resume it owes stays owed (it is `used_by_ask`, like a
    /// yes a later message used first).
    pub fn card_always_written(&self, id: &str) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE cards SET state = 'used', used_by_ask = 1 WHERE id = ?1 AND state = 'approved' AND scope = 'always'",
            [id],
        )? == 1)
    }

    /// The access card for `resource` in `session` that was approved and not used yet, if any (a repeat of the ask).
    pub fn card_access_waiting(&self, session: &str, resource: &str, now: i64) -> Option<Card> {
        let c = self.db.locked();
        super::logged(
            "waiting access grant",
            list(
                &c,
                "WHERE session = ?1 AND resource = ?2 AND tool = 'request_access' AND state = 'approved'
                   AND granted_at IS NOT NULL AND deadline >= ?3 LIMIT 1",
                params![session, resource, now],
            ),
        )?
        .pop()
    }

    /// The once grants of access cards for `session` an activation may take: approved and not used yet, bound to the
    /// card's session (C8) and not past the deadline. Nothing is marked: an app chat's activation reserves them and
    /// takes them with [`Store::card_access_commit`] once its session started with them.
    pub fn card_access_once(&self, session: &str, now: i64) -> Result<Vec<Card>> {
        Ok(list(
            &self.db.locked(),
            "WHERE session = ?1 AND tool = 'request_access' AND scope = 'once' AND state = 'approved'
               AND allowed = 1 AND granted_at IS NOT NULL AND deadline >= ?2 ORDER BY answered_at",
            params![session, now],
        )?)
    }

    /// Marks the once grants `ids` used, each in one compare-and-set, so of two takers racing for one grant exactly one
    /// gets it (L3). Returns the ids this call took. The card stays owed its resume (`used_by_ask`): a later message may
    /// have taken the grant first, and the resume then runs without it (C7).
    pub fn card_access_commit(&self, ids: &[String]) -> Result<Vec<String>> {
        let c = self.db.locked();
        let tx = c.unchecked_transaction()?;
        let mut taken = Vec::new();
        for id in ids {
            let won = tx.execute(
                "UPDATE cards SET state = 'used', used_by_ask = 1 WHERE id = ?1 AND state = 'approved'",
                [id],
            )?;
            if won == 1 {
                taken.push(id.clone());
            }
        }
        tx.commit()?;
        Ok(taken)
    }

    /// The once grants of access cards for `session`, taken in one step for the activation that is about to start (the
    /// kernel's one-shot, which takes them after its claim and before exec): [`Store::card_access_once`] less `reserved`
    /// (held by an app chat's activation that has not started yet), each marked used by [`Store::card_access_commit`].
    pub fn card_access_consume(&self, session: &str, now: i64, reserved: &HashSet<String>) -> Result<Vec<Card>> {
        let open: Vec<Card> =
            self.card_access_once(session, now)?.into_iter().filter(|c| !reserved.contains(&c.id)).collect();
        let won = self.card_access_commit(&open.iter().map(|c| c.id.clone()).collect::<Vec<_>>())?;
        Ok(open.into_iter().filter(|c| won.contains(&c.id)).map(|c| Card { state: CardState::Used, ..c }).collect())
    }

    /// The agents `person` has open cards or unused grants for.
    pub fn cards_agents(&self, person: &str) -> Vec<String> {
        let c = self.db.locked();
        super::logged(
            "card agents",
            c.prepare("SELECT DISTINCT agent FROM cards WHERE person = ?1 AND state IN ('pending','live','parked','approved')")
                .and_then(|mut st| st.query_map([person], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()),
        )
        .unwrap_or_default()
    }

    /// The grant of an approved card was delivered through the live channel, or taken by the next ask: one use (A4).
    pub fn card_use(&self, id: &str) -> Result<bool> {
        Ok(self.db.locked().execute("UPDATE cards SET state = 'used' WHERE id = ?1 AND state = 'approved'", [id])? == 1)
    }

    /// An open card is withdrawn (the asker cancelled, the person's access ended). Returns whether it was open.
    pub fn card_withdraw(&self, id: &str) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE cards SET state = 'withdrawn' WHERE id = ?1 AND state IN ('pending','live','parked')",
            [id],
        )? == 1)
    }

    /// The run's process is gone: its live cards park (C6). Returns them.
    pub fn cards_park_run(&self, run_id: &str) -> Result<Vec<Card>> {
        let c = self.db.locked();
        move_where(&c, "run_id = ?1 AND state IN ('pending','live')", params![run_id], CardState::Parked)
    }

    /// A bot's ACP process is gone: its live cards park.
    pub fn cards_park_bot(&self, bot_id: &str) -> Result<Vec<Card>> {
        let c = self.db.locked();
        move_where(
            &c,
            "source = 'acp' AND bot_id = ?1 AND state IN ('pending','live')",
            params![bot_id],
            CardState::Parked,
        )
    }

    /// `person`'s open and unused-approved cards end (A7); `agent` narrows it to one agent they lost. Returns them.
    pub fn cards_withdraw_person(&self, person: &str, agent: Option<&str>) -> Result<Vec<Card>> {
        ensure!(!person.is_empty(), "the owner's cards are never withdrawn");
        let c = self.db.locked();
        let states = "state IN ('pending','live','parked','approved')";
        match agent {
            Some(a) => move_where(
                &c,
                &format!("person = ?1 AND agent = ?2 AND {states}"),
                params![person, a],
                CardState::Withdrawn,
            ),
            None => move_where(&c, &format!("person = ?1 AND {states}"), params![person], CardState::Withdrawn),
        }
    }

    /// Past its deadline: open cards and unused grants expire (silence is no). Returns the cards that were open.
    pub fn cards_sweep(&self, now: i64) -> Result<Vec<Card>> {
        let c = self.db.locked();
        move_where(
            &c,
            "deadline < ?1 AND state IN ('pending','live','parked','approved')",
            params![now],
            CardState::Expired,
        )
    }

    /// After a host restart: live waits are gone, so live cards park; an approved card whose grant was not recorded
    /// gets it (C4). Returns the cards that parked.
    pub fn cards_restart(&self, now: i64) -> Result<Vec<Card>> {
        let c = self.db.locked();
        let parked = move_where(&c, "state IN ('pending','live')", params![], CardState::Parked)?;
        c.execute(
            "UPDATE cards SET granted_at = ?1 WHERE state = 'approved' AND allowed = 1 AND granted_at IS NULL",
            [now],
        )?;
        Ok(parked)
    }

    /// Owed resumes: answered yes while the asker was gone, with a grant, not resumed yet, before the deadline. A card
    /// whose grant a later ask used first (`used_by_ask`) is still owed: its resume runs as a wake without the grant
    /// (C7, A9). The record of an owed resume is this row, never a door's queue (C4, C5).
    pub fn cards_awaiting_resume(&self, now: i64) -> Vec<Card> {
        let c = self.db.locked();
        super::logged("cards awaiting resume", list(&c, &format!("WHERE {OWED} ORDER BY answered_at"), params![now]))
            .unwrap_or_default()
    }

    /// The owed resumes of one session: a single wake turn answers them all (L5).
    pub fn cards_owed_in(&self, session: &str, now: i64) -> Vec<Card> {
        let c = self.db.locked();
        super::logged(
            "owed cards of a session",
            list(&c, &format!("WHERE session = ?2 AND {OWED} ORDER BY answered_at"), params![now, session]),
        )
        .unwrap_or_default()
    }

    /// Claims the resume of a card: a compare-and-set on `resumed_at`, so a door's copy and a restart's re-derive can't
    /// both start a turn. True only for the caller that set it. Taken just before the turn is handed over; a refused
    /// hand-over gives it back ([`Self::card_resume_release`]).
    pub fn card_resume_claim(&self, id: &str, now: i64) -> Result<bool> {
        let sql = format!("UPDATE cards SET resumed_at = ?1 WHERE id = ?2 AND {OWED}");
        Ok(self.db.locked().execute(&sql, params![now, id])? == 1)
    }

    /// The hand-over was refused (the session is held, the actor is gone): the resume is owed again.
    pub fn card_resume_release(&self, id: &str) -> Result<bool> {
        let sql = "UPDATE cards SET resumed_at = NULL WHERE id = ?1 AND resumed_at IS NOT NULL";
        Ok(self.db.locked().execute(sql, [id])? == 1)
    }

    /// An owed resume that can't happen (the person lost the agent, the session is gone): the card ends.
    pub fn card_resume_drop(&self, id: &str) -> Result<bool> {
        Ok(self.db.locked().execute(
            "UPDATE cards SET state = 'withdrawn' WHERE id = ?1 AND resumed_at IS NULL AND state IN ('approved','used')",
            [id],
        )? == 1)
    }
}

/// The owed-resume condition; `?1` is now.
const OWED: &str = "answered_parked = 1 AND granted_at IS NOT NULL AND resumed_at IS NULL AND deadline >= ?1
    AND (state = 'approved' OR (state = 'used' AND used_by_ask = 1))";

#[cfg(test)]
mod tests {
    use super::super::tests::temp_store;
    use super::*;

    pub(crate) fn ask(session: &str, resource: &str) -> NewCard {
        NewCard {
            session: session.into(),
            source: "run",
            bot_id: None,
            run_id: Some("r1".into()),
            person: "sam".into(),
            agent: "echo".into(),
            approver: "owner",
            tool: "Bash".into(),
            input: "{}".into(),
            resource: resource.into(),
            shown: resource.into(),
            deadline: 10_000,
            reply_to: None,
            asked_by: None,
        }
    }

    fn new(s: &Store, session: &str, resource: &str) -> Card {
        match s.card_open(&ask(session, resource), 100).unwrap() {
            Opened::New(c) => c,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_repeat_joins_the_open_card_and_only_one_is_open_per_session_and_resource() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:ls");
        for state in [CardState::Pending, CardState::Live, CardState::Parked] {
            s.card_live(&a.id, None, None).unwrap();
            if state == CardState::Parked {
                s.cards_park_run("r1").unwrap();
            }
            let Opened::Joined(j) = s.card_open(&ask("s1", "Bash:ls"), 101).unwrap() else { panic!("joins") };
            assert_eq!(j.id, a.id);
        }
        // Another session, or another resource, is its own card.
        assert!(matches!(s.card_open(&ask("s2", "Bash:ls"), 101).unwrap(), Opened::New(_)));
        assert!(matches!(s.card_open(&ask("s1", "Bash:pwd"), 101).unwrap(), Opened::New(_)));
        assert_eq!(s.cards_open(None).len(), 3);
    }

    #[test]
    fn the_first_answer_wins_and_a_closed_card_takes_none() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:ls");
        s.card_live(&a.id, Some("e1"), None).unwrap();
        let Answered::Applied { card, was_live } = s.card_answer(&a.id, true, "allow", "kai", 200, false).unwrap()
        else {
            panic!("applied")
        };
        assert!(was_live && card.state == CardState::Approved && card.granted_at == Some(200) && !card.answered_parked);
        // A second device, a late retry, a flip to no: nothing (A1).
        for allow in [true, false] {
            assert_eq!(
                s.card_answer(&a.id, allow, "x", "kai", 201, false).unwrap(),
                Answered::Nothing(Some(CardState::Approved))
            );
        }
        assert_eq!(s.card_answer("nope", true, "x", "kai", 201, false).unwrap(), Answered::Nothing(None));
        assert_eq!(s.card(&a.id).unwrap().answered_by.as_deref(), Some("kai"));
    }

    #[test]
    fn expired_denied_and_withdrawn_never_become_access() {
        let s = temp_store();
        let late = new(&s, "s1", "Bash:a");
        // Past the deadline the answer finds it expired and changes nothing else.
        assert_eq!(
            s.card_answer(&late.id, true, "allow", "kai", 10_001, false).unwrap(),
            Answered::Nothing(Some(CardState::Expired))
        );
        assert_eq!(s.card(&late.id).unwrap().state, CardState::Expired);
        let denied = new(&s, "s1", "Bash:b");
        s.card_answer(&denied.id, false, "deny", "kai", 200, false).unwrap();
        let withdrawn = new(&s, "s1", "Bash:c");
        assert!(s.card_withdraw(&withdrawn.id).unwrap() && !s.card_withdraw(&withdrawn.id).unwrap());
        for id in [&late.id, &denied.id, &withdrawn.id] {
            assert!(matches!(s.card_answer(id, true, "allow", "kai", 300, false).unwrap(), Answered::Nothing(_)));
            assert!(!s.card_use(id).unwrap(), "no grant to use");
        }
        for r in ["Bash:a", "Bash:b", "Bash:c"] {
            assert!(
                matches!(s.card_open(&ask("s1", r), 400).unwrap(), Opened::New(_)),
                "{r} asks again; nothing granted"
            );
        }
    }

    #[test]
    fn a_once_grant_is_used_once_by_its_own_session() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:ls");
        s.cards_park_run("r1").unwrap();
        let Answered::Applied { card, was_live } = s.card_answer(&a.id, true, "allow", "kai", 200, false).unwrap()
        else {
            panic!("applied")
        };
        assert!(!was_live && card.answered_parked, "answered while the asker was gone: the grant waits");
        assert_eq!(s.cards_awaiting_resume(0).len(), 1);
        // Another thread of the same person cannot use it (C8); the same session can, once (A4).
        assert!(matches!(s.card_open(&ask("s2", "Bash:ls"), 300).unwrap(), Opened::New(_)));
        assert!(matches!(s.card_open(&ask("s1", "Bash:ls"), 301).unwrap(), Opened::Granted(_)));
        assert!(matches!(s.card_open(&ask("s1", "Bash:ls"), 302).unwrap(), Opened::New(_)), "the grant is spent");
        assert_eq!(s.card(&a.id).unwrap().state, CardState::Used);
        // Used by a later ask first: the resume is still owed, as a wake that carries no grant (C7, A9) ...
        assert_eq!(s.cards_awaiting_resume(0).len(), 1);
        assert!(s.card_resume_claim(&a.id, 400).unwrap());
        assert!(!s.card_resume_claim(&a.id, 401).unwrap(), "one resume per card");
        assert_eq!(s.cards_awaiting_resume(0).len(), 0);
        // ... and a refused hand-over gives the claim back.
        assert!(s.card_resume_release(&a.id).unwrap());
        assert_eq!(s.cards_awaiting_resume(0).len(), 1);
        assert!(s.cards_awaiting_resume(10_001).is_empty(), "past its deadline nothing is owed");
    }

    #[test]
    fn silence_expires_parked_cards_and_unused_grants() {
        let s = temp_store();
        let parked = new(&s, "s1", "Bash:a");
        s.cards_park_run("r1").unwrap();
        let granted = new(&s, "s1", "Bash:b");
        s.card_answer(&granted.id, true, "allow", "kai", 200, false).unwrap();
        let fresh = s.card_open(&NewCard { deadline: 99_999, ..ask("s1", "Bash:c") }, 300).unwrap();
        let Opened::New(fresh) = fresh else { panic!() };
        let swept: Vec<_> = s.cards_sweep(10_001).unwrap().into_iter().map(|c| c.id).collect();
        assert!(swept.contains(&parked.id) && swept.contains(&granted.id) && !swept.contains(&fresh.id));
        assert_eq!(s.card(&parked.id).unwrap().state, CardState::Expired);
        assert_eq!(s.card(&granted.id).unwrap().state, CardState::Expired);
        assert!(
            matches!(s.card_open(&ask("s1", "Bash:b"), 10_002).unwrap(), Opened::New(_)),
            "an expired grant is no grant"
        );
    }

    #[test]
    fn revoking_a_person_withdraws_their_open_cards_and_unused_grants_only() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:a");
        let b = new(&s, "s1", "Bash:b");
        s.card_answer(&b.id, true, "allow", "kai", 200, false).unwrap();
        let used = new(&s, "s1", "Bash:c");
        s.card_answer(&used.id, true, "allow", "kai", 200, false).unwrap();
        s.card_use(&used.id).unwrap();
        let other = s.card_open(&NewCard { person: "eve".into(), ..ask("s9", "Bash:a") }, 100).unwrap();
        let Opened::New(other) = other else { panic!() };
        let gone: Vec<_> = s.cards_withdraw_person("sam", None).unwrap().into_iter().map(|c| c.id).collect();
        assert!(gone.contains(&a.id) && gone.contains(&b.id) && !gone.contains(&used.id));
        assert_eq!(s.card(&a.id).unwrap().state, CardState::Withdrawn);
        assert_eq!(s.card(&used.id).unwrap().state, CardState::Used, "history stays");
        assert_eq!(s.card(&other.id).unwrap().state, CardState::Pending, "someone else's is untouched");
        assert!(matches!(s.card_answer(&a.id, true, "allow", "kai", 300, false).unwrap(), Answered::Nothing(_)));
        assert!(s.cards_withdraw_person("", None).is_err(), "never the owner's");
        // One agent only.
        let c = new(&s, "s3", "Bash:z");
        assert_eq!(s.cards_withdraw_person("sam", Some("fox")).unwrap().len(), 0);
        assert_eq!(s.cards_withdraw_person("sam", Some("echo")).unwrap()[0].id, c.id);
    }

    #[test]
    fn a_restart_parks_live_cards_and_keeps_the_rest() {
        let s = temp_store();
        let live = new(&s, "s1", "Bash:a");
        s.card_live(&live.id, Some("e1"), None).unwrap();
        let pending = new(&s, "s1", "Bash:b");
        let approved = new(&s, "s1", "Bash:c");
        s.card_answer(&approved.id, true, "allow", "kai", 200, false).unwrap();
        let parked: Vec<_> = s.cards_restart(500).unwrap().into_iter().map(|c| c.id).collect();
        assert!(parked.contains(&live.id) && parked.contains(&pending.id) && !parked.contains(&approved.id));
        assert_eq!(s.card(&live.id).unwrap().state, CardState::Parked);
        assert_eq!(s.card(&approved.id).unwrap().state, CardState::Approved, "an answered card is not undone");
        // An approval that lost its grant is re-derived; one that never was a yes is not.
        let c = s.db.locked();
        c.execute("UPDATE cards SET granted_at = NULL WHERE id = ?1", [&approved.id]).unwrap();
        drop(c);
        s.cards_restart(600).unwrap();
        assert_eq!(s.card(&approved.id).unwrap().granted_at, Some(600));
        let denied = new(&s, "s1", "Bash:d");
        s.card_answer(&denied.id, false, "deny", "kai", 200, false).unwrap();
        s.cards_restart(700).unwrap();
        assert_eq!(s.card(&denied.id).unwrap().granted_at, None);
    }

    #[test]
    fn a_spent_yes_cannot_be_taken_twice_and_goes_back_if_nobody_heard_it() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:ls");
        s.card_live(&a.id, Some("e"), None).unwrap();
        let Answered::Applied { card, was_live } = s.card_answer(&a.id, true, "allow", "kai", 200, true).unwrap()
        else {
            panic!()
        };
        assert!(was_live && card.state == CardState::Used, "spent in the answer itself");
        assert!(
            matches!(s.card_open(&ask("s1", "Bash:ls"), 201).unwrap(), Opened::New(_)),
            "a parallel ask finds no grant"
        );
        // Nobody heard it: the grant is back, as the answer to a parked card.
        assert!(s.card_undelivered(&a.id).unwrap());
        let back = s.card(&a.id).unwrap();
        assert!(back.state == CardState::Approved && back.answered_parked);
        // A grant taken by an ask is never given back.
        let b = new(&s, "s2", "Bash:ls");
        s.cards_park_run("r1").unwrap();
        s.card_answer(&b.id, true, "allow", "kai", 202, false).unwrap();
        assert!(matches!(s.card_open(&ask("s2", "Bash:ls"), 203).unwrap(), Opened::Granted(_)));
        assert!(!s.card_undelivered(&b.id).unwrap());
        // No spend for a parked card or a no.
        let c = new(&s, "s3", "Bash:ls");
        s.cards_park_run("r1").unwrap();
        let Answered::Applied { card, .. } = s.card_answer(&c.id, true, "allow", "kai", 204, true).unwrap() else {
            panic!()
        };
        assert_eq!(card.state, CardState::Approved);
        assert_eq!(s.cards_agents("sam"), vec!["echo".to_owned()]);
    }

    #[test]
    fn parking_follows_the_process_not_the_clock() {
        let s = temp_store();
        let a = new(&s, "s1", "Bash:a");
        s.card_live(&a.id, None, Some("r1")).unwrap();
        assert!(s.cards_park_run("r2").unwrap().is_empty(), "another run's exit parks nothing");
        assert_eq!(s.cards_park_run("r1").unwrap().len(), 1);
        assert_eq!(s.card(&a.id).unwrap().state, CardState::Parked);
        s.card_live(&a.id, None, Some("r3")).unwrap();
        assert_eq!(s.card(&a.id).unwrap().state, CardState::Live, "a new process joining makes it live again");
    }
}
