//! Secret cards on a real hub with a stand-in `kith-auth` (a script under the rig's temp folder that records what it was
//! given and answers from files). Nothing here touches the real `~/.kith` or the Keychain.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::resume::tests_seen;
use super::super::tests::{joined, owner_run};
use super::super::{RESUME_KEY, sweep};
use super::*;
use crate::api::devices::{Caller, Forbidden};
use crate::api::rig::Rig;
use crate::api::{dispatch, roles};
use crate::channels::hq::TEST_ENV;
use crate::remote::account::TEST_BIN;
use std::path::PathBuf;

const STUB: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
d = os.environ["KITH_FAKE"]
a = sys.argv[1:]
def log(name, text):
    with open(os.path.join(d, name), "a") as f:
        f.write(text + "\n")
def has(n):
    return os.path.exists(os.path.join(d, n))
def cat(n):
    sys.stdout.write(open(os.path.join(d, n)).read())
cmd = a[0] if a else ""
if cmd == "account-state":
    if not has("state.json"):
        sys.exit(1)
    cat("state.json")
elif cmd == "keys":
    if has("keys.json"):
        cat("keys.json")
    else:
        print("[]")
elif cmd == "set":
    stdin = sys.stdin.read()
    log("calls.log", json.dumps(a))
    log("env.log", json.dumps(dict(os.environ)))
    log("stdin.log", stdin)
    if has("delay"):
        time.sleep(float(open(os.path.join(d, "delay")).read()))
    if has("fail"):
        sys.stderr.write("boom " + stdin)
        sys.exit(3)
    log("sets.log", a[1])
    body = json.loads(stdin)
    grant = a[a.index("--grant") + 1] if "--grant" in a else None
    print(json.dumps({"profile": a[1], "service": a[1].split("/")[0], "vars": sorted(body), "granted": grant}))
elif cmd in ("grant", "revoke"):
    log("calls.log", json.dumps(a))
    if has("fail"):
        sys.exit(3)
    print("{}")
else:
    sys.exit(2)
"#;

const SESSION: &str = "6b1c2c1e-2a1d-4b38-9f55-0c1d6f9e0a11";

const NONE: &str =
    r#"{"declared":true,"secrets":["GITHUB_TOKEN"],"granted":[],"missing":["GITHUB_TOKEN"],"profiles":[]}"#;
const PROFILES: &str = r#"{"declared":true,"secrets":["GITHUB_TOKEN"],"granted":[],"missing":["GITHUB_TOKEN"],
  "profiles":[{"profile":"github/work","account":"work","owner":"kai","agents":["money"]},
              {"profile":"github/home","account":"home","owner":"kai","agents":[]}]}"#;
const GRANTED: &str = r#"{"declared":true,"secrets":["GITHUB_TOKEN"],"granted":["github/work"],"missing":[],
  "profiles":[{"profile":"github/work","account":"work","owner":"kai","agents":["house"]}]}"#;
const UNDECLARED: &str = r#"{"declared":false,"secrets":[],"granted":[],"missing":[],"profiles":[]}"#;

/// The rig with the stand-in kith-auth in place of the repo's.
pub(super) struct Fake {
    pub r: Rig,
    pub dir: PathBuf,
}

impl Fake {
    pub async fn new() -> Self {
        let r = Rig::new().await;
        let dir = r.dir.join("fake");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = r.dir.join("kith-auth-fake");
        std::fs::write(&bin, STUB).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        *TEST_BIN.lock().unwrap() = Some(bin);
        TEST_ENV.lock().unwrap().push(("KITH_FAKE", dir.clone()));
        r.owner_bot("house");
        Self { r, dir }
    }

    pub fn state(&self, json: &str) {
        std::fs::write(self.dir.join("state.json"), json).unwrap();
    }

