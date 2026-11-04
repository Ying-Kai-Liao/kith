//! The Kith cloud (Cloudflare relay + accounts, spec §4.2, §4.3, §8.4, §9.6): which one
//! this host uses, its `/v1/host/*` API (signed with `Kith-Sig`), the state pull that may
//! only renew or remove account devices, never add one, and account access requests with
//! the commit-then-reveal SAS. The relay socket itself lives in `relay`.

use crate::LockExt;
use crate::hub::Hub;
use crate::remote::crypto::{self, b64};
use crate::store::{Device, DeviceSource, Scope, Store, now_ms};
use anyhow::{Context, Result, anyhow, bail};
use kith_wire::cloud::{
    Decided, Decision, EmptyAnswer, HostAccessRequest, HostDecision, HostGrant, HostNonce, HostPlatform, HostRegister,
    HostRegistered, HostState, IceConfig, ScreenIceRequest, path,
};
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
/// A member's browser unused this long is removed; they ask again and the owner approves again.
const WEB_IDLE_MS: i64 = 30 * 24 * HOUR_MS;
/// kv: the saved `Approval`.
const APPROVAL: &str = "account_approval";

pub fn approval(store: &Store) -> Approval {
    if store.kv_get(APPROVAL).as_deref() == Some("auto") { Approval::Auto } else { Approval::Code }
}

/// When this host sent its recent SAS nonces (kv, JSON array of ms).
const SAS_NONCES: &str = "sas_nonces";
/// The same for members' requests whose code is compared (Code mode), kept apart so they can't use up the owner's.
const MEMBER_SAS_NONCES: &str = "member_sas_nonces";
/// Every host nonce is one guess at the phone's code for whoever controls the cloud, revealed
/// or not: at most this many per hour keeps a 6-digit SAS out of reach of a search.
pub(super) const SAS_NONCES_PER_HOUR: usize = 5;
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

/// The production cloud, used when nothing else is set.
const MAIN_CLOUD: &str = "https://api.kithbot.app";
#[cfg(test)]
const OTHER_CLOUD: &str = "https://cloud.example.com";
/// Where people open shared links for the production cloud (the apex serves the invite pages).
const MAIN_LINKS: &str = "https://kithbot.app";

fn default_url() -> String {
    MAIN_CLOUD.to_owned()
}

/// The base that shareable pages (`/i/<computer>/<invite>`) live under: the production cloud's apex
/// domain, or any other cloud's own URL.
/// Whether `cloud_url` is the production cloud (a host can be pointed elsewhere with `kith serve relay`).
pub fn is_main_cloud(cloud_url: &str) -> bool {
    cloud_url.trim().trim_end_matches('/') == MAIN_CLOUD
}

