#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use serde_json::json;

struct Home(PathBuf);

impl Home {
    fn new() -> (Self, Layout) {
        let root = std::env::temp_dir().join(format!("kith-person-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("agents/video/memory/people")).unwrap();
        let root = root.canonicalize().unwrap();
        let layout = Layout::new(root.join("state"), root.join("agents"));
        (Self(root), layout)
    }

    fn mem(&self, name: &str) -> PathBuf {
        self.0.join("agents/video/memory").join(name)
    }

    fn put(&self, name: &str, text: &str) {
        let p = self.mem(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn names(v: &serde_json::Value) -> Vec<String> {
    v["agent"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap().to_owned()).collect()
}

fn removed(v: &serde_json::Value) -> usize {
    v["removed"].as_array().unwrap().len()
}

#[test]
fn lists_only_that_persons_files() {
    let (home, l) = Home::new();
    home.put("people/alan/likes.md", "- tea\n");
    home.put("people/alan/deep/more.md", "- more\n");
    home.put("people/alanna/x.md", "- not alan\n");
    home.put("people/bot-7a0c9f07/x.md", "- a bot\n");
    home.put("MEMORY.md", "- index\n");
    assert_eq!(
        names(&person::list(&l, "video", "alan").unwrap()),
        ["people/alan/deep/more.md", "people/alan/likes.md"]
    );
    assert_eq!(names(&person::list(&l, "video", "bot-7a0c9f07").unwrap()), ["people/bot-7a0c9f07/x.md"]);
    assert!(names(&person::list(&l, "video", "nobody").unwrap()).is_empty());
    assert_eq!(person::list(&l, "video", "alan").unwrap()["shared"], json!([]));
    // Without a person the list is whole.
    assert_eq!(names(&list_files(&l, "video").unwrap()).len(), 5);
}

#[test]
fn refuses_ids_that_could_leave_the_people_folder() {
    let (home, l) = Home::new();
    home.put("people/alan/likes.md", "- tea\n");
    home.put("secret.md", "- keep\n");
    let conn = open(&l).unwrap();
    for bad in
        ["", ".", "..", "../alan", "alan/..", "a/b", "/etc", "/", "alan/", ".hidden", "a\\b", "a\0b", "../../role"]
    {
        assert!(person::list(&l, "video", bad).is_err(), "{bad:?}");
        assert!(person::forget(&conn, &l, "video", bad, None).is_err(), "{bad:?}");
    }
    for bad in ["../../secret.md", "/abs.md", "", "a//b.md", "./likes.md"] {
        assert!(person::forget(&conn, &l, "video", "alan", Some(bad)).is_err(), "{bad:?}");
    }
    assert!(home.mem("secret.md").exists() && home.mem("people/alan/likes.md").exists());
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_folder_is_left_alone() {
    let (home, l) = Home::new();
    home.put("secret.md", "- keep\n");
    std::fs::create_dir_all(home.mem("people/alan")).unwrap();
    std::os::unix::fs::symlink(home.mem("secret.md"), home.mem("people/alan/link.md")).unwrap();
    std::os::unix::fs::symlink(home.mem("."), home.mem("people/evil")).unwrap();
    let conn = open(&l).unwrap();
    assert!(person::forget(&conn, &l, "video", "alan", Some("link.md")).is_err());
    assert_eq!(removed(&person::forget(&conn, &l, "video", "evil", None).unwrap()), 0);
    assert!(home.mem("secret.md").exists());
}

#[test]
fn forgets_one_file_or_the_folder_and_restore_brings_it_back() {
    let (home, l) = Home::new();
    home.put("people/alan/likes.md", "- tea\n");
    home.put("people/alan/deep/more.md", "- more\n");
    home.put("people/mia/likes.md", "- coffee\n");
    let conn = open(&l).unwrap();
    let one = person::forget(&conn, &l, "video", "alan", Some("deep/more.md")).unwrap();
    assert_eq!(removed(&one), 1);
    assert!(!home.mem("people/alan/deep/more.md").exists() && home.mem("people/alan/likes.md").exists());
    let all = person::forget(&conn, &l, "video", "alan", None).unwrap();
    assert_eq!(all["removed"][0]["name"], "people/alan/likes.md");
    assert!(!home.mem("people/alan/likes.md").exists());
    assert!(home.mem("people/mia/likes.md").exists());
    // Double forget, a person with no folder and a file already gone: success, nothing removed.
    assert_eq!(removed(&person::forget(&conn, &l, "video", "alan", None).unwrap()), 0);
    assert_eq!(removed(&person::forget(&conn, &l, "video", "nobody", None).unwrap()), 0);
    assert_eq!(removed(&person::forget(&conn, &l, "video", "alan", Some("likes.md")).unwrap()), 0);
    // Restore, through the history the forget wrote.
    let versions = history(&conn, &l, "video", false, "people/alan/likes.md", 10).unwrap();
    let before = versions["versions"].as_array().unwrap().iter().find(|v| v["deleted"] == false).unwrap();
    restore(&conn, &l, "video", false, "people/alan/likes.md", before["id"].as_i64().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(home.mem("people/alan/likes.md")).unwrap(), "- tea\n");
}
