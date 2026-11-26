use super::*;
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Store};
use std::sync::Arc;

struct Fixture {
    hub: Arc<Hub>,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-feed-host-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "feed-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        let f = Self { hub, dir };
        f.chat("mine", None, Some("ops"));
        f.chat("sams", Some("sam"), Some("ops"));
        f.chat("group", None, None);
        f
    }

    fn chat(&self, id: &str, person: Option<&str>, agent: Option<&str>) {
        let config: BotConfig = serde_json::from_value(json!({
            "id": id, "name": id, "backend": "fixture", "cwd": self.dir, "hqAgent": agent, "person": person,
        }))
        .unwrap();
        self.hub.store.save_bot(&config).unwrap();
    }

    fn posts(&self) -> Vec<crate::store::Post> {
        self.hub.store.posts_since(0, 100).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

fn run<'a>(chat: &'a str, id: &str, ok: bool) -> RunFact<'a> {
    RunFact {
        chat,
        person: None,
        agent: None,
        entry: Some("e1"),
        key: "r1".into(),
        name: "Morning brief",
        text: "all fine",
        ok,
        attachment: None,
        kind: None,
        fact_at: 1000,
        fact_id: id.into(),
    }
}

fn page(name: &str, sha: &str) -> SharedFile {
    SharedFile { id: format!("id-{name}"), name: name.into(), size: 10, sha256: sha.into() }
}

#[test]
fn the_owners_chat_gets_a_post_and_an_invitees_none() {
    let f = Fixture::new();
    run_post(&f.hub, run("mine", "run-1", true));
    run_post(&f.hub, run("sams", "run-1", true));
    let posts = f.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].agent.as_str(), posts[0].kind.as_str()), ("ops", "plain"));
    assert_eq!(posts[0].source_chat, "mine");
}

#[test]
fn an_item_of_a_person_and_a_chat_with_no_agent_get_none() {
    let f = Fixture::new();
    run_post(&f.hub, RunFact { person: Some("sam"), ..run("mine", "run-1", true) });
    run_post(&f.hub, run("group", "run-2", true));
    run_post(&f.hub, run("gone", "run-3", true));
    assert!(f.posts().is_empty());
    run_post(&f.hub, RunFact { agent: Some("ops"), ..run("group", "run-4", true) });
    assert_eq!(f.posts().len(), 1, "a group takes the routine's agent");
}

#[test]
fn a_retried_run_makes_no_second_change_and_a_failure_is_failing() {
    let f = Fixture::new();
    run_post(&f.hub, run("mine", "run-1", true));
    let rev = f.posts()[0].rev;
    run_post(&f.hub, run("mine", "run-1", true));
    assert_eq!(f.posts()[0].rev, rev, "the same fact id again is a no-op");
    run_post(&f.hub, RunFact { fact_at: 2000, ..run("mine", "run-2", false) });
    let posts = f.posts();
    assert_eq!((posts.len(), posts[0].kind.as_str()), (1, "failing"));
}

#[test]
fn an_html_file_makes_a_page_post_and_a_text_file_none() {
    let f = Fixture::new();
    let (html, txt) = (page("report.html", "abc"), page("notes.txt", "def"));
    page_posts(&f.hub, "mine", None, None, "e1", &[&txt]);
    assert!(f.posts().is_empty());
    page_posts(&f.hub, "mine", None, None, "e1", &[&html, &txt]);
    let posts = f.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].kind, "page");
    assert_eq!(posts[0].fold_key, "page:ops:report.html");
}
