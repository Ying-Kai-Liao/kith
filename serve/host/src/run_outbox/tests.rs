//! A run's outbox: each item lands once, in its person's own chat with the agent, with only the files it was given.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Store};

struct Rig {
    hub: Arc<Hub>,
    root: PathBuf,
    dir: PathBuf,
}

/// Chats with `echo`: a group first (never chosen), the owner's, and sam's.
fn rig() -> Rig {
    let dir = std::env::temp_dir().join(format!("kith-run-outbox-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("db")).unwrap();
    for (id, kind, person) in [("grp", "group", None), ("own", "agent", None), ("sam", "agent", Some("sam"))] {
        let cfg: BotConfig = serde_json::from_value(json!({
            "id": id, "name": "Echo", "backend": "claude", "cwd": dir.to_string_lossy(), "hqAgent": "echo",
            "kind": kind, "person": person,
        }))
        .unwrap();
        store.save_bot(&cfg).unwrap();
    }
    let hub = Hub::new(store, "t".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
    Rig { hub, root: dir.join("outbox"), dir }
}

fn texts(r: &Rig, bot: &str) -> Vec<Value> {
    let mut all = r.hub.store.history(bot, i64::MAX, 100).unwrap();
    all.sort_by_key(|e| e.seq);
    all.into_iter().map(|e| e.data).collect()
}

fn item(dir: &Path, stem: &str, body: &Value) {
    std::fs::write(dir.join(format!("{stem}.json")), body.to_string()).unwrap();
}

#[test]
fn an_item_lands_once_in_its_persons_chat_across_restarts() {
    let r = rig();
    let sam = make(&r.root, "echo", Some("sam")).unwrap();
    let owner = make(&r.root, "echo", None).unwrap();
    assert_ne!(sam, owner);
    item(&sam, "1", &json!({"as": "echo", "text": "for sam"}));
    item(&owner, "2", &json!({"as": "image", "text": "for the owner"}));
    drain(&r.hub, &r.root, &no_jobs);
    drain(&r.hub, &r.root, &no_jobs);

    let got = texts(&r, "sam");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["text"], "for sam");
    assert_eq!(got[0]["outbox"], json!({"agent": "echo", "person": "sam", "via": "app", "job": null, "as": "echo"}));
    assert_eq!(texts(&r, "own")[0]["outbox"]["person"], Value::Null);
    assert_eq!(texts(&r, "own").len(), 1);
    assert!(texts(&r, "grp").is_empty(), "never a group");

    // The host stopped after posting but before clearing the item: the next start posts nothing new.
    item(&sam, "1", &json!({"as": "echo", "text": "for sam"}));
    let hub = Hub::new(
        Store::open(&r.dir.join("db")).unwrap(),
        "t".into(),
        Identity::load_or_create(&r.dir).unwrap(),
        "t".into(),
        0,
    );
    drain(&hub, &r.root, &no_jobs);
    assert_eq!(texts(&r, "sam").len(), 1);
    assert!(std::fs::read_dir(&sam).unwrap().next().is_none(), "the box is empty");
}

#[test]
fn only_plain_files_inside_the_box_are_shared() {
    let r = rig();
    let sam = make(&r.root, "echo", Some("sam")).unwrap();
    let secret = r.dir.join("secret.txt");
    std::fs::write(&secret, "the owner's").unwrap();
    std::fs::write(sam.join("9-0"), "a chart").unwrap();
    std::os::unix::fs::symlink(&secret, sam.join("9-1")).unwrap();
    std::fs::hard_link(&secret, sam.join("9-2")).unwrap();
    item(
        &sam,
        "9",
        &json!({"as": "echo", "caption": "three files", "files": [
            {"file": "9-0", "name": "chart.txt"}, {"file": "9-1", "name": "link.txt"},
            {"file": "9-2", "name": "hard.txt"}, {"file": "../secret.txt", "name": "up.txt"},
        ]}),
    );
    drain(&r.hub, &r.root, &no_jobs);

    let got = texts(&r, "sam");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["text"], "three files");
    let files = got[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0]["name"], "chart.txt");
    assert_eq!(files[0]["size"], 7);
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "the owner's");
}

#[test]
fn a_schedule_run_reaches_the_jobs_person_and_its_box_goes_when_done() {
    let r = rig();
    let run = "sched-echo-news-1";
    std::fs::create_dir_all(r.root.join(run)).unwrap();
    std::fs::write(
        r.root.join(format!("{run}.json")),
        json!({"agent": "echo", "person": "sam", "via": "schedule", "job": "news"}).to_string(),
    )
    .unwrap();
    item(&r.root.join(run), "5", &json!({"as": "echo", "text": "the news"}));
    // A person with no app chat with the agent gets nothing, and the item is not kept.
    std::fs::create_dir_all(r.root.join("sched-echo-x-2")).unwrap();
    std::fs::write(
        r.root.join("sched-echo-x-2.json"),
        json!({"agent": "echo", "person": "ana", "via": "schedule", "job": "x"}).to_string(),
    )
    .unwrap();
    item(&r.root.join("sched-echo-x-2"), "6", &json!({"as": "echo", "text": "for ana"}));
    drain(&r.hub, &r.root, &no_jobs);

    let got = texts(&r, "sam");
    assert_eq!(got[0]["text"], "the news");
    assert_eq!(got[0]["outbox"]["job"], "news");
    assert_eq!(texts(&r, "own").len(), 0);
    assert!(std::fs::read_dir(r.root.join("sched-echo-x-2")).unwrap().next().is_none());
    assert!(r.root.join(format!("{run}.json")).exists(), "kept a while for a run still going");
}

