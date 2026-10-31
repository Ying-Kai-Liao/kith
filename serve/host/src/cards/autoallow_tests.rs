//! The owner's card for a call the auto-mode classifier blocked: its options, what each answer does, and "Always allow"
//! written to the real `kith allow` of the kernel in the repo (a throwaway `KITH_HOME`, as the other card tests do).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::tests::{entry_status, ls, owner_run, shown, window};
use super::*;
use crate::api::rig::Rig;

const REASON: &str = "[Data Exfiltration]";

fn ask_auto(hub: &Arc<Hub>, who: RunIdentity, tool: &'static str, input: Value) -> tokio::task::JoinHandle<Decision> {
    ask_auto_for(hub, who, tool, input, REASON)
}

fn ask_auto_for(
    hub: &Arc<Hub>,
    who: RunIdentity,
    tool: &'static str,
    input: Value,
    reason: &'static str,
) -> tokio::task::JoinHandle<Decision> {
    let hub = hub.clone();
    tokio::spawn(async move { request_auto_denied(&hub, &who, tool, &input, reason).await })
}

fn data(r: &Rig, card: &Card) -> Value {
    r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data
}

fn ids(data: &Value) -> Vec<String> {
    data["options"].as_array().unwrap().iter().map(|o| o["optionId"].as_str().unwrap().to_owned()).collect()
}

/// The agent's allow list as `kith allow house --json` reads it.
fn saved(r: &Rig) -> Vec<Value> {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-allow");
    let out = std::process::Command::new(bin)
        .args(["house", "--json"])
        .envs(crate::channels::hq::TEST_ENV.lock().unwrap().iter().map(|(k, v)| (*k, v)))
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(r.dir.exists());
    v["allowed"].as_array().unwrap().clone()
}

async fn until(what: impl Fn() -> bool) {
    for _ in 0..200 {
        if what() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

use std::path::PathBuf;

#[tokio::test]
async fn a_blocked_call_gets_an_owner_card_with_the_reason_and_three_options() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = ask_auto(&r.hub, owner_run("auto-a"), "Bash", ls());
    let card = shown(&r.hub).await;
    assert_eq!(card.approver, "owner");
    assert!(autoallow::is_auto(&card), "{}", card.resource);
    let d = data(&r, &card);
    assert_eq!(ids(&d), ["allow", autoallow::ALWAYS, "deny"]);
    let names: Vec<_> = d["options"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Allow once", "Always allow", "Deny"]);
    let kinds: Vec<_> = d["options"].as_array().unwrap().iter().map(|o| o["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["allow_once", "allow_always", "reject_once"]);
    assert_eq!(d["title"], "Run ls /tmp");
    assert_eq!(d["agent"], "house");
    let detail = d["detail"].as_str().unwrap();
    assert!(detail.contains("Auto mode blocked this: [Data Exfiltration]"), "{detail}");
    assert!(detail.contains("Always allow remembers Bash(ls /tmp)."), "{detail}");
    assert_eq!(d["autoAllow"]["rule"], "Bash(ls /tmp)", "what was offered is kept on the entry");
    assert!(d.get("edge").is_none_or(Value::is_null));
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
}

#[tokio::test]
async fn allow_once_lets_that_call_retry_and_remembers_nothing() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = ask_auto(&r.hub, owner_run("auto-b"), "Bash", ls());
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    assert!(call.await.unwrap().allow);
    assert_eq!(entry_status(&r.hub, &card), "answered");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(saved(&r).len(), 0);
    // The yes was spent on that one denial: the next denial of the same call is a new card.
    let again = ask_auto(&r.hub, owner_run("auto-b"), "Bash", ls());
    let second = shown(&r.hub).await;
    assert_ne!(second.id, card.id);
    answer(&r.hub, second.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!again.await.unwrap().allow);
}

#[tokio::test]
async fn always_allow_saves_one_narrow_rule_once_and_a_deny_saves_nothing() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = ask_auto(&r.hub, owner_run("auto-c"), "Bash", ls());
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(saved(&r).is_empty(), "a no remembers nothing");

    for session in ["auto-c", "auto-c2"] {
        let call = ask_auto(&r.hub, owner_run(session), "Bash", ls());
        let card = shown(&r.hub).await;
        answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(autoallow::ALWAYS), None).unwrap();
        assert!(call.await.unwrap().allow, "the run hears its yes at once");
        until(|| data(&r, &card)["outcome"].is_string()).await;
        assert_eq!(data(&r, &card)["outcome"], "Allowed, and remembered for house: Bash(ls /tmp)");
    }
    let rules = saved(&r);
    assert_eq!(rules.len(), 1, "the same rule twice is one entry: {rules:?}");
    assert_eq!((rules[0]["rule"].as_str(), rules[0]["kind"].as_str()), (Some("Bash(ls /tmp)"), Some("rule")));
    assert_eq!(rules[0]["reason"], REASON);
}

#[tokio::test]
async fn a_late_always_still_saves_and_a_late_once_does_nothing() {
    let r = Rig::new().await;
    r.hub.store.kv_set("cards.resume", "on").unwrap();
    window(&r.hub, 0);
    let who = owner_run("auto-d");
    let waiting = request_auto_denied(&r.hub, &who, "Bash", &ls(), REASON).await;
    assert!(!waiting.allow && waiting.waiting, "no answer in the live window leaves the denial");
    let card = shown(&r.hub).await;
    run_exited(&r.hub, &who.run_id);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), None).unwrap();
    let kept = r.hub.store.card(&card.id).unwrap();
    assert_eq!(kept.state, CardState::Used, "a yes with no run to hear it is spent, not owed");
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0, "and it resumes nothing");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(saved(&r).len(), 0);

    let who = owner_run("auto-e");
    assert!(!request_auto_denied(&r.hub, &who, "Bash", &ls(), REASON).await.allow);
    let card = shown(&r.hub).await;
    run_exited(&r.hub, &who.run_id);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(autoallow::ALWAYS), None).unwrap();
    until(|| !saved(&r).is_empty()).await;
    assert_eq!(saved(&r).len(), 1, "the rule is kept for the next run");
    assert_eq!(r.hub.store.cards_awaiting_resume(now_ms()).len(), 0);
}

