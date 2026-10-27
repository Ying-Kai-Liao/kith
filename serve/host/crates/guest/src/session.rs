//! One bot's live session with its computer: connect (failing fast or waiting), calls that survive a dropped socket,
//! the events subscription, and the join/status bookkeeping every command shares.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use kith_wire::crypto::{computer_id, random, unb64_n};
use kith_wire::rpc::{self, BotProof, BotSelf, ProveBotInviteParams, method};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use crate::config::{AgentRef, Guest, State, Store};
use crate::exit::{CliError, Exit};
use crate::output::Out;
use crate::relay::{Channel, Event, Fail, Target, connect};

pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// One request, from sending to its answer.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a command that already had a working connection keeps trying to get it back.
const RECONNECT_BUDGET: Duration = Duration::from_secs(60);

/// Knobs that tests turn down.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Time to open one socket and finish its handshake, and the whole budget of a fail-fast connect.
    pub dial_timeout: Duration,
    /// First retry delay; it doubles up to 15x.
    pub backoff: Duration,
    /// Between `botStatus` polls while the owner has not answered.
    pub poll: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { dial_timeout: Duration::from_secs(20), backoff: Duration::from_secs(1), poll: Duration::from_secs(5) }
    }
}

/// Jittered 1, 2, 4, 8, 15 s (scaled by `base`).
pub struct Backoff {
    base: Duration,
    n: usize,
}

impl Backoff {
    pub fn new(base: Duration) -> Self {
        Self { base, n: 0 }
    }

    pub fn delay(&mut self) -> Duration {
        const STEPS: [u32; 5] = [1, 2, 4, 8, 15];
        let step = STEPS[self.n.min(STEPS.len() - 1)];
        self.n += 1;
        // +-20 %, so bots restarted together don't knock together.
        let jitter = 0.8 + 0.4 * f64::from(random::<1>()[0]) / 255.0;
        self.base.mul_f64(f64::from(step) * jitter)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Patience {
    /// Offline or not-yet-admitted is an answer, given right away.
    FailFast,
    /// Keep trying until then.
    Until(Instant),
}

/// An answer of the host's, read as the contract's type. One this version can't read is a version mismatch.
pub fn answer<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, CliError> {
    serde_json::from_value(v).map_err(|_| CliError::version())
}

/// The contract's params for a method, as the JSON a call sends.
pub fn params<T: serde::Serialize>(p: &T) -> Value {
    serde_json::to_value(p).unwrap_or_default()
}

/// `1.2.3` < `1.10.0`; anything unparsable counts as 0.
pub fn version_lt(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Vec<u64> {
        s.split('.').map(|p| p.trim_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0)).collect()
    };
    parts(a) < parts(b)
}

pub enum Ev {
    Event(Value),
    /// The subscription was re-opened after a break: read the thread to catch up.
    Resync,
}

pub struct Client<'a> {
    out: &'a Out,
    pub store: Store,
    pub key: SigningKey,
    pub guest: Guest,
    target: Target,
    chan: Option<Channel>,
    connected_before: bool,
    sub: Option<u64>,
    sub_since: i64,
    pub tuning: Tuning,
}

impl<'a> Client<'a> {
    pub fn new(
        store: Store,
        key: SigningKey,
        guest: Guest,
        cloud: Option<&str>,
        out: &'a Out,
        tuning: Tuning,
    ) -> Result<Self, CliError> {
        let host_sign_key: [u8; 32] = unb64_n(&guest.host_sign_key).map_err(|_| CliError::identity())?;
        // The pinned key must still hash to the computer id the config names.
        if computer_id(&host_sign_key) != guest.computer_id {
            return Err(CliError::identity());
        }
        let target = Target {
            cloud: cloud.map_or_else(|| guest.cloud.clone(), str::to_string),
            computer_id: guest.computer_id.clone(),
            host_sign_key,
        };
        Ok(Self {
            out,
            store,
            key,
            guest,
            target,
            chan: None,
            connected_before: false,
            sub: None,
            sub_since: 0,
            tuning,
        })
    }

    /// The bot from the files or environment; `NotJoined` when there is none.
    pub fn from_store(store: Store, cloud: Option<&str>, out: &'a Out, tuning: Tuning) -> Result<Self, CliError> {
        let (key, guest) = store.load()?.ok_or_else(CliError::not_joined)?;
        Self::new(store, key, guest, cloud, out, tuning)
    }

    pub fn save(&self) -> Result<(), CliError> {
        self.store.save(None, &self.guest)
    }

