#![allow(clippy::unwrap_used)]
//! The unit rules of `project-id`; the whole verb is compared with the Python by `kernel/tests/difftest` (golden/project-id.json).

use std::path::Path;

use super::{Ctx, Out, is_guest, normalize, repr, run};

fn ctx(root: &Path) -> Ctx {
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    Ctx {
        state,
        home: root.join("home"),
        cwd: root.to_path_buf(),
        guest: false,
    }
}

fn go(c: &Ctx, args: &[&str]) -> Out {
    run(c, &args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("kith-projid-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    // realpath, so a tmp that is itself a link (macOS /var) does not change what the paths compare to
    std::fs::canonicalize(&d).unwrap()
}

#[test]
fn remotes_are_normalised_to_host_slash_owner_slash_repo() {
    for (url, want) in [
        ("git@github.com:Owner/Repo.git", "github.com/owner/repo"),
        ("https://github.com/Owner/Repo", "github.com/owner/repo"),
        ("ssh://git@host:22/a/B.git/", "host/22/a/b.git"),
        ("https://user@example.com/x.git", "example.com/x"),
        ("  github.com/a/b  ", "github.com/a/b"),
        ("/srv/git/repo.git", "/srv/git/repo"),
    ] {
        assert_eq!(normalize(url), want, "{url}");
    }
}

#[test]
fn repr_picks_quotes_like_python() {
    assert_eq!(repr("a"), "'a'");
    assert_eq!(repr("it's"), "\"it's\"");
    assert_eq!(repr("it's \"x\""), "'it\\'s \"x\"'");
    assert_eq!(repr("a\nb\\"), "'a\\nb\\\\'");
}

#[test]
fn a_guest_run_is_refused_not_answered() {
    let root = scratch("guest");
    let mut c = ctx(&root);
    c.guest = true;
    let out = go(&c, &[]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("kernel/bin/project-id"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn register_then_match_by_path_and_list() {
    let root = scratch("reg");
    let c = ctx(&root);
    let dir = root.join("work dir").join("inner");
    std::fs::create_dir_all(&dir).unwrap();
    let top = root.join("work dir");
    let top_s = top.to_str().unwrap();
    let out = go(&c, &["--register", "alpha", top_s, "--home", ""]);
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (0, "alpha\n"),
        "{}",
        out.stderr
    );
    let got = go(&c, &[dir.to_str().unwrap()]);
    assert_eq!((got.code, got.stdout.as_str()), (0, "alpha\n"));
    assert_eq!(go(&c, &[root.to_str().unwrap()]).code, 1);
    let list = go(&c, &["--list"]);
    assert_eq!(list.stdout, format!("alpha\t{top_s}\n"));
    let bad = go(&c, &["--info", "nope"]);
    assert_eq!(
        (bad.code, bad.stderr.as_str()),
        (1, "project-id: no project 'nope'\n")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_file_is_written_as_python_dumps_it() {
    let root = scratch("fmt");
    let c = ctx(&root);
    let dir = root.join("é dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        c.state.join("projects.json"),
        "{\"projects\": {\"é\": {\"remotes\": [], \"paths\": []}}, \"note\": \"é\"}",
    )
    .unwrap();
    let out = go(
        &c,
        &["--register", "p1", dir.to_str().unwrap(), "--home", ""],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = std::fs::read_to_string(c.state.join("projects.json")).unwrap();
    let want = format!(
        "{{\n  \"projects\": {{\n    \"\\u00e9\": {{\n      \"remotes\": [],\n      \"paths\": []\n    }},\n    \"p1\": {{\n      \"remotes\": [],\n      \"paths\": [\n        \"{}\"\n      ]\n    }}\n  }},\n  \"note\": \"\\u00e9\"\n}}\n",
        dir.to_str().unwrap().replace('é', "\\u00e9")
    );
    assert_eq!(text, want);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn register_refuses_a_bad_id_and_an_unknown_parent() {
    let root = scratch("bad");
    let c = ctx(&root);
    assert_eq!(go(&c, &["--register", "Bad"]).code, 2);
    let out = go(&c, &["--register", "x", "--home", "", "--in", "ghost"]);
    assert_eq!(out.code, 2);
    assert_eq!(
        out.stderr,
        "project-id: 'ghost' is not a project (register it first)\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn a_token_that_is_not_utf8_still_makes_a_guest() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let env = |token: OsString, socket: &'static str| {
        move |n: &str| match n {
            "KITH_RUN_TOKEN" => Some(token.clone()),
            "KITH_RUN_SOCKET" => Some(OsString::from(socket)),
            _ => None,
        }
    };
    assert!(is_guest(env(OsString::from_vec(vec![0xff, 0xfe]), "/s")));
    assert!(is_guest(env(OsString::from("t"), "/s")));
    assert!(!is_guest(env(OsString::from(""), "/s")));
    assert!(!is_guest(env(OsString::from("t"), "")));
}

#[test]
fn two_registers_at_once_both_land() {
    let root = scratch("concurrent");
    let c = std::sync::Arc::new(ctx(&root));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let c = c.clone();
            std::thread::spawn(move || {
                let id = format!("proj{i}");
                let out = go(&c, &["--register", &id, "--home", ""]);
                assert_eq!(out.code, 0, "{}", out.stderr);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(c.state.join("projects.json")).unwrap()).unwrap();
    for i in 0..8 {
        assert!(reg["projects"].get(format!("proj{i}")).is_some(), "proj{i} lost: {reg}");
    }
}

#[test]
fn a_crash_before_the_rename_leaves_the_old_file_whole() {
    let root = scratch("crash");
    let c = ctx(&root);
    assert_eq!(go(&c, &["--register", "first", "--home", ""]).code, 0);
    let file = c.state.join("projects.json");
    let before = std::fs::read_to_string(&file).unwrap();
    let err = super::write_atomic(&file, b"{\"projects\": {\"torn", true).unwrap_err();
    assert!(err.to_string().contains("crash"), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    let strays: Vec<_> = std::fs::read_dir(&c.state)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "temp left behind: {strays:?}");
}

#[test]
fn a_held_lock_makes_a_register_wait_until_it_is_released() {
    let root = scratch("held");
    let c = std::sync::Arc::new(ctx(&root));
    let held = super::lock(&c.state).unwrap();
    let c2 = c.clone();
    let t = std::thread::spawn(move || go(&c2, &["--register", "late", "--home", ""]));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!c.state.join("projects.json").exists(), "register ran under a held lock");
    drop(held);
    assert_eq!(t.join().unwrap().code, 0);
    assert!(c.state.join("projects.json").exists());
}
