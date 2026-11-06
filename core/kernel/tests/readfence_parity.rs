#![allow(clippy::unwrap_used)]
#![cfg(unix)]
//! `kith kernel readfence` against `kernel/lib/kith_readfence.py`: the same payload, roots and environment give the same
//! bytes. The Python is run for real (python3 on the PATH).

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use kith_kernel::readfence::{Ctx, run};

fn py_run(
    args: &[String],
    stdin: &[u8],
    cwd: &str,
    home: Option<&str>,
    config: Option<&str>,
) -> String {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/lib/kith_readfence.py");
    let mut c = Command::new("python3");
    c.arg("-I")
        .arg(script)
        .args(args)
        .env_clear()
        .current_dir(cwd);
    if let Some(h) = home {
        c.env("HOME", h);
    }
    if let Some(cfg) = config {
        c.env("CLAUDE_CONFIG_DIR", cfg);
    }
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

#[test]
fn matches_the_python_for_every_payload() {
    let tmp = std::env::temp_dir().join(format!("readfence-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let real = std::fs::canonicalize(&tmp).unwrap();
    let wd = real.join("wd");
    let other = real.join("other");
    let home = real.join("home");
    let cfg = real.join("cfg");
    let results = cfg.join("projects/-wd/sess/tool-results");
    for d in [&wd.join("sub"), &other, &home, &results] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(wd.join("sub/f.txt"), "x").unwrap();
    std::fs::write(results.join("out.txt"), "x").unwrap();
    std::fs::write(cfg.join("projects/-wd/sess/other.txt"), "x").unwrap();
    std::fs::write(other.join("secret"), "x").unwrap();
    std::os::unix::fs::symlink(&other, wd.join("outlink")).unwrap();
    std::os::unix::fs::symlink(wd.join("sub"), wd.join("inlink")).unwrap();
    std::os::unix::fs::symlink("../other", wd.join("rel")).unwrap();
    // a chain of 42 links ending outside, and a loop
    for i in 0..42 {
        let to = if i == 41 {
            other.clone()
        } else {
            wd.join(format!("c{}", i + 1))
        };
        std::os::unix::fs::symlink(to, wd.join(format!("c{i}"))).unwrap();
    }
    std::os::unix::fs::symlink(wd.join("loopb"), wd.join("loopa")).unwrap();
    std::os::unix::fs::symlink(wd.join("loopa"), wd.join("loopb")).unwrap();
    let wd_s = wd.to_str().unwrap().to_owned();
    let other_s = other.to_str().unwrap().to_owned();
    let home_s = home.to_str().unwrap().to_owned();
    let cfg_s = cfg.to_str().unwrap().to_owned();
    let tr = results.join("out.txt").to_str().unwrap().to_owned();
    let nt = cfg
        .join("projects/-wd/sess/other.txt")
        .to_str()
        .unwrap()
        .to_owned();

    let q = |s: &str| serde_json::to_string(s).unwrap();
    let tool = |t: &str, k: &str, v: &str| {
        format!(
            r#"{{"tool_name":"{t}","tool_input":{{"{k}":{v}}},"cwd":{}}}"#,
            q(&wd_s)
        )
    };
    let mut cases: Vec<(String, String)> = Vec::new();
    for (t, k) in [
        ("Read", "file_path"),
        ("Grep", "path"),
        ("Glob", "path"),
        ("LS", "path"),
        ("NotebookRead", "notebook_path"),
    ] {
        cases.push((
            format!("{t} in root"),
            tool(t, k, &q(&format!("{wd_s}/sub/f.txt"))),
        ));
        cases.push((format!("{t} relative"), tool(t, k, &q("sub/f.txt"))));
        cases.push((
            format!("{t} out of root"),
            tool(t, k, &q(&format!("{other_s}/secret"))),
        ));
        cases.push((format!("{t} dotdot"), tool(t, k, &q("../other/secret"))));
        cases.push((format!("{t} missing"), tool(t, k, &q("nope/none.txt"))));
        cases.push((format!("{t} empty"), tool(t, k, r#""""#)));
        cases.push((format!("{t} null"), tool(t, k, "null")));
    }
    for (n, p) in [
        ("link out", "outlink/secret"),
        ("link in", "inlink/f.txt"),
        ("rel link out", "rel/secret"),
        ("dotdot after link", "outlink/../wd/sub/f.txt"),
        ("dotdot after link out", "outlink/../x"),
        ("root itself", "."),
        ("tilde", "~/x"),
        ("tilde alone", "~"),
        ("trailing slash", "sub/"),
        ("sibling prefix", "../wdx/f"),
        ("non-ascii", "caf\u{e9}/\u{1f600}\"q\\"),
    ] {
        cases.push((n.into(), tool("Read", "file_path", &q(p))));
    }
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty());
    if let Some(u) = &user {
        cases.push((
            "tilde USER".into(),
            tool("Read", "file_path", &q(&format!("~{u}/x"))),
        ));
        cases.push((
            "tilde USER alone".into(),
            tool("Read", "file_path", &q(&format!("~{u}"))),
        ));
    }
    cases.push(("tool-results".into(), tool("Read", "file_path", &q(&tr))));
    cases.push((
        "tool-results sibling".into(),
        tool("Read", "file_path", &q(&nt)),
    ));
    cases.push(("config dir itself".into(), tool("LS", "path", &q(&cfg_s))));
    cases.push(("number".into(), tool("Read", "file_path", "5")));
    cases.push(("float".into(), tool("Read", "file_path", "1.5e30")));
    cases.push(("bool".into(), tool("Read", "file_path", "true")));
    cases.push((
        "list".into(),
        tool("Read", "file_path", r#"["a", 1, null, "it's"]"#),
    ));
    cases.push((
        "object".into(),
        tool("Read", "file_path", r#"{"a": [1.0]}"#),
    ));
    cases.push(("non-file tool".into(), tool("Bash", "command", &q("ls /"))));
    cases.push(("no tool".into(), "{}".into()));
    cases.push(("tool list".into(), r#"{"tool_name":["Read"]}"#.into()));
    cases.push(("tool number".into(), r#"{"tool_name":5}"#.into()));
    cases.push(("no tool_input".into(), r#"{"tool_name":"Read"}"#.into()));
    cases.push((
        "tool_input empty list".into(),
        r#"{"tool_name":"Grep","tool_input":[],"cwd":"/"}"#.into(),
    ));
    cases.push((
        "tool_input string".into(),
        r#"{"tool_name":"Read","tool_input":"x"}"#.into(),
    ));
    cases.push((
        "Grep no path".into(),
        format!(
            r#"{{"tool_name":"Grep","tool_input":{{"pattern":"x"}},"cwd":{}}}"#,
            q(&wd_s)
        ),
    ));
    cases.push((
        "Grep no path outside".into(),
        format!(
            r#"{{"tool_name":"Grep","tool_input":{{"pattern":"x"}},"cwd":{}}}"#,
            q(&other_s)
        ),
    ));
    cases.push((
        "Read no param".into(),
        format!(
            r#"{{"tool_name":"Read","tool_input":{{}},"cwd":{}}}"#,
            q(&wd_s)
        ),
    ));
    cases.push((
        "no cwd".into(),
        r#"{"tool_name":"Read","tool_input":{"file_path":"f"}}"#.into(),
    ));
    cases.push((
        "cwd number relative".into(),
        r#"{"tool_name":"Read","tool_input":{"file_path":"f"},"cwd":5}"#.into(),
    ));
    cases.push((
        "cwd number absolute".into(),
        format!(
            r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}},"cwd":5}}"#,
            q(&format!("{wd_s}/sub"))
        ),
    ));
    cases.push((
        "cwd trailing slash".into(),
        format!(
            r#"{{"tool_name":"Read","tool_input":{{"file_path":"sub"}},"cwd":{}}}"#,
            q(&format!("{wd_s}/"))
        ),
    ));
    for (n, p) in [
        ("bad json", "{"),
        ("empty", ""),
        ("array", "[]"),
        ("null", "null"),
        ("string", "\"x\""),
        ("control byte", "\u{0}"),
    ] {
        cases.push((n.into(), p.into()));
    }

    let arg_sets: Vec<(&str, Vec<String>)> = vec![
        ("one root", vec!["--root".into(), wd_s.clone()]),
        ("no root", vec![]),
        (
            "trailing --root",
            vec!["--root".into(), wd_s.clone(), "--root".into()],
        ),
        (
            "double --root",
            vec!["--root".into(), "--root".into(), wd_s.clone()],
        ),
        (
            "two roots",
            vec![
                "--root".into(),
                other_s.clone(),
                "--root".into(),
                wd_s.clone(),
            ],
        ),
        ("slash root", vec!["--root".into(), "/".into()]),
        (
            "link root",
            vec!["--root".into(), format!("{wd_s}/outlink")],
        ),
    ];
    let configs: [Option<&str>; 3] = [Some(&cfg_s), Some(""), None];
    let mut n = 0;
    // Cases the Rust refuses where the Python reads the text as a plain name or follows a long chain.
    let stricter: Vec<(&str, String)> = vec![
        (
            "chain over the cap",
            tool("Read", "file_path", &q("c0/secret")),
        ),
        ("link loop", tool("Read", "file_path", &q("loopa/x"))),
        (
            "tilde user unknown",
            tool("Read", "file_path", &q("~nosuchuser_zz/x")),
        ),
    ];
    let refused = "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"deny\", \"permissionDecisionReason\": \"the read check could not run, so this read is refused\"}}\n";
    for (name, payload) in &stricter {
        let ctx = Ctx {
            home: Some(home_s.clone()),
            config_dir: None,
            cwd: Some(wd_s.clone()),
        };
        let got = run(&ctx, &["--root".into(), wd_s.clone()], payload.as_bytes());
        assert_eq!(got, refused, "stricter case {name:?}");
    }
    // HOME unset: both fall back to the password database (when the user has a home there).
    if std::env::var("HOME").is_ok_and(|h| std::path::Path::new(&h).is_dir()) {
        for (name, payload) in cases.iter().filter(|(n, _)| n.starts_with("tilde")) {
            let args = vec!["--root".to_owned(), wd_s.clone()];
            let want = py_run(&args, payload.as_bytes(), &wd_s, None, Some(&cfg_s));
            let ctx = Ctx {
                home: None,
                config_dir: Some(cfg_s.clone()),
                cwd: Some(wd_s.clone()),
            };
            assert_eq!(
                run(&ctx, &args, payload.as_bytes()),
                want,
                "HOME unset, case {name:?}"
            );
        }
    }
    for (name, payload) in &cases {
        for (ai, (an, args)) in arg_sets.iter().enumerate() {
            // The config variants matter to the tool-results rule, not to the root rules: one argument set carries them.
            for config in configs.iter().copied().take(if ai == 0 { 3 } else { 1 }) {
                let want = py_run(args, payload.as_bytes(), &wd_s, Some(&home_s), config);
                let ctx = Ctx {
                    home: Some(home_s.clone()),
                    config_dir: config.map(str::to_owned),
                    cwd: Some(wd_s.clone()),
                };
                let got = run(&ctx, args, payload.as_bytes());
                assert_eq!(got, want, "case {name:?} / {an} / config {config:?}");
                n += 1;
            }
        }
    }
    assert!(n > 400);
    let _ = std::fs::remove_dir_all(&tmp);
}
