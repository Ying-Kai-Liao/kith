//! An agent calling its person: a pending incoming call that every open app of that person shows full screen.
//!
//! The call lives in memory (a host restart drops it); only the per-agent ring history is kept on disk, so a restart
//! doesn't reset the daily limit. It reaches apps as a `call` event on the ordinary event stream, so the relay
//! carries it too, and `event_for` routes it by the chat's owner: the owner's devices for an owner chat, only that
//! person's devices for an invitee's. Accepting starts an ordinary Kith voice call in the app; declining or letting
//! it ring out leaves a line in the chat. There is no push: an app that isn't open misses the call.

use crate::LockExt;
use crate::agent::bot::Cmd;
use crate::hub::Hub;
use crate::store::{EntryKind, Lane, now_ms};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a call rings before it counts as missed.
pub const RING_MS: i64 = 45_000;
/// One call per chat per this long, and at most `PER_DAY` in any 24 hours.
const MIN_GAP_MS: i64 = 10 * 60 * 1000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const PER_DAY: usize = 6;
/// An accepted call counts as active (so another agent's call is busy) until the app logs its end, or this long.
const ACTIVE_MS: i64 = 30 * 60 * 1000;
/// An accepted call counts as dropped when the app hasn't started its voice call (`callGreet`) within this long.
const GREET_MS: i64 = 60_000;
/// Longest reason kept, in characters.
const MAX_REASON: usize = 200;
/// How long `wait` holds a `callPerson` request open.
const WAIT: Duration = Duration::from_secs(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ringing,
    Accepted,
    Declined,
    Missed,
    /// An accepted call whose app logged its end.
    Ended,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ringing => "ringing",
            Self::Accepted | Self::Ended => "accepted",
            Self::Declined => "declined",
            Self::Missed => "missed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Call {
    pub id: String,
    pub bot_id: String,
    /// Whose call it is: `None` is the owner, else the invited person.
    pub person: Option<String>,
    /// The agent that rang when it isn't the chat's own (`coder` working for `architect`): shown, never counted.
    pub from: Option<String>,
    pub reason: String,
    pub created: i64,
    pub state: State,
    settled: i64,
    greeted: bool,
}

impl Call {
    fn live(&self, now: i64) -> bool {
        // An accepted call whose app never started the voice call (`greet`) doesn't hold the person for long.
        let limit = if self.greeted { ACTIVE_MS } else { GREET_MS };
        self.state == State::Ringing || (self.state == State::Accepted && now - self.settled < limit)
    }
}

#[derive(Default)]
struct Inner {
    calls: HashMap<String, Call>,
    /// Chat and person -> when its calls rang in the last day, oldest first.
    rung: HashMap<String, Vec<i64>>,
}

pub struct Calls {
    ring_ms: AtomicI64,
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Calls {
    pub fn open(path: Option<PathBuf>, ring_ms: i64) -> Self {
        let rung = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { ring_ms: AtomicI64::new(ring_ms), path, inner: Mutex::new(Inner { calls: HashMap::new(), rung }) }
    }

    /// The host's own: the ring history lives in its data folder (not in unit tests, whose hubs share one).
    pub fn for_host() -> Self {
        let path = (!cfg!(test)).then(|| crate::service::data_dir().join("calls.json"));
        Self::open(path, RING_MS)
    }

    fn ring_ms(&self) -> i64 {
        self.ring_ms.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn set_ring_ms(&self, ms: i64) {
        self.ring_ms.store(ms, Ordering::Relaxed);
    }

    fn save(&self, rung: &HashMap<String, Vec<i64>>) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("tmp");
        let written = serde_json::to_vec(rung)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(error) = written {
            tracing::warn!(%error, "couldn't save the call history");
        }
    }

    /// The calls still ringing, as events, for an app that just connected.
    pub fn ringing(&self, hub: &Hub) -> Vec<Value> {
        let g = self.inner.locked();
        g.calls.values().filter(|c| c.state == State::Ringing).map(|c| wire(hub, c)).collect()
    }

    fn get(&self, id: &str) -> Option<Call> {
        self.inner.locked().calls.get(id).cloned()
    }

    /// The app logged the end of its call with `bot`: it is no longer active.
    pub fn ended(&self, bot: &str) {
        for c in self.inner.locked().calls.values_mut() {
            if c.bot_id == bot && c.state == State::Accepted {
                c.state = State::Ended;
            }
        }
    }
}

/// The event for a call. `person` stays on the host: events are routed by the chat's owner.
fn wire(hub: &Hub, call: &Call) -> Value {
    let name = shown(hub, call);
    json!({
        "type": "call",
        "rev": hub.store.current_rev(),
        "call": {
            "id": call.id, "botId": call.bot_id, "name": name, "reason": call.reason,
            "createdAt": call.created, "remainingMs": (call.created + hub.calls.ring_ms() - now_ms()).max(0),
            "state": call.state.as_str(),
        },
    })
}

/// Who the screen says is calling: the chat's agent, or `coder (for architect)` when another agent rang in its name.
fn shown(hub: &Hub, call: &Call) -> String {
    let name = hub.store.bot(&call.bot_id).ok().flatten().map(|r| r.config.name).unwrap_or_default();
    match &call.from {
        Some(from) => format!("{from} (for {name})"),
        None => name,
    }
}

/// The `from` a run named for itself: one short word, or nothing. It is a label only; it decides no one's access.
fn clean_from(from: Option<&str>) -> Option<String> {
    let from: String = from?.chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).take(40).collect();
    (!from.is_empty()).then_some(from)
}