fn no_jobs(_: &str) -> Option<Vec<Value>> {
    None
}

fn job(person: Option<&str>, paused: bool) -> Vec<Value> {
    vec![json!({
        "id": "daily-news", "cron": "0 7 * * *", "task": "the news", "paused": paused, "next_run": null,
        "last_run": null, "last_status": null, "source": "kith", "person": person,
    })]
}

fn op(dir: &Path, stem: &str, op: &str) {
    item(dir, stem, &json!({"as": "echo", "at": 1, "schedule": {"id": "daily-news", "op": op}}));
}

#[test]
fn a_schedule_change_shows_its_card_and_the_jobs_last_card_folds() {
    let r = rig();
    let sam = make(&r.root, "echo", Some("sam")).unwrap();
    op(&sam, "1", "added");
    drain(&r.hub, &r.root, &|_| Some(job(Some("sam"), false)));
    let got = texts(&r, "sam");
    assert_eq!(got.len(), 1);
    let card = &got[0]["richCard"];
    assert_eq!(card["type"], "schedule");
    assert_eq!(card["ref"], json!({"agent": "echo", "id": "daily-news"}));
    assert_eq!(card["snapshot"]["every"], "Every day at 7:00");
    assert_eq!(card["snapshot"]["task"], "the news");
    assert!(got[0]["text"].as_str().unwrap().starts_with("Schedule added · Daily news\nEvery day at 7:00"));
    assert_eq!(got[0]["outbox"]["person"], "sam");

    op(&sam, "2", "paused");
    drain(&r.hub, &r.root, &|_| Some(job(Some("sam"), true)));
    let got = texts(&r, "sam");
    assert_eq!(got.len(), 2);
    assert_eq!(got[0]["richCard"]["snapshot"]["folded"], "changed");
    assert_eq!(got[1]["text"], "Schedule paused · Daily news\nIt won't run until you resume it.");
    assert!(got[1]["richCard"]["snapshot"].get("folded").is_none());

    // Gone from the jobs: the card keeps what the last one knew of it.
    op(&sam, "3", "removed");
    drain(&r.hub, &r.root, &|_| Some(vec![]));
    let got = texts(&r, "sam");
    assert_eq!(got[1]["richCard"]["snapshot"]["folded"], "removed");
    assert_eq!(got[2]["text"], "Schedule removed · Daily news\nIt won't run again.");
    assert_eq!(got[2]["richCard"]["snapshot"]["every"], "Every day at 7:00");
}

#[test]
fn a_change_the_live_jobs_dont_show_posts_nothing() {
    let r = rig();
    let sam = make(&r.root, "echo", Some("sam")).unwrap();
    let reads = std::cell::Cell::new(0);
    op(&sam, "1", "paused"); // the job isn't paused
    op(&sam, "2", "made-up");
    op(&sam, "3", "removed"); // the job is still there
    drain(&r.hub, &r.root, &|_| {
        reads.set(reads.get() + 1);
        Some(job(Some("sam"), false))
    });
    assert_eq!(reads.get(), 1, "the jobs are read once a drain");
    // The owner's job, named from sam's chat.
    op(&sam, "4", "added");
    drain(&r.hub, &r.root, &|_| Some(job(None, false)));
    // The jobs couldn't be read.
    op(&sam, "5", "added");
    drain(&r.hub, &r.root, &no_jobs);
    assert_eq!(texts(&r, "sam"), Vec::<Value>::new());
    assert!(std::fs::read_dir(&sam).unwrap().next().is_none(), "and the items are not kept");
}

#[test]
fn a_scheduled_brief_comes_as_a_news_card_and_a_chats_reply_does_not() {
    let r = rig();
    let run = "sched-echo-morning-brief-1";
    std::fs::create_dir_all(r.root.join(run)).unwrap();
    std::fs::write(
        r.root.join(format!("{run}.json")),
        json!({"agent": "echo", "person": "sam", "via": "schedule", "job": "morning-brief", "cron": "0 7 * * *"})
            .to_string(),
    )
    .unwrap();
    let brief = "Your morning:\n- **Rain by noon** — take a coat (BOM, https://bom.gov.au/nsw)\n- [Kith 2.0 ships](https://github.com/k/kith) — cards in chat\n";
    item(&r.root.join(run), "1", &json!({"as": "echo", "text": brief}));
    let sam = make(&r.root, "echo", Some("sam")).unwrap();
    item(&sam, "2", &json!({"as": "echo", "text": brief}));
    drain(&r.hub, &r.root, &no_jobs);

    let mut got = texts(&r, "sam");
    assert_eq!(got.len(), 2);
    got.sort_by_key(|e| e["outbox"]["via"] != "schedule");
    let card = &got[0]["richCard"];
    assert_eq!(got[0]["text"], brief.trim(), "the text as sent is its twin");
    assert_eq!(card["type"], "news");
    assert_eq!(card["ref"], json!({"agent": "echo", "job": "morning-brief"}));
    assert_eq!(card["snapshot"]["every"], "Every day at 7:00");
    assert_eq!(card["snapshot"]["rows"][0]["title"], "Rain by noon");
    assert_eq!(card["snapshot"]["rows"][0]["source"], "BOM");
    assert_eq!(card["snapshot"]["rows"][1]["domain"], "github.com");
    assert!(got[1].get("richCard").is_none());
}
