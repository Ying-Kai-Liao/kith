//! `kith-sched`'s cron dialect (`field`, `parse` and `MACROS` in kernel/bin/kith-sched), then croner for the clock.
//!
//! The Python parser decides what a spec means: each field becomes the set of values it allows, `0` and `7` both mean
//! Sunday, and a day-of-month and a day-of-week that are both restricted fire on either. The sets are written out as
//! explicit lists for croner, so croner never sees a spelling the Python parser does not give the same meaning.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use chrono::{DateTime, TimeZone, Utc};
use croner::Cron;

use super::Invalid;

/// The @ macros `kith-sched` accepts and what they expand to.
pub const MACROS: &[(&str, &str)] = &[
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@weekly", "0 0 * * 0"),
    ("@monthly", "0 0 1 * *"),
];

/// A parsed spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CronSpec {
    /// A spec `kith-sched` also accepts: the values each field allows, with its exact meaning.
    Sets(Sets),
    /// A spec only croner accepts (`MON-FRI`, `JAN`, `L`, `#`, `W`, `?`, `@yearly`, `@annually`): the host's routines may
    /// hold these, so they must keep parsing. croner decides what they mean, as it does in the host.
    Croner(String),
}

/// The values each field of a `kith-sched` spec allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sets {
    minutes: BTreeSet<u64>,
    hours: BTreeSet<u64>,
    days: BTreeSet<u64>,
    months: BTreeSet<u64>,
    weekdays: BTreeSet<u64>,
    /// The day-of-month field is not a bare `*`.
    dom_restricted: bool,
    /// The day-of-week field is not a bare `*`.
    dow_restricted: bool,
}

impl CronSpec {
    /// `kith-sched`'s `parse`, with its messages; a spec it refuses but croner takes is kept for croner. A spec both
    /// refuse is refused with `kith-sched`'s message. A range that runs backwards (`5-3`) is accepted there and allows
    /// nothing; it is accepted here and never fires.
    pub fn parse(cron: &str) -> Result<Self, Invalid> {
        match Sets::parse(cron) {
            Ok(sets) => Ok(Self::Sets(sets)),
            Err(e) => {
                let text = cron.trim();
                // Exactly five fields (or an `@` macro): croner would also take a leading seconds and a trailing year
                // field, which `kith-sched` and the host's routines never held; those keep `kith-sched`'s message.
                let fields = text.split_whitespace().count();
                if (fields == 5 || text.starts_with('@')) && text.parse::<Cron>().is_ok() {
                    Ok(Self::Croner(text.to_owned()))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// The first fire strictly after `after` (ms), reading the clock in `zone`; `None` when it never fires.
    pub fn next<Z: TimeZone>(&self, after: i64, zone: &Z) -> Option<i64> {
        // Fires are whole minutes, so the next one after `after` is the next one after its minute. croner keeps the
        // sub-second part of what it is given (and mishandles hh:59:59.999), so it is cut off first.
        let after = after.div_euclid(60_000) * 60_000;
        let from = DateTime::<Utc>::from_timestamp_millis(after)?.with_timezone(zone);
        let cron: Cron = match self {
            Self::Sets(sets) => {
                if sets.never() {
                    return None;
                }
                sets.croner_text().parse().ok()?
            }
            Self::Croner(text) => text.parse().ok()?,
        };
        // An Err is "no occurrence" (a day that no month has): croner searches a bounded span.
        // croner can answer a time at or before `from` in the repeated hour of a fall-back (01:30 EDT asked from the
        // second 01:00): a next time is never in the past, so ask again from croner's own answer until one is after
        // `after`. Each pass moves on by at least a minute, so the repeated hour is crossed in at most 60 passes; the
        // second pass of the hour does not fire again. (Stepping by whole hours skipped real slots of a spec that
        // fires several times an hour.)
        let mut from = from;
        let mut from_ms = after;
        for _ in 0..200 {
            let t = cron
                .find_next_occurrence(&from, false)
                .ok()?
                .timestamp_millis();
            if t > after {
                return Some(t);
            }
            from_ms = t.max(from_ms + 60_000);
            from = DateTime::<Utc>::from_timestamp_millis(from_ms)?.with_timezone(zone);
        }
        None
    }
}

impl Sets {
    fn parse(cron: &str) -> Result<Self, Invalid> {
        let trimmed = cron.trim();
        let text = MACROS
            .iter()
            .find(|(name, _)| *name == trimmed)
            .map_or(cron, |(_, expansion)| expansion);
        let f: Vec<&str> = text.split_whitespace().collect();
        if f.len() != 5 {
            return Err(Invalid(format!(
                "cron needs 5 fields (minute hour day month weekday), got {}",
                py_repr(cron)
            )));
        }
        // The same order as Python's tuple, so the first bad field is the one it names.
        let weekdays = field(f[4], 0, 7)?.into_iter().map(|d| d % 7).collect();
        Ok(Self {
            minutes: field(f[0], 0, 59)?,
            hours: field(f[1], 0, 23)?,
            days: field(f[2], 1, 31)?,
            months: field(f[3], 1, 12)?,
            weekdays,
            dom_restricted: f[2] != "*",
            dow_restricted: f[4] != "*",
        })
    }

    fn never(&self) -> bool {
        [
            &self.minutes,
            &self.hours,
            &self.days,
            &self.months,
            &self.weekdays,
        ]
        .iter()
        .any(|s| s.is_empty())
    }

    fn croner_text(&self) -> String {
        let list = |s: &BTreeSet<u64>| s.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        // A restricted field stays a list even when it names every value: croner then treats day-of-month and
        // day-of-week as either-may-match, as `matches` in kith-sched does.
        let dom = if self.dom_restricted {
            list(&self.days)
        } else {
            "*".into()
        };
        let dow = if self.dow_restricted {
            list(&self.weekdays)
        } else {
            "*".into()
        };
        format!(
            "{} {} {} {} {}",
            list(&self.minutes),
            list(&self.hours),
            dom,
            list(&self.months),
            dow
        )
    }
}

/// One cron field -> the values it allows (`field` in kith-sched).
fn field(spec: &str, lo: u64, hi: u64) -> Result<BTreeSet<u64>, Invalid> {
    let bad = || Invalid(format!("bad cron field {}", py_repr(spec)));
    let mut out = BTreeSet::new();
    for part in spec.split(',') {
        let (rng, step_text) = match part.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (part, None),
        };
        let (a, b) = if rng == "*" {
            (lo, hi)
        } else if let Some((a, b)) = rng.split_once('-') {
            (number(a).ok_or_else(bad)?, number(b).ok_or_else(bad)?)
        } else {
            let n = number(rng).ok_or_else(bad)?;
            (n, n)
        };
        let step = match step_text {
            Some(s) => number(s).ok_or_else(bad)?,
            None => 1,
        };
        // "5/15" = from 5 to the end, every 15.
        let b = if step_text.is_some() && rng != "*" && !rng.contains('-') {
            hi
        } else {
            b
        };
        if !(lo..=hi).contains(&a) || !(lo..=hi).contains(&b) || step < 1 {
            return Err(Invalid(format!(
                "cron field {} out of range {lo}-{hi}",
                py_repr(spec)
            )));
        }
        let mut v = a;
        while v <= b {
            out.insert(v);
            v = match v.checked_add(step) {
                Some(n) => n,
                None => break,
            };
        }
    }
    Ok(out)
}

/// `\d+`, ASCII; a number too big for u64 is saturated (it is out of range either way).
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.parse().unwrap_or(u64::MAX))
}

/// Python's `repr` of a string, for the cases a spec can show (quotes, backslash, control characters).
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
