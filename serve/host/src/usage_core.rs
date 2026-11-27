//! The host's plan-meter readings, written to the core's account row (`kith account usage ID --json READING`) so
//! `kith account list --json` and the `accounts` answer show them. Best effort: a refused or failed write is logged
//! once until one works again, and never fails the poll.

use crate::usage::Window;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// The core takes 1 to 8 windows per reading.
const MAX_WINDOWS: usize = 8;

static WARNED: AtomicBool = AtomicBool::new(false);

/// A name the core takes: up to 32 letters, digits, space . _ -, starting with a letter or digit.
fn name_ok(s: &str) -> bool {
    s.chars().count() <= 32
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || " _.-".contains(c))
}

/// The core's reading for `windows` read at `now_ms`: names are the window ids (the core carries no label or reset text),
/// times are epoch seconds. `None` when no window is one the core would take.
pub fn reading(windows: &[Window], now_ms: i64) -> Option<Value> {
    let wins: Vec<Value> = windows
        .iter()
        .filter(|w| name_ok(&w.id) && w.percent.is_finite())
        .take(MAX_WINDOWS)
        .map(|w| {
            let mut out = json!({"name": w.id, "percent": w.percent.clamp(0.0, 100.0)});
            if let Some(at) = w.resets_at {
                out["resets_at"] = json!(at / 1000);
            }
            out
        })
        .collect();
    (!wins.is_empty()).then(|| json!({"windows": wins, "read_at": now_ms / 1000}))
}

/// Writes `windows` into account `id`'s row (`claude`, `codex`, `claude:work`, ...).
pub async fn write(id: &str, windows: &[Window]) {
    let Some(reading) = reading(windows, crate::store::now_ms()) else { return };
    let result = async {
        let bin = tokio::task::spawn_blocking(crate::agent::hq::bin)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no kith binary"))?;
        let text = reading.to_string();
        crate::agent::hq::run(&bin, &["account", "usage", id, "--json", &text], None, WRITE_TIMEOUT).await
    }
    .await;
    match result {
        Ok(_) => WARNED.store(false, Ordering::Relaxed),
        Err(error) => {
            if !WARNED.swap(true, Ordering::Relaxed) {
                let error = format!("{error:#}");
                tracing::warn!(entry = %id, error, "couldn't record an account's usage in the core");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_to_the_cores_reading() {
        let mut w = Window::new("five_hour", "5-hour", 42.5, Some(1_790_000_123_456));
        w.resets_text = Some("Sep 25".into());
        let r = reading(&[w, Window::new("seven_day", "Weekly", 130.0, None)], 1_790_000_000_999).unwrap();
        assert_eq!(
            r,
            json!({"windows": [{"name": "five_hour", "percent": 42.5, "resets_at": 1_790_000_123},
                               {"name": "seven_day", "percent": 100.0}], "read_at": 1_790_000_000})
        );
    }

    #[test]
    fn leaves_out_names_the_core_would_refuse() {
        let bad = Window::new("seven_day_fable/x", "x", 1.0, None);
        assert!(reading(&[bad], 0).is_none());
        let many: Vec<Window> = (0..10).map(|i| Window::new(format!("w{i}"), "w", 1.0, None)).collect();
        assert_eq!(reading(&many, 0).unwrap()["windows"].as_array().unwrap().len(), MAX_WINDOWS);
    }
}