    fn set_state(&mut self, state: State) {
        if self.guest.state != state {
            self.guest.state = state;
            // Not being able to remember a state is not worth failing a command that already has its answer.
            let _ = self.save();
        }
    }

    // ---- connecting ----

    pub async fn ensure(&mut self, patience: Patience) -> Result<(), CliError> {
        if self.chan.is_some() {
            return Ok(());
        }
        let give_up = match patience {
            Patience::FailFast => Instant::now() + self.tuning.dial_timeout,
            Patience::Until(t) => t,
        };
        let mut backoff = Backoff::new(self.tuning.backoff);
        loop {
            let fail = match connect(&self.target, &self.key, self.tuning.dial_timeout).await {
                Ok(ch) => {
                    self.chan = Some(ch);
                    self.connected_before = true;
                    return Ok(());
                }
                Err(f) => f,
            };
            let joining = self.guest.state == State::Joining;
            match &fail {
                Fail::Final(_) | Fail::Host(_) => return Err(fail.into_cli()),
                // Blocked or removed: nothing to wait for. (Before the computer has admitted the bot, the same 403 just means "not yet".)
                Fail::Refused if !joining => {
                    self.set_state(State::Revoked);
                    return Err(CliError::removed());
                }
                Fail::Offline | Fail::Refused if matches!(patience, Patience::FailFast) => {
                    return Err(Self::not_there(&fail));
                }
                _ => {}
            }
            let wait = backoff.delay();
            if Instant::now() + wait > give_up {
                return Err(Self::not_there(&fail));
            }
            sleep(wait).await;
        }
    }

    /// The computer can't be reached right now: offline, or it hasn't admitted a bot that joined at the cloud yet.
    fn not_there(fail: &Fail) -> CliError {
        match fail {
            Fail::Refused => CliError::new(
                Exit::Offline,
                "hostOffline",
                "Joined at the cloud, but the computer hasn't admitted this bot yet. It is probably offline. Run `kith status` later to finish.",
            ),
            Fail::Offline => CliError::offline(),
            other => other.clone().into_cli(),
        }
    }

    fn drop_channel(&mut self) {
        self.chan = None;
        self.sub = None;
    }

    fn patience_after_connected(&self) -> Patience {
        if self.connected_before { Patience::Until(Instant::now() + RECONNECT_BUDGET) } else { Patience::FailFast }
    }

    /// A request with its answer. A dropped socket is replaced and the request sent again; botAsk carries a client nonce
    /// so a resend can't ask twice, and the other methods read or are idempotent.
    pub async fn call(&mut self, method: &str, body: Value) -> Result<Value, CliError> {
        let mut resends = 0;
        loop {
            self.ensure(self.patience_after_connected()).await?;
            let Some(chan) = self.chan.as_mut() else { continue };
            match chan.call(method, body.clone(), CALL_TIMEOUT).await {
                Ok(v) => return Ok(v),
                Err(Fail::Dropped(_) | Fail::Network(_) | Fail::Offline) if resends < 4 => {
                    self.drop_channel();
                    resends += 1;
                }
                Err(f) => {
                    let e = f.into_cli();
                    if e.code == "revoked" {
                        self.set_state(State::Revoked);
                    }
                    return Err(e);
                }
            }
        }
    }

    // ---- events ----

    /// Starts `sub: "events"` from `since`.
    pub async fn open_events(&mut self, since: i64) -> Result<(), CliError> {
        self.sub_since = since;
        self.resubscribe().await
    }

    pub fn advance_events(&mut self, seq: i64) {
        self.sub_since = self.sub_since.max(seq);
    }

    async fn resubscribe(&mut self) -> Result<(), CliError> {
        loop {
            self.ensure(self.patience_after_connected()).await?;
            let Some(chan) = self.chan.as_mut() else { continue };
            match chan.subscribe("events", json!({"since": self.sub_since, "client": "kith"})).await {
                Ok(id) => {
                    self.sub = Some(id);
                    return Ok(());
                }
                Err(f) if f.is_transient() => self.drop_channel(),
                Err(f) => return Err(f.into_cli()),
            }
        }
    }

    /// The next event, or `None` at `until`. A break in the connection is repaired (reconnect, resubscribe) and reported
    /// as `Resync` so the caller reads what it missed.
    pub async fn next_event(&mut self, until: Instant) -> Result<Option<Ev>, CliError> {
        let mut repaired = false;
        loop {
            if self.chan.is_none() || self.sub.is_none() {
                self.drop_channel();
                self.resubscribe().await?;
                repaired = true;
            }
            let (Some(chan), Some(sub)) = (self.chan.as_mut(), self.sub) else { continue };
            match chan.next_event(sub, until).await {
                Ok(Some(Event::Ev(v))) => return Ok(Some(if repaired { Ev::Resync } else { Ev::Event(v) })),
                Ok(None) => return Ok(repaired.then_some(Ev::Resync)),
                Ok(Some(Event::End)) | Err(Fail::Dropped(_) | Fail::Network(_) | Fail::Offline) => self.drop_channel(),
                Err(f) => return Err(f.into_cli()),
            }
        }
    }