    pub fn flag(&self, name: &str, on: bool) {
        let p = self.dir.join(name);
        if on {
            std::fs::write(p, "1").unwrap();
        } else {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn delay(&self, secs: &str) {
        std::fs::write(self.dir.join("delay"), secs).unwrap();
    }

    pub fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    pub fn sets(&self) -> usize {
        self.log("sets.log").lines().count()
    }

    pub async fn ask(&self, service: &str) -> Value {
        request(&self.r.hub, &Origin::run(&owner_run(SESSION)), service, "to open pull requests").await
    }

    /// The open secret card and its entry.
    pub fn open(&self) -> (Card, Value) {
        let card = self.r.hub.store.cards_open(None).into_iter().find(|c| c.tool == TOOL).expect("an open secret card");
        let data = self.r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data;
        (card, data)
    }

    pub fn entry(&self, card: &Card) -> Value {
        self.r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data
    }

    pub fn card(&self, card: &Card) -> Card {
        self.r.hub.store.card(&card.id).unwrap()
    }

    pub async fn provide(&self, card: &Card, account: Option<&str>, pairs: &[(&str, &str)]) -> anyhow::Result<Value> {
        provide(&self.r.hub, card.entry_id.as_ref().unwrap(), account, vals(pairs)).await
    }

    /// Moves a card's deadline in the store (the clock is real; this is how a test makes it pass).
    pub fn set_deadline(&self, card: &Card, at: i64) {
        let c = rusqlite::Connection::open(self.r.dir.join("t.db")).unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c.execute("UPDATE cards SET deadline = ?2 WHERE id = ?1", rusqlite::params![card.id, at]).unwrap();
    }
}

pub(super) fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, Zeroizing<String>> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), Zeroizing::new((*v).to_owned()))).collect()
}

/// A marker value built at run time, so the source never holds a literal that looks like a key.
pub(super) fn marker() -> String {
    format!("mk{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn said(e: &anyhow::Error) -> String {
    e.downcast_ref::<UserError>().map_or_else(|| format!("NOT A USER ERROR: {e:#}"), |u| u.0.clone())
}

// ---- the ask

#[tokio::test]
async fn a_service_the_agent_cannot_have_is_refused_without_a_card() {
    let f = Fake::new().await;
    f.state(UNDECLARED);
    let got = f.ask("github").await;
    assert_eq!((got["ok"].as_bool(), got["carded"].as_bool()), (Some(false), Some(false)), "{got}");
    assert_eq!(f.r.hub.store.cards_open(None).len(), 0);
    for bad in ["", "GitHub", "../x", "-x", "a/b", "x y"] {
        assert_eq!(f.ask(bad).await["ok"], false, "{bad:?}");
    }
    // kith-auth missing or failing is a refusal too, not a card.
    std::fs::remove_file(f.dir.join("state.json")).unwrap();
    assert_eq!(f.ask("github").await["ok"], false);
    assert_eq!(f.r.hub.store.cards_open(None).len(), 0);
}

#[tokio::test]
async fn no_account_yet_asks_for_a_key_with_one_field_per_missing_variable() {
    let f = Fake::new().await;
    f.state(NONE);
    let got = f.ask("github").await;
    assert_eq!(got["ok"], true);
    assert_eq!(got["carded"], true);
    assert!(got["message"].as_str().unwrap().contains("you won't see the key. End your turn."), "{got}");
    let (card, data) = f.open();
    assert_eq!((card.approver.as_str(), card.person.as_str(), card.state), ("owner", "", CardState::Live));
    assert_eq!(data["card"], "key");
    assert_eq!(data["secret"]["vars"], json!(["GITHUB_TOKEN"]));
    assert_eq!(data["secret"]["replaces"], false);
    assert_eq!(data["secret"]["profile"], Value::Null);
    assert_eq!(data["secret"]["state"], "pending");
    assert_eq!(data["secret"]["service"], "github");
    // An old app draws only this: a deny and a line that says to update.
    assert_eq!(data["options"], json!([{"optionId": "deny", "name": "Not now", "kind": "reject_once"}]));
    assert_eq!(data["detail"], OLD_APP_DETAIL);
    let chat = f.r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap();
    assert_eq!(chat.kind, "permission");
}

/// Rule 5: with no chat with the agent on this computer, the key card goes to the owner's Needs-you list, never to a
/// member's devices.
#[tokio::test]
async fn with_no_chat_the_key_card_is_in_the_agents_card_lane_for_the_owner_only() {
    let f = Fake::new().await;
    for row in f.r.hub.store.bots().unwrap() {
        f.r.hub.delete_bot(&row.config.id).unwrap();
    }
    f.state(NONE);
    assert_eq!(f.ask("github").await["carded"], true);
    let (card, _) = f.open();
    let entry = f.r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap();
    assert_eq!(entry.bot_id, "cards:house");
    let event = json!({"type": "entry", "rev": entry.rev, "entry": entry});
    assert!(roles::event_for(&f.r.hub.store, None, event.clone()).is_some());
    assert!(roles::event_for(&f.r.hub.store, Some("sam"), event).is_none());
}

#[tokio::test]
async fn existing_accounts_without_a_grant_make_an_access_card() {
    let f = Fake::new().await;
    f.state(PROFILES);
    f.ask("github").await;
    let (_, data) = f.open();
    assert_eq!(data["card"], "account");
    let ids: Vec<&str> = data["options"].as_array().unwrap().iter().map(|o| o["optionId"].as_str().unwrap()).collect();
    assert_eq!(ids, ["profile:github/work", "profile:github/home", "another", "deny"]);
    assert_eq!(data["accounts"][0], json!({"profile": "github/work", "account": "work", "agents": ["money"]}));
    assert_eq!(data["secret"]["vars"], json!(["GITHUB_TOKEN"]));
}

#[tokio::test]
async fn an_agent_with_a_grant_gets_a_replacing_key_card_for_that_account() {
    let f = Fake::new().await;
    f.state(GRANTED);
    f.ask("github").await;
    let (_, data) = f.open();
    assert_eq!(data["card"], "key");
    assert_eq!(data["secret"]["replaces"], true);
    assert_eq!(data["secret"]["profile"], "github/work");
    assert_eq!(data["secret"]["account"], "work");
    // Nothing is missing, so a rotation asks for every variable the service declares.
    assert_eq!(data["secret"]["vars"], json!(["GITHUB_TOKEN"]));
}

#[tokio::test]
async fn a_repeat_in_the_same_session_is_the_same_card_and_a_new_session_a_new_one() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let again = f.ask("github").await;
    assert_eq!(again["carded"], true);
    assert_eq!(f.r.hub.store.cards_open(None).len(), 1, "one open card per session and service");
    let other = request(&f.r.hub, &Origin::run(&owner_run("another-session")), "github", "x").await;
    assert_eq!(other["carded"], true);
    assert_eq!(f.r.hub.store.cards_open(None).len(), 2);
}

