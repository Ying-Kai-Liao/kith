//! Who answers a card (designs/2026-10-25-access-arch.md, rules 1 and 2). [`approver`] is the only place that names
//! the approver; every producer (a -p run's card, an app chat's prompt, an escape card, a key card) calls it and stores
//! what it says on the card (`answerable_by`, None is the owner alone). [`may_answer`] is the guard's check and
//! [`can_answer`] what the apps are told; both read that stored field, so the buttons a person sees are the answers the
//! guard takes. What a card names is judged once, when it opens (`in_grants`), never again at answer time; who may answer
//! is derived again from it on the sweep and when kith-auth changes ([`rederive`]).

use crate::api::roles;
use crate::hub::Hub;
use crate::store::{Card, Store};
use serde_json::Value;
use std::sync::Arc;

/// What a card asks for, as far as who answers it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// A key or an account (cards/secret.rs): the owner's alone.
    Secret,
    /// A path or domain beyond the run's folders (cards/access.rs): the owner's alone.
    Escape,
    /// A tool call. `in_grants`: every path it names is inside the asker's grant roots and it runs no code.
    Tool { in_grants: bool },
}

/// One ask, for [`approver`].
pub struct Request<'a> {
    /// The asker; empty for the owner's own run.
    pub person: &'a str,
    pub agent: &'a str,
    /// The owner's own run (an owner `kith ask`), whoever it names.
    pub owner_run: bool,
    pub ask: Ask,
}

impl Request<'_> {
    /// What the card stores as `in_grants`: whether a person could answer it at all.
    pub fn in_grants(&self) -> bool {
        !self.owner_run && !self.person.is_empty() && self.ask == (Ask::Tool { in_grants: true })
    }
}

/// Whether every path a -p run's tool names lies in the asker's grant roots and the tool runs no code.
pub fn tool_in_grants(person: &str, agent: &str, tool: &str, kind: &str, input: &Value) -> bool {
    !roles::runs_code(tool, kind) && roles::within(&roles::grant_roots(person, agent), tool, input)
}

/// [`tool_in_grants`] for an app chat's (ACP) tool call: one with no tool name is judged by its `locations`.
pub fn acp_in_grants(person: &str, agent: &str, name: &str, kind: &str, call: &Value) -> bool {
    !roles::runs_code(name, kind) && roles::acp_within(&roles::grant_roots(person, agent), name, kind, call)
}

/// Whether kith-auth says `person` holds `agent` with the member preset now; None when it can't be asked.
async fn member(person: &str, agent: &str) -> Option<bool> {
    let v = roles::resolve(person, agent).await?;
    Some(v["allowed"].as_bool() == Some(true) && v["preset"].as_str() == Some("member"))
}

/// Who answers `req` besides the owner: the member asker for what is within their grants; a bot guest's owner for what
/// is within the bot's, when that owner is a member of the agent; else nobody (None, the owner alone). Outer None: kith-auth
/// couldn't be asked.
#[allow(clippy::option_option, reason = "kith-auth unreachable and the owner alone are different answers")]
async fn derive(store: &Store, req: &Request<'_>) -> Option<Option<String>> {
    if !req.in_grants() {
        return Some(None);
    }
    let who = match store.bot_guest_of_person(req.person) {
        // A bot of the computer's owner: theirs.
        Some(bot) => match bot.owner_person {
            Some(o) => o,
            None => return Some(None),
        },
        None => req.person.to_owned(),
    };
    Some(member(&who, req.agent).await?.then_some(who))
}

/// Who answers `req` besides the owner (None: the owner alone). Fails toward the owner.
pub async fn approver(store: &Store, req: &Request<'_>) -> Option<String> {
    derive(store, req).await.flatten()
}

/// Whether `viewer` (None: the owner) may answer `card` now: the owner always; a person only when the card names them
/// and kith-auth still says they are a member of its agent. What the card names is not judged again here.
pub async fn may_answer(viewer: Option<&str>, card: &Card) -> bool {
    let Some(v) = viewer else { return true };
    card.answerable_by.as_deref() == Some(v) && member(v, &card.agent).await == Some(true)
}

