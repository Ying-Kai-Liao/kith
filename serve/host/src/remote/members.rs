//! Invited people, from the cloud's side: which accounts it lists as members of this computer. The cloud only records
//! who accepted which invite; this host decides. It honours a membership only when its invite is one this host made
//! and still has open, binds the account to that invite's kith-auth person, and drops the person's devices when the
//! cloud stops listing the membership.

use super::account;
use crate::LockExt;
use crate::hub::Hub;
use crate::store::{Membership, Store, now_ms};
use anyhow::{Context, Result};
use kith_wire::cloud::path;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::sync::Mutex;

/// kv: membership ids the cloud has not yet confirmed as revoked (JSON array).
const PENDING_REVOKES: &str = "membership_revokes";

/// One active membership of `GET /v1/host/state`: the cloud contract's `Membership`.
pub use kith_wire::cloud::Membership as Listed;

/// Applies the cloud's membership list. `None` (an older cloud that doesn't list them) changes nothing. Never fails
/// the pull: a problem is logged and the next pull tries again.
pub async fn sync(hub: &Hub, listed: Option<&[Listed]>, owner_uid: Option<&str>) {
    let Some(listed) = listed else { return };
    let bin = account::bin().await;
    if let Err(e) = apply(hub, bin.as_deref(), listed, owner_uid).await {
        tracing::warn!(error = format!("{e:#}"), "couldn't apply the cloud's memberships");
    }
    if let Some(bin) = bin.as_deref() {
        forget_removed(hub, bin).await;
    }
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
async fn locked_run(bin: &Path, args: &[&str]) -> Result<String> {
    let _w = crate::api::people::AUTH_WRITES.lock().await;
    account::run(bin, args).await
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
}

pub async fn auth_people(bin: &Path) -> Result<Vec<AuthPerson>> {
    let out = account::run(bin, &["people", "--json"]).await?;
    serde_json::from_str(&out).context("reading kith-auth people --json")
}

/// The kith-auth person `id`, or None when there is no such person.
pub async fn auth_person(bin: &Path, id: &str) -> Result<Option<AuthPerson>> {
    Ok(auth_people(bin).await?.into_iter().find(|p| p.id == id))
}

/// Applies the cloud's list of accounts this computer's owner revoked, on any of their Macs (R6): a person here whose
/// Kith account is listed gets the revoked state in kith-auth, and one revoked here whose account is no longer listed
/// gets it cleared (undone on another Mac). A person with no Kith account is this Mac's alone and is left as it is.
/// Runs before the memberships, so a membership the revoke cut finds its person already revoked (and keeps their handle
/// and bots for the undo). Every invited person's devices, memberships and open invites are then parked or given back to
/// match their revoked state, flipped here or not, in the same hold of the kith-auth lock as the flip: what a failed
/// pull or a race with the owner's own revoke left half done is put right by the next pull (architect's RevokePark
/// model, fix). Returns the people newly revoked here, whose running turns the caller stops. None (a cloud that
/// predates revoking) changes nothing. An error fails the pull: this host stays out of sync rather than serve on access
/// it could not apply (I2).
pub async fn apply_revoked(hub: &Hub, bin: Option<&Path>, listed: Option<&[String]>) -> Result<Vec<String>> {
    let (Some(bin), Some(listed)) = (bin, listed) else { return Ok(vec![]) };
    let mut cut = vec![];
    let mut moved = false;
    let applied: Result<()> = async {
        let _w = crate::api::people::AUTH_WRITES.lock().await;
        for p in auth_people(bin).await?.into_iter().filter(|p| !p.owner && p.bot_of.is_empty()) {
            let mut revoked = p.revoked_at.is_some();
            // A person with no Kith account is this Mac's alone: their state is left as it is, only parked to match.
            if !p.kith.is_empty() && listed.contains(&p.kith) != revoked {
                revoked = !revoked;
                let flag = if revoked { "--revoke" } else { "--unrevoke" };
                account::run(bin, &["person", &p.id, flag, "--local"])
                    .await
                    .with_context(|| format!("kith-auth person {} {flag}", p.id))?;
                tracing::info!(person = p.id, revoked, "applied the owner's revoke from the cloud");
                if revoked {
                    cut.push(p.id.clone());
                }
            }
            moved |= crate::api::people::park(hub, &p.id, revoked);
        }
        Ok(())
    }
    .await;
    // Also when a later flip failed: what was parked before it already stopped granting.
    if moved {
        hub.auth_changed();
    }
    applied.map(|()| cut)
}

/// Whether `person` is revoked here (their own state). False when kith-auth can't say: the caller then does what it
/// did before revoking existed.
async fn revoked_here(bin: &Path, person: &str) -> bool {
    auth_person(bin, person).await.ok().flatten().is_some_and(|p| p.revoked_at.is_some())
}

fn pending(store: &Store) -> Vec<String> {
    store.kv_get(PENDING_REVOKES).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Changes the queue in one step: it is read again here, so a membership queued while a retry was awaiting the cloud
/// is kept, and no `.await` sits between the read and the write (same shape as cloud.rs `update_pending_revokes`).
fn update_pending(store: &Store, change: impl FnOnce(&mut Vec<String>)) {
    static WRITING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = WRITING.locked();
    let mut ids = pending(store);
    change(&mut ids);
    let saved =
        serde_json::to_string(&ids).map_err(anyhow::Error::from).and_then(|v| store.kv_set(PENDING_REVOKES, &v));
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
    let mut confirmed = vec![];
    for id in ids {
        match super::cloud::post(hub, &path::host_revoke_membership(&id), &json!({})).await {
            Ok(_) => confirmed.push(id),
            Err(e) if super::cloud::refused_with(&e, 404) => confirmed.push(id),
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), membership = id, "couldn't revoke a membership in the cloud");
            }
        }
    }
    // Only what the cloud confirmed leaves the queue: an id queued during the awaits above stays.
    update_pending(&hub.store, |ids| ids.retain(|i| !confirmed.contains(i)));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::remote::identity::Identity;
    use axum::response::IntoResponse;
    use std::sync::Arc;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct Gate {
        entered: Notify,
        release: Notify,
        first: std::sync::atomic::AtomicBool,
    }

    /// A cloud that holds its first revoke call until released and refuses every other.
    async fn gated_cloud(gate: Arc<Gate>) -> String {
        use axum::extract::State as Ax;
        use axum::http::StatusCode;
        use std::sync::atomic::Ordering;
        let app = axum::Router::new()
            .route(
                "/v1/host/memberships/{id}/revoke",
                axum::routing::post(|Ax(g): Ax<Arc<Gate>>| async move {
                    if g.first.swap(true, Ordering::SeqCst) {
                        return (StatusCode::INTERNAL_SERVER_ERROR, "no").into_response();
                    }
                    g.entered.notify_one();
                    g.release.notified().await;
                    axum::Json(json!({})).into_response()
                }),
            )
            .with_state(gate);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    #[tokio::test]
    async fn a_membership_queued_during_a_retry_is_kept() {
        let dir = std::env::temp_dir().join(format!("kith-members-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let hub = Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0);
        let gate = Arc::new(Gate::default());
        hub.store.kv_set("test_cloud_url", &gated_cloud(gate.clone()).await).unwrap();
        update_pending(&hub.store, |ids| ids.push("mem_a".into()));

        let retry = tokio::spawn({
            let hub = hub.clone();
            async move { retry_revokes(&hub).await }
        });
        gate.entered.notified().await;
        update_pending(&hub.store, |ids| ids.push("mem_b".into()));
        gate.release.notify_one();
        retry.await.unwrap();

        assert_eq!(pending(&hub.store), ["mem_b"], "the confirmed id is gone, the one queued meanwhile is not");
    }
}