fn emit(hub: &Hub, call: &Call) {
    let _ = hub.events.send(wire(hub, call));
}

/// One line, cut to length.
fn clean(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_REASON).collect()
}

/// What the rate limit counts: the calls of one chat to its person. Never the `from` a run names, which it picks.
fn rate_key(chat: &str, person: Option<&str>) -> String {
    format!("{chat}\u{1f}{}", person.unwrap_or_default())
}

/// The proof a run has of serving `person` (`None` is the owner) in chat `bot`: the host puts it in that run's
/// environment (`KITH_CALL_KEY`), and `kith-call` presents it. It covers both, so a run can ring no other chat and
/// no other person, whatever else its environment says.
pub fn key(host_token: &str, bot: &str, person: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("kith-call:{host_token}:{bot}:{}", person.unwrap_or_default()));
    digest.iter().fold(String::new(), |s, b| s + &format!("{b:02x}"))
}

/// Whether `given` is the key issued for chat `bot` and the person it belongs to.
pub fn key_ok(hub: &Hub, bot: &str, given: Option<&str>) -> bool {
    let Some(row) = hub.store.bot(bot).ok().flatten().filter(|r| !r.deleted) else { return false };
    given == Some(key(&hub.token, bot, row.config.person.as_deref()).as_str())
}

/// What a run serving chat `bot` gets to ring with: `KITH_CALL_CHAT` and `KITH_CALL_KEY`.
pub fn env_for(hub: &Hub, bot: &str) -> Vec<(String, String)> {
    let person = hub.store.bot(bot).ok().flatten().and_then(|r| r.config.person);
    vec![("KITH_CALL_CHAT".into(), bot.to_owned()), ("KITH_CALL_KEY".into(), key(&hub.token, bot, person.as_deref()))]
}

/// The same for a run that doesn't come from an app chat (a channel's): the agent's main chat with the person it
/// serves (`None` is the owner). Nothing when that person has no app chat with the agent, so it can't ring.
pub fn run_env(hub: &Hub, agent: &str, person: Option<&str>) -> Vec<(String, String)> {
    chat_of(hub, agent, person).map(|c| env_for(hub, &c)).unwrap_or_default()
}

/// What a run starting for `agent` and `person` (`None` is the owner) is given as its origin: the call env of their
/// main app chat and, with `outbox`, that chat's run outbox (`KITH_RUN_OUTBOX`). Both name the same chat and person,
/// made here, so nothing the run later sets in its environment can move it. Nothing when the person has no app chat
/// with the agent (`chat` is null): the caller then knows there is no origin to stamp.
pub fn run_origin(hub: &Hub, agent: &str, person: Option<&str>, outbox: bool) -> Value {
    let Some(chat) = chat_of(hub, agent, person) else { return json!({"chat": null, "env": {}}) };
    let mut env: serde_json::Map<String, Value> = env_for(hub, &chat).into_iter().map(|(k, v)| (k, v.into())).collect();
    if outbox {
        match crate::run_outbox::for_chat(agent, person) {
            Ok(dir) => {
                env.insert("KITH_RUN_OUTBOX".into(), dir.to_string_lossy().into_owned().into());
            }
            Err(error) => tracing::warn!(agent, error = format!("{error:#}"), "no outbox for a run's origin"),
        }
    }
    json!({"chat": chat, "env": env})
}

