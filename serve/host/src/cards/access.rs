//! Escape cards (F11 step 2b): the way out of a guest's sandbox is a request, not a denial
//! (designs/2026-10-24-f11-step2-build.md, "2b").
//!
//! A boxed run that hits the sandbox (an EPERM, a refused connection) calls kith-permit's `request_access(path|domain,
//! reason)` and ends its turn. The host resolves the real path, refuses what is never granted (secrets, rule W: no
//! card, whoever asks), and otherwise makes a Permission card for the approver. A yes cannot widen a process that is
//! already running, so it is applied at the **next activation** of the session:
//!
//! - **once**: the card is approved and parked with its session (C8). The session resumes through the doors of
//!   `resume.rs`; at launch the kernel calls `consumeAccess` (after it took the session's writer claim, before exec), which
//!   marks the grant used in one compare-and-set (L3) and returns it for the profile. One grant, one activation. An app
//!   chat's launch [reserves](Reservation) the grant instead: it is taken when the session started or loaded with it,
//!   and stays for the next activation when that failed.
//! - **always**: a kith-auth write (`kith-auth access`, under the auth.json lock, refused for secrets, rule W and for
//!   anyone but a member). `consumeAccess` then reads it back from kith-auth every time, so a revoke ends it (A7).
//!
//! `consumeAccess` is asked once per run (a run's helpers inherit its sandbox and get no second call), only by a guest
//! token, and only while kith-auth still lets the person use the agent. Everything it returns is checked again: a path
//! that no longer resolves to itself (a link moved), or that resolves into the secrets or rule W, is dropped; the kernel
//! checks once more when it builds the profile (`kernel/lib/kith_guard.py`).

use super::{Card, CardState, resource};
use crate::LockExt;
use crate::access::Ask;
use crate::hub::Hub;
use crate::runauth::{RunIdentity, TokenScope};
use crate::store::{Store, now_ms};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The tool name an escape card carries (the permission card kind the apps already draw).
pub const TOOL: &str = "request_access";

const MAX_REASON: usize = 500;

/// Runs that already took their grants.
static CONSUMED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The open [`Reservation`]s, by the run id of the launch they are for.
static RESERVED: LazyLock<Mutex<HashMap<String, Held>>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Held {
    /// Once grants the session was loaded with before and keeps while it stays loaded: taken already.
    carried: Vec<String>,
    /// Once grants this launch's `consumeAccess` handed out, taken at [`Reservation::commit`].
    reserved: Vec<String>,
}

/// The once grants of an app chat's activation (`agent/bot/hq.rs`), in the order access-arch rule 4 gives: the launch's
/// `consumeAccess` reserves them instead of taking them, the session starts or loads with the profile they are in,
/// and [`Reservation::commit`] takes them before the first turn (M4). Dropped without a commit (no session started or
/// loaded, the process didn't start) the grants stay for the next activation. A reserved grant is no other launch's meanwhile.
///
/// A session that stays loaded keeps the once grants it was loaded with: `carried` are handed out again to the launch
/// of each later turn, so its profile stays the same until something else changes it.
pub struct Reservation {
    run_id: String,
}

impl Reservation {
    /// The reservation for the launch whose run grant has `run_id`; `carried` are the once grants its session holds.
    pub fn open(run_id: &str, carried: Vec<String>) -> Self {
        RESERVED.locked().insert(run_id.to_owned(), Held { carried, reserved: Vec::new() });
        Self { run_id: run_id.to_owned() }
    }

    /// The session started with what the launch was given: its reserved grants are taken. Returns the once grants the
    /// session now holds (carried and reserved), to carry into the next turn's launch.
    pub fn commit(self, store: &Store) -> Vec<String> {
        let Some(held) = RESERVED.locked().remove(&self.run_id) else { return Vec::new() };
        if let Err(error) = store.card_access_commit(&held.reserved) {
            tracing::warn!(error = format!("{error:#}"), "couldn't take the reserved once grants");
        }
        let mut holds = held.carried;
        holds.extend(held.reserved);
        holds
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.locked().remove(&self.run_id);
    }
}

