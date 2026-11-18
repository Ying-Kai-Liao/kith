#![allow(clippy::unwrap_used)]
//! `status MODULE | --json | --require FILE` (retire-python Step 0). The plain `status` is covered in `tests.rs`.

use std::path::PathBuf;

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

use super::status::Outcome;
use super::*;
use crate::tests::machine;

/// A module whose only file is `<name>-src.json`; the tests set its switch file and tombstone by hand.
struct Stub(&'static str);

impl Module for Stub {
    fn name(&self) -> &'static str {
        self.0
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        vec![env.machine.join(format!("{}-src.json", self.0))]
    }
    fn import(&self, _: &Env, _: Option<&mut Core>, _: Opts) -> Result<Vec<String>> {
        unreachable!("status never imports")
    }
    fn same_answer(&self, _: &Env, _: &Connection) -> Result<Vec<String>> {
        unreachable!("status never checks")
    }
    fn export_back(&self, _: &Env, _: &mut Core) -> Result<Vec<String>> {
        unreachable!("status never exports")
    }
}

fn setup(names: &[&'static str]) -> (TempDir, Env, Vec<Box<dyn Module>>) {
    let (t, m) = machine();
    std::fs::create_dir_all(&m).expect("machine");
    let reg = names
        .iter()
        .map(|n| Box::new(Stub(n)) as Box<dyn Module>)
        .collect();
    (t, Env::new(&m), reg)
}

fn record(env: &Env, name: &str, phase: Phase, at: i64) {
    let s = Switch {
        phase,
        snapshot: None,
        at,
    };
    state::write(&env.layout, name, &s).expect("write");
}

fn tombstone_it(env: &Env, name: &str) {
    tombstone::ensure(&env.machine.join(format!("{name}-src.json")), name, 5).expect("tombstone");
}

/// A module switched and tombstoned.
fn switched(env: &Env, name: &str, at: i64) {
    tombstone_it(env, name);
    record(env, name, Phase::Switched, at);
}

fn a(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}

fn go(reg: &[Box<dyn Module>], env: &Env, args: &[&str]) -> Outcome {
    let mut v = vec!["status"];
    v.extend_from_slice(args);
    run_outcome_with(reg, env, &a(&v)).expect("outcome")
}

fn json(o: &Outcome) -> Value {
    assert_eq!(o.out.len(), 1, "{o:?}");
    serde_json::from_str(&o.out[0]).expect("json")
}

#[test]
fn status_module_exits_zero_only_when_switched() {
    let (_t, env, reg) = setup(&["fa", "fb"]);
    let o = go(&reg, &env, &["fa"]);
    assert_eq!(o.code, 1);
    assert_eq!(o.err, vec!["fa: not switched; run: kith migrate step fa"]);
    assert!(o.out.is_empty());
    switched(&env, "fa", 1_700_000_000);
    let o = go(&reg, &env, &["fa"]);
    assert_eq!(o.code, 0);
    assert_eq!(o.out, vec!["fa: switched (since 1700000000)"]);
    assert!(o.err.is_empty());
    // The other module is still unswitched.
    assert_eq!(go(&reg, &env, &["fb"]).code, 1);
}

#[test]
fn stepping_and_rolling_back_are_not_switched_and_name_their_own_step() {
    let (_t, env, reg) = setup(&["fa", "fb"]);
    record(&env, "fa", Phase::Stepping, 7);
    record(&env, "fb", Phase::RollingBack, 8);
    let o = go(&reg, &env, &["fa"]);
    assert_eq!(o.code, 1);
    assert_eq!(
        o.err,
        vec!["fa: stepping (since 7); run: kith migrate step fa"]
    );
    let o = go(&reg, &env, &["fb"]);
    assert_eq!(o.code, 1);
    assert_eq!(
        o.err,
        vec!["fb: rolling-back (since 8); run: kith rollback fb"]
    );
}

#[test]
fn a_switched_record_with_a_live_old_file_is_unfinished() {
    let (_t, env, reg) = setup(&["fa"]);
    // The crash between the record and the tombstone: the old reader's file is still the real one.
    std::fs::write(env.machine.join("fa-src.json"), "{}").unwrap();
    record(&env, "fa", Phase::Switched, 9);
    let o = go(&reg, &env, &["fa"]);
    assert_eq!(o.code, 1);
    assert!(
        o.err[0].starts_with("fa: switched but not finished ("),
        "{o:?}"
    );
    assert!(o.err[0].ends_with("run: kith migrate step fa"), "{o:?}");
    let j = json(&go(&reg, &env, &["fa", "--json"]));
    assert_eq!(j["state"], "unfinished");
    assert_eq!(j["ok"], false);
}

#[test]
fn a_damaged_record_is_not_ok_and_does_not_stop_the_listing() {
    let (_t, env, reg) = setup(&["fa", "fb"]);
    switched(&env, "fb", 3);
    let p = state::path(&env.layout, "fa").unwrap();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "garbage\n").unwrap();
    let o = go(&reg, &env, &["fa"]);
    assert_eq!(o.code, 1);
    assert!(o.err[0].starts_with("fa: switch record damaged ("), "{o:?}");
    assert!(o.err[0].contains("fa.switch"), "{o:?}");
    let j = json(&go(&reg, &env, &["--json"]));
    assert_eq!(j["modules"]["fa"]["state"], "damaged");
    assert_eq!(j["modules"]["fb"]["state"], "switched");
}