/// The agent's main chat with `person` (`None` is the owner): never a group.
pub fn chat_of(hub: &Hub, agent: &str, person: Option<&str>) -> Option<String> {
    hub.store.bots().ok().and_then(|rows| {
        rows.into_iter()
            .find(|r| {
                !r.deleted
                    && !r.config.is_group()
                    && r.config.hq_agent.as_deref() == Some(agent)
                    && r.config.person.as_deref() == person
            })
            .map(|r| r.config.id)
    })
}

/// An agent rings its person. `bot_id` is the chat the agent works in, which also says who is called. The answer's
/// `status` is `ringing`, `busy`, `rate-limited` or `not-allowed`. `from` is the agent a run says it is (`kith-call`
/// sends `KITH_AGENT`): a label for the call screen only.
fn ring(hub: &Arc<Hub>, bot_id: &str, reason: &str, from: Option<&str>) -> Result<Value> {
    let reason = clean(reason);
    if reason.is_empty() {
        bail!("reason is required: say in a few words why you're calling");
    }
    let Some(row) = hub.store.bot(bot_id)?.filter(|r| !r.deleted && !r.config.is_group()) else {
        return Ok(json!({"status": "not-allowed"}));
    };
    let person = row.config.person.clone();
    let from = clean_from(from).filter(|f| row.config.hq_agent.as_deref() != Some(f));
    let now = now_ms();
    let call = {
        let mut g = hub.calls.inner.locked();
        g.calls.retain(|_, c| c.live(now) || now - c.settled < 60_000);
        // A person is on one call at a time, whoever it is with.
        if g.calls.values().any(|c| c.person == person && c.live(now)) {
            return Ok(json!({"status": "busy"}));
        }
        let rung = g.rung.entry(rate_key(bot_id, person.as_deref())).or_default();
        rung.retain(|t| now - t < DAY_MS);
        let gap = rung.last().map(|t| MIN_GAP_MS - (now - t)).filter(|w| *w > 0);
        if gap.is_some() || rung.len() >= PER_DAY {
            let retry = gap.unwrap_or_else(|| DAY_MS - (now - rung[rung.len() - PER_DAY]));
            return Ok(json!({"status": "rate-limited", "retryAfterSeconds": retry / 1000 + 1}));
        }
        rung.push(now);
        let call = Call {
            id: uuid::Uuid::new_v4().to_string(),
            bot_id: bot_id.to_owned(),
            person,
            from,
            reason,
            created: now,
            state: State::Ringing,
            settled: now,
            greeted: false,
        };
        g.calls.insert(call.id.clone(), call.clone());
        // Saved under the lock, so two rings can't write the file out of order.
        hub.calls.save(&g.rung);
        call
    };
    emit(hub, &call);
    let (hub, id) = (hub.clone(), call.id.clone());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(u64::try_from(hub.calls.ring_ms()).unwrap_or(0))).await;
        let _ = settle(&hub, &id, State::Missed, None);
    });
    Ok(json!({"status": "ringing", "id": call.id}))
}

/// Who may settle a call: its person (`None` inside is the owner), or anyone (the host's own timer).
type Who<'a> = Option<Option<&'a str>>;

/// Ringing -> `to`, once. The first of accept, decline and timeout wins; the others find it settled.
fn settle(hub: &Arc<Hub>, id: &str, to: State, who: Who) -> Result<Call> {
    let call = {
        let mut g = hub.calls.inner.locked();
        let Some(c) = g.calls.get_mut(id).filter(|c| who.is_none_or(|w| w == c.person.as_deref())) else {
            return Err(crate::api::UserError("That call is gone.".into()).into());
        };
        if c.state != State::Ringing {
            return Err(crate::api::UserError(format!("That call is already {}.", c.state.as_str())).into());
        }
        c.state = to;
        c.settled = now_ms();
        c.clone()
    };
    emit(hub, &call);
    if matches!(to, State::Declined | State::Missed) {
        let name = shown(hub, &call);
        // `missedCall` marks it for later styling; apps draw it as the notice it is.
        let data = json!({
            "text": format!("{name} tried to call: {}", call.reason),
            "missedCall": true, "callState": to.as_str(), "callReason": call.reason,
        });
        // Always the main chat: the voice call talks there, so the call's lines and the greeting go with it.
        let lane = Lane::main(&call.bot_id);
        hub.add_entry(&lane, EntryKind::Notice, hub.store.max_turn(&call.bot_id), &data);
    }
    Ok(call)
}