/// The once grants `who`'s launch is handed: reserved for it when its activation holds a [`Reservation`] (with the
/// ones its session carries), else taken now. Never one another launch has reserved.
fn once_grants(hub: &Hub, who: &RunIdentity) -> anyhow::Result<Vec<Card>> {
    let now = now_ms();
    let mut open = RESERVED.locked();
    let taken: HashSet<String> = open.values().flat_map(|h| h.reserved.iter().cloned()).collect();
    let Some(held) = open.get_mut(&who.run_id) else {
        return hub.store.card_access_consume(&who.session, now, &taken);
    };
    let mut cards: Vec<Card> = held
        .carried
        .iter()
        .filter_map(|id| hub.store.card(id))
        .filter(|c| c.session == who.session && c.tool == TOOL && c.state == CardState::Used)
        .collect();
    for card in hub.store.card_access_once(&who.session, now)? {
        if !taken.contains(&card.id) {
            held.reserved.push(card.id.clone());
            cards.push(card);
        }
    }
    Ok(cards)
}

fn refused(why: impl Into<String>) -> Value {
    json!({"ok": false, "error": why.into()})
}

/// What a run asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Path { real: PathBuf, write: bool },
    Domain(String),
}

impl Target {
    /// The card's resource key: one open card per session and key (A6).
    pub fn key(&self) -> String {
        match self {
            Self::Path { real, write } => {
                format!("access:{}:{}", if *write { "write" } else { "read" }, real.display())
            }
            Self::Domain(d) => format!("access:domain:{d}"),
        }
    }

    pub fn shown(&self) -> String {
        match self {
            Self::Path { real, write: false } => format!("read {}", real.display()),
            Self::Path { real, write: true } => format!("read and write {}", real.display()),
            Self::Domain(d) => format!("connect to {d}"),
        }
    }
}

/// A host name as kith-guard accepts it: lowercase labels, at least two, no wildcard, address or port.
pub fn valid_domain(d: &str) -> bool {
    let labels: Vec<&str> = d.split('.').collect();
    d.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && labels.last().and_then(|l| l.bytes().next()).is_some_and(|b| b.is_ascii_lowercase())
}

/// The places rule W adds to the home folder's list that are not folders of the sandbox: the kernel and the people files.
fn kernel_extras() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(k) = crate::kernel::current_kernel() {
        out.push(std::fs::canonicalize(&k).unwrap_or(k));
    }
    out.extend(resource::roots().kith.iter().map(|r| r.join("people")));
    out
}

/// Whether a path is never granted, and why.
pub fn never(path: &Path) -> Option<&'static str> {
    let roots = resource::roots();
    if resource::is_sensitive(path, &roots)
        || resource::holds_sensitive(path, &roots)
        || resource::touches_secret_dirs(path, &roots)
        || resource::holds_secrets_folder(path, &roots)
    {
        return Some(
            "That is a secret or a login, or holds one. It can't be approved; don't try another way to reach it.",
        );
    }
    let home = dirs::home_dir().unwrap_or_default();
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    if resource::is_rule_w(path, &home, &kernel_extras()) {
        return Some(
            "That is somewhere a guest run never writes or loads from (settings, hooks, shell startup files, program folders). It can't be approved.",
        );
    }
    None
}

/// Reads the request. `Err` is the reason it is refused, in words for the agent.
pub fn target_of(req: &Value) -> Result<Target, String> {
    let target = req["target"].as_str().unwrap_or_default();
    match req["kind"].as_str() {
        Some("path") => {
            let real = resource::resolve(target).ok_or("Give an absolute path (not a relative one).")?;
            if let Some(why) = never(&real) {
                return Err(why.into());
            }
            Ok(Target::Path { real, write: req["write"].as_bool() == Some(true) })
        }
        Some("domain") => {
            let d = target.trim().to_ascii_lowercase();
            if !valid_domain(&d) {
                return Err("Give a plain host name such as pypi.org (no wildcard, address, port or path).".into());
            }
            Ok(Target::Domain(d))
        }
        _ => Err("Ask for a path or a domain.".into()),
    }
}

