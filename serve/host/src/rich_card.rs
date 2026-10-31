//! Rich cards, `data.richCard` on an entry: `{type, v, ref, snapshot}`, the host's state shaped for the apps' Schedule
//! and News cards (DESIGN.md "Reply cards"). Every word on a card is host state or text already sent. The entry's `text`
//! is the card's twin, the plain row an app that doesn't know the card shows.
//!
//! The snapshot is the state when the card was posted; an app shows live state from `ref` when it can read it
//! (`kithInfo` schedules, the routine store) and the snapshot when it can't or the job is gone.
pub mod news;
pub mod plan;

use crate::routines::{Routine, Trigger};
use chrono::{DateTime, Datelike as _, FixedOffset, Local, TimeZone as _, Timelike as _};
use serde::Deserialize;
use serde_json::{Value, json};

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// What `kith-sched` leaves in a run's outbox when the run changes a job.
#[derive(Deserialize)]
pub struct Op {
    pub id: String,
    pub op: String,
}

/// A job's name for people: its id with dashes as spaces, first letter up ("daily-news" is "Daily news").
pub fn job_name(id: &str) -> String {
    let words = id.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn clock(hour: u32, minute: u32) -> String {
    format!("{hour}:{minute:02}")
}

/// A five-field cron (or an @macro) in words: "Every day at 7:43", "Weekdays at 7:30", "Every 30 minutes". One it
/// can't say simply is shown as written.
pub fn cron_words(cron: &str) -> String {
    let expanded = match cron.trim() {
        "@hourly" => "0 * * * *",
        "@daily" | "@midnight" => "0 0 * * *",
        "@weekly" => "0 0 * * 0",
        "@monthly" => "0 0 1 * *",
        other => other,
    };
    match expanded.split_whitespace().collect::<Vec<_>>().as_slice() {
        [minute, hour, dom, "*", dow] => words(minute, hour, dom, dow),
        _ => None,
    }
    .unwrap_or_else(|| cron.trim().to_owned())
}

fn words(minute: &str, hour: &str, dom: &str, dow: &str) -> Option<String> {
    let every = |n: &str, one: &str, many: &str| match n.parse::<u32>() {
        Ok(1) => Some(format!("Every {one}")),
        Ok(n) if n > 1 => Some(format!("Every {n} {many}")),
        _ => None,
    };
    if (dom, dow) != ("*", "*") && hour.parse::<u32>().is_err() {
        return None;
    }
    match (minute, hour) {
        ("*", "*") => return Some("Every minute".into()),
        (m, "*") if m.starts_with("*/") => return every(&m[2..], "minute", "minutes"),
        ("0", "*") => return Some("Every hour".into()),
        ("0", h) if h.starts_with("*/") => return every(&h[2..], "hour", "hours"),
        _ => {}
    }
    let minute: u32 = minute.parse().ok().filter(|m| *m < 60)?;
    if hour == "*" {
        return Some(format!("Every hour at :{minute:02}"));
    }
    let at = clock(hour.parse().ok().filter(|h| *h < 24)?, minute);
    let day = |d: &str| d.parse::<usize>().ok().filter(|d| *d <= 7).map(|d| DAYS[d % 7]);
    Some(match (dom, dow) {
        ("*", "*") => format!("Every day at {at}"),
        ("*", "1-5") => format!("Weekdays at {at}"),
        ("*", "0,6" | "6,0" | "6,7" | "6-7") => format!("Weekends at {at}"),
        ("*", days) => {
            let names = days.split(',').map(day).collect::<Option<Vec<_>>>()?;
            match names.as_slice() {
                [one] => format!("Every {one} at {at}"),
                [rest @ .., last] => format!("{} and {last} at {at}", rest.join(", ")),
                [] => return None,
            }
        }
        (d, "*") => format!("Day {} of every month at {at}", d.parse::<u32>().ok().filter(|d| (1..=31).contains(d))?),
        _ => return None,
    })
}

/// When `at` is, from `now`: "today 7:43", "tomorrow 7:43", "Friday 7:43", "31 Oct 7:43".
pub fn when_words(at: DateTime<FixedOffset>, now: DateTime<FixedOffset>) -> String {
    let at = at.with_timezone(now.offset());
    let time = clock(at.hour(), at.minute());
    match (at.date_naive() - now.date_naive()).num_days() {
        0 => format!("today {time}"),
        1 => format!("tomorrow {time}"),
        -1 => format!("yesterday {time}"),
        2..=6 => format!("{} {time}", DAYS[at.weekday().num_days_from_sunday() as usize]),
        _ => format!("{} {time}", at.format("%-d %b")),
    }
}

fn local_iso(ms: i64) -> Option<String> {
    Local.timestamp_millis_opt(ms).single().map(|t| t.fixed_offset().to_rfc3339())
}

/// The Schedule card for what a run did to `agent`'s job, from the live jobs (`kith-info` schedules): its entry data
/// (`text`, `final`, `richCard`). `None` when the op doesn't match them (an item the run made up, or another person's
/// job), so nothing is posted. A removed job keeps what `prior` (the job's last card) knew of it.
pub fn schedule(
    agent: &str,
    person: Option<&str>,
    op: &Op,
    jobs: &[Value],
    prior: Option<&Value>,
    now: DateTime<FixedOffset>,
) -> Option<Value> {
    let valid_id =
        !op.id.is_empty() && op.id.len() <= 64 && op.id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    let job = jobs.iter().find(|j| j["source"] == "kith" && j["id"] == op.id.as_str());
    let paused = job.is_some_and(|j| j["paused"] == true);
    let ok = match op.op.as_str() {
        "removed" => job.is_none(),
        "added" | "updated" => job.is_some(),
        "paused" => paused,
        "resumed" => job.is_some() && !paused,
        _ => false,
    };
    if !valid_id || !ok || job.is_some_and(|j| j["person"].as_str() != person) {
        return None;
    }
    let name = job_name(&op.id);
    let mut snapshot = json!({"name": name, "op": op.op, "at": now.timestamp_millis(), "person": person});
    if let Some(job) = job {
        let cron = job["cron"].as_str().unwrap_or_default();
        for (key, value) in [
            ("cron", json!(cron)),
            ("every", json!(cron_words(cron))),
            ("task", job["task"].clone()),
            ("paused", json!(paused)),
            ("nextRun", job["next_run"].clone()),
            ("lastRun", job["last_run"].clone()),
            ("lastStatus", job["last_status"].clone()),
        ] {
            snapshot[key] = value;
        }
    } else if let Some(prior) = prior {
        for key in ["cron", "every", "task"] {
            snapshot[key] = prior[key].clone();
        }
    }
    let every = snapshot["every"].as_str().unwrap_or_default();
    let next = job
        .and_then(|j| j["next_run"].as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| when_words(t, now));
    let text = match op.op.as_str() {
        "paused" => format!("Schedule paused · {name}\nIt won't run until you resume it."),
        "removed" => format!("Schedule removed · {name}\nIt won't run again."),
        done => {
            let when = next.map_or_else(|| every.to_owned(), |next| format!("{every} · next {next}"));
            format!("Schedule {done} · {name}\n{when}\nRuns as you\n\nOpen Kith to pause, run or remove it.")
        }
    };
    Some(json!({
        "text": text,
        "final": true,
        "richCard": {"type": "schedule", "v": 1, "ref": {"agent": agent, "id": op.id}, "snapshot": snapshot},
    }))
}

/// A routine's triggers in words ("Every day at 7:00", "Every 30 minutes", "On a webhook").
fn triggers_words(triggers: &[Trigger], now: DateTime<FixedOffset>) -> String {
    let one = |t: &Trigger| match t {
        Trigger::Cron { expression, .. } => cron_words(expression),
        Trigger::Interval { seconds } => match *seconds {
            s if s % 3600 == 0 && s > 3600 => format!("Every {} hours", s / 3600),
            3600 => "Every hour".into(),
            s if s % 60 == 0 && s > 60 => format!("Every {} minutes", s / 60),
            60 => "Every minute".into(),
            s => format!("Every {s} seconds"),
        },
        Trigger::Once { at } => Local
            .timestamp_millis_opt(*at)
            .single()
            .map_or_else(|| "Once".into(), |t| format!("Once, {}", when_words(t.fixed_offset(), now))),
        Trigger::Webhook => "On a webhook".into(),
        Trigger::Event { source, event, .. } => format!("On {source} {event}"),
    };
    triggers.iter().map(one).collect::<Vec<_>>().join("; ")
}

/// The Schedule card of an outside bot's routine, for its "Created/Updated routine" notice.
pub fn routine(r: &Routine, updated: bool, now: DateTime<FixedOffset>) -> Value {
    json!({
        "type": "schedule",
        "v": 1,
        "ref": {"bot": r.bot_id, "routine": r.id},
        "snapshot": {
            "name": r.name,
            "op": if updated { "updated" } else { "created" },
            "at": now.timestamp_millis(),
            "every": triggers_words(&r.triggers, now),
            "task": r.instruction,
            "paused": !r.enabled,
            "nextRun": r.next_runs.iter().flatten().min().and_then(|ms| local_iso(*ms)),
        },
    })
}

#[cfg(test)]
mod tests;
