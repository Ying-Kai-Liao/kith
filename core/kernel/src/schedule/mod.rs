//! One schedule type for every trigger (M4-1; docs/plans/core-design.md section 9). It replaces the two dialects in use:
//! `kith-sched`'s local-time cron and the host routines' IANA-zone triggers. Pure rules, no state: nothing calls this at
//! runtime yet.
//!
//! # Serde shapes
//!
//! Tagged by `type`, field names as written here:
//!
//! ```json
//! {"type":"cron","expr":"0 9 * * 1-5","tz":"local"}
//! {"type":"cron","expr":"@daily","tz":"Asia/Taipei"}
//! {"type":"interval","every":900}
//! {"type":"once","at":1793000000000}
//! {"type":"webhook"}
//! {"type":"event","source":"github","event":"pull_request.opened","filters":{"repo":"o/r"}}
//! ```
//!
//! `tz` is `"local"` (follow the Mac; what a `kith-sched` job with no zone becomes) or an IANA name. `every` is whole
//! seconds. `at` and every time this module returns are milliseconds since the Unix epoch, as in the host's routines.
//! The host's `expression`/`timeZone`/`seconds` spellings map one to one onto `expr`/`tz`/`every`.
//!
//! # Cron
//!
//! The grammar is `kith-sched`'s, exactly (see [`cron`]): five fields, `*`, numbers, `a-b`, lists, `/n` steps, `5/15`
//! (to the end, every 15), `0` and `7` both Sunday, and the macros `@hourly @daily @midnight @weekly @monthly`. A spec
//! it accepts keeps its exact meaning and next times. Fields are expanded to explicit values and handed to croner (the
//! host's engine, major 3) together with the zone.
//!
//! A spec `kith-sched` refuses is still accepted when croner's `Cron::from_str` accepts it, because the host's routines
//! may hold such specs and must not stop parsing at the migration: names (`MON-FRI`, `JAN`), `L`, `#`, `W`, `?`,
//! `@yearly` and `@annually`. croner decides what those mean, as it does in the host. A spec both refuse is refused
//! with `kith-sched`'s message.
//!
//! # Intended differences
//!
//! * Superset: the croner-only spellings above are accepted here and refused by `kith-sched`, `@yearly` and
//!   `@annually` on purpose. Only five-field specs (and `@` macros) take that path: a six- or seven-field spec (a
//!   leading seconds or trailing year field) is refused with `kith-sched`'s "needs 5 fields" message.
//! * Daylight saving follows croner's rule for the zone; `kith-sched` matches plain local minutes. Two jobs change:
//!   `30 2 * * *` on the spring-forward day (2026-03-08 in New York) never fires under `kith-sched` and fires at 03:00
//!   local here; `30 1 * * *` on the fall-back day (2026-11-01) fires twice under `kith-sched` (both 01:30s) and once
//!   here, at the first 01:30. A stepped
//!   minute such as `*/30 * * * *` fires at 01:00 and 01:30 EDT on that day, skips the repeated 01:00 and 01:30 EST, and
//!   resumes at 02:00 EST: it does not fire on every real half hour. The host's routines share this rule; pinned in
//!   `every_half_hour_across_the_fall_back_day`.)
//! * No 366-day horizon: `kith-sched`'s `next_time` gives up a year out, so `0 0 29 2 *` asked in March has no next
//!   there. Here it is the real date; a spec that can never fire (`0 0 31 2 *`, `5-3 * * * *`) validates and has none.
//! * `Interval` and `Once` are rounded up to the one-minute tick: a due time inside a minute fires at the next whole
//!   minute. The host's routines carry seconds, so a 30 s interval becomes 60 s and a 90 s one steps by 2 minutes
//!   (`next_n_in` chains from each rounded fire).
//! * Digits are ASCII only (Python's `\d` also takes other scripts' digits).

pub mod cron;

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Local, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

pub use cron::CronSpec;

const TICK_MS: i64 = 60_000;
/// `kith-sched`'s `CATCH_UP`: a job the Mac slept through runs once if the miss is under a day.
pub const CATCH_UP_MS: i64 = 86_400_000;
const MAX_INTERVAL_SECS: u64 = 31_536_000;
const EVENT_SOURCES: &[&str] = &[
    "slack",
    "github",
    "origin",
    "microsoftTeams",
    "linear",
    "sentry",
    "pagerduty",
    "email",
];

/// Why a trigger is refused. The text is `kith-sched`'s own for a cron spec it also refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

/// Where a cron spec's clock is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Zone {
    /// This Mac's local time, whatever it is when asked.
    Local,
    /// An IANA zone name (`Europe/Paris`).
    Iana(String),
}

impl Serialize for Zone {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Local => s.serialize_str("local"),
            Self::Iana(name) => s.serialize_str(name),
        }
    }
}

impl<'de> Deserialize<'de> for Zone {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl Zone {
    /// `"local"` or an IANA name; any other name is refused.
    pub fn parse(text: &str) -> Result<Self, Invalid> {
        if text == "local" {
            return Ok(Self::Local);
        }
        text.parse::<Tz>()
            .map(|_| Self::Iana(text.to_owned()))
            .map_err(|_| Invalid(format!("unknown time zone {text:?}")))
    }
}

/// When something runs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Trigger {
    /// A five-field cron spec (or an @ macro) read in a zone.
    Cron { expr: String, tz: Zone },
    /// Every `every` seconds.
    Interval { every: u64 },
    /// Once, at `at` (ms since the epoch).
    Once { at: i64 },
    /// When a webhook is delivered. Never "due" by the clock.
    Webhook,
    /// When a matching event arrives. Never "due" by the clock.
    Event {
        source: String,
        event: String,
        #[serde(default)]
        filters: BTreeMap<String, String>,
    },
}

