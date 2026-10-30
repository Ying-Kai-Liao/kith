//! HTTP API: `POST /api/<method>` commands + `GET /events` SSE for this computer's own
//! apps and helpers (loopback + `Authorization: Bearer <token>`), and `GET /channel`, the
//! direct end-to-end encrypted channel phones use (see `channel`).

mod backups;
#[cfg(test)]
mod bot_guests_tests;
pub mod botguests;
#[cfg(test)]
mod botguests_props;
mod bots;
mod calls;
mod cards;
#[cfg(test)]
mod contract_tests;
pub mod devices;
#[cfg(test)]
mod error_tests;
mod events;
mod files;
mod folders;
mod host;
mod http;
mod kith_info;
mod marketplace;
mod peers;
#[cfg(test)]
mod peers_tests;
pub(crate) mod people;
#[cfg(test)]
mod people_tests;
#[cfg(test)]
mod proof_tests;
mod remote;
#[cfg(test)]
mod revoke_tests;
#[cfg(test)]
pub(crate) mod rig;
pub mod roles;
#[cfg(test)]
mod roles_props;
#[cfg(test)]
mod roles_tests;
#[cfg(test)]
mod room_points_props;
#[cfg(test)]
mod room_secrets_props;
mod threads;

pub use events::{events_stream, term_events};
pub use http::router;

use crate::api::devices::{Caller, Forbidden, NotAllowed, Principal};
use crate::hub::Hub;
use crate::market;
use anyhow::{Result, anyhow};
use axum::http::StatusCode;
use serde_json::Value;
use std::ops::ControlFlow;
use std::sync::Arc;

/// `POST /api/<method>` with a method this host doesn't know (404, not 400).
#[derive(Debug)]
struct UnknownMethod;

impl std::fmt::Display for UnknownMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unknown method")
    }
}

impl std::error::Error for UnknownMethod {}

/// HTTP status and message for a failed call; channels send the same pair in `err`.
pub fn error_status(e: &anyhow::Error) -> (StatusCode, String) {
    if e.is::<UnknownMethod>() {
        (StatusCode::NOT_FOUND, e.to_string())
    } else if e.is::<Forbidden>() || e.is::<NotAllowed>() {
        (StatusCode::FORBIDDEN, e.to_string())
    } else if e.is::<crate::remote::cloud::Conflict>()
        || e.is::<crate::terminal::HeldByWriter>()
        || e.is::<crate::terminal::TerminalBusy>()
    {
        (StatusCode::CONFLICT, e.to_string())
    } else if let Some(bot) = e.downcast_ref::<botguests::BotError>() {
        (StatusCode::from_u16(bot.status).unwrap_or(StatusCode::BAD_REQUEST), bot.message.clone())
    } else {
        // `{:#}` keeps the context chain ("starting `npx …`: No such file or directory").
        (StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

/// A stable machine word for a failed call from a bot guest (`err.code`, docs/kith-bot-guests.md 5.6); None for
/// everyone else's errors, and apps ignore it.
pub fn error_code(e: &anyhow::Error) -> Option<&'static str> {
    e.downcast_ref::<botguests::BotError>().map(|bot| bot.code.as_str())
}

fn str_arg<'a>(b: &'a Value, k: &str) -> Result<&'a str> {
    b[k].as_str().ok_or_else(|| anyhow!("`{k}` is required"))
}

/// The actor a built-in tool call came from (`actor`, set by `mcp.rs`), when it is one of `bot`'s; else its own chat's.
fn actor_arg<'a>(b: &'a Value, bot: &'a str) -> &'a str {
    b["actor"].as_str().filter(|a| crate::agent::bot::bot_of(a) == bot).unwrap_or(bot)
}

/// What a guest sees instead of an internal error: the real chain (paths, command lines, kith-auth's stderr) goes to
/// the host log under the same `id`.
#[derive(Debug)]
struct Hidden(String);

impl std::fmt::Display for Hidden {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Something went wrong on the host. (ref {})", self.0)
    }
}

impl std::error::Error for Hidden {}

/// A refusal written for the person who reads it (a call that is gone or already settled): guests see its text (400).
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UserError {}

/// Errors written for the person who sees them keep their text; everything else is internal.
fn user_facing(e: &anyhow::Error) -> bool {
    e.is::<UnknownMethod>()
        || e.is::<UserError>()
        || e.is::<Forbidden>()
        || e.is::<NotAllowed>()
        || e.is::<crate::remote::cloud::Conflict>()
        || e.is::<crate::terminal::HeldByWriter>()
        || e.is::<crate::terminal::TerminalBusy>()
        || e.is::<botguests::BotError>()
        || e.is::<crate::agent::hq::Refused>()
        || e.is::<crate::agent::backups::NoAccount>()
}

/// Owner calls keep the full error text; anyone else gets only what `user_facing` allows.
fn hide_internals(owner: bool, method: &str, r: Result<Value>) -> Result<Value> {
    match r {
        Err(e) if !owner && !user_facing(&e) => {
            let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
            tracing::warn!(%id, method, "call failed for a guest: {e:#}");
            Err(Hidden(id).into())
        }
        other => other,
    }
}

