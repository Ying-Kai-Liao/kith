//! Webhook and event deliveries (M4-4; the port of `serve/host/src/routines/hooks.rs` and `Trigger::matches`). The host
//! keeps only the listener: it hands the raw headers and body to `schedule.fire` and the core decides everything, in
//! this order:
//!
//! 1. **who**: the job must exist and have a `webhook` or `event` trigger, its key must be set in the secrets lane (the
//!    record [`key_name`]), and the delivery must carry that key (`Authorization: Bearer <key>`) or a GitHub-style
//!    `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(key, body)>`. Every failure here answers the same `unauthorized`,
//!    so job ids cannot be probed. Nothing is read from a keychain a second way: the key is a `secrets.*` record, and
//!    where none was written (the routines are not imported until M4-5) the delivery is refused, never fired;
//! 2. **what**: the delivery id, then the event (a GitHub `ping` only checks the connection and fires nothing);
//! 3. **whether**: a paused job refuses (`paused`, nothing written); an `event` trigger whose source, event and filters
//!    do not match answers `ignored` (nothing written);
//! 4. **the fire**: the same transaction as a tick or a run-now, with the slot `event:d<sha256 of the delivery id>`. A
//!    sender's retry of one delivery id therefore finds its slot decided and fires at most once (the answer is what was
//!    decided). A delivery with no id gets a slot of its own (`event:n<now>-<hash of the body>`): the sender gave no way
//!    to tell a retry from a second delivery. A busy job answers `busy` and writes nothing, so the retry fires once the
//!    job is free.
//!
//! The caller is the host, as itself (`Principal::Host`). `caller` is who the host says sent it (the real principal,
//! I17) and the answer repeats it. It is never used to decide anything (the proof above does), and M4-5 stores it with
//! the fire row so the real caller stays visible.
//!
//! The body is UTF-8 text: the listener refuses a body that is not as `invalid` and never converts it lossily (a
//! replaced byte would turn a correct signature into `unauthorized`).

use std::collections::BTreeMap;

use hmac::{Hmac, Mac as _};
use kith_kernel::paths::Paths;
use kith_kernel::schedule::Trigger;
use kith_kernel::secrets::Ctx;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::fire::{self, Fire, Outcome, READS, no_step, outcome_json};
use super::sync::{self, Read};
use super::{index, mac_id, trigger_of};
use crate::authority;
use crate::conversations::claims;
use crate::db::Core;
use crate::error::{Error, Result};

/// The largest body a delivery may have.
pub(super) const MAX_BODY: usize = 64 * 1024;
const MAX_DELIVERY_ID: usize = 200;

/// Request headers by lowercase name.
pub(super) type Headers = BTreeMap<String, String>;

/// Why a delivery did not fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refused {
    /// Unknown job, a job without a webhook, no key, or the wrong proof: told apart nowhere. Only before the proof has
    /// passed: after it, a failure is `Gone` or a retryable error, never a permanent auth failure for a valid key.
    Unauthorized,
    Paused,
    /// The job was removed after the proof passed. Not `Unauthorized`: the sender's key was good.
    Gone,
    Invalid(String),
}

impl Refused {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Paused => "paused",
            Self::Gone => "gone",
            Self::Invalid(_) => "invalid",
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Unauthorized => "invalid webhook key or signature".into(),
            Self::Paused => "routine is paused".into(),
            Self::Gone => "routine no longer exists".into(),
            Self::Invalid(why) => why.clone(),
        }
    }
}

/// The secret record that holds a job's webhook key.
pub(crate) fn key_name(agent: &str, job: &str) -> String {
    format!("routine-webhook/{agent}/{job}")
}

/// Equal bytes, in time that depends only on the lengths.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `Authorization: Bearer <key>`, or `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(key, body)>`.
pub(super) fn authorized(key: &str, headers: &Headers, body: &[u8]) -> bool {
    if key.is_empty() {
        return false;
    }
    if let Some(given) = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        && ct_eq(given.as_bytes(), key.as_bytes())
    {
        return true;
    }
    let Some(sig) = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.strip_prefix("sha256="))
        .and_then(hex)
    else {
        return false;
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac takes any key length");
    mac.update(body);
    // `verify_slice` compares in constant time.
    mac.verify_slice(&sig).is_ok()
}

/// The sender's id for this delivery, so a retry never runs twice.
pub(super) fn delivery_id(headers: &Headers) -> std::result::Result<Option<String>, Refused> {
    let id = ["x-delivery-id", "x-github-delivery", "idempotency-key"]
        .iter()
        .find_map(|h| headers.get(*h));
    match id {
        None => Ok(None),
        Some(id)
            if !id.is_empty()
                && id.len() <= MAX_DELIVERY_ID
                && id.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Ok(Some(id.clone()))
        }
        Some(_) => Err(Refused::Invalid("invalid delivery id".into())),
    }
}

