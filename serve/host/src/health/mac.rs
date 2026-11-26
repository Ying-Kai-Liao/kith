//! The macOS reader: sysctl for memory, swap and pressure, mach host statistics for cpu ticks and used pages,
//! getfsstat for the mounted volumes.

use super::{Disk, Memory, busy_percent};
use std::ffi::CString;
use std::mem::{MaybeUninit, size_of};
use std::time::Duration;

/// How long the cpu is watched: two tick counts this far apart.
const CPU_WINDOW: Duration = Duration::from_millis(250);

/// A fixed-size value from `sysctlbyname`; None when the name is unknown or the size is not `T`'s.
/// `T` must be a plain-data type that is valid when all bytes are zero (an integer or a C struct of them).
fn sysctl<T: Copy>(name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let mut v = MaybeUninit::<T>::zeroed();
    let mut len = size_of::<T>();
    // SAFETY: `v` is `len` writable bytes and the name is NUL-terminated; sysctl writes at most `len` bytes.
    let rc =
        unsafe { libc::sysctlbyname(cname.as_ptr(), v.as_mut_ptr().cast(), &raw mut len, std::ptr::null_mut(), 0) };
    // SAFETY: zeroed to begin with, and `T` is plain data (see above).
    (rc == 0 && len == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
fn pressure_word(level: i32) -> Option<&'static str> {
    match level {
        1 => Some("normal"),
        2 => Some("warn"),
        4 => Some("critical"),
        _ => None,
    }
}

#[allow(deprecated, reason = "libc marks the mach calls deprecated in favour of the mach2 crate")]
fn cpu_ticks() -> Option<[u32; 4]> {
    let mut info = MaybeUninit::<libc::host_cpu_load_info>::zeroed();
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `info` has room for `count` integers; the call fills it and updates `count`.
    let rc = unsafe {
        libc::host_statistics(
            libc::mach_host_self(),
            libc::HOST_CPU_LOAD_INFO,
            info.as_mut_ptr().cast(),
            &raw mut count,
        )
    };
    // SAFETY: filled by the call when it returned 0.
    (rc == 0).then(|| unsafe { info.assume_init() }.cpu_ticks)
}

pub fn cpu_percent() -> Option<f64> {
    let a = cpu_ticks()?;
    std::thread::sleep(CPU_WINDOW);
    let b = cpu_ticks()?;
    // Ticks are user, system, idle, nice.
    let d: Vec<u64> = a.iter().zip(&b).map(|(x, y)| u64::from(y.wrapping_sub(*x))).collect();
    busy_percent(d[0] + d[1] + d[3], d.iter().sum())
}

/// Pages the way Activity Monitor counts "Memory Used": app (internal minus purgeable), wired and compressed.
#[allow(deprecated, reason = "libc marks the mach calls deprecated in favour of the mach2 crate")]
fn used_bytes() -> Option<u64> {
    let page: i32 = sysctl("hw.pagesize")?;
    let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `vm` has room for `count` integers; the call fills it and updates `count`.
    let rc = unsafe {
        libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, vm.as_mut_ptr().cast(), &raw mut count)
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: filled by the call.
    let vm = unsafe { vm.assume_init() };
    let pages = u64::from(vm.internal_page_count).saturating_sub(u64::from(vm.purgeable_count))
        + u64::from(vm.wire_count)
        + u64::from(vm.compressor_page_count);
    Some(pages * u64::from(page.unsigned_abs()))
}

pub fn memory() -> Memory {
    let swap: Option<libc::xsw_usage> = sysctl("vm.swapusage");
    Memory {
        pressure: sysctl::<i32>("kern.memorystatus_vm_pressure_level").and_then(pressure_word),
        used: used_bytes(),
        total: sysctl::<u64>("hw.memsize"),
        swap_used: swap.map(|s| s.xsu_used),
        swap_total: swap.map(|s| s.xsu_total),
    }
}

/// Whether a mount is a volume of the person's: a disk filesystem, and not the sealed system snapshot at `/`, one
/// of the system's helper volumes (Preboot, VM, Update) under `/System/Volumes` (only Data is the person's), a
/// snapshot (`from` has an `@`), a Time Machine mount or a simulator runtime.
fn keep_mount(mount: &str, fstype: &str, from: &str) -> bool {
    if !matches!(fstype, "apfs" | "hfs" | "exfat" | "msdos" | "ntfs" | "ufs") || from.contains('@') {
        return false;
    }
    if mount == "/System/Volumes/Data" {
        return true;
    }
    !(mount == "/"
        || mount.starts_with("/System/Volumes/")
        || mount.starts_with("/Library/Developer/CoreSimulator/")
        || mount.contains("/.timemachine/")
        || mount.starts_with("/private/var/vm"))
}

/// What a volume is called: its mount's last part ("Macos 1TB"), "Data" for the system's own.
fn volume_name(mount: &str) -> String {
    mount.rsplit('/').find(|s| !s.is_empty()).unwrap_or("/").to_string()
}

fn text(raw: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = raw.iter().map(|c| c.to_ne_bytes()[0]).take_while(|b| *b != 0).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub fn disks() -> Vec<Disk> {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let Ok(len) = usize::try_from(n) else { return Vec::new() };
    if len == 0 {
        return Vec::new();
    }
    // A volume mounted since the count is left out, one unplugged since is left zeroed and skipped below.
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(len);
    let bytes = i32::try_from(len * size_of::<libc::statfs>()).unwrap_or(i32::MAX);
    // SAFETY: the buffer has room for `len` entries (`bytes` of them); getfsstat writes at most that many.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    let Ok(got) = usize::try_from(got) else { return Vec::new() };
    // SAFETY: getfsstat initialised the first `got` entries, and `got <= len` is the buffer's capacity.
    unsafe { buf.set_len(got.min(len)) };
    buf.iter()
        .filter_map(|s| {
            let (mount, fstype, from) = (text(&s.f_mntonname), text(&s.f_fstypename), text(&s.f_mntfromname));
            let total = s.f_blocks * u64::from(s.f_bsize);
            (keep_mount(&mount, &fstype, &from) && total > 0).then(|| Disk {
                name: volume_name(&mount),
                free: s.f_bavail * u64::from(s.f_bsize),
                mount,
                total,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_levels_are_the_systems_words() {
        assert_eq!(pressure_word(1), Some("normal"));
        assert_eq!(pressure_word(2), Some("warn"));
        assert_eq!(pressure_word(4), Some("critical"));
        assert_eq!(pressure_word(3), None);
    }

    #[test]
    fn only_the_persons_volumes_are_kept() {
        assert!(keep_mount("/System/Volumes/Data", "apfs", "/dev/disk3s5"));
        assert!(keep_mount("/Volumes/Macos 1TB", "apfs", "/dev/disk5s1"));
        assert!(keep_mount("/Volumes/USB", "exfat", "/dev/disk6s1"));
        assert!(!keep_mount("/", "apfs", "/dev/disk3s1s1"));
        assert!(!keep_mount("/System/Volumes/VM", "apfs", "/dev/disk3s6"));
        assert!(!keep_mount("/System/Volumes/Preboot", "apfs", "/dev/disk3s2"));
        assert!(!keep_mount("/dev", "devfs", "devfs"));
        assert!(!keep_mount("/System/Volumes/Data/home", "autofs", "map auto_home"));
        assert!(!keep_mount("/Volumes/Backup/.timemachine/x", "apfs", "com.apple.TimeMachine.2026@/dev/disk7s1"));
        assert!(!keep_mount("/Library/Developer/CoreSimulator/Volumes/iOS", "apfs", "/dev/disk8s1"));
    }

    #[test]
    fn a_volume_is_named_by_its_mount() {
        assert_eq!(volume_name("/Volumes/Macos 1TB"), "Macos 1TB");
        assert_eq!(volume_name("/System/Volumes/Data"), "Data");
        assert_eq!(volume_name("/"), "/");
    }
}
