use super::*;
use crate::feed::{By, Fold, PostSource, PostWrite, put_post};
use crate::remote::identity::Identity;
use crate::store::{BotConfig, Store};
use serde_json::{Value, json};

fn file() -> SharedFile {
    SharedFile { id: uuid::Uuid::new_v4().to_string(), name: "board.html".into(), size: 10, sha256: "v1".into() }
}

fn job(post: &str, version: &str) -> Job {
    Job { hub: Weak::new(), post: post.into(), version: version.into(), chat: "mine".into(), file: file() }
}

#[test]
fn the_same_post_and_version_is_queued_once() {
    let mut q = Queue::default();
    assert!(q.push(job("p1", "v1")));
    assert!(!q.push(job("p1", "v1")));
    assert!(q.push(job("p1", "v2")), "a new version is a new job");
    assert!(q.push(job("p2", "v1")));
    assert_eq!(q.waiting.len(), 3);
}

#[test]
fn a_version_is_tried_once_even_after_its_job_ran() {
    let mut q = Queue::default();
    assert!(q.push(job("p1", "v1")));
    q.waiting.pop_front();
    assert!(!q.push(job("p1", "v1")), "one try per version: no retry loop");
}

#[test]
fn the_memory_of_tried_versions_is_bounded() {
    let mut q = Queue::default();
    for i in 0..=REMEMBERED {
        q.push(job(&format!("p{i}"), "v"));
    }
    assert_eq!(q.tried.len(), REMEMBERED);
    assert!(q.push(job("p0", "v")), "the oldest was forgotten");
}

struct Fixture {
    hub: Arc<Hub>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-feed-shot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let hub = Hub::new(store, "feed-test".into(), Identity::load_or_create(&dir).unwrap(), "test".into(), 0);
        let config: BotConfig = serde_json::from_value(json!({
            "id": "mine", "name": "mine", "backend": "fixture", "cwd": dir, "hqAgent": "ops",
        }))
        .unwrap();
        hub.store.save_bot(&config).unwrap();
        Self { hub, dir }
    }

    fn page(&self, version: &str, fact_at: i64) -> Post {
        let attachment = json!({"type": "page", "file": file(), "shot": null, "version": version});
        let w = PostWrite {
            fold: Fold::Page { name: "board.html".into(), version: version.into() },
            agent: "ops".into(),
            kind: "page".into(),
            title: "board.html".into(),
            line: String::new(),
            attachment: Some(attachment),
            source: PostSource { chat: "mine".into(), thread: None, entry: None },
            by: By::Host,
            fact_at,
            fact_id: version.into(),
        };
        put_post(&self.hub, &w).unwrap().unwrap()
    }

    fn png(&self) -> PathBuf {
        let p = self.dir.join("shot.png");
        std::fs::write(&p, b"\x89PNG fake").unwrap();
        p
    }

    fn root(&self) -> PathBuf {
        self.dir.join("files")
    }

    fn stored(&self) -> usize {
        std::fs::read_dir(self.root()).map_or(0, Iterator::count)
    }

    fn shot_of(&self, id: &str) -> Value {
        let post = self.hub.store.posts_since(0, 10).unwrap().into_iter().find(|p| p.id == id).unwrap();
        post.attachment.unwrap()["shot"].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

#[test]
fn a_picture_of_the_shown_version_is_set_and_kept() {
    let fx = Fixture::new();
    let post = fx.page("v1", 1);
    let j = Job { post: post.id.clone(), ..job("", "v1") };
    set_picture(&fx.hub, &fx.root(), &j, &fx.png()).unwrap();
    assert_eq!(fx.shot_of(&post.id)["name"], "board.png");
    assert_eq!(fx.stored(), 1, "the PNG is a file of the chat");
}

#[test]
fn a_picture_of_an_old_version_is_dropped_and_its_file_removed() {
    let fx = Fixture::new();
    let post = fx.page("v1", 1);
    let newer = fx.page("v2", 2);
    assert_eq!(post.id, newer.id, "the page folds into one card");
    let j = Job { post: post.id.clone(), ..job("", "v1") };
    set_picture(&fx.hub, &fx.root(), &j, &fx.png()).unwrap();
    assert!(fx.shot_of(&post.id).is_null(), "v1's picture must not show on v2");
    assert_eq!(fx.stored(), 0);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn without_a_mac_there_is_no_shooter() {
    assert_eq!(cli(Path::new("a.html"), Path::new("a.png")), 1);
    let error = run_helper(Path::new("a.html"), Path::new("a.png"), RUN_LIMIT, true).unwrap_err();
    assert_eq!(error.to_string(), NO_SHOOTER);
}