/// What a job sees and its event trigger matches; `None` for a GitHub `ping`, which only checks the connection.
///
/// A JSON object is the event as-is; GitHub deliveries become `{source:"github", event: "<event>.<action>", repo, sender,
/// text, url, payload}`; anything else is `{text}`.
pub(super) fn event(headers: &Headers, body: &[u8]) -> std::result::Result<Option<Value>, Refused> {
    if body.len() > MAX_BODY {
        return Err(Refused::Invalid("event too large".into()));
    }
    let parsed: Option<Value> = if body.iter().all(u8::is_ascii_whitespace) {
        Some(json!({}))
    } else {
        serde_json::from_slice(body).ok()
    };
    if let Some(name) = headers.get("x-github-event") {
        if name == "ping" {
            return Ok(None);
        }
        let payload = parsed.ok_or_else(|| {
            Refused::Invalid("GitHub deliveries must use application/json".into())
        })?;
        return Ok(Some(github(name, &payload)));
    }
    Ok(Some(match parsed {
        Some(v @ Value::Object(_)) => v,
        Some(other) => json!({ "payload": other }),
        None => json!({ "text": String::from_utf8_lossy(body) }),
    }))
}

fn github(name: &str, payload: &Value) -> Value {
    let event = payload["action"]
        .as_str()
        .map_or_else(|| name.to_owned(), |action| format!("{name}.{action}"));
    let first = |paths: &[&[&str]]| {
        paths.iter().find_map(|path| {
            let v = path.iter().fold(payload, |v, k| &v[*k]);
            v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
        })
    };
    let text = first(&[
        &["pull_request", "title"],
        &["issue", "title"],
        &["discussion", "title"],
        &["release", "name"],
        &["release", "tag_name"],
        &["comment", "body"],
        &["review", "body"],
        &["head_commit", "message"],
        &["workflow_run", "name"],
    ]);
    let url = first(&[
        &["comment", "html_url"],
        &["review", "html_url"],
        &["pull_request", "html_url"],
        &["issue", "html_url"],
        &["discussion", "html_url"],
        &["release", "html_url"],
        &["workflow_run", "html_url"],
        &["compare"],
    ]);
    json!({
        "source": "github",
        "event": event,
        "repo": payload["repository"]["full_name"],
        "sender": payload["sender"]["login"],
        "text": text,
        "url": url,
        "payload": payload,
    })
}

/// Whether the job's trigger wants this event: a `webhook` takes any, an `event` takes the ones its source, event name
/// (`*` for any) and filters match. A filter `messageContains` looks for the text in the event's `text`; any other
/// filter is a top-level string field that must equal the value.
pub(super) fn wants(trigger: &Trigger, event: &Value) -> bool {
    match trigger {
        Trigger::Webhook => true,
        Trigger::Event {
            source,
            event: name,
            filters,
        } => {
            event["source"].as_str() == Some(source)
                && (name == "*" || event["event"].as_str() == Some(name))
                && filters.iter().all(|(key, value)| {
                    if key == "messageContains" {
                        event["text"]
                            .as_str()
                            .is_some_and(|text| text.contains(value.as_str()))
                    } else {
                        event[key].as_str() == Some(value)
                    }
                })
        }
        _ => false,
    }
}

/// The slot of a delivery (see the module doc).
pub(super) fn slot_of(delivery: Option<&str>, body: &[u8], now: i64) -> String {
    let short = |bytes: &[u8]| {
        use std::fmt::Write as _;
        Sha256::digest(bytes)
            .iter()
            .take(16)
            .fold(String::new(), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
    };
    match delivery {
        Some(id) => format!("event:d{}", short(id.as_bytes())),
        None => format!("event:n{now}-{}", short(body)),
    }
}

/// The job's webhook key, from the secrets lane. A job with no record (or an empty one) has no key.
fn key_of(core: &Core, agent: &str, job: &str) -> Result<Option<String>> {
    let name = key_name(agent, job);
    let has: Option<i64> = rusqlite::OptionalExtension::optional(core.conn().query_row(
        "SELECT 1 FROM secret_records WHERE name = ?1",
        [&name],
        |r| r.get(0),
    ))?;
    // No record needs no key store: the common refusal never touches the OS.
    if has.is_none() {
        return Ok(None);
    }
    let mut guard = core
        .secrets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(crate::secrets::Envelope::os()?);
    }
    let envelope = guard
        .as_mut()
        .ok_or_else(|| Error::Io("kith-core: the secrets envelope is unavailable".into()))?;
    Ok(envelope
        .get(core.conn(), &name)?
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .filter(|k| !k.is_empty()))
}

fn refused(r: &Refused, caller: &str) -> Value {
    json!({"outcome": "refused", "code": r.code(), "reason": r.reason(), "retry": false, "final": true, "caller": caller})
}

