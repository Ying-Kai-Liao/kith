//! The must-ask judge against what `kernel/lib/kith_guard.py` answered (frozen by
//! `kernel/tests/difftest/golden-lib/gen-guard-must-ask.py`), and a few pieces of the Python it leans on.

use std::path::Path;

use serde_json::Value;

use super::guard::Places;
use super::judge::{must_ask, must_ask_rule, sentence_class};
use super::pyport::{fnmatch, redact, shlex_words};

const GOLDEN: &str =
    include_str!("../../../../kernel/tests/difftest/golden-lib/guard-must-ask.json");

fn want(v: &Value) -> Option<&str> {
    v.get("want").and_then(Value::as_str)
}

fn world() -> (Value, Places) {
    let g: Value = serde_json::from_str(GOLDEN).unwrap();
    let w = &g["world"];
    let f = |k: &str| w[k].as_str().unwrap().to_owned();
    let places = Places::new(
        &f("home"),
        &f("state"),
        &f("machine"),
        &f("agents"),
        &f("kith_home"),
        Path::new(&f("kernel")),
        &f("package"),
    )
    .with_secrets(&f("secrets"), None);
    (g, places)
}

#[test]
fn must_ask_agrees_with_the_python_on_every_frozen_call() {
    let (g, places) = world();
    let mut bad = vec![];
    for c in g["calls"].as_array().unwrap() {
        let roots: Vec<String> = c["roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
        let got = must_ask(c["tool"].as_str().unwrap(), &c["input"], &roots, &places);
        if got != want(c) {
            bad.push(format!(
                "{} {} roots {roots:?}: got {got:?}, want {:?}",
                c["tool"],
                c["input"],
                want(c)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} differ, first: {:#?}",
        bad.len(),
        &bad[..bad.len().min(12)]
    );
}

#[test]
fn must_ask_rule_agrees_with_the_python_on_every_frozen_rule() {
    let (g, places) = world();
    let fill = |t: &str| {
        let mut out = t.to_owned();
        for (k, v) in g["fill"].as_object().unwrap() {
            out = out.replace(&format!("{{{{{k}}}}}"), v.as_str().unwrap());
        }
        out
    };
    let mut bad = vec![];
    for r in g["rules"].as_array().unwrap() {
        let rule = r["rule"].as_str().unwrap();
        let got = must_ask_rule(rule, &fill, &places);
        if got != want(r) {
            bad.push(format!("{rule:?}: got {got:?}, want {:?}", want(r)));
        }
    }
    assert!(bad.is_empty(), "{} differ: {bad:#?}", bad.len());
}

#[test]
fn sentence_class_agrees_with_the_python_on_every_frozen_sentence() {
    let (g, _) = world();
    for s in g["sentences"].as_array().unwrap() {
        let text = s["text"].as_str().unwrap();
        assert_eq!(sentence_class(text), want(s), "{text:?}");
    }
}

#[allow(clippy::unnecessary_wraps)]
fn words(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|s| (*s).to_owned()).collect())
}

#[test]
fn shlex_splits_like_python_posix_with_punctuation() {
    assert_eq!(
        shlex_words("a b;c&&d"),
        words(&["a", "b", ";", "c", "&&", "d"])
    );
    assert_eq!(
        shlex_words("echo 'a b' \"c d\" e\\ f"),
        words(&["echo", "a b", "c d", "e f"])
    );
    assert_eq!(
        shlex_words("echo a # c d\necho"),
        words(&["echo", "a", "echo"])
    );
    assert_eq!(
        shlex_words("x >out 2>&1"),
        words(&["x", ">", "out", "2", ">&", "1"])
    );
    assert_eq!(shlex_words("echo ''"), words(&["echo", ""]));
    assert_eq!(shlex_words("echo 'open"), None);
    assert_eq!(shlex_words("echo end\\"), None);
}

#[test]
fn fnmatch_stars_cross_slashes_and_classes_work() {
    assert!(fnmatch("/a/b/c.json", "/a/*/c.json"));
    assert!(fnmatch("/a/b/x/c.json", "/a/*/c.json"));
    assert!(fnmatch("/a/b", "/a/?"));
    assert!(fnmatch("/a/b", "/a/[a-c]"));
    assert!(!fnmatch("/a/d", "/a/[!d]"));
    assert!(fnmatch("/a/[", "/a/["));
}

#[test]
fn redact_changes_what_the_python_changes() {
    assert_eq!(redact("card 4111 1111 1111 1111 ok"), "card [redacted] ok");
    assert_eq!(
        redact("card 4111 1111 1111 1112 ok"),
        "card 4111 1111 1111 1112 ok"
    );
    // the value stops at the `]`, so the Python's output grows one
    assert_eq!(redact("token=[redacted]"), "token=[redacted]]");
    assert_eq!(redact("ssn 123-45-6789"), "ssn [redacted]");
    assert_eq!(
        redact("id A123456789 xA123456789"),
        "id [redacted] xA123456789"
    );
    assert_eq!(redact("https://bob:pw@h/x"), "https://bob:[redacted]@h/x");
}
