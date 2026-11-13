//! The time of day, the way `kith account list` prints it: local time (`%a %d %b %H:%M`, `%H:%M`) in the C locale, and the
//! clock. Local time needs `localtime_r` (the only `unsafe` here); where there is none the time is shown in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds since the epoch, with the fraction (`time.time()`).
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

struct Parts {
    wday: usize,
    mday: u32,
    mon: usize,
    hour: u32,
    min: u32,
}

#[cfg(unix)]
#[allow(unsafe_code, clippy::cast_sign_loss)]
fn parts(ts: i64) -> Parts {
    let t: libc::time_t = ts as libc::time_t;
    // SAFETY: `tm` is plain data that localtime_r fills in; both pointers live for the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&raw const t, &raw mut tm);
        tm
    };
    Parts {
        wday: tm.tm_wday.rem_euclid(7) as usize,
        mday: tm.tm_mday as u32,
        mon: tm.tm_mon.rem_euclid(12) as usize,
        hour: tm.tm_hour as u32,
        min: tm.tm_min as u32,
    }
}

#[cfg(not(unix))]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn parts(ts: i64) -> Parts {
    // UTC, by the civil-from-days rule.
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let mday = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mon = (if mp < 10 { mp + 2 } else { mp - 10 }) as usize;
    Parts {
        wday: (days + 4).rem_euclid(7) as usize,
        mday,
        mon,
        hour: (secs / 3600) as u32,
        min: (secs % 3600 / 60) as u32,
    }
}

/// `time.strftime('%a %d %b %H:%M', time.localtime(ts))`.
pub fn when(ts: i64) -> String {
    let p = parts(ts);
    format!(
        "{} {:02} {} {:02}:{:02}",
        DAYS[p.wday], p.mday, MONTHS[p.mon], p.hour, p.min
    )
}

/// `time.strftime('%H:%M', time.localtime(ts))`.
pub fn hhmm(ts: i64) -> String {
    let p = parts(ts);
    format!("{:02}:{:02}", p.hour, p.min)
}
