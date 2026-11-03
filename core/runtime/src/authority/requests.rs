//! Access requests (M2-3): a person asking the owner for use of some agents, and the owner's yes or no.
//!
//! # Where requests come from today
//!
//! Nothing in `kernel/bin/kith-auth` or `kith-people` has a request: there the owner only grants (`member`) and an
//! invited person only binds a platform handle (`link`, the codes `people.invite` makes). The "access requests" that
//! exist today are the host's, and they are DEVICE requests, not requests for agents:
//! - `serve/cloud` (`createAccessRequest`, `/v1/computers/{id}/access-requests`) takes a device's key and its SAS
//!   commit; the host lists them (`HostAccessRequest`: device key and name, platform, e-mail, expiry) and shows them as
//!   `accessRequests` (`api/remote.rs`; `kith-serve access list`).
//! - `decideAccessRequest` -> `remote/cloud.rs` `decide` -> `settle` refuses an approval before the device has shown its
//!   code, a web browser for the owner's own device, and a member's device whose membership ended while it waited
//!   (the request is then denied on the cloud and dropped). Otherwise it asks the cloud for a grant, and on a yes
//!   stores the DEVICE with that grant (`Control` scope; `Screen` only for the owner's own). A no only tells the cloud.
//!   Only the owner's other devices may answer a member's request (`is_member_request`).
//!
//! That is who may connect a phone or a browser. It says nothing about which agents the person may use: that is the
//! membership (`people.grant`), which stays the owner's own write.
//!
//! The requests here are the other half, which M2 adds: "this person asks for these agents, with this preset and mode".
//! Approving one IS a `people.grant` (`write::member`, the one write path of M2-2) of exactly that, for all the agents
//! or none, in the transaction that marks the request approved. The device approval of the host stays where it is (lane
//! H decides how the two meet); this module neither reads nor changes devices.
//!
//! # Rules
//!
//! - Approving gives what was asked and nothing else: the person's preset on each agent becomes the requested one (a
//!   plain `guest` when none was asked) and their mode the requested one (cleared when none was). `deny`, `model`,
//!   `helpers` and `budget` the owner already set on a row are left as they are. A free-shell mode is given only when
//!   the owner approves with `trusted`, as `people.grant` needs, and a request never carries `trusted` itself.
//!   Everything `member` refuses is refused here, and after the write each agent is asked of M2-1's `resolve`: one that
//!   does not allow the person (a bot whose owner does not hold the agent) refuses the whole approval.
//! - A refusal leaves the request pending, except a person who is gone or revoked: that one is denied and recorded, as
//!   the host denies a request of a member whose membership ended.
//! - A request is decided once. A second approve or deny, an approve after a deny (or the reverse) and a decision
//!   after the request ran out are refused, never repeated: the host forgets a request once decided, so a repeat is
//!   "unknown request" there.
//! - Expired is not stored: a pending request past `expires_at` is expired. It is refused and no longer listed.
//! - Two owner devices answering at once: each write is a `BEGIN IMMEDIATE` transaction, so the second reads the
//!   request already decided and is refused.

use kith_kernel::secrets::{self, Ctx, Rules};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::store;
use super::write::{self, Settings};
use crate::db::Core;
use crate::error::{Error, Result};

/// A request as the table keeps it.
struct Req {
    id: String,
    person: String,
    agents: Vec<String>,
    preset: Option<String>,
    mode: Option<String>,
    via: Option<String>,
    status: String,
    created_at: i64,
    expires_at: i64,
    decided_at: Option<i64>,
}

impl Req {
    /// What the owner sees: a pending request past its time is `expired`.
    fn state(&self, now: i64) -> &str {
        if self.status == "pending" && self.expires_at <= now {
            "expired"
        } else {
            &self.status
        }
    }

    fn json(&self, now: i64) -> Value {
        json!({
            "id": self.id,
            "person": self.person,
            "agents": self.agents,
            "preset": self.preset,
            "mode": self.mode,
            "via": self.via,
            "status": self.state(now),
            "createdAt": self.created_at,
            "expiresAt": self.expires_at,
            "decidedAt": self.decided_at,
        })
    }
}

const COLUMNS: &str =
    "id, person, agents, preset, mode, via, status, created_at, expires_at, decided_at";

