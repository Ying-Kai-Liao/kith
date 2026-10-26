//! The Kith cloud (Cloudflare relay + accounts, spec §4.2, §4.3, §8.4, §9.6): which one
//! this host uses, its `/v1/host/*` API (signed with `Kith-Sig`), the state pull that may
//! only renew or remove account devices, never add one, and account access requests with
//! the commit-then-reveal SAS. The relay socket itself lives in `relay`.

use crate::LockExt;
use crate::hub::Hub;
use crate::remote::crypto::{self, b64};
use crate::store::{Device, DeviceSource, Scope, Store, now_ms};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;

/// How long one successful state pull keeps an account device allowed.
pub const LEASE_MS: i64 = 15 * 60 * 1000;
const TIMEOUT: Duration = Duration::from_secs(15);
/// Grant revocations the cloud hasn't confirmed yet (kv, JSON array of grant ids).
const PENDING_REVOKES: &str = "grant_revokes";
/// kv: the saved `Approval`.
const APPROVAL: &str = "account_approval";

pub fn approval(store: &Store) -> Approval {
    if store.kv_get(APPROVAL).as_deref() == Some("auto") { Approval::Auto } else { Approval::Code }
}

/// When this host sent its recent SAS nonces (kv, JSON array of ms).
const SAS_NONCES: &str = "sas_nonces";
/// Every host nonce is one guess at the phone's code for whoever controls the cloud, revealed
/// or not: at most this many per hour keeps a 6-digit SAS out of reach of a search.
const SAS_NONCES_PER_HOUR: usize = 5;
const HOUR_MS: i64 = 60 * 60 * 1000;

/// Whether the cloud is switched off by the environment or by the owner (kv `cloud_enabled`).
fn switched_off(store: &Store) -> bool {
    std::env::var("KITH_SERVE_CLOUD").is_ok_and(|v| v == "off")
        || store.kv_get("cloud_enabled").as_deref() == Some("false")
}

/// The `relay_url` of `<data dir>/config.json` (`{"relay_url": "https://…"}`), when it is a
/// valid cloud URL. A missing file is normal; an unreadable or invalid one is logged once.
fn relay_url_from(config: &Path) -> Option<String> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let bytes = std::fs::read(config).ok()?;
    let found = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|v| v.get("relay_url")?.as_str().map(|u| u.trim().trim_end_matches('/').to_owned()))
        .filter(|u| valid_url(&format!("{u}/")));
    if found.is_none() && !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(path = %config.display(), "config.json has no valid relay_url; the built-in relay (if any) is used");
    }
    found
}

/// A cloud URL set through the environment, or `None`.
fn env_url() -> Option<String> {
    std::env::var("KITH_SERVE_CLOUD_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_owned())
        .filter(|u| valid_url(&format!("{u}/")))
}

/// The relay a Dev build uses when nothing else is set (the relay is public).
const DEV_RELAY: &str = "https://kith-cloud-dev.lykevin890919.workers.dev";
/// The production cloud a Main build uses when nothing else is set.
const MAIN_CLOUD: &str = "https://api.kithbot.app";
/// Where people open shared links for the production cloud (the apex serves the invite pages).
const MAIN_LINKS: &str = "https://kithbot.app";

fn default_url() -> String {
    use crate::environment::Environment;
    let url = match Environment::current() {
        Environment::Dev => DEV_RELAY,
        Environment::Main => MAIN_CLOUD,
    };
    url.to_owned()
}

/// The base that shareable pages (`/i/<computer>/<invite>`) live under: the production cloud's apex
/// domain, or any other cloud's own URL.
pub fn links_base(cloud_url: &str) -> String {
    let trimmed = cloud_url.trim().trim_end_matches('/');
    if trimmed == MAIN_CLOUD { MAIN_LINKS.to_owned() } else { trimmed.to_owned() }
}

/// The first of the environment's URL, the config file's, the build's default.
/// The file is read only when the environment gives no URL, so an unused config entry never logs.
fn pick(
    env: Option<String>,
    config: impl FnOnce() -> Option<String>,
    default: impl FnOnce() -> Option<String>,
) -> Option<String> {
    env.or_else(config).or_else(default)
}

/// The cloud base URL in use, or `None` when the cloud is off or no relay URL is set.
/// `KITH_SERVE_CLOUD=off` > turned off here > `KITH_SERVE_CLOUD_URL` > `relay_url` in
/// `<data dir>/config.json` > the build's built-in cloud (the Dev relay, or api.kithbot.app on Main). The URL is never saved in the store: on/off is the only stored cloud setting.
/// Unit tests point at a fake cloud through kv `test_cloud_url`, so they never reach a real one.
pub fn url(store: &Store) -> Option<String> {
    if switched_off(store) {
        return None;
    }
    if cfg!(test) {
        return store.kv_get("test_cloud_url");
    }
    pick(env_url(), || relay_url_from(&crate::service::data_dir().join("config.json")), || Some(default_url()))
}

/// Why the cloud has no URL, for the status: `None` when it is off on purpose or has one.
pub fn unset_reason(store: &Store) -> Option<String> {
    (!switched_off(store) && url(store).is_none())
        .then(|| "No relay URL is set: add relay_url to config.json or set KITH_SERVE_CLOUD_URL.".to_owned())
}

/// A cloud URL this host may be pointed at: https, or plain http to this computer (local dev).
fn valid_url(u: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(u) else { return false };
    let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    parsed.path() == "/"
        && parsed.query().is_none()
        && (parsed.scheme() == "https" || (parsed.scheme() == "http" && local))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub user_id: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// `cloudStatus` and the `cloud` event.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    pub enabled: bool,
    pub url: Option<String>,
    pub registered: bool,
    pub connected: bool,
    pub owner: Option<Owner>,
    pub last_error: Option<String>,
    pub approval: Approval,
}

/// How account devices get in (`setApproval`, local only).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Approval {
    /// The user compares the 6-digit code and approves each device here.
    #[default]
    Code,
    /// Approved as soon as the SAS exchange completes, with no one comparing the code.
    Auto,
}