#[tokio::test]
async fn the_chat_tool_refuses_a_chat_with_a_person_and_a_chat_without_an_agent() {
    let f = Fake::new().await;
    f.state(NONE);
    let r = &f.r;
    // The owner's chat with the agent asks.
    let chat = r.owner_bot("house");
    let got = chat_request(&r.hub, &chat, &chat, &json!({"service": "github", "purpose": "pr"})).await.unwrap();
    assert_eq!(got["carded"], true, "{got}");
    let (card, _) = f.open();
    assert_eq!((card.source.as_str(), card.bot_id.as_deref()), ("acp", Some(chat.as_str())));
    assert_eq!(card.session, format!("acp:{chat}:"));
    // A chat with a person set (an invitee's) never asks.
    let mut cfg = r.hub.store.bot(&chat).unwrap().unwrap().config;
    cfg.id = "sam-chat".into();
    cfg.person = Some("sam".into());
    r.hub.store.save_bot(&cfg).unwrap();
    let refused = chat_request(&r.hub, "sam-chat", "sam-chat", &json!({"service": "github"})).await.unwrap();
    assert_eq!(refused["ok"], false, "{refused}");
    // No such chat.
    let none = chat_request(&r.hub, "nope", "nope", &json!({"service": "github"})).await.unwrap();
    assert_eq!(none["ok"], false);
}

// ---- provideSecret

