//! Secret cards in the core (M2-12, lane E): the owner types a key into a Key card and the core seals it into the
//! envelope (`secrets/`) under the card's claim. Checked as `verification/secrets/Secrets.tla` (variants `core*`); the
//! host's version, `serve/host/src/cards/secret.rs`, keeps deciding until lane H switches its callers.
//!
//! The order, and why (each step is a transaction of its own, but the last two are one):
//!
//! 1. **claim** ([`begin`]): an open card before its deadline, nobody else holding it. The holder is unique to this call
//!    (a second owner device is another holder), the claim carries this process's pid, and it is long enough
//!    ([`CLAIM_MS`]) to outlast the key store's tools, which are killed at their own timeout inside the call. A second
//!    `provide`, a plain answer, a no, the sweep, a withdrawal and a park all find the claim and leave the card alone.
//! 2. **warm** ([`commit`]): the master key is read from the key store BEFORE the write transaction, so a slow or locked
//!    key store cannot hold core.db's write lock. (Only the very first key of a computer is minted inside it.)
//! 3. **write and save, one commit**: the same transaction checks that the claim is still ours and the card is still
//!    open, seals every value with its record name as associated data, and spends the card with `saved`. Either both
//!    happen or neither: a crash, a failed seal or a lost claim leaves no key behind a card that is still open, which
//!    is what makes "at most one write per card" hold across a writer that dies (the model's `core` rows; the host,
//!    which writes the Keychain and then saves, has the gap shown by `core-split`).
//!
//! A failed step releases the claim and the card stays open ([`provide`]). A no ([`super::answer`] with allow false)
//! closes the card; every later provide finds it closed.
//!
//! The value is the only secret here. It travels the call's params into [`Zeroizing`] buffers and from there into the
//! envelope; it is in no error, no log line, no result, no card column. Zeroizing is best effort: the params object the
//! transport parsed is the caller's. Results name the service, account and variable names only.
//!
//! Left to the host's grants (lane B has no grants table yet): handing the saved key to the asking agent, and the
//! "use this account" Access card, which writes no key. Both would be another effect inside step 3.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{
    Card, Claim, State, bad, card, claim, get, new_id, now_ms, release, saved_in, text,
    void_dead_claims,
};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::gate::Liveness;
use crate::secrets::Envelope;

/// The tool a key card carries (the permission card the apps already draw).
pub(crate) const TOOL: &str = "request_secret";

/// A write has this long before the clock matters again. Past the key store's own bound (30 s a tool, a few tools).
pub(crate) const CLAIM_MS: i64 = 5 * 60 * 1000;
/// A saved card keeps its owed resume alive at least this long (the host's).
pub(crate) const RESUME_KEEP_MS: i64 = 60 * 60 * 1000;
/// The longest value a field takes, in bytes (the host's cap).
const MAX_VALUE: usize = 1536;

/// A service or account label: lowercase letters and digits, then `.` `_` `-`, at most 40 (kith-auth's).
fn label_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// A variable name: uppercase letters, digits and `_`, not starting with a digit, at most 64.
fn var_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// What a key card asks for: kept as JSON text in the card's `input` (no value of any key is ever there).
/// `{"service": "github", "vars": ["TOKEN"], "replaces": false, "profile": null}`
#[derive(Debug, PartialEq, Eq)]
struct Spec {
    service: String,
    vars: Vec<String>,
    /// The card replaces the key of `profile` (the card names it); otherwise it adds a new account and never replaces.
    replaces: bool,
    profile: Option<String>,
}

fn spec_of(card: &Card) -> Result<Spec> {
    let unreadable = || {
        Error::Refused(format!(
            "kith-core: cards: the key card {} does not say which service and variables it asks for, so nothing is saved",
            card.id
        ))
    };
    let v: Value = serde_json::from_str(&card.input).map_err(|_| unreadable())?;
    let service = v
        .get("service")
        .and_then(Value::as_str)
        .filter(|s| label_ok(s))
        .ok_or_else(unreadable)?
        .to_owned();
    let vars: Vec<String> = v
        .get("vars")
        .and_then(Value::as_array)
        .ok_or_else(unreadable)?
        .iter()
        .map(|x| x.as_str().filter(|s| var_ok(s)).map(str::to_owned))
        .collect::<Option<_>>()
        .ok_or_else(unreadable)?;
    if vars.is_empty() {
        return Err(unreadable());
    }
    Ok(Spec {
        service,
        vars,
        replaces: v.get("replaces").and_then(Value::as_bool).unwrap_or(false),
        profile: v.get("profile").and_then(Value::as_str).map(str::to_owned),
    })
}

/// What a provide will write: the account and, per variable, the record name in the envelope.
#[derive(Debug)]
struct Plan {
    service: String,
    profile: String,
    replaces: bool,
    /// `(variable, record name)` in the card's order.
    records: Vec<(String, String)>,
}

