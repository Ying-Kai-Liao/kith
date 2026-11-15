//! Asking the gate (see `agent::gate`) for a slot before a turn starts. An actor with turns queued and no slot
//! has one place in the gate's line however many turns it has queued, and shows "Waiting for room" meanwhile;
//! its commands (Stop, unsend, a room turn cancelled) keep working because it only waits for the grant in the
//! actor's select loop.

use super::{Actor, Queued};
use crate::agent::gate::Permit;
use crate::hub::BotStatus;
use crate::store::now_ms;

/// Resolves with the slot once the gate grants it; never while the actor isn't in line.
pub(super) async fn admitted(admission: &mut Option<crate::agent::gate::Admission>) -> Permit {
    match admission {
        Some(admission) => admission.granted().await,
        None => std::future::pending().await,
    }
}

impl Actor {
    /// Whether this actor holds a slot. When it doesn't, it joins the line (once) and the next grant starts
    /// its queue again.
    pub(super) fn have_room(&mut self) -> bool {
        if self.permit.is_some() {
            return true;
        }
        if self.admission.is_none() {
            let mut admission = self.hub.gate.enter_as(&self.rt, self.cfg.person.is_some());
            if let Some(permit) = admission.try_take() {
                self.permit = Some(permit);
                return true;
            }
            self.admission = Some(admission);
            self.show_waiting();
        }
        false
    }

    /// Waits for a slot before the turn a host restart cut off is resumed.
    pub(super) async fn await_room(&mut self) {
        let mut admission = self.hub.gate.enter_as(&self.rt, self.cfg.person.is_some());
        let permit = if let Some(permit) = admission.try_take() {
            permit
        } else {
            self.show_waiting();
            let permit = admission.granted().await;
            self.waiting_shown = false;
            permit
        };
        self.permit = Some(permit);
    }

    /// Housekeeping after every event: a slot is only held by a turn that runs, and a line only stood in for
    /// turns that are still queued.
    pub(super) fn tend_room(&mut self) {
        if self.turn.is_none() {
            self.permit = None;
        }
        if self.admission.is_some() && self.queue.is_empty() {
            self.admission = None;
        }
        if self.waiting_shown && self.admission.is_none() {
            self.waiting_shown = false;
            // The turn that took the slot has replaced the line already; otherwise nothing runs here.
            if self.turn.is_none() {
                self.hub.set_runtime(&self.rt, |r| {
                    if r.activity.starts_with(crate::agent::gate::WAITING) {
                        r.status = BotStatus::Idle;
                        r.activity = String::new();
                        r.started_at = None;
                        r.lane = None;
                    }
                });
            }
        }
    }

    /// Working, so the apps show the bot as busy rather than idle, with the reason it hasn't begun.
    fn show_waiting(&mut self) {
        self.waiting_shown = true;
        let text = self.hub.gate.waiting_text();
        let lane = self.queue.iter().find_map(|q| match q {
            Queued::User { lane, .. } => Some(lane.clone()),
            Queued::Group(turn, _) => Some(turn.lane.clone()),
            _ => None,
        });
        self.hub.set_runtime(&self.rt, |r| {
            r.status = BotStatus::Working;
            r.activity = text;
            r.started_at = Some(now_ms());
            r.lane = lane;
            r.message = None;
        });
    }
}