/// Runs one API method for `caller` (permissions per spec §6.6); a guest's internal errors are replaced (`Hidden`).
pub async fn dispatch(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<Value> {
    let owner = matches!(devices::principal(&hub.store, caller), Principal::Owner);
    hide_internals(owner, method, run_dispatch(hub, caller, method, b).await)
}

async fn run_dispatch(hub: &Arc<Hub>, caller: &Caller, method: &str, mut b: Value) -> Result<Value> {
    let permitted = devices::permit(caller, method);
    match devices::principal(&hub.store, caller) {
        Principal::Owner => {
            permitted?;
            // Never taken from a client: only `member_call` says who is a guest.
            if let Some(o) = b.as_object_mut() {
                o.remove("asMember");
            }
        }
        Principal::Nobody => return Err(Forbidden.into()),
        Principal::Member(person) => return member_call(hub, caller, &person, true, method, b, permitted).await,
        Principal::Unproven(person) => return member_call(hub, caller, &person, false, method, b, permitted).await,
        bot @ (Principal::Bot { .. } | Principal::BotPending(_)) => {
            return botguests::bot_call(hub, caller, bot, method, b, permitted).await;
        }
        Principal::Peer(computer) => return peers::peer_call(hub, caller, &computer, method, &b, permitted),
    }
    let observed = crate::analytics::observe_call(hub, caller, method, &b);
    let result = route(hub, caller, method, b).await;
    if result.is_ok()
        && let Some((event, properties)) = observed
    {
        crate::analytics::capture(hub, event, properties);
    }
    result
}

async fn route(hub: &Arc<Hub>, caller: &Caller, method: &str, b: Value) -> Result<Value> {
    crate::samples::refuse(hub, method, &b)?;
    if method.contains("onnector")
        || method.starts_with("composio")
        || method == "setComposioKey"
        || method.starts_with("credential")
        // Only signing in reads the vault; other computer tools work without a keyring.
        || (method == "computerCall" && b["name"] == "type_login")
        // The on-computer voice needs no key.
        || (method.starts_with("voice") && !method.starts_with("voiceLocal"))
        || method == "setVoiceKey"
        || matches!(method, "agentAuth" | "agentAuthenticate" | "setAgentEnv")
    {
        market::vault::unlock(hub.clone()).await?;
    }
    if method == "readFile" {
        return files::read(hub, &b).await;
    }
    // Each method group answers its own methods and hands the body back otherwise.
    let b = match host::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match folders::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match backups::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match kith_info::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match marketplace::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match calls::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match cards::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match bots::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match threads::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match people::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match botguests::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    let b = match peers::call(hub, method, b).await? {
        ControlFlow::Break(v) => return Ok(v),
        ControlFlow::Continue(b) => b,
    };
    match remote::call(hub, caller, method, b).await? {
        ControlFlow::Break(v) => Ok(v),
        ControlFlow::Continue(_) => Err(UnknownMethod.into()),
    }
}

/// A call from an invited person: the device check, then the allow-list and the chat checks, every one audited, then
/// the method itself with its answer shaped for them.
async fn member_call(
    hub: &Arc<Hub>,
    caller: &Caller,
    person: &str,
    proven: bool,
    method: &str,
    mut b: Value,
    permitted: Result<(), Forbidden>,
) -> Result<Value> {
    // Who is asking, set here so a client can't claim to be someone else (see `threads::resolve_citation`).
    if matches!(method, "resolveCitation" | "respondPermission") {
        b["asPerson"] = person.into();
    }
    if let Some(o) = b.as_object_mut() {
        o.remove("asMember");
    }
    if method == "upload" {
        b["asMember"] = person.into();
    }
    let verdict = match permitted {
        Ok(()) => roles::guard(hub, person, proven, method, &b).await,
        Err(e) => Err(e.into()),
    };
    let target = roles::target_of(&b);
    let outcome = match verdict {
        Ok(()) => route(hub, caller, method, b).await,
        Err(e) => Err(e),
    };
    // A refused call is logged as denied. The proof call is allowed to every unproven device but only counts when it checks out.
    let allowed = outcome.is_ok()
        || (method != "proveInvite" && !matches!(&outcome, Err(e) if e.is::<NotAllowed>() || e.is::<Forbidden>()));
    hub.store.audit_add(person, caller.device_key(), method, target.as_deref(), allowed);
    let mut shaped = roles::shape(hub, person, method, outcome?);
    if method == "hello" && shaped.is_object() {
        shaped["grantedAgents"] = roles::granted_agents(hub, person).await.into();
    }
    #[cfg(test)]
    answers_as_the_contract_says(if proven { "member" } else { "unproven" }, method, &shaped);
    Ok(shaped)
}

/// Under test, every answer an invited person or a bot gets is what the host RPC contract
/// (serve/contract/host/contract.json) says its method answers `who` with: the strict producer check, so a field or
/// word the contract doesn't name fails the test that sent it.
#[cfg(test)]
pub(super) fn answers_as_the_contract_says(who: &str, method: &str, answer: &Value) {
    use kith_wire::{check::Mode, rpc::SCHEMA};
    let m = SCHEMA.method(method).filter(|m| m.who.contains(&who));
    let m = m.unwrap_or_else(|| panic!("{method} answered a {who} but the host contract doesn't open it to one"));
    let problems = SCHEMA.check(m.result, answer, Mode::Producer);
    assert_eq!(problems, Vec::<String>::new(), "{method} answered a {who} outside the host contract:\n{answer:#}");
}
