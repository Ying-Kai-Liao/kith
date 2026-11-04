//! Access requests (M2-3): a device asking this computer for access, and the owner's yes or no. This is today's flow of
//! `serve/host/src/remote/cloud.rs` (`sync_requests` files a request, `decide` / `settle` answer it; `accessRequests`
//! and `decideAccessRequest` in `api/remote.rs`; `kith-serve access list|approve|deny`), moved into the core with the
//! same decisions. `kernel/bin/kith-auth access` is NOT this: that is a grant of a folder or a domain to a person.
//!
//! # What moves and what stays
//!
//! The core decides; the cloud and the device store stay the host's. `approve` does what `settle` does before it
//! calls the cloud: the refusals, the request's end, and then it says which device to add (key, name, platform, person,
//! scopes). The host then asks the cloud for the grant and stores the device, as `settle` does after the decision.
//! `deny` is the request's end. Switching the host onto these rows is M2-17.
//!
//! The SAS nonces (`commit`, `host_nonce`) are the host's secrets and stay in its memory; a row carries the code the
//! owner compares once the host has worked it out.
//!
//! # Rules, as `settle` has them
//!
//! - An unknown request, one already decided, and one that ran out are "unknown request": the host forgets a request
//!   once decided or when the cloud stops listing it. (The host does not look at the clock itself; the cloud drops an
//!   expired request on its next pull. Here a pending request past `expires_at` is expired the same way: not listed,
//!   not decidable.)
//! - An approval needs the device's code first ("The device hasn't shown its code yet."). A denial does not.
//! - A web browser is never the owner's device: approving the owner's own request from one denies it and refuses.
//! - A member's request whose person ended while it waited (suspended, removed, never known) is denied and refused on
//!   approval. Denying one is plain.
//! - `members`: the owner's other devices answer members' requests only, never the owner's own (the host's
//!   `is_member_request`); anything else is "This can only be done at the computer itself.", for an unknown id too.
//! - Two devices answering at once: each answer is a `BEGIN IMMEDIATE` claim transaction, so the second one finds the
//!   request decided and is refused.

use kith_kernel::secrets::{self, Ctx};
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde_json::{Value, json};

use super::write;
use crate::db::Core;
use crate::error::{Error, Result};

const COLUMNS: &str =
    "id, device_key, device_name, platform, email, person, code, status, created_at, expires_at";

/// A request as the table keeps it.
struct Req {
    id: String,
    device_key: String,
    device_name: String,
    platform: String,
    email: Option<String>,
    person: Option<String>,
    code: Option<String>,
    status: String,
    created_at: i64,
    expires_at: i64,
}

impl Req {
    fn of(r: &Row<'_>) -> rusqlite::Result<Req> {
        Ok(Req {
            id: r.get(0)?,
            device_key: r.get(1)?,
            device_name: r.get(2)?,
            platform: r.get(3)?,
            email: r.get(4)?,
            person: r.get(5)?,
            code: r.get(6)?,
            status: r.get(7)?,
            created_at: r.get(8)?,
            expires_at: r.get(9)?,
        })
    }

    /// Still waiting for the owner: pending and not run out.
    fn open(&self, now: i64) -> bool {
        self.status == "pending" && self.expires_at > now
    }

    /// The shape of the host's `Request::json`, which `accessRequests` answers.
    fn json(&self) -> Value {
        json!({
            "requestId": self.id,
            "deviceKey": self.device_key,
            "deviceName": self.device_name,
            "platform": self.platform,
            "email": self.email,
            "personId": self.person,
            "code": self.code,
            "createdAt": self.created_at,
            "expiresAt": self.expires_at,
        })
    }
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

/// The host's `Forbidden` (a remote device answering a request that is not a member's).
fn only_at_the_computer() -> Error {
    refuse("This can only be done at the computer itself.")
}

fn load(tx: &Connection, id: &str) -> Result<Option<Req>> {
    Ok(tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM access_requests WHERE id = ?1"),
            [id],
            Req::of,
        )
        .optional()?)
}

