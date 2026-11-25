//! What an ask waits on (R7), for the host's agent status and Needs-you: `{on: "card", card, since}` when the ask or any open
//! descendant is parked on a card (so a root asker shows "waiting on a card" while a grandchild waits on one), else
//! `{on: "agent", agent, since}` when it has an open child, else nothing.

use serde_json::{Value, json};

use super::Ask;

fn live(a: &Ask) -> bool {
    a.state == "open" && a.ended_ms.is_none()
}

/// What `a` waits on, given every ask.
pub(super) fn of(a: &Ask, asks: &[Ask]) -> Option<Value> {
    if !live(a) {
        return None;
    }
    // The open asks below `a`, by `parent`, through open ones only (a settled ask waits on nothing).
    let mut below: Vec<&Ask> = Vec::new();
    let mut up = vec![a];
    while let Some(p) = up.pop() {
        for c in asks
            .iter()
            .filter(|c| c.parent.as_deref() == Some(p.id.as_str()) && live(c))
        {
            if !below.iter().any(|b| b.id == c.id) {
                below.push(c);
                up.push(c);
            }
        }
    }
    let parked = std::iter::once(a)
        .chain(below.iter().copied())
        .filter(|x| x.parked_card.is_some())
        .min_by_key(|x| (x.parked_ms.unwrap_or(0), &x.id));
    if let Some(p) = parked {
        return Some(json!({"on": "card", "card": p.parked_card, "since": p.parked_ms}));
    }
    asks.iter()
        .filter(|c| c.parent.as_deref() == Some(a.id.as_str()) && live(c))
        .min_by_key(|c| (c.at_ms.unwrap_or(0), &c.id))
        .map(|c| json!({"on": "agent", "agent": c.asked, "since": c.at_ms}))
}