#[test]
fn an_unknown_module_is_an_error() {
    let (_t, env, reg) = setup(&["fa"]);
    let e = run_outcome_with(&reg, &env, &a(&["status", "nope"])).unwrap_err();
    assert!(e.to_string().contains("no module"), "{e}");
}

#[test]
fn json_lists_every_module_in_registry_order() {
    let (_t, env, reg) = setup(&["fb", "fa", "fc"]);
    switched(&env, "fa", 1_700_000_001);
    record(&env, "fc", Phase::Stepping, 0);
    let o = go(&reg, &env, &["--json"]);
    assert_eq!(o.code, 0);
    let j = json(&o);
    let names: Vec<&str> = j["modules"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(names, ["fb", "fa", "fc"]);
    assert_eq!(j["modules"]["fb"]["state"], "not switched");
    assert!(j["modules"]["fb"].get("since").is_none());
    assert_eq!(j["modules"]["fa"]["state"], "switched");
    assert_eq!(j["modules"]["fa"]["since"], 1_700_000_001);
    assert_eq!(j["modules"]["fa"]["ok"], true);
    // A time that did not parse (0) is left out rather than shown as 1970.
    assert_eq!(j["modules"]["fc"]["state"], "stepping");
    assert!(j["modules"]["fc"].get("since").is_none());
}

#[test]
fn json_for_one_module_goes_to_stdout_whatever_the_exit_code() {
    let (_t, env, reg) = setup(&["fa"]);
    let o = go(&reg, &env, &["fa", "--json"]);
    assert_eq!(o.code, 1);
    assert!(o.err.is_empty());
    let j = json(&o);
    assert_eq!(
        (j["module"].as_str(), j["state"].as_str()),
        (Some("fa"), Some("not switched"))
    );
    switched(&env, "fa", 4);
    let o = go(&reg, &env, &["--json", "fa"]);
    assert_eq!(o.code, 0);
    assert_eq!(json(&o)["ok"], true);
}

fn require_file(t: &TempDir, text: &str) -> String {
    let p = t.path().join("requires.txt");
    std::fs::write(&p, text).unwrap();
    p.display().to_string()
}

#[test]
fn require_passes_for_an_empty_list_and_for_comments_only() {
    let (t, env, reg) = setup(&["fa"]);
    let f = require_file(&t, "# the header\n\n   \n# fa is not listed yet\n");
    let o = go(&reg, &env, &["--require", &f]);
    assert_eq!(o.code, 0);
    assert_eq!(o.out, vec!["all 0 required modules switched"]);
}

#[test]
fn require_reads_names_with_comments_and_checks_all() {
    let (t, env, reg) = setup(&["fa", "fb"]);
    let f = require_file(&t, "# gate\nfa  # first\n\nfb\nfa\n");
    switched(&env, "fa", 1);
    let o = go(&reg, &env, &["--require", &f]);
    assert_eq!(o.code, 1);
    assert_eq!(o.err, vec!["fb: not switched; run: kith migrate step fb"]);
    switched(&env, "fb", 2);
    let o = go(&reg, &env, &["--require", &f]);
    assert_eq!(o.code, 0);
    assert_eq!(o.out, vec!["all 2 required modules switched"]);
}

#[test]
fn require_names_every_missing_module_with_its_own_step() {
    let (t, env, reg) = setup(&["fa", "fb", "fc"]);
    let f = require_file(&t, "fa\nfb\nfc\nnewer\n");
    record(&env, "fb", Phase::RollingBack, 5);
    let o = go(&reg, &env, &["--require", &f]);
    assert_eq!(o.code, 1);
    assert_eq!(
        o.err,
        vec![
            "fa: not switched; run: kith migrate step fa",
            "fb: rolling-back (since 5); run: kith rollback fb",
            "fc: not switched; run: kith migrate step fc",
            "newer: unknown module (this kith program lacks it; run: kith update)",
        ]
    );
}

#[test]
fn require_json_reports_ok_and_each_module_on_stdout() {
    let (t, env, reg) = setup(&["fa", "fb"]);
    let f = require_file(&t, "fa\nfb\n");
    switched(&env, "fa", 1);
    let o = go(&reg, &env, &["--require", &f, "--json"]);
    assert_eq!(o.code, 1);
    assert!(o.err.is_empty());
    let j = json(&o);
    assert_eq!(j["ok"], false);
    assert_eq!(j["required"][0]["ok"], true);
    assert_eq!(j["required"][1]["state"], "not switched");
}

#[test]
fn require_with_a_missing_file_fails() {
    let (t, env, reg) = setup(&["fa"]);
    let gone = t.path().join("nope.txt").display().to_string();
    let e = run_outcome_with(&reg, &env, &a(&["status", "--require", &gone])).unwrap_err();
    assert!(e.to_string().contains("nope.txt"), "{e}");
}

#[test]
fn bad_status_arguments_are_usage_errors() {
    let (t, env, reg) = setup(&["fa"]);
    let f = require_file(&t, "fa\n");
    for args in [
        vec!["status", "--require"],
        vec!["status", "--require", "--json"],
        vec!["status", "--require", &f, "--require", &f],
        vec!["status", "fa", "--require", &f],
        vec!["status", "fa", "fa"],
        vec!["status", "--all"],
    ] {
        let e = run_outcome_with(&reg, &env, &a(&args)).unwrap_err();
        assert!(e.to_string().contains("usage"), "{args:?}: {e}");
    }
}

#[test]
fn plain_status_is_as_it_was() {
    let (_t, env, reg) = setup(&["fa", "fb"]);
    switched(&env, "fa", 11);
    let o = run_outcome_with(&reg, &env, &a(&["status"])).unwrap();
    assert_eq!(o.code, 0);
    assert_eq!(o.out, vec!["fa: switched (since 11)", "fb: not switched"]);
    assert_eq!(run_with(&reg, &env, &a(&["status"])).unwrap(), o.out);
}

#[test]
fn the_real_registry_answers_the_new_variants() {
    let (_t, env, _) = setup(&[]);
    let reg = registry();
    let o = go(&reg, &env, &["accounts"]);
    assert_eq!(o.code, 1);
    assert_eq!(
        o.err,
        vec!["accounts: not switched; run: kith migrate step accounts"]
    );
    let j = json(&go(&reg, &env, &["--json"]));
    assert_eq!(j["modules"].as_object().unwrap().len(), reg.len());
}
