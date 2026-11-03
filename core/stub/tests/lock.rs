//! The writer lock across real processes. Child roles re-run this test binary (`role` below) so no helper binary
//! is needed.

use kith_stub::{Error, Taken, file_name, take};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kith-stub-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn child(role: &str, folder: &Path, key: &str) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "role", "--nocapture", "--test-threads=1"])
        .env("KITH_STUB_ROLE", role)
        .env("KITH_STUB_DIR", folder)
        .env("KITH_STUB_KEY", key);
    c
}

fn held(t: Taken) -> kith_stub::Lock {
    match t {
        Taken::Held(l) => l,
        Taken::Busy => panic!("expected Held, got Busy"),
    }
}

// One reader for the child's whole life: a fresh BufReader per call would swallow the next line in its buffer.
fn line(reader: &mut impl BufRead, want: &str) -> String {
    loop {
        let mut s = String::new();
        assert!(reader.read_line(&mut s).unwrap() > 0, "child ended early");
        // The test harness prints "test role ... " before our first line, so match anywhere in the line.
        if let Some(at) = s.find(want) {
            return s[at + want.len()..].trim().to_string();
        }
    }
}

/// Not a test by itself: the body of the child processes the tests below start.
#[test]
fn role() {
    let Ok(role) = std::env::var("KITH_STUB_ROLE") else {
        return;
    };
    let folder = PathBuf::from(std::env::var("KITH_STUB_DIR").unwrap());
    let key = std::env::var("KITH_STUB_KEY").unwrap();
    match role.as_str() {
        // Exit 0 for Busy, 1 for Held.
        "try" => std::process::exit(match take(&folder, &key).unwrap() {
            Taken::Busy => 0,
            Taken::Held(_) => 1,
        }),
        // Take the lock, start a long-lived child (it must not inherit the lock), report both, wait to be killed.
        "holder" => {
            let _lock = held(take(&folder, &key).unwrap());
            // Never waited on on purpose: this process is killed while the sleeper lives on.
            #[allow(clippy::zombie_processes)]
            let sleeper = child("sleep", &folder, &key)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!("sleeper {}", sleeper.id());
            println!("locked");
            std::thread::sleep(Duration::from_secs(120));
        }
        "sleep" => std::thread::sleep(Duration::from_secs(25)),
        other => panic!("unknown role {other}"),
    }
}

