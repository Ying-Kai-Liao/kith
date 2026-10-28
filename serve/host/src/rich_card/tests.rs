//! Cards say only what the host knows: a schedule in words, a job's change as the live jobs show it, a brief's rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

/// Thursday morning in Sydney.
fn now() -> DateTime<FixedOffset> {
    at("2026-10-29T08:00:00+11:00")
}

#[test]
fn a_cron_reads_as_words_and_one_it_cant_say_stays_as_written() {
    for (cron, words) in [
        ("43 7 * * *", "Every day at 7:43"),
        ("30 7 * * 1-5", "Weekdays at 7:30"),
        ("0 10 * * 6,0", "Weekends at 10:00"),
        ("0 9 * * 1", "Every Monday at 9:00"),
        ("0 9 * * 1,3,5", "Monday, Wednesday and Friday at 9:00"),
        ("0 18 * * 7", "Every Sunday at 18:00"),
        ("0 9 1 * *", "Day 1 of every month at 9:00"),
        ("*/30 * * * *", "Every 30 minutes"),
        ("* * * * *", "Every minute"),
        ("0 * * * *", "Every hour"),
        ("15 * * * *", "Every hour at :15"),
        ("0 */2 * * *", "Every 2 hours"),
        ("@daily", "Every day at 0:00"),
        ("0 9 * 6 *", "0 9 * 6 *"),
        ("0 9 1 * 1", "0 9 1 * 1"),
        ("*/5 9 * * *", "*/5 9 * * *"),
        ("61 7 * * *", "61 7 * * *"),
    ] {
        assert_eq!(cron_words(cron), words, "{cron}");
    }
}

#[test]
fn a_time_reads_from_today() {
    for (time, words) in [
        ("2026-10-29T07:43:00+11:00", "today 7:43"),
        ("2026-10-30T07:00:00+11:00", "tomorrow 7:00"),
        ("2026-10-28T21:10:00+11:00", "yesterday 21:10"),
        ("2026-10-31T07:00:00+11:00", "Saturday 7:00"),
        ("2026-11-10T07:00:00+11:00", "10 Nov 7:00"),
        // Another offset reads in today's.
        ("2026-10-29T20:00:00Z", "tomorrow 7:00"),
    ] {
        assert_eq!(when_words(at(time), now()), words, "{time}");
    }
}

fn jobs(person: Option<&str>, paused: bool) -> Vec<Value> {
    vec![json!({
        "id": "daily-news", "cron": "0 7 * * *", "task": "the news", "paused": paused,
        "next_run": if paused { Value::Null } else { json!("2026-10-30T07:00:00+11:00") },
        "last_run": null, "last_status": null, "source": "kith", "person": person,
    })]
}

fn op(id: &str, op: &str) -> Op {
    Op { id: id.into(), op: op.into() }
}

#[test]
fn a_schedule_card_carries_the_live_job_and_a_twin_to_read_without_it() {
    let card = schedule("echo", None, &op("daily-news", "added"), &jobs(None, false), None, now()).unwrap();
    assert_eq!(
        card["text"],
        "Schedule added · Daily news\nEvery day at 7:00 · next tomorrow 7:00\nRuns as you\n\nOpen Kith to pause, run or remove it."
    );
    assert_eq!(card["richCard"]["type"], "schedule");
    assert_eq!(card["richCard"]["v"], 1);
    assert_eq!(card["richCard"]["ref"], json!({"agent": "echo", "id": "daily-news"}));
    let snap = &card["richCard"]["snapshot"];
    assert_eq!(snap["name"], "Daily news");
    assert_eq!(snap["cron"], "0 7 * * *");
    assert_eq!(snap["task"], "the news");
    assert_eq!(snap["paused"], false);
    assert_eq!(snap["nextRun"], "2026-10-30T07:00:00+11:00");
    assert_eq!(snap["at"], now().timestamp_millis());

    let paused = schedule("echo", None, &op("daily-news", "paused"), &jobs(None, true), None, now()).unwrap();
    assert_eq!(paused["text"], "Schedule paused · Daily news\nIt won't run until you resume it.");
    assert_eq!(paused["richCard"]["snapshot"]["paused"], true);

    let prior = snap.clone();
    let removed = schedule("echo", None, &op("daily-news", "removed"), &[], Some(&prior), now()).unwrap();
    assert_eq!(removed["text"], "Schedule removed · Daily news\nIt won't run again.");
    assert_eq!(removed["richCard"]["snapshot"]["every"], "Every day at 7:00");
    assert_eq!(removed["richCard"]["snapshot"]["task"], "the news");
}

