//! `kith kernel wake` against a real core daemon: the params the verb builds go through the daemon's dispatch, which checks
//! them against the table's rows (a stub `core-call` cannot see that check, lesson #1072).

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use kith_kernel::paths::Paths;
use kith_kernel::wake::{Fail, run};
use serde_json::Value;

use crate::core_front::{Answer, call_owner, serve_for_test};

/// A short folder: a Unix socket path holds about a hundred bytes.
fn folders(tag: &str) -> Paths {
    let home = PathBuf::from("/tmp").join(format!("kwc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("kith")).unwrap();
    let home = home.canonicalize().unwrap();
    let kith_home = home.join("kith");
    let lookup = move |k: &str| (k == "KITH_HOME").then(|| kith_home.to_string_lossy().into_owned());
    kith_kernel::paths::paths(&home, &home.join("kith/kernel"), &lookup)
}

/// The verb over `folders`' daemon (what `wake_cmd::call` does in a real run, but at these folders and not the process's).
fn kith(f: &Paths, line: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let call = |method: &str, params: &Value| match call_owner(f, method, params, false) {
        Answer::Done(v) => Ok(v),
        Answer::Down { why, .. } => Err(Fail::NoAnswer(why)),
        Answer::Refused(why) => Err(Fail::Refused(why)),
    };
    let get = |k: &str| env.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned());
    let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&args, &get, &call, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn every_verb_goes_through_the_real_dispatch_and_a_missing_core_refuses_and_writes_nothing() {
    let f = folders("verbs");
    let daemon = serve_for_test(&f);
    let said = |line: &str| {
        let (code, out, err) = kith(&f, line, &[]);
        assert_eq!(code, 0, "{line}: {err}");
        out
    };
    assert_eq!(said("enqueue s1 --id card:1 --agent designer --why answered -- yes, go"), "queued\n");
    assert_eq!(said("enqueue s1 --id card:1 --agent designer -- yes, go"), "duplicate\n");
    assert_eq!(said("enqueue s1 --id ci:2 --agent designer -- green"), "merged\n");
    assert_eq!(said("status card:1"), "queued\n");
    assert_eq!(said("status card:1 --json"), "{\"id\": \"card:1\", \"state\": \"queued\", \"run\": null}\n");
    assert!(said("ls").contains("s1  designer  owner  run  owed  2 request(s)"));
    assert!(said("ls --json").contains("\"session\": \"s1\""));
    assert_eq!(said("withdraw s1 --id ci:2"), "withdrew 1\n");
    assert_eq!(said("withdraw s1"), "withdrew 1\n");
    assert_eq!(said("status card:1"), "withdrawn\n");
    assert!(said("wait --on at:+30m --session s2 --agent designer -- ping").starts_with("waiting "));
    assert_eq!(said("withdraw-person nobody"), "withdrew 0\n");
    assert_eq!(said("drain"), "");

    // The core's own refusals are said as kith-wake says them, and exit 1.
    let (code, _, err) = kith(&f, "enqueue s9 --id x:1 --agent a --person anna -- x", &[]);
    assert_eq!(code, 1);
    assert!(err.starts_with("kith-wake: "), "{err}");

    // R8: the terminal exit's call.
    let released = call_owner(&f, "wake.released", &serde_json::json!({"session": "s1"}), false);
    assert!(
        matches!(released, Answer::Done(ref v) if v["unparked"].as_array().is_some_and(Vec::is_empty)),
        "wake.released refused or unanswered"
    );

    daemon.stop();
    // The tombstone with no core answering: refused, nothing written.
    let (code, out, err) = kith(&f, "enqueue s1 --id late:1 --agent designer -- x", &[]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("did not answer") && err.contains("may not have been made"), "{err}");
    assert!(!f.state.join("sessions").join("queue").exists(), "no queue file or folder was made");
    let _ = std::fs::remove_dir_all(&f.home);
}
