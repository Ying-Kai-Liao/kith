//! Event streams shared by SSE and channels: catch-up then live events, and setup terminals.

use super::devices::{Caller, Principal, principal};
use super::roles;
use crate::LockExt;
use crate::hub::Hub;
use anyhow::Result;
use futures::future::Either;
use futures::{Stream, StreamExt, stream};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio_stream::wrappers::BroadcastStream;

/// Entries a fresh client receives per bot before paging with `history`.
const CATCH_UP_PER_BOT: i64 = 200;

/// A setup terminal's output: everything so far, then live, ending after `exit`.
pub fn term_events(hub: &Hub, id: &str) -> Option<impl Stream<Item = Value> + Send + use<>> {
    let term = hub.terms.get(id)?;
    let (head, rx) = term.attach();
    let done = head.iter().any(|v| v["type"] == "exit");
    let live = BroadcastStream::new(rx).filter_map(|msg| async move { msg.ok() }).scan(done, |done, v| {
        let out = (!*done).then(|| {
            *done = v["type"] == "exit";
            v
        });
        async move { out }
    });
    Some(stream::iter(head).chain(live))
}

/// Counts a connected phone (pushes are held while one is connected) for as long as its stream lives.
struct PhoneClientGuard(Arc<Hub>);

impl PhoneClientGuard {
    fn new(hub: Arc<Hub>) -> Self {
        hub.phone_clients.fetch_add(1, Ordering::Relaxed);
        Self(hub)
    }
}

impl Drop for PhoneClientGuard {
    fn drop(&mut self) {
        self.0.phone_clients.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A bot guest's stream: the entries of its own conversations (shaped to what a bot may see) after `since`, then live
/// ones. Nothing else, so a bot learns nothing of the computer's other chats.
fn bot_events(hub: &Arc<Hub>, since: i64, bot_id: &str) -> impl Stream<Item = Value> + Send + use<> {
    let live = BroadcastStream::new(hub.events.subscribe());
    let mut catch_up: Vec<Value> = vec![];
    for e in hub.store.entries_since(since, CATCH_UP_PER_BOT).unwrap_or_default() {
        catch_up.push(json!({"type": "entry", "rev": e.rev, "entry": e}));
    }
    let mut catch_up: Vec<Value> =
        catch_up.into_iter().filter_map(|v| roles::event_for_bot(&hub.store, bot_id, v)).collect();
    catch_up.sort_by_key(|v| v["rev"].as_i64().unwrap_or(0));
    catch_up.insert(
        0,
        roles::hello_for(&json!({
            "hostId": hub.host_id,
            "computerId": hub.identity.computer_id(),
            "rev": hub.store.current_rev(),
        })),
    );
    let (hub, bot) = (hub.clone(), bot_id.to_owned());
    let tail = live.filter_map(move |msg| {
        let (hub, bot) = (hub.clone(), bot.clone());
        async move {
            match msg {
                Ok(v) => roles::event_for_bot(&hub.store, &bot, v),
                Err(_) => Some(json!({"type": "resync"})),
            }
        }
    });
    stream::iter(catch_up).chain(tail)
}

/// Event types only this computer's own apps receive.
const LOCAL_EVENTS: &[&str] = &["accessRequests", "cloud"];

#[cfg(test)]
mod tests;

/// Subscribes first, then yields a catch-up (everything after `since`, in rev order),
/// then live events. Duplicates are fine: clients upsert by id/rev. Shared by SSE and channels.
pub fn events_stream(
    hub: &Arc<Hub>,
    since: i64,
    client: Option<&str>,
    caller: &Caller,
) -> Result<impl Stream<Item = Value> + Send + use<>> {
    let live = BroadcastStream::new(hub.events.subscribe());
    // None is the owner, who never sees an invitee's chats; an invited person sees only their own.
    let who: Option<String> = match principal(&hub.store, caller) {
        Principal::Owner => None,
        Principal::Member(p) => Some(p),
        Principal::Bot { bot_id, .. } => return Ok(Either::Left(bot_events(hub, since, &bot_id))),
        Principal::Nobody | Principal::Unproven(_) | Principal::BotPending(_) => {
            anyhow::bail!("this device isn't allowed on this computer")
        }
    };
    let mut catch_up: Vec<Value> = vec![];
    for bot in hub.bots_json(since)? {
        catch_up.push(json!({"type": "bot", "rev": bot["rev"], "bot": bot}));
    }
    for e in hub.store.entries_since(since, CATCH_UP_PER_BOT)? {
        catch_up.push(json!({"type": "entry", "rev": e.rev, "entry": e}));
    }
    // A call still ringing, for an app that connects (or reconnects) while it does.
    catch_up.extend(hub.calls.ringing(hub));
    let mut catch_up: Vec<Value> =
        catch_up.into_iter().filter_map(|v| roles::event_for(&hub.store, who.as_deref(), v)).collect();
    catch_up.sort_by_key(|v| v["rev"].as_i64().unwrap_or(0));
    let hello = json!({
        "type": "hello",
        "hostId": hub.host_id,
        "computerId": hub.identity.computer_id(),
        "rev": hub.store.current_rev(),
        "usage": hub.usage.locked().clone(),
        "screen": hub.screen.state(),
    });
    catch_up.insert(0, if who.is_some() { roles::hello_for(&hello) } else { hello });

    let local = matches!(caller, Caller::Local);
    let guard = Arc::new(matches!(client, Some("ios" | "android")).then(|| PhoneClientGuard::new(hub.clone())));
    let hub = hub.clone();
    let tail = live.filter_map(move |msg| {
        let _keep = guard.clone();
        let (hub, who) = (hub.clone(), who.clone());
        async move {
            match msg {
                // The owner's other devices get members' requests, the ones they may answer (see `decideAccessRequest`).
                Ok(v) if !local && who.is_none() && v["type"] == "accessRequests" => Some(json!({
                    "type": "accessRequests",
                    "requests": v["requests"].as_array().into_iter().flatten()
                        .filter(|r| !r["personId"].is_null()).cloned().collect::<Vec<_>>(),
                })),
                Ok(v) if !local && v["type"].as_str().is_some_and(|t| LOCAL_EVENTS.contains(&t)) => None,
                Ok(v) => roles::event_for(&hub.store, who.as_deref(), v),
                // Lagged: tell the client to resync from its last rev.
                Err(_) => Some(json!({"type": "resync"})),
            }
        }
    });
    Ok(Either::Right(stream::iter(catch_up).chain(tail)))
}
