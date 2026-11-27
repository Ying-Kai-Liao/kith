use super::*;
use crate::feed::{Fold, PostSource, PostWrite, put_post};
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Store};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    hub: Arc<Hub>,
    dir: PathBuf,
    layout: Layout,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-like-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents").join("ops").join("memory")).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "like-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        let config: BotConfig = serde_json::from_value(json!({
            "id": "mine", "name": "mine", "backend": "fixture", "cwd": dir, "hqAgent": "ops",
        }))
        .unwrap();
        hub.store.save_bot(&config).unwrap();
        let layout = Layout::new(dir.join("state"), dir.join("agents"));
        Self { hub, dir, layout }
    }

    fn post(&self, title: &str) -> Post {
        let w = PostWrite {
            fold: Fold::Tool {
                fold: Some(title.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase()),
            },
            agent: "ops".into(),
            kind: "file".into(),
            title: title.into(),
            line: String::new(),
            attachment: None,
            source: PostSource { chat: "mine".into(), thread: None, entry: None },
            by: crate::feed::By::Agent,
            fact_at: 1,
            fact_id: title.into(),
        };
        put_post(&self.hub, &w).unwrap().unwrap()
    }

    fn memory(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join("agents/ops/memory").join(name)).unwrap_or_default()
    }

    fn like(&self, id: &str, liked: bool) -> Post {
        like_post_in(&self.hub, &self.layout, id, liked).unwrap()
    }

    fn noted(&self, id: &str) -> bool {
        self.hub.store.post(id).unwrap().unwrap().noted_at.is_some()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn a_like_writes_one_line_and_the_index_once_however_often_it_is_tapped() {
    let f = Fixture::new();
    let p = f.post("Shipped the thing");
    assert!(f.like(&p.id, true).liked_at.is_some());
    f.like(&p.id, true);
    f.like(&p.id, false);
    f.like(&p.id, true);
    let text = f.memory("feedback.md");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("Kai liked your Feed post \"Shipped the thing\" (file)."), "{text}");
    assert_eq!(f.memory("MEMORY.md").matches("(feedback.md)").count(), 1);
    assert!(f.noted(&p.id));
}

#[test]
fn like_then_unlike_before_noting_writes_nothing() {
    let f = Fixture::new();
    let p = f.post("Quiet");
    f.hub.store.set_post_liked(&p.id, true).unwrap();
    f.hub.store.set_post_liked(&p.id, false).unwrap();
    assert_eq!(note(&f.hub, &f.layout, &p.id).unwrap(), Noted::Nothing);
    assert_eq!(f.memory("feedback.md"), "");
    assert!(!f.noted(&p.id), "a later like can still be noted");
}

#[test]
fn a_repeat_takes_no_rev_and_an_unlike_never_removes_the_line() {
    let f = Fixture::new();
    let p = f.post("Kept");
    let liked = f.like(&p.id, true);
    assert_eq!(f.like(&p.id, true).rev, liked.rev);
    let unliked = f.like(&p.id, false);
    assert!(unliked.rev > liked.rev && unliked.liked_at.is_none());
    assert_eq!(f.like(&p.id, false).rev, unliked.rev);
    assert_eq!(f.memory("feedback.md").lines().count(), 1);
    assert!(like_post_in(&f.hub, &f.layout, "p-missing", true).is_err());
}

#[test]
fn a_crash_between_the_append_and_noted_at_writes_no_second_line() {
    let f = Fixture::new();
    let p = f.post("Crashy");
    f.hub.store.set_post_liked(&p.id, true).unwrap();
    assert_eq!(note(&f.hub, &f.layout, &p.id).unwrap(), Noted::Written);
    f.hub.store.exec_for_test(&format!("UPDATE posts SET noted_at = NULL WHERE id = '{}'", p.id));
    retry_all(&f.hub, &f.layout);
    assert_eq!(f.memory("feedback.md").lines().count(), 1);
    assert!(f.noted(&p.id));
}

#[test]
fn a_refused_title_keeps_the_like_and_gives_up_the_note() {
    let f = Fixture::new();
    let p = f.post("hidden\u{200b}text");
    assert!(f.like(&p.id, true).liked_at.is_some());
    assert_eq!(f.memory("feedback.md"), "");
    assert!(f.noted(&p.id));
}

#[test]
fn a_missing_agent_folder_gives_up_the_note() {
    let f = Fixture::new();
    let p = f.post("Orphan");
    std::fs::remove_dir_all(f.dir.join("agents/ops")).unwrap();
    f.like(&p.id, true);
    assert!(f.noted(&p.id));
}