/// `requestAccess` of the run socket: the reply line. Never blocks on the approver: the answer is applied at the next
/// activation, so the run is told to end its turn.
pub async fn request(hub: &Arc<Hub>, who: &RunIdentity, req: &Value) -> Value {
    if who.scope != TokenScope::Guest || who.person.is_empty() {
        return refused("Only a run inside the guest sandbox asks for access; yours isn't boxed.");
    }
    let reason: String =
        req["reason"].as_str().unwrap_or_default().chars().filter(|c| !c.is_control()).take(MAX_REASON).collect();
    if reason.trim().is_empty() {
        return refused("Say why you need it: a reason is needed.");
    }
    let target = match target_of(req) {
        Ok(t) => t,
        Err(why) => return refused(why),
    };
    if let Target::Path { real, .. } = &target
        && crate::api::roles::grant_roots(&who.person, &who.agent).iter().any(|r| real.starts_with(r))
    {
        return refused("That is already inside your own folders; try again from there.");
    }
    let cfg = super::settings(&hub.store);
    let now = now_ms();
    let key = target.key();
    if hub.store.card_access_waiting(&who.session, &key, now).is_some() {
        return json!({"ok": true, "carded": true, "message":
            "That is already approved. It applies the next time you run: end your turn now."});
    }
    let ask = crate::access::Request { person: &who.person, agent: &who.agent, owner_run: false, ask: Ask::Escape };
    let answerable_by = crate::access::approver(&hub.store, &ask).await;
    let new = crate::store::NewCard {
        session: who.session.clone(),
        source: "run",
        bot_id: None,
        run_id: Some(who.run_id.clone()),
        person: who.person.clone(),
        agent: who.agent.clone(),
        answerable_by,
        in_grants: false,
        tool: TOOL.to_owned(),
        input: json!({"kind": req["kind"], "target": target.shown(), "reason": reason, "write": req["write"]})
            .to_string(),
        resource: key,
        shown: format!("{}\n\nWhy: {reason}", target.shown()),
        deadline: now.saturating_add(cfg.expiry_ms),
        reply_to: None,
        asked_by: None,
    };
    let card = match hub.store.card_open(&new, now) {
        Ok(crate::store::Opened::New(c) | crate::store::Opened::Joined(c) | crate::store::Opened::Granted(c)) => c,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "couldn't store an access card");
            return refused("Approvals aren't available right now.");
        }
    };
    let Some(card) = super::show(hub, who, &card) else {
        return refused("There is nowhere to ask about this: no chat with this agent on this computer.");
    };
    let label = super::approver_label(hub, &card).await;
    json!({"ok": true, "carded": true, "message": format!(
        "Asked {label} to allow: {}. The answer applies the next time you run, not during this turn. Don't retry it or try \
         another way around. End your turn now and tell the user the request is waiting for approval.",
        target.shown()
    )})
}