/// The values, checked against the card, and the names they go under. Nothing here claims or writes; a refusal names the
/// field, never the value.
fn plan_for(
    spec: &Spec,
    account: Option<&str>,
    values: &BTreeMap<String, Zeroizing<String>>,
) -> Result<Plan> {
    let account = if spec.replaces {
        // The card names the account it replaces; a caller's cannot move it.
        spec.profile
            .as_deref()
            .and_then(|p| p.split_once('/'))
            .filter(|(s, a)| *s == spec.service && label_ok(a))
            .map(|(_, a)| a)
    } else {
        account.filter(|a| label_ok(a))
    }
    .ok_or_else(|| bad("that account name can't be used"))?;
    if values.len() != spec.vars.len() || !spec.vars.iter().all(|v| values.contains_key(v)) {
        return Err(bad("enter every field of the key"));
    }
    if values.values().any(|x| x.len() > MAX_VALUE) {
        return Err(bad("that key is too long"));
    }
    if values.values().any(|x| x.is_empty() || x.contains('\0')) {
        return Err(bad("enter every field of the key"));
    }
    Ok(Plan {
        service: spec.service.clone(),
        profile: format!("{}/{account}", spec.service),
        replaces: spec.replaces,
        records: spec
            .vars
            .iter()
            .map(|v| (v.clone(), format!("key/{}/{account}/{v}", spec.service)))
            .collect(),
    })
}

/// The right to write for one card: the card and who holds its claim.
#[derive(Debug)]
pub(crate) struct Held {
    pub id: String,
    pub holder: String,
}

/// Why nothing was claimed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// The card is no longer open (answered, denied, saved, withdrawn, expired) or does not exist.
    Closed(Option<State>),
    /// Open, but its time ran out: the sweep closes it; nothing is written.
    Expired,
    /// Another provide holds the claim.
    Claimed(String),
}

/// What a provide came to.
#[derive(Debug)]
pub(crate) enum Provided {
    /// Written and saved: the card is `used`.
    Saved {
        card: Box<Card>,
        profile: String,
        vars: Vec<String>,
    },
    Refused(Refused),
}

/// Step 1: the claim, for this process. The caller has validated the values already: a bad value never takes the claim.
fn begin(
    core: &mut Core,
    id: &str,
    now: i64,
    live: &dyn Liveness,
) -> Result<std::result::Result<Held, Refused>> {
    let holder = format!("provide:{}", new_id()?);
    let c = Claim {
        holder: &holder,
        pid: Some(std::process::id()),
        until: now + CLAIM_MS,
    };
    if claim(core, id, &c, now, live)?.is_some() {
        return Ok(Ok(Held {
            id: id.to_owned(),
            holder,
        }));
    }
    // Not claimed: say why, from the card as it is now.
    Ok(Err(match card(core, id)? {
        None => Refused::Closed(None),
        Some(k) if !k.state.open() => Refused::Closed(Some(k.state)),
        Some(k) => match k.holder(now).map(str::to_owned) {
            Some(h) => Refused::Claimed(h),
            None => Refused::Expired,
        },
    }))
}

/// Steps 2 and 3: warm the key, then in one transaction check the claim, seal and store every value, and save the card.
/// An error rolls the whole transaction back: nothing was written. The caller releases the claim.
fn commit(
    core: &mut Core,
    env: &mut Envelope,
    held: &Held,
    plan: &Plan,
    values: &BTreeMap<String, Zeroizing<String>>,
    now: i64,
    live: &dyn Liveness,
) -> Result<Card> {
    // Reads the master key from the key store (or finds there is none to read, which a first key mints inside the
    // transaction below). A locked store fails here, before the write lock is taken.
    env.get(core.conn(), &plan.records[0].1)?;
    let lost = || {
        Error::Refused(
            "kith-core: cards: the card was closed or its claim lost before the key could be saved; nothing was written"
                .into(),
        )
    };
    let tx = core.claim_tx()?;
    void_dead_claims(&tx, live)?;
    let still = get(&tx, &held.id)?
        .filter(|k| k.state.open() && k.claim_by.as_deref() == Some(held.holder.as_str()))
        .ok_or_else(lost)?;
    if !plan.replaces {
        // A new account never silently replaces a key the owner already has under that name.
        let have = Envelope::list(&tx)?;
        if plan.records.iter().any(|(_, name)| have.contains(name)) {
            return Err(bad("that account name can't be used"));
        }
    }
    for (var, name) in &plan.records {
        let value = values
            .get(var)
            .ok_or_else(|| bad("enter every field of the key"))?;
        env.put(&tx, name, value.as_bytes())?;
    }
    if !saved_in(
        &tx,
        &held.id,
        &held.holder,
        "owner",
        now,
        now + RESUME_KEEP_MS,
    )? {
        // Not reachable after the check above in one transaction; kept so a rollback, not a half write, is the answer.
        return Err(lost());
    }
    let card = get(&tx, &still.id)?.ok_or_else(lost)?;
    tx.commit()?;
    Ok(card)
}