#[tokio::test]
async fn what_cannot_be_remembered_gets_no_always_option() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    for (n, command) in ["rm -rf /tmp/build", "python3", "echo $AWS_SECRET_ACCESS_KEY"].into_iter().enumerate() {
        let call = ask_auto(&r.hub, owner_run(&format!("auto-f{n}")), "Bash", json!({"command": command}));
        let card = shown(&r.hub).await;
        assert_eq!(ids(&data(&r, &card)), ["allow", "deny"], "{command}");
        answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
        assert!(!call.await.unwrap().allow);
    }
}

#[tokio::test]
async fn a_rule_a_permission_rule_cannot_hold_is_kept_as_a_sentence_naming_the_agent() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = ask_auto_for(
        &r.hub,
        owner_run("auto-g"),
        "Bash",
        json!({"command": "git push origin main"}),
        "[Irreversible Local Destruction]",
    );
    let card = shown(&r.hub).await;
    let d = data(&r, &card);
    assert_eq!(ids(&d), ["allow", autoallow::ALWAYS, "deny"]);
    assert!(d["detail"].as_str().unwrap().contains("this exact action, for house"), "{d}");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(autoallow::ALWAYS), None).unwrap();
    assert!(call.await.unwrap().allow);
    until(|| !saved(&r).is_empty()).await;
    let rules = saved(&r);
    assert_eq!(rules[0]["kind"], "prose");
    let text = rules[0]["rule"].as_str().unwrap();
    assert!(text.contains("\"house\"") && text.contains("git push origin main"), "{text}");
}

#[tokio::test]
async fn an_auto_card_never_joins_or_uses_an_ordinary_approval_of_the_same_call() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let ordinary = tokio::spawn({
        let hub = r.hub.clone();
        async move { request_permission(&hub, &owner_run("auto-h"), "Bash", &ls()).await }
    });
    let first = shown(&r.hub).await;
    assert!(!autoallow::is_auto(&first));
    assert_eq!(ids(&data(&r, &first)), ["allow", "deny"]);
    let auto = ask_auto(&r.hub, owner_run("auto-h"), "Bash", ls());
    for _ in 0..200 {
        let open = r.hub.store.cards_open(None);
        if open.len() == 2 && open.iter().all(|c| c.entry_id.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let open = r.hub.store.cards_open(None);
    assert_eq!(open.len(), 2, "two cards: {open:?}");
    for c in open {
        answer(&r.hub, c.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    }
    assert!(!ordinary.await.unwrap().allow && !auto.await.unwrap().allow);
}

#[tokio::test]
async fn a_sentence_is_not_offered_when_the_block_is_a_hard_deny() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    // A hard_deny block (Data Exfiltration) is not lifted by a sentence; an exact rule still resolves before the classifier.
    let call = ask_auto(&r.hub, owner_run("auto-i"), "Bash", json!({"command": "git push origin main"}));
    let card = shown(&r.hub).await;
    assert_eq!(ids(&data(&r, &card)), ["allow", "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
    let call = ask_auto(&r.hub, owner_run("auto-j"), "Bash", ls());
    let card = shown(&r.hub).await;
    assert_eq!(ids(&data(&r, &card)), ["allow", autoallow::ALWAYS, "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
}

#[tokio::test]
async fn a_save_that_is_not_what_the_card_offered_writes_nothing() {
    let r = Rig::new().await;
    window(&r.hub, 20);
    let call = ask_auto(&r.hub, owner_run("auto-k"), "Bash", ls());
    let card = shown(&r.hub).await;
    let kept = json!({"kind": "auto", "reason": REASON, "rule": "Bash(ls /etc)", "ruleKind": "rule"});
    let why = autoallow::save(&card, &kept).await.unwrap_err();
    assert!(why.contains("no longer what was offered"), "{why}");
    assert_eq!(saved(&r).len(), 0, "the file stays as it was");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), None).unwrap();
    assert!(!call.await.unwrap().allow);
}
