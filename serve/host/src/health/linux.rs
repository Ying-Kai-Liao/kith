//! The Linux reader: /proc/stat for cpu, /proc/meminfo for memory and swap, /proc/mounts and statvfs for volumes.
//! No memory pressure word: the kernel's pressure file needs a build option, so it stays null.

use super::{Disk, Memory, busy_percent};
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::time::Duration;

const CPU_WINDOW: Duration = Duration::from_millis(250);

/// The aggregate `cpu` line of /proc/stat as (busy, total) ticks: user, nice, system are busy; idle and iowait not.
fn cpu_line(stat: &str) -> Option<(u64, u64)> {
    let f: Vec<u64> =
        stat.lines().next()?.strip_prefix("cpu")?.split_whitespace().map_while(|x| x.parse().ok()).collect();
    if f.len() < 4 {
        return None;
    }
    let total: u64 = f.iter().take(8).sum();
    Some((f[0] + f[1] + f[2], total))
}

pub fn cpu_percent() -> Option<f64> {
    let a = cpu_line(&std::fs::read_to_string("/proc/stat").ok()?)?;
    std::thread::sleep(CPU_WINDOW);
    let b = cpu_line(&std::fs::read_to_string("/proc/stat").ok()?)?;
    busy_percent(b.0.saturating_sub(a.0), b.1.saturating_sub(a.1))
}

/// A /proc/meminfo value in bytes (the file counts kB).
fn meminfo(text: &str, key: &str) -> Option<u64> {
    let line = text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?;
    line.split_whitespace().next()?.parse::<u64>().ok().map(|kb| kb * 1024)
}

pub fn memory() -> Memory {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else { return Memory::default() };
    let total = meminfo(&text, "MemTotal");
    let swap_total = meminfo(&text, "SwapTotal");
    Memory {
        pressure: None,
        used: total.zip(meminfo(&text, "MemAvailable")).map(|(t, a)| t.saturating_sub(a)),
        total,
        swap_used: swap_total.zip(meminfo(&text, "SwapFree")).map(|(t, f)| t.saturating_sub(f)),
        swap_total,
    }
}

/// /proc/mounts escapes a space in a path as `\040`.
fn unescape(s: &str) -> String {
    s.replace("\\040", " ")
}

fn is_disk_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "ext2" | "ext3" | "ext4" | "xfs" | "btrfs" | "zfs" | "f2fs" | "vfat" | "exfat" | "ntfs" | "ntfs3" | "fuseblk"
    )
}

pub fn disks() -> Vec<Disk> {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return Vec::new() };
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_dev, mount, fstype) = (f.next()?, unescape(f.next()?), f.next()?);
            if !is_disk_fs(fstype) {
                return None;
            }
            let path = CString::new(mount.as_str()).ok()?;
            let mut s = MaybeUninit::<libc::statvfs>::zeroed();
            // SAFETY: a NUL-terminated path and a buffer of the struct's size.
            if unsafe { libc::statvfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: filled by the call when it returned 0.
            let s = unsafe { s.assume_init() };
            #[allow(clippy::useless_conversion, reason = "these are u32 on 32-bit targets")]
            let (frsize, blocks, avail) = (u64::from(s.f_frsize), u64::from(s.f_blocks), u64::from(s.f_bavail));
            let name = mount.rsplit('/').find(|p| !p.is_empty()).unwrap_or("/").to_string();
            (blocks > 0).then_some(Disk { mount, name, total: blocks * frsize, free: avail * frsize })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_line_is_busy_and_total_ticks() {
        let stat = "cpu  100 10 50 800 20 0 5 0 0 0\ncpu0 1 1 1 1 1 1 1 1 1 1\n";
        assert_eq!(cpu_line(stat), Some((160, 985)));
        assert_eq!(cpu_line("intr 1 2"), None);
    }

    #[test]
    fn meminfo_counts_bytes() {
        let t = "MemTotal:       16384 kB\nMemAvailable:    4096 kB\n";
        assert_eq!(meminfo(t, "MemTotal"), Some(16384 * 1024));
        assert_eq!(meminfo(t, "SwapTotal"), None);
    }
}