/// A pending account access request, with this host's own nonce (memory only).
#[derive(Clone)]
struct Request {
    id: String,
    device_key: [u8; 32],
    device_name: String,
    platform: String,
    email: Option<String>,
    commit: [u8; 32],
    host_nonce: [u8; 32],
    /// The SAS, once the device revealed a nonce that matches its commit.
    code: Option<String>,
    /// The invited person that account is on this computer; None is the owner.
    person: Option<String>,
    created_at: i64,
    expires_at: i64,
}

impl Request {
    fn json(&self) -> Value {
        json!({
            "requestId": self.id,
            "deviceKey": b64(&self.device_key),
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

pub struct Cloud {
    status: Mutex<CloudStatus>,
    requests: Mutex<Vec<Request>>,
    /// One state pull at a time.
    pulling: tokio::sync::Mutex<()>,
    /// Bumped when the cloud is turned on or off or pointed elsewhere: the relay restarts.
    pub config: watch::Sender<u64>,
}

impl Default for Cloud {
    fn default() -> Self {
        Self {
            status: Mutex::default(),
            requests: Mutex::default(),
            pulling: tokio::sync::Mutex::new(()),
            config: watch::Sender::new(0),
        }
    }
}

impl Cloud {
    pub fn status(&self) -> CloudStatus {
        self.status.locked().clone()
    }

    pub fn requests_json(&self) -> Vec<Value> {
        self.requests.locked().iter().map(Request::json).collect()
    }
}

/// Changes the status and tells this computer's apps when it actually changed.
pub fn update_status(hub: &Hub, f: impl FnOnce(&mut CloudStatus)) {
    let changed = {
        let mut s = hub.cloud.status.locked();
        let before = s.clone();
        f(&mut s);
        (*s != before).then(|| s.clone())
    };
    if let Some(s) = changed {
        let _ = hub.events.send(json!({"type": "cloud", "cloud": s}));
    }
}

fn emit_requests(hub: &Hub) {
    let _ = hub.events.send(json!({"type": "accessRequests", "requests": hub.cloud.requests_json()}));
}

/// An error answer from the cloud (`{"error":{"code","message"}}`).
#[derive(Debug)]
pub struct CloudError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the Kith cloud refused ({} {}): {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for CloudError {}

fn cloud_status_of(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<CloudError>().map(|c| c.status)
}

/// Something that has to wait for the other side first (409).
#[derive(Debug)]
pub struct Conflict(pub &'static str);

impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Conflict {}

/// The lowercase `Host` header value for `base` (with a non-default port), as `Kith-Sig` signs it.
pub fn authority(base: &str) -> Result<String> {
    let u = reqwest::Url::parse(base).context("invalid cloud URL")?;
    let host = u.host_str().ok_or_else(|| anyhow!("cloud URL has no host"))?;
    Ok(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    }
    .to_ascii_lowercase())
}

fn base(hub: &Hub) -> Result<String> {
    url(&hub.store).ok_or_else(|| anyhow!("The Kith cloud is off on this computer."))
}

/// One signed `/v1/host/*` call.
async fn call(hub: &Hub, base: &str, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
    let sig = hub.identity.sign_request(method.as_str(), &authority(base)?, path, &bytes);
    let mut req = crate::http()
        .request(method, format!("{base}{path}"))
        .header(crypto::Labels::Kith.sig_header(), sig)
        .timeout(TIMEOUT);
    if body.is_some() {
        req = req.header("content-type", "application/json").body(bytes);
    }
    let res = req.send().await.context("the Kith cloud is unreachable")?;
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let e = &v["error"];
        return Err(CloudError {
            status: status.as_u16(),
            code: e["code"].as_str().unwrap_or("internal").to_owned(),
            message: e["message"].as_str().unwrap_or_default().to_owned(),
        }
        .into());
    }
    Ok(v)
}

/// `POST /v1/host/register`: makes this computer known to the cloud (no account needed).
pub async fn register(hub: &Hub, base: &str) -> Result<()> {
    let body = json!({
        "boxKey": hub.identity.box_pub_b64(),
        "name": crate::service::host_name(),
        "platform": std::env::consts::OS,
        "device": tokio::task::spawn_blocking(crate::service::device).await?,
        "version": env!("CARGO_PKG_VERSION"),
    });
    let v = call(hub, base, Method::POST, "/v1/host/register", Some(&body)).await?;
    if v["computerId"].as_str() != Some(hub.identity.computer_id().as_str()) {
        bail!("the cloud registered a different computer id");
    }
    tracing::info!(computer_id = hub.identity.computer_id(), "registered with the Kith cloud");
    update_status(hub, |s| s.registered = true);
    Ok(())
}

/// Short-lived media credentials, issued only after the host checks screen authorization.
pub async fn screen_ice(hub: &Hub, device_key: &str) -> Result<crate::screen::IceConfig> {
    let value =
        call(hub, &base(hub)?, Method::POST, "/v1/host/screen-ice", Some(&json!({"deviceKey": device_key}))).await?;
    let config: crate::screen::IceConfig =
        serde_json::from_value(value).context("invalid screen relay configuration")?;
    if config.ice_servers.is_empty() || config.expires_at <= now_ms() + 60_000 {
        bail!("the screen relay returned expired or empty credentials");
    }
    Ok(config)
}

// MARK: state (§4.3)

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    owner: Option<Owner>,
    /// None from a cloud that predates invites: the memberships this host knows are left alone.
    #[serde(default)]
    memberships: Option<Vec<super::members::Listed>>,
    /// None from a cloud that predates bot guests: no bot is admitted, and the ones this host knows are left alone.
    #[serde(default)]
    bot_devices: Option<Vec<super::botsync::Listed>>,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    requests: Vec<StateRequest>,
    /// The accounts this computer's owner revoked (on any of their Macs). None from a cloud that predates revoking.
    #[serde(default)]
    revoked: Option<Vec<String>>,
}

#[allow(clippy::struct_field_names, reason = "wire names")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub grant_id: String,
    pub device_key: String,
    /// The account that owns the device.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateRequest {
    request_id: String,
    device_key: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    user_id: Option<String>,
    email: Option<String>,
    commit: String,
    #[serde(default)]
    host_nonce: Option<String>,
    #[serde(default)]
    device_nonce: Option<String>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    expires_at: i64,
}

/// `GET /v1/host/state` and apply it: renew or remove account devices, answer access requests.
pub async fn pull(hub: &Hub, base: &str) -> Result<()> {
    let _one = hub.cloud.pulling.lock().await;
    let state: State = serde_json::from_value(call(hub, base, Method::GET, "/v1/host/state", None).await?)
        .context("reading the cloud state")?;
    let owner_uid = state.owner.as_ref().map(|o| o.user_id.clone());
    // Before the memberships: a membership the revoke cut then ends on a person already revoked (R6). A failure fails
    // the pull, so this host stays out of sync instead of serving on access it couldn't apply.
    let bin = super::account::bin().await;
    for person in super::members::apply_revoked(bin.as_deref(), state.revoked.as_deref()).await? {
        crate::api::people::cut_off(hub, &person).await;
    }
    // Before the grants: a member's device is kept only while its account is bound to its person.
    super::members::sync(hub, state.memberships.as_deref(), owner_uid.as_deref()).await;
    super::botsync::sync(hub, state.bot_devices.as_deref()).await;
    apply_grants(hub, &state.grants, owner_uid.as_deref())?;
    update_status(hub, |s| s.owner = state.owner);
    super::account::sync(hub, owner_uid.as_deref()).await;
    sync_requests(hub, base, state.requests, owner_uid.as_deref()).await;
    let auto = approval(&hub.store) == Approval::Auto;
    // A member of this computer was invited by its owner, so their devices don't wait for a comparison.
    let ready: Vec<String> = hub
        .cloud
        .requests
        .locked()
        .iter()
        .filter(|r| r.code.is_some() && (auto || r.person.is_some()))
        .map(|r| r.id.clone())
        .collect();
    // The SAS exchange still runs (the device's commit must match); only the user's comparison is skipped.
    for id in ready {
        if let Err(e) = decide(hub, &id, true).await {
            tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't auto-approve an access request");
        }
    }
    retry_revokes(hub, base).await;
    super::members::retry_revokes(hub).await;
    super::botsync::retry_revokes(hub).await;
    Ok(())
}

/// Whether a grant's account is the one this device was approved for: a member's device needs the account bound to
/// its person, the owner's needs the owner's account. A cloud that doesn't name the account (older) is taken as
/// the owner's.
fn grant_matches(hub: &Hub, d: &Device, user_id: Option<&str>, owner_uid: Option<&str>) -> bool {
    match (&d.person_id, user_id) {
        (Some(person), Some(u)) => hub.store.membership_of_user(u).is_some_and(|m| &m.person_id == person),
        (Some(_), None) => false,
        (None, Some(u)) => owner_uid == Some(u),
        (None, None) => true,
    }
}

/// Whose access request this is: Ok(None) the owner's, Ok(Some(person)) an invited person's, Err when the account is
/// neither (the cloud lists only owner and member requests, so this is a stale or forged one).
fn requester(hub: &Hub, owner_uid: Option<&str>, user_id: Option<&str>) -> Result<Option<String>, &'static str> {
    match user_id {
        None => Ok(None),
        Some(u) if owner_uid == Some(u) => Ok(None),
        Some(u) => hub.store.membership_of_user(u).map(|m| Some(m.person_id)).ok_or("not a member of this computer"),
    }
}

/// A web browser is only ever an invitee's device: the owner's own account never gets one (a browser profile is a
/// weaker place for the owner's keys than an app). The platform is the device's own claim, so this is a policy, not
/// proof; a missing platform (older requests) counts as native.
fn is_web(platform: &str) -> bool {
    platform.eq_ignore_ascii_case("web")
}

/// Whether a request is the owner's (its code is compared by the owner, so it counts against the hourly budget).
fn is_owner_request(hub: &Hub, owner_uid: Option<&str>, user_id: Option<&str>) -> bool {
    matches!(requester(hub, owner_uid, user_id), Ok(None))
}

/// The cloud can only shorten access: a local account device listed with the same grant and
/// key is renewed, one missing is removed; grants this host never approved are ignored, and so are grants
/// whose account is not the device's owner or its bound person.
pub fn apply_grants(hub: &Hub, grants: &[Grant], owner_uid: Option<&str>) -> Result<()> {
    let until = now_ms() + LEASE_MS;
    let mut known = vec![false; grants.len()];
    for d in hub.store.devices()? {
        if d.source != DeviceSource::Account {
            continue;
        }
        let listed = grants.iter().position(|g| Some(&g.grant_id) == d.grant_id.as_ref() && g.device_key == d.key);
        if let Some(i) = listed {
            known[i] = true;
            if grant_matches(hub, &d, grants[i].user_id.as_deref(), owner_uid) {
                hub.store.set_lease(&d.key, until)?;
            } else {
                tracing::warn!(device = d.key, "grant's account doesn't match the device; removing it");
                hub.revoke_device(&d.key)?;
            }
        } else {
            tracing::info!(device = d.key, "account device's grant is gone from the cloud");
            hub.revoke_device(&d.key)?;
        }
    }
    for (g, _) in grants.iter().zip(known).filter(|(_, k)| !k) {
        tracing::warn!(grant_id = g.grant_id, "unknown grant from cloud ignored");
    }
    Ok(())
}

enum Next {
    /// Seen for the first time without a host nonce: send ours.
    Offer(Box<Request>),
    /// Refuse (lost our nonce in a restart, tampered with, or the commit didn't match).
    Deny(String),
}

/// Keeps the pending access requests in step with the cloud (§4.2 B steps 4 and 6).
async fn sync_requests(hub: &Hub, base: &str, list: Vec<StateRequest>, owner_uid: Option<&str>) {
    let mut next = vec![];
    let mut changed = false;
    let mut throttled = false;
    {
        let mut mine = hub.cloud.requests.locked();
        let before = mine.len();
        mine.retain(|r| {
            let kept = list.iter().any(|s| s.request_id == r.id);
            if !kept && r.code.is_none() {
                // Still counted against the hourly nonce budget.
                tracing::warn!(request_id = r.id, "access request vanished without revealing its nonce");
            }
            kept
        });
        changed |= mine.len() != before;
        let mut budget = nonce_budget(&hub.store);
        for s in &list {
            let (Ok(dk), Ok(commit)) = (crypto::unb64_n::<32>(&s.device_key), crypto::unb64_n::<32>(&s.commit)) else {
                next.push(Next::Deny(s.request_id.clone()));
                continue;
            };
            let Some(i) = mine.iter().position(|r| r.id == s.request_id) else {
                // Our nonce is only in memory: after a restart this request can't be finished.
                if s.host_nonce.is_some() {
                    next.push(Next::Deny(s.request_id.clone()));
                } else if mine.iter().any(|r| r.device_key == dk && r.code.is_none())
                    || next.iter().any(|n| matches!(n, Next::Offer(r) if r.device_key == dk))
                {
                    // One unrevealed request per device at a time; this one waits its turn.
                } else if let Err(why) = requester(hub, owner_uid, s.user_id.as_deref()) {
                    tracing::warn!(request_id = s.request_id, why, "denying an access request from an unknown account");
                    next.push(Next::Deny(s.request_id.clone()));
                } else if is_owner_request(hub, owner_uid, s.user_id.as_deref()) && is_web(&s.platform) {
                    tracing::warn!(request_id = s.request_id, "denying a web device's request for the owner's account");
                    next.push(Next::Deny(s.request_id.clone()));
                } else if is_owner_request(hub, owner_uid, s.user_id.as_deref()) && budget.len() >= SAS_NONCES_PER_HOUR
                {
                    throttled = true;
                } else {
                    // A member's code is never compared (they skip the comparison), so their requests don't spend
                    // the budget that guards the owner's own 6-digit code, and can't use it up to lock the owner out.
                    if is_owner_request(hub, owner_uid, s.user_id.as_deref()) {
                        budget.push(now_ms());
                    }
                    next.push(Next::Offer(Box::new(Request {
                        id: s.request_id.clone(),
                        device_key: dk,
                        device_name: s.device_name.clone(),
                        platform: s.platform.clone(),
                        email: s.email.clone(),
                        person: requester(hub, owner_uid, s.user_id.as_deref()).unwrap_or_default(),
                        commit,
                        host_nonce: crypto::random(),
                        code: None,
                        created_at: s.created_at,
                        expires_at: s.expires_at,
                    })));
                }
                continue;
            };
            let r = &mut mine[i];
            let ours = b64(&r.host_nonce);
            if r.device_key != dk || r.commit != commit || s.host_nonce.as_deref() != Some(ours.as_str()) {
                tracing::warn!(request_id = r.id, "access request changed in the cloud; denying it");
                next.push(Next::Deny(r.id.clone()));
                mine.remove(i);
                changed = true;
                continue;
            }
            if r.code.is_some() {
                continue;
            }
            let Some(nd) = s.device_nonce.as_deref() else { continue };
            // The commit is bound to the device's label family; whichever matches picks the SAS labels.
            let family = crypto::unb64_n::<32>(nd).ok().and_then(|nd| {
                crypto::Labels::ALL.into_iter().find(|l| crypto::sas_commit(*l, &dk, &nd) == r.commit).map(|l| (l, nd))
            });
            if let Some((l, nd)) = family {
                r.code = Some(crypto::sas_code(l, &hub.identity.sign_pub(), &dk, &nd, &r.host_nonce));
            } else {
                tracing::warn!(request_id = r.id, "device nonce doesn't match its commit; denying the request");
                next.push(Next::Deny(r.id.clone()));
                mine.remove(i);
            }
            changed = true;
        }
        // Saved before any nonce leaves, so a restart can't reset the budget.
        set_nonce_budget(&hub.store, &budget);
    }
    if throttled {
        tracing::warn!("accessRequests throttled: too many account access requests this hour");
        update_status(hub, |s| {
            s.last_error = Some("Too many requests to use this computer. New ones wait up to an hour.".into());
        });
    }
    for n in next {
        match n {
            Next::Offer(r) => {
                let path = format!("/v1/host/access-requests/{}/nonce", r.id);
                match call(hub, base, Method::POST, &path, Some(&json!({"nonce": b64(&r.host_nonce)}))).await {
                    Ok(_) => {
                        hub.cloud.requests.locked().push(*r);
                        changed = true;
                    }
                    // Someone else set a nonce: not one we can stand behind.
                    Err(e) if cloud_status_of(&e) == Some(409) => deny(hub, base, &r.id).await,
                    Err(e) => {
                        tracing::warn!(
                            error = format!("{e:#}"),
                            request_id = r.id,
                            "couldn't answer an access request"
                        );
                    }
                }
            }
            Next::Deny(id) => deny(hub, base, &id).await,
        }
    }
    if changed {
        emit_requests(hub);
    }
}

/// The host nonces sent in the last hour.
fn nonce_budget(store: &Store) -> Vec<i64> {
    let since = now_ms() - HOUR_MS;
    let sent: Vec<i64> = store.kv_get(SAS_NONCES).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    sent.into_iter().filter(|t| *t > since).collect()
}

fn set_nonce_budget(store: &Store, sent: &[i64]) {
    let saved = serde_json::to_string(sent).map_err(anyhow::Error::from).and_then(|v| store.kv_set(SAS_NONCES, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't save the access request budget");
    }
}

async fn decision(hub: &Hub, base: &str, id: &str, approve: bool) -> Result<Value> {
    let path = format!("/v1/host/access-requests/{id}/decision");
    let decision = if approve { "approve" } else { "deny" };
    call(hub, base, Method::POST, &path, Some(&json!({"decision": decision}))).await
}

async fn deny(hub: &Hub, base: &str, id: &str) {
    if let Err(e) = decision(hub, base, id, false).await {
        tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't deny an access request");
    }
}

/// `decideAccessRequest`: approving adds the device the user saw (its key, the SAS they
/// compared) with the grant the cloud returns; nothing else can add an account device.
pub async fn decide(hub: &Hub, id: &str, approve: bool) -> Result<()> {
    let base = base(hub)?;
    let r =
        hub.cloud.requests.locked().iter().find(|r| r.id == id).cloned().ok_or_else(|| anyhow!("unknown request"))?;
    if approve && r.code.is_none() {
        return Err(Conflict("The device hasn't shown its code yet.").into());
    }
    if approve && r.person.is_none() && is_web(&r.platform) {
        drop(decision(hub, &base, id, false).await);
        hub.cloud.requests.locked().retain(|x| x.id != id);
        emit_requests(hub);
        bail!("A web browser can't be the owner's device.");
    }
    if approve
        && let Some(person) = &r.person
        && !hub.store.memberships()?.iter().any(|m| &m.person_id == person)
    {
        // Their membership ended while the request waited.
        drop(decision(hub, &base, id, false).await);
        hub.cloud.requests.locked().retain(|x| x.id != id);
        emit_requests(hub);
        bail!("That account is no longer a member of this computer.");
    }
    let answer = decision(hub, &base, id, approve).await;
    hub.cloud.requests.locked().retain(|x| x.id != id);
    emit_requests(hub);
    let answer = answer?;
    if !approve {
        return Ok(());
    }
    let grant = answer["grantId"]
        .as_str()
        .filter(|g| g.starts_with("grt_"))
        .ok_or_else(|| anyhow!("the cloud gave no grant"))?;
    hub.store.put_device(&Device {
        key: b64(&r.device_key),
        name: r.device_name.chars().take(100).collect(),
        platform: r.platform,
        source: DeviceSource::Account,
        grant_id: Some(grant.to_owned()),
        scopes: vec![Scope::Control, Scope::Screen],
        lease_until: Some(now_ms() + LEASE_MS),
        created_at: now_ms(),
        last_seen_at: None,
        person_id: r.person,
    })?;
    tracing::info!(grant_id = grant, "account device approved");
    hub.auth_changed();
    Ok(())
}

// MARK: grants removed here

fn pending_revokes(store: &Store) -> Vec<String> {
    store.kv_get(PENDING_REVOKES).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

fn set_pending_revokes(store: &Store, ids: &[String]) {
    let saved = serde_json::to_string(ids).map_err(anyhow::Error::from).and_then(|v| store.kv_set(PENDING_REVOKES, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't remember a grant to revoke");
    }
}

/// Tells the cloud an account device was removed here. Local removal already took effect;
/// a failure is retried on later state pulls.
pub async fn revoke_grant(hub: Arc<Hub>, grant_id: String) {
    let mut pending = pending_revokes(&hub.store);
    if !pending.contains(&grant_id) {
        pending.push(grant_id);
        set_pending_revokes(&hub.store, &pending);
    }
    if let Ok(base) = base(&hub) {
        retry_revokes(&hub, &base).await;
    }
}

async fn retry_revokes(hub: &Hub, base: &str) {
    let pending = pending_revokes(&hub.store);
    if pending.is_empty() {
        return;
    }
    let mut left = vec![];
    for id in pending {
        match call(hub, base, Method::POST, &format!("/v1/host/grants/{id}/revoke"), Some(&json!({}))).await {
            Ok(_) => {}
            Err(e) if cloud_status_of(&e) == Some(404) => {}
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), grant_id = id, "couldn't revoke a grant in the cloud");
                left.push(id);
            }
        }
    }
    set_pending_revokes(&hub.store, &left);
}

// MARK: invites and memberships

/// A signed `POST /v1/host/*` for the invite calls (`path` from `/v1/host/`).
pub async fn post(hub: &Hub, path: &str, body: &Value) -> Result<Value> {
    call(hub, &base(hub)?, Method::POST, path, Some(body)).await
}

/// Whether `e` is the cloud answering with `status`.
pub fn refused_with(e: &anyhow::Error, status: u16) -> bool {
    cloud_status_of(e) == Some(status)
}

// MARK: loopback methods

/// `unclaim`: takes this computer out of its account.
pub async fn unclaim(hub: &Hub) -> Result<()> {
    let base = base(hub)?;
    call(hub, &base, Method::POST, "/v1/host/unclaim", Some(&json!({}))).await?;
    tracing::info!("left the account");
    pull(hub, &base).await
}

/// `setCloud`: turns the cloud on or off; the relay restarts.
pub fn set_cloud(hub: &Hub, enabled: bool) -> Result<CloudStatus> {
    hub.store.kv_set("cloud_enabled", if enabled { "true" } else { "false" })?;
    let now = url(&hub.store);
    update_status(hub, |s| {
        s.enabled = now.is_some();
        s.url.clone_from(&now);
    });
    hub.cloud.config.send_modify(|g| *g += 1);
    Ok(hub.cloud.status())
}

/// `setApproval`: whether account devices need the 6-digit check. Local only.
pub fn set_approval(hub: &Hub, approval: Approval) -> Result<CloudStatus> {
    hub.store.kv_set(APPROVAL, if approval == Approval::Auto { "auto" } else { "code" })?;
    tracing::info!(?approval, "account approval changed");
    update_status(hub, |s| s.approval = approval);
    Ok(hub.cloud.status())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::remote::identity::Identity;

    fn temp_hub() -> Arc<Hub> {
        let dir = std::env::temp_dir().join(format!("kith-cloud-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0)
    }

    fn account_device(key: &str, grant: &str) -> Device {
        Device {
            key: key.into(),
            name: "Phone".into(),
            platform: "ios".into(),
            source: DeviceSource::Account,
            grant_id: Some(grant.into()),
            scopes: vec![Scope::Control, Scope::Screen],
            lease_until: Some(now_ms() + 1000),
            created_at: now_ms(),
            last_seen_at: None,
            person_id: None,
        }
    }

    #[test]
    fn state_only_renews_or_removes() {
        let hub = temp_hub();
        hub.store.put_device(&account_device("kept", "grt_a")).unwrap();
        hub.store.put_device(&account_device("gone", "grt_b")).unwrap();
        hub.store.put_device(&account_device("swapped", "grt_c")).unwrap();
        let mut local = account_device("qr", "x");
        (local.source, local.grant_id, local.lease_until) = (DeviceSource::Local, None, None);
        hub.store.put_device(&local).unwrap();
        let grant = |g: &str, k: &str| Grant { grant_id: g.into(), device_key: k.into(), user_id: None };
        apply_grants(
            &hub,
            &[grant("grt_a", "kept"), grant("grt_c", "another key"), grant("grt_injected", "attacker")],
            None,
        )
        .unwrap();
        let keys: Vec<String> = hub.store.devices().unwrap().into_iter().map(|d| d.key).collect();
        assert_eq!(keys, ["kept", "qr"], "unknown grants add nothing; a grant naming another key doesn't count");
        assert!(hub.store.device("kept").unwrap().lease_until.unwrap() > now_ms() + LEASE_MS - 5000, "renewed");
        assert_eq!(hub.store.device("qr").unwrap().lease_until, None, "QR devices aren't touched");
    }

    #[test]
    fn only_safe_cloud_urls() {
        assert!(valid_url("https://relay.example.workers.dev/"));
        assert!(valid_url("http://127.0.0.1:8787/"));
        assert!(!valid_url("http://example.com/"));
        assert!(!valid_url("https://example.com/path/"));
        assert!(!valid_url("ftp://example.com/"));
        assert_eq!(authority("https://Kith.Example.dev").unwrap(), "kith.example.dev");
        assert_eq!(authority("http://127.0.0.1:8787").unwrap(), "127.0.0.1:8787");
        assert_eq!(authority("https://x.dev:443").unwrap(), "x.dev", "default port is left out");
    }

    #[test]
    fn relay_url_comes_from_config_json_when_valid() {
        let dir = std::env::temp_dir().join(format!("kith-cloud-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.json");
        assert_eq!(relay_url_from(&file), None, "missing file");
        for (body, want) in [
            ("", None),
            ("not json", None),
            ("[]", None),
            (r#"{"relay_url": ""}"#, None),
            (r#"{"relay_url": 5}"#, None),
            (r#"{"relay_url": "http://example.com"}"#, None),
            (r#"{"relay_url": "https://relay.example.workers.dev/"}"#, Some("https://relay.example.workers.dev")),
            (r#"{"relay_url": " http://127.0.0.1:8787 "}"#, Some("http://127.0.0.1:8787")),
        ] {
            std::fs::write(&file, body).unwrap();
            assert_eq!(relay_url_from(&file).as_deref(), want, "{body}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_relay_is_the_environments_then_the_files_then_the_default() {
        let s = |u: &str| Some(u.to_owned());
        assert_eq!(pick(s("e"), || s("c"), || s("d")), s("e"));
        assert_eq!(pick(None, || s("c"), || s("d")), s("c"));
        assert_eq!(pick(None, || None, || s("d")), s("d"));
        assert_eq!(pick(None, || None, || None), None);
        assert_eq!(
            pick(
                s("e"),
                || panic!("the file is not read when the environment has a URL"),
                || panic!("nor the default")
            ),
            s("e")
        );
        assert!(valid_url(&format!("{DEV_RELAY}/")));
    }

    #[test]
    fn main_defaults_to_the_production_cloud_and_links_use_the_apex() {
        assert!(valid_url(&format!("{MAIN_CLOUD}/")));
        assert_eq!(links_base("https://api.kithbot.app"), "https://kithbot.app");
        assert_eq!(links_base("https://api.kithbot.app/"), "https://kithbot.app");
        assert_eq!(links_base(DEV_RELAY), DEV_RELAY);
        assert_eq!(links_base("http://127.0.0.1:8787/"), "http://127.0.0.1:8787");
    }

    #[derive(Default)]
    struct FakeCloud {
        state: Mutex<Value>,
        posts: Mutex<Vec<(String, Value)>>,
    }

    async fn fake_cloud(fake: Arc<FakeCloud>) -> String {
        use axum::extract::{OriginalUri, State as Ax};
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route("/v1/host/state", get(|Ax(f): Ax<Arc<FakeCloud>>| async move { axum::Json(f.state.lock().unwrap().clone()) }))
            .route(
                "/v1/host/access-requests/{id}/{action}",
                post(|Ax(f): Ax<Arc<FakeCloud>>, OriginalUri(uri): OriginalUri, axum::Json(b): axum::Json<Value>| async move {
                    let path = uri.path().to_owned();
                    f.posts.lock().unwrap().push((path.clone(), b.clone()));
                    axum::Json(if path.ends_with("/decision") && b["decision"] == "approve" {
                        json!({"status": "approved", "grantId": "grt_new"})
                    } else {
                        json!({})
                    })
                }),
            )
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn access_request_needs_a_matching_reveal_before_approval() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        let (dk, other_dk): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
        let (nd, wrong): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
        let request = |id: &str, host_nonce: Option<String>, device_nonce: Option<[u8; 32]>| {
            let dk = if id == "req_bad" { other_dk } else { dk };
            json!({"requestId": id, "deviceKey": b64(&dk), "deviceName": "Kevin's iPhone", "platform": "ios",
                "email": "k@example.com", "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, &dk, &nd)),
                "hostNonce": host_nonce, "deviceNonce": device_nonce.map(|n| b64(&n)),
                "createdAt": 1, "expiresAt": 2})
        };
        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_1", "email": "k@example.com"}, "grants": [],
                "requests": [request("req_ok", None, None), request("req_bad", None, None)]});
        pull(&hub, &base).await.unwrap();
        let nonces: Vec<(String, Value)> = fake.posts.lock().unwrap().clone();
        assert_eq!(nonces.len(), 2, "the host sends one nonce per request");
        let nh =
            |id: &str| nonces.iter().find(|(p, _)| p.contains(id)).unwrap().1["nonce"].as_str().unwrap().to_owned();
        assert_eq!(hub.cloud.requests_json()[0]["code"], Value::Null, "no code before the reveal");
        let early = decide(&hub, "req_ok", true).await.unwrap_err();
        assert!(early.is::<Conflict>(), "approve before the reveal is refused");
        assert_eq!(hub.cloud.status().owner.unwrap().user_id, "user_1");

        *fake.state.lock().unwrap() = json!({"owner": null, "grants": [], "requests": [
            request("req_ok", Some(nh("req_ok")), Some(nd)),
            request("req_bad", Some(nh("req_bad")), Some(wrong)),
        ]});
        pull(&hub, &base).await.unwrap();
        let denied =
            fake.posts.lock().unwrap().iter().any(|(p, b)| p.ends_with("req_bad/decision") && b["decision"] == "deny");
        assert!(denied, "a reveal that doesn't match its commit is denied");
        let list = hub.cloud.requests_json();
        assert_eq!(list.len(), 1);
        let nh_ok: [u8; 32] = crypto::unb64_n(&nh("req_ok")).unwrap();
        assert_eq!(list[0]["code"], crypto::sas_code(crypto::Labels::Kith, &hub.identity.sign_pub(), &dk, &nd, &nh_ok));

        decide(&hub, "req_ok", true).await.unwrap();
        let d = hub.store.device(&b64(&dk)).unwrap();
        assert_eq!((d.source, d.grant_id.as_deref()), (DeviceSource::Account, Some("grt_new")));
        assert_eq!(hub.cloud.requests_json().len(), 0);
    }

    #[tokio::test]
    async fn auto_approval_still_needs_a_matching_reveal() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        set_approval(&hub, Approval::Auto).unwrap();
        let (dk, nd): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
        let request = |host_nonce: Option<String>, device_nonce: Option<[u8; 32]>| {
            json!({"owner": null, "grants": [], "requests": [{"requestId": "req_1", "deviceKey": b64(&dk),
                "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, &dk, &nd)), "hostNonce": host_nonce,
                "deviceNonce": device_nonce.map(|n| b64(&n))}]})
        };
        *fake.state.lock().unwrap() = request(None, None);
        pull(&hub, &base).await.unwrap();
        assert!(hub.store.device(&b64(&dk)).is_none(), "nothing is approved before the reveal");
        let nh = fake.posts.lock().unwrap()[0].1["nonce"].as_str().unwrap().to_owned();
        *fake.state.lock().unwrap() = request(Some(nh), Some(nd));
        pull(&hub, &base).await.unwrap();
        assert_eq!(hub.store.device(&b64(&dk)).unwrap().source, DeviceSource::Account, "approved without the user");
    }

    #[tokio::test]
    async fn host_nonces_are_rationed() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        let request =
            |id: String, dk: [u8; 32]| json!({"requestId": id, "deviceKey": b64(&dk), "commit": b64(&[0u8; 32])});
        let one_key: [u8; 32] = crypto::random();
        let mut requests: Vec<Value> = (0..2).map(|i| request(format!("same_{i}"), one_key)).collect();
        requests.extend((0..8).map(|i| request(format!("req_{i}"), crypto::random())));
        *fake.state.lock().unwrap() = json!({"owner": null, "grants": [], "requests": requests});
        pull(&hub, &base).await.unwrap();
        let sent = fake.posts.lock().unwrap().len();
        assert_eq!(sent, SAS_NONCES_PER_HOUR, "a compromised cloud gets a few guesses an hour, not a million");
        assert!(!fake.posts.lock().unwrap().iter().any(|(p, _)| p.contains("same_1")), "one open request per key");

        // Requests that never reveal still used up the budget, across a restart too.
        *fake.state.lock().unwrap() =
            json!({"owner": null, "grants": [], "requests": [request("late".into(), crypto::random())]});
        hub.cloud.requests.locked().clear();
        pull(&hub, &base).await.unwrap();
        assert_eq!(fake.posts.lock().unwrap().len(), SAS_NONCES_PER_HOUR);
        assert!(hub.cloud.status().last_error.is_some(), "the Mac hears about it");
    }

    #[tokio::test]
    async fn a_members_requests_do_not_spend_the_owners_budget() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store
            .add_membership(&crate::store::Membership {
                membership_id: "mem_m".into(),
                user_id: "user_m".into(),
                person_id: "sam".into(),
                invite_id: "inv".into(),
                created_at: now_ms(),
            })
            .unwrap();
        let request = |id: String, user: Option<&str>| json!({"requestId": id, "deviceKey": b64(&crypto::random::<32>()), "commit": b64(&[0u8; 32]), "userId": user});
        let mut requests: Vec<Value> = (0..8).map(|i| request(format!("member_{i}"), Some("user_m"))).collect();
        requests.push(request("mine".into(), None));
        *fake.state.lock().unwrap() = json!({"owner": null, "grants": [], "requests": requests});
        pull(&hub, &base).await.unwrap();
        let posts = fake.posts.lock().unwrap();
        assert!(posts.iter().any(|(p, _)| p.contains("mine")), "the owner's own request still gets its code");
        assert_eq!(posts.len(), 9, "members' requests are not rationed: their code is never compared");
    }

    #[tokio::test]
    async fn web_devices_are_refused_for_the_owner_but_not_members() {
        for mode in [Approval::Auto, Approval::Code] {
            let hub = temp_hub();
            let fake = Arc::new(FakeCloud::default());
            let base = fake_cloud(fake.clone()).await;
            hub.store.kv_set("test_cloud_url", &base).unwrap();
            set_approval(&hub, mode).unwrap();
            hub.store
                .add_membership(&crate::store::Membership {
                    membership_id: "mem_m".into(),
                    user_id: "user_m".into(),
                    person_id: "sam".into(),
                    invite_id: "inv".into(),
                    created_at: now_ms(),
                })
                .unwrap();
            let request = |id: &str, user: &str, platform: &str| json!({"requestId": id, "deviceKey": b64(&crypto::random::<32>()), "commit": b64(&[0u8; 32]), "userId": user, "platform": platform});
            *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_o", "email": "o@example.com"}, "grants": [], "requests": [
                request("owner_web", "user_o", "web"), request("owner_ios", "user_o", "ios"),
                request("member_web", "user_m", "web"), request("owner_none", "user_o", "")]});
            pull(&hub, &base).await.unwrap();
            let posts = fake.posts.lock().unwrap();
            assert!(posts.iter().any(|(p, b)| p.ends_with("owner_web/decision") && b["decision"] == "deny"));
            assert!(!posts.iter().any(|(p, _)| p.contains("owner_web") && !p.ends_with("/decision")));
            for ok in ["owner_ios", "member_web", "owner_none"] {
                assert!(!posts.iter().any(|(p, b)| p.contains(ok) && b["decision"] == "deny"), "{ok} is not denied");
                assert!(posts.iter().any(|(p, _)| p.contains(ok)), "{ok} still gets its nonce");
            }
        }
    }
}