#[test]
fn second_process_gets_busy_then_held_after_drop() {
    let dir = scratch("busy");
    let lock = held(take(&dir, "s1").unwrap());
    let status = child("try", &dir, "s1").status().unwrap();
    assert_eq!(status.code(), Some(0), "another process must see Busy");
    drop(lock);
    let status = child("try", &dir, "s1").status().unwrap();
    assert_eq!(status.code(), Some(1), "after drop it is free");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn second_take_in_the_same_process_is_busy() {
    let dir = scratch("inproc");
    let a = held(take(&dir, "s1").unwrap());
    assert!(matches!(take(&dir, "s1").unwrap(), Taken::Busy));
    let t = std::thread::spawn({
        let dir = dir.clone();
        move || matches!(take(&dir, "s1").unwrap(), Taken::Busy)
    });
    assert!(t.join().unwrap(), "a second thread must see Busy too");
    // A different key is independent.
    let b = held(take(&dir, "s2").unwrap());
    drop(a);
    let again = held(take(&dir, "s1").unwrap());
    // Windows cannot delete files that are still open.
    drop((b, again));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn child_of_a_killed_holder_does_not_keep_the_lock() {
    let dir = scratch("inherit");
    let mut holder = child("holder", &dir, "s1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(holder.stdout.take().unwrap());
    let sleeper: u32 = line(&mut reader, "sleeper").parse().unwrap();
    line(&mut reader, "locked");
    assert!(matches!(take(&dir, "s1").unwrap(), Taken::Busy));
    holder.kill().unwrap();
    holder.wait().unwrap();
    // The sleeper is still alive and would still hold an inherited descriptor.
    let start = Instant::now();
    let lock = loop {
        match take(&dir, "s1").unwrap() {
            Taken::Held(l) => break l,
            Taken::Busy => {
                assert!(start.elapsed() < Duration::from_secs(10), "lock still held");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    #[cfg(unix)]
    // SAFETY: signalling a pid we started.
    unsafe {
        assert_eq!(libc_kill(sleeper), 0, "sleeper was still running");
    }
    #[cfg(not(unix))]
    let _ = sleeper;
    drop(lock);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
unsafe fn libc_kill(pid: u32) -> i32 {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 9) }
}

#[test]
fn lock_file_stays_after_drop_and_is_reused() {
    let dir = scratch("stays");
    let lock = held(take(&dir, "s1").unwrap());
    let path = lock.path().to_path_buf();
    let inode = file_id(&path);
    drop(lock);
    assert!(path.exists(), "the lock file must survive a drop");
    for _ in 0..3 {
        let l = held(take(&dir, "s1").unwrap());
        assert_eq!(l.path(), path);
        drop(l);
    }
    assert_eq!(file_id(&path), inode, "the same file every time");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
fn file_id(p: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap())
}
// The file is never recreated, so its creation time is stable (std exposes no file index on stable).
#[cfg(windows)]
fn file_id(p: &Path) -> u64 {
    std::os::windows::fs::MetadataExt::creation_time(&std::fs::metadata(p).unwrap())
}

/// The library never deletes: no removal call appears in its source.
#[test]
fn source_never_removes_anything() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for code in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            for banned in [
                "remove_file",
                "remove_dir",
                "unlink",
                "DeleteFile",
                "rename(",
            ] {
                assert!(!code.contains(banned), "{banned} in: {code}");
            }
        }
    }
}

#[test]
fn works_without_core_db_and_creates_nested_folder() {
    let dir = scratch("nested").join("a").join("b");
    let l = held(take(&dir, "s1").unwrap());
    assert!(dir.is_dir());
    drop(l);
    std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn folder_is_0700_and_a_loose_own_folder_is_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let dir = scratch("mode");
    drop(held(take(&dir, "s1").unwrap()));
    assert_eq!(mode(&dir), 0o700);
    let lock_file = dir.join(file_name("s1").unwrap());
    assert_eq!(mode(&lock_file), 0o600);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(held(take(&dir, "s1").unwrap()));
    assert_eq!(mode(&dir), 0o700);
    // Parent folders made on the way are owner-only too.
    let nested = scratch("modeparent").join("x");
    drop(held(take(&nested, "s1").unwrap()));
    assert_eq!(mode(nested.parent().unwrap()), 0o700);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(nested.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_folder_is_refused() {
    let real = scratch("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = scratch("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(take(&link, "s1"), Err(Error::Folder(_))));
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_dir_all(&real).unwrap();
}

#[test]
fn keys_are_checked_not_rewritten() {
    for bad in [
        "",
        "..",
        "a/b",
        "a\\b",
        "../x",
        "a..b",
        "a\0b",
        "a\nb",
        "C:x",
        "a*b",
        "a?b",
        "a|b",
        "a<b",
        "a>b",
        "a\"b",
        "A",
        "Abc",
        "a b",
        "caf\u{e9}",
        "a\u{301}",
        "\u{ff21}",
    ] {
        assert!(matches!(file_name(bad), Err(Error::Key(_))), "{bad:?}");
    }
    assert!(file_name(&"a".repeat(201)).is_err());
    assert_eq!(file_name("0a1b-2c.3d_4").unwrap(), "w-0a1b-2c.3d_4.lock");
    assert_eq!(file_name("con").unwrap(), "w-con.lock");
    let dir = scratch("keys");
    assert!(matches!(take(&dir, "../escape"), Err(Error::Key(_))));
    assert!(!dir.exists(), "a bad key must not create anything");
}
