//! Invited people, from the cloud's side: which accounts it lists as members of this computer. The cloud only records
//! who accepted which invite; this host decides. It honours a membership only when its invite is one this host made
//! and still has open, binds the account to that invite's kith-auth person, and drops the person's devices when the
//! cloud stops listing the membership.

use super::account;
use crate::hub::Hub;
use crate::store::{Membership, Store, now_ms};
use anyhow::{Context, Result};
use kith_wire::cloud::path;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// kv: membership ids the cloud has not yet confirmed as revoked (JSON array).
const PENDING_REVOKES: &str = "membership_revokes";

/// One active membership of `GET /v1/host/state`: the cloud contract's `Membership`.
pub use kith_wire::cloud::Membership as Listed;

/// Applies the cloud's membership list. `None` (an older cloud that doesn't list them) changes nothing. Never fails
/// the pull: a problem is logged and the next pull tries again.
pub async fn sync(hub: &Hub, listed: Option<&[Listed]>, owner_uid: Option<&str>) {
    let bin = account::bin().await;
    if let Some(listed) = listed
        && let Err(e) = apply(hub, bin.as_deref(), listed, owner_uid).await
    {
        tracing::warn!(error = format!("{e:#}"), "couldn't apply the cloud's memberships");
    }
    // Every pass, whatever changed (or whether the cloud lists memberships at all): a request of a person with no
    // membership here is dead.
    super::cloud::deny_without_membership(hub).await;
    if listed.is_none() {
        return;
    }
    if let Some(bin) = bin.as_deref() {
        forget_removed(hub, bin).await;
    }
    refresh_mirror(hub).await;
    if let Err(e) = hub.store.audit_prune(now_ms()) {
        tracing::warn!(error = format!("{e:#}"), "couldn't prune the audit log");
    }
}

/// A person removed while revoked (kith-auth `--rm`, or the owner dropping them from the list) no longer exists to be
/// undone: what the revoke kept (device keys, invite token hashes) goes with them, whichever way they were removed.
async fn forget_removed(hub: &Hub, bin: &Path) {
    let Ok(kept) = hub.store.suspended_people() else { return };
    if kept.is_empty() {
        return;
    }
    // Only when kith-auth answers: a failed read must not look like everyone gone.
    let Ok(people) = auth_people(bin).await else { return };
    for gone in kept.iter().filter(|k| !people.iter().any(|p| &p.id == *k)) {
        tracing::info!(person = gone, "a removed person's kept access goes");
        if let Err(e) = hub.store.forget_suspended(gone) {
            tracing::warn!(error = format!("{e:#}"), person = gone, "couldn't forget what a removed person kept");
        }
    }
}

/// A kith-auth change that rewrites auth.json, one writer at a time with the owner's own changes (`AUTH_WRITES`).
/// Callers here never hold the lock.
pub(crate) async fn locked_run(bin: &Path, args: &[&str]) -> Result<String> {
    // A switched host sends it to the core (`people.handle`) and never runs kith-auth on the tombstone.
    let _w = crate::api::people::AUTH_WRITES.lock().await;
    crate::api::authority::run_auth(bin, args).await
}

