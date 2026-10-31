//! Room secrets on a real hub with the repo's own kith-auth and a stand-in Keychain (a script that keeps its items in a
//! file under the rig's temp folder). The value is a marker built at run time; most tests look for it everywhere.
//! Nothing here touches the real `~/.kith` or Keychain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::many_single_char_names)]

use super::room_secret::{self, recipient};
use super::secret::kith_auth;
use crate::api::botguests::BotError;
use crate::api::devices::{Caller, Forbidden, NotAllowed};
use crate::api::rig::Rig;
use crate::api::{dispatch, roles};
use crate::channels::hq::TEST_ENV;
use crate::store::{BotGuest, BotState, Device, DeviceSource, Scope, now_ms};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `security`: add-generic-password through `-i`, find-generic-password, delete-generic-password. `sleep` in the rec
/// folder holds a delete for that many seconds, so a test can end a bot while a fetch has the value in hand.
const STUB: &str = r"#!/usr/bin/env python3
import fcntl, json, os, sys, time
rec = os.environ['STUB_REC']
lock = open(rec + '/lock', 'w')
fcntl.flock(lock, fcntl.LOCK_EX)   # the real Keychain is safe to share; this file is not
store = rec + '/keychain.json'
with open(rec + '/calls.log', 'a') as f:
    f.write(json.dumps({'argv': sys.argv, 'env': dict(os.environ)}) + '\n')
db = json.load(open(store)) if os.path.exists(store) else {}
a = sys.argv[1:]
if a[:1] == ['-i']:
    data = sys.stdin.read()
    for line in data.splitlines():
        w = line.split()
        if w[:1] == ['add-generic-password']:
            db[w[w.index('-s') + 1] + '#' + w[w.index('-a') + 1]] = bytes.fromhex(w[w.index('-X') + 1]).decode()
    json.dump(db, open(store, 'w'))
elif a[:1] == ['find-generic-password']:
    k = a[a.index('-s') + 1] + '#' + a[a.index('-a') + 1]
    if k not in db:
        sys.exit(44)
    print(db[k])
elif a[:1] == ['delete-generic-password']:
    if os.path.exists(rec + '/sleep'):
        time.sleep(float(open(rec + '/sleep').read()))
        db = json.load(open(store)) if os.path.exists(store) else {}
    db.pop(a[a.index('-s') + 1] + '#' + a[a.index('-a') + 1], None)
    json.dump(db, open(store, 'w'))
else:
    sys.exit(2)
";

/// Points the rig's kith-auth at a stand-in Keychain in the rig's folder; returns the folder it keeps its items in.
pub(crate) fn keychain(r: &Rig) -> PathBuf {
    let rec = r.dir.join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    let sec = r.dir.join("security");
    std::fs::write(&sec, STUB).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&sec, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    TEST_ENV.lock().unwrap().extend([("KITH_SECURITY_BIN", sec), ("STUB_REC", rec.clone())]);
    rec
}

pub(crate) struct World {
    pub(crate) r: Rig,
    pub(crate) room: String,
    pub(crate) house: String,
    pub(crate) rec: PathBuf,
}

impl World {
    /// The owner's room of `house` and bot guest `bg1` (Rex), with a stand-in Keychain.
    pub(crate) async fn new() -> Self {
        let r = Rig::new().await;
        let rec = keychain(&r);
        let house = r.owner_bot("house");
        let w = Self { room: String::new(), house, rec, r };
        w.guest("bg1", None, "bot-key");
        let room =
            w.r.hub
                .create_bot(
                    serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": [w.house]}))
                        .unwrap(),
                )
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
        let w = Self { room, ..w };
        w.set_members(&[&w.house, "bg1"]);
        w
    }

