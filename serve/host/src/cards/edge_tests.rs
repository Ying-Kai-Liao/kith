//! The owner's cards for a guest's edge asks (guest workbench 1.4): a host the sandbox asked for and a helper, each with
//! "Always for <person>" and the job that holds it, written to the real kith-auth before the run hears the yes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::tests::{ask, guest_run, joined, shown, window};
use super::*;
use crate::api::rig::Rig;

const CAPS: &str = "helpers: [money]\njobs:\n  - name: make-videos\n    label: Make videos\n    domains: [registry.npmjs.org, github.com]\n";

fn caps(r: &Rig, text: &str) {
    std::fs::write(r.dir.join("home/agents/house/capabilities.yaml"), text).unwrap();
}

fn net(host: &str) -> Value {
    json!({"host": host})
}

fn options(r: &Rig, card: &Card) -> Value {
    r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data
}

fn ids(data: &Value) -> Vec<String> {
    data["options"].as_array().unwrap().iter().map(|o| o["optionId"].as_str().unwrap().to_owned()).collect()
}

fn job(r: &Rig) -> Value {
    let v: Value = serde_json::from_str(&r.auth(&["jobs", "house", "--person", "sam", "--json"])).unwrap();
    v["jobs"][0].clone()
}

async fn until(what: impl Fn() -> bool) {
    for _ in 0..200 {
        if what() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_host_in_an_unticked_job_offers_always_and_the_job_and_each_keeps_only_what_it_says() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    caps(&r, CAPS);
    window(&r.hub, 20);

    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "SandboxNetworkAccess", net("github.com"));
    let card = shown(&r.hub).await;
    assert_eq!(card.approver, "owner");
    let data = options(&r, &card);
    assert_eq!(ids(&data), ["allow", edge::ALWAYS, edge::JOB, "deny"]);
    assert_eq!(data["options"][1]["name"], "Always for Sam");
    assert_eq!(data["options"][2]["name"], "Turn on Make videos: registry.npmjs.org, github.com");
    assert!(data["detail"].as_str().unwrap().ends_with("Part of Make videos, not on for Sam."), "{data}");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(edge::ALWAYS), "owner").unwrap();
    assert!(call.await.unwrap().allow);
    let domains = || {
        let v: Value = serde_json::from_str(&r.auth(&["resolve", "house", "--person", "sam"])).unwrap();
        v["access"]["domains"].clone()
    };
    until(|| domains() == json!(["github.com"])).await;
    assert_eq!(domains(), json!(["github.com"]), "always keeps the one host");
    assert_eq!(job(&r)["ticked"], false, "and never the whole job");

    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "SandboxNetworkAccess", net("registry.npmjs.org"));
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(edge::JOB), "owner").unwrap();
    assert!(call.await.unwrap().allow);
    until(|| job(&r)["ticked"] == true).await;
    assert_eq!(
        job(&r)["granted"]["domains"],
        json!(["registry.npmjs.org", "github.com"]),
        "the job as the card listed it"
    );

    // A host no job holds: once, always and no.
    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "SandboxNetworkAccess", net("pypi.org"));
    let card = shown(&r.hub).await;
    assert_eq!(ids(&options(&r, &card)), ["allow", edge::ALWAYS, "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
    assert!(call.await.unwrap().allow);
}

#[tokio::test]
async fn a_package_update_that_widens_a_ticked_job_asks_once_and_a_no_declines_that_update() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    caps(&r, CAPS);
    window(&r.hub, 20);
    assert!(r.auth_ok(&["job", "house", "sam", "make-videos"]));
    caps(&r, &CAPS.replace("github.com]", "github.com, example.com]"));

    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "SandboxNetworkAccess", net("example.com"));
    let card = shown(&r.hub).await;
    let data = options(&r, &card);
    assert_eq!(data["options"][2]["name"], "Keep Make videos on, with example.com");
    assert!(data["detail"].as_str().unwrap().contains("A package update added example.com"), "{data}");
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), "owner").unwrap();
    assert!(!call.await.unwrap().allow);
    until(|| job(&r)["declined"] == true).await;
    assert_eq!(job(&r)["declined"], true);
    assert_eq!(job(&r)["granted"]["domains"], json!(["registry.npmjs.org", "github.com"]), "the tick is as it was");

    // Declined: the host still asks, piece by piece, with no job button.
    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), "SandboxNetworkAccess", net("example.com"));
    let card = shown(&r.hub).await;
    assert_eq!(ids(&options(&r, &card)), ["allow", edge::ALWAYS, "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("deny"), "owner").unwrap();
    assert!(!call.await.unwrap().allow);
}

#[tokio::test]
async fn a_helpers_yes_is_written_before_the_run_hears_it() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    caps(&r, CAPS);
    window(&r.hub, 20);
    let input = json!({"agent": "money", "task": "check the budget"});
    assert!(!r.auth_ok(&["may", "sam", "money", "--via", "house", "--once"]));

    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), edge::HELPER_TOOL, input.clone());
    let card = shown(&r.hub).await;
    assert_eq!(ids(&options(&r, &card)), ["allow", edge::ALWAYS, "deny"]);
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some("allow"), "owner").unwrap();
    assert!(call.await.unwrap().allow);
    assert!(
        r.auth_ok(&["may", "sam", "money", "--via", "house", "--once"]),
        "the once grant is there when the run goes on"
    );
    assert!(!r.auth_ok(&["may", "sam", "money", "--via", "house"]), "and is only a once");

    let call = ask(&r.hub, guest_run("sam", "1:1:house:sam"), edge::HELPER_TOOL, input);
    let card = shown(&r.hub).await;
    answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(edge::ALWAYS), "owner").unwrap();
    assert!(call.await.unwrap().allow);
    assert!(r.auth_ok(&["may", "sam", "money", "--via", "house"]), "always lands in the row's helpers");
}

