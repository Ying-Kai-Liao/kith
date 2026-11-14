//! An invite that names rooms (rooms with people, Q4): once an invited person's device has proved the invite, the host
//! puts them in each room, once.
//!
//! The join is level-triggered, not a reaction to one event: [`reconcile`] looks at every invite with a room part not
//! done and joins whoever has proved it, and it runs at each `proveInvite`, at every host start and every minute after.
//! So a restart
//! between the prove and the join loses nothing (model `invite-edge`). The done mark, written once the person is in (or
//! the room is gone), is what keeps Kai's removal: a second device, a retried prove or a restart finds the part done and
//! joins no one (model `invite-nomarker`). Nothing here stores a grant: the join goes through
//! [`room_people::add_person_for`], so it is the same step Kai's own add takes (one gen, the live check of every agent).

use super::room_people;
use crate::hub::Hub;
use crate::store::Invite;
use std::sync::Arc;

/// One reconcile at a time. Two devices proving together must not both read a part as not done, join, and the second
/// join land after Kai removed the person the first one let in; each pass reads the marks after taking this.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Whether a device of the invite's person has shown its token: the host recorded the membership of that invite, and a
/// device of the person is proven. A revoked person's devices and memberships are set aside, so they never pass.
fn proved(hub: &Hub, invite: &Invite) -> bool {
    let member = hub
        .store
        .memberships()
        .is_ok_and(|ms| ms.iter().any(|m| m.person_id == invite.person_id && m.invite_id == invite.invite_id));
    member && hub.store.devices_of(&invite.person_id).is_ok_and(|ds| ds.iter().any(|d| hub.store.device_proven(&d.key)))
}

/// Whether the room is gone: no bot row, or a deleted one. Decided from the row alone: a room that gained an outside bot
/// or lost its agents is not gone, its part waits (and says why in the log). `None` only on a store error.
fn room_gone(hub: &Hub, room: &str) -> Option<bool> {
    hub.store.bot(room).ok().map(|r| r.is_none_or(|r| r.deleted))
}

/// Joins every proved, open invite's person to the rooms it names that are not done, and marks each done. A part that
/// can't be done now (the setting is off, the person isn't granted every agent in the room) stays for the next pass.
pub async fn reconcile(hub: Arc<Hub>) {
    let _one = ONE_AT_A_TIME.lock().await;
    for (invite_id, room) in hub.store.invite_rooms_todo() {
        // Only a person who proved the invite (a cancelled invite never got a membership; a revoked person's are set
        // aside). `used_at` is set by then, so `Invite::open` would say no, and a delayed join may come after the day.
        let Some(invite) = hub.store.invite(&invite_id) else { continue };
        if !proved(&hub, &invite) {
            continue;
        }
        match room_gone(&hub, &room) {
            // No join, so nothing for a removal to be undone by: a plain mark.
            Some(true) => {
                if let Err(e) = hub.store.mark_invite_room_done(&invite_id, &room) {
                    tracing::warn!(error = format!("{e:#}"), invite = invite_id, room, "couldn't mark a room done");
                }
            }
            // The join and its done mark are one transaction (`add_person_for`).
            Some(false) if room_people::enabled(&hub.store) => {
                // A refusal repeats each pass until fixed; `invites` shows the part as not done.
                if let Err(e) = room_people::add_person_for(&hub, &room, &invite.person_id, Some(&invite_id)).await {
                    tracing::debug!(
                        error = format!("{e:#}"),
                        invite = invite_id,
                        room,
                        "couldn't join an invited person yet"
                    );
                }
            }
            Some(false) | None => {}
        }
    }
}

/// How often [`run`] looks again, for what no event announces: an undo of a revoke, a grant given later, the setting
/// turned on.
const EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// A pass at host start, which finishes a join a restart cut off, then one each minute.
pub async fn run(hub: Arc<Hub>) {
    loop {
        reconcile(hub.clone()).await;
        tokio::time::sleep(EVERY).await;
    }
}