/// What the approver sees on an escape card.
pub fn entry_view(card: &Card) -> (String, String, Value) {
    let input: Value = serde_json::from_str(&card.input).unwrap_or(Value::Null);
    let title = format!("Let {} {}", card.agent, input["target"].as_str().unwrap_or_default());
    let detail = format!(
        "{}\n\nOnce: the next time {} runs in this conversation. Always: every run of {} for this person, until you take it away.",
        card.shown, card.agent, card.agent
    );
    let options = json!([
        {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
        {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
        {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
    ]);
    (title, detail, options)
}

/// Splits a card's resource key into (mode, target).
fn parts(resource: &str) -> Option<(&str, &str)> {
    resource.strip_prefix("access:")?.split_once(':')
}

/// The answer to an escape card. A no changes nothing. A yes is owed a resume at the next activation; an "always" is
/// first written to kith-auth (so the resume finds it) and falls back to a once grant when kith-auth refuses. The
/// fallback is never silent: the card then reads "Allowed once only" and says why ([`kept_once`]).
pub fn answered(hub: &Arc<Hub>, card: &Card, allow: bool, option_id: Option<&str>) {
    if !allow {
        return;
    }
    // A yes that arrived while the asking process was still up was spent as a live one; nobody is waiting for it.
    if card.state == CardState::Used && hub.store.card_undelivered(&card.id).unwrap_or(false) {
        tracing::debug!(card = card.id, "access yes returned to its session");
    }
    if option_id != Some("always") {
        super::follow_up(hub, card);
        return;
    }
    let (hub, card) = (hub.clone(), card.clone());
    // Recorded first, so a launch that races this write can't take it as a once grant as well.
    if !hub.store.card_scope(&card.id, "always").unwrap_or(false) {
        kept_once(&hub, &card, "the always couldn't be recorded");
        super::follow_up(&hub, &card);
        return;
    }
    tokio::spawn(async move {
        let written = write_always(&card).await;
        match written {
            Ok(()) => {
                let _ = hub.store.card_always_written(&card.id);
            }
            Err(why) => {
                tracing::warn!(card = card.id, why, "kith-auth refused an always; it stays a once grant");
                let _ = hub.store.card_scope(&card.id, "once");
                kept_once(&hub, &card, &why);
            }
        }
        if let Some(fresh) = hub.store.card(&card.id) {
            super::follow_up(&hub, &fresh);
        }
    });
}

/// An "always" that stayed a once grant: the card says so in its outcome (`outcome`, which the apps show in place of the
/// option's own words), and `selected` is the once option, so an app that doesn't know `outcome` reads "Allowed once".
fn kept_once(hub: &Hub, card: &Card, why: &str) {
    let Some(id) = &card.entry_id else { return };
    let Some(mut e) = hub.store.entry(id) else { return };
    let why = why.trim().trim_start_matches("kith-auth:").trim();
    let why = if why.is_empty() { "kith-auth refused it" } else { why };
    e.data["selected"] = "allow".into();
    e.data["outcome"] = format!("Allowed once only: it can't be kept for always ({why})").into();
    hub.set_entry(id, &e.data);
}

/// `kith-auth access <agent> <person> read|write|domain <target>`.
async fn write_always(card: &Card) -> Result<(), String> {
    let (mode, target) = parts(&card.resource).ok_or("not an access card")?;
    let bin = crate::remote::account::bin().await.ok_or("kith-auth can't be reached")?;
    // A write grant covers reading too, so "read" cards write the narrower entry.
    let out = crate::channels::hq::output(&bin, &["access", &card.agent, &card.person, mode, target])
        .await
        .ok_or("kith-auth didn't answer")?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) }
}

/// What a launch gets: once grants of the session (consumed here, atomically) and the membership's always entries (read from
/// kith-auth now). Each is checked again; nothing is returned for a run the host doesn't know, a second ask of one run,
/// or a person who no longer may use the agent (A7).
pub async fn consume(hub: &Arc<Hub>, who: &RunIdentity) -> Value {
    if who.scope != TokenScope::Guest || who.person.is_empty() {
        return refused("not authorized");
    }
    if crate::api::roles::may_use_checked(&who.person, &who.agent).await != Some(true) {
        return refused("not authorized");
    }
    if !CONSUMED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(who.run_id.clone()) {
        return refused("this run already took its access");
    }
    let (mut read, mut write, mut domains) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<String>::new());
    let mut take = |mode: &str, target: &str| match mode {
        "domain" if valid_domain(target) => domains.push(target.to_owned()),
        "read" | "write" => {
            // By real path, again: a link that moved since the yes makes this a different place.
            let ok = resource::resolve(target).is_some_and(|r| r == Path::new(target) && never(&r).is_none());
            if ok {
                if mode == "write" { write.push(target.to_owned()) } else { read.push(target.to_owned()) }
            }
        }
        _ => {}
    };
    match once_grants(hub, who) {
        Ok(cards) => {
            for c in cards {
                if let Some((mode, target)) = parts(&c.resource) {
                    take(mode, target);
                }
            }
        }
        Err(error) => tracing::warn!(error = format!("{error:#}"), "couldn't take the once grants"),
    }
    let always = crate::api::roles::resolve(&who.person, &who.agent).await.map(|v| v["access"].clone());
    if let Some(a) = always {
        for (field, mode) in [("read", "read"), ("write", "write"), ("domains", "domain")] {
            for t in a[field].as_array().into_iter().flatten().filter_map(Value::as_str) {
                take(mode, t);
            }
        }
    }
    for list in [&mut read, &mut write, &mut domains] {
        list.sort();
        list.dedup();
    }
    json!({"ok": true, "grants": {"read": read, "write": write, "domains": domains}})
}
