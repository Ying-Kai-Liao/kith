//! The arguments of this user's processes, read exactly: macOS `sysctl(KERN_PROCARGS2)`, Linux `/proc/<pid>/cmdline`.
//! Never a `ps` line split on spaces: a path with a space in it would come apart.

#![allow(unsafe_code)]

/// Every process of the calling user as (pid, its argv). A process that cannot be read (gone, or not ours) is left out.
#[must_use]
pub fn own_commands() -> Vec<(u32, Vec<String>)> {
    imp::own_commands()
}

/// A `KERN_PROCARGS2` buffer: an `int` count, the executable's path, NUL padding, then that many NUL-terminated arguments.
/// Pure, so the macOS layout is tested on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let count = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let rest = buf.get(4..)?;
    let path_end = rest.iter().position(|b| *b == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|b| *b != 0)?;
    let argv: Vec<String> = rest[start..]
        .split(|b| *b == 0)
        .take(usize::try_from(count).ok()?)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some(argv)
}

/// `/proc/<pid>/cmdline`: the arguments, each ended by a NUL.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cmdline(raw: &[u8]) -> Vec<String> {
    let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::fs::MetadataExt as _;

    pub(super) fn own_commands() -> Vec<(u32, Vec<String>)> {
        // SAFETY: geteuid only reads.
        let me = unsafe { libc::geteuid() };
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|e| {
                let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                (e.metadata().ok()?.uid() == me).then_some(())?;
                let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
                Some((pid, super::parse_cmdline(&raw)))
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    fn pids() -> Vec<u32> {
        // SAFETY: a null buffer of size 0 only asks how many processes there are.
        let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let Ok(n) = usize::try_from(n) else {
            return Vec::new();
        };
        let mut buf = vec![0 as libc::c_int; n + 64];
        let Ok(bytes) = libc::c_int::try_from(buf.len() * size_of::<libc::c_int>()) else {
            return Vec::new();
        };
        // SAFETY: `buf` is `bytes` long and outlives the call, which writes at most that much.
        unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast::<libc::c_void>(), bytes) };
        // Zero entries (the unfilled tail) are no process.
        buf.into_iter()
            .filter_map(|p| u32::try_from(p).ok().filter(|p| *p > 0))
            .collect()
    }

    fn owned_by_me(pid: u32) -> bool {
        let Ok(pid) = libc::c_int::try_from(pid) else {
            return false;
        };
        // SAFETY: an all-zero proc_bsdinfo is a valid value for proc_pidinfo to fill in.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let Ok(size) = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
            return false;
        };
        // SAFETY: `info` is `size` bytes and outlives the call; proc_pidinfo writes at most `size` bytes into it.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        // SAFETY: geteuid only reads.
        got == size && info.pbi_uid == unsafe { libc::geteuid() }
    }

    fn argv(pid: u32) -> Option<Vec<String>> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut len = 0_usize;
        // SAFETY: a null buffer only asks for the size; `mib` is three ints.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0_u8; len];
        // SAFETY: `buf` is `len` bytes and outlives the call, which writes at most `len` bytes into it.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        super::parse_procargs2(&buf)
    }

    pub(super) fn own_commands() -> Vec<(u32, Vec<String>)> {
        pids()
            .into_iter()
            .filter(|p| owned_by_me(*p))
            .filter_map(|p| Some((p, argv(p)?)))
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    pub(super) fn own_commands() -> Vec<(u32, Vec<String>)> {
        Vec::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn procargs2_keeps_a_path_with_a_space_whole() {
        let mut b = 3_i32.to_ne_bytes().to_vec();
        b.extend(b"/bin/sh\0\0\0");
        b.extend(b"/bin/sh\0/Volumes/Macos 1TB/kith-sched\0tick\0HOME=/x\0");
        assert_eq!(
            parse_procargs2(&b).unwrap(),
            ["/bin/sh", "/Volumes/Macos 1TB/kith-sched", "tick"]
        );
    }

    #[test]
    fn cmdline_splits_on_nul_only() {
        assert_eq!(
            parse_cmdline(b"/bin/sh\0/a b/kith-sched\0tick\0"),
            ["/bin/sh", "/a b/kith-sched", "tick"]
        );
        assert_eq!(parse_cmdline(b""), Vec::<String>::new());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_process_is_listed_with_its_own_arguments() {
        let me = std::process::id();
        let all = own_commands();
        let mine = all.iter().find(|(p, _)| *p == me).expect("own pid listed");
        let first = std::env::args().next().unwrap();
        assert!(mine.1.contains(&first), "{:?}", mine.1);
    }
}