#[test]
fn a_change_the_jobs_dont_show_has_no_card() {
    let live = jobs(Some("sam"), false);
    let none = |id: &str, o: &str, person: Option<&str>, jobs: &[Value]| {
        schedule("echo", person, &op(id, o), jobs, None, now()).is_none()
    };
    assert!(none("daily-news", "paused", Some("sam"), &live), "not paused");
    assert!(none("daily-news", "removed", Some("sam"), &live), "still there");
    assert!(none("other", "added", Some("sam"), &live), "no such job");
    assert!(none("daily-news", "resumed", Some("sam"), &jobs(Some("sam"), true)), "still paused");
    assert!(none("daily-news", "renamed", Some("sam"), &live), "no such change");
    assert!(none("daily-news", "added", None, &live), "sam's job, from the owner's chat");
    assert!(none("daily-news", "added", Some("ann"), &live), "sam's job, from ann's chat");
    let mut odd = live.clone();
    odd[0]["id"] = json!("../news");
    assert!(none("../news", "added", Some("sam"), &odd), "an id kith-sched never makes");
    odd[0]["id"] = json!("daily-news");
    odd[0]["source"] = json!("launchd");
    assert!(none("daily-news", "added", Some("sam"), &odd), "not a kith job");
    assert!(!none("daily-news", "resumed", Some("sam"), &live));
}

#[test]
fn a_brief_becomes_rows_of_title_line_source_and_link() {
    let text = "Good morning. Three things:\n\n\
        - **Rain by noon** — take a coat (BOM, https://bom.gov.au/nsw)\n\
        - [Kith 2.0 ships](https://github.com/k/kith) — cards in chat\n\
        - Rates hold at 4.1%: the RBA waits for November. [ABC](https://www.abc.net.au/news/rates)\n\
        \nThat's all.";
    let card = news::card(text, "echo", "morning-brief", Some("0 7 * * *")).unwrap();
    assert_eq!(card["type"], "news");
    assert_eq!(card["ref"], json!({"agent": "echo", "job": "morning-brief"}));
    let snap = &card["snapshot"];
    assert_eq!(snap["name"], "Morning brief");
    assert_eq!(snap["every"], "Every day at 7:00");
    assert_eq!(snap["lead"], "Good morning. Three things:");
    assert_eq!(snap["tail"], "That's all.");
    assert_eq!(
        snap["rows"],
        json!([
            {"title": "Rain by noon", "line": "take a coat", "source": "BOM", "domain": "bom.gov.au",
             "url": "https://bom.gov.au/nsw"},
            {"title": "Kith 2.0 ships", "line": "cards in chat", "source": null, "domain": "github.com",
             "url": "https://github.com/k/kith"},
            {"title": "Rates hold at 4.1%", "line": "the RBA waits for November.", "source": "ABC",
             "domain": "abc.net.au", "url": "https://www.abc.net.au/news/rates"},
        ])
    );
}

#[test]
fn numbered_items_and_lines_that_continue_them_are_rows_too() {
    let text = "1. Title one — its line\n   https://a.com/1\n2) Other: thing https://b.org/2.";
    let rows = news::card(text, "echo", "j", None).unwrap()["snapshot"]["rows"].clone();
    assert_eq!(rows[0]["title"], "Title one");
    assert_eq!(rows[0]["line"], "its line");
    assert_eq!(rows[1]["title"], "Other");
    assert_eq!(rows[1]["line"], "thing");
    assert_eq!(rows[1]["url"], "https://b.org/2");
}

#[test]
fn text_that_isnt_a_brief_stays_plain() {
    for text in [
        "Just one thing: https://a.com/x",
        "- one https://a.com/1",
        "- one https://a.com/1\n\nIn between.\n\n- two https://b.com/2",
        "- one https://a.com/1\n- two, no link",
        "- one http://localhost:3000/x\n- two https://b.com/2",
        "No list at all.",
    ] {
        assert!(news::card(text, "echo", "j", None).is_none(), "{text}");
    }
}
