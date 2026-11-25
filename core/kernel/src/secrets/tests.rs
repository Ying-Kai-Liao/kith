#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use super::*;
use crate::access::tests::{FUTURE, NOW, Tmp, ctx, people, rules};
use serde_json::json;
use std::process::Stdio;
use std::time::Instant;

#[test]
fn a_missing_file_is_empty_with_every_table() {
    let t = Tmp::new("missing");
    let a = load_auth(&ctx(&t)).unwrap();
    for table in TABLES {
        assert_eq!(a[table], json!({}), "{table}");
    }
}

#[test]
fn a_file_missing_tables_gets_the_defaults_and_keeps_the_rest() {
    let t = Tmp::new("old");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(
        &c.auth,
        r#"{"profiles": {"p": {}}, "extra": 1, "people": {"kai": {"owner": true}}}"#,
    )
    .unwrap();
    let a = load_auth(&c).unwrap();
    assert_eq!(a["extra"], json!(1));
    assert_eq!(a["members"], json!({}));
    assert_eq!(a["once"], json!({}));
    assert_eq!(owner(&a).as_deref(), Some("kai"));
}

#[test]
fn a_damaged_file_dies_with_pythons_words() {
    let t = Tmp::new("damaged");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    let path = c.auth.display().to_string();
    for (text, why) in [
        ("", "Expecting value: line 1 column 1 (char 0)"),
        (
            "{",
            "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
        ),
        (
            "{\"a\": 1,}",
            "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)",
        ),
        (
            "{\"a\" 1}",
            "Expecting ':' delimiter: line 1 column 6 (char 5)",
        ),
        ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
        ("{} x", "Extra data: line 1 column 4 (char 3)"),
        (
            "\n\n {\"a\": [1,\n 2 3]}",
            "Expecting ',' delimiter: line 4 column 4 (char 16)",
        ),
        ("\"a\\x\"", "Invalid \\escape: line 1 column 3 (char 2)"),
        (
            "\"abc",
            "Unterminated string starting at: line 1 column 1 (char 0)",
        ),
    ] {
        std::fs::write(&c.auth, text).unwrap();
        let d = load_auth(&c).unwrap_err();
        assert_eq!(
            d,
            Die {
                msg: format!("{path}: invalid JSON ({why})"),
                code: 1
            },
            "{text:?}"
        );
    }
    std::fs::write(&c.auth, "[]").unwrap();
    assert!(load_auth(&c).is_err());
}

// --- take_once and the file protocol ---------------------------------------------------------------------------------

fn write_auth(c: &Ctx, a: &Value) {
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, serde_json::to_string(a).unwrap()).unwrap();
}