/// A Kith account id as kith-auth takes it. Anything else the cloud sends is not an account.
fn valid_user_id(id: &str) -> bool {
    id.strip_prefix("user_").is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

pub async fn apply(hub: &Hub, bin: Option<&Path>, listed: &[Listed], owner_uid: Option<&str>) -> Result<()> {
    let known = hub.store.memberships()?;
    for m in listed.iter().filter(|m| !known.iter().any(|k| k.membership_id == m.membership_id)) {
        if !valid_user_id(&m.user_id) || owner_uid == Some(m.user_id.as_str()) {
            tracing::warn!(
                membership = m.membership_id,
                "ignoring a membership with an invalid or the owner's account"
            );
            continue;
        }
        let Some(invite) = hub.store.invite(&m.invite_id).filter(|i| i.open(now_ms())) else {
            // Not ours (or used up, revoked, expired): nothing here grants it, so it stays a name in the cloud.
            tracing::debug!(membership = m.membership_id, "ignoring a membership this computer didn't invite");
            continue;
        };
        if hub.store.membership_of_user(&m.user_id).is_some() {
            tracing::warn!(membership = m.membership_id, "this account is already a member under another invite");
            continue;
        }
        let Some(bin) = bin else {
            tracing::warn!("kith-auth not found; can't bind an invited account to its person");
            continue;
        };
        if let Err(e) = locked_run(bin, &["handle", &invite.person_id, "kith", &m.user_id]).await {
            tracing::warn!(error = format!("{e:#}"), person = invite.person_id, "couldn't bind the invited account");
            continue;
        }
        if !hub.store.use_invite(&invite.invite_id, now_ms())? {
            // Revoked or used between the check and now.
            locked_run(bin, &["handle", &invite.person_id, "kith", "--rm"]).await.ok();
            continue;
        }
        hub.store.add_membership(&Membership {
            membership_id: m.membership_id.clone(),
            user_id: m.user_id.clone(),
            person_id: invite.person_id.clone(),
            invite_id: invite.invite_id,
            created_at: now_ms(),
        })?;
        tracing::info!(person = invite.person_id, "an invited account joined");
    }
    for gone in known.iter().filter(|k| !listed.iter().any(|m| m.membership_id == k.membership_id)) {
        tracing::info!(person = gone.person_id, "membership gone from the cloud; removing the person's devices");
        hub.store.remove_membership(&gone.membership_id)?;
        end_access(hub, bin, &gone.person_id).await?;
    }
    Ok(())
}

/// Ends a person's access once a membership of theirs is gone (and already removed from the store). With no membership
/// left, every device goes and the account is unbound; with another still active, its handle is kept (pointed at it) and
/// the gone account's devices fall with their grants. Their agents' memberships and memory stay.
pub async fn end_access(hub: &Hub, bin: Option<&Path>, person: &str) -> Result<()> {
    let remaining: Vec<Membership> = hub.store.memberships()?.into_iter().filter(|m| m.person_id == person).collect();
    let Some(still) = remaining.first() else {
        for d in hub.store.devices_of(person)? {
            hub.revoke_device(&d.key)?;
        }
        // A revoked person keeps their account handle and their bots: an undo, here or on another Mac, finds them by
        // the handle, and their bots are dormant (kith-auth refuses them through their owner), not ended.
        if let Some(bin) = bin
            && revoked_here(bin, person).await
        {
            return Ok(());
        }
        // A removal (not a revoke) is final: what a revoke would have kept goes, and so does everything of theirs that is
        // still running or owed (R5).
        hub.store.forget_suspended(person)?;
        crate::api::people::sweep_asks(hub, &[person.to_owned()]).await;
        crate::api::botguests::sweep_claims(hub, vec![person.to_owned()], false).await;
        // Their bot guests lose their owner with the last membership.
        crate::api::botguests::revoke_owned_by(hub, person, "person").await;
        if let Some(bin) = bin
            && let Err(e) = locked_run(bin, &["handle", person, "kith", "--rm"]).await
        {
            tracing::warn!(error = format!("{e:#}"), person, "couldn't remove an invited account's handle");
        }
        return Ok(());
    };
    if let Some(bin) = bin
        && let Err(e) = locked_run(bin, &["handle", person, "kith", &still.user_id]).await
    {
        tracing::warn!(error = format!("{e:#}"), person, "couldn't keep an invited account's handle");
    }
    Ok(())
}

/// One person of `kith-auth people --json`, as far as a revoke goes.
#[derive(Deserialize, Debug)]
pub struct AuthPerson {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: bool,
    #[serde(default)]
    pub bot_of: String,
    /// Their Kith account, the one name a person has on every Mac of the owner; empty for one known only here.
    #[serde(default)]
    pub kith: String,
    /// Their chat handles (`telegram` -> id) other than a Kith account.
    #[serde(default)]
    pub handles: std::collections::BTreeMap<String, String>,
    /// When they themselves were revoked (not merely the owner of a revoked bot's).
    #[serde(default)]
    pub revoked_at: Option<i64>,
    /// The agents they hold a membership row for, as the rows stand (a revoked person's too).
    #[serde(default)]
    pub agents: Vec<AuthAgent>,
}

/// One membership row of a person in `kith-auth people --json`: `via` is `own`, or `everyone` for the `*` row.
#[derive(Deserialize, Debug)]
pub struct AuthAgent {
    pub agent: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub via: String,
}

pub async fn auth_people(bin: &Path) -> Result<Vec<AuthPerson>> {
    let out = crate::api::authority::run_auth(bin, &["people", "--json"]).await?;
    serde_json::from_str(&out).context("reading kith-auth people --json")
}

/// [`auth_people`] as the owner's access stands now. With `KITH_CORE_AUTHORITY=1` the core holds the membership rows
/// (M2-17), so each person's `agents` come from
/// `people.list` and kith-auth only says who the people are. A core that can't be asked is an error, never a list that
/// reads as the old file.
pub async fn auth_people_now(bin: &Path) -> Result<Vec<AuthPerson>> {
    let mut people = auth_people(bin).await?;
    if crate::api::authority::guard_on_core() {
        let rows = crate::api::authority::owner_call("people.list", json!({}), true).await?;
        let rows = rows["rows"].as_array().cloned().unwrap_or_default();
        let agent = |r: &serde_json::Value| AuthAgent {
            agent: r["agent"].as_str().unwrap_or_default().to_owned(),
            role: r["preset"].as_str().unwrap_or_default().to_owned(),
            via: r["via"].as_str().unwrap_or_default().to_owned(),
        };
        for p in &mut people {
            // The person's own rows, and the `*` row for everyone, as kith-auth lists them.
            p.agents =
                rows.iter().filter(|r| r["via"] == "everyone" || r["person"] == p.id.as_str()).map(agent).collect();
            // The `*` row first, so a person's own row for the same agent is the one the mirror keeps.
            p.agents.sort_by_key(|a| a.via != "everyone");
        }
    }
    Ok(people)
}

/// Makes the store's copy of each person's own agents (`PersonRow.agents`) what kith-auth says, so a membership added or
/// removed with `kith-auth member` shows in the one sync reader of it, `roles::event_for`. kith-auth wins: a person
/// kith-auth no longer lists holds nothing. A revoked person's copy is left as it stands (their rows stand for the
/// undo, and a revoke already cuts them off whatever the copy says). Only called with a list kith-auth really gave.
pub fn mirror_agents(store: &Store, auth: &[AuthPerson]) {
    let Ok(stored) = store.people() else { return };
    for row in stored {
        let want: std::collections::BTreeMap<String, String> = match auth.iter().find(|a| a.id == row.person_id) {
            Some(a) if a.revoked_at.is_some() => continue,
            // The `*` row too: `startChat` asks kith-auth, which honours it, so the one sync reader of this copy
            // (`roles::event_for`) must let a `*` agent's events through. kith-auth never lists a built-in agent in
            // `agents` (`agent_rows` skips them), and the `*` row never reaches a bot, so neither appears here.
            Some(a) => a.agents.iter().map(|r| (r.agent.clone(), r.role.clone())).collect(),
            None => std::collections::BTreeMap::new(),
        };
        if want != row.agents
            && let Err(e) = store.set_person_agents(&row.person_id, &want)
        {
            tracing::warn!(error = format!("{e:#}"), person = row.person_id, "couldn't mirror kith-auth's agents");
        }
    }
}

/// Reads kith-auth and mirrors it (see [`mirror_agents`]); a failed read changes nothing. Read and write happen under
/// `AUTH_WRITES`, as every other change of kith-auth's rows and this copy does: otherwise a `setPersonAccess` landing
/// between the read and the write would be overwritten by the older list.
pub async fn refresh_mirror(hub: &Hub) {
    let Some(bin) = account::bin().await else { return };
    let _w = crate::api::people::AUTH_WRITES.lock().await;
    if let Ok(auth) = auth_people_now(&bin).await {
        mirror_agents(&hub.store, &auth);
    }
}

/// How often the mirror is brought back in step with kith-auth, for a change made with the CLI. A member removed there
/// can hear events of their own chat for this long at most.
const MIRROR_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

pub fn start_mirror(hub: &std::sync::Arc<Hub>) {
    if cfg!(test) {
        return;
    }
    let hub = hub.clone();
    tokio::spawn(async move {
        loop {
            refresh_mirror(&hub).await;
            tokio::time::sleep(MIRROR_EVERY).await;
        }
    });
}

/// The kith-auth person `id`, or None when there is no such person.
pub async fn auth_person(bin: &Path, id: &str) -> Result<Option<AuthPerson>> {
    Ok(auth_people(bin).await?.into_iter().find(|p| p.id == id))
}

/// Applies the cloud's list of accounts this computer's owner revoked, on any of their Macs (R6): a person here whose
/// Kith account is listed gets the revoked state in kith-auth, and one revoked here whose account is no longer listed
/// gets it cleared (undone on another Mac). A person with no Kith account is this Mac's alone and is left as it is.
/// Runs before the memberships, so a membership the revoke cut finds its person already revoked (and keeps their handle
/// and bots for the undo). Returns the people newly revoked here, whose running turns the caller stops. None (a cloud
/// that predates revoking) changes nothing. An error fails the pull: this host stays out of sync rather than serve on
/// access it could not apply (I2).
pub async fn apply_revoked(bin: Option<&Path>, listed: Option<&[String]>) -> Result<Revoked> {
    let (Some(bin), Some(listed)) = (bin, listed) else { return Ok(Revoked::default()) };
    let mut out = Revoked::default();
    for p in auth_people(bin).await?.into_iter().filter(|p| !p.kith.is_empty() && !p.owner && p.bot_of.is_empty()) {
        let revoked = listed.contains(&p.kith);
        if revoked == p.revoked_at.is_some() {
            continue;
        }
        let flag = if revoked { "--revoke" } else { "--unrevoke" };
        // Each person is logged on their own (a pull can change several): a core that missed the change is said here, once
        // per person, because kith-auth exits 0 for it.
        let said = {
            let _w = crate::api::people::AUTH_WRITES.lock().await;
            crate::api::authority::run_auth_noted(bin, &["person", &p.id, flag, "--local"]).await
        }
        .with_context(|| format!("kith-auth person {} {flag}", p.id))?
        .1;
        account::note_stderr(&p.id, &said);
        tracing::info!(person = p.id, revoked, "applied the owner's revoke from the cloud");
        if revoked { out.cut.push(p.id) } else { out.restored.push(p.id) }
    }
    Ok(out)
}

/// The people [`apply_revoked`] changed here.
#[derive(Default)]
pub struct Revoked {
    /// Newly revoked: the caller cuts them off.
    pub cut: Vec<String>,
    /// Undone on another Mac: the caller gives back what the revoke kept.
    pub restored: Vec<String>,
}

/// Whether `person` is revoked here (their own state). False when kith-auth can't say: the caller then does what it
/// did before revoking existed.
async fn revoked_here(bin: &Path, person: &str) -> bool {
    auth_person(bin, person).await.ok().flatten().is_some_and(|p| p.revoked_at.is_some())
}

fn pending(store: &Store) -> Vec<String> {
    store.kv_get(PENDING_REVOKES).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

fn set_pending(store: &Store, ids: &[String]) {
    let saved = serde_json::to_string(ids).map_err(anyhow::Error::from).and_then(|v| store.kv_set(PENDING_REVOKES, &v));
    if let Err(e) = saved {
        tracing::warn!(error = format!("{e:#}"), "couldn't remember a membership to revoke");
    }
}

/// Tells the cloud of memberships that ended here before a revoke kept them (earlier versions queued these); a failure
/// is retried on later pulls.
pub async fn retry_revokes(hub: &Hub) {
    let ids = pending(&hub.store);
    if ids.is_empty() {
        return;
    }
    let mut left = vec![];
    for id in ids {
        match super::cloud::post(hub, &path::host_revoke_membership(&id), &json!({})).await {
            Ok(_) => {}
            Err(e) if super::cloud::refused_with(&e, 404) => {}
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), membership = id, "couldn't revoke a membership in the cloud");
                left.push(id);
            }
        }
    }
    set_pending(&hub.store, &left);
}