/// A member's app chat asks the owner for a host as the bot actor does: the adapter's sandbox ask is titled with the
/// host and has no tool name; the entry and the `acp` card carry what `on_permission` gives them.
async fn app_card(r: &Rig, chat: &str, host: &str) -> (Card, Value) {
    let call = json!({"toolCallId": "t1", "title": host, "kind": "fetch", "rawInput": {"host": host}});
    let tool = edge::acp_tool(&call, host).to_owned();
    let mut options = vec![
        json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"}),
        json!({"optionId": "reject", "name": "Reject", "kind": "reject_once"}),
    ];
    let (_, kept) = edge::for_acp("sam", "Sam", "house", &tool, &call["rawInput"], &mut options).await.unwrap();
    let data = json!({"status": "pending", "options": options, "edge": kept});
    let entry = r.hub.add_entry(&Lane::main(chat), EntryKind::Permission, 1, &data).unwrap();
    let new = NewCard {
        session: format!("acp:{chat}:"),
        source: "acp",
        bot_id: Some(chat.to_owned()),
        run_id: None,
        person: "sam".into(),
        agent: "house".into(),
        approver: "owner",
        tool,
        input: call["rawInput"].to_string(),
        resource: format!("net:{host}"),
        shown: host.into(),
        deadline: now_ms() + 60_000,
        reply_to: None,
        asked_by: None,
    };
    let Opened::New(card) = r.hub.store.card_open(&new, now_ms()).unwrap() else { panic!() };
    r.hub.store.card_live(&card.id, Some(&entry.id), None).unwrap();
    (r.hub.store.card(&card.id).unwrap(), data)
}

