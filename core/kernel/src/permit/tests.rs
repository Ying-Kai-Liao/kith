#![allow(clippy::unwrap_used)]

use super::apply::covers;
use super::host::{call_host, restart_host};
use super::{Env, Out, hooks, pyval, run};
use serde_json::{Map, Value, json};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// A short folder (unix socket paths are short on macOS).
fn world(name: &str) -> PathBuf {
    let root = PathBuf::from(format!("/tmp/kp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn go(env: &Env, a: &[&str], stdin: &str) -> Out {
    run(
        env,
        Path::new("/nonexistent"),
        &args(a),
        &mut stdin.as_bytes(),
    )
}

/// A socket that reads one line, remembers it and answers `reply`.
fn socket_host(path: &Path, reply: &'static str) -> std::thread::JoinHandle<String> {
    let l = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        (&s).write_all(reply.as_bytes()).unwrap();
        line
    })
}

fn owner_env(sock: &Path) -> Env {
    Env::with(&[
        ("KITH_ASK_TOKEN", "ask-tok"),
        ("KITH_ASK_SOCKET", sock.to_str().unwrap()),
    ])
}

fn denied_payload(command: &str) -> String {
    json!({"hook_event_name": "PermissionDenied", "tool_name": "Bash", "tool_input": {"command": command},
           "reason": "Auto mode blocked it", "session_id": "s1"})
    .to_string()
}

fn pre_payload(command: &str) -> String {
    json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": command}}).to_string()
}

#[test]
fn once_keys_are_pythons() {
    let key = |tool: &str, input: &str| {
        hooks::once_key(tool, &serde_json::from_str::<Value>(input).unwrap())
    };
    // values printed by kernel/bin/kith-permit's once_key
    assert_eq!(
        key("Bash", r#"{"command": "ls", "description": "d"}"#),
        "d843942b56a7986db62a873359106ecdead895574b206385a9fe9eb5105a47e8"
    );
    assert_eq!(
        key(
            "Edit",
            r#"{"n": [1, 1.5, 1e20, null, true, {"b": 1, "a": 2}], "new_string": "\"q\"\té中😀\u0001", "file_path": "/a"}"#
        ),
        "139bd3c61242236c10caf773e80e35a7aafb09f0572175207d25e8468847a0d0"
    );
    assert_eq!(
        key("Read", "{}"),
        "569623c60422887b5e3ce753fdb89d74d2b362b74dd6312419d3e41d5b297e04"
    );
}

#[test]
fn python_helpers() {
    assert_eq!(pyval::int(" 12 "), Some(12));
    assert_eq!(pyval::int("+7"), Some(7));
    assert_eq!(pyval::int("1_0"), Some(10));
    assert_eq!(pyval::int("-3"), Some(-3));
    for bad in ["", "x", "1__0", "_1", "1_", "1.5", "0x10", "--1"] {
        assert_eq!(pyval::int(bad), None, "{bad}");
    }
    assert_eq!(pyval::repr_str("it's"), "\"it's\"");
    assert_eq!(pyval::repr_str("a'b\"c"), "'a\\'b\"c'");
    assert_eq!(pyval::strip("\u{1c} x \n"), "x");
    assert_eq!(
        pyval::repr(&json!({"a": [1, null, true, 1.5, "x"]})),
        "{'a': [1, None, True, 1.5, 'x']}"
    );
}

#[test]
fn covers_is_the_regex_of_kith_guard() {
    assert!(covers("/a/b", "/a/b"));
    assert!(covers("/a/b", "/a/b/c/d"));
    assert!(!covers("/a/b", "/a/bc"));
    assert!(covers("/a/b/**", "/a/b/c"));
    assert!(covers("/a/**/.git/hooks", "/a/.git/hooks"));
    assert!(covers("/a/**/.git/hooks", "/a/x/y/.git/hooks/pre-commit"));
    assert!(!covers("/a/**/.git/hooks", "/b/x/.git/hooks"));
    assert!(covers("/**/.claude", "/x/y/.claude/settings.json"));
    assert!(covers("/**/.claude", "/.claude"));
    // a single star is a plain character, and `.` takes no newline
    assert!(!covers("/a/*", "/a/b"));
    assert!(!covers("/a", "/a/b\nc"));
}

#[test]
fn a_line_that_is_no_verb_is_the_mcp_server() {
    let env = Env::default();
    for a in [
        &[][..],
        &["issue"],
        &["issue", "a", "b"],
        &["denied"],
        &["denied", "--dir"],
        &["once", "x", "y"],
        &["apply"],
        &["bogus"],
    ] {
        assert_eq!(go(&env, a, ""), Out::default(), "{a:?}");
        assert_eq!(go(&env, a, "\n  \n"), Out::default(), "{a:?}");
        assert_eq!(go(&env, a, "{\"jsonrpc\": \"2.0\"}\n").code, 2, "{a:?}");
    }
}

#[test]
fn issue_with_a_pid_that_is_no_number_says_so() {
    let out = go(&Env::default(), &["issue", "a", "s", "x"], "");
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stderr,
        "ValueError: invalid literal for int() with base 10: 'x'\n"
    );
}