fn once_auth() -> Value {
    let mut a = people();
    a["members"]["house"]["sam"]["helper_once"] = json!({"video": FUTURE, "late": 1});
    a
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn json_of(o: &Outcome) -> Value {
    serde_json::from_str(&o.stdout).unwrap()
}

#[test]
fn take_once_uses_the_grant_up() {
    let mut a = once_auth();
    assert!(take_once(&mut a, "sam", "video", Some("house")));
    assert!(!take_once(&mut a, "sam", "video", Some("house")), "used up");
    assert!(!take_once(&mut a, "sam", "video", None));
    assert!(!take_once(&mut a, "ola", "video", Some("house")));
    assert_eq!(
        a["members"]["house"]["sam"]["helper_once"],
        json!({"late": 1})
    );
}

#[test]
fn resolve_take_once_lets_one_launch_through_and_the_second_has_no_grant() {
    let t = Tmp::new("take-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let first = run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    );
    assert_eq!(first.code, 0);
    let v = json_of(&first);
    assert_eq!(v["allowed"], json!(true));
    assert_eq!(v["why"], "helper of house (just this once)");
    assert_eq!(v["held"], json!(false));
    let second = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(second["allowed"], json!(false));
    assert_eq!(
        second["why"],
        "sam may not use video (not in their house helpers)"
    );
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"],
        json!({"late": 1})
    );
}

#[test]
fn resolve_without_take_once_leaves_the_file_alone() {
    let t = Tmp::new("no-take");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let before = std::fs::read(&c.auth).unwrap();
    let v = json_of(&run(&c, "resolve", &args(&["video", "--via", "house"])));
    assert_eq!(v["allowed"], json!(false));
    assert_eq!(std::fs::read(&c.auth).unwrap(), before);
    assert!(
        !c.auth.with_file_name("auth.json.lock").exists()
            || std::fs::metadata(c.auth.with_file_name("auth.json.lock")).is_ok()
    );
}

#[test]
fn a_removal_between_the_card_and_the_launch_still_wins() {
    let t = Tmp::new("revoked-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    let mut a = once_auth();
    a["people"]["sam"]["revoked"] = json!(true);
    write_auth(&c, &a);
    let v = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(v["allowed"], json!(false));
    assert_eq!(v["why"], "sam's access was removed");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"]["video"],
        json!(FUTURE),
        "not used up"
    );
}

#[test]
fn a_held_person_does_not_use_the_grant() {
    let t = Tmp::new("held-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    std::fs::create_dir_all(t.0.join("serve")).unwrap();
    std::fs::write(
        t.0.join("serve/cloud-sync.json"),
        r#"{"linked": true, "until": 1}"#,
    )
    .unwrap();
    let v = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(v["held"], json!(true));
    assert_eq!(v["allowed"], json!(false));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"]["video"],
        json!(FUTURE)
    );
}

#[test]
fn bridge_held_reads_the_cloud_sync_file() {
    let t = Tmp::new("held");
    let c = ctx(&t);
    let a = people();
    let r = rules(&c, &a);
    let serve = t.0.join("serve");
    assert!(!r.bridge_held("sam"), "no file: never linked");
    std::fs::create_dir_all(&serve).unwrap();
    for (text, want) in [
        (r#"{"linked": true, "until": 1}"#, true),
        (r#"{"linked": true, "until": 4102444800000}"#, false),
        (r#"{"linked": false, "until": 1}"#, false),
        (r#"{"linked": true}"#, true),
        (r#"{"linked": true, "until": "abc"}"#, false),
        ("[1]", false),
        ("not json", false),
    ] {
        std::fs::write(serve.join("cloud-sync.json"), text).unwrap();
        assert_eq!(r.bridge_held("sam"), want, "{text}");
    }
    std::fs::write(
        serve.join("cloud-sync.json"),
        r#"{"linked": true, "until": 1}"#,
    )
    .unwrap();
    assert!(!r.bridge_held("kai"), "the owner is never held");
    assert!(!r.bridge_held(""));
}

#[test]
fn may_prints_why_and_exits_by_the_answer() {
    let t = Tmp::new("may-cli");
    let c = ctx(&t);
    write_auth(&c, &once_auth());
    assert_eq!(
        run(&c, "may", &args(&["sam", "house"])),
        Outcome {
            stdout: "member\n".into(),
            stderr: String::new(),
            code: 0
        }
    );
    let no = run(&c, "may", &args(&["sam", "video", "--via", "house"]));
    assert_eq!(
        (no.stdout.as_str(), no.code),
        ("sam may not use video (not in their house helpers)\n", 1)
    );
    let yes = run(
        &c,
        "may",
        &args(&["sam", "video", "--via", "house", "--once"]),
    );
    assert_eq!((yes.stdout.as_str(), yes.code), ("helper of house\n", 0));
    let usage = run(&c, "may", &args(&["sam"]));
    assert_eq!(
        (usage.stderr.as_str(), usage.code),
        ("kith-auth: may: see kith-auth --help\n", 2)
    );
    let unknown = run(&c, "frobnicate", &args(&[]));
    assert_eq!(unknown.code, 2);
}

#[test]
fn resolve_accepts_json_as_a_no_op_and_may_does_not() {
    let t = Tmp::new("json-flag");
    let c = ctx(&t);
    write_auth(&c, &people());
    let a = run(&c, "resolve", &args(&["house", "--json"]));
    let b = run(&c, "resolve", &args(&["house"]));
    assert_eq!(a, b);
    assert_eq!(run(&c, "may", &args(&["sam", "house", "--json"])).code, 2);
}

#[test]
fn an_unknown_dash_argument_is_usage_and_reads_no_auth_json() {
    let t = Tmp::new("usage");
    let c = ctx(&t);
    // no auth.json at all: reading it would create the lock file's folder, usage must touch nothing
    for list in [
        vec!["--help"],
        vec!["-h"],
        vec!["house", "--bogus"],
        vec!["house", "--person", "sam", "-x"],
        vec!["--once", "x"],
    ] {
        let o = run(&c, "resolve", &args(&list));
        assert_eq!(o.code, 2, "{list:?}");
        assert!(
            o.stderr
                .starts_with("kith-auth: usage: kith kernel secrets resolve"),
            "{list:?}"
        );
        assert_eq!(o.stdout, "");
    }
    let o = run(&c, "may", &args(&["--help"]));
    assert_eq!(o.code, 2);
    assert!(!t.0.join("secrets").exists(), "nothing was created or read");
    // a flag's value may start with a dash; known flags still work
    write_auth(&c, &people());
    assert_eq!(
        run(&c, "resolve", &args(&["house", "--via", "-x", "--json"])).code,
        0
    );
}

#[test]
fn resolve_acts_for_kith_person_and_the_owner_in_a_sandbox_is_no_one() {
    let t = Tmp::new("acting");
    write_auth(&ctx(&t), &people());
    let owner_run = run(&ctx(&t), "resolve", &args(&["house"]));
    assert_eq!(json_of(&owner_run)["person"], "kai");
    let boxed = Ctx {
        sandbox: Sandbox::Fixed(true),
        ..ctx(&t)
    };
    let v = json_of(&run(&boxed, "resolve", &args(&["house"])));
    assert_eq!(v["person"], "-");
    assert_eq!(v["allowed"], json!(false));
    let sam = Ctx {
        person: Some("samuel".into()),
        sandbox: Sandbox::Fixed(true),
        ..ctx(&t)
    };
    assert_eq!(
        json_of(&run(&sam, "resolve", &args(&["house"])))["person"],
        "sam"
    );
    let named = json_of(&run(
        &boxed,
        "resolve",
        &args(&["house", "--person", "sam"]),
    ));
    assert_eq!(named["person"], "sam");
}

#[test]
fn a_damaged_file_stops_resolve_with_exit_1_and_kith_auths_prefix() {
    let t = Tmp::new("resolve-damaged");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, "").unwrap();
    let o = run(&c, "resolve", &args(&["house"]));
    assert_eq!(o.code, 1);
    assert_eq!(
        o.stderr,
        format!(
            "kith-auth: {}: invalid JSON (Expecting value: line 1 column 1 (char 0))\n",
            c.auth.display()
        )
    );
    assert_eq!(o.stdout, "");
}

#[test]
fn save_auth_filters_like_kith_auth_and_writes_atomically_with_mode_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let t = Tmp::new("save");
    let c = ctx(&t);
    let mut a = once_auth();
    a["invites"] =
        json!({"live": {"expires": NOW / 1000.0 + 60.0}, "dead": {"expires": 5}, "none": {}});
    a["retired"] = json!({});
    write_auth(&c, &a);
    let mut locked = load_auth_locked(&c).unwrap();
    locked.auth["members"]["house"]["sam"]["note"] = json!("é");
    save_auth(&c, &locked).unwrap();
    drop(locked);
    let text = std::fs::read_to_string(&c.auth).unwrap();
    let saved: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved["invites"],
        json!({"live": {"expires": NOW / 1000.0 + 60.0}})
    );
    for gone in ["retired", "once"] {
        assert!(saved.get(gone).is_none(), "{gone}");
    }
    for kept in ["profiles", "grants", "env"] {
        assert_eq!(saved[kept], json!({}), "{kept}");
    }
    assert!(text.ends_with("}\n"));
    assert!(text.contains("\n  \"people\": {\n"), "indent=2");
    assert!(text.contains("\\u00e9"), "ASCII only");
    assert_eq!(
        std::fs::metadata(&c.auth).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let leftovers: Vec<_> = std::fs::read_dir(c.auth.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn the_save_matches_pythons_bytes() {
    // the file Python's save_auth would write for the same data (checked against kith-auth by hand and by the difftest)
    let t = Tmp::new("bytes");
    let c = ctx(&t);
    write_auth(
        &c,
        &json!({"people": {"kai": {"owner": true}}, "grants": {"a": {"s": ["p", "q"]}}}),
    );
    let locked = load_auth_locked(&c).unwrap();
    save_auth(&c, &locked).unwrap();
    assert_eq!(
        std::fs::read_to_string(&c.auth).unwrap(),
        "{\n  \"people\": {\n    \"kai\": {\n      \"owner\": true\n    }\n  },\n  \"grants\": {\n    \"a\": {\n      \"s\": [\n        \"p\",\n        \"q\"\n      ]\n    }\n  },\n  \"profiles\": {},\n  \"env\": {}\n}\n"
    );
}

#[test]
fn a_python_writer_holding_the_lock_makes_take_once_wait_and_see_its_write() {
    // Python's lock_auth/save_auth and ours must be one lock: hold it from Python, change the file there, and the
    // transaction here only reads once Python lets go.
    if Command::new("python3").arg("-V").output().is_err() {
        return;
    }
    let t = Tmp::new("py-lock");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let script = r"
import fcntl, json, sys, time
auth = sys.argv[1]
f = open(auth + '.lock', 'a')
fcntl.flock(f, fcntl.LOCK_EX)
print('locked', flush=True)
time.sleep(1.0)
d = json.load(open(auth))
d['members']['house']['sam']['helper_once'] = {}   # Python used it up
json.dump(d, open(auth, 'w'))
";
    let mut py = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(&c.auth)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(py.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line.trim(), "locked");
    let start = Instant::now();
    let out = run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    );
    let waited = start.elapsed();
    py.wait().unwrap();
    assert!(waited >= Duration::from_millis(700), "waited {waited:?}");
    // it saw Python's write: no grant left, so refused
    assert_eq!(json_of(&out)["allowed"], json!(false));
}

#[test]
fn two_launches_racing_for_one_grant_exactly_one_wins() {
    let t = Tmp::new("race");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let wins = std::thread::scope(|s| {
        let runs: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    json_of(&run(
                        &c,
                        "resolve",
                        &args(&["video", "--via", "house", "--take-once"]),
                    ))
                })
            })
            .collect();
        runs.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|v| v["allowed"] == json!(true))
            .count()
    });
    assert_eq!(wins, 1);
}