/// `provide`: claim, write and save a key card's values; a failure releases the claim and leaves the card open.
/// `clock` is read at the claim and again at the save (a write can take a while).
pub(crate) fn provide(
    core: &mut Core,
    env: &mut Envelope,
    id: &str,
    account: Option<&str>,
    values: &BTreeMap<String, Zeroizing<String>>,
    clock: &dyn Fn() -> i64,
    live: &dyn Liveness,
) -> Result<Provided> {
    let found = card(core, id)?.ok_or_else(|| bad(&format!("no card {id}")))?;
    if found.tool != TOOL {
        return Err(Error::NotAllowed(format!(
            "kith-core: cards.provide: card {id} is not a key card; {}",
            super::SECRET_HINT
        )));
    }
    let plan = plan_for(&spec_of(&found)?, account, values)?;
    let started = clock();
    let held = match begin(core, id, started, live)? {
        Ok(h) => h,
        Err(why) => return Ok(Provided::Refused(why)),
    };
    // A panic must not leave a claim that this live process would hold for good.
    let done = catch_unwind(AssertUnwindSafe(|| {
        commit(core, env, &held, &plan, values, clock(), live)
    }));
    let outcome = match done {
        Ok(r) => r,
        Err(panic) => {
            let _ = release(core, &held.id, &held.holder, live);
            resume_unwind(panic);
        }
    };
    match outcome {
        Ok(card) => Ok(Provided::Saved {
            card: Box::new(card),
            profile: plan.profile,
            vars: plan.records.into_iter().map(|(v, _)| v).collect(),
        }),
        Err(e) => {
            // Best effort: if this fails too the claim ends with this process, or its time, as the sweep reads it.
            let _ = release(core, &held.id, &held.holder, live);
            Err(e)
        }
    }
}

/// The params of a `cards.provide` row, read and shape-checked (no card, no key store yet). A refusal names the field,
/// never a value.
struct Asked {
    id: String,
    account: Option<String>,
    values: BTreeMap<String, Zeroizing<String>>,
}

impl Asked {
    fn read(params: &Value) -> Result<Self> {
        let id = text(params, "id")?.to_owned();
        let given = params
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("values is required"))?;
        let mut values = BTreeMap::new();
        for (k, v) in given {
            let s = v
                .as_str()
                .ok_or_else(|| bad("every field of the key is text"))?;
            values.insert(k.clone(), Zeroizing::new(s.to_owned()));
        }
        Ok(Self {
            id,
            account: params
                .get("account")
                .and_then(Value::as_str)
                .map(str::to_owned),
            values,
        })
    }

    /// Never echoes a value: refusals say which field, results name the service, account and variables.
    fn run(
        &self,
        core: &mut Core,
        env: &mut Envelope,
        clock: &dyn Fn() -> i64,
        live: &dyn Liveness,
    ) -> Result<Value> {
        let id = self.id.as_str();
        let account = self.account.as_deref();
        Ok(
            match provide(core, env, id, account, &self.values, clock, live)? {
                Provided::Saved {
                    card,
                    profile,
                    vars,
                } => json!({
                    "result": "saved", "id": card.id, "state": card.state.as_str(),
                    "service": profile.split('/').next(), "profile": profile, "vars": vars,
                }),
                Provided::Refused(Refused::Closed(Some(s))) => {
                    json!({"result": "closed", "id": id, "state": s.as_str()})
                }
                Provided::Refused(Refused::Closed(None)) => {
                    json!({"result": "closed", "id": id, "state": null})
                }
                Provided::Refused(Refused::Expired) => {
                    json!({"result": "expired", "id": id, "state": "expired"})
                }
                Provided::Refused(Refused::Claimed(by)) => {
                    json!({"result": "claimed", "id": id, "by": by})
                }
            },
        )
    }
}

/// The `cards.provide` row: `{id, account?, values}`. The params are checked before the key store is touched.
pub(super) fn provide_row(core: &mut Core, params: &Value, live: &dyn Liveness) -> Result<Value> {
    let asked = Asked::read(params)?;
    let mut env = Envelope::os()?;
    asked.run(core, &mut env, &now_ms, live)
}

/// The same over an envelope the caller made (tests: a fake key store).
#[cfg(test)]
pub(crate) fn provide_row_with(
    core: &mut Core,
    env: &mut Envelope,
    params: &Value,
    clock: &dyn Fn() -> i64,
    live: &dyn Liveness,
) -> Result<Value> {
    Asked::read(params)?.run(core, env, clock, live)
}

#[cfg(test)]
mod tests;