pub fn accept(hub: &Arc<Hub>, who: Option<&str>, id: &str) -> Result<Value> {
    settle(hub, id, State::Accepted, Some(who)).map(|c| json!({"botId": c.bot_id}))
}

pub fn decline(hub: &Arc<Hub>, who: Option<&str>, id: &str) -> Result<Value> {
    settle(hub, id, State::Declined, Some(who)).map(|_| json!({}))
}

/// The accepting app's call is live: the agent opens it, on a turn that is not the person's words (a notice anchors
/// it in the chat as a small line).
pub fn greet(hub: &Arc<Hub>, who: Option<&str>, id: &str) -> Result<Value> {
    let call = {
        let mut g = hub.calls.inner.locked();
        let Some(c) = g.calls.get_mut(id).filter(|c| who == c.person.as_deref()) else {
            return Err(crate::api::UserError("That call is gone.".into()).into());
        };
        if c.state != State::Accepted || c.greeted {
            return Err(crate::api::UserError("That call isn't waiting for the agent to speak.".into()).into());
        }
        c.greeted = true;
        c.clone()
    };
    let row = hub.store.bot(&call.bot_id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
    // Always the main chat: the voice call talks there, so the call's lines and the greeting go with it.
    let lane = Lane::main(&call.bot_id);
    let data = json!({"text": format!("{} called: {}", row.config.name, call.reason), "callState": "accepted", "callReason": call.reason, "via": "call", "greeting": true});
    let turn = hub.store.max_turn(&call.bot_id);
    let Some(notice) = hub.add_entry(&lane, EntryKind::Notice, turn, &data) else { bail!("couldn't save the call") };
    let ask = format!("[The person accepted your call. Greet them and say why you called: {}]", call.reason);
    let prompt = crate::chat::prompt::of_message(&hub.store, &row.config, &ask, &[], true);
    hub.send_cmd(&call.bot_id, Cmd::Send { lane, entry_id: notice.id, text: prompt, route: false })?;
    Ok(json!({}))
}

/// `callPerson` and the `call_person` tool, after the caller has said which chat it is. With `wait` the answer holds
/// until the person answered or the ring ended: `status` then is `accepted`, `declined` or `missed`.
pub async fn ring_and_wait(
    hub: &Arc<Hub>,
    bot_id: &str,
    reason: &str,
    from: Option<&str>,
    wait: bool,
) -> Result<Value> {
    let mut out = ring(hub, bot_id, reason, from)?;
    let Some(id) = out["id"].as_str().filter(|_| wait).map(str::to_owned) else { return Ok(out) };
    let deadline = tokio::time::Instant::now() + WAIT;
    while tokio::time::Instant::now() < deadline {
        match hub.calls.get(&id).map(|c| c.state) {
            Some(State::Ringing) => tokio::time::sleep(Duration::from_millis(250)).await,
            Some(state) => {
                out["status"] = state.as_str().into();
                break;
            }
            None => break,
        }
    }
    Ok(out)
}

/// The `call_person` tool of an agent's `kith-files` server.
pub fn tool() -> Value {
    json!({
        "name": "call_person",
        "description": "Ring the person you work for, as a phone call in their open Kith app. Use it only when something needs their voice now; it can be used once per 10 minutes and 6 times a day. The answer's status is ringing (the call is on their screen; if they decline or miss it, your reason shows in the chat), busy, rate-limited or not-allowed. Don't wait for it or call again.",
        "inputSchema": {
            "type": "object",
            "properties": {"reason": {"type": "string", "description": "Why you are calling, in a few words. The person reads it on the call screen."}},
            "required": ["reason"],
        },
        "annotations": {"readOnlyHint": false, "idempotentHint": false},
    })
}

#[cfg(test)]
mod tests;
