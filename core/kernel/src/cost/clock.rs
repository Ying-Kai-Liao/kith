//! "Now" as `kith-cost` takes it: an aware local time with a fixed offset (the offset of that instant, so a month's
//! midnight keeps it, daylight saving or not). `KITH_COST_NOW` (ISO local time, or epoch seconds) hands tests a fixed one.

use chrono::{
    DateTime, Datelike as _, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone as _,
    Timelike as _, Utc,
};

#[derive(Clone, Copy, Debug)]
pub struct Now(pub DateTime<FixedOffset>);

/// The text `KITH_COST_NOW` gave that is no time.
#[derive(Debug)]
pub struct BadClock(pub String);

impl Now {
    /// The clock, or `KITH_COST_NOW` when it is set (and not blank).
    pub fn from_env(v: Option<&str>) -> Result<Now, BadClock> {
        match v.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Now(Local::now().fixed_offset())),
            Some(v) => parse(v).map(Now),
        }
    }

    /// A fixed instant, for tests.
    pub fn at(t: DateTime<FixedOffset>) -> Now {
        Now(t)
    }

    pub fn date(&self) -> NaiveDate {
        self.0.date_naive()
    }

    /// The first instant of this calendar month, in this instant's own offset (as `now().replace(day=1, hour=0, ...)`).
    pub fn month_start(&self) -> DateTime<FixedOffset> {
        let d = self.date().with_day(1).unwrap_or_else(|| self.date());
        self.at_midnight(d)
    }

    fn at_midnight(&self, d: NaiveDate) -> DateTime<FixedOffset> {
        self.0
            .offset()
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
            .single()
            .unwrap_or(self.0)
    }

    /// `utc(t)`: the instant as the ledger writes it, `2026-11-05T12:00:00`.
    pub fn utc_text(t: DateTime<FixedOffset>) -> String {
        t.with_timezone(&Utc)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    }

    /// Where an app's period starts: today, the last 7 days, this calendar month, or all (`"0"`).
    pub fn app_since(&self, period: &str) -> String {
        match period {
            "today" => Self::utc_text(self.at_midnight(self.date())),
            "week" => Self::utc_text(self.0 - Duration::days(7)),
            "month" => Self::utc_text(self.month_start()),
            _ => "0".to_owned(),
        }
    }

    /// The month's full name (`%B`).
    pub fn month_name(&self) -> String {
        self.0.format("%B").to_string()
    }

    /// The first day of next month: where this month's limit resets.
    pub fn next_month(&self) -> NaiveDate {
        next_month(self.date())
    }

    /// Days to the end of today, as a fraction of a day (`left_today` in `forecast`), microsecond exact.
    pub fn left_today(&self) -> f64 {
        let t = self.0.time();
        let since_midnight =
            i64::from(t.num_seconds_from_midnight()) * 1_000_000 + i64::from(t.nanosecond() / 1000);
        let left = 86_400_i64 * 1_000_000 - since_midnight;
        // total_seconds() is micro / 10**6, then / 86400
        (left as f64 / 1e6) / 86_400.0
    }
}

pub fn next_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(d)
}

/// The last day of `d`'s month.
pub fn month_end(d: NaiveDate) -> NaiveDate {
    next_month(d).pred_opt().unwrap_or(d)
}

fn local_naive(n: NaiveDateTime) -> Result<DateTime<FixedOffset>, BadClock> {
    // A wall time that happens twice reads as the first one (fold 0), as `astimezone()` does.
    Local
        .from_local_datetime(&n)
        .earliest()
        .or_else(|| Local.from_local_datetime(&n).latest())
        .map(|t| t.fixed_offset())
        .ok_or_else(|| BadClock(format!("{n} is not a local time")))
}

fn parse(v: &str) -> Result<DateTime<FixedOffset>, BadClock> {
    if let Ok(secs) = v.parse::<f64>()
        && secs.is_finite()
    {
        let whole = secs.floor();
        let nanos = ((secs - whole) * 1e9).round() as u32;
        return Local
            .timestamp_opt(whole as i64, nanos.min(999_999_999))
            .single()
            .map(|t| t.fixed_offset())
            .ok_or_else(|| BadClock(v.to_owned()));
    }
    // `datetime.fromisoformat`: a date, or a date and a time after `T` or a space, with or without an offset (`Z` too).
    let text = v.replacen(' ', "T", 1);
    // An aware time is moved to the local zone, as `.astimezone()` does.
    let local = |t: DateTime<FixedOffset>| t.with_timezone(&Local).fixed_offset();
    if let Ok(t) = DateTime::parse_from_rfc3339(&text) {
        return Ok(local(t));
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M%z",
    ] {
        if let Ok(t) = DateTime::parse_from_str(&text, f) {
            return Ok(local(t));
        }
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(&text, f) {
            return local_naive(n);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(&text, "%Y-%m-%d") {
        return local_naive(d.and_hms_opt(0, 0, 0).unwrap_or_default());
    }
    Err(BadClock(format!("Invalid isoformat string: '{v}'")))
}
