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

/// A parsed spec: the values each field allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronSpec {
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
    /// `kith-sched`'s `parse`, with its messages. A range that runs backwards (`5-3`) is accepted there and allows
    /// nothing; it is accepted here and never fires.
    pub fn parse(cron: &str) -> Result<Self, Invalid> {
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

    /// The first fire strictly after `after` (ms), reading the clock in `zone`; `None` when it never fires.
    pub fn next<Z: TimeZone>(&self, after: i64, zone: &Z) -> Option<i64> {
        if [
            &self.minutes,
            &self.hours,
            &self.days,
            &self.months,
            &self.weekdays,
        ]
        .iter()
        .any(|s| s.is_empty())
        {
            return None;
        }
        let from = DateTime::<Utc>::from_timestamp_millis(after)?.with_timezone(zone);
        let cron: Cron = self.croner_text().parse().ok()?;
        // An Err is "no occurrence" (a day that no month has): croner searches a bounded span.
        cron.find_next_occurrence(&from, false)
            .ok()
            .map(|t| t.timestamp_millis())
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
