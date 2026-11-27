use super::*;
use crate::feed::{By, PostSource};
use chrono::{Local, TimeZone};

fn noon(day: u32) -> i64 {
    Local.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap().timestamp_millis()
}

fn write(fold: Fold, kind: &str, title: &str, fact_at: i64, fact_id: &str) -> PostWrite {
    PostWrite {
        fold,
        agent: "ops".into(),
        kind: kind.into(),
        title: title.into(),
        line: String::new(),
        attachment: None,
        source: PostSource { chat: "chat".into(), thread: None, entry: None },
        by: By::Host,
        fact_at,
        fact_id: fact_id.into(),
    }
}

impl PostWrite {
    fn with_kind(mut self, kind: &str) -> Self {
        self.kind = kind.into();
        self
    }
}

fn run(key: &str, ok: bool, at: i64, id: &str) -> PostWrite {
    let kind = if ok { "number" } else { "failing" };
    write(Fold::Routine { key: key.into(), ok }, kind, &format!("run {id}"), at, id)
}

/// The row of one fold key as `Store::put_post` keeps it: what `decide` answers is stored, a no-op leaves it.
struct Row(Option<Post>);

impl Row {
    fn put(&mut self, w: &PostWrite) -> bool {
        let next = decide(self.0.as_ref(), w, &key(w), 5_000_000).unwrap();
        let changed = next.is_some();
        if let Some(mut p) = next {
            if self.0.is_none() {
                p.id = "p-1".into();
            }
            self.0 = Some(p);
        }
        changed
    }

    fn post(&self) -> &Post {
        self.0.as_ref().unwrap()
    }
}

fn pr(n: u32, at: i64) -> PostWrite {
    write(Fold::Merges { pr: format!("o/r#{n}"), title: format!("change {n}") }, "merges", "", at, &format!("o/r#{n}"))
}

#[test]
fn a_late_older_failed_run_counts_in_the_streak_and_does_not_replace_the_newer() {
    let mut row = Row(None);
    assert!(row.put(&run("r", false, 2000, "r2")));
    assert_eq!(row.post().streak, 1);
    assert!(row.put(&run("r", false, 1000, "r1")), "the run is recorded");
    let p = row.post();
    assert_eq!((p.streak, p.title.as_str(), p.fact_id.as_str()), (2, "run r2", "r2"), "r2 stays shown");
    assert_eq!(p.kind, "failing");
    assert_eq!(p.items.iter().map(|i| i["id"].as_str().unwrap()).collect::<Vec<_>>(), ["r1", "r2"], "in run order");
}

#[test]
fn a_streak_is_counted_back_from_the_newest_run_in_run_order() {
    let mut row = Row(None);
    row.put(&run("r", false, 1000, "r1"));
    row.put(&run("r", false, 2000, "r2"));
    row.put(&run("r", true, 3000, "r3"));
    assert_eq!((row.post().streak, row.post().kind.as_str()), (0, "number"), "a good run ends it");
    row.put(&run("r", false, 4000, "r4"));
    assert_eq!(row.post().streak, 1);
    // An older good run landing late changes nothing about where the streak ends.
    row.put(&run("r", true, 500, "r0"));
    assert_eq!((row.post().streak, row.post().fact_id.as_str()), (1, "r4"));
}

#[test]
fn a_late_older_good_run_does_not_replace_a_newer_failure() {
    let mut row = Row(None);
    row.put(&run("r", false, 2000, "r2"));
    row.put(&run("r", true, 1000, "r1"));
    assert_eq!((row.post().kind.as_str(), row.post().streak), ("failing", 1));
}

#[test]
fn the_same_run_twice_is_a_no_op_and_ties_go_by_fact_id() {
    let mut row = Row(None);
    assert!(row.put(&run("r", false, 2000, "r2")));
    assert!(!row.put(&run("r", false, 2000, "r2")), "a retried publish");
    assert!(row.put(&run("r", true, 2000, "r3")), "same instant, larger id: newer");
    assert_eq!(row.post().fact_id, "r3");
}

#[test]
fn a_routine_remembers_its_last_ten_runs() {
    let mut row = Row(None);
    for n in 0..13 {
        row.put(&run("r", false, 1000 + i64::from(n), &format!("run{n:02}")));
    }
    let p = row.post();
    assert_eq!(p.items.len(), 10);
    assert_eq!((p.items[0]["id"].as_str(), p.streak), (Some("run03"), 10));
}

#[test]
fn a_fold_keeps_the_id_and_the_like_and_brings_a_dismissed_post_back() {
    let mut row = Row(None);
    row.put(&run("r", true, 1000, "r1"));
    let p = row.0.as_mut().unwrap();
    (p.liked_at, p.hidden_at, p.created_at) = (Some(7), Some(8), 3);
    row.put(&run("r", false, 2000, "r2"));
    let p = row.post();
    assert_eq!((p.id.as_str(), p.liked_at, p.hidden_at, p.created_at), ("p-1", Some(7), None, 3));
    assert_eq!(p.updated_at, 5_000_000);
}

