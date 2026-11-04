//! The clock the lease reads (verification/lease, `SleepClockCounts = TRUE`). It must count the time the machine
//! sleeps: a hand that sleeps ten minutes inside a thirty second lease has to wake to a lapsed lease, so the first
//! thing it runs (the expiry check) kills the guests. `std::time::Instant` does not do that (it stops in sleep on
//! macOS and on Linux), so it is never used for a lease deadline.
//!
//! - Linux: `CLOCK_BOOTTIME`, which counts suspend (`CLOCK_MONOTONIC` does not).
//! - macOS: `CLOCK_MONOTONIC_RAW`. clock_gettime(3) says `CLOCK_MONOTONIC` will "continue to increment while the
//!   system is asleep", and that `CLOCK_UPTIME_RAW` works "in the same manner as CLOCK_MONOTONIC_RAW, but does not
//!   increment while the system is asleep", so RAW does count sleep (as in the XNU source). Not observed on a real
//!   sleep yet. `CLOCK_UPTIME_RAW` / `mach_absolute_time` stop in sleep and are never used. Kept over
//!   `CLOCK_MONOTONIC`, whose sleep guarantee the page states outright, only because RAW is unslewed.
//! - Windows: `QueryInterruptTime`, the interrupt-time count, which includes sleep and hibernation (not
//!   `QueryUnbiasedInterruptTime`, which leaves it out).
//! - Other Unix: `CLOCK_MONOTONIC`, the best there is.
//!
//! A reading is a `Duration` since an arbitrary point (boot): only differences mean anything. A [`Clock`] is a
//! function so the tests can hand the holder a clock they move themselves.

use std::sync::Arc;
use std::time::Duration;

/// Reads the lease clock.
pub type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The real clock.
pub fn lease_clock() -> Clock {
    Arc::new(now)
}

#[cfg(target_os = "linux")]
const ID: libc::clockid_t = libc::CLOCK_BOOTTIME;
#[cfg(target_vendor = "apple")]
const ID: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;
#[cfg(all(unix, not(target_os = "linux"), not(target_vendor = "apple")))]
const ID: libc::clockid_t = libc::CLOCK_MONOTONIC;

#[cfg(unix)]
fn now() -> Duration {
    read(ID)
}

#[cfg(unix)]
fn read(id: libc::clockid_t) -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out pointer, and the clock ids above exist on their targets.
    let rc = unsafe { libc::clock_gettime(id, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime failed");
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

#[cfg(windows)]
fn now() -> Duration {
    let mut ticks = 0u64;
    // SAFETY: `ticks` is a valid out pointer.
    unsafe { windows_sys::Win32::System::WindowsProgramming::QueryInterruptTime(&mut ticks) };
    // 100 ns units.
    Duration::from_nanos(ticks.saturating_mul(100))
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    /// A clock the test moves by hand.
    #[derive(Clone, Default)]
    pub struct Fake(Arc<Mutex<Duration>>);

    impl Fake {
        pub fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }

        pub fn clock(&self) -> Clock {
            let t = self.0.clone();
            Arc::new(move || *t.lock().unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_clock_does_not_go_backwards() {
        let c = lease_clock();
        let a = c();
        std::thread::sleep(Duration::from_millis(5));
        let b = c();
        assert!(b >= a + Duration::from_millis(5), "{a:?} {b:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_it_is_never_behind_the_clock_that_stops_in_suspend() {
        assert!(now() >= read(libc::CLOCK_MONOTONIC));
    }
}