#[tokio::test]
async fn a_key_is_written_to_stdin_only_and_then_the_card_is_saved_and_the_agent_told_the_name() {
    let f = Fake::new().await;
    f.r.hub.store.kv_set(RESUME_KEY, "on").unwrap();
    f.r.hub.terminal.set_env(crate::terminal::Env { state: f.r.dir.join("claims"), sessions: None });
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    let m = marker();
    let got = f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap();
    assert_eq!(
        got,
        json!({"saved": {"service": "github", "vars": ["GITHUB_TOKEN"], "profile": "github/work"}, "resumed": true})
    );
    assert!(!got.to_string().contains(&m));
    // kith-auth was given the profile, the flags and the grant on argv, and the value on stdin alone.
    assert_eq!(f.log("calls.log").trim(), r#"["set", "github/work", "--stdin-json", "--grant", "house"]"#);
    assert_eq!(f.log("stdin.log").trim(), json!({"GITHUB_TOKEN": m}).to_string());
    assert!(!f.log("env.log").contains(&m) && !f.log("calls.log").contains(&m));
    // The card is spent and says what became of it, by names.
    let saved = f.card(&card);
    assert_eq!(
        (saved.state, saved.answer.as_deref(), saved.answered_by.as_deref()),
        (CardState::Used, Some("saved"), Some("owner"))
    );
    let data = f.entry(&card);
    assert_eq!((data["status"].as_str(), data["secret"]["state"].as_str()), (Some("answered"), Some("saved")));
    assert_eq!(
        (data["secret"]["profile"].as_str(), data["secret"]["account"].as_str()),
        (Some("github/work"), Some("work"))
    );
    // The resume tells the session what was saved, never the value.
    for _ in 0..200 {
        if !tests_seen::spawns_for(SESSION).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let spawns = tests_seen::spawns_for(SESSION);
    assert_eq!(spawns.len(), 1, "one resume");
    let note = spawns[0].args.last().unwrap();
    assert_eq!(
        note,
        "[Kith] the owner saved GITHUB_TOKEN (github/work) for you. Use it through kith-auth exec; never print it."
    );
    assert!(!format!("{:?}", spawns[0]).contains(&m));
}

#[tokio::test]
async fn rotating_writes_into_the_granted_account_and_needs_no_account_name() {
    let f = Fake::new().await;
    f.state(GRANTED);
    f.ask("github").await;
    let (card, _) = f.open();
    // The name given is not used: the card fixed the profile.
    let m = marker();
    let got = f.provide(&card, None, &[("GITHUB_TOKEN", &m)]).await.unwrap();
    assert_eq!(got["saved"]["profile"], "github/work");
    assert!(f.log("calls.log").contains("github/work"));
}

#[tokio::test]
async fn a_bad_account_or_bad_fields_write_nothing_and_leave_the_card_open() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    let m = marker();
    for bad in
        [None, Some(""), Some("Work"), Some("a/b"), Some("-x"), Some("../x"), Some("a b"), Some(&"x".repeat(41)[..])]
    {
        let e = f.provide(&card, bad, &[("GITHUB_TOKEN", &m)]).await.unwrap_err();
        assert_eq!(said(&e), BAD_ACCOUNT, "{bad:?}");
    }
    for pairs in [
        vec![],
        vec![("GITHUB_TOKEN", "")],
        vec![("OTHER", "x")],
        vec![("GITHUB_TOKEN", "a\0b")],
        vec![("GITHUB_TOKEN", "x"), ("EXTRA", "y")],
    ] {
        let e = f.provide(&card, Some("work"), &pairs).await.unwrap_err();
        assert_eq!(said(&e), BAD_VALUES, "{pairs:?}");
    }
    let big = "x".repeat(MAX_VALUE + 1);
    assert_eq!(said(&f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &big)]).await.unwrap_err()), TOO_LONG);
    assert_eq!(f.sets(), 0);
    assert!(f.card(&card).state.open());
    // An account that is already the owner's is not silently replaced.
    std::fs::write(f.dir.join("keys.json"), r#"[{"profile":"github/work"}]"#).unwrap();
    assert_eq!(said(&f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err()), BAD_ACCOUNT);
    assert_eq!(f.sets(), 0);
}

#[tokio::test]
async fn a_failed_write_says_one_fixed_thing_leaves_the_card_open_and_can_be_tried_again() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    let m = marker();
    f.flag("fail", true);
    let e = f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err();
    // The stand-in's stderr repeats its stdin: none of it may come back.
    assert_eq!(said(&e), NOT_SAVED);
    assert!(!format!("{e:#}").contains(&m) && !format!("{e:?}").contains("boom"));
    let after = f.card(&card);
    assert!(after.state.open() && after.deadline == card.deadline, "open, with its own deadline: {after:?}");
    assert_eq!(f.entry(&card)["status"], "pending");
    f.flag("fail", false);
    f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap();
    assert_eq!(f.card(&card).state, CardState::Used);
    assert_eq!(f.sets(), 1);
}