    /// A bot guest (`Rex`) with a device of its own.
    pub(crate) fn guest(&self, id: &str, owner_person: Option<&str>, key: &str) -> Caller {
        let g = BotGuest {
            bot_id: id.into(),
            person_id: format!("bot-{id}"),
            owner_person: owner_person.map(str::to_owned),
            name: "Rex".into(),
            provider: "grok".into(),
            agents: vec![],
            state: BotState::Active,
            device_key: Some(key.into()),
            cloud_device: None,
            rate_per_hour: 0,
            concurrent: 0,
            turn_timeout_s: 300,
            wake_profile: None,
            created_at: 0,
            joined_at: None,
            decided_at: None,
            decided_by: None,
            last_seen_at: None,
            revoked_at: None,
            revoked_why: None,
        };
        self.r.hub.store.put_bot_guest(&g).unwrap();
        self.r
            .hub
            .store
            .put_device(&Device {
                key: key.into(),
                name: "Rex".into(),
                platform: "bot".into(),
                source: DeviceSource::Bot,
                grant_id: Some(format!("bot:{id}")),
                scopes: vec![Scope::Control],
                lease_until: None,
                created_at: 1,
                last_seen_at: None,
                person_id: None,
            })
            .unwrap();
        self.r.hub.store.set_device_proven(key).unwrap();
        Caller::Device { key: key.into(), scopes: vec![Scope::Control] }
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn bot(&self, key: &str) -> Caller {
        Caller::Device { key: key.into(), scopes: vec![Scope::Control] }
    }

    pub(crate) fn set_members(&self, members: &[&str]) {
        self.r.hub.update_bot(&json!({"id": self.room, "members": members})).unwrap();
    }

    pub(crate) async fn send(&self, to: &str, value: &str) -> anyhow::Result<Value> {
        dispatch(
            &self.r.hub,
            &Caller::Local,
            "sendSecret",
            json!({"room": self.room, "to": to, "value": value, "label": "grok key"}),
        )
        .await
    }

    fn items(&self) -> String {
        let all = std::fs::read_to_string(self.rec.join("keychain.json")).unwrap_or_default();
        if all.trim() == "{}" { String::new() } else { all }
    }

    async fn held(&self) -> Vec<Value> {
        room_secret::held().await.unwrap()
    }

    fn row(&self, id: &str) -> crate::store::RoomSecret {
        self.r.hub.store.room_secret(id).unwrap()
    }

    /// Moves a row's time in the store (the clock is real; this is how a test makes it pass).
    pub(crate) fn set_expires(&self, id: &str, at: i64) {
        let c = rusqlite::Connection::open(self.r.dir.join("t.db")).unwrap();
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c.execute("UPDATE room_secrets SET expires_at = ?2 WHERE id = ?1", rusqlite::params![id, at]).unwrap();
    }
}

pub(crate) fn marker() -> String {
    format!("mk{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<BotError>().is_some_and(|b| b.status == 404)
}

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

// ---- the send

#[tokio::test]
async fn the_owner_hands_a_bot_a_secret_once_and_the_room_shows_a_quiet_line() {
    let w = World::new().await;
    let m = marker();
    let sent = w.send("bg1", &m).await.unwrap();
    let id = sent["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("rs_") && sent["expiresAt"].as_i64().unwrap() > now_ms(), "{sent}");
    assert!(!sent.to_string().contains(&m));
    let row = w.row(&id);
    assert_eq!((row.state.as_str(), row.recipient.as_str(), row.recipient_kind.as_str()), ("pending", "bg1", "bot"));
    // The default life is ten minutes.
    assert!((row.expires_at - row.created_at - 600_000).abs() < 2_000);
    // Only the Keychain holds it, under the item of this id; kith-auth knows who and when.
    assert!(w.items().contains(&format!("kith/once/{id}#VALUE")) && w.items().contains(&m));
    let held = w.held().await;
    assert_eq!(held.len(), 1);
    assert_eq!((held[0]["id"].as_str(), held[0]["bot"].as_str()), (Some(id.as_str()), Some("bg1")));
    // The room's quiet line: names only, no label, no value, no time.
    let line =
        w.r.hub
            .store
            .entries_since(0, 1000)
            .unwrap()
            .into_iter()
            .find(|e| e.data["notice"] == "secretSent")
            .expect("the quiet line");
    assert_eq!(line.kind.as_str(), "notice");
    let text = line.data["text"].as_str().unwrap();
    assert!(text.ends_with(" sent Rex a secret"), "{text}");
    assert!(!line.data.to_string().contains("grok key") && !line.data.to_string().contains(&m));
    assert_eq!(row.notice_seq, Some(line.seq));
    // It is the bot's, once.
    let got = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": id})).await.unwrap();
    assert_eq!(
        (got["value"].as_str(), got["label"].as_str(), got["room"].as_str()),
        (Some(m.as_str()), Some("grok key"), Some(w.room.as_str()))
    );
    assert_eq!(w.row(&id).state, "taken");
    assert!(!w.items().contains(&m), "taken: the Keychain item is gone");
    assert_eq!(w.held().await.len(), 0);
    let again = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": id})).await.unwrap_err();
    assert!(not_found(&again), "{again:#}");
}

#[tokio::test]
async fn a_send_to_someone_who_is_no_member_stores_nothing() {
    let w = World::new().await;
    let m = marker();
    // A bot that is let in but not in this room; an id that is nobody's; a member's own chat that is no agent's.
    w.guest("bg2", None, "bot-key-2");
    let room = w.room.clone();
    for to in ["bg2", "nobody", room.as_str()] {
        let e = w.send(to, &m).await.unwrap_err();
        assert_eq!(e.to_string(), "Couldn't send the secret", "{to}");
    }
    assert!(w.items().is_empty() && w.held().await.is_empty(), "no put for a plain mistake");
    assert_eq!(w.r.hub.store.room_secrets_of(&w.room, 10).len(), 0);
    // Bad values and times are refused with plain words, before anything is stored.
    for (value, ttl) in [("", 600), ("a", 30), ("a", 7200)] {
        let body = json!({"room": w.room, "to": "bg1", "value": value, "ttlS": ttl});
        assert!(dispatch(&w.r.hub, &Caller::Local, "sendSecret", body).await.is_err());
    }
    let long = "x".repeat(2000);
    assert!(w.send("bg1", &long).await.is_err());
    assert_eq!(w.items().len(), 0);
}

#[tokio::test]
async fn the_step_under_the_lock_refuses_a_member_who_left_and_the_item_is_dropped() {
    let w = World::new().await;
    let m = marker();
    // The put went through; the room was saved without the bot before the step: nothing is committed.
    let id = "rs_00000000000000aa";
    let put = kith_auth(
        &["once", "put", id, "--bot", "bg1", "--expires", &(now_ms() + 60_000).to_string()],
        Some(zeroize::Zeroizing::new(m.clone().into_bytes())),
    )
    .await
    .unwrap();
    assert!(put.ok && w.items().contains(&m));
    w.set_members(&[&w.house]);
    let row = crate::store::RoomSecret {
        id: id.into(),
        room: w.room.clone(),
        sender_kind: "owner".into(),
        sender: String::new(),
        recipient_kind: "bot".into(),
        recipient: "bg1".into(),
        agent: None,
        label: None,
        notice_seq: None,
        created_at: now_ms(),
        expires_at: now_ms() + 60_000,
        state: "pending".into(),
        done_at: None,
    };
    let before = w.r.hub.store.entries_since(0, 1000).unwrap().len();
    let done =
        w.r.hub.commit_room_secret(&row, |s| recipient(s, &w.room, "bg1").is_some(), &json!({"notice": "secretSent"}));
    assert!(done.is_err());
    assert!(w.r.hub.store.room_secret(id).is_none(), "no row");
    assert_eq!(w.r.hub.store.entries_since(0, 1000).unwrap().len(), before, "no line");
    room_secret::drop_items(&[id.to_owned()]).await;
    assert!(!w.items().contains(&m) && w.held().await.is_empty());
}

#[tokio::test]
async fn only_the_owners_own_bot_is_a_recipient() {
    let w = World::new().await;
    // A bot of an invited person can't be a member of a room today; name it anyway: the recipient check refuses it.
    w.guest("bg_theirs", Some("ann"), "bot-key-ann");
    let mut cfg = w.r.hub.store.bot(&w.room).unwrap().unwrap().config;
    cfg.members.push("bg_theirs".into());
    w.r.hub.store.save_bot(&cfg).unwrap();
    w.r.hub.store.put_room_bot(&w.room, "bg_theirs", crate::store::RoomPoint { seq: 0, rev: 0 }).unwrap();
    assert!(recipient(&w.r.hub.store, &w.room, "bg_theirs").is_none());
    assert!(recipient(&w.r.hub.store, &w.room, "bg1").is_some());
    // Not let in (revoked): nobody.
    w.r.hub.store.end_bot_guest("bg1", BotState::Revoked, "test", now_ms()).unwrap();
    assert!(recipient(&w.r.hub.store, &w.room, "bg1").is_none());
    // A member with no join point fails closed.
    w.guest("bg3", None, "bot-key-3");
    let mut cfg = w.r.hub.store.bot(&w.room).unwrap().unwrap().config;
    cfg.members.push("bg3".into());
    w.r.hub.store.save_bot(&cfg).unwrap();
    assert!(recipient(&w.r.hub.store, &w.room, "bg3").is_none(), "no point fails closed");
    // And a row made out to such a bot is not fetched: a forged row of another owner's bot.
    let theirs = w.guest("bg_x", Some("ann"), "bot-key-x");
    let row = crate::store::RoomSecret {
        id: "rs_00000000000000bb".into(),
        room: w.room.clone(),
        sender_kind: "owner".into(),
        sender: String::new(),
        recipient_kind: "bot".into(),
        recipient: "bg_x".into(),
        agent: None,
        label: None,
        notice_seq: Some(i64::MAX),
        created_at: now_ms(),
        expires_at: now_ms() + 60_000,
        state: "pending".into(),
        done_at: None,
    };
    w.r.hub.store.put_room_secret(&row).unwrap();
    let e = dispatch(&w.r.hub, &theirs, "botSecretFetch", json!({"id": row.id})).await.unwrap_err();
    assert!(not_found(&e));
}

// ---- who can ask

#[tokio::test]
async fn the_methods_are_the_owners_and_a_bot_gets_only_its_fetch() {
    let w = World::new().await;
    let m = marker();
    let id = w.send("bg1", &m).await.unwrap()["id"].as_str().unwrap().to_owned();
    for method in ["sendSecret", "withdrawSecret", "roomSecrets"] {
        assert!(!roles::member_may(method) && !roles::bot_may(method), "{method} is on a guest list");
        let body = json!({"room": w.room, "to": "bg1", "value": m, "id": id});
        let e = dispatch(&w.r.hub, &w.bot("bot-key"), method, body).await.unwrap_err();
        assert!(e.is::<NotAllowed>() || e.is::<Forbidden>(), "{method}: {e:#}");
    }
    assert!(roles::bot_may("botSecretFetch") && !roles::member_may("botSecretFetch"));
    // A member's device gets none of them, and not the fetch either.
    w.r.auth(&["person", "sam"]);
    let member = w.r.device("sam-phone", Some("sam"));
    for method in ["sendSecret", "withdrawSecret", "roomSecrets", "botSecretFetch"] {
        let body = json!({"room": w.room, "to": "bg1", "value": m, "id": id});
        assert!(dispatch(&w.r.hub, &member, method, body).await.is_err(), "{method}");
    }
    assert_eq!(w.row(&id).state, "pending", "nothing refused changed it");
}

#[tokio::test]
async fn another_bots_fetch_and_a_wrong_id_are_the_same_not_found() {
    let w = World::new().await;
    let other = w.guest("bg2", None, "bot-key-2");
    w.set_members(&[&w.house, "bg1", "bg2"]);
    let id = w.send("bg1", &marker()).await.unwrap()["id"].as_str().unwrap().to_owned();
    for call in [(&other, id.as_str()), (&w.bot("bot-key"), "rs_ffffffffffffffff"), (&w.bot("bot-key"), "")] {
        let e = dispatch(&w.r.hub, call.0, "botSecretFetch", json!({"id": call.1})).await.unwrap_err();
        assert!(not_found(&e), "{e:#}");
    }
    let e = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({})).await.unwrap_err();
    assert!(not_found(&e));
    assert_eq!(w.row(&id).state, "pending");
    // Its own owner-less 404 text is the same for all of them.
    let a = dispatch(&w.r.hub, &other, "botSecretFetch", json!({"id": id})).await.unwrap_err().to_string();
    let b = dispatch(&w.r.hub, &other, "botSecretFetch", json!({"id": "rs_ffffffffffffffff"}))
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(a, b);
}

// ---- ends

#[tokio::test]
async fn every_end_drops_the_secret_and_its_item() {
    let w = World::new().await;
    let other = w.guest("bg2", None, "bot-key-2");
    let _ = other;
    w.set_members(&[&w.house, "bg1", "bg2"]);
    // 1. Withdrawn by the owner.
    let a = w.send("bg1", &marker()).await.unwrap()["id"].as_str().unwrap().to_owned();
    let out = dispatch(&w.r.hub, &Caller::Local, "withdrawSecret", json!({"id": a})).await.unwrap();
    assert_eq!(out, json!({"withdrawn": true}));
    assert_eq!(w.row(&a).state, "dropped");
    assert!(w.held().await.is_empty() && w.items().is_empty());
    assert_eq!(
        dispatch(&w.r.hub, &Caller::Local, "withdrawSecret", json!({"id": a})).await.unwrap(),
        json!({"withdrawn": false})
    );
    // 2. The recipient is taken out of the room: the row ends with it, and a re-add doesn't bring it back.
    let b = w.send("bg1", &marker()).await.unwrap()["id"].as_str().unwrap().to_owned();
    w.set_members(&[&w.house, "bg2"]);
    assert_eq!(w.row(&b).state, "dropped");
    w.set_members(&[&w.house, "bg1", "bg2"]);
    let e = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": b})).await.unwrap_err();
    assert!(not_found(&e));
    for _ in 0..50 {
        if w.held().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(w.held().await.is_empty() && w.items().is_empty(), "the item went with it");
    // 3. The bot ends (revoked): its secrets, in every room.
    let c = w.send("bg2", &marker()).await.unwrap()["id"].as_str().unwrap().to_owned();
    assert!(crate::api::botguests::end_bot(&w.r.hub, "bg2", BotState::Revoked, "owner", "test").await);
    assert_eq!(w.row(&c).state, "dropped");
    // 4. The room is deleted.
    let d = w.send("bg1", &marker()).await.unwrap()["id"].as_str().unwrap().to_owned();
    w.r.hub.delete_bot(&w.room).unwrap();
    assert_eq!(w.row(&d).state, "dropped");
    for _ in 0..50 {
        if w.held().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(w.held().await.is_empty() && w.items().is_empty(), "no item outlives its row");
}

#[tokio::test]
async fn a_bot_ended_between_the_claim_and_the_answer_gets_no_value() {
    let w = World::new().await;
    let m = marker();
    let id = w.send("bg1", &m).await.unwrap()["id"].as_str().unwrap().to_owned();
    // Hold `once take` at its delete, after it read the value, so the end lands while the host has it.
    std::fs::write(w.rec.join("sleep"), "1.5").unwrap();
    let hub = w.r.hub.clone();
    let bot = w.bot("bot-key");
    let fetch = {
        let id = id.clone();
        tokio::spawn(async move { dispatch(&hub, &bot, "botSecretFetch", json!({"id": id})).await })
    };
    for _ in 0..100 {
        if w.row(&id).state == "taking" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(w.row(&id).state, "taking", "the claim is made");
    assert!(crate::api::botguests::end_bot(&w.r.hub, "bg1", BotState::Revoked, "owner", "test").await);
    let answer = fetch.await.unwrap();
    let e = answer.expect_err("a revoked bot is answered no value");
    assert!(!format!("{e:#}").contains(&m));
    assert_eq!(w.row(&id).state, "dropped");
    assert!(!w.items().contains(&m));
}

#[tokio::test]
async fn expiry_ends_pending_and_stale_taking_rows_and_the_orphan_sweep_waits_out_its_grace() {
    let w = World::new().await;
    let m = marker();
    let a = w.send("bg1", &m).await.unwrap()["id"].as_str().unwrap().to_owned();
    // A restart left a claim `taking` and the time then passed.
    assert!(w.r.hub.store.claim_room_secret(&a, now_ms()));
    w.set_expires(&a, now_ms() - 1);
    // A once item with no row, just made: the sweep leaves it (a send is between its put and its row)...
    let orphan = "rs_00000000000000cc";
    let at = (now_ms() + 600_000).to_string();
    let args = ["once", "put", orphan, "--bot", "bg1", "--expires", at.as_str()];
    assert!(kith_auth(&args, Some(zeroize::Zeroizing::new(b"x".to_vec()))).await.unwrap().ok);
    room_secret::sweep(&w.r.hub).await;
    assert_eq!(w.row(&a).state, "expired");
    let held: Vec<String> = w.held().await.iter().map(|i| i["id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(held, vec![orphan.to_owned()], "the expired one's item is dropped; the fresh orphan stays");
    // ...and drops it once its entry is older than the grace.
    let auth = w.r.dir.join("auth.json");
    let mut j: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    j["once"][orphan]["created"] = json!(now_ms() - 10 * 60 * 1000);
    std::fs::write(&auth, j.to_string()).unwrap();
    room_secret::sweep(&w.r.hub).await;
    assert_eq!(w.held().await.len(), 0);
}

// ---- a Kith agent recipient

#[tokio::test]
async fn a_kith_agent_is_told_in_its_prompt_and_the_sweep_sees_it_used() {
    let w = World::new().await;
    let m = marker();
    let sent = w.send(&w.house, &m).await.unwrap();
    let id = sent["id"].as_str().unwrap().to_owned();
    let row = w.row(&id);
    assert_eq!((row.recipient_kind.as_str(), row.agent.as_deref()), ("agent", Some("house")));
    assert_eq!(w.held().await[0]["agent"], "house");
    let lines = room_secret::prompt_lines(&w.r.hub, &w.room, &w.house);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(&id) && lines[0].contains("grok key") && lines[0].contains("kith-auth exec --secret"));
    assert!(!lines[0].contains(&m));
    assert_eq!(room_secret::prompt_lines(&w.r.hub, &w.room, "someone else").len(), 0);
    // A bot can't fetch an agent's secret, and the owner's list shows it without a value.
    let e = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": id})).await.unwrap_err();
    assert!(not_found(&e));
    let listed = dispatch(&w.r.hub, &Caller::Local, "roomSecrets", json!({"room": w.room})).await.unwrap();
    assert_eq!(listed["secrets"][0]["id"], id);
    assert!(!listed.to_string().contains(&m));
    // The agent uses it, once, in its own command's environment.
    let used = kith_auth(&["exec", "--for", "house", "--secret", &id, "--", "true"], None).await.unwrap();
    assert!(used.ok);
    assert!(w.items().is_empty(), "taken from the Keychain");
    room_secret::sweep(&w.r.hub).await;
    assert_eq!(w.row(&id).state, "taken");
    assert!(room_secret::prompt_lines(&w.r.hub, &w.room, &w.house).is_empty(), "no longer offered");
}

// ---- the bot's side

#[tokio::test]
async fn the_bot_hears_of_it_by_event_and_in_its_room_read_and_nobody_else_does() {
    let w = World::new().await;
    let other = w.guest("bg2", None, "bot-key-2");
    let _ = other;
    w.set_members(&[&w.house, "bg1", "bg2"]);
    let mut events = w.r.hub.events.subscribe();
    let m = marker();
    let id = w.send("bg1", &m).await.unwrap()["id"].as_str().unwrap().to_owned();
    let mut seen = vec![];
    while let Ok(ev) = events.try_recv() {
        seen.push(ev);
    }
    let store = &w.r.hub.store;
    w.r.auth(&["person", "sam"]);
    let secret_events: Vec<&Value> = seen.iter().filter(|e| e["type"] == "roomSecret").collect();
    assert_eq!(secret_events.len(), 1);
    let ev = secret_events[0].clone();
    assert!(ev["label"] == "grok key" && ev["id"] == id.as_str());
    // The owner hears it; the recipient bot too; another bot and an invited person do not.
    assert!(roles::event_for(store, None, ev.clone()).is_some());
    let mine = roles::event_for_bot(store, "bg1", ev.clone()).expect("the recipient");
    assert_eq!(
        (mine["type"].as_str(), mine["room"].as_str(), mine["id"].as_str()),
        (Some("roomSecret"), Some(w.room.as_str()), Some(id.as_str()))
    );
    assert!(mine.get("to").is_none());
    assert!(roles::event_for_bot(store, "bg2", ev.clone()).is_none());
    assert!(roles::event_for(store, Some("sam"), ev.clone()).is_none());
    // The quiet line reaches the owner and no bot, as every notice.
    for e in &seen {
        for who in ["bg1", "bg2"] {
            let got = roles::event_for_bot(store, who, e.clone());
            assert!(!got.is_some_and(|g| g.to_string().contains("secretSent")), "{who}: {e}");
        }
        assert!(roles::event_for_bot(store, "bg2", e.clone()).is_none_or(|g| g["type"] != "roomSecret"));
    }
    // botRoomRead lists this bot's pending secret and the other's lists none; neither carries a notice or a value.
    let read =
        dispatch(&w.r.hub, &w.bot("bot-key"), "botRoomRead", json!({"room": w.room, "limit": 200})).await.unwrap();
    assert_eq!(read["secrets"].as_array().unwrap().len(), 1, "{read}");
    assert_eq!(read["secrets"][0]["id"], id);
    assert_eq!(read["secrets"][0]["label"], "grok key");
    assert!(read["secrets"][0].get("value").is_none());
    assert!(!read.to_string().contains("secretSent") && !read.to_string().contains(&m));
    let theirs = dispatch(&w.r.hub, &other, "botRoomRead", json!({"room": w.room})).await.unwrap();
    assert_eq!(theirs["secrets"], json!([]));
    // Once fetched, the list is empty again.
    dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": id})).await.unwrap();
    let after = dispatch(&w.r.hub, &w.bot("bot-key"), "botRoomRead", json!({"room": w.room})).await.unwrap();
    assert_eq!(after["secrets"], json!([]));
}

// ---- nowhere the host kept or sent it

/// Everything the host kept or sent about a secret, searched for its value: the whole database file with its log,
/// every event as the owner, another bot and the recipient hear it, the audit rows, tracing, analytics, push, kith-auth's
/// file and log. Only the recipient's fetch answer holds it.
#[tokio::test(flavor = "current_thread")]
async fn the_value_is_nowhere_the_host_kept_or_sent_except_the_fetch_answer() {
    let w = World::new().await;
    let other = w.guest("bg2", None, "bot-key-2");
    w.set_members(&[&w.house, "bg1", "bg2"]);
    let tap = Tap::default();
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(tap.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    );
    let mut events = w.r.hub.events.subscribe();
    let pushed = crate::remote::push::SENT.lock().unwrap().len();
    let m = marker();
    // A phone sends it, through the front door, so the router, the audit and the analytics hook all see the body.
    let phone = w.r.device("owner-phone", None);
    let body = json!({"room": w.room, "to": "bg1", "value": m, "label": "grok key"});
    let sent = dispatch(&w.r.hub, &phone, "sendSecret", body).await.unwrap();
    let id = sent["id"].as_str().unwrap().to_owned();
    // A refused attempt by a bot carrying the same value, and a fetch by the wrong bot.
    let _ = dispatch(&w.r.hub, &w.bot("bot-key"), "sendSecret", json!({"room": w.room, "to": "bg2", "value": m})).await;
    let _ = dispatch(&w.r.hub, &other, "botSecretFetch", json!({"id": id})).await;
    // The sweep, the lists and the reads run with the secret pending.
    room_secret::sweep(&w.r.hub).await;
    let _ = dispatch(&w.r.hub, &phone, "roomSecrets", json!({"room": w.room})).await.unwrap();
    let read = dispatch(&w.r.hub, &w.bot("bot-key"), "botRoomRead", json!({"room": w.room})).await.unwrap();
    assert!(!read.to_string().contains(&m));
    assert!(
        roles::event_for_bot(
            &w.r.hub.store,
            "bg2",
            json!({"type": "roomSecret", "room": w.room, "id": id, "to": "bg1"})
        )
        .is_none()
    );
    let got = dispatch(&w.r.hub, &w.bot("bot-key"), "botSecretFetch", json!({"id": id})).await.unwrap();
    assert_eq!(got["value"], m.as_str(), "the recipient has it");

    let mut found: Vec<&str> = vec![];
    // The database file, with its write-ahead log.
    for file in std::fs::read_dir(&w.r.dir).unwrap().filter_map(Result::ok) {
        if file.file_name().to_string_lossy().starts_with("t.db")
            && contains_bytes(&std::fs::read(file.path()).unwrap(), &m)
        {
            found.push("the database file");
        }
    }
    // Events, as each of them is heard.
    let mut all = String::new();
    while let Ok(ev) = events.try_recv() {
        let store = &w.r.hub.store;
        for heard in [
            roles::event_for(store, None, ev.clone()),
            roles::event_for(store, Some("sam"), ev.clone()),
            roles::event_for_bot(store, "bg2", ev.clone()),
            roles::event_for_bot(store, "bg1", ev.clone()),
        ]
        .into_iter()
        .flatten()
        {
            all.push_str(&heard.to_string());
        }
        all.push_str(&ev.to_string());
    }
    assert!(all.contains("roomSecret") && all.contains("secretSent"), "the events carried the send");
    if all.contains(&m) {
        found.push("an event");
    }
    if format!("{:?}", w.r.hub.store.audit_list(0, None).unwrap()).contains(&m) {
        found.push("an audit row");
    }
    if w.r.hub.analytics.drain_events().iter().any(|e| e.to_string().contains(&m)) {
        found.push("the analytics queue");
    }
    if crate::remote::push::SENT.lock().unwrap()[pushed..].iter().any(|p| p.contains(&m)) {
        found.push("a push");
    }
    if contains_bytes(&tap.0.lock().unwrap(), &m) {
        found.push("the log");
    }
    for name in ["auth.json", "auth.log", "auth.json.lock"] {
        if std::fs::read(w.r.dir.join(name)).is_ok_and(|b| contains_bytes(&b, &m)) {
            found.push("kith-auth's file or log");
        }
    }
    let rows = format!("{:?}", w.r.hub.store.room_secrets_of(&w.room, 50));
    if rows.contains(&m) {
        found.push("a room_secrets row");
    }
    let entries = format!("{:?}", w.r.hub.store.entries_since(0, 10_000).unwrap());
    if entries.contains(&m) {
        found.push("an entry");
    }
    // The stand-in Keychain was given it by hex on stdin and never on an argv or in an env.
    let calls = std::fs::read_to_string(w.rec.join("calls.log")).unwrap();
    let hex: String = m.bytes().fold(String::new(), |mut h, b| {
        use std::fmt::Write;
        let _ = write!(h, "{b:02x}");
        h
    });
    if calls.contains(&m) || calls.contains(&hex) {
        found.push("an argv or env of security");
    }
    assert!(found.is_empty(), "the value leaked into: {found:?}");
}

#[test]
fn analytics_maps_sending_a_secret_to_no_event() {
    let body = json!({"room": "r", "to": "b", "value": "v"});
    for method in ["sendSecret", "withdrawSecret", "roomSecrets", "botSecretFetch"] {
        assert!(crate::analytics::observe(&Caller::Local, method, &body).is_none(), "{method}");
    }
}