pub fn links_base(cloud_url: &str) -> String {
    let trimmed = cloud_url.trim().trim_end_matches('/');
    if is_main_cloud(trimmed) { MAIN_LINKS.to_owned() } else { trimmed.to_owned() }
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

/// The account this computer belongs to: the cloud contract's (serve/contract/cloud/contract.json).
pub use kith_wire::cloud::Owner;

/// `cloudStatus` and the `cloud` event.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[expect(clippy::struct_excessive_bools, reason = "independent status flags the apps read as they are")]
pub struct CloudStatus {
    pub enabled: bool,
    pub url: Option<String>,
    pub registered: bool,
    pub connected: bool,
    /// The owner this computer is pinned to (`remote::owner`).
    pub owner: Option<Owner>,
    pub last_error: Option<String>,
    pub approval: Approval,
    /// The cloud names another owner than `owner`: nothing that depends on the owner is applied until someone on
    /// this computer confirms it (`confirmOwner`).
    pub owner_change: Option<OwnerChange>,
    /// `owner` is the cloud's first answer: this Mac knew no owner account of its own when it was pinned. Shown
    /// until someone on this computer confirms it (`confirmOwner` with that account).
    pub owner_from_cloud: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OwnerChange {
    /// The cloud's owner; None when it says this computer has none.
    pub owner: Option<Owner>,
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
    /// Mac link pairings in progress (memory only).
    pub(super) pairings: super::peers::Pairings,
    /// One state pull at a time.
    pulling: tokio::sync::Mutex<()>,
    /// Held across every load-change-store of the deny queue: the pull's end step and `deny_or_queue` (which does not
    /// hold `pulling`) both write it.
    deny_queue: Mutex<()>,
    /// Bumped when the cloud is turned on or off or pointed elsewhere: the relay restarts.
    pub config: watch::Sender<u64>,
}

impl Default for Cloud {
    fn default() -> Self {
        Self {
            status: Mutex::default(),
            requests: Mutex::default(),
            pairings: super::peers::Pairings::default(),
            pulling: tokio::sync::Mutex::new(()),
            deny_queue: Mutex::new(()),
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

    /// What a caller may see: all of them on this computer, members' requests only on the owner's other devices.
    pub fn requests_json_for(&self, local: bool) -> Vec<Value> {
        self.requests.locked().iter().filter(|r| local || r.person.is_some()).map(Request::json).collect()
    }

    pub fn is_member_request(&self, id: &str) -> bool {
        self.requests.locked().iter().any(|r| r.id == id && r.person.is_some())
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

/// Whether the core holds the authority over access requests (`KITH_CORE_AUTHORITY=1`). A test says so on the hub it
/// builds, not through the process-wide `TEST_CORE`, which would reach every other test pulling at the same time.
fn core_on(hub: &Hub) -> bool {
    #[cfg(test)]
    return hub.store.kv_get("test_core_authority").is_some();
    #[cfg(not(test))]
    {
        let _ = hub;
        crate::api::authority::core_selected()
    }
}

/// Tells the apps the requests changed. With `KITH_CORE_AUTHORITY=1` the list is the core's (what `accessRequests`
/// answers), so the event and the answer agree; a core that can't be asked sends nothing (the app asks again).
async fn emit_requests(hub: &Hub) {
    let requests = if core_on(hub) {
        match core_requests(true).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "couldn't read the access requests from the core");
                return;
            }
        }
    } else {
        hub.cloud.requests_json()
    };
    let _ = hub.events.send(json!({"type": "accessRequests", "requests": requests}));
}

/// The requests waiting for the owner, from the core (`people.requests`). `local`: every request, as on this computer;
/// otherwise members' only, as on the owner's other devices.
pub async fn core_requests(local: bool) -> Result<Vec<Value>> {
    let answer = crate::api::authority::owner_call("people.requests", json!({"local": local}), true).await?;
    Ok(answer["requests"].as_array().cloned().unwrap_or_default())
}

/// Puts the requests this host holds in the core (`people.fileRequest`, idempotent), on every pull: a core that was down
/// when a request came, and a request held in memory from before the switch was turned on, are filed on the next one.
/// Returns whether anything new went in.
async fn file_in_core(hub: &Hub) -> bool {
    if !core_on(hub) {
        return false;
    }
    let mine: Vec<Request> = hub.cloud.requests.locked().clone();
    let mut filed = false;
    let deadline = tokio::time::Instant::now() + CORE_DENY_BUDGET;
    for r in mine {
        let params = json!({
            "id": r.id, "deviceKey": b64(&r.device_key), "deviceName": r.device_name, "platform": r.platform,
            "email": r.email, "person": r.person, "createdAt": r.created_at, "expiresAt": r.expires_at, "code": r.code,
        });
        let call = crate::api::authority::owner_call("people.fileRequest", params, false);
        match tokio::time::timeout_at(deadline, call).await {
            Err(_) => {
                tracing::warn!("the core didn't answer in time; the rest of the requests are filed on the next pull");
                break;
            }
            Ok(Ok(answer)) => filed |= answer["filed"] == true,
            Ok(Err(e)) => tracing::warn!(
                error = format!("{e:#}"),
                request_id = r.id,
                "couldn't file an access request in the core"
            ),
        }
    }
    filed
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

pub(super) fn cloud_status_of(e: &anyhow::Error) -> Option<u16> {
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

pub(super) fn base(hub: &Hub) -> Result<String> {
    url(&hub.store).ok_or_else(|| anyhow!("The Kith cloud is off on this computer."))
}

/// One signed `/v1/host/*` call.
pub(super) async fn call(hub: &Hub, base: &str, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
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

/// One signed `/v1/host/*` call with a contract body, read as the contract's answer.
pub(super) async fn send<B: Serialize, T: serde::de::DeserializeOwned>(
    hub: &Hub,
    base: &str,
    path: &str,
    body: &B,
) -> Result<T> {
    let v = call(hub, base, Method::POST, path, Some(&serde_json::to_value(body)?)).await?;
    serde_json::from_value(v).with_context(|| format!("reading the cloud's answer to {path}"))
}

/// What this computer registers as: its OS, in the contract's words (the same as `std::env::consts::OS`).
fn host_platform() -> HostPlatform {
    match std::env::consts::OS {
        "macos" => HostPlatform::Macos,
        "linux" => HostPlatform::Linux,
        "windows" => HostPlatform::Windows,
        _ => HostPlatform::Unknown,
    }
}

/// `POST /v1/host/register`: makes this computer known to the cloud (no account needed).
pub async fn register(hub: &Hub, base: &str) -> Result<()> {
    let body = HostRegister {
        box_key: hub.identity.box_pub_b64(),
        name: crate::service::host_name(),
        platform: host_platform(),
        // The device word is the host RPC's too: it stays a string in the cloud contract.
        device: serde_json::to_value(tokio::task::spawn_blocking(crate::service::device).await?)?
            .as_str()
            .map(str::to_owned),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    let v: HostRegistered = send(hub, base, path::HOST_REGISTER, &body).await?;
    if v.computer_id != hub.identity.computer_id() {
        bail!("the cloud registered a different computer id");
    }
    tracing::info!(computer_id = hub.identity.computer_id(), "registered with the Kith cloud");
    update_status(hub, |s| s.registered = true);
    Ok(())
}

/// Short-lived media credentials, issued only after the host checks screen authorization.
pub async fn screen_ice(hub: &Hub, device_key: &str) -> Result<IceConfig> {
    let body = ScreenIceRequest { device_key: device_key.to_owned() };
    let config: IceConfig =
        send(hub, &base(hub)?, path::HOST_SCREEN_ICE, &body).await.context("invalid screen relay configuration")?;
    if config.ice_servers.is_empty() || config.expires_at <= now_ms() + 60_000 {
        bail!("the screen relay returned expired or empty credentials");
    }
    Ok(config)
}

// MARK: state (§4.3)

/// `GET /v1/host/state` and apply it: renew or remove account devices, answer access requests. While the cloud names
/// another owner than the pinned one, only what removes access is applied.
pub async fn pull(hub: &Hub, base: &str) -> Result<()> {
    let _one = hub.cloud.pulling.lock().await;
    // memberships, botDevices and revoked are None from a cloud that predates them: what this host knows stays.
    let state: HostState = serde_json::from_value(call(hub, base, Method::GET, path::HOST_STATE, None).await?)
        .context("reading the cloud state")?;
    let bin = super::account::bin().await;
    let known = if super::owner::unpinned(&hub.store) {
        // The kith-auth the account handle is written through (`account::sync`), so a test never reads the Mac's own.
        let auth = tokio::task::spawn_blocking(super::account::auth_bin).await.ok().flatten();
        super::owner::known(&hub.store, auth.as_deref()).await
    } else {
        None
    };
    let (owner, change) = match super::owner::check(&hub.store, state.owner.as_ref(), known.as_deref())? {
        super::owner::Check::Trusted(owner) => (owner, None),
        super::owner::Check::Held { pinned, cloud } => {
            let change = OwnerChange { owner: cloud };
            if hub.cloud.status().owner_change.as_ref() != Some(&change) {
                tracing::warn!(
                    pinned = pinned.as_ref().map(|o| o.user_id.as_str()),
                    cloud = change.owner.as_ref().map(|o| o.user_id.as_str()),
                    "the cloud names another owner for this computer; waiting for a confirmation here"
                );
            }
            (pinned, Some(change))
        }
    };
    let held = change.is_some();
    let owner_uid = owner.as_ref().map(|o| o.user_id.clone());
    // Before the memberships: a membership the revoke cut then ends on a person already revoked (R6). A failure fails
    // the pull, so this host stays out of sync instead of serving on access it couldn't apply.
    let changed = super::members::apply_revoked(bin.as_deref(), state.revoked.as_deref()).await?;
    for person in changed.cut {
        crate::api::people::cut_off(hub, &person).await;
    }
    for person in changed.restored {
        crate::api::people::restored(hub, &person);
    }
    // Before the grants: a member's device is kept only while its account is bound to its person.
    super::members::sync(hub, state.memberships.as_deref(), owner_uid.as_deref()).await;
    super::botsync::sync(hub, state.bot_devices.as_deref()).await;
    if held {
        revoke_ungranted(hub, &state.grants)?;
        // Pairings are the pinned owner's: none outlives a change of owner, and none is made until it is confirmed.
        super::peers::drop_all(hub)?;
    } else {
        apply_grants(hub, &state.grants, owner_uid.as_deref())?;
        super::peers::apply(hub, state.peers.as_deref())?;
    }
    update_status(hub, |s| {
        s.owner = owner;
        s.owner_change = change;
        s.owner_from_cloud = super::owner::from_cloud(&hub.store);
    });
    if !held {
        super::account::sync(hub, owner_uid.as_deref()).await;
        sync_requests(hub, base, state.requests, owner_uid.as_deref()).await;
        // Never approved here: a computer gets in only by a yes typed on this one (`decidePeerRequest`).
        super::peers::sync(hub, base, state.peer_requests).await;
        let auto = approval(&hub.store) == Approval::Auto;
        // In Code mode every device waits for the owner to compare its code, a member's too: the invite named a person,
        // not a device, and only the code tells that person's device from one the cloud swapped in.
        let ready: Vec<String> =
            hub.cloud.requests.locked().iter().filter(|r| auto && r.code.is_some()).map(|r| r.id.clone()).collect();
        // The SAS exchange still runs (the device's commit must match); only the user's comparison is skipped.
        let deadline = tokio::time::Instant::now() + CORE_DENY_BUDGET;
        for id in ready {
            // The core settles the auto-approval as well when it holds the authority: no yes goes round it.
            let settled = if core_on(hub) {
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!("the core's budget for this pull is spent; the rest are approved on the next one");
                    break;
                }
                decide_in_core_until(hub, &id, true, true, false, Some(deadline)).await
            } else {
                settle(hub, &id, true, false).await
            };
            if let Err(e) = settled {
                tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't auto-approve an access request");
            }
        }
    }
    expire_idle_web(hub);
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
pub fn apply_grants(hub: &Hub, grants: &[HostGrant], owner_uid: Option<&str>) -> Result<()> {
    let until = now_ms() + LEASE_MS;
    let mut known = vec![false; grants.len()];
    for d in hub.store.devices()? {
        if d.source != DeviceSource::Account {
            continue;
        }
        let listed = grants.iter().position(|g| Some(&g.grant_id) == d.grant_id.as_ref() && g.device_key == d.key);
        // The owner's undo of a revoke grants the person's device again under a new id; the row this Mac kept for the
        // undo is that device (same key, same account), so it follows the new grant instead of needing a new approval.
        let regranted = || {
            let mut same = grants.iter().enumerate().filter(|(_, g)| g.device_key == d.key);
            let (i, g) = same.next()?;
            (same.next().is_none() && !known[i] && grant_matches(hub, &d, g.user_id.as_deref(), owner_uid)).then_some(i)
        };
        let listed = listed.or_else(|| {
            let i = regranted()?;
            hub.store.set_grant_id(&d.key, &grants[i].grant_id).ok()?;
            Some(i)
        });
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

/// While the owner is held: an account device whose grant is gone is removed, and nothing is renewed.
fn revoke_ungranted(hub: &Hub, grants: &[HostGrant]) -> Result<()> {
    for d in hub.store.devices()? {
        let listed = grants.iter().any(|g| Some(&g.grant_id) == d.grant_id.as_ref() && g.device_key == d.key);
        if d.source == DeviceSource::Account && !listed {
            tracing::info!(device = d.key, "account device's grant is gone from the cloud");
            hub.revoke_device(&d.key)?;
        }
    }
    Ok(())
}

impl Next {
    fn id(&self) -> &str {
        match self {
            Next::Offer(r) => &r.id,
            Next::Deny(id) => id,
        }
    }
}

enum Next {
    /// Seen for the first time without a host nonce: send ours.
    Offer(Box<Request>),
    /// Refuse (lost our nonce in a restart, tampered with, or the commit didn't match).
    Deny(String),
}

/// Keeps the pending access requests in step with the cloud (§4.2 B steps 4 and 6).
async fn sync_requests(hub: &Hub, base: &str, list: Vec<HostAccessRequest>, owner_uid: Option<&str>) {
    let mut next = vec![];
    let mut changed = false;
    let mut throttled = false;
    // Requests this pull ends on the host's own word (denied, or no longer listed by the cloud): the core is told too.
    let mut ended: Vec<String> = vec![];
    {
        let mut mine = hub.cloud.requests.locked();
        let before = mine.len();
        mine.retain(|r| {
            let kept = list.iter().any(|s| s.request_id == r.id);
            if !kept {
                ended.push(r.id.clone());
            }
            if !kept && r.code.is_none() {
                // Still counted against the hourly nonce budget.
                tracing::warn!(request_id = r.id, "access request vanished without revealing its nonce");
            }
            kept
        });
        changed |= mine.len() != before;
        let mut budget = nonce_budget(&hub.store, SAS_NONCES);
        let mut member_budget = nonce_budget(&hub.store, MEMBER_SAS_NONCES);
        // In Auto mode nobody compares a member's code, so their requests aren't rationed.
        let compared = approval(&hub.store) == Approval::Code;
        for s in &list {
            let (Ok(dk), Ok(commit)) = (crypto::unb64_n::<32>(&s.device_key), crypto::unb64_n::<32>(&s.commit)) else {
                next.push(Next::Deny(s.request_id.clone()));
                continue;
            };
            let owner = is_owner_request(hub, owner_uid, s.user_id.as_deref());
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
                } else if owner && is_web(&s.platform) {
                    tracing::warn!(request_id = s.request_id, "denying a web device's request for the owner's account");
                    next.push(Next::Deny(s.request_id.clone()));
                } else if (owner && budget.len() >= SAS_NONCES_PER_HOUR)
                    || (!owner && compared && member_budget.len() >= SAS_NONCES_PER_HOUR)
                {
                    throttled = true;
                } else {
                    // Members' requests spend a budget of their own, so they can't use up the one that guards the
                    // owner's 6-digit code and lock the owner out.
                    if owner {
                        budget.push(now_ms());
                    } else if compared {
                        member_budget.push(now_ms());
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
            let committed = crypto::unb64_n::<32>(nd)
                .ok()
                .filter(|nd| crypto::sas_commit(crypto::Labels::Kith, &dk, nd) == r.commit);
            if let Some(nd) = committed {
                r.code =
                    Some(crypto::sas_code(crypto::Labels::Kith, &hub.identity.sign_pub(), &dk, &nd, &r.host_nonce));
            } else {
                tracing::warn!(request_id = r.id, "device nonce doesn't match its commit; denying the request");
                next.push(Next::Deny(r.id.clone()));
                mine.remove(i);
            }
            changed = true;
        }
        // Saved before any nonce leaves, so a restart can't reset the budget.
        set_nonce_budget(&hub.store, SAS_NONCES, &budget);
        set_nonce_budget(&hub.store, MEMBER_SAS_NONCES, &member_budget);
    }
    // Written before the first await: the host no longer holds these and the cloud no longer lists them, so a pull cut
    // off below (the relay's wait, a closed connection, a restart) would lose them for good.
    let denying: Vec<String> = next.iter().filter(|n| matches!(n, Next::Deny(_))).map(|n| n.id().to_owned()).collect();
    queue_for_core(hub, ended.iter().chain(&denying));
    if throttled {
        tracing::warn!("accessRequests throttled: too many account access requests this hour");
        update_status(hub, |s| {
            s.last_error = Some("Too many requests to use this computer. New ones wait up to an hour.".into());
        });
    }
    for n in next {
        match n {
            Next::Offer(r) => {
                let nonce = HostNonce { nonce: b64(&r.host_nonce) };
                match send::<_, EmptyAnswer>(hub, base, &path::host_nonce(&r.id), &nonce).await {
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
            Next::Deny(id) => {
                deny(hub, base, &id).await;
            }
        }
    }
    // "Unknown request" in the core (never filed there, or already ended) is fine. One the core could not be told about
    // stays queued (kept across a restart) and is ended on the next sync; the switch off ends them all at once.
    end_queued_in_core(hub, async {}).await;
    // Before the event, so what it carries (the core's list, with the switch on) already has this pull's requests.
    changed |= file_in_core(hub).await;
    if changed {
        emit_requests(hub).await;
    }
}

/// The most one state pull waits on the core for the requests it ends: the client waits up to 30 s a call, so a hung
/// core with N requests would hold the lease renewal behind it N times. One budget for the whole batch; what it didn't
/// reach is left for the next pull.
const CORE_DENY_BUDGET: Duration = Duration::from_secs(10);

/// Where the ids `sync_requests` ended but the core could not be told about wait for the next sync.
const DENY_QUEUE_KEY: &str = "core_deny_queue";

/// The pull's end step: ends the queued ids in the core, then takes out of the queue only the ones it finished. It
/// never stores its snapshot back, so a deny queued meanwhile survives. `between` runs after the read and before the
/// write-back (a seam for the test of exactly that).
async fn end_queued_in_core(hub: &Hub, between: impl std::future::Future<Output = ()>) {
    let mut ended = take_deny_queue(hub);
    let held: Vec<String> = hub.cloud.requests.locked().iter().map(|r| r.id.clone()).collect();
    ended.retain(|id| !held.contains(id));
    ended.sort();
    ended.dedup();
    let done = deny_in_core(hub, &ended).await;
    between.await;
    // A held id is back in the host's list (the cloud re-listed it): it leaves the queue unended, as before.
    edit_deny_queue(hub, |queue| queue.retain(|id| !done.contains(id) && !held.contains(id)));
}

/// The one way to change the queue: load, change and store under one lock.
fn edit_deny_queue(hub: &Hub, change: impl FnOnce(&mut Vec<String>)) {
    let _one = hub.cloud.deny_queue.locked();
    let mut queue = take_deny_queue(hub);
    change(&mut queue);
    queue.sort();
    queue.dedup();
    set_deny_queue(hub, &queue);
}

fn take_deny_queue(hub: &Hub) -> Vec<String> {
    hub.store.kv_get(DENY_QUEUE_KEY).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Adds `ids` to the queue (kept across a restart) before anything that can wait or fail is tried.
fn queue_for_core<'a>(hub: &Hub, ids: impl IntoIterator<Item = &'a String>) {
    edit_deny_queue(hub, |queue| queue.extend(ids.into_iter().cloned()));
}

/// Ends `id` in the core; one the core could not be told about is queued for the next sync, which the host's own copy
/// of the request no longer leads to once it is dropped. "Unknown request" is fine.
async fn deny_or_queue(hub: &Hub, id: &str, deadline: Option<tokio::time::Instant>) {
    match core_call_until(deadline, "people.deny", json!({"id": id, "local": true}), false).await {
        Ok(_) => {}
        Err(e) if e.downcast_ref::<crate::api::authority::CoreRefused>().is_some() => {}
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't deny an access request in the core");
            queue_for_core(hub, [&id.to_owned()]);
        }
    }
}

fn set_deny_queue(hub: &Hub, ids: &[String]) {
    let saved =
        serde_json::to_string(ids).map_err(anyhow::Error::from).and_then(|v| hub.store.kv_set(DENY_QUEUE_KEY, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't save the access requests still to end in the core");
    }
}

/// Ends `ids` in the core (`people.deny`, local) when it holds the authority, so they stop showing in the app's list
/// (M2-17). Returns the ids that are ended there: all of them with the switch off; with it on, those the core ended
/// or no longer knows ("unknown request" is a refusal, and fine). A core that can't be asked within the budget is
/// logged and its ids are left out.
async fn deny_in_core(hub: &Hub, ids: &[String]) -> Vec<String> {
    if !core_on(hub) {
        return ids.to_vec();
    }
    let deadline = tokio::time::Instant::now() + CORE_DENY_BUDGET;
    let mut done = vec![];
    for id in ids {
        let call = crate::api::authority::owner_call("people.deny", json!({"id": id, "local": true}), false);
        match tokio::time::timeout_at(deadline, call).await {
            Ok(Ok(_)) => done.push(id.clone()),
            Ok(Err(e)) if e.downcast_ref::<crate::api::authority::CoreRefused>().is_some() => done.push(id.clone()),
            Ok(Err(e)) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    request_id = id,
                    "couldn't deny an access request in the core"
                );
            }
            Err(_) => {
                tracing::warn!("the core didn't answer in time; the rest of the denials wait for the next pull");
                break;
            }
        }
    }
    done
}

/// Denies the pending requests of a person who has no membership here (M2-17, gap "membership gone"): the cloud can
/// report a membership gone while the person stays in kith-auth, and the core reads only kith-auth, so it could not see
/// that and would still approve. Runs on every membership sync, not only when a membership changed: a person suspended
/// or removed, or whose membership the cloud dropped, may have left a request behind at any earlier pull. With the
/// switch on the core is told too (`people.deny`; "unknown request" there is fine, it was never filed or already ended).
pub(super) async fn deny_without_membership(hub: &Hub) {
    let Ok(memberships) = hub.store.memberships() else { return };
    let orphans: Vec<String> = hub
        .cloud
        .requests
        .locked()
        .iter()
        .filter(|r| r.person.as_ref().is_some_and(|p| !memberships.iter().any(|m| &m.person_id == p)))
        .map(|r| r.id.clone())
        .collect();
    if orphans.is_empty() {
        return;
    }
    let base = base(hub).ok();
    for id in &orphans {
        tracing::info!(request_id = id, "denying an access request: its account has no membership here");
    }
    // A request the core could not be told about stays where it is, so the next sync denies it again.
    let done = deny_in_core(hub, &orphans).await;
    for id in &done {
        if let Some(base) = &base {
            deny(hub, base, id).await;
        }
    }
    hub.cloud.requests.locked().retain(|r| !done.contains(&r.id));
    emit_requests(hub).await;
}

/// The host nonces sent in the last hour, from the budget kept under `key`.
pub(super) fn nonce_budget(store: &Store, key: &str) -> Vec<i64> {
    let since = now_ms() - HOUR_MS;
    let sent: Vec<i64> = store.kv_get(key).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    sent.into_iter().filter(|t| *t > since).collect()
}

pub(super) fn set_nonce_budget(store: &Store, key: &str, sent: &[i64]) {
    let saved = serde_json::to_string(sent).map_err(anyhow::Error::from).and_then(|v| store.kv_set(key, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't save the access request budget");
    }
}

async fn decision(hub: &Hub, base: &str, id: &str, approve: bool) -> Result<Decided> {
    let body = HostDecision { decision: if approve { Decision::Approve } else { Decision::Deny } };
    send(hub, base, &path::host_decision(id), &body).await
}

async fn deny(hub: &Hub, base: &str, id: &str) {
    if let Err(e) = decision(hub, base, id, false).await {
        tracing::warn!(error = format!("{e:#}"), request_id = id, "couldn't deny an access request");
    }
}

/// `decideAccessRequest`: approving adds the device the user saw (its key, the SAS they
/// compared) with the grant the cloud returns; nothing else can add an account device.
pub async fn decide(hub: &Hub, id: &str, approve: bool) -> Result<()> {
    settle(hub, id, approve, true).await
}

/// The core's record of a request is not the one this host holds: the request ends on the cloud and in the core (a
/// row that is already approved there answers "unknown request", which is fine), and this host's copy goes.
async fn refuse_mismatch(hub: &Hub, base: &str, id: &str, deadline: Option<tokio::time::Instant>) -> Result<()> {
    tracing::warn!(request_id = id, "the core's device differs from this host's request; denying it");
    deny_or_queue(hub, id, deadline).await;
    drop(decision(hub, base, id, false).await);
    hub.cloud.requests.locked().retain(|x| x.id != id);
    emit_requests(hub).await;
    bail!("The core's record of that request doesn't match what this computer was asked. It was denied.");
}

/// `decideAccessRequest` with `KITH_CORE_AUTHORITY=1` (M2-17): the core decides (`people.approve` / `people.deny`: the
/// refusals, the request's end) and answers which device to add; the host then asks the cloud for the grant and stores
/// exactly that device, as `settle` does after its own checks. `local`: the caller is the owner at this computer; any
/// other caller is answered for members' requests only. `by_owner` as in `settle`. A core that can't be asked is an error and nothing happens (the
/// request stays as it was); there is no fallback to the host's own check.
pub async fn decide_in_core(hub: &Hub, id: &str, approve: bool, local: bool, by_owner: bool) -> Result<()> {
    decide_in_core_until(hub, id, approve, local, by_owner, None).await
}

/// A call to the core that gives up at `deadline` (an error: nothing was decided as far as this host knows). Only the
/// call to the core is bounded, never the cloud steps after it, so a decision is not cut off half way.
async fn core_call_until(
    deadline: Option<tokio::time::Instant>,
    method: &'static str,
    params: Value,
    repeatable: bool,
) -> Result<Value> {
    let call = crate::api::authority::owner_call(method, params, repeatable);
    match deadline {
        Some(d) => tokio::time::timeout_at(d, call).await.unwrap_or_else(|_| bail!("the core didn't answer in time")),
        None => call.await,
    }
}

/// `decide_in_core` with the core calls bounded by `deadline` (the auto-approval of a pull spends one budget).
async fn decide_in_core_until(
    hub: &Hub,
    id: &str,
    approve: bool,
    local: bool,
    by_owner: bool,
    deadline: Option<tokio::time::Instant>,
) -> Result<()> {
    const NEEDS_CODE: &str = "The device hasn't shown its code yet.";
    const WEB_OWNER: &str = "A web browser can't be the owner's device.";
    const NOT_MEMBER: &str = "That account is no longer a member of this computer.";
    const ONLY_HERE: &str = "This can only be done at the computer itself.";
    if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
        bail!("the core's budget for this pull is spent");
    }
    let base = base(hub)?;
    // The host's own checks come first, as in `settle`: the core's rows can be filed by any process of the owner's user
    // and its membership view is the core's own tables, which a host-side suspension does not reach until M2-8/M2-18.
    let held =
        hub.cloud.requests.locked().iter().find(|r| r.id == id).cloned().ok_or_else(|| anyhow!("unknown request"))?;
    if approve && held.code.is_none() {
        return Err(Conflict(NEEDS_CODE).into());
    }
    if approve
        && let Some(person) = &held.person
        && !hub.store.memberships()?.iter().any(|m| &m.person_id == person)
    {
        // Their membership ended while the request waited: ended in the core too, so it stops showing there.
        deny_or_queue(hub, id, deadline).await;
        drop(decision(hub, &base, id, false).await);
        hub.cloud.requests.locked().retain(|x| x.id != id);
        emit_requests(hub).await;
        bail!(NOT_MEMBER);
    }
    // The device the core holds must be the one this host holds the request of, and that is looked at before the core
    // approves: an approval cannot be taken back (the row is no longer pending), so a refusal after it would leave an
    // approved row for a device nobody was granted. A row filed by something else first ends here, in the core too.
    if approve {
        let rows = core_call_until(deadline, "people.requests", json!({"local": true}), true).await?;
        let row = rows["requests"].as_array().and_then(|l| l.iter().find(|r| r["requestId"] == id));
        if row.is_some_and(|r| {
            r["deviceKey"].as_str() != Some(b64(&held.device_key).as_str())
                || r["personId"].as_str() != held.person.as_deref()
                || r["platform"].as_str() != Some(held.platform.as_str())
                || r["deviceName"].as_str() != Some(held.device_name.as_str())
        }) {
            return refuse_mismatch(hub, &base, id, deadline).await;
        }
    }
    let method = if approve { "people.approve" } else { "people.deny" };
    // A decision is never cut off by the budget (the core may have committed it); it runs on the client's own timeout.
    let said = match core_call_until(None, method, json!({"id": id, "local": local}), false).await {
        Ok(said) => said,
        Err(e) => {
            let Some(crate::api::authority::CoreRefused(why)) = e.downcast_ref() else { return Err(e) };
            return Err(match why.as_str() {
                ONLY_HERE => crate::api::devices::Forbidden.into(),
                NEEDS_CODE => Conflict(NEEDS_CODE).into(),
                // The core ended the request with this refusal; the cloud and this host's copy follow, as in `settle`.
                WEB_OWNER | NOT_MEMBER => {
                    drop(decision(hub, &base, id, false).await);
                    hub.cloud.requests.locked().retain(|x| x.id != id);
                    emit_requests(hub).await;
                    anyhow!(why.clone())
                }
                _ => anyhow!(why.clone()),
            });
        }
    };
    // The device the core names must be the one this host holds the request of (its key and person): the core's row may
    // have been filed by something else first. Refused before the cloud is asked for a grant, and the request ends.
    if approve
        && (said["device"]["key"].as_str() != Some(b64(&held.device_key).as_str())
            || said["device"]["personId"].as_str() != held.person.as_deref())
    {
        return refuse_mismatch(hub, &base, id, deadline).await;
    }
    let answer = decision(hub, &base, id, approve).await;
    hub.cloud.requests.locked().retain(|x| x.id != id);
    emit_requests(hub).await;
    let answer = answer?;
    if !approve {
        return Ok(());
    }
    let grant = answer
        .grant_id
        .as_deref()
        .filter(|g| g.starts_with("grt_"))
        .ok_or_else(|| anyhow!("the cloud gave no grant"))?;
    // The device exactly as the core answered it, not rebuilt from this host's copy of the request.
    let device = &said["device"];
    let key = device["key"].as_str().filter(|k| !k.is_empty()).ok_or_else(|| anyhow!("the core named no device"))?;
    let person = device["personId"].as_str().map(str::to_owned);
    let scopes: Vec<Scope> = device["scopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| match s.as_str() {
            Some("control") => Some(Scope::Control),
            Some("screen") => Some(Scope::Screen),
            _ => None,
        })
        .collect();
    hub.store.put_device(&Device {
        key: key.to_owned(),
        name: device["name"].as_str().unwrap_or_default().chars().take(100).collect(),
        platform: device["platform"].as_str().unwrap_or_default().to_owned(),
        source: DeviceSource::Account,
        grant_id: Some(grant.to_owned()),
        scopes,
        lease_until: Some(now_ms() + LEASE_MS),
        created_at: now_ms(),
        last_seen_at: None,
        person_id: person.clone(),
    })?;
    // The owner approved this device on the Mac after comparing its code (see `settle`).
    if by_owner && person.is_some() {
        hub.store.set_device_proven(key)?;
    }
    tracing::info!(grant_id = grant, "account device approved (by the core)");
    hub.auth_changed();
    Ok(())
}

/// `by_owner`: the owner decided on this Mac (not auto mode), so a member's device needs no invite token as well.
async fn settle(hub: &Hub, id: &str, approve: bool, by_owner: bool) -> Result<()> {
    let base = base(hub)?;
    let r =
        hub.cloud.requests.locked().iter().find(|r| r.id == id).cloned().ok_or_else(|| anyhow!("unknown request"))?;
    if approve && r.code.is_none() {
        return Err(Conflict("The device hasn't shown its code yet.").into());
    }
    if approve && r.person.is_none() && is_web(&r.platform) {
        drop(decision(hub, &base, id, false).await);
        hub.cloud.requests.locked().retain(|x| x.id != id);
        emit_requests(hub).await;
        bail!("A web browser can't be the owner's device.");
    }
    if approve
        && let Some(person) = &r.person
        && !hub.store.memberships()?.iter().any(|m| &m.person_id == person)
    {
        // Their membership ended while the request waited.
        drop(decision(hub, &base, id, false).await);
        hub.cloud.requests.locked().retain(|x| x.id != id);
        emit_requests(hub).await;
        bail!("That account is no longer a member of this computer.");
    }
    let answer = decision(hub, &base, id, approve).await;
    hub.cloud.requests.locked().retain(|x| x.id != id);
    emit_requests(hub).await;
    let answer = answer?;
    if !approve {
        return Ok(());
    }
    let grant = answer
        .grant_id
        .as_deref()
        .filter(|g| g.starts_with("grt_"))
        .ok_or_else(|| anyhow!("the cloud gave no grant"))?;
    let member = by_owner && r.person.is_some();
    hub.store.put_device(&Device {
        key: b64(&r.device_key),
        name: r.device_name.chars().take(100).collect(),
        platform: r.platform,
        source: DeviceSource::Account,
        grant_id: Some(grant.to_owned()),
        // A member never shares this computer's screen, so their device isn't granted it.
        scopes: if r.person.is_some() { vec![Scope::Control] } else { vec![Scope::Control, Scope::Screen] },
        lease_until: Some(now_ms() + LEASE_MS),
        created_at: now_ms(),
        last_seen_at: None,
        person_id: r.person,
    })?;
    // The owner approved this device on the Mac after comparing its code, which the cloud can't fake: that is as strong
    // as the invite token, and a member's new browser (after a sign-out, in another tab) has no token left to show.
    if member {
        hub.store.set_device_proven(&b64(&r.device_key))?;
    }
    tracing::info!(grant_id = grant, "account device approved");
    hub.auth_changed();
    Ok(())
}

// MARK: grants removed here

fn pending_revokes(store: &Store) -> Vec<String> {
    store.kv_get(PENDING_REVOKES).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Changes the queue of grants to revoke in one step: it is read again here, so a grant queued while the caller was
/// awaiting the cloud is kept, and no `.await` sits between the read and the write.
fn update_pending_revokes(store: &Store, change: impl FnOnce(&mut Vec<String>)) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = WRITING.locked();
    let mut pending = pending_revokes(store);
    change(&mut pending);
    let saved =
        serde_json::to_string(&pending).map_err(anyhow::Error::from).and_then(|v| store.kv_set(PENDING_REVOKES, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't remember a grant to revoke");
    }
}

fn queue_revokes(store: &Store, ids: impl IntoIterator<Item = String>) {
    update_pending_revokes(store, |pending| {
        for id in ids {
            if !pending.contains(&id) {
                pending.push(id);
            }
        }
    });
}

/// Tells the cloud an account device was removed here. Local removal already took effect;
/// a failure is retried on later state pulls.
pub async fn revoke_grant(hub: Arc<Hub>, grant_id: String) {
    queue_revokes(&hub.store, [grant_id]);
    if let Ok(base) = base(&hub) {
        retry_revokes(&hub, &base).await;
    }
}

/// Removes members' web devices nobody used for `WEB_IDLE_MS` (never used: counted from approval), and queues their
/// grants for the cloud. A browser is the device most likely left signed in somewhere; a phone or the owner's own stay.
/// One with a channel open now is in use, however long ago it was opened.
fn expire_idle_web(hub: &Hub) {
    let Ok(devices) = hub.store.devices() else { return };
    let cutoff = now_ms() - WEB_IDLE_MS;
    let mut grants = vec![];
    for d in devices {
        let idle = d.source == DeviceSource::Account
            && d.person_id.is_some()
            && is_web(&d.platform)
            && d.last_seen_at.unwrap_or(d.created_at) < cutoff
            && !hub.connected.locked().contains_key(&d.key);
        if !idle || !hub.revoke_device(&d.key).unwrap_or(false) {
            continue;
        }
        tracing::info!(device = d.key, "removed a member's web device unused for 30 days");
        grants.extend(d.grant_id);
    }
    queue_revokes(&hub.store, grants);
}

async fn retry_revokes(hub: &Hub, base: &str) {
    let pending = pending_revokes(&hub.store);
    if pending.is_empty() {
        return;
    }
    let mut done = vec![];
    for id in pending {
        match call(hub, base, Method::POST, &path::host_revoke_grant(&id), Some(&json!({}))).await {
            Ok(_) => done.push(id),
            Err(e) if cloud_status_of(&e) == Some(404) => done.push(id),
            Err(e) => tracing::warn!(error = format!("{e:#}"), grant_id = id, "couldn't revoke a grant in the cloud"),
        }
    }
    update_pending_revokes(&hub.store, |pending| pending.retain(|id| !done.contains(id)));
}

// MARK: invites and memberships

/// A signed `POST /v1/host/*` for the invite calls (`path` from `kith_wire::cloud::path`).
pub async fn post<B: Serialize>(hub: &Hub, path: &str, body: &B) -> Result<Value> {
    call(hub, &base(hub)?, Method::POST, path, Some(&serde_json::to_value(body)?)).await
}

/// Whether `e` is the cloud answering with `status`.
pub fn refused_with(e: &anyhow::Error, status: u16) -> bool {
    cloud_status_of(e) == Some(status)
}

// MARK: loopback methods

/// `unclaim`: takes this computer out of its account.
pub async fn unclaim(hub: &Hub) -> Result<()> {
    let base = base(hub)?;
    call(hub, &base, Method::POST, path::HOST_UNCLAIM, Some(&json!({}))).await?;
    tracing::info!("left the account");
    super::owner::pin(&hub.store, None)?;
    pull(hub, &base).await
}

/// `confirmOwner` (local only): someone on this computer accepts the owner the cloud names, `user_id` (None: no
/// owner) as they saw it, so a change that happened since is not confirmed with it. With no change waiting, it confirms
/// an owner that was set from the cloud.
pub async fn confirm_owner(hub: &Hub, user_id: Option<&str>) -> Result<CloudStatus> {
    let base = base(hub)?;
    let status = hub.cloud.status();
    let accepted = match status.owner_change {
        Some(change) => change.owner,
        None if status.owner_from_cloud => status.owner,
        None => bail!("The Kith cloud names no other owner for this computer."),
    };
    if accepted.as_ref().map(|o| o.user_id.as_str()) != user_id {
        return Err(Conflict("The cloud's owner changed again; check it and confirm again.").into());
    }
    super::owner::pin(&hub.store, accepted.as_ref())?;
    tracing::info!(owner = user_id, "the owner was confirmed on this computer");
    pull(hub, &base).await?;
    Ok(hub.cloud.status())
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
    fn a_members_idle_browser_is_removed_after_30_days() {
        let hub = temp_hub();
        let old = now_ms() - WEB_IDLE_MS - 1000;
        let device = |key: &str, platform: &str, person: Option<&str>, created_at: i64| Device {
            platform: platform.into(),
            person_id: person.map(str::to_owned),
            created_at,
            ..account_device(key, &format!("grt_{key}"))
        };
        for d in [
            device("idle", "web", Some("sam"), old),
            device("fresh", "web", Some("sam"), now_ms()),
            device("phone", "ios", Some("sam"), old),
            device("owners", "web", None, old),
            device("open", "web", Some("sam"), old),
        ] {
            hub.store.put_device(&d).unwrap();
        }
        hub.connected.locked().insert("open".into(), 1);
        expire_idle_web(&hub);
        assert!(hub.store.device("idle").is_none(), "unused for 30 days: gone");
        assert_eq!(pending_revokes(&hub.store), ["grt_idle"], "and its grant goes to the cloud");
        for kept in ["fresh", "phone", "owners", "open"] {
            assert!(hub.store.device(kept).is_some(), "{kept} stays");
        }
    }

    #[tokio::test]
    async fn a_grant_queued_while_a_retry_is_in_flight_is_not_lost() {
        use axum::extract::State as Ax;
        let hub = temp_hub();
        let (hit, gate) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        let app = axum::Router::new()
            .route(
                "/v1/host/grants/{id}/revoke",
                axum::routing::post(
                    |Ax((hit, gate)): Ax<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>| async move {
                        hit.notify_one();
                        gate.notified().await;
                        axum::Json(json!({}))
                    },
                ),
            )
            .with_state((hit.clone(), gate.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        queue_revokes(&hub.store, ["grt_a".to_owned()]);
        let retrying = hub.clone();
        let retry = tokio::spawn(async move { retry_revokes(&retrying, &base).await });
        hit.notified().await; // the cloud call for grt_a is pending
        queue_revokes(&hub.store, ["grt_b".to_owned()]); // the owner removes another device meanwhile
        gate.notify_one();
        retry.await.unwrap();
        assert_eq!(pending_revokes(&hub.store), ["grt_b"], "grt_a went through; grt_b is still queued");
    }

    #[test]
    fn the_owners_phone_sees_and_may_answer_only_members_requests() {
        let hub = temp_hub();
        let request = |id: &str, person: Option<&str>| Request {
            id: id.into(),
            device_key: crypto::random(),
            device_name: "Kith web".into(),
            platform: "web".into(),
            email: None,
            commit: [0; 32],
            host_nonce: [0; 32],
            code: Some("123456".into()),
            person: person.map(str::to_owned),
            created_at: now_ms(),
            expires_at: now_ms() + HOUR_MS,
        };
        hub.cloud.requests.locked().extend([request("req_owner", None), request("req_sam", Some("sam"))]);
        let ids = |v: Vec<Value>| v.iter().map(|r| r["requestId"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(ids(hub.cloud.requests_json_for(true)), ["req_owner", "req_sam"], "the computer sees all");
        assert_eq!(ids(hub.cloud.requests_json_for(false)), ["req_sam"], "a phone sees members' only");
        assert!(hub.cloud.is_member_request("req_sam"));
        assert!(!hub.cloud.is_member_request("req_owner"), "an owner device is approved at the computer");
        assert!(!hub.cloud.is_member_request("req_gone"));
    }

    /// invitee-access property 2, generated: whatever the request and however it is settled, a member's device counts
    /// as proven only when the owner approved it by hand (`decide`, from the Mac or the owner's phone), never when auto
    /// mode approved it, and nothing but an approved request with its code shown adds a device.
    #[test]
    fn a_members_device_is_proven_only_by_the_owners_own_approval() {
        use crate::api::devices::{Caller, Principal, principal};
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let base = rt.block_on(fake_cloud(Arc::new(FakeCloud::default())));
        let mut config = Config { failure_persistence: None, ..Config::default() };
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 64;
        }
        let case = (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>());
        TestRunner::new(config)
            .run(&case, |(member_request, still_member, web, code_shown, approve, by_owner)| {
                let hub = temp_hub();
                hub.store.kv_set("test_cloud_url", &base).unwrap();
                if still_member {
                    add_member(&hub);
                }
                let key = crypto::random();
                hub.cloud.requests.locked().push(Request {
                    id: "req".into(),
                    device_key: key,
                    device_name: "Kith".into(),
                    platform: if web { "web" } else { "ios" }.into(),
                    email: None,
                    commit: [0; 32],
                    host_nonce: [0; 32],
                    code: code_shown.then(|| "123456".into()),
                    person: member_request.then(|| "sam".into()),
                    created_at: now_ms(),
                    expires_at: now_ms() + HOUR_MS,
                });
                drop(rt.block_on(settle(&hub, "req", approve, by_owner)));
                let added = approve && code_shown && if member_request { still_member } else { !web };
                let want = match (added, member_request) {
                    (false, _) => Principal::Nobody,
                    (true, false) => Principal::Owner,
                    (true, true) if by_owner => Principal::Member("sam".into()),
                    (true, true) => Principal::Unproven("sam".into()),
                };
                let caller = Caller::Device { key: b64(&key), scopes: vec![Scope::Control] };
                prop_assert_eq!(principal(&hub.store, &caller), want);
                Ok(())
            })
            .unwrap();
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
        let grant = |g: &str, k: &str| HostGrant { grant_id: g.into(), device_key: k.into(), ..HostGrant::default() };
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
        assert!(valid_url(&format!("{OTHER_CLOUD}/")));
    }

    #[test]
    fn only_the_production_cloud_counts_as_main() {
        assert!(is_main_cloud(MAIN_CLOUD) && is_main_cloud(&format!(" {MAIN_CLOUD}/ ")));
        assert!(!is_main_cloud(OTHER_CLOUD) && !is_main_cloud("http://127.0.0.1:8792"));
    }

    #[test]
    fn main_defaults_to_the_production_cloud_and_links_use_the_apex() {
        assert!(valid_url(&format!("{MAIN_CLOUD}/")));
        assert_eq!(links_base("https://api.kithbot.app"), "https://kithbot.app");
        assert_eq!(links_base("https://api.kithbot.app/"), "https://kithbot.app");
        assert_eq!(links_base(OTHER_CLOUD), OTHER_CLOUD);
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
                    axum::Json(if !path.ends_with("/decision") {
                        json!({})
                    } else if b["decision"] == "approve" {
                        json!({"status": "approved", "grantId": "grt_new"})
                    } else {
                        json!({"status": "denied"})
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

        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_1"}, "grants": [], "requests": [
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
        assert_eq!(d.scopes, [Scope::Control, Scope::Screen], "the owner's device can share the screen");
        assert_eq!(hub.cloud.requests_json().len(), 0);
    }

    fn add_member(hub: &Hub) {
        hub.store
            .add_membership(&crate::store::Membership {
                membership_id: "mem_m".into(),
                user_id: "user_m".into(),
                person_id: "sam".into(),
                invite_id: "inv".into(),
                created_at: now_ms(),
            })
            .unwrap();
    }

    /// A member's request from `dk` (committed to `nd`), with the host's and the device's nonces once they are out.
    fn member_request(
        id: &str,
        dk: [u8; 32],
        nd: [u8; 32],
        host_nonce: Option<&str>,
        device_nonce: Option<[u8; 32]>,
    ) -> Value {
        json!({"requestId": id, "deviceKey": b64(&dk), "deviceName": "Sam's iPhone", "platform": "ios", "userId": "user_m",
            "email": "sam@example.com", "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, &dk, &nd)),
            "hostNonce": host_nonce, "deviceNonce": device_nonce.map(|n| b64(&n)), "createdAt": 0, "expiresAt": 0})
    }

    /// M2-3: the script the core's `people.requests|approve|deny` rows are held to
    /// (`core/runtime/src/authority/tests/requests.rs` replays the same file). It runs through `decide`, the real
    /// `settle`, against the fake cloud; a request arrives by being put in the list, as `sync_requests` does once the
    /// device has revealed its code, and "the cloud stops listing it" is the pull that drops it.
    #[tokio::test]
    async fn access_request_parity_script() {
        let script: Value =
            serde_json::from_str(include_str!("../../../../kernel/tests/fixtures/access_requests_parity.json"))
                .unwrap();
        for scenario in script["scenarios"].as_array().unwrap() {
            let name = scenario["name"].as_str().unwrap();
            let hub = temp_hub();
            let fake = Arc::new(FakeCloud::default());
            let base = fake_cloud(fake.clone()).await;
            hub.store.kv_set("test_cloud_url", &base).unwrap();
            add_member(&hub);
            let ids = |hub: &Hub| -> Vec<String> {
                hub.cloud.requests_json_for(true).iter().map(|r| r["requestId"].as_str().unwrap().to_owned()).collect()
            };
            for (n, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
                let at = format!("{name}: step {n} {step}");
                if let Some(f) = step.get("file") {
                    let key = [u8::try_from(f["device"].as_u64().unwrap()).unwrap(); 32];
                    hub.cloud.requests.locked().push(Request {
                        id: f["id"].as_str().unwrap().to_owned(),
                        device_key: key,
                        device_name: "Phone".into(),
                        platform: f["platform"].as_str().unwrap().to_owned(),
                        email: None,
                        commit: [0; 32],
                        host_nonce: [0; 32],
                        code: (f["code"] == true).then(|| "123456".to_owned()),
                        person: f["person"].as_str().map(str::to_owned),
                        created_at: i64::try_from(n).unwrap(),
                        expires_at: i64::MAX,
                    });
                } else if step.get("suspend").is_some() {
                    hub.store.suspend_person("sam").unwrap();
                } else if step.get("sync").is_some() {
                    // the host's membership sync after a suspension: the waiting requests are denied
                    deny_without_membership(&hub).await;
                } else if step.get("undo").is_some() {
                    hub.store.restore_person("sam").unwrap();
                } else if step.get("membership_gone").is_some() {
                    hub.store.remove_membership("mem_m").unwrap();
                    // the host's membership sync, which denies that person's pending requests (M2-17)
                    deny_without_membership(&hub).await;
                } else if let Some(id) = step["expire"].as_str() {
                    hub.cloud.requests.locked().retain(|r| r.id != id);
                } else if let Some(want) = step.get("list") {
                    assert_eq!(&serde_json::json!(ids(&hub)), want, "{at}");
                } else {
                    let (id, approve) = match step["approve"].as_str() {
                        Some(id) => (id, true),
                        None => (step["deny"].as_str().unwrap(), false),
                    };
                    let device = hub.cloud.requests.locked().iter().find(|r| r.id == id).map(|r| b64(&r.device_key));
                    let got = decide(&hub, id, approve).await;
                    match (&step["expect"]["err"], got) {
                        (Value::String(want), Err(e)) => assert_eq!(&e.to_string(), want, "{at}"),
                        (Value::Null, Ok(())) => {
                            if let Some(want) = step["expect"].get("scopes") {
                                let d = hub.store.device(&device.unwrap()).unwrap();
                                let scopes: Vec<&str> = d
                                    .scopes
                                    .iter()
                                    .map(|s| if *s == Scope::Control { "control" } else { "screen" })
                                    .collect();
                                assert_eq!(&serde_json::json!(scopes), want, "{at}");
                            }
                        }
                        (want, got) => panic!("{at}: wanted {want}, got {got:?}"),
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn in_code_mode_a_members_device_waits_for_the_code_too() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        assert_eq!(approval(&hub.store), Approval::Code);
        let (dk, bad_dk, nd, wrong): ([u8; 32], [u8; 32], [u8; 32], [u8; 32]) =
            (crypto::random(), crypto::random(), crypto::random(), crypto::random());
        let state = |requests: Vec<Value>| json!({"owner": {"userId": "user_o"}, "grants": [], "requests": requests});
        *fake.state.lock().unwrap() =
            state(vec![member_request("req_m", dk, nd, None, None), member_request("req_bad", bad_dk, nd, None, None)]);
        pull(&hub, &base).await.unwrap();
        let nh = |id: &str| {
            let posts = fake.posts.lock().unwrap();
            posts.iter().find(|(p, _)| p.contains(id)).unwrap().1["nonce"].as_str().unwrap().to_owned()
        };
        let (nh_m, nh_bad) = (nh("req_m"), nh("req_bad"));
        *fake.state.lock().unwrap() = state(vec![
            member_request("req_m", dk, nd, Some(&nh_m), Some(nd)),
            member_request("req_bad", bad_dk, nd, Some(&nh_bad), Some(wrong)),
        ]);
        pull(&hub, &base).await.unwrap();
        let denied =
            fake.posts.lock().unwrap().iter().any(|(p, b)| p.ends_with("req_bad/decision") && b["decision"] == "deny");
        assert!(denied, "a member's reveal that doesn't match its commit is refused");
        assert!(hub.store.device(&b64(&dk)).is_none(), "not approved without the owner checking the code");
        let list = hub.cloud.requests_json();
        assert_eq!(list.len(), 1);
        let nh_m: [u8; 32] = crypto::unb64_n(&nh_m).unwrap();
        assert_eq!(list[0]["code"], crypto::sas_code(crypto::Labels::Kith, &hub.identity.sign_pub(), &dk, &nd, &nh_m));
        assert_eq!(list[0]["personId"], "sam");
        // Another pull leaves it waiting for the owner.
        pull(&hub, &base).await.unwrap();
        assert!(hub.store.device(&b64(&dk)).is_none());
        assert!(!fake.posts.lock().unwrap().iter().any(|(p, _)| p.ends_with("req_m/decision")));

        decide(&hub, "req_m", true).await.unwrap();
        let d = hub.store.device(&b64(&dk)).unwrap();
        assert_eq!((d.source, d.person_id.as_deref()), (DeviceSource::Account, Some("sam")));
        assert_eq!(d.scopes, [Scope::Control], "a member's device never carries the screen");
        assert!(hub.store.device_proven(&b64(&dk)), "the owner's approval stands in for the invite token");
    }

    #[tokio::test]
    async fn in_auto_mode_a_members_device_is_approved_on_its_reveal() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        set_approval(&hub, Approval::Auto).unwrap();
        add_member(&hub);
        let (dk, nd): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
        let state = |r: Value| json!({"owner": {"userId": "user_o"}, "grants": [], "requests": [r]});
        *fake.state.lock().unwrap() = state(member_request("req_m", dk, nd, None, None));
        pull(&hub, &base).await.unwrap();
        assert!(hub.store.device(&b64(&dk)).is_none(), "nothing is approved before the reveal");
        let nh = fake.posts.lock().unwrap()[0].1["nonce"].as_str().unwrap().to_owned();
        *fake.state.lock().unwrap() = state(member_request("req_m", dk, nd, Some(&nh), Some(nd)));
        pull(&hub, &base).await.unwrap();
        let d = hub.store.device(&b64(&dk)).unwrap();
        assert_eq!((d.source, d.person_id.as_deref()), (DeviceSource::Account, Some("sam")), "approved on its own");
        assert_eq!(d.scopes, [Scope::Control]);
        assert!(!hub.store.device_proven(&b64(&dk)), "nobody checked the code: it still shows its invite");
    }

    #[tokio::test]
    async fn the_cloud_alone_cannot_change_the_owner() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        set_approval(&hub, Approval::Auto).unwrap();
        hub.store.put_device(&account_device("phone", "grt_a")).unwrap();
        hub.store.put_device(&account_device("lost", "grt_b")).unwrap();
        let grant = |g: &str, k: &str, u: &str| json!({"grantId": g, "deviceKey": k, "userId": u});
        let renewed = |key: &str| hub.store.device(key).unwrap().lease_until.unwrap() > now_ms() + LEASE_MS - 5000;
        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_kai"},
            "grants": [grant("grt_a", "phone", "user_kai"), grant("grt_b", "lost", "user_kai")], "requests": []});
        pull(&hub, &base).await.unwrap();
        assert!(renewed("phone"), "the first pull pins the owner it names");
        assert!(hub.cloud.status().owner_from_cloud, "when this Mac knew no owner of its own, and says so");

        hub.store.put_device(&account_device("phone", "grt_a")).unwrap();
        let (dk, nd): ([u8; 32], [u8; 32]) = (crypto::random(), crypto::random());
        let request = json!({"requestId": "req_1", "deviceKey": b64(&dk), "userId": "user_mallory", "deviceName": "Phone",
            "platform": "ios", "commit": b64(&crypto::sas_commit(crypto::Labels::Kith, &dk, &nd)), "createdAt": 0, "expiresAt": 0});
        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_mallory"},
            "grants": [grant("grt_a", "phone", "user_mallory")], "requests": [request]});
        pull(&hub, &base).await.unwrap();
        let status = hub.cloud.status();
        assert_eq!(status.owner.unwrap().user_id, "user_kai", "the owner stays the pinned one");
        assert_eq!(status.owner_change.unwrap().owner.unwrap().user_id, "user_mallory", "the apps see the change");
        assert!(hub.store.device("phone").is_some(), "the cloud's owner doesn't remove the owner's devices");
        assert!(!renewed("phone"), "nor renews them");
        assert!(hub.store.device("lost").is_none(), "a grant gone from the cloud is still revoked");
        assert!(fake.posts.lock().unwrap().is_empty(), "no access request is answered");
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());

        let stale = confirm_owner(&hub, Some("user_kai")).await.unwrap_err();
        assert!(stale.is::<Conflict>(), "only the owner the cloud names now is confirmed");
        let status = confirm_owner(&hub, Some("user_mallory")).await.unwrap();
        assert_eq!((status.owner.unwrap().user_id, status.owner_change), ("user_mallory".to_owned(), None));
        assert!(!status.owner_from_cloud, "confirmed here");
        assert!(renewed("phone"), "confirmed here: the state applies again");
        assert_eq!(fake.posts.lock().unwrap().len(), 1, "and the request gets its nonce");
        assert!(confirm_owner(&hub, Some("user_mallory")).await.is_err(), "nothing left to confirm");
    }

    #[tokio::test]
    async fn an_owner_set_from_the_cloud_is_confirmed_here() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_kai"}, "grants": [], "requests": []});
        pull(&hub, &base).await.unwrap();
        assert!(hub.cloud.status().owner_from_cloud);
        let other = confirm_owner(&hub, Some("user_other")).await.unwrap_err();
        assert!(other.is::<Conflict>(), "only the owner shown");
        let status = confirm_owner(&hub, Some("user_kai")).await.unwrap();
        assert_eq!((status.owner.unwrap().user_id, status.owner_from_cloud), ("user_kai".to_owned(), false));
        pull(&hub, &base).await.unwrap();
        assert!(!hub.cloud.status().owner_from_cloud, "and stays confirmed");
        assert!(confirm_owner(&hub, Some("user_kai")).await.is_err(), "nothing left to confirm");
    }

    #[tokio::test]
    async fn a_claim_signed_here_moves_the_owner_without_a_confirmation() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        *fake.state.lock().unwrap() = json!({"owner": null, "grants": [], "requests": []});
        pull(&hub, &base).await.unwrap();
        *fake.state.lock().unwrap() = json!({"owner": {"userId": "user_kai"}, "grants": [], "requests": []});
        pull(&hub, &base).await.unwrap();
        assert!(hub.cloud.status().owner_change.is_some(), "an unclaimed computer isn't claimed by the cloud alone");
        crate::remote::owner::signed_for(&hub.store, "user_kai").unwrap();
        pull(&hub, &base).await.unwrap();
        let status = hub.cloud.status();
        assert_eq!((status.owner.unwrap().user_id, status.owner_change), ("user_kai".to_owned(), None));
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
                "deviceNonce": device_nonce.map(|n| b64(&n)), "deviceName": "Phone", "platform": "ios",
                "createdAt": 1, "expiresAt": 2}]})
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
        let request = |id: String, dk: [u8; 32]| {
            json!({"requestId": id, "deviceKey": b64(&dk), "commit": b64(&[0u8; 32]),
                "deviceName": "Phone", "platform": "ios", "createdAt": 1, "expiresAt": 2})
        };
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
        // Code mode compares members' codes, so they are rationed too, on a budget of their own; Auto mode doesn't.
        for (mode, members_sent) in [(Approval::Code, SAS_NONCES_PER_HOUR), (Approval::Auto, 8)] {
            let hub = temp_hub();
            let fake = Arc::new(FakeCloud::default());
            let base = fake_cloud(fake.clone()).await;
            set_approval(&hub, mode).unwrap();
            add_member(&hub);
            let request = |id: String, user: Option<&str>| {
                json!({"requestId": id, "deviceKey": b64(&crypto::random::<32>()), "commit": b64(&[0u8; 32]), "userId": user,
                "deviceName": "Phone", "platform": "ios", "createdAt": 1, "expiresAt": 2})
            };
            let mut requests: Vec<Value> = (0..8).map(|i| request(format!("member_{i}"), Some("user_m"))).collect();
            requests.push(request("mine".into(), None));
            *fake.state.lock().unwrap() = json!({"owner": null, "grants": [], "requests": requests});
            pull(&hub, &base).await.unwrap();
            let posts = fake.posts.lock().unwrap();
            assert!(posts.iter().any(|(p, _)| p.contains("mine")), "the owner's own request still gets its code");
            assert_eq!(posts.iter().filter(|(p, _)| p.contains("member_")).count(), members_sent, "{mode:?}");
        }
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
            let request = |id: &str, user: &str, platform: &str| {
                json!({"requestId": id, "deviceKey": b64(&crypto::random::<32>()), "commit": b64(&[0u8; 32]), "userId": user, "platform": platform,
                "deviceName": "Phone", "createdAt": 1, "expiresAt": 2})
            };
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

    // MARK: the core decides (M2-17, KITH_CORE_AUTHORITY=1)

    /// A core on a folder of its own with the owner `kai` and `sam` in it, set as the host's authority for the test.
    struct CoreRig {
        _tmp: crate::api::authority_props::Tmp,
        daemon: Option<crate::core_front::TestDaemon>,
        _one_at_a_time: tokio::sync::MutexGuard<'static, ()>,
    }

    impl CoreRig {
        async fn new() -> Self {
            use crate::api::authority::{CoreData, TEST_CORE};
            let guard = crate::remote::account::ONE_AT_A_TIME.lock().await;
            let tmp = crate::api::authority_props::Tmp::new();
            let home = tmp.path().join("home");
            std::fs::create_dir_all(home.join("agents")).unwrap();
            let env = json!({"KITH_AGENTS": home.join("agents").to_string_lossy()});
            let core = Arc::new(CoreData::new(
                crate::api::authority_props::folders(&tmp.path().join("machine")),
                home.clone(),
                home.join("kernel"),
                env.as_object().unwrap().clone(),
            ));
            let daemon = crate::core_front::serve_for_test(&core.folders);
            for params in [json!({"id": "kai", "owner": true}), json!({"id": "sam"})] {
                let mut p = params;
                p["home"] = home.to_str().unwrap().into();
                p["kernel"] = home.join("kernel").to_str().unwrap().into();
                p["env"] = Value::Object(core.env.clone());
                crate::core_front::ask(&core.folders, "people.addPerson", &p).expect("a person in the core");
            }
            *TEST_CORE.lock().unwrap() = Some(core);
            Self { _tmp: tmp, daemon: Some(daemon), _one_at_a_time: guard }
        }

        /// The core answers again, with what it held (the same folders).
        fn restart(&mut self) {
            let core = crate::api::authority::TEST_CORE.lock().unwrap().clone().unwrap();
            self.daemon = Some(crate::core_front::serve_for_test(&core.folders));
        }

        /// The core stops answering.
        fn stop(&mut self) {
            if let Some(d) = self.daemon.take() {
                d.stop();
            }
        }
    }

    impl Drop for CoreRig {
        fn drop(&mut self) {
            *crate::api::authority::TEST_CORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            self.stop();
        }
    }

    /// A request waiting for the owner, with its code shown, as `sync_requests` holds it.
    fn waiting(id: &str, device: u8, person: Option<&str>, n: i64) -> Request {
        Request {
            id: id.into(),
            device_key: [device; 32],
            device_name: "Phone".into(),
            platform: "ios".into(),
            email: None,
            commit: [0; 32],
            host_nonce: [0; 32],
            code: Some("123456".into()),
            person: person.map(str::to_owned),
            created_at: now_ms() + n,
            expires_at: now_ms() + HOUR_MS,
        }
    }

    async fn ids_in_core(local: bool) -> Vec<String> {
        core_requests(local).await.unwrap().iter().map(|r| r["requestId"].as_str().unwrap().to_owned()).collect()
    }

    #[tokio::test]
    async fn with_the_core_the_requests_and_the_device_are_the_cores() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().extend([waiting("req_own", 1, None, 0), waiting("req_sam", 2, Some("sam"), 1)]);

        assert!(file_in_core(&hub).await, "both go in");
        assert!(!file_in_core(&hub).await, "filing again changes nothing");
        assert_eq!(ids_in_core(true).await, ["req_own", "req_sam"]);
        assert_eq!(ids_in_core(false).await, ["req_sam"], "the owner's other devices see members' only");

        // The event carries the core's list.
        let mut events = hub.events.subscribe();
        emit_requests(&hub).await;
        let event = events.try_recv().unwrap();
        assert_eq!(event["type"], "accessRequests");
        assert_eq!(event["requests"].as_array().unwrap().len(), 2);

        // A remote device can't answer the owner's own request; nothing changes.
        let err = decide_in_core(&hub, "req_own", true, false, true).await.unwrap_err();
        assert!(err.downcast_ref::<crate::api::devices::Forbidden>().is_some(), "{err:#}");
        assert_eq!(ids_in_core(true).await, ["req_own", "req_sam"]);

        // The device stored is the one the core answered, not this host's copy of the request (a different name or
        // platform in the copy would be refused first, see `refuse_mismatch`).
        hub.cloud.requests.locked().iter_mut().for_each(|r| r.email = Some("copy@example.com".into()));
        decide_in_core(&hub, "req_sam", true, false, true).await.unwrap();
        let d = hub.store.device(&b64(&[2; 32])).unwrap();
        assert_eq!(
            (d.name.as_str(), d.person_id.as_deref(), d.scopes.clone()),
            ("Phone", Some("sam"), vec![Scope::Control])
        );
        assert_eq!(d.grant_id.as_deref(), Some("grt_new"));
        assert!(hub.store.device_proven(&b64(&[2; 32])), "the owner's yes stands in for the invite token");
        decide_in_core(&hub, "req_own", true, true, true).await.unwrap();
        let d = hub.store.device(&b64(&[1; 32])).unwrap();
        assert_eq!(d.scopes, [Scope::Control, Scope::Screen], "the owner's own device may share the screen");
        assert_eq!(ids_in_core(true).await, Vec::<String>::new());
        assert!(hub.cloud.requests_json().is_empty(), "this host forgets what the core ended");
        // Decided is decided, from either side.
        let again = decide_in_core(&hub, "req_sam", false, true, true).await.unwrap_err();
        assert_eq!(again.to_string(), "unknown request");
        drop(rig);
    }

    #[tokio::test]
    async fn with_the_core_a_denial_tells_the_cloud_and_a_web_browser_is_never_the_owners_device() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        let mut web = waiting("req_web", 3, None, 0);
        web.platform = "web".into();
        hub.cloud.requests.locked().extend([waiting("req_no", 1, Some("sam"), 1), web]);
        file_in_core(&hub).await;

        decide_in_core(&hub, "req_no", false, true, true).await.unwrap();
        assert!(hub.store.device(&b64(&[1; 32])).is_none());
        let denied = |id: &str| {
            fake.posts
                .lock()
                .unwrap()
                .iter()
                .any(|(p, b)| p.ends_with(&format!("{id}/decision")) && b["decision"] == "deny")
        };
        assert!(denied("req_no"));

        let err = decide_in_core(&hub, "req_web", true, true, true).await.unwrap_err();
        assert_eq!(err.to_string(), "A web browser can't be the owner's device.");
        assert!(denied("req_web"), "the cloud is told, as `settle` does");
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());
        assert_eq!(ids_in_core(true).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn with_the_core_down_nothing_is_decided_and_nothing_falls_back() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let mut rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().push(waiting("req_sam", 2, Some("sam"), 0));
        file_in_core(&hub).await;
        rig.stop();

        let err = decide_in_core(&hub, "req_sam", true, true, true).await.unwrap_err();
        assert!(err.downcast_ref::<crate::api::authority::CoreDown>().is_some(), "{err:#}");
        assert!(core_requests(true).await.is_err(), "the listing is an error, not this host's copy");
        assert!(hub.store.device(&b64(&[2; 32])).is_none());
        assert_eq!(hub.cloud.requests_json().len(), 1, "the request is still held");
        assert!(fake.posts.lock().unwrap().is_empty(), "the cloud was not asked");
    }

    #[tokio::test]
    async fn a_person_with_no_membership_has_their_requests_denied_every_pass_in_the_core_too() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let mut rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().extend([waiting("req_own", 1, None, 0), waiting("req_sam", 2, Some("sam"), 1)]);
        file_in_core(&hub).await;
        deny_without_membership(&hub).await;
        assert_eq!(
            ids_in_core(true).await,
            ["req_own", "req_sam"],
            "a member keeps theirs while they have a membership"
        );

        // The cloud reports the membership gone; the person stays in the core (and in kith-auth).
        hub.store.remove_membership("mem_m").unwrap();
        rig.stop();
        deny_without_membership(&hub).await;
        assert_eq!(hub.cloud.requests_json().len(), 2, "the core can't be told, so the request is kept to try again");
        // A core that is back is told on the next pass: simulate it by one that never saw the request.
        drop(rig);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        file_in_core(&hub).await;
        deny_without_membership(&hub).await;
        assert_eq!(ids_in_core(true).await, ["req_own"]);
        assert_eq!(hub.cloud.requests_json().len(), 1);
        assert!(
            fake.posts.lock().unwrap().iter().any(|(p, b)| p.ends_with("req_sam/decision") && b["decision"] == "deny")
        );
    }

    #[tokio::test]
    async fn without_the_core_a_person_with_no_membership_is_denied_too_and_nothing_is_filed() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        hub.cloud.requests.locked().extend([waiting("req_own", 1, None, 0), waiting("req_sam", 2, Some("sam"), 1)]);
        assert!(!file_in_core(&hub).await, "the switch is off: the core is not asked");
        deny_without_membership(&hub).await;
        assert_eq!(hub.cloud.requests_json().len(), 1);
        assert_eq!(hub.cloud.requests_json()[0]["requestId"], "req_own");
        assert!(
            fake.posts.lock().unwrap().iter().any(|(p, b)| p.ends_with("req_sam/decision") && b["decision"] == "deny")
        );
    }

    #[tokio::test]
    async fn with_the_core_a_suspension_made_through_the_host_stops_the_approval() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().push(waiting("req_sam", 2, Some("sam"), 0));
        file_in_core(&hub).await;
        // The host's revoke moves the membership; the core's own table does not see it.
        hub.store.suspend_person("sam").unwrap();

        let err = decide_in_core(&hub, "req_sam", true, true, true).await.unwrap_err();
        assert_eq!(err.to_string(), "That account is no longer a member of this computer.");
        assert!(hub.store.device(&b64(&[2; 32])).is_none());
        assert!(
            fake.posts.lock().unwrap().iter().any(|(p, b)| p.ends_with("req_sam/decision") && b["decision"] == "deny")
        );
        assert_eq!(ids_in_core(true).await, Vec::<String>::new(), "ended in the core too");
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());
    }

    #[tokio::test]
    async fn with_the_core_a_row_filed_first_by_someone_else_is_never_stored_as_the_hosts_device() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        // A process of the owner's user files the ids first: one with another device key, one as the owner's own
        // request (no person) for the member's phone.
        let t = now_ms();
        // And one with the host's key and person but another platform: a web device must not be stored as a phone.
        for (id, key, person, platform) in [
            ("req_key", [9u8; 32], Some("sam"), "ios"),
            ("req_person", [3; 32], None, "ios"),
            ("req_plat", [4; 32], Some("sam"), "android"),
        ] {
            let filed = json!({
                "id": id, "deviceKey": b64(&key), "deviceName": "Phone", "platform": platform, "person": person,
                "createdAt": t, "expiresAt": t + HOUR_MS, "code": "123456",
            });
            crate::api::authority::owner_call("people.fileRequest", filed, false).await.unwrap();
        }
        hub.cloud.requests.locked().extend([
            waiting("req_key", 2, Some("sam"), 0),
            waiting("req_person", 3, Some("sam"), 1),
            waiting("req_plat", 4, Some("sam"), 2),
        ]);
        // The host's own filing is refused by the core (and logged), not merged into the other row.
        assert!(!file_in_core(&hub).await);

        for id in ["req_key", "req_person", "req_plat"] {
            let err = decide_in_core(&hub, id, true, true, true).await.unwrap_err();
            assert!(err.to_string().contains("doesn't match what this computer was asked"), "{id}: {err:#}");
            assert!(
                fake.posts
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(p, b)| p.ends_with(&format!("{id}/decision")) && b["decision"] == "deny"),
                "{id}: the cloud is told no"
            );
        }
        assert!(hub.store.devices().unwrap().iter().all(|d| d.source != DeviceSource::Account));
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());
    }

    #[tokio::test]
    async fn with_the_core_the_hosts_own_checks_come_before_the_core_is_asked() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let mut rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        let mut unshown = waiting("req_sam", 2, Some("sam"), 0);
        unshown.code = None;
        hub.cloud.requests.locked().push(unshown);
        // A row only the core holds (filed by something else): the host does not hold it, so it can't be decided here.
        let t = now_ms();
        let filed = json!({
            "id": "req_core", "deviceKey": b64(&[7; 32]), "deviceName": "Phone", "platform": "ios", "person": "sam",
            "createdAt": t, "expiresAt": t + HOUR_MS, "code": "123456",
        });
        crate::api::authority::owner_call("people.fileRequest", filed, false).await.unwrap();
        rig.stop();

        // With the core down, none of these reaches it: each is answered by the host's own check.
        let err = decide_in_core(&hub, "req_core", true, true, true).await.unwrap_err();
        assert_eq!(err.to_string(), "unknown request");
        let err = decide_in_core(&hub, "req_core", false, true, true).await.unwrap_err();
        assert_eq!(err.to_string(), "unknown request");
        let err = decide_in_core(&hub, "req_sam", true, true, true).await.unwrap_err();
        assert_eq!(err.to_string(), "The device hasn't shown its code yet.");
        assert!(err.downcast_ref::<Conflict>().is_some());
        assert!(fake.posts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn with_the_core_a_request_the_cloud_stops_listing_or_the_host_denies_ends_in_the_core_too() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().extend([waiting("req_own", 1, None, 0), waiting("req_sam", 2, Some("sam"), 1)]);
        file_in_core(&hub).await;
        assert_eq!(ids_in_core(true).await, ["req_own", "req_sam"]);

        // The cloud lists only req_own, and with a nonce this host never sent: the host denies that one itself.
        let dk = crypto::unb64_n::<32>(&b64(&[1; 32])).unwrap();
        let listed: HostAccessRequest =
            serde_json::from_value(member_request("req_own", dk, [0; 32], Some(&b64(&[5; 32])), None)).unwrap();
        sync_requests(&hub, &base, vec![listed], None).await;
        assert_eq!(ids_in_core(true).await, Vec::<String>::new(), "the vanished one and the denied one are both gone");
    }

    #[tokio::test]
    async fn with_the_core_an_end_the_core_missed_stays_queued_and_is_made_on_the_next_sync() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let mut rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().extend([waiting("req_own", 1, None, 0), waiting("req_sam", 2, Some("sam"), 1)]);
        file_in_core(&hub).await;

        // The cloud stops listing both while the core is down: the host drops them, the core can't be told.
        rig.stop();
        sync_requests(&hub, &base, vec![], None).await;
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());
        assert_eq!(take_deny_queue(&hub), ["req_own", "req_sam"], "kept to end next sync");

        // The core is back and still holds both; the next sync (with the cloud still listing nothing) ends them.
        rig.restart();
        assert_eq!(ids_in_core(true).await, ["req_own", "req_sam"]);
        sync_requests(&hub, &base, vec![], None).await;
        assert_eq!(ids_in_core(true).await, Vec::<String>::new());
        assert_eq!(take_deny_queue(&hub), Vec::<String>::new());
        // Sync running again with nothing left is a no-op.
        sync_requests(&hub, &base, vec![], None).await;
        assert_eq!(take_deny_queue(&hub), Vec::<String>::new());
    }

    #[tokio::test]
    async fn with_the_switch_off_nothing_is_queued_for_the_core() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        hub.cloud.requests.locked().push(waiting("req_own", 1, None, 0));
        sync_requests(&hub, &base, vec![], None).await;
        assert_eq!(hub.cloud.requests_json(), Vec::<Value>::new());
        assert_eq!(take_deny_queue(&hub), Vec::<String>::new());
    }

    #[tokio::test]
    async fn with_the_core_a_decision_past_its_budget_is_not_made() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().push(waiting("req_sam", 2, Some("sam"), 0));
        file_in_core(&hub).await;

        // The budget is already spent: the core call is given up on, nothing is decided, and the request is as it was.
        let spent = Some(tokio::time::Instant::now());
        let err = decide_in_core_until(&hub, "req_sam", true, true, false, spent).await.unwrap_err();
        assert!(err.to_string().contains("budget"), "{err:#}");
        assert!(fake.posts.lock().unwrap().is_empty());
        assert_eq!(ids_in_core(true).await, ["req_sam"], "still pending in the core");
        assert_eq!(hub.cloud.requests_json().len(), 1);
    }

    #[tokio::test]
    async fn a_deny_queued_while_the_pull_ends_the_queue_survives_its_write_back() {
        let hub = temp_hub();
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        queue_for_core(&hub, [&"req_old".to_owned()]);
        // `deny_or_queue` runs between the pull's read of the queue and its write-back, with the core refusing the
        // call (unknown request is fine) only for req_old; req_new reaches the queue because the core is unreachable.
        let late = async {
            queue_for_core(&hub, [&"req_new".to_owned()]);
        };
        end_queued_in_core(&hub, late).await;
        assert_eq!(take_deny_queue(&hub), ["req_new"], "req_old is ended, req_new was not lost");
    }

    #[tokio::test]
    async fn the_gate_flipping_between_filing_and_deciding_decides_a_request_once() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        add_member(&hub);
        let _rig = CoreRig::new().await;
        hub.cloud.requests.locked().extend([waiting("req_a", 1, Some("sam"), 0), waiting("req_b", 2, Some("sam"), 1)]);

        // On when filed, off when decided: the host store decides (`settle`) and the device is stored once. The
        // core's row is only a mirror; it stays pending there and is not what the person's access reads.
        hub.store.kv_set("test_core_authority", "1").unwrap();
        file_in_core(&hub).await;
        hub.store.kv_delete("test_core_authority").unwrap();
        assert!(!core_on(&hub));
        decide(&hub, "req_a", true).await.unwrap();
        assert_eq!(hub.store.devices().unwrap().iter().filter(|d| d.key == b64(&[1; 32])).count(), 1);
        assert!(hub.cloud.requests_json().iter().all(|r| r["requestId"] != "req_a"));
        let again = decide(&hub, "req_a", true).await.unwrap_err();
        assert!(again.to_string().contains("unknown request"), "{again:#}");
        assert_eq!(hub.store.devices().unwrap().iter().filter(|d| d.key == b64(&[1; 32])).count(), 1);

        // Off when filed (nothing is in the core), on when decided: the core refuses what it never saw, nothing is
        // stored, and the request waits; the next pull files it and the decision then goes through once.
        hub.store.kv_set("test_core_authority", "1").unwrap();
        let err = decide_in_core(&hub, "req_b", true, true, true).await.unwrap_err();
        assert!(err.to_string().contains("unknown request"), "{err:#}");
        assert!(hub.store.device(&b64(&[2; 32])).is_none(), "nothing was stored by the refused decision");
        file_in_core(&hub).await;
        decide_in_core(&hub, "req_b", true, true, true).await.unwrap();
        assert!(hub.store.device(&b64(&[2; 32])).is_some());
        assert!(decide_in_core(&hub, "req_b", true, true, true).await.is_err());
    }

    #[tokio::test]
    async fn with_the_core_a_pull_cut_off_after_it_dropped_a_request_has_already_queued_it() {
        let hub = temp_hub();
        let fake = Arc::new(FakeCloud::default());
        let base = fake_cloud(fake.clone()).await;
        hub.store.kv_set("test_cloud_url", &base).unwrap();
        let _rig = CoreRig::new().await;
        hub.store.kv_set("test_core_authority", "1").unwrap();
        hub.cloud.requests.locked().push(waiting("req_own", 1, None, 0));
        file_in_core(&hub).await;

        // The relay drops a pull that waits too long: here it is dropped at its first wait, after the host forgot the
        // request and before the core was told.
        let cut = tokio::time::timeout(Duration::ZERO, sync_requests(&hub, &base, vec![], None)).await;
        assert!(cut.is_err(), "the pull was cut off");
        assert_eq!(take_deny_queue(&hub), ["req_own"]);
        // The next sync ends it.
        sync_requests(&hub, &base, vec![], None).await;
        assert_eq!(ids_in_core(true).await, Vec::<String>::new());
        assert_eq!(take_deny_queue(&hub), Vec::<String>::new());
    }
}