fn req_of(r: &rusqlite::Row<'_>) -> Result<Req> {
    let agents: String = r.get(2)?;
    let agents = serde_json::from_str::<Vec<String>>(&agents)
        .map_err(|e| super::bad("access_requests.agents", e))?;
    Ok(Req {
        id: r.get(0)?,
        person: r.get(1)?,
        agents,
        preset: r.get(3)?,
        mode: r.get(4)?,
        via: r.get(5)?,
        status: r.get(6)?,
        created_at: r.get(7)?,
        expires_at: r.get(8)?,
        decided_at: r.get(9)?,
    })
}

fn now_of(c: &Ctx) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    {
        c.now_ms as i64
    }
}

fn refuse(msg: impl std::fmt::Display) -> Error {
    Error::Refused(format!("authority: {msg}"))
}

fn load(tx: &Connection, id: &str) -> Result<Option<Req>> {
    let mut q = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM access_requests WHERE id = ?1"
    ))?;
    let mut rows = q.query([id])?;
    match rows.next()? {
        Some(r) => Ok(Some(req_of(r)?)),
        None => Ok(None),
    }
}

/// The request, if it can still be decided: it exists, is pending and has not run out.
fn open(tx: &Connection, c: &Ctx, id: &str) -> Result<Req> {
    let r = load(tx, id)?.ok_or_else(|| refuse(format!("no access request '{id}'")))?;
    match r.state(now_of(c)) {
        "pending" => Ok(r),
        "expired" => Err(refuse(format!(
            "access request {id} expired: ask for a new one"
        ))),
        done => Err(refuse(format!("access request {id} was already {done}"))),
    }
}

/// Records the decision. The `status = 'pending'` guard is a second line behind the transaction.
fn decide(tx: &Connection, c: &Ctx, id: &str, to: &str) -> Result<()> {
    let n = tx.execute(
        "UPDATE access_requests SET status = ?2, decided_at = ?3 WHERE id = ?1 AND status = 'pending'",
        params![id, to, now_of(c)],
    )?;
    if n == 1 {
        Ok(())
    } else {
        Err(refuse(format!("access request {id} was already decided")))
    }
}

/// `people.requests`: the requests waiting for the owner (`all`: every one, decided and expired too), oldest first.
pub(super) fn list(core: &mut Core, c: &Ctx, all: bool) -> Result<Value> {
    let tx = core.claim_tx()?;
    let now = now_of(c);
    let mut q = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM access_requests ORDER BY created_at, id"
    ))?;
    let mut rows = q.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let r = req_of(r)?;
        if all || r.state(now) == "pending" {
            out.push(r.json(now));
        }
    }
    Ok(json!({ "rows": out }))
}

enum Outcome {
    Done(Value),
    /// The person is gone or revoked: the request is denied (and that is kept) and the approval refused.
    Gone(String),
}

/// `people.approve`.
pub(super) fn approve(core: &mut Core, c: &Ctx, id: &str, trusted: bool) -> Result<Value> {
    match store::run_tx(core, c, |tx, a| approve_in(tx, c, a, id, trusted))? {
        Outcome::Done(v) => Ok(v),
        Outcome::Gone(why) => Err(refuse(why)),
    }
}

fn approve_in(tx: &Connection, c: &Ctx, a: &mut Value, id: &str, trusted: bool) -> Result<Outcome> {
    let r = open(tx, c, id)?;
    if r.agents.is_empty() {
        return Err(refuse(format!("access request {id} names no agent")));
    }
    let known = a
        .get("people")
        .and_then(|p| p.get(&r.person))
        .is_some_and(Value::is_object);
    if !known || secrets::cut_off(a, &secrets::canonical(a, &r.person)) {
        decide(tx, c, id, "denied")?;
        let why = if known {
            format!("{}'s access was removed", r.person)
        } else {
            format!("{} is no longer a person kith knows", r.person)
        };
        return Ok(Outcome::Gone(format!(
            "{why}: access request {id} is denied"
        )));
    }
    // The grant sets the preset and the mode to the requested ones, clearing what the row had when nothing was
    // asked, so the access after is the request and not the request on top of an older row.
    let kv: Settings = [
        (
            "preset".to_owned(),
            r.preset.clone().unwrap_or_else(|| "guest".to_owned()),
        ),
        ("mode".to_owned(), r.mode.clone().unwrap_or_default()),
    ]
    .into();
    let said = write::member(a, c, &r.agents.join(","), &r.person, &kv, trusted, false)
        .map_err(|d| store::refusal(&d))?;
    for agent in &r.agents {
        let got = Rules::new(c, a)
            .resolve(agent, &r.person, None, false)
            .map_err(|d| store::refusal(&d))?;
        if got.get("allowed") != Some(&Value::Bool(true)) {
            return Err(refuse(format!(
                "{} may not use {agent} now: {}",
                r.person,
                got.get("why").and_then(Value::as_str).unwrap_or_default()
            )));
        }
    }
    decide(tx, c, id, "approved")?;
    Ok(Outcome::Done(json!({
        "id": id,
        "status": "approved",
        "person": r.person,
        "agents": r.agents,
        "lines": said,
    })))
}