#[test]
fn a_duplicate_pr_in_a_day_is_added_once() {
    let mut row = Row(None);
    assert!(row.put(&pr(1, noon(27))));
    assert_eq!((row.post().title.as_str(), row.post().kind.as_str()), ("Merged 1 PR today", "merges"));
    assert!(row.put(&pr(2, noon(27) + 60_000)));
    assert!(!row.put(&pr(1, noon(27) + 120_000)), "a retry of a merge already in");
    let p = row.post();
    assert_eq!((p.items.len(), p.title.as_str()), (2, "Merged 2 PRs today"));
    assert_eq!(p.line, "o/r#1 change 1; o/r#2 change 2");
}

#[test]
fn a_merge_lands_on_the_day_it_happened() {
    let (a, b) = (pr(1, noon(26)), pr(2, noon(27)));
    assert_ne!(key(&a), key(&b), "two days, two cards");
    assert_eq!(key(&pr(3, noon(27) + 1000)), key(&b), "a late retry finds the day's card");
    assert!(key(&b).ends_with("2026-11-27"));
}

fn page(version: &str, at: i64) -> PostWrite {
    let mut w = write(Fold::Page { name: "plan.html".into(), version: version.into() }, "page", "Plan", at, version);
    w.attachment =
        Some(json!({"type": "page", "file": {"name": "plan.html"}, "shot": {"name": "old.png"}, "version": version}));
    w
}

#[test]
fn a_new_page_version_replaces_the_card_and_drops_the_picture() {
    let mut row = Row(None);
    row.put(&page("v1", 1000));
    assert!(row.post().attachment.as_ref().unwrap()["shot"].is_null(), "a picture comes only from the shooter");
    assert!(!row.put(&page("v1", 2000)), "the same version again");
    assert!(!row.put(&page("v0", 500)), "an older file");
    assert!(row.put(&page("v2", 2000)));
    assert_eq!(row.post().attachment.as_ref().unwrap()["version"], "v2");
}

#[test]
fn a_tool_post_with_a_fold_replaces_its_own_and_one_without_is_always_new() {
    let mut row = Row(None);
    let named = |at: i64, id: &str| write(Fold::Tool { fold: Some("daily".into()) }, "number", "Sales", at, id);
    row.put(&named(2000, "a"));
    assert!(!row.put(&named(1000, "b")), "older than what is shown");
    assert!(row.put(&named(2000, "c")));
    assert_eq!(row.post().fact_id, "c");
    let loose = write(Fold::Tool { fold: None }, "file", "Report", 1, "x");
    assert_ne!(key(&loose), key(&loose), "every one is its own card");
}

#[test]
fn title_and_line_are_one_short_line() {
    let mut w = write(Fold::Tool { fold: None }, "file", &format!("a\n  b {}", "x".repeat(200)), 1, "x");
    w.line = format!("one\ntwo {}", "y".repeat(400));
    let p = decide(None, &w, "k", 1).unwrap().unwrap();
    assert_eq!(p.title.chars().count(), 80);
    assert_eq!(p.line.chars().count(), 200);
    assert!(p.title.starts_with("a b x") && p.line.starts_with("one two y") && !p.line.contains('\n'));
    assert_eq!(one_line("  a \n b  ", 80), "a b");
}

#[test]
fn a_write_no_card_can_be_made_of_is_refused() {
    let tool = |a: Option<Value>| {
        let mut w = write(Fold::Tool { fold: None }, "file", "T", 1, "x");
        w.attachment = a;
        decide(None, &w, "k", 1)
    };
    assert!(tool(Some(json!({"type": "link", "url": "https://a.b/c"}))).is_ok());
    assert!(tool(Some(json!({"type": "link", "url": "http://a.b"}))).is_err(), "https only");
    assert!(tool(Some(json!({"type": "link", "url": "javascript:alert(1)"}))).is_err());
    assert!(tool(Some(json!({"type": "gif"}))).is_err());
    assert!(tool(Some(json!({"type": "number", "value": "five"}))).is_err());
    assert!(decide(None, &write(Fold::Tool { fold: None }, "file", " ", 1, "x"), "k", 1).is_err(), "no title");
    assert!(decide(None, &write(Fold::Tool { fold: Some("A b".into()) }, "file", "T", 1, "x"), "k", 1).is_err());
    assert!(decide(None, &run("r", true, 1, "x").with_kind("failing"), "k", 1).is_err(), "a good run is not failing");
}