    pub async fn close(&mut self) {
        if let Some(c) = self.chan.take() {
            c.close().await;
        }
    }

    // ---- status and joining ----

    /// `botStatus`, written to the cached config. Works while pending.
    pub async fn refresh_status(&mut self) -> Result<BotSelf, CliError> {
        let st: BotSelf = answer(self.call(method::BOT_STATUS, params(&rpc::BotStatusParams {})).await?)?;
        if version_lt(CLIENT_VERSION, &st.min_client) {
            return Err(CliError::version());
        }
        let state = State::of(st.state).ok_or_else(CliError::version)?;
        if !st.bot_id.is_empty() {
            self.guest.bot_id = Some(st.bot_id.clone());
        }
        if !st.name.is_empty() {
            self.guest.name.clone_from(&st.name);
        }
        self.guest.owner = Some(st.owner.name.clone()).filter(|o| !o.is_empty());
        self.guest.agents = st
            .agents
            .iter()
            .map(|a| AgentRef { agent: a.agent.clone(), name: a.name.clone().unwrap_or_default() })
            .collect();
        self.guest.state = state;
        if state != State::Joining {
            self.guest.token = None;
        }
        self.save()?;
        Ok(st)
    }

    /// Shows the invite token to the computer (`proveBotInvite`), once, then reads the bot's status.
    pub async fn prove(&mut self) -> Result<BotSelf, CliError> {
        let Some(token) = self.guest.token.clone() else {
            return Err(CliError::invite(
                "Joining isn't finished and this VM no longer holds the invite. Ask for a new invite link.",
            ));
        };
        let proved = self
            .call(
                method::PROVE_BOT_INVITE,
                params(&ProveBotInviteParams { invite_id: self.guest.invite_id.clone(), token }),
            )
            .await
            .map_err(|e| match e.code {
                // The computer refusing the proof is a problem with the invite, not with the bot's access.
                "forbidden" | "notFound" | "badRequest" => {
                    CliError::invite("The computer didn't accept this invite. Ask for a new link.")
                }
                _ => e,
            })?;
        let proved: BotProof = answer(proved)?;
        let state = State::of(proved.state).ok_or_else(CliError::version)?;
        self.guest.state = state;
        self.guest.token = None;
        self.save()?;
        self.refresh_status().await
    }

    /// Finishes joining if needed and proves the bot may use the computer now: exit 5 while pending, 6 once removed.
    pub async fn ensure_usable(&mut self) -> Result<(), CliError> {
        match self.guest.state {
            State::Revoked | State::Denied => Err(self.gone()),
            State::Joining => {
                self.prove().await?;
                self.check_usable()
            }
            State::Pending => {
                self.refresh_status().await?;
                self.check_usable()
            }
            State::Active => Ok(()),
        }
    }

    pub fn check_usable(&self) -> Result<(), CliError> {
        match self.guest.state {
            State::Active => Ok(()),
            State::Pending | State::Joining => Err(self.pending()),
            State::Revoked | State::Denied => Err(self.gone()),
        }
    }

    pub fn pending(&self) -> CliError {
        CliError::new(Exit::Pending, "pending", "Waiting for the computer's owner to allow this bot")
    }

    pub fn gone(&self) -> CliError {
        if self.guest.state == State::Denied {
            CliError::new(Exit::Removed, "denied", "The computer's owner didn't allow this bot")
        } else {
            CliError::removed()
        }
    }

    pub fn info(&self, line: &str) {
        self.out.info(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_lt("0.1.0", "0.2.0"));
        assert!(version_lt("1.2.3", "1.10.0"));
        assert!(!version_lt("1.0.0", "1.0.0"));
        assert!(!version_lt("2.0.0", "1.9.9"));
    }

    #[test]
    fn backoff_follows_the_documented_steps() {
        let mut b = Backoff::new(Duration::from_secs(1));
        let waits: Vec<f64> = (0..7).map(|_| b.delay().as_secs_f64()).collect();
        for (w, step) in waits.iter().zip([1.0, 2.0, 4.0, 8.0, 15.0, 15.0, 15.0]) {
            assert!(*w >= step * 0.79 && *w <= step * 1.21, "{w} vs {step}");
        }
    }
}