#[tokio::test]
async fn a_closed_card_writes_nothing() {
    let f = Fake::new().await;
    f.state(NONE);
    let m = marker();
    // Denied.
    f.ask("github").await;
    let (card, _) = f.open();
    respond(&f.r.hub, card.entry_id.as_ref().unwrap(), Some("deny")).await.unwrap();
    assert_eq!(f.card(&card).state, CardState::Denied);
    assert_eq!(f.entry(&card)["secret"]["state"], "denied");
    assert_eq!(said(&f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err()), CLOSED);
    // Expired (the deadline passed, nobody swept yet).
    let other = request(&f.r.hub, &Origin::run(&owner_run("s2")), "github", "x").await;
    assert_eq!(other["carded"], true);
    let (card2, _) = f.open();
    f.set_deadline(&card2, now_ms() - 1);
    assert_eq!(said(&f.provide(&card2, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err()), CLOSED);
    sweep(&f.r.hub);
    assert_eq!(f.entry(&card2)["secret"]["state"], "expired");
    assert_eq!(f.entry(&card2)["status"], "expired");
    assert_eq!(said(&f.provide(&card2, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err()), CLOSED);
    // Not a secret card at all, and an entry that is not a card.
    let e = provide(&f.r.hub, "no-such-entry", Some("work"), vals(&[("GITHUB_TOKEN", &m)])).await.unwrap_err();
    assert_eq!(said(&e), CLOSED);
    assert_eq!(f.sets(), 0, "nothing was written");
}

#[tokio::test]
#[allow(clippy::many_single_char_names, reason = "two devices, a and b")]
async fn two_devices_at_once_write_once_and_the_second_finds_it_closed() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    f.delay("0.4");
    let m = marker();
    let pairs = [("GITHUB_TOKEN", m.as_str())];
    let (a, b) = tokio::join!(f.provide(&card, Some("work"), &pairs), f.provide(&card, Some("home"), &pairs));
    let (ok, err) = match (a, b) {
        (Ok(v), Err(e)) | (Err(e), Ok(v)) => (v, e),
        other => panic!("exactly one device wins: {other:?}"),
    };
    assert_eq!(ok["saved"]["service"], "github");
    assert_eq!(said(&err), CLOSED);
    assert_eq!(f.sets(), 1, "one write per card");
    // And once saved, a late third finds it closed too.
    assert_eq!(said(&f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err()), CLOSED);
    assert_eq!(f.sets(), 1);
}

#[tokio::test]
async fn the_deadline_passing_during_the_write_does_not_lose_the_saved_key() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    f.set_deadline(&card, now_ms() + 150);
    f.delay("0.7");
    let m = marker();
    f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap();
    // The sweep runs after the deadline: the key is in, so the card stays saved, not expired.
    sweep(&f.r.hub);
    assert_eq!(f.card(&card).state, CardState::Used);
    assert_eq!(f.entry(&card)["secret"]["state"], "saved");
    assert_eq!(f.sets(), 1);
}

#[tokio::test]
async fn the_clock_cannot_expire_a_card_while_its_key_is_being_written() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    f.set_deadline(&card, now_ms() + 100);
    f.delay("0.6");
    let m = marker();
    let hub = f.r.hub.clone();
    let sweeper = tokio::spawn(async move {
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(30)).await;
            sweep(&hub);
        }
    });
    f.provide(&card, Some("work"), &[("GITHUB_TOKEN", &m)]).await.unwrap();
    sweeper.await.unwrap();
    assert_eq!(f.card(&card).state, CardState::Used);
    assert_eq!(f.sets(), 1);
}

// ---- respondPermission on a secret card

