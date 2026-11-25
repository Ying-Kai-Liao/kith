//! The claim and the engine of a run here (`start::claude`): the pid the claim names, the order, and the stub.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::assemble::Env;
use super::start::{Engine, Go, claude};
use crate::threads::Ctx;
use crate::threads::writer::Taker;

fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Ran {
    wrote: String,
    claims: Vec<String>,
    code: i32,
    root: PathBuf,
}

impl Ran {
    fn line(&self, key: &str) -> String {
        self.wrote
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_default()
            .to_owned()
    }
}

/// A run with a fake stub (which takes its lock arguments off and runs what follows) and a fake claude that writes what it
/// sees, started as a child of this process so that the test goes on after it.
fn run_fake(tag: &str, extra: &[(&str, &str)]) -> Ran {
    let root = std::env::temp_dir().join(format!("kith-start-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (state, machine, out) = (root.join("state"), root.join("machine"), root.join("out"));
    let claims = state.join("live/terminal");
    script(
        &root.join("bin/kith-stub"),
        "echo \"$@\" > \"$OUT.stub\"\nwhile [ \"$1\" != \"--\" ]; do shift; done; shift\nexec \"$@\"",
    );
    script(
        &root.join("bin/claude"),
        "{ echo \"pid=$$\"; echo \"ppid=$PPID\"; echo \"args=$*\"; echo \"claims=$(ls \"$CLAIMS\" 2>/dev/null | tr '\\n' ' ')\"; } > \"$OUT\"",
    );
    let mut env: Env = [
        (
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        ),
        ("HOME", root.display().to_string()),
        ("KITH_MACHINE", machine.display().to_string()),
        (
            "KITH_STUB_BIN",
            root.join("bin/kith-stub").display().to_string(),
        ),
        ("OUT", out.display().to_string()),
        ("CLAIMS", claims.display().to_string()),
        // no host to ask, and nothing to restart
        ("KITH_PERMIT_NO_RESTART", "1".to_owned()),
        ("KITH_SERVE_DIR", root.join("none").display().to_string()),
        ("KITH_SERVE_PORT", "1".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    for (k, v) in extra {
        env.insert((*k).to_owned(), (*v).to_owned());
    }
    let envf = |k: &str| env.get(k).cloned();
    let ctx = Ctx {
        state: Some(state),
        machine: Some(machine),
        home: root.clone(),
        env: &envf,
    };
    let taker = Taker::new(&ctx, None);
    let code = claude(
        Go {
            agent: "echo",
            kernel: &root.join("k"),
            env: env.clone(),
            args: vec!["--model".to_owned(), "x".to_owned()],
            task: "do it".to_owned(),
            owner: true,
            claim_person: String::new(),
            settings_file: root.join("settings.json"),
            permit_mcp: "{}".to_owned(),
            alone: false,
            sid_file: String::new(),
            kbin: None,
        },
        &taker,
        i64::from(std::process::id()),
        Engine::Spawn,
    );
    Ran {
        wrote: std::fs::read_to_string(&out).unwrap_or_default(),
        claims: std::fs::read_dir(&claims)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".claim"))
                    .collect()
            })
            .unwrap_or_default(),
        code,
        root,
    }
}

#[test]
fn the_claim_is_this_process_and_the_engine_descends_from_it_and_starts_after_it() {
    let r = run_fake("pid", &[]);
    assert_eq!(r.code, 0, "{}", r.wrote);
    let me = std::process::id().to_string();
    // I2: the engine descends from this process (here as its child; under `Engine::Exec` it is this very pid).
    assert_eq!(r.line("ppid="), me, "{}", r.wrote);
    // I3: the claim was there before the engine ran, and it names this process.
    assert_eq!(r.claims.len(), 1, "{:?}", r.claims);
    assert!(r.line("claims=").contains(".claim"), "{}", r.wrote);
    let claim: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(r.root.join("state/live/terminal").join(&r.claims[0])).unwrap(),
    )
    .unwrap();
    assert_eq!(claim["pid"].to_string(), me);
    assert_eq!(claim["kind"], "oneshot");
    // The session the claim is under is the one Claude was told to be, and the stub locks it.
    let sid = r.claims[0].trim_end_matches(".claim");
    assert!(
        r.line("args=")
            .contains(&format!("--session-id {sid} -p do it")),
        "{}",
        r.wrote
    );
    assert!(
        std::fs::read_to_string(r.root.join("out.stub"))
            .unwrap()
            .contains(&format!("--key {sid} --"))
    );
    let _ = std::fs::remove_dir_all(&r.root);
}

#[test]
fn a_required_stub_that_is_missing_stops_the_run_before_any_claim() {
    let r = run_fake(
        "nostub",
        &[
            ("KITH_REQUIRE_STUB", "1"),
            ("KITH_STUB_BIN", "/nonexistent/kith-stub"),
        ],
    );
    assert_eq!(r.code, 1);
    assert!(
        r.wrote.is_empty() && r.claims.is_empty(),
        "no engine, no claim"
    );
    let _ = std::fs::remove_dir_all(&r.root);
}

#[test]
fn a_missing_stub_is_a_warning_for_the_owner_and_the_run_is_still_claimed() {
    let r = run_fake("warn", &[("KITH_STUB_BIN", "/nonexistent/kith-stub")]);
    assert_eq!(r.code, 0, "{}", r.wrote);
    assert!(
        r.line("args=").starts_with("--model x --session-id"),
        "{}",
        r.wrote
    );
    assert_eq!(r.claims.len(), 1);
    let _ = std::fs::remove_dir_all(&r.root);
}