#[test]
fn issue_asks_the_host_and_prints_export_lines() {
    let root = world("issue");
    std::fs::create_dir_all(root.join("serve")).unwrap();
    std::fs::write(root.join("serve/token"), "serve-tok\n").unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0u8; 4096];
        while !String::from_utf8_lossy(&seen).contains('}') {
            let n = s.read(&mut chunk).unwrap();
            seen.extend_from_slice(&chunk[..n]);
        }
        let body = r#"{"env": {"KITH_ASK_TOKEN": "t o'k", "KITH_ASK_SOCKET": "/tmp/s"}}"#;
        // chunked, to exercise the reader
        write!(s, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).unwrap();
        String::from_utf8(seen).unwrap()
    });
    let mut env = Env::with(&[("KITH_ASK_ID", "ask-1")]);
    env.set("KITH_SERVE_DIR", root.join("serve").to_str().unwrap());
    env.set("KITH_SERVE_PORT", &port.to_string());
    env.set("KITH_STATE", root.join(".kith").to_str().unwrap());
    env.home = root.clone();
    std::fs::create_dir_all(root.join(".kith/live/ask")).unwrap();
    std::fs::write(
        root.join(".kith/live/ask/ask-1.reg"),
        r#"{"agent": "beta"}"#,
    )
    .unwrap();
    let out = go(&env, &["issue", "alpha", "sess", "42"], "");
    assert_eq!(
        out.stdout,
        "export KITH_ASK_TOKEN='t o'\"'\"'k'\nexport KITH_ASK_SOCKET=/tmp/s\n"
    );
    assert_eq!(out.code, 0);
    let seen = server.join().unwrap();
    assert!(seen.starts_with("POST /api/issueAskToken HTTP/1.1\r\n"));
    assert!(seen.contains("Authorization: Bearer serve-tok\r\n"));
    assert!(seen.ends_with(
        r#"{"agent": "alpha", "session": "sess", "pid": 42, "replyTo": "ask-1", "askedBy": "beta"}"#
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn issue_without_a_host_and_without_a_restart_refuses() {
    let mut env = Env::with(&[("KITH_PERMIT_NO_RESTART", "1")]);
    env.set("KITH_SERVE_DIR", "/nonexistent");
    env.set("KITH_SERVE_PORT", "1");
    let out = go(&env, &["issue", "a", "s", "1"], "");
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stderr,
        "kith: no approval cards for this run: the Kith host isn't running; asks are refused\n"
    );
}

#[test]
fn restart_runs_launchctl_in_the_apps_order_and_waits_for_the_port() {
    let root = world("restart");
    let log = root.join("calls");
    let script = root.join("lc");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\n[ \"$1\" = print ] && exit 1\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut env = Env::with(&[("KITH_LAUNCHCTL", script.to_str().unwrap())]);
    env.set("KITH_SERVE_DIR", "/x");
    env.set(
        "KITH_SERVE_PORT",
        &l.local_addr().unwrap().port().to_string(),
    );
    env.home = root.clone();
    assert!(restart_host(
        &env,
        Path::new("/k"),
        true,
        Duration::from_secs(3)
    ));
    let calls: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        calls[0].starts_with("print gui/") && calls[0].ends_with("/dev.kith.serve"),
        "{calls:?}"
    );
    assert!(
        calls[1].starts_with("bootstrap gui/")
            && calls[1].ends_with("/Library/LaunchAgents/dev.kith.serve.plist"),
        "{calls:?}"
    );
    assert!(calls[2].starts_with("kickstart -k gui/"), "{calls:?}");
    // not on macOS: nothing is run
    std::fs::remove_file(&log).unwrap();
    assert!(!restart_host(
        &env,
        Path::new("/k"),
        false,
        Duration::from_secs(1)
    ));
    assert!(!log.exists());
    // a launchctl that refuses
    std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
    assert!(!restart_host(
        &env,
        Path::new("/k"),
        true,
        Duration::from_secs(1)
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn call_host_is_one_line_each_way() {
    let root = world("call");
    let sock = root.join("s");
    let server = socket_host(&sock, "{\"ok\": true, \"behavior\": \"allow\"}\n");
    let reply = call_host(
        &owner_env(&sock),
        "requestPermission",
        &Map::new(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(reply["behavior"], "allow");
    let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(
        sent,
        json!({"token": "ask-tok", "method": "requestPermission"})
    );
    // no token, no socket
    assert!(call_host(&Env::default(), "m", &Map::new(), Duration::from_secs(1)).is_none());
    assert!(
        call_host(
            &owner_env(&root.join("none")),
            "m",
            &Map::new(),
            Duration::from_secs(1)
        )
        .is_none()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn denied_allows_the_retry_once_and_only_once() {
    let root = world("denied");
    let sock = root.join("s");
    let dir = root.join("once/r1");
    let server = socket_host(&sock, "{\"ok\": true, \"behavior\": \"allow\"}\n");
    let env = owner_env(&sock);
    let out = go(
        &env,
        &["denied", "--dir", dir.to_str().unwrap()],
        &denied_payload("rm x"),
    );
    assert_eq!(
        out.stdout,
        "{\"hookSpecificOutput\": {\"hookEventName\": \"PermissionDenied\", \"retry\": true}}\n"
    );
    let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(
        sent["autoMode"],
        json!({"reason": "Auto mode blocked it", "session": "s1"})
    );
    assert_eq!(sent["tool"], "Bash");
    let once = |c: &str| {
        go(
            &Env::default(),
            &["once", "--dir", dir.to_str().unwrap()],
            &pre_payload(c),
        )
        .stdout
    };
    assert!(once("rm x").contains("\"permissionDecision\": \"allow\""));
    assert_eq!(once("rm x"), "");
    assert_eq!(once("another"), "");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_no_is_told_on_the_retry_and_not_told_twice() {
    let root = world("refused");
    let sock = root.join("s");
    let dir = root.join("once/r1");
    let server = socket_host(
        &sock,
        "{\"ok\": true, \"behavior\": \"deny\", \"message\": \"Not today\"}\n",
    );
    let env = owner_env(&sock);
    let d = dir.to_str().unwrap();
    assert!(
        go(&env, &["denied", "--dir", d], &denied_payload("c"))
            .stdout
            .contains("\"retry\": true")
    );
    server.join().unwrap();
    // the same call again inside ten minutes: no card, no retry (the socket is gone, so a card would fail loudly)
    assert_eq!(
        go(&env, &["denied", "--dir", d], &denied_payload("c")).stdout,
        ""
    );
    let out = go(&Env::default(), &["once", "--dir", d], &pre_payload("c")).stdout;
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"]["permissionDecisionReason"],
        "Auto mode blocked this. Not today"
    );
    assert_eq!(
        go(&Env::default(), &["once", "--dir", d], &pre_payload("c")).stdout,
        ""
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_run_without_the_app_or_a_verdict_is_told_or_left_alone() {
    let root = world("novd");
    let dir = root.join("once/r1");
    let d = dir.to_str().unwrap();
    let out = go(
        &Env::default(),
        &["denied", "--dir", d],
        &denied_payload("c"),
    );
    assert!(out.stdout.contains("\"retry\": true"));
    let note = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("note-")
        })
        .unwrap();
    assert!(
        std::fs::read_to_string(note)
            .unwrap()
            .contains("this run has no way to reach the Kith app")
    );
    let nv = json!({"hook_event_name": "PermissionDenied", "tool_name": "Bash", "tool_input": {}, "reason": "Classifier unavailable now"});
    assert_eq!(
        go(&Env::default(), &["denied", "--dir", d], &nv.to_string()).stdout,
        ""
    );
    // a guest's token is never used
    let guest = Env::with(&[
        ("KITH_RUN_TOKEN", "t"),
        ("KITH_ASK_TOKEN", "t"),
        ("KITH_ASK_SOCKET", "/nonexistent"),
    ]);
    assert_eq!(
        go(&guest, &["denied", "--dir", d], &denied_payload("g")).stdout,
        ""
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_short_one_shot_expires() {
    let root = world("short");
    let d = root.join("r1");
    std::fs::create_dir_all(&d).unwrap();
    let key = |c: &str| hooks::once_key("Bash", &json!({"command": c}));
    std::fs::write(d.join(key("fresh")), "4000000000 short\n").unwrap();
    std::fs::write(d.join(key("old")), "1000000000 short\n").unwrap();
    std::fs::write(d.join(key("odd")), "abc short\n").unwrap();
    let once = |c: &str| {
        go(
            &Env::default(),
            &["once", "--dir", d.to_str().unwrap()],
            &pre_payload(c),
        )
        .stdout
    };
    assert!(once("fresh").contains("allow"));
    assert_eq!(once("old"), "");
    assert_eq!(once("odd"), "");
    // used up either way
    assert!(!d.join(key("old")).exists() && !d.join(key("odd")).exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn apply_adds_what_the_host_granted() {
    let root = std::fs::canonicalize(world("apply")).unwrap();
    let proj = root.join("proj");
    std::fs::create_dir_all(proj.join("docs")).unwrap();
    std::fs::write(proj.join("notes.txt"), "n").unwrap();
    let file = root.join("s.json");
    std::fs::write(&file, r#"{"sandbox": {"filesystem": {"denyRead": ["/"]}}}"#).unwrap();
    let sock = root.join("s");
    let reply: &'static str = Box::leak(
        format!(
            "{}\n",
            json!({"ok": true, "grants": {"read": [proj.join("docs"), proj.join("notes.txt"), "relative", root.join(".kith/secrets")],
                                        "write": [proj.join(".claude")], "domains": ["example.com", "no_good"]}})
        )
        .into_boxed_str(),
    );
    let server = socket_host(&sock, reply);
    let mut env = Env::with(&[
        ("KITH_RUN_TOKEN", "run-tok"),
        ("KITH_RUN_SOCKET", sock.to_str().unwrap()),
        ("KITH_STATE", root.join(".kith").to_str().unwrap()),
    ]);
    env.home = root.clone();
    let out = go(&env, &["apply", file.to_str().unwrap()], "");
    server.join().unwrap();
    assert_eq!(out.code, 0);
    let p = proj.display();
    assert_eq!(
        out.stderr,
        format!(
            "kith: access given: read {p}/docs\nkith: access given: read {p}/notes.txt\nkith: access given: domain example.com\n"
        )
    );
    let s: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        s["sandbox"]["filesystem"]["allowRead"],
        json!([format!("{p}/docs"), format!("{p}/notes.txt")])
    );
    assert_eq!(
        s["sandbox"]["network"]["allowedDomains"],
        json!(["example.com"])
    );
    assert_eq!(
        s["permissions"]["additionalDirectories"],
        json!([format!("{p}/docs")])
    );
    assert_eq!(
        s["permissions"]["deny"],
        json!([
            format!("Edit(/{p}/docs/**)"),
            format!("Edit(/{p}/notes.txt)")
        ])
    );
    // the file is written the way json.dump(indent=1) writes it, with no newline at the end
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.starts_with("{\n \"sandbox\": {\n  \"filesystem\": {"),
        "{text}"
    );
    assert!(!text.ends_with('\n'));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn apply_that_cannot_use_the_file_goes_on_without_it() {
    let root = std::fs::canonicalize(world("apply-bad")).unwrap();
    let sock = root.join("s");
    let reply = "{\"ok\": true, \"grants\": {\"domains\": [\"example.com\"]}}\n";
    let mut env = Env::with(&[
        ("KITH_RUN_TOKEN", "t"),
        ("KITH_RUN_SOCKET", sock.to_str().unwrap()),
    ]);
    env.home = root.clone();
    let try_file = |name: &str, text: Option<&str>| {
        let file = root.join(name);
        if let Some(t) = text {
            std::fs::write(&file, t).unwrap();
        }
        let l = socket_host(&sock, reply);
        let out = go(&env, &["apply", file.to_str().unwrap()], "");
        l.join().unwrap();
        let _ = std::fs::remove_file(&sock);
        (out, file)
    };
    let (out, file) = try_file("missing.json", None);
    assert_eq!(out.code, 0);
    assert_eq!(
        out.stderr,
        format!(
            "kith: could not add the granted access ([Errno 2] No such file or directory: '{}'); the run goes on without it\n",
            file.display()
        )
    );
    let (out, _) = try_file("bad.json", Some("{"));
    assert!(
        out.stderr.contains(
            "(Expecting property name enclosed in double quotes: line 1 column 2 (char 1))"
        ),
        "{}",
        out.stderr
    );
    let (out, _) = try_file("list.json", Some("[]"));
    assert!(
        out.stderr
            .contains("(list indices must be integers or slices, not str)"),
        "{}",
        out.stderr
    );
    let (out, _) = try_file("nosb.json", Some("{}"));
    assert!(out.stderr.contains("('sandbox')"), "{}", out.stderr);
    let (out, f) = try_file("nofs.json", Some(r#"{"sandbox": {}}"#));
    assert!(out.stderr.contains("('filesystem')"), "{}", out.stderr);
    assert_eq!(std::fs::read_to_string(f).unwrap(), r#"{"sandbox": {}}"#);
    let _ = std::fs::remove_dir_all(root);
}
