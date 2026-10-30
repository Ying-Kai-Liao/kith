//! The host's side of the kernel's delegation records (`kernel/bin/kith-ask`, `kith ask delegations --json`).
//!
//! An agent asks another agent with `kith ask`, that one asks a third, and so on. The kernel keeps one record per ask id
//! (its chain, hop, cost, state) and enforces the caps: 8 asks deep on one Mac, 2 crossings to another, and a cost cap per
//! chain (`delegation.chain_cost_cap_usd`, US$5). The host keeps no store of its own: it reads the records through the
//! contract's types (`kith_wire::kernel`) and, when a chain has reached its cost cap, puts one Needs-you card in front of
//! the owner. `kith ask --cap-told CHAIN` is how the kernel learns the card was raised, so a restart, or a second sweep,
//! never raises it twice for the same cap. Without a host the kernel's refusal of the next ask is all there is.
//!
//! The card answers: "Raise to $X" (doubles the cap, `kith ask --cap X`) or "Leave it". It is a plain permission entry the
//! apps already draw, marked with `delegation` so `respondPermission` routes the answer here.

use crate::agent::hq;
use crate::hub::Hub;
use crate::remote::push::{self, AlertKind};
use crate::store::{Entry, EntryKind, Lane};
use anyhow::{Result, anyhow, bail};
use kith_wire::kernel::{Delegation, DelegationState, Delegations, command};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(30);
const READ: Duration = Duration::from_secs(20);

async fn kernel() -> Result<std::path::PathBuf> {
    tokio::task::spawn_blocking(hq::bin).await?.ok_or_else(|| anyhow!("kith isn't installed on this computer"))
}

/// The delegation records on this Mac, newest first.
pub async fn list() -> Result<Vec<Delegation>> {
    let out = hq::run(&kernel().await?, &command::delegations(), None, READ).await?;
    Ok(serde_json::from_str::<Delegations>(out.trim())?.delegations)
}

#[allow(clippy::cast_precision_loss, reason = "micro-dollars far below 2^53")]
fn usd(micro: i64) -> f64 {
    micro as f64 / 1e6
}

fn money(x: f64) -> String {
    if x.fract() == 0.0 { format!("US${x:.0}") } else { format!("US${x:.2}") }
}

