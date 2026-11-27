use super::*;
use crate::remote::identity::Identity;
use crate::runauth::ask_identity;
use crate::store::Store;
use std::sync::Arc;

struct Fixture {
    hub: Arc<Hub>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-feed-agent-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "feed-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        let f = Self { hub, dir };
        f.chat("mine", "ops", None);
        f.chat("sams", "ops", Some("sam"));
        f.chat("others", "other", None);
        f
    }

    fn chat(&self, id: &str, agent: &str, person: Option<&str>) {
        let config: BotConfig = serde_json::from_value(json!({
            "id": id, "name": id, "backend": "fixture", "cwd": self.dir, "hqAgent": agent, "person": person,
        }))
        .unwrap();
        self.hub.store.save_bot(&config).unwrap();
    }

    async fn post(&self, session: &str, args: Value) -> Value {
        agent_post(&self.hub, &ask_identity("ops", session, None), &args).await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

fn card(title: &str) -> Value {
    json!({"kind": "note", "title": title})
}

#[tokio::test]
async fn the_author_is_the_tokens_agent_and_the_chat_the_sessions() {
    let f = Fixture::new();
    // `agent` and `chat` are not arguments: whatever a request says is dropped by the gate, and not read here either.
    let mut args = card("Done");
    args["agent"] = "other".into();
    args["chat"] = "others".into();
    let said = f.post("acp:mine:thr1", args).await;
    assert_eq!(said["ok"], true, "{said}");
    let p = &f.hub.store.posts_page(None, 10).unwrap()[0];
    assert_eq!((p.agent.as_str(), p.by.as_str()), ("ops", "agent"));
    assert_eq!((p.source_chat.as_str(), p.source_thread.as_deref()), ("mine", Some("thr1")));
}

#[tokio::test]
async fn any_other_session_posts_from_the_agents_owner_chat() {
    let f = Fixture::new();
    for session in ["", "claude-abc", "acp:"] {
        // The title is required, and `""` is one of the sessions: the title is not the session.
        assert_eq!(f.post(session, card(&format!("from {session}"))).await["ok"], true);
    }
    let posts = f.hub.store.posts_page(None, 10).unwrap();
    assert_eq!(posts.len(), 3);
    assert!(posts.iter().all(|p| p.source_chat == "mine" && p.source_thread.is_none()));
}

#[tokio::test]
async fn a_chat_of_an_invited_person_a_deleted_chat_and_no_chat_are_refused() {
    let f = Fixture::new();
    let sams = f.post("acp:sams:", card("x")).await;
    assert_eq!(sams["ok"], false);
    assert!(sams["error"].as_str().unwrap().contains("invited person"), "{sams}");
    f.hub.store.delete_bot("mine").unwrap();
    assert_eq!(f.post("acp:mine:", card("x")).await["ok"], false, "a deleted chat");
    let none = f.post("", card("x")).await;
    assert_eq!(none["ok"], false, "no owner chat left for the agent: {none}");
    assert_eq!(f.hub.store.posts_page(None, 10).unwrap().len(), 0);
}

#[tokio::test]
async fn twelve_new_cards_a_day_and_a_fold_that_replaces_does_not_count() {
    let f = Fixture::new();
    for n in 0..11 {
        assert_eq!(f.post("acp:mine:", card(&format!("c{n}"))).await["ok"], true);
    }
    let fold = |title: &str| json!({"kind": "note", "title": title, "fold": "daily"});
    assert_eq!(f.post("acp:mine:", fold("one")).await["ok"], true, "the twelfth");
    let over = f.post("acp:mine:", card("thirteen")).await;
    assert_eq!(over["ok"], false);
    assert!(over["error"].as_str().unwrap().starts_with("feed limit for today: fold into an earlier post"), "{over}");
    assert_eq!(f.post("acp:mine:", fold("two")).await["ok"], true, "a fold that replaces a card");
    assert_eq!(f.hub.store.posts_page(None, 50).unwrap().len(), 12);
    // another agent has its own day
    let other = agent_post(&f.hub, &ask_identity("other", "acp:others:", None), &card("mine")).await;
    assert_eq!(other["ok"], true, "{other}");
}

#[tokio::test]
async fn a_merge_names_its_pr_and_folds_into_the_agents_day() {
    let f = Fixture::new();
    let merge = |pr: &str| json!({"kind": "merges", "title": "Fix it", "pr": pr});
    assert_eq!(f.post("", merge("acme/app#1")).await["ok"], true);
    assert_eq!(f.post("", merge("acme/app#2")).await["ok"], true);
    assert_eq!(f.post("", merge("acme/app#2")).await["ok"], true, "a repeat is quiet");
    assert_eq!(f.post("", json!({"kind": "merges", "title": "x"})).await["ok"], false, "no pr");
    assert_eq!(f.post("", merge("not a pr")).await["ok"], false);
    assert_eq!(f.post("", json!({"kind": "note", "title": "x", "pr": "acme/app#3"})).await["ok"], false);
    let posts = f.hub.store.posts_page(None, 10).unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].kind.as_str(), posts[0].items.len()), ("merges", 2));
}

#[tokio::test]
async fn numbers_links_and_only_one_attachment() {
    let f = Fixture::new();
    let n = json!({"kind": "number", "title": "Users", "number": {"value": 12.5, "unit": "k", "delta": -1.0}});
    assert_eq!(f.post("", n).await["ok"], true);
    assert_eq!(f.post("", json!({"kind": "link", "title": "Doc", "url": "https://a.b/c"})).await["ok"], true);
    assert_eq!(f.post("", json!({"kind": "link", "title": "Doc", "url": "http://a.b/c"})).await["ok"], false);
    let two = json!({"kind": "x", "title": "t", "url": "https://a.b", "number": {"value": 1}});
    assert_eq!(f.post("", two).await["ok"], false);
    let attachments: Vec<Value> =
        f.hub.store.posts_page(None, 10).unwrap().into_iter().filter_map(|p| p.attachment).collect();
    assert_eq!(attachments.len(), 2);
}

#[tokio::test]
async fn a_file_is_copied_into_the_chats_files_and_html_is_a_page() {
    let f = Fixture::new();
    let html = f.dir.join("report.html");
    std::fs::write(&html, "<h1>hi</h1>").unwrap();
    let args = |p: &Path| json!({"kind": "file", "title": "Report", "file": p.to_str().unwrap()});
    assert_eq!(f.post("acp:mine:", args(&html)).await["ok"], true);
    // the same version again: no new card, and no copy left behind
    let before = std::fs::read_dir(crate::chat::files::root("mine")).unwrap().count();
    assert_eq!(f.post("acp:mine:", args(&html)).await["ok"], true);
    assert_eq!(std::fs::read_dir(crate::chat::files::root("mine")).unwrap().count(), before);
    let posts = f.hub.store.posts_page(None, 10).unwrap();
    assert_eq!(posts.len(), 1);
    let a = posts[0].attachment.as_ref().unwrap();
    assert_eq!((posts[0].kind.as_str(), a["type"].as_str()), ("page", Some("page")));
    assert_eq!(a["version"], a["file"]["sha256"]);
    // a relative path, and a file that is not there
    assert_eq!(f.post("acp:mine:", json!({"kind": "file", "title": "t", "file": "report.html"})).await["ok"], false);
    assert_eq!(f.post("acp:mine:", args(&f.dir.join("missing.pdf"))).await["ok"], false);
}
