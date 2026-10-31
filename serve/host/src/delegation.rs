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

/// A kernel command that changes the cap or marks a card raised. The kernel refuses those from inside an agent's run
/// (`KITH_AGENT`, `KITH_ASK_ID`, `CLAUDE_CODE_SESSION_ID`); a host started from an agent or a Claude Code terminal has
/// them in its environment, and they are not the host's. Empty reads as unset.
async fn as_owner(bin: &std::path::Path, args: &[&str]) -> Result<String> {
    let blank: Vec<(String, String)> =
        ["KITH_ASK_ID", "KITH_AGENT", "CLAUDE_CODE_SESSION_ID"].map(|k| (k.to_owned(), String::new())).into();
    hq::run_env(bin, args, None, READ, None, &blank).await
}

/// The delegation records on this Mac, newest first.
pub async fn list() -> Result<Vec<Delegation>> {
    let out = hq::run(&kernel().await?, &command::delegations(), None, READ).await?;
    Ok(serde_json::from_str::<Delegations>(out.trim())?.delegations)
}

/// Stop, a revoke or a removal (R5, R8): the delegations those sessions or people started end in one step. Their --bg runs
/// stop, their records settle `stopped`, no answer wakes the caller afterwards, and the wakes and waiters the callers are
/// owed go. Nothing to end is fine. An error is logged: the access or the run is already stopped.
pub async fn end(sessions: &[String], persons: &[String]) {
    let Ok(bin) = kernel().await else { return };
    end_with(&bin, sessions, persons).await;
}

async fn end_with(bin: &std::path::Path, sessions: &[String], persons: &[String]) {
    let (sessions, persons): (Vec<&String>, Vec<&String>) =
        (sessions.iter().filter(|s| plain(s)).collect(), persons.iter().filter(|p| plain(p)).collect());
    if sessions.is_empty() && persons.is_empty() {
        return;
    }
    let mut args = vec!["ask", "--end"];
    for s in &sessions {
        args.extend(["--session", s.as_str()]);
    }
    for p in &persons {
        args.extend(["--person", p.as_str()]);
    }
    if let Err(e) = as_owner(bin, &args).await {
        tracing::warn!(error = format!("{e:#}"), "couldn't end the delegations of a stopped place");
    }
}

