//! The verbs already switched to Rust (paths, engines, launch) run inside an embedded kith-core: the real `kith`
//! binary takes the lifetime lock and bumps the core's generation for each one. The difftest cannot show this (it
//! compares the files a command leaves, and a core made on the side would differ), so it is proved here.

use std::path::PathBuf;
use std::process::Command;

fn home(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kith-core-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("kith")).expect("home");
    d
}

fn paths(home: &std::path::Path) -> kith_kernel::paths::Paths {
    let env = |k: &str| match k {
        "HOME" => Some(home.to_string_lossy().into_owned()),
        "KITH_HOME" => Some(home.join("kith").to_string_lossy().into_owned()),
        _ => None,
    };
    kith_kernel::paths::paths(home, &home.join("kith/kernel"), &env)
}

fn kith(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kith"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("KITH_HOME", home.join("kith"))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run kith")
}

#[test]
fn paths_and_engines_pass_through_an_embedded_core() {
    let h = home("verbs");
    let p = paths(&h);
    // No core yet: the verb runs bare and leaves no core behind.
    assert!(kith(&h, &["paths", "--json"]).status.success());
    assert!(!p.machine.join("core").exists());

    // The daemon (or `kith migrate`) made the core: generation 1.
    drop(kith_core::start(&p).expect("start"));
    for verb in [&["paths", "--json"][..], &["kernel", "paths", "--json"], &["kernel", "engines", "--json"]] {
        let out = kith(&h, verb);
        assert!(out.status.success(), "{verb:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    // Each of the three took the lock and bumped the generation; this open is the fifth holder.
    match kith_core::open_embedded(&p).expect("open") {
        kith_core::Embedded::Open(core) => assert_eq!(core.generation(), 5),
        kith_core::Embedded::DaemonRunning { .. } => panic!("the lock should be free"),
    }

    // A running daemon holds the lock: the verb still answers.
    let daemon = kith_core::start(&p).expect("daemon");
    let out = kith(&h, &["paths", "--json"]);
    assert!(out.status.success());
    drop(daemon);
    let _ = std::fs::remove_dir_all(&h);
}