/// A kernel id, safe to put on a command line.
fn plain(id: &str) -> bool {
    id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The records whose chain reached its cap and has no card yet, one per chain. A chain whose cap was raised since, or
/// turned off, is under it again and gets none; a state this version doesn't know is left alone.
pub fn needing_a_card(all: &[Delegation]) -> Vec<&Delegation> {
    let mut out: Vec<&Delegation> = Vec::new();
    for d in all {
        let capped = d.capped_ms.is_some() && d.cap_card_ms.is_none() && d.state != DelegationState::Unknown;
        let still_over = matches!((d.chain_cost_micro_usd, d.cap_micro_usd), (Some(cost), Some(cap)) if cost >= cap);
        if capped && still_over && plain(&d.chain) && !out.iter().any(|o| o.chain == d.chain) {
            out.push(d);
        }
    }
    out
}

/// "architect, asked by a terminal": the chain as the owner knows it, from the record of its root ask when this Mac has it.
fn label(all: &[Delegation], d: &Delegation) -> String {
    let root = all.iter().find(|r| r.id == d.chain).unwrap_or(d);
    let agent = root.agent.split('@').next().unwrap_or(&root.agent);
    format!("{agent}, asked by {}", root.caller.agent.as_deref().unwrap_or("a terminal"))
}

/// The card's entry data for a chain at its cap.
pub fn card(bot: &str, agent: &str, label: &str, chain: &str, spent: f64, cap: f64) -> Value {
    let raise = (cap * 2.0).max(cap + 1.0).round();
    json!({
        "title": format!("{label} reached its {} cap", money(cap)),
        "toolKind": "other",
        "command": Value::Null,
        "detail": format!(
            "The chain of asks that started with {label} has spent {} at API prices. It starts no new ask until you raise \
             the cap; asks already running finish.", money(spent)),
        "diffs": [],
        "cwd": Value::Null,
        "options": [
            {"optionId": "raise", "name": format!("Raise to {}", money(raise)), "kind": "allow_once"},
            {"optionId": "leave", "name": "Leave it", "kind": "reject_once"},
        ],
        "status": "pending",
        "selected": Value::Null,
        "author": bot,
        "approver": "owner",
        "agent": agent,
        "delegation": {"chain": chain, "spent": spent, "cap": cap, "raiseTo": raise},
    })
}

/// Puts the chain's card in the owner's Needs-you (the agent's card lane when it has no chat) and tells their devices.
fn raise(hub: &Hub, all: &[Delegation], d: &Delegation) -> bool {
    let (Some(cost), Some(cap)) = (d.chain_cost_micro_usd, d.cap_micro_usd) else { return false };
    let root = all.iter().find(|r| r.id == d.chain).unwrap_or(d);
    let agent = root.agent.split('@').next().unwrap_or(&root.agent);
    let chat = crate::cards::chat_for(hub, "", agent);
    let lane = Lane::main(&chat.id);
    let label = label(all, d);
    let data = card(&chat.id, agent, &label, &d.chain, usd(cost), usd(cap));
    if hub.add_entry(&lane, EntryKind::Permission, hub.store.max_turn(&lane.chat), &data).is_none() {
        tracing::warn!(chain = %d.chain, "couldn't save the delegation cap card");
        return false;
    }
    let title = data["title"].as_str().unwrap_or_default();
    push::notify(hub, &chat, Some(&chat.id), &format!("{label} needs you"), title, AlertKind::NeedsInput);
    true
}

/// One look at the records: a card for each chain that reached its cap.
pub async fn sweep(hub: &Arc<Hub>) {
    let all = match list().await {
        Ok(all) => all,
        Err(error) => return tracing::debug!(error = format!("{error:#}"), "couldn't read the delegation records"),
    };
    for d in needing_a_card(&all) {
        // Told first: a crash after the card must not raise it again; a card that fails to save is logged.
        let Ok(bin) = kernel().await else { return };
        if hq::run(&bin, &["ask", "--cap-told", &d.chain], None, READ).await.is_err() {
            continue;
        }
        raise(hub, &all, d);
    }
}

/// At start, then every 30 seconds.
pub async fn sweep_loop(hub: Arc<Hub>) {
    loop {
        sweep(&hub).await;
        tokio::time::sleep(EVERY).await;
    }
}

/// The owner's answer on a cap card (`respondPermission`): raise the cap, or leave it.
pub async fn respond(hub: &Hub, entry: &Entry, option: Option<&str>, by: &str) -> Result<Value> {
    if by != "owner" {
        bail!("Only the owner answers a delegation cap card.");
    }
    if entry.data["status"] != "pending" {
        return Ok(json!({"closed": true}));
    }
    let option = option.unwrap_or("leave");
    match option {
        "raise" => {
            let to = entry.data["delegation"]["raiseTo"].as_f64().filter(|t| *t > 0.0);
            let to = to.ok_or_else(|| anyhow!("the card has no amount to raise to"))?;
            hq::run(&kernel().await?, &["ask", "--cap", &to.to_string()], None, READ).await?;
        }
        "leave" => {}
        _ => bail!("That isn't one of the choices on this card."),
    }
    let mut data = entry.data.clone();
    data["status"] = "answered".into();
    data["selected"] = option.into();
    hub.set_entry(&entry.id, &data);
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kith_wire::kernel::{Caller, ReplyTo};

    fn rec(id: &str, chain: &str, capped: bool, told: bool, cost: i64, cap: Option<i64>) -> Delegation {
        Delegation {
            id: id.into(),
            agent: "echo".into(),
            chain: chain.into(),
            hop: 0,
            cross_hops: 0,
            crossed: false,
            caller: Caller { agent: Some("architect".into()), person: None, via: None },
            reply_to: ReplyTo { session: None, ask: None },
            state: DelegationState::Capped,
            cost_micro_usd: Some(cost),
            chain_cost_micro_usd: Some(cost),
            cap_micro_usd: cap,
            started_ms: 1,
            answered_ms: None,
            capped_ms: capped.then_some(2),
            cap_card_ms: told.then_some(3),
        }
    }

    #[test]
    fn a_capped_chain_without_a_card_gets_one_and_only_one() {
        let all = [
            rec("a", "a", true, false, 5_250_000, Some(5_000_000)),
            rec("b", "a", true, false, 5_250_000, Some(5_000_000)),
            rec("c", "c", true, true, 5_250_000, Some(5_000_000)),
            rec("d", "d", false, false, 1_000_000, Some(5_000_000)),
        ];
        let due: Vec<&str> = needing_a_card(&all).iter().map(|d| d.id.as_str()).collect();
        assert_eq!(due, ["a"], "one per chain; a told chain and an uncapped one get none");
    }

    #[test]
    fn a_chain_whose_cap_was_raised_or_turned_off_gets_no_card() {
        let raised = rec("a", "a", true, false, 5_250_000, Some(10_000_000));
        let off = rec("b", "b", true, false, 5_250_000, None);
        assert!(needing_a_card(&[raised, off]).is_empty());
    }

    #[test]
    fn a_state_this_version_does_not_know_is_left_alone() {
        let mut d = rec("a", "a", true, false, 6_000_000, Some(5_000_000));
        d.state = DelegationState::Unknown;
        assert!(needing_a_card(&[d]).is_empty());
    }

    #[test]
    fn a_chain_id_is_checked_before_it_reaches_a_command_line() {
        let all = [rec("a", "--cap", true, false, 6_000_000, Some(5_000_000))];
        assert!(needing_a_card(&all).is_empty());
        assert!(plain("ask-20261031104140-1f08b7"));
        assert!(!plain("a b") && !plain("") && !plain("a;b"));
    }

    #[test]
    fn the_card_names_the_chain_and_offers_raise_or_leave() {
        let c = card("bot1", "echo", "echo, asked by architect", "ask-1", 5.25, 5.0);
        assert_eq!(c["title"], "echo, asked by architect reached its US$5 cap");
        assert_eq!(c["approver"], "owner");
        assert_eq!(c["status"], "pending");
        let names: Vec<&str> = c["options"].as_array().unwrap().iter().filter_map(|o| o["name"].as_str()).collect();
        assert_eq!(names, ["Raise to US$10", "Leave it"]);
        assert_eq!(c["delegation"]["raiseTo"], 10.0);
    }

    #[test]
    fn the_label_is_the_root_ask_and_who_made_it() {
        let root = rec("a", "a", false, false, 1, None);
        let mut nested = rec("b", "a", true, false, 1, None);
        nested.agent = "relay@mini".into();
        assert_eq!(label(&[root.clone(), nested.clone()], &nested), "echo, asked by architect");
        let mut terminal = root;
        terminal.caller.agent = None;
        assert_eq!(label(&[terminal.clone()], &terminal), "echo, asked by a terminal");
    }

    // The raise-and-answer path on a real hub and the real kernel's `kith ask`.
    mod on_a_hub {
        use super::*;
        use crate::remote::identity::Identity;
        use crate::store::Store;

        fn hub() -> Arc<Hub> {
            let dir = std::env::temp_dir().join(format!("kith-delegation-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Store::open(std::path::Path::new(":memory:")).unwrap();
            Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0)
        }

        #[tokio::test]
        async fn raising_a_card_puts_a_pending_entry_in_the_owners_needs_you_and_pushes() {
            let hub = hub();
            let all = [rec("a", "a", true, false, 5_250_000, Some(5_000_000))];
            let due = needing_a_card(&all);
            assert!(raise(&hub, &all, due[0]));
            let lane = Lane::main(&crate::cards::chat_for(&hub, "", "echo").id);
            let cards = hub.store.entries_from(&lane.chat, 0).unwrap();
            assert_eq!(cards.len(), 1, "one card");
            assert_eq!(cards[0].data["delegation"]["chain"], "a");
            let sent = push::SENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            assert!(
                sent.iter().any(|s| s.starts_with("echo, asked by architect needs you")),
                "the owner is told: {sent:?}"
            );
        }

        #[tokio::test]
        async fn only_the_owner_answers_it_and_leave_closes_it_without_touching_the_cap() {
            let hub = hub();
            let all = [rec("a", "a", true, false, 5_250_000, Some(5_000_000))];
            assert!(raise(&hub, &all, &all[0]));
            let lane = Lane::main(&crate::cards::chat_for(&hub, "", "echo").id);
            let entry = hub.store.entries_from(&lane.chat, 0).unwrap().remove(0);
            assert!(respond(&hub, &entry, Some("leave"), "mia").await.is_err());
            respond(&hub, &entry, Some("leave"), "owner").await.unwrap();
            assert_eq!(hub.store.entry(&entry.id).unwrap().data["status"], "answered");
            let again = hub.store.entry(&entry.id).unwrap();
            assert_eq!(respond(&hub, &again, Some("leave"), "owner").await.unwrap(), json!({"closed": true}));
        }
    }
}
