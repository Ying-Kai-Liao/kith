//! UTC dates used by the tidier and transcript search.

pub fn ymd(ms: i64) -> String {
    if ms <= 0 {
        return "unknown date".into();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map_or_else(|| "unknown date".into(), |date| date.format("%Y-%m-%d").to_string())
}
