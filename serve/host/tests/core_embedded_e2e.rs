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

/// A launch names a fresh 32-hex-digit once-folder each time; everything else in its answer must match.
fn without_run_ids(raw: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let run = raw[i..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
        if run == 32 {
            out.extend_from_slice(b"<id>");
            i += 32;
        } else {
            let step = run.max(1);
            out.extend_from_slice(&raw[i..i + step]);
            i += step;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `kith AGENT --launch-json` is the hot path: its Rust verb (`kith kernel launch AGENT --launch-json`, what the router
/// execs while `launch` is switched in lib/core-switched.txt) must answer the same through an embedded core as bare.
/// (Tested through the Rust verb directly: the router script is bash and finds the program through the host install.)
#[test]
fn launch_json_passes_through_an_embedded_core_and_answers_the_same() {
    use std::os::unix::fs::PermissionsExt as _;
    let h = home("launch");
    let p = paths(&h);
    let role = h.join("kith/agents/echo/role");
    std::fs::create_dir_all(&role).expect("role");
    std::fs::write(role.join("agent.md"), "---\nname: echo\ndescription: test agent\n---\nYou are echo.\n")
        .expect("agent");
    std::fs::create_dir_all(h.join("kith/state")).expect("state");
    std::fs::write(h.join("kith/state/user.md"), "About the owner.\n").expect("user");
    let fakes = h.join("fakes");
    std::fs::create_dir_all(&fakes).expect("fakes");
    let claude = fakes.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\ncase \"$1\" in --version) echo \"2.1.309 (Claude Code)\" ;; *) echo '[]' ;; esac\n",
    )
    .expect("claude");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let kernel = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel");
    let launch = || {
        Command::new(env!("CARGO_BIN_EXE_kith"))
            .args(["kernel", "launch", "echo", "--launch-json", "--kernel"])
            .arg(&kernel)
            .env_clear()
            .env("HOME", &h)
            .env("KITH_HOME", h.join("kith"))
            .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
            .output()
            .expect("run kith")
    };

    // No core yet: bare, and no core made as a side effect.
    let bare = launch();
    assert!(bare.status.success(), "{}", String::from_utf8_lossy(&bare.stderr));
    assert!(!p.machine.join("core").exists());

    // With a core: the same answer, and the launch took the lock and bumped the generation (1 at start, 2 for the
    // launch, 3 for this open).
    drop(kith_core::start(&p).expect("start"));
    let through = launch();
    assert!(through.status.success(), "{}", String::from_utf8_lossy(&through.stderr));
    assert_eq!(without_run_ids(&bare.stdout), without_run_ids(&through.stdout));
    assert_eq!(bare.stderr, through.stderr);
    match kith_core::open_embedded(&p).expect("open") {
        kith_core::Embedded::Open(core) => assert_eq!(core.generation(), 3),
        kith_core::Embedded::DaemonRunning { .. } => panic!("the lock should be free"),
    }
    let _ = std::fs::remove_dir_all(&h);
}
