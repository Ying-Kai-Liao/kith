//! Windows tests of the parked compaction process (W12-4): it is a spawn of this program that waits for `go`, starts the program
//! and ends with its exit code. The test binary stands in for `kith kernel lifecycle parked` (`proc::TEST_LAUNCHER`).
#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::proc::{self, TEST_LAUNCHER};

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        // with a space in the name: the sweep's folders are the user's
        let d = std::env::temp_dir().join(format!(
            "kith lifecycle {}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The parked process's side under the test binary: with `KITH_TEST_PARKED` set it runs [`proc::parked_main`] on the arguments
/// from `--cwd` on and ends with its code; run on its own it is a test that does nothing.
#[test]
fn parked_helper() {
    if std::env::var_os("KITH_TEST_PARKED").is_none() {
        return;
    }
    let all: Vec<String> = std::env::args().collect();
    let from = all.iter().position(|a| a == "--cwd").unwrap();
    std::process::exit(proc::parked_main(&all[from..]));
}

/// What the sweep passes: the whole environment, as the compaction does.
fn env() -> Vec<(OsString, OsString)> {
    let mut e: Vec<_> = std::env::vars_os().collect();
    e.push(("KITH_TEST_PARKED".into(), "1".into()));
    e
}

fn use_the_test_binary() {
    *TEST_LAUNCHER.lock().unwrap() = Some(vec![
        std::env::current_exe().unwrap().into_os_string(),
        "--exact".into(),
        "lifecycle::win_tests::parked_helper".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
        "--".into(),
    ]);
}

fn cmd(script: &str) -> Vec<OsString> {
    vec!["cmd".into(), "/C".into(), script.into()]
}

fn run(argv: &[OsString], cwd: &Path) -> (Vec<u8>, Option<i32>) {
    use_the_test_binary();
    let p = proc::parked(argv, cwd, &env(), false, None).unwrap();
    proc::write_byte(p.go, b'1');
    let said = proc::read_to_end(p.err);
    let rc = proc::reap(p.pid, Some(Duration::from_secs(60)), false);
    proc::close(p.go);
    proc::close(p.err);
    (said, rc)
}

#[test]
fn the_parked_process_runs_the_program_in_its_folder_when_told_and_ends_with_its_code() {
    let _one = proc::exclusive();
    let t = Tmp::new();
    let (said, rc) = run(&cmd("cd > here.txt && exit 7"), &t.0);
    assert_eq!(said, b"", "the program started");
    assert_eq!(rc, Some(7));
    let here = std::fs::read_to_string(t.0.join("here.txt")).unwrap();
    assert_eq!(Path::new(here.trim()), t.0.as_path());
    let (said, rc) = run(&cmd("exit 0"), &t.0);
    assert_eq!((said.as_slice(), rc), (&b""[..], Some(0)));
}

#[test]
fn a_program_that_will_not_start_says_why_and_the_code_is_127() {
    let _one = proc::exclusive();
    let t = Tmp::new();
    let (said, rc) = run(&["no-such-program-kith".into()], &t.0);
    let text = String::from_utf8(said).unwrap();
    assert!(text.contains("no-such-program-kith"), "{text}");
    assert_eq!(rc, Some(127));
    // a folder that is gone
    let (said, rc) = run(&cmd("exit 0"), &t.0.join("gone"));
    assert_ne!(said.len(), 0);
    assert_eq!(rc, Some(127));
}

#[test]
fn closing_go_unwritten_ends_it_without_running_anything() {
    let _one = proc::exclusive();
    let t = Tmp::new();
    use_the_test_binary();
    let p = proc::parked(&cmd("echo ran > ran.txt"), &t.0, &env(), false, None).unwrap();
    // parked: alive, and its pid is the one a claim would name
    assert!(matches!(
        crate::procstart::probe(u32::try_from(p.pid).unwrap()),
        crate::procstart::Probe::Alive(_)
    ));
    proc::close(p.go);
    assert_eq!(
        proc::reap(p.pid, Some(Duration::from_secs(60)), false),
        Some(127)
    );
    proc::close(p.err);
    assert!(!t.0.join("ran.txt").exists());
}

#[test]
fn a_run_past_the_timeout_is_ended_with_everything_below_it() {
    let _one = proc::exclusive();
    let t = Tmp::new();
    use_the_test_binary();
    let p = proc::parked(&cmd("ping -n 60 127.0.0.1 >NUL"), &t.0, &env(), false, None).unwrap();
    let top = u32::try_from(p.pid).unwrap();
    proc::write_byte(p.go, b'1');
    assert_eq!(proc::read_to_end(p.err), b"");
    // the parked process, `cmd` below it and `ping` below that
    let end = Instant::now() + Duration::from_secs(20);
    let ping = loop {
        let rows = kith_winsys::procs::table().unwrap();
        let found = rows
            .iter()
            .filter(|r| r.exe.eq_ignore_ascii_case("PING.EXE"))
            .find(|r| {
                rows.iter()
                    .find(|q| q.pid == r.parent)
                    .is_some_and(|q| q.parent == top)
            })
            .map(|r| r.pid);
        if let Some(pid) = found {
            break pid;
        }
        assert!(Instant::now() < end, "ping never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        proc::reap(p.pid, Some(Duration::from_millis(500)), false),
        None
    );
    let end = Instant::now() + Duration::from_secs(10);
    while !matches!(crate::procstart::probe(ping), crate::procstart::Probe::Gone) {
        assert!(
            Instant::now() < end,
            "ping below the parked process is still running"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(matches!(
        crate::procstart::probe(top),
        crate::procstart::Probe::Gone
    ));
    proc::close(p.go);
    proc::close(p.err);
}

#[test]
fn which_finds_a_program_by_its_bare_name_on_the_path() {
    let t = Tmp::new();
    std::fs::write(t.0.join("claude.cmd"), "@exit 0").unwrap();
    let path = t.0.to_string_lossy().into_owned();
    let env = move |k: &str| (k == "PATH").then(|| path.clone());
    assert_eq!(proc::which("claude", &env), Some(t.0.join("claude.cmd")));
    assert_eq!(proc::which("nothing-here", &env), None);
}