/// `schedule.fire`: the params are `agent`, `id` (the job), `headers` (name to value), `body` (the exact request body,
/// text) and `caller`.
pub(super) fn call(
    core: &mut Core,
    paths: &Paths,
    ctx: &Ctx,
    params: &Value,
    now: i64,
) -> Result<Value> {
    let word = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadParams(format!("kith-core: schedule: {k} is required")))
    };
    let mut headers = Headers::new();
    if let Some(h) = params.get("headers").and_then(Value::as_object) {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                headers.insert(k.to_ascii_lowercase(), v.to_owned());
            }
        }
    }
    deliver(
        core,
        paths,
        ctx,
        word("agent")?,
        word("id")?,
        &headers,
        word("body")?.as_bytes(),
        word("caller")?,
        now,
    )
}

/// One delivery, as above.
#[allow(clippy::too_many_arguments, reason = "one delivery is these things")]
pub(super) fn deliver(
    core: &mut Core,
    paths: &Paths,
    ctx: &Ctx,
    agent: &str,
    job: &str,
    headers: &Headers,
    body: &[u8],
    caller: &str,
    now: i64,
) -> Result<Value> {
    deliver_between(
        core,
        paths,
        ctx,
        agent,
        job,
        headers,
        body,
        caller,
        now,
        &|| {},
    )
}

/// [`deliver`], calling `between` after the pre-checks and before the fire reads the jobs file again: the window in
/// which the file can change under the delivery. Tests change it there.
#[allow(clippy::too_many_arguments, reason = "one delivery is these things")]
pub(super) fn deliver_between(
    core: &mut Core,
    paths: &Paths,
    ctx: &Ctx,
    agent: &str,
    job: &str,
    headers: &Headers,
    body: &[u8],
    caller: &str,
    now: i64,
    between: &dyn Fn(),
) -> Result<Value> {
    let no = |r: Refused| Ok(refused(&r, caller));
    // 1. Who. Whatever is wrong with the job is the same answer as a wrong key.
    let idx = index(paths, Some(agent));
    let Some(a) = idx
        .agents
        .first()
        .filter(|a| a.agent == agent && a.problem.is_none())
    else {
        return no(Refused::Unauthorized);
    };
    let Some(def) = a.jobs.iter().find(|j| sync::job_id(j) == job) else {
        return no(Refused::Unauthorized);
    };
    let Ok(trigger) = trigger_of(def) else {
        return no(Refused::Unauthorized);
    };
    if !matches!(trigger, Trigger::Webhook | Trigger::Event { .. }) {
        return no(Refused::Unauthorized);
    }
    let Some(key) = key_of(core, agent, job)? else {
        return no(Refused::Unauthorized);
    };
    if !authorized(&key, headers, body) {
        return no(Refused::Unauthorized);
    }
    // 2. What.
    let delivery = match delivery_id(headers) {
        Ok(d) => d,
        Err(r) => return no(r),
    };
    let event = match event(headers, body) {
        Ok(Some(e)) => e,
        Ok(None) => return Ok(json!({"outcome": "ping", "caller": caller})),
        Err(r) => return no(r),
    };
    // 3. Whether.
    if sync::is_paused(def) {
        return no(Refused::Paused);
    }
    if !wants(&trigger, &event) {
        return Ok(
            json!({"outcome": "ignored", "reason": "event does not match this routine", "caller": caller}),
        );
    }
    // 4. The fire.
    let slot = slot_of(delivery.as_deref(), body, now);
    let mac = mac_id(&paths.machine)?;
    between();
    for _ in 0..READS {
        // A file that changed between the read and the transaction is read again; the slot is the same, so this is one fire.
        let idx = index(paths, Some(agent));
        // The proof passed, so a failure here is not the sender's: the job went away, or the file is unreadable now.
        let Some(a) = idx.agents.first().filter(|a| a.agent == agent) else {
            return no(Refused::Gone);
        };
        if let Some(why) = &a.problem {
            return Err(Error::Refused(format!(
                "kith-core: schedule: {agent}'s jobs file cannot be read ({why}); deliver to {job:?} again"
            )));
        }
        let Some(def) = a.jobs.iter().find(|j| sync::job_id(j) == job) else {
            return no(Refused::Gone);
        };
        let auth = authority::tables_within(core.conn())?;
        let person = fire::person_of(
            def.get("person")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty()),
            &auth,
        );
        let revoke_seen = claims::revoke_epoch(core, &person)?;
        let read = Read {
            agent: &a.agent,
            hash: &a.hash,
            jobs: &a.jobs,
            paths: Some(paths),
        };
        let out = fire::fire_event(
            core,
            &Fire {
                read: &read,
                job,
                slot: Some(&slot),
                missed: None,
                now,
                mac: &mac,
                person: &person,
                revoke_seen,
                advance_to: None,
                ctx,
                after: &no_step,
            },
            &event,
        )?;
        if out == Outcome::Paused {
            // Paused after the pre-check: the same answer as the pre-check, and nothing was written.
            return no(Refused::Paused);
        }
        if out != Outcome::Stale {
            let mut v = outcome_json(agent, job, Some(&slot), &out);
            v["caller"] = json!(caller);
            return Ok(v);
        }
    }
    Err(Error::Refused(format!(
        "kith-core: schedule: {agent}'s jobs file kept changing; deliver to {job:?} again"
    )))
}
