//! `hostHealth`: this computer's cpu load, memory pressure and swap, and free space per mounted volume, for the
//! owner's app. What resource-manager's watch measures, read here without calling its scripts. Each platform has a
//! reader (`mac`, `linux`); where a number can't be read the field is null, never a failure. The answer is cached
//! for a few seconds, so an app polling it costs the host one read per window.

use anyhow::Result;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use mac as sys;

/// Neither macOS nor Linux (Windows): nothing is read yet, so every number is null and no volume is listed.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    pub fn cpu_percent() -> Option<f64> {
        None
    }
    pub fn memory() -> super::Memory {
        super::Memory::default()
    }
    pub fn disks() -> Vec<super::Disk> {
        Vec::new()
    }
}

const CACHE_FOR: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Memory {
    pressure: Option<&'static str>,
    used: Option<u64>,
    total: Option<u64>,
    swap_used: Option<u64>,
    swap_total: Option<u64>,
}

struct Disk {
    mount: String,
    name: String,
    total: u64,
    free: u64,
}

static CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

/// The answer to `hostHealth`. The read blocks (a short cpu sample, system calls), so it runs off the async runtime.
pub async fn answer() -> Result<Value> {
    if let Some((at, v)) = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
        && at.elapsed() < CACHE_FOR
    {
        return Ok(v.clone());
    }
    let v = tokio::task::spawn_blocking(collect).await?;
    *CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Instant::now(), v.clone()));
    Ok(v)
}

fn collect() -> Value {
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let cpu = sys::cpu_percent().map(|p| json!({"percent": (p * 10.0).round() / 10.0, "cores": cores}));
    let m = sys::memory();
    let disks: Vec<Value> = sys::disks()
        .into_iter()
        .map(|d| json!({"mount": d.mount, "name": d.name, "totalBytes": d.total, "freeBytes": d.free}))
        .collect();
    json!({
        "at": at,
        "cpu": cpu,
        "memory": {
            "pressure": m.pressure,
            "usedBytes": m.used,
            "totalBytes": m.total,
            "swapUsedBytes": m.swap_used,
            "swapTotalBytes": m.swap_total,
        },
        "disks": disks,
    })
}

/// The share of `busy` in `total` ticks, as a percentage; None when no time passed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn busy_percent(busy: u64, total: u64) -> Option<f64> {
    #[allow(clippy::cast_precision_loss, reason = "tick counts of one sample window are far below 2^52")]
    (total > 0).then(|| (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn busy_share_of_a_window() {
        assert_eq!(super::busy_percent(25, 100), Some(25.0));
        assert_eq!(super::busy_percent(0, 0), None);
        assert_eq!(super::busy_percent(200, 100), Some(100.0));
    }

    #[test]
    fn the_answer_has_every_key_of_the_contract() {
        let v = super::collect();
        for k in ["at", "cpu", "memory", "disks"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        for k in ["pressure", "usedBytes", "totalBytes", "swapUsedBytes", "swapTotalBytes"] {
            assert!(v["memory"].get(k).is_some(), "{k}");
        }
        assert!(v["disks"].is_array());
    }
}