#[test]
fn python_and_rust_racing_for_one_grant_exactly_one_wins() {
    let kith_auth = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
    if Command::new("python3").arg("-V").output().is_err() || !kith_auth.is_file() {
        return;
    }
    let t = Tmp::new("py-race");
    let home = t.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let python = |t: &Tmp| {
        Command::new("python3")
            .arg(&kith_auth)
            .args(["resolve", "video", "--via", "house", "--take-once"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("KITH_HOME", t.0.join("kith"))
            .env("KITH_AUTH", &c.auth)
            .env("KITH_PERSON", "sam")
            .env("KITH_SERVE_DIR", t.0.join("serve"))
            .env("KITH_SERVE_PORT", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let procs: Vec<_> = (0..4).map(|_| python(&t)).collect();
    let rust: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    json_of(&run(
                        &c,
                        "resolve",
                        &args(&["video", "--via", "house", "--take-once"]),
                    ))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut wins = rust.iter().filter(|v| v["allowed"] == json!(true)).count();
    for p in procs {
        let o = p.wait_with_output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| panic!("python: {}", String::from_utf8_lossy(&o.stderr)));
        wins += usize::from(v["allowed"] == json!(true));
    }
    assert_eq!(wins, 1);
}

#[test]
fn json_errors_match_python() {
    for (text, want) in [
        ("", "Expecting value: line 1 column 1 (char 0)"),
        ("  ", "Expecting value: line 1 column 3 (char 2)"),
        (
            "{\"a\"",
            "Expecting ':' delimiter: line 1 column 5 (char 4)",
        ),
        ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
        (
            "{\"a\":1",
            "Expecting ',' delimiter: line 1 column 7 (char 6)",
        ),
        ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
        ("nul", "Expecting value: line 1 column 1 (char 0)"),
        (
            "\"a\nb\"",
            "Invalid control character at: line 1 column 3 (char 2)",
        ),
        (
            "\"\\u12\"",
            "Invalid \\uXXXX escape: line 1 column 3 (char 2)",
        ),
        ("01", "Extra data: line 1 column 2 (char 1)"),
        ("-", "Expecting value: line 1 column 1 (char 0)"),
        ("{\"a\":tru}", "Expecting value: line 1 column 6 (char 5)"),
        (
            "{\"a\":\"\\",
            "Unterminated string starting at: line 1 column 6 (char 5)",
        ),
        ("é", "Expecting value: line 1 column 1 (char 0)"),
        ("{\"a\":1} {", "Extra data: line 1 column 9 (char 8)"),
    ] {
        assert_eq!(pyjson::error_of(text).as_deref(), Some(want), "{text:?}");
    }
    assert_eq!(
        pyjson::error_of("{\"a\": [1, 2.5e3, null, true, \"x\"]}"),
        None
    );
    assert_eq!(pyjson::error_of("NaN"), None, "Python reads it");
}

#[test]
fn dumps_prints_like_python() {
    let v = json!({"a": [1, 2.5, "é\n\u{7f}"], "b": {}, "c": [], "d": null, "e": 1e16, "f": 1e-5, "g": true});
    assert_eq!(
        pyjson::dumps(&v),
        "{\"a\": [1, 2.5, \"\\u00e9\\n\\u007f\"], \"b\": {}, \"c\": [], \"d\": null, \"e\": 1e+16, \"f\": 1e-05, \"g\": true}"
    );
    assert_eq!(
        pyjson::dumps_indent(&json!({"a": [], "b": {"c": 1}})),
        "{\n  \"a\": [],\n  \"b\": {\n    \"c\": 1\n  }\n}"
    );
}

#[test]
fn a_lone_dash_is_a_person_not_a_flag() {
    let t = Tmp::new("dash");
    let c = ctx(&t);
    write_auth(&c, &people());
    // `-` is the "no one" kith-auth prints for the owner in a guest's sandbox; it stays a name, not usage (exit 2)
    let v = json_of(&run(&c, "resolve", &args(&["house", "--person", "-"])));
    assert_eq!(v["person"], "-");
    assert_eq!(v["allowed"], json!(false));
    assert_ne!(run(&c, "may", &args(&["-", "house"])).code, 2);
    // any other dash word is still usage, and auth.json stays as written
    let before = std::fs::read(&c.auth).unwrap();
    assert_eq!(run(&c, "resolve", &args(&["house", "--bogus"])).code, 2);
    assert_eq!(std::fs::read(&c.auth).unwrap(), before);
}

#[test]
fn the_runners_own_user_has_a_home_from_the_password_database_and_is_no_guest() {
    // getpwuid_r, as Python's pwd: the user running the test has an entry (the runner's, on Linux CI too), and no
    // program from PATH or $HOME is consulted. A PATH that holds a fake `id` and `getent` changes nothing.
    let home = os_home().expect("the test user has a passwd entry");
    assert!(home.is_absolute(), "{home:?}");
    assert!(std::fs::read_dir(&home).is_ok(), "{home:?} is readable");
    let uid = test_uid();
    if let Some(line) = std::fs::read_to_string("/etc/passwd").ok().and_then(|t| {
        t.lines()
            .find(|l| l.split(':').nth(2) == Some(uid.as_str()))
            .map(str::to_owned)
    }) {
        // Where /etc/passwd is the database (Linux CI), the two agree.
        assert_eq!(line.split(':').nth(5).map(PathBuf::from), Some(home));
    }
}

/// The numeric uid of this process, from `id -u` (a test-only use of a program; the code under test runs none).
fn test_uid() -> String {
    let out = Command::new("/usr/bin/id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn owner_by_the_os_probe_is_not_a_guest() {
    // The real probe on this machine: the test user's home is readable, so it is no guest sandbox.
    let t = Tmp::new("os-probe");
    let mut c = ctx(&t);
    c.sandbox = Sandbox::Os;
    assert!(os_home().is_some(), "the passwd home must be found");
    assert!(!c.in_guest_sandbox());
}

#[test]
fn help_is_kith_auth_own_usage_on_stdout() {
    let t = Tmp::new("help");
    let c = ctx(&t);
    // no auth.json: the help answer reads nothing
    for flag in ["-h", "--help"] {
        let o = run(&c, flag, &args(&[]));
        assert_eq!(o.code, 0, "{flag}");
        assert_eq!(o.stderr, "", "{flag}");
        assert!(
            o.stdout
                .starts_with("kith-auth — which agent may use which shared service"),
            "{flag}"
        );
    }
    assert!(!t.0.join("secrets").exists(), "nothing was created or read");
}

// What core/runtime/src/cutover/tombstone.rs `note()` writes.
const NOTE: &str = r#"{"kith_tombstone":1,"module":"authority","message":"this data moved to the Kith core (module authority); the old reader must not use this file","rollback":"kith rollback authority"}"#;
const NOTE_NO_ROLLBACK: &str = r#"{"kith_tombstone":1,"module":"authority"}"#;

#[test]
fn a_tombstone_is_refused_by_every_read_and_never_written_over() {
    let t = Tmp::new("tombstone");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, NOTE).unwrap();
    let want = Die {
        msg: format!(
            "{} moved to the Kith core (module authority); this reader must not use it (kith rollback authority)",
            c.auth.display()
        ),
        code: 3,
    };
    assert_eq!(load_auth(&c).unwrap_err(), want);
    let out = run(&c, "may", &args(&["kai", "house"]));
    assert_eq!(out.code, 3);
    assert_eq!(out.stdout, "");
    // the note appears after a locked load: the save under the lock still refuses and leaves the bytes
    std::fs::write(&c.auth, "{}").unwrap();
    let locked = load_auth_locked(&c).unwrap();
    std::fs::write(&c.auth, NOTE).unwrap();
    assert_eq!(save_auth(&c, &locked).unwrap_err(), want);
    assert_eq!(std::fs::read_to_string(&c.auth).unwrap(), NOTE);
    let left: Vec<_> = std::fs::read_dir(c.auth.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.rsplit('.')
                .next()
                .is_some_and(|e| e.eq_ignore_ascii_case("tmp"))
        })
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_tombstone_without_a_rollback_field_names_the_default() {
    let t = Tmp::new("tombstone-bare");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, NOTE_NO_ROLLBACK).unwrap();
    assert_eq!(
        load_auth(&c).unwrap_err(),
        Die {
            msg: format!(
                "{} moved to the Kith core (module authority); this reader must not use it (kith rollback authority)",
                c.auth.display()
            ),
            code: 3,
        }
    );
}