#[tokio::test]
async fn a_members_app_chat_ask_keeps_what_the_owner_picked_and_its_agent_hears_its_own_allow_once() {
    let r = Rig::new().await;
    let chat = r.owner_bot("house");
    let _sam = joined(&r, "member").await;
    caps(&r, CAPS);
    let domains = || {
        let v: Value = serde_json::from_str(&r.auth(&["resolve", "house", "--person", "sam"])).unwrap();
        v["access"]["domains"].clone()
    };

    let (card, data) = app_card(&r, &chat, "github.com").await;
    assert_eq!(ids(&data), ["allow", edge::ALWAYS, edge::JOB, "reject"]);
    assert_eq!(data["edge"]["job"], "make-videos");
    let entry = card.entry_id.as_ref().unwrap();
    assert_eq!(answer(&r.hub, entry, Some(edge::ALWAYS), "owner").unwrap(), Outcome::Recorded { forward: true });
    assert_eq!(edge::adapter_option(&data["options"], edge::ALWAYS), Some("allow"), "the actor sends its allow_once");
    until(|| domains() == json!(["github.com"])).await;
    assert_eq!(domains(), json!(["github.com"]), "kith-auth access house sam domain github.com");
    assert_eq!(job(&r)["ticked"], false);

    let (card, data) = app_card(&r, &chat, "registry.npmjs.org").await;
    let entry = card.entry_id.as_ref().unwrap();
    assert_eq!(answer(&r.hub, entry, Some(edge::JOB), "owner").unwrap(), Outcome::Recorded { forward: true });
    assert_eq!(edge::adapter_option(&data["options"], edge::JOB), Some("allow"));
    until(|| job(&r)["ticked"] == true).await;
    assert_eq!(job(&r)["granted"]["domains"], json!(["registry.npmjs.org", "github.com"]), "the job is on");
}

#[test]
fn an_always_or_a_job_reaches_the_agent_as_its_own_allow_once() {
    let offered = json!([
        {"optionId": "always", "kind": "allow_always"},
        {"optionId": edge::ALWAYS, "kind": "allow_always"},
        {"optionId": edge::JOB, "kind": "allow_always"},
        {"optionId": "once", "kind": "allow_once"},
        {"optionId": "no", "kind": "reject_once"},
    ]);
    assert_eq!(edge::adapter_option(&offered, edge::ALWAYS), Some("once"), "never kith-always or the adapter's always");
    assert_eq!(edge::adapter_option(&offered, edge::JOB), Some("once"));
    assert_eq!(edge::adapter_option(&offered, "once"), Some("once"));
    assert_eq!(edge::adapter_option(&offered, "no"), Some("no"));
    let no_once =
        json!([{"optionId": "always", "kind": "allow_always"}, {"optionId": edge::ALWAYS, "kind": "allow_always"}]);
    assert_eq!(edge::adapter_option(&no_once, edge::ALWAYS), None, "a cancel, not an id the agent never offered");
}

#[test]
fn only_a_plain_host_or_a_named_helper_is_an_edge() {
    let d = |h: &str| Some(edge::Edge::Domain(h.into()));
    assert_eq!(edge::of("SandboxNetworkAccess", &json!({"host": "a.org", "port": 443}), ""), d("a.org"));
    assert_eq!(
        edge::of("a.org", &json!({"host": "a.org"}), ""),
        d("a.org"),
        "a bot chat's ask is titled with its host"
    );
    assert_eq!(edge::of("mcp__x__go", &json!({"host": "a.org"}), ""), None, "another tool with a host field");
    assert_eq!(edge::of("SandboxNetworkAccess", &json!({"host": "a.org", "cmd": "x"}), ""), None);
    assert_eq!(edge::of("SandboxNetworkAccess", &json!({"host": "*.org"}), ""), None);
    assert_eq!(edge::of(access::TOOL, &json!({}), "access:domain:a.org"), d("a.org"));
    assert_eq!(edge::of(access::TOOL, &json!({}), "access:read:/x"), None, "a path names no job");
    assert_eq!(edge::of(edge::HELPER_TOOL, &json!({"agent": "money"}), ""), Some(edge::Edge::Helper("money".into())));
    assert_eq!(edge::of(edge::HELPER_TOOL, &json!({"agent": "a:b"}), ""), None);
}