/// The request, if it can still be decided. `members` is checked first, as the host checks `Forbidden` before it looks
/// at the request at all.
fn open(tx: &Connection, c: &Ctx, id: &str, members: bool) -> Result<Req> {
    let found = load(tx, id)?.filter(|r| r.open(now_of(c)));
    if members && found.as_ref().is_none_or(|r| r.person.is_none()) {
        return Err(only_at_the_computer());
    }
    found.ok_or_else(|| refuse("unknown request"))
}

/// Ends the request. The `status = 'pending'` guard is a second line behind the transaction.
fn end(tx: &Connection, c: &Ctx, id: &str, to: &str) -> Result<()> {
    let n = tx.execute(
        "UPDATE access_requests SET status = ?2, decided_at = ?3 WHERE id = ?1 AND status = 'pending'",
        params![id, to, now_of(c)],
    )?;
    if n == 1 {
        Ok(())
    } else {
        Err(refuse("unknown request"))
    }
}

/// Whether the person still has an account on this computer: someone kith knows who is not cut off. (The host asks its
/// membership table, which the removal and the suspension of a person end.)
fn is_member(tx: &Connection, person: &str) -> Result<bool> {
    let mut auth = super::read_tables(tx, &mut || {})?;
    write::tables(&mut auth);
    let known = auth
        .get("people")
        .and_then(|p| p.get(person))
        .is_some_and(Value::is_object);
    Ok(known && !secrets::cut_off(&auth, &secrets::canonical(&auth, person)))
}

/// A web browser is only ever an invitee's device (the platform is the device's own claim; a missing one counts as
/// native).
fn is_web(platform: &str) -> bool {
    platform.eq_ignore_ascii_case("web")
}

/// `people.requests` (`accessRequests`): the requests waiting for the owner, oldest first. `members`: only the
/// members' (what the owner's other devices see).
pub(super) fn list(core: &mut Core, c: &Ctx, members: bool) -> Result<Value> {
    let tx = core.claim_tx()?;
    let now = now_of(c);
    let mut q = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM access_requests WHERE status = 'pending' ORDER BY created_at, id"
    ))?;
    let all = q
        .query_map([], Req::of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows: Vec<Value> = all
        .iter()
        .filter(|r| r.open(now) && (!members || r.person.is_some()))
        .map(Req::json)
        .collect();
    Ok(json!({ "requests": rows }))
}

/// What `approve` settled: the answer, or a refusal that must be kept (the request is denied by it).
enum Outcome {
    Done(Value),
    Denied(&'static str),
}

/// `people.approve` (`decideAccessRequest` with `approve: true`, up to the cloud's grant).
pub(super) fn approve(core: &mut Core, c: &Ctx, id: &str, members: bool) -> Result<Value> {
    let tx = core.claim_tx()?;
    // The refusals that end the request commit; the transaction is not dropped for them.
    let said = approve_in(&tx, c, id, members)?;
    tx.commit()?;
    match said {
        Outcome::Done(v) => Ok(v),
        Outcome::Denied(why) => Err(refuse(why)),
    }
}

fn approve_in(tx: &Connection, c: &Ctx, id: &str, members: bool) -> Result<Outcome> {
    let r = open(tx, c, id, members)?;
    if r.code.is_none() {
        return Err(refuse("The device hasn't shown its code yet."));
    }
    if r.person.is_none() && is_web(&r.platform) {
        end(tx, c, id, "denied")?;
        return Ok(Outcome::Denied(
            "A web browser can't be the owner's device.",
        ));
    }
    if let Some(person) = &r.person
        && !is_member(tx, person)?
    {
        end(tx, c, id, "denied")?;
        return Ok(Outcome::Denied(
            "That account is no longer a member of this computer.",
        ));
    }
    end(tx, c, id, "approved")?;
    Ok(Outcome::Done(json!({
        "requestId": r.id,
        "status": "approved",
        // The device `settle` stores once the cloud has given its grant. A member never shares this computer's screen.
        "device": {
            "key": r.device_key,
            "name": r.device_name.chars().take(100).collect::<String>(),
            "platform": r.platform,
            "personId": r.person,
            "scopes": if r.person.is_some() { json!(["control"]) } else { json!(["control", "screen"]) },
        },
    })))
}

/// `people.deny` (`decideAccessRequest` with `approve: false`): the request ends; the host tells the cloud.
pub(super) fn deny(core: &mut Core, c: &Ctx, id: &str, members: bool) -> Result<Value> {
    let tx = core.claim_tx()?;
    open(&tx, c, id, members)?;
    end(&tx, c, id, "denied")?;
    tx.commit()?;
    Ok(json!({ "requestId": id, "status": "denied" }))
}

/// A request the cloud lists and the host takes up (`sync_requests`). Not an owner row: no `people.*` method files
/// one. M2-17 calls it when the host switches.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct NewRequest<'a> {
    pub id: &'a str,
    pub device_key: &'a str,
    pub device_name: &'a str,
    pub platform: &'a str,
    pub email: Option<&'a str>,
    pub person: Option<&'a str>,
    pub created_at: i64,
    pub expires_at: i64,
}

