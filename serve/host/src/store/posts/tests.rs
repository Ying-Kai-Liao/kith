use super::*;
use crate::feed::{By, Fold, PostSource};
use crate::store::tests::temp_store;
use crate::store::{BotConfig, EntryKind, Lane};

fn chat(s: &Store, id: &str, person: Option<&str>) {
    let cfg: BotConfig = serde_json::from_value(json!({
        "id": id, "name": id, "backend": "fixture", "cwd": "/tmp", "hqAgent": "ops", "person": person,
    }))
    .unwrap();
    s.save_bot(&cfg).unwrap();
}

fn tool(chat: &str, fold: &str, title: &str, at: i64, id: &str) -> PostWrite {
    PostWrite {
        fold: Fold::Tool { fold: Some(fold.into()) },
        agent: "ops".into(),
        kind: "file".into(),
        title: title.into(),
        line: String::new(),
        attachment: None,
        source: PostSource { chat: chat.into(), thread: Some("t1".into()), entry: Some("e1".into()) },
        by: By::Agent,
        fact_at: at,
        fact_id: id.into(),
    }
}

fn set_updated(s: &Store, id: &str, at: i64) {
    s.exec_for_test(&format!("UPDATE posts SET updated_at = {at} WHERE id = '{id}'"));
}

#[test]
fn a_post_gets_an_id_a_rev_and_keeps_both_across_folds() {
    let s = temp_store();
    chat(&s, "c1", None);
    let a = s.put_post(&tool("c1", "daily", "One", 10, "a")).unwrap().unwrap();
    assert!(a.id.starts_with("p-") && a.rev > 0);
    assert_eq!(s.post(&a.id).unwrap().unwrap(), a);
    let rev = s.current_rev();
    assert_eq!(rev, a.rev, "the post took the shared rev");
    assert!(s.put_post(&tool("c1", "daily", "One", 10, "a")).unwrap().is_none(), "the same fact twice");
    assert_eq!(s.current_rev(), rev, "a no-op takes no rev");
    let b = s.put_post(&tool("c1", "daily", "Two", 20, "b")).unwrap().unwrap();
    assert_eq!((b.id.as_str(), b.created_at), (a.id.as_str(), a.created_at));
    assert!(b.rev > a.rev && b.title == "Two");
    assert_eq!(s.posts_page(None, 10).unwrap().len(), 1, "one row per fold key");
    let other = s.put_post(&tool("c1", "weekly", "Three", 30, "c")).unwrap().unwrap();
    assert_ne!(other.id, a.id);
}

#[test]
fn a_post_never_names_the_chat_of_an_invited_person() {
    let s = temp_store();
    chat(&s, "mine", None);
    chat(&s, "sams", Some("sam"));
    chat(&s, "gone", None);
    s.delete_bot("gone").unwrap();
    let rev = s.current_rev();
    for refused in ["sams", "gone", "nobody"] {
        assert!(s.put_post(&tool(refused, "daily", "T", 1, "a")).is_err(), "{refused}");
    }
    assert_eq!((s.current_rev(), s.posts_page(None, 10).unwrap().len()), (rev, 0), "nothing was written");
    assert!(s.put_post(&tool("mine", "daily", "T", 1, "a")).unwrap().is_some());
}

#[test]
fn a_page_of_the_feed_is_newest_first_without_hidden_ones() {
    let s = temp_store();
    chat(&s, "c1", None);
    let ids: Vec<String> = (0..4)
        .map(|n| {
            let p = s.put_post(&tool("c1", &format!("f{n}"), &format!("P{n}"), n, "x")).unwrap().unwrap();
            set_updated(&s, &p.id, 1000 + n);
            p.id
        })
        .collect();
    s.set_post_hidden(&ids[2], true).unwrap();
    let titles =
        |before, limit| -> Vec<String> { s.posts_page(before, limit).unwrap().into_iter().map(|p| p.title).collect() };
    assert_eq!(titles(None, 10), ["P3", "P1", "P0"]);
    assert_eq!(titles(Some(1003), 10), ["P1", "P0"], "before is exclusive");
    assert_eq!(titles(None, 2), ["P3", "P1"]);
}

#[test]
fn a_fresh_stream_gets_the_newest_not_hidden_and_a_returning_one_hears_of_a_hide() {
    let s = temp_store();
    chat(&s, "c1", None);
    let posts: Vec<Post> =
        (0..4).map(|n| s.put_post(&tool("c1", &format!("f{n}"), "T", n, "x")).unwrap().unwrap()).collect();
    let (hidden, _) = s.set_post_hidden(&posts[1].id, true).unwrap().unwrap();
    let revs = |since, cap| -> Vec<i64> { s.posts_since(since, cap).unwrap().iter().map(|p| p.rev).collect() };
    assert_eq!(revs(0, 100), [posts[0].rev, posts[2].rev, posts[3].rev], "no hidden for a fresh client");
    assert_eq!(revs(0, 2), [posts[2].rev, posts[3].rev], "the newest, in rev order");
    assert_eq!(revs(posts[2].rev, 100), [posts[3].rev, hidden.rev], "a hide reaches one that had it");
}