#[tokio::test]
async fn choosing_an_account_grants_it_saves_the_card_and_resumes_the_agent() {
    let f = Fake::new().await;
    f.r.hub.store.kv_set(RESUME_KEY, "on").unwrap();
    f.r.hub.terminal.set_env(crate::terminal::Env { state: f.r.dir.join("claims"), sessions: None });
    f.state(PROFILES);
    let session = "0e2f1a52-9a9e-4f9e-8f0e-6a1f7d0b3c22";
    request(&f.r.hub, &Origin::run(&owner_run(session)), "github", "x").await;
    let (card, _) = f.open();
    let id = card.entry_id.clone().unwrap();
    // Something that was not offered, or is not a profile at all, does nothing.
    for bad in ["profile:github/other", "profile:../x", "allow", "profile:", "profile:slack/work"] {
        let e = respond(&f.r.hub, &id, Some(bad)).await.unwrap_err();
        assert!(said(&e).contains("isn't one of the choices"), "{bad}: {}", said(&e));
    }
    assert_eq!(f.log("calls.log").len(), 0);
    respond(&f.r.hub, &id, Some("profile:github/work")).await.unwrap();
    assert_eq!(f.log("calls.log").trim(), r#"["grant", "house", "github", "github/work"]"#);
    assert_eq!(f.card(&card).state, CardState::Used);
    let data = f.entry(&card);
    assert_eq!(
        (data["secret"]["state"].as_str(), data["secret"]["profile"].as_str()),
        (Some("saved"), Some("github/work"))
    );
    for _ in 0..200 {
        if !tests_seen::spawns_for(session).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let spawns = tests_seen::spawns_for(session);
    assert_eq!(spawns.len(), 1);
    assert!(spawns[0].args.last().unwrap().contains("saved GITHUB_TOKEN (github/work)"), "{:?}", spawns[0].args);
    // A second device answering finds it closed.
    assert_eq!(respond(&f.r.hub, &id, Some("profile:github/home")).await.unwrap(), json!({"closed": true}));
    assert_eq!(f.log("calls.log").lines().count(), 1);
}

#[tokio::test]
async fn another_account_turns_the_card_into_a_key_card_in_place() {
    let f = Fake::new().await;
    f.state(PROFILES);
    f.ask("github").await;
    let (card, _) = f.open();
    let id = card.entry_id.clone().unwrap();
    respond(&f.r.hub, &id, Some("another")).await.unwrap();
    let data = f.entry(&card);
    assert_eq!(data["card"], "key");
    assert_eq!(data["options"], json!([{"optionId": "deny", "name": "Not now", "kind": "reject_once"}]));
    assert_eq!(data["detail"], OLD_APP_DETAIL);
    assert_eq!(data["status"], "pending");
    assert!(f.card(&card).state.open(), "the same card, still waiting");
    // On a key card only deny is accepted.
    for bad in ["another", "profile:github/work", "allow"] {
        assert!(respond(&f.r.hub, &id, Some(bad)).await.is_err(), "{bad}");
    }
    let m = marker();
    f.provide(&card, Some("third"), &[("GITHUB_TOKEN", &m)]).await.unwrap();
    assert_eq!(f.card(&card).state, CardState::Used);
}

#[tokio::test]
async fn a_no_ends_the_card_and_the_agent_hears_nothing() {
    let f = Fake::new().await;
    f.state(NONE);
    let session = "11111111-2222-4333-8444-555555555555";
    request(&f.r.hub, &Origin::run(&owner_run(session)), "github", "x").await;
    let (card, _) = f.open();
    let id = card.entry_id.clone().unwrap();
    respond(&f.r.hub, &id, Some("deny")).await.unwrap();
    let denied = f.card(&card);
    assert_eq!((denied.state, denied.allowed), (CardState::Denied, Some(false)));
    assert_eq!(f.entry(&card)["status"], "answered");
    assert_eq!(f.r.hub.store.cards_awaiting_resume(0).len(), 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(tests_seen::spawns_for(session).len(), 0);
    assert_eq!(respond(&f.r.hub, &id, Some("deny")).await.unwrap(), json!({"closed": true}));
    // A cancel (no option) is a no as well.
    request(&f.r.hub, &Origin::run(&owner_run("22222222-2222-4333-8444-555555555555")), "github", "x").await;
    let (c2, _) = f.open();
    respond(&f.r.hub, c2.entry_id.as_ref().unwrap(), None).await.unwrap();
    assert_eq!(f.card(&c2).state, CardState::Denied);
    assert_eq!(f.entry(&c2)["status"], "cancelled");
}

// ---- who may

#[tokio::test]
async fn only_the_owner_provides_lists_or_revokes() {
    let f = Fake::new().await;
    f.state(NONE);
    f.ask("github").await;
    let (card, _) = f.open();
    let id = card.entry_id.clone().unwrap();
    let r = &f.r;
    let m = marker();
    let body = json!({"entryId": id, "account": "work", "values": {"GITHUB_TOKEN": m}});
    // Making a member needs the real kith-auth; the stand-in goes back right after.
    let stub = TEST_BIN.lock().unwrap().replace(r.bin.clone());
    let member = joined(r, "member").await;
    *TEST_BIN.lock().unwrap() = stub;
    let guest_card = id.clone();
    for (who, label) in [(member, "member")] {
        for method in ["provideSecret", "listKeys", "revokeKey", "respondPermission"] {
            let e = dispatch(&r.hub, &who, method, json!({"entryId": guest_card, "optionId": "deny", "agent": "house", "service": "github", "profile": "github/work", "values": {"GITHUB_TOKEN": m}}))
                .await
                .unwrap_err();
            assert!(e.is::<crate::api::devices::NotAllowed>() || e.is::<Forbidden>(), "{label} {method}: {e:#}");
        }
    }
    let pairing = Caller::Pairing { key: "p".into() };
    for method in ["provideSecret", "listKeys", "revokeKey"] {
        assert!(dispatch(&r.hub, &pairing, method, body.clone()).await.is_err(), "{method}");
    }
    assert!(!roles::member_may("provideSecret") && !roles::bot_may("provideSecret"));
    assert_eq!(f.sets(), 0, "no refused call wrote anything");
    // The owner's own phone (Control scope, over the channel) may: not a local-only method.
    let phone = r.device("owner-phone", None);
    let done = dispatch(&r.hub, &phone, "provideSecret", body).await.unwrap();
    assert_eq!(done["saved"]["profile"], "github/work");
    let keys = dispatch(&r.hub, &phone, "listKeys", json!({})).await.unwrap();
    assert_eq!(keys, json!({"keys": []}));
    let gone =
        dispatch(&r.hub, &phone, "revokeKey", json!({"agent": "house", "service": "github", "profile": "github/work"}))
            .await
            .unwrap();
    assert_eq!(gone, json!({"ok": true}));
    assert!(f.log("calls.log").contains(r#"["revoke", "house", "github", "--profile", "github/work"]"#));
    // Nothing but well-formed names reach kith-auth.
    for bad in [("-x", "github", "github/work"), ("house", "github", "slack/work"), ("house", "Git", "git/work")] {
        assert!(revoke_key(bad.0, bad.1, bad.2).await.is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn an_old_screen_only_device_cannot_provide() {
    let f = Fake::new().await;
    let screen_only = Caller::Device { key: "k".into(), scopes: vec![crate::store::Scope::Screen] };
    assert!(crate::api::devices::permit(&screen_only, "provideSecret").is_err());
    let control = Caller::Device { key: "k".into(), scopes: vec![crate::store::Scope::Control] };
    assert!(crate::api::devices::permit(&control, "provideSecret").is_ok());
    assert!(crate::api::devices::permit(&control, "listKeys").is_ok());
    assert!(crate::api::devices::permit(&control, "revokeKey").is_ok());
    drop(f);
}

// ---- redaction

/// A writer that keeps what tracing prints.
#[derive(Clone, Default)]
struct Tap(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Tap {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tap {
    type Writer = Tap;

    fn make_writer(&'a self) -> Tap {
        self.clone()
    }
}

fn contains_bytes(hay: &[u8], needle: &str) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Everything the host kept or sent about a key, searched for the key: the whole database file (entries, cards, kv,
/// audit, with its write-ahead log), every event, the analytics queue, push text, tracing and what a resume would start.
#[tokio::test(flavor = "current_thread")]
async fn the_key_is_in_the_keychain_call_alone_and_nowhere_the_host_kept_or_sent() {
    let f = Fake::new().await;
    f.r.hub.store.kv_set(RESUME_KEY, "on").unwrap();
    f.r.hub.terminal.set_env(crate::terminal::Env { state: f.r.dir.join("claims"), sessions: None });
    f.state(NONE);
    let tap = Tap::default();
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(tap.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    );
    let mut events = f.r.hub.events.subscribe();
    let pushed_before = crate::remote::push::SENT.lock().unwrap().len();
    let session = "7a7a7a7a-1111-4222-8333-444444444444";
    request(&f.r.hub, &Origin::run(&owner_run(session)), "github", "to open pull requests").await;
    let (card, _) = f.open();
    let m = marker();
    // Through the front door, as a phone would send it, so the router and the analytics hook see the body.
    let phone = f.r.device("owner-phone", None);
    let body = json!({"entryId": card.entry_id, "account": "work", "values": {"GITHUB_TOKEN": m}});
    let got = dispatch(&f.r.hub, &phone, "provideSecret", body).await.unwrap();
    assert!(!got.to_string().contains(&m));
    // A failed attempt on another card too: its error text is fixed.
    request(&f.r.hub, &Origin::run(&owner_run("other-session")), "github", "x").await;
    let second = f.r.hub.store.cards_open(None).into_iter().find(|c| c.tool == TOOL).unwrap();
    f.flag("fail", true);
    let e = f.provide(&second, Some("home"), &[("GITHUB_TOKEN", &m)]).await.unwrap_err();
    assert!(!format!("{e:#}").contains(&m));
    f.flag("fail", false);
    for _ in 0..200 {
        if !tests_seen::spawns_for(session).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Present only in the stand-in's stdin capture.
    assert!(f.log("stdin.log").contains(&m), "the key reached kith-auth");
    assert!(!f.log("calls.log").contains(&m) && !f.log("env.log").contains(&m), "not argv, not env");

    let mut found: Vec<&str> = Vec::new();
    let db = std::fs::read_dir(&f.r.dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("t.db"));
    for file in db {
        if contains_bytes(&std::fs::read(file.path()).unwrap(), &m) {
            found.push("the database file");
        }
    }
    let mut seen = String::new();
    while let Ok(ev) = events.try_recv() {
        seen.push_str(&ev.to_string());
    }
    assert!(seen.contains("request_secret") || seen.contains("\"secret\""), "the events carried the cards: {seen}");
    if seen.contains(&m) {
        found.push("an event");
    }
    if f.r.hub.analytics.drain_events().iter().any(|e| e.to_string().contains(&m)) {
        found.push("the analytics queue");
    }
    if crate::remote::push::SENT.lock().unwrap()[pushed_before..].iter().any(|p| p.contains(&m)) {
        found.push("a push");
    }
    assert!(crate::remote::push::SENT.lock().unwrap().len() > pushed_before, "the card did ask for a push");
    if contains_bytes(&tap.0.lock().unwrap(), &m) {
        found.push("the log");
    }
    if tests_seen::spawns_for(session).iter().any(|s| format!("{s:?}").contains(&m)) {
        found.push("a resume's args or env");
    }
    for c in f.r.hub.store.cards_open(None).iter().chain([&card, &second]) {
        let text =
            format!("{:?}{:?}", f.r.hub.store.card(&c.id), c.entry_id.as_deref().and_then(|e| f.r.hub.store.entry(e)));
        if text.contains(&m) {
            found.push("a card or its entry");
        }
    }
    assert!(found.is_empty(), "the key leaked into: {found:?}");
}

#[test]
fn the_analytics_hook_has_no_arm_for_a_key() {
    for method in ["provideSecret", "listKeys", "revokeKey", "requestSecret"] {
        assert!(
            crate::analytics::observe(&Caller::Local, method, &json!({"values": {"A": "b"}, "account": "x"})).is_none(),
            "{method}"
        );
    }
}

#[test]
fn the_owner_is_told_the_card_exists_and_no_one_else() {
    let key = json!({"type": "entry", "entry": {"botId": "b", "kind": "permission", "data": {"card": "key", "status": "pending"}}});
    let account = json!({"type": "entry", "entry": {"botId": "b", "kind": "permission", "data": {"card": "account"}}});
    for ev in [&key, &account] {
        assert!(roles::secret_card(&ev["entry"]["data"]));
    }
    assert_eq!(roles::bot_entry(&key["entry"], "c", "the owner"), None);
    assert_eq!(roles::bot_entry(&account["entry"], "c", "the owner"), None);
    // Any other permission card still reads "waiting" to a bot.
    let plain =
        json!({"id": "e", "rev": 1, "turn": 1, "createdAt": 1, "kind": "permission", "data": {"status": "pending"}});
    assert!(roles::bot_entry(&plain, "c", "the owner").is_some());
}
