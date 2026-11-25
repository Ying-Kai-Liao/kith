//! `project-id` (the program) answers like `kernel/bin/project-id` (the sh stub): same stdout, stderr and exit code for the same
//! input. The stub is run on Unix only; it finds the program through `KITH_BIN`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn stub() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/project-id")
}

/// A scratch home with an empty state folder, so neither run reads a real registry.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kith-project-id-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("state")).unwrap();
    std::fs::create_dir_all(d.join("work")).unwrap();
    d
}

fn env(cmd: &mut Command, home: &Path) {
    cmd.current_dir(home.join("work"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("KITH_STATE", home.join("state"))
        .env("KITH_HOME", home.join("kith"));
}

fn program(home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_project-id"));
    env(&mut c, home);
    c.args(args).output().unwrap()
}

fn through_stub(home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(stub());
    env(&mut c, home);
    c.env("KITH_BIN", env!("CARGO_BIN_EXE_kith")).args(args).output().unwrap()
}

#[test]
fn the_program_matches_the_stub() {
    let home = scratch("same");
    for args in [
        &["--list"][..],
        &[][..],
        &["--info", "nope"][..],
        &["--where", "nope"][..],
        &["--root"][..],
        &["--bogus"][..],
        &["--register", "demo", "work"][..],
        &["--list"][..],
        &["--info", "demo"][..],
        &["-h"][..],
    ] {
        let (a, b) = (program(&home, args), through_stub(&home, args));
        let home_s = home.to_string_lossy();
        let norm = |o: &[u8]| String::from_utf8_lossy(o).replace(&*home_s, "<home>");
        assert_eq!(norm(&a.stdout), norm(&b.stdout), "stdout of {args:?}");
        assert_eq!(norm(&a.stderr), norm(&b.stderr), "stderr of {args:?}");
        assert_eq!(a.status.code(), b.status.code(), "exit code of {args:?}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_guest_run_without_a_core_socket_is_answered_with_nothing_like_the_stub() {
    let home = scratch("guest");
    let run = |bin: &Path| {
        let mut c = Command::new(bin);
        env(&mut c, &home);
        c.env("KITH_RUN_TOKEN", "t").env("KITH_BIN", env!("CARGO_BIN_EXE_kith")).output().unwrap()
    };
    let (a, b) = (run(Path::new(env!("CARGO_BIN_EXE_project-id"))), run(&stub()));
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(a.status.code(), Some(1), "a guest is answered with nothing, never the owner's registry");
    let _ = std::fs::remove_dir_all(&home);
}