#[test]
fn hiding_takes_a_rev_once_and_a_missing_post_is_none() {
    let s = temp_store();
    chat(&s, "c1", None);
    let p = s.put_post(&tool("c1", "f", "T", 1, "a")).unwrap().unwrap();
    let (h, changed) = s.set_post_hidden(&p.id, true).unwrap().unwrap();
    assert!(changed && h.hidden_at.is_some() && h.rev > p.rev && h.json()["hidden"] == true);
    let (again, changed) = s.set_post_hidden(&p.id, true).unwrap().unwrap();
    assert!(!changed && again.rev == h.rev);
    let (back, changed) = s.set_post_hidden(&p.id, false).unwrap().unwrap();
    assert!(changed && back.hidden_at.is_none());
    assert!(s.set_post_hidden("p-missing", true).unwrap().is_none());
}

#[test]
fn a_changed_post_comes_back_after_it_was_hidden() {
    let s = temp_store();
    chat(&s, "c1", None);
    let p = s.put_post(&tool("c1", "f", "T", 1, "a")).unwrap().unwrap();
    s.set_post_hidden(&p.id, true).unwrap();
    s.put_post(&tool("c1", "f", "T2", 2, "b")).unwrap();
    assert_eq!(s.posts_page(None, 10).unwrap().len(), 1);
}

#[test]
fn a_shot_lands_only_on_the_version_the_post_shows() {
    let s = temp_store();
    chat(&s, "c1", None);
    let page = |version: &str, at: i64| PostWrite {
        fold: Fold::Page { name: "p.html".into(), version: version.into() },
        kind: "page".into(),
        attachment: Some(json!({"type": "page", "file": {"name": "p.html"}, "shot": null, "version": version})),
        ..tool("c1", "x", "Page", at, version)
    };
    let p = s.put_post(&page("v1", 1)).unwrap().unwrap();
    s.put_post(&page("v2", 2)).unwrap();
    assert!(s.put_post_shot(&p.id, "v1", Some(json!({"name": "v1.png"}))).unwrap().is_none(), "a stale version");
    assert!(s.post(&p.id).unwrap().unwrap().attachment.unwrap()["shot"].is_null());
    let shown = s.put_post_shot(&p.id, "v2", Some(json!({"name": "v2.png"}))).unwrap().unwrap();
    assert_eq!(shown.attachment.unwrap()["shot"]["name"], "v2.png");
    assert!(s.put_post_shot("p-missing", "v2", None).unwrap().is_none());
    let note = s.put_post(&tool("c1", "n", "Note", 3, "n")).unwrap().unwrap();
    assert!(s.put_post_shot(&note.id, "v2", None).unwrap().is_none(), "a post that is no page");
}

#[test]
fn the_retention_pass_keeps_liked_posts_longer() {
    let s = temp_store();
    chat(&s, "c1", None);
    let day = 24 * 3600 * 1000;
    let now = 1_000 * day;
    let ids: Vec<String> =
        (0..4).map(|n| s.put_post(&tool("c1", &format!("f{n}"), "T", n, "x")).unwrap().unwrap().id).collect();
    for (id, age, liked) in [(&ids[0], 29, false), (&ids[1], 31, false), (&ids[2], 179, true), (&ids[3], 181, true)] {
        set_updated(&s, id, now - age * day);
        if liked {
            s.exec_for_test(&format!("UPDATE posts SET liked_at = 1 WHERE id = '{id}'"));
        }
    }
    assert_eq!(s.posts_purge(now).unwrap(), 2);
    assert!(s.post(&ids[0]).unwrap().is_some());
    assert!(s.post(&ids[1]).unwrap().is_none() && s.post(&ids[3]).unwrap().is_none());
    assert!(s.post(&ids[2]).unwrap().is_some());
}

#[test]
fn story_rows_name_the_newest_agent_entry_of_each_chat() {
    let s = temp_store();
    chat(&s, "c1", None);
    chat(&s, "c2", None);
    let say = |bot: &str, kind: EntryKind| s.insert_entry(&Lane::main(bot), kind, 1, &json!({"text": "x"})).unwrap();
    say("c1", EntryKind::Agent);
    let last = say("c1", EntryKind::Agent);
    say("c2", EntryKind::User);
    let rows = s.story_rows(0).unwrap();
    assert_eq!(rows.len(), 1, "a chat with only the owner's words has no story");
    assert_eq!((rows[0].0.as_str(), rows[0].2.as_str()), ("c1", last.id.as_str()));
    assert_eq!(s.story_rows(last.created_at + 1_000).unwrap().len(), 0);
}