/// What the apps are told (`canAnswer`): [`may_answer`] without asking kith-auth, from the stored field alone.
pub fn can_answer(viewer: Option<&str>, card: &Card) -> bool {
    viewer.is_none_or(|v| card.answerable_by.as_deref() == Some(v))
}

/// Derives who answers each open card again (the sweep tick, and every change to kith-auth): one whose approver changed
/// has its entry updated, which tells the apps, and the new approver is told by a push. A card whose asker lost the agent
/// is withdrawn; nothing else closes a card here. A card kith-auth couldn't be asked about stays as it was.
pub async fn rederive(hub: &Arc<Hub>) {
    for card in hub.store.cards_open_all() {
        if card.tool == crate::cards::secret::TOOL {
            continue;
        }
        if !card.person.is_empty() && roles::may_use_checked(&card.person, &card.agent).await == Some(false) {
            crate::cards::withdraw_person(hub, &card.person, Some(&card.agent));
            continue;
        }
        let req = Request {
            person: &card.person,
            agent: &card.agent,
            owner_run: false,
            ask: Ask::Tool { in_grants: card.in_grants },
        };
        let Some(now) = derive(&hub.store, &req).await else { continue };
        if now == card.answerable_by {
            continue;
        }
        match hub.store.card_set_answerable(&card.id, now.as_deref()) {
            Ok(Some(changed)) => crate::cards::approver_changed(hub, &changed, card.answerable_by.as_deref()),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = format!("{error:#}"), card = %card.id, "couldn't change a card's approver");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(answerable_by: Option<&str>) -> Card {
        Card {
            id: "c".into(),
            session: "s".into(),
            source: "run".into(),
            bot_id: None,
            run_id: None,
            entry_id: None,
            person: "sam".into(),
            agent: "echo".into(),
            approver: if answerable_by.is_some() { "member" } else { "owner" }.into(),
            tool: "Read".into(),
            input: "{}".into(),
            resource: "r".into(),
            shown: String::new(),
            scope: "once".into(),
            state: crate::store::CardState::Live,
            created_at: 0,
            deadline: i64::MAX,
            answer: None,
            allowed: None,
            answered_by: None,
            answered_at: None,
            answered_parked: false,
            granted_at: None,
            resumed_at: None,
            applied_at: None,
            reply_to: None,
            asked_by: None,
            answerable_by: answerable_by.map(str::to_owned),
            in_grants: answerable_by.is_some(),
        }
    }

    #[test]
    fn the_owner_can_answer_every_card_and_a_person_only_the_ones_naming_them() {
        assert!(can_answer(None, &card(None)) && can_answer(None, &card(Some("sam"))));
        assert!(can_answer(Some("sam"), &card(Some("sam"))));
        assert!(!can_answer(Some("sam"), &card(None)), "an owner card");
        assert!(!can_answer(Some("ana"), &card(Some("sam"))), "someone else's");
        // A person whose id is the word "owner" is a person like any other.
        assert!(!can_answer(Some("owner"), &card(None)));
    }

    #[test]
    fn only_a_tool_within_a_persons_grants_can_have_a_person_answer() {
        let r = |person, owner_run, ask| Request { person, agent: "echo", owner_run, ask };
        assert!(r("sam", false, Ask::Tool { in_grants: true }).in_grants());
        assert!(!r("sam", false, Ask::Tool { in_grants: false }).in_grants());
        assert!(!r("sam", true, Ask::Tool { in_grants: true }).in_grants(), "the owner's own run");
        assert!(!r("", false, Ask::Tool { in_grants: true }).in_grants(), "no person");
        assert!(!r("sam", false, Ask::Secret).in_grants() && !r("sam", false, Ask::Escape).in_grants());
    }

    #[tokio::test]
    async fn the_owner_answers_whatever_is_not_within_a_persons_grants() {
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        for ask in [Ask::Secret, Ask::Escape, Ask::Tool { in_grants: false }] {
            let req = Request { person: "sam", agent: "echo", owner_run: false, ask };
            assert_eq!(approver(&store, &req).await, None);
        }
        let own = Request { person: "sam", agent: "echo", owner_run: true, ask: Ask::Tool { in_grants: true } };
        assert_eq!(approver(&store, &own).await, None);
    }
}