/// How long a decided or run-out request stays in the table, so the cloud listing it once more cannot file it again
/// as a new one.
const KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Files a request, pending, with the refusals `sync_requests` has: a member who is not a member (the host denies it on
/// the cloud) and a second request from a device that has one waiting for its code (it waits its turn there).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn create(core: &mut Core, c: &Ctx, new: &NewRequest<'_>) -> Result<()> {
    let tx = core.claim_tx()?;
    let now = now_of(c);
    if new.id.is_empty() || new.device_key.is_empty() {
        return Err(Error::BadParams(
            "authority: a request needs an id and a device key".to_owned(),
        ));
    }
    // The host also denies a web browser asking for the owner's own account at this point; `approve` keeps its own
    // refusal (as `settle` does), so a request that got past is still not approved.
    if let Some(p) = new.person
        && !is_member(&tx, p)?
    {
        return Err(refuse("not a member of this computer"));
    }
    tx.execute(
        "DELETE FROM access_requests WHERE (status != 'pending' AND decided_at < ?1) \
         OR (status = 'pending' AND expires_at < ?1)",
        [now - KEEP_MS],
    )?;
    let waiting: i64 = tx.query_row(
        "SELECT count(*) FROM access_requests WHERE device_key = ?1 AND code IS NULL \
         AND status = 'pending' AND expires_at > ?2",
        params![new.device_key, now],
        |r| r.get(0),
    )?;
    if waiting > 0 {
        return Err(refuse("this device has a request waiting for its code"));
    }
    tx.execute(
        "INSERT INTO access_requests (id, device_key, device_name, platform, email, person, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            new.id,
            new.device_key,
            new.device_name,
            new.platform,
            new.email,
            new.person,
            new.created_at,
            new.expires_at
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            refuse(format!("request {} is already known", new.id))
        }
        e => e.into(),
    })?;
    tx.commit()?;
    Ok(())
}

/// The device revealed a nonce that matches its commit: the owner can now compare this code.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reveal(db: &mut Core, id: &str, digits: &str) -> Result<()> {
    let tx = db.claim_tx()?;
    let n = tx.execute(
        "UPDATE access_requests SET code = ?2 WHERE id = ?1 AND status = 'pending' AND code IS NULL",
        params![id, digits],
    )?;
    if n != 1 {
        return Err(refuse("unknown request"));
    }
    tx.commit()?;
    Ok(())
}

/// The status column of one request, read straight from the table (the tests do not ask the module that wrote it).
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(super) fn status_of(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row(
        "SELECT status FROM access_requests WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .optional()
    .expect("status")
}

/// The body of [`approve`] / [`deny`] on a connection of its own, for the tests of two devices answering at once.
#[cfg(test)]
pub(super) fn decide_on(conn: &mut Connection, c: &Ctx, id: &str, approve: bool) -> Result<Value> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let said = if approve {
        match approve_in(&tx, c, id, false)? {
            Outcome::Done(v) => Ok(v),
            Outcome::Denied(why) => Err(refuse(why)),
        }
    } else {
        open(&tx, c, id, false)?;
        end(&tx, c, id, "denied")?;
        Ok(json!({ "requestId": id, "status": "denied" }))
    };
    tx.commit()?;
    said
}