impl Trigger {
    /// Checks the trigger's shape (not the clock: a `Once` in the past is valid and simply has no next time).
    pub fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Cron { expr, tz } => {
                CronSpec::parse(expr)?;
                if let Zone::Iana(name) = tz {
                    Zone::parse(name)?;
                }
            }
            Self::Interval { every } if !(1..=MAX_INTERVAL_SECS).contains(every) => {
                return Err(Invalid(
                    "interval must be between 1 second and 365 days".into(),
                ));
            }
            Self::Event {
                source,
                event,
                filters,
            } => {
                if !EVENT_SOURCES.contains(&source.as_str()) {
                    return Err(Invalid("unsupported event source".into()));
                }
                if event.is_empty()
                    || event.len() > 200
                    || filters.len() > 20
                    || filters
                        .iter()
                        .any(|(k, v)| k.is_empty() || k.len() > 200 || v.len() > 4000)
                {
                    return Err(Invalid("invalid event filter".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The first fire strictly after `after` (ms), `Local` read in this Mac's zone. `None`: it never fires again, or
    /// the trigger is a webhook or event.
    pub fn next_after(&self, after: i64) -> Option<i64> {
        self.next_after_in(after, &Local)
    }

    /// [`Self::next_after`] with `Local` meaning `local` (tests pass a fixed offset; the Mac passes [`Local`]).
    pub fn next_after_in<L: TimeZone>(&self, after: i64, local: &L) -> Option<i64> {
        match self {
            Self::Cron { expr, tz } => {
                let spec = CronSpec::parse(expr).ok()?;
                match tz {
                    Zone::Local => spec.next(after, local),
                    Zone::Iana(name) => spec.next(after, &name.parse::<Tz>().ok()?),
                }
            }
            Self::Interval { every } => {
                let secs = i64::try_from(*every).ok()?.checked_mul(1000)?;
                Some(ceil_tick(after.checked_add(secs)?))
            }
            Self::Once { at } => (*at > after).then(|| ceil_tick(*at)),
            Self::Webhook | Self::Event { .. } => None,
        }
    }

    /// Up to `n` fires after `after`, each after the one before.
    pub fn next_n_in<L: TimeZone>(&self, after: i64, n: usize, local: &L) -> Vec<i64> {
        let mut out = Vec::new();
        let mut at = after;
        while out.len() < n {
            let Some(next) = self.next_after_in(at, local) else {
                break;
            };
            out.push(next);
            at = next;
        }
        out
    }

    /// The catch-up rule of `kith-sched tick`. The last tick `last` and `now` are cut to the minute; only the day before
    /// `now` is looked at (a miss of a day or more is forgotten), and the one fire found in `(since, now]` is the run
    /// owed: once, however many slots were missed. `None` when nothing is due. This is a cron rule: for `Interval` and
    /// `Once` it only asks whether the next fire after the cut-off is already past (`Interval` counts from `last`, so
    /// with no `last` a 5-minute interval is not due), which is not how a store should schedule them.
    pub fn due_in<L: TimeZone>(&self, last: Option<i64>, now: i64, local: &L) -> Option<i64> {
        let now = now.div_euclid(TICK_MS) * TICK_MS;
        let last = last.map_or(now - TICK_MS, |t| t.div_euclid(TICK_MS) * TICK_MS);
        let since = last.max(now - CATCH_UP_MS);
        if since >= now {
            return None;
        }
        self.next_after_in(since, local).filter(|due| *due <= now)
    }

    /// [`Self::due_in`] on this Mac's clock.
    pub fn due(&self, last: Option<i64>, now: i64) -> Option<i64> {
        self.due_in(last, now, &Local)
    }
}

fn ceil_tick(ms: i64) -> i64 {
    let floor = ms.div_euclid(TICK_MS) * TICK_MS;
    if floor == ms { ms } else { floor + TICK_MS }
}

/// Most fires `schedule.preview` returns.
pub const PREVIEW_MAX: usize = 100;

/// `schedule.preview`: `{trigger, after?, n?}` -> `{fires: [{at, iso}]}`. `after` defaults to `now` (the caller's
/// clock; this module reads none), `n` to 5, at most [`PREVIEW_MAX`].
pub fn preview(params: &Value, now: i64) -> Result<Value, Invalid> {
    let trigger: Trigger =
        serde_json::from_value(params.get("trigger").cloned().unwrap_or(Value::Null))
            .map_err(|e| Invalid(format!("trigger: {e}")))?;
    trigger.validate()?;
    let after = params.get("after").and_then(Value::as_i64).unwrap_or(now);
    let n = params.get("n").and_then(Value::as_u64).map_or(5, |n| {
        usize::try_from(n).unwrap_or(PREVIEW_MAX).min(PREVIEW_MAX)
    });
    let fires: Vec<Value> = trigger
        .next_n_in(after, n, &Local)
        .into_iter()
        .map(|at| {
            let iso = DateTime::<Utc>::from_timestamp_millis(at).map(|d| d.to_rfc3339());
            json!({"at": at, "iso": iso})
        })
        .collect();
    Ok(json!({ "fires": fires }))
}

#[cfg(test)]
mod tests;