/// `people.deny`.
pub(super) fn deny(core: &mut Core, c: &Ctx, id: &str) -> Result<Value> {
    store::run_tx(core, c, |tx, _| deny_in(tx, c, id))
}

fn deny_in(tx: &Connection, c: &Ctx, id: &str) -> Result<Value> {
    let r = open(tx, c, id)?;
    decide(tx, c, id, "denied")?;
    Ok(json!({
        "id": id,
        "status": "denied",
        "person": r.person,
        "lines": [format!("access request {id} from {} denied", r.person)],
    }))
}

/// A request somebody makes (a member's device through the host, an invite code): not an owner row, so no
/// `people.*` method carries it. Lane H's callers (`serve/host`) reach it when they switch.
pub(crate) struct NewRequest<'a> {
    pub id: &'a str,
    pub person: &'a str,
    pub agents: &'a [&'a str],
    pub preset: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub via: Option<&'a str>,
    pub ttl_ms: i64,
}

/// Files a request, pending. The person must be someone kith knows and not revoked (as `invite` needs); the agents,
/// the preset and the mode are checked when the owner approves, since they can change while it waits.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn create(core: &mut Core, c: &Ctx, new: &NewRequest<'_>) -> Result<Value> {
    store::run_tx(core, c, |tx, a| create_in(tx, c, a, new))
}

fn create_in(tx: &Connection, c: &Ctx, a: &Value, new: &NewRequest<'_>) -> Result<Value> {
    let bad = |msg: &str| Error::BadParams(format!("authority: {msg}"));
    if new.id.is_empty() {
        return Err(bad("a request needs an id"));
    }
    let mut agents: Vec<&str> = Vec::new();
    for x in new.agents {
        if x.is_empty() || x.contains(',') {
            return Err(bad("an agent is a name, one at a time"));
        }
        if !agents.contains(x) {
            agents.push(x);
        }
    }
    if agents.is_empty() {
        return Err(bad("a request names at least one agent"));
    }
    if new.person == "*" || new.person.is_empty() {
        return Err(bad("a request is from one person, not everyone"));
    }
    if new.ttl_ms <= 0 {
        return Err(bad("a request must stay open for some time"));
    }
    let known = a
        .get("people")
        .and_then(|p| p.get(new.person))
        .is_some_and(Value::is_object);
    if !known {
        return Err(refuse(format!("no person '{}'", new.person)));
    }
    if secrets::cut_off(a, &secrets::canonical(a, new.person)) {
        return Err(refuse(format!("{}'s access was removed", new.person)));
    }
    let now = now_of(c);
    tx.execute(
        "INSERT INTO access_requests (id, person, agents, preset, mode, via, status, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
        params![
            new.id,
            new.person,
            json!(agents).to_string(),
            new.preset,
            new.mode,
            new.via,
            now,
            now.saturating_add(new.ttl_ms)
        ],
    )?;
    Ok(load(tx, new.id)?.map_or(Value::Null, |r| r.json(now)))
}

/// The status column of one request, read straight from the table (the tests do not ask the module that wrote it).
#[cfg(test)]
pub(super) fn status_of(conn: &Connection, id: &str) -> Option<String> {
    use rusqlite::OptionalExtension as _;
    conn.query_row(
        "SELECT status FROM access_requests WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

/// The same body as [`approve`] and [`deny`] on a connection of its own, for the tests of two devices at once.
#[cfg(test)]
pub(super) fn decide_on(conn: &mut Connection, c: &Ctx, id: &str, approve: bool) -> Result<Value> {
    let said = store::run_conn(conn, c, |tx, a| {
        if approve {
            approve_in(tx, c, a, id, false)
        } else {
            deny_in(tx, c, id).map(Outcome::Done)
        }
    })?;
    match said {
        Outcome::Done(v) => Ok(v),
        Outcome::Gone(why) => Err(refuse(why)),
    }
}