/// [`end`] without waiting, for the sync callers of Stop.
pub fn end_soon(sessions: Vec<String>, persons: Vec<String>) {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        drop(rt.spawn(async move { end(&sessions, &persons).await }));
    }
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
             the cap; asks already running finish. The cap is one setting for every chain on this Mac.", money(spent)),
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
        if as_owner(&bin, &["ask", "--cap-told", &d.chain]).await.is_err() {
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
            let bin = kernel().await?;
            // Never lower it: the cap is the Mac's, and another chain's card may have raised it since. "none" stays none.
            let now = as_owner(&bin, &["ask", "--cap"]).await?;
            if let Ok(current) = now.trim().parse::<f64>() {
                as_owner(&bin, &["ask", "--cap", &current.max(to).to_string()]).await?;
            }
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
            brief: None,
            session: None,
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
        assert_eq!(needing_a_card(&[raised, off]).len(), 0);
    }

    #[test]
    fn a_state_this_version_does_not_know_is_left_alone() {
        let mut d = rec("a", "a", true, false, 6_000_000, Some(5_000_000));
        d.state = DelegationState::Unknown;
        assert_eq!(needing_a_card(&[d]).len(), 0);
    }

    #[test]
    fn a_chain_id_is_checked_before_it_reaches_a_command_line() {
        let all = [rec("a", "--cap", true, false, 6_000_000, Some(5_000_000))];
        assert_eq!(needing_a_card(&all).len(), 0);
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

    // A host started inside an agent's run still changes the cap and marks cards, through the real kernel.
    #[tokio::test]
    async fn a_host_started_from_an_agent_still_sets_the_cap_and_marks_cards() {
        use crate::channels::hq::TEST_ENV;
        let _one = crate::remote::account::ONE_AT_A_TIME.lock().await;
        let dir = std::env::temp_dir().join(format!("kith-delegation-k-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        *TEST_ENV.lock().unwrap() = vec![
            ("KITH_HOME", dir.join("home")),
            ("KITH_STATE", dir.join("state")),
            ("HOME", dir.clone()),
            ("KITH_AGENT", "architect".into()),
            ("KITH_ASK_ID", "ask-1".into()),
            ("CLAUDE_CODE_SESSION_ID", "sess-1".into()),
        ];
        let kith = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith");
        let set = as_owner(&kith, &["ask", "--cap", "7"]).await;
        let told = as_owner(&kith, &["ask", "--cap-told", "ask-1"]).await;
        let plain = hq::run(&kith, &["ask", "--cap", "9"], None, READ).await;
        let shown = as_owner(&kith, &["ask", "--cap"]).await;
        TEST_ENV.lock().unwrap().clear();
        assert!(set.is_ok() && told.is_ok(), "{set:?} {told:?}");
        assert!(plain.is_err(), "the kernel still refuses an agent's own call");
        assert_eq!(shown.unwrap().trim(), "7");
    }

    // Stop, a revoke and a removal end the delegations of a lane or a person through the real kernel (R5, R8).
    #[tokio::test]
    async fn ending_settles_the_lanes_asks_and_the_persons_and_no_later_answer_wakes_anyone() {
        use crate::channels::hq::TEST_ENV;
        let _one = crate::remote::account::ONE_AT_A_TIME.lock().await;
        let dir = std::env::temp_dir().join(format!("kith-delegation-e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let env = [("KITH_HOME", dir.join("home")), ("KITH_STATE", dir.join("state")), ("HOME", dir.clone())];
        *TEST_ENV.lock().unwrap() = env.to_vec();
        let kith = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith");
        let boxdir = {
            let mut cmd = std::process::Command::new("python3");
            cmd.args([
                "-c",
                "import sys; sys.path.insert(0, sys.argv[1]); from kith_paths import paths; print(paths()['KITH_MACHINE'] / 'live' / 'ask')",
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/lib").to_string_lossy(),
            ]);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            std::path::PathBuf::from(String::from_utf8(cmd.output().unwrap().stdout).unwrap().trim())
        };
        std::fs::create_dir_all(&boxdir).unwrap();
        let reg = |id: &str, session: Option<&str>, person: Option<&str>| {
            let reply = session.map_or(json!({}), |s| json!({"session": s}));
            let body = json!({"asked": "echo", "agent": "architect", "person": person, "via": "terminal",
                "reply_to": reply, "chain": id, "hop": 0, "cross_hops": 0, "crossed": false, "cost": 0.0,
                "cost_base": 0.0, "state": "open", "at": 1.0});
            std::fs::write(boxdir.join(format!("{id}.reg")), body.to_string()).unwrap();
        };
        let state_of = |id: &str| -> String {
            let v: Value =
                serde_json::from_str(&std::fs::read_to_string(boxdir.join(format!("{id}.reg"))).unwrap()).unwrap();
            v["state"].as_str().unwrap().to_owned()
        };
        reg("ask-lane", Some("sess-lane"), None);
        reg("ask-other", Some("sess-other"), None);
        reg("ask-mia", Some("sess-mia"), Some("mia"));
        reg("ask-old", None, None);
        end_with(&kith, &["sess-lane".to_owned()], &[]).await;
        assert_eq!(state_of("ask-lane"), "stopped");
        assert_eq!(state_of("ask-other"), "open", "another lane's ask is untouched");
        assert_eq!(state_of("ask-old"), "open", "an old record belongs to no lane");
        // Stop beats an answer on its way: it is refused and wakes no one.
        let late = as_owner(&kith, &["ask", "--answer", "ask-lane", "--as", "echo", "--", "too late"]).await.unwrap();
        assert_eq!(late.trim(), "stopped");
        end_with(&kith, &[], &["mia".to_owned()]).await;
        assert_eq!(state_of("ask-mia"), "stopped", "a person's ask ends with their access");
        assert_eq!(state_of("ask-other"), "open");
        end_with(&kith, &["sess-lane".to_owned()], &["mia".to_owned()]).await;
        assert_eq!(state_of("ask-lane"), "stopped", "ending twice is fine");
        TEST_ENV.lock().unwrap().clear();
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
