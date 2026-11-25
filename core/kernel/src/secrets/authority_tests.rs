#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};

use super::authority::{Authority, test_client, test_no_client};
use super::kit::{Fake, Home, NOTE, tables};
use super::{Sandbox, run};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// A home whose auth.json is the tombstone, with `fake` as its core.
fn tombstoned(name: &str, fake: &std::sync::Arc<Fake>) -> Home {
    let h = Home::new(name);
    h.write("home/.kith/secrets/auth.json", NOTE);
    test_client(fake.clone(), h.auth());
    h
}

#[test]
fn may_on_a_tombstone_is_one_call_of_the_core_and_never_reads_the_file() {
    let fake = Fake::answering(json!({"allowed": true, "why": "member"}));
    let h = tombstoned("may", &fake);
    let out = run(
        &h.ctx(),
        "may",
        &args(&["sam", "echo", "--via", "kai", "--once"]),
    );
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "member\n", "")
    );
    let asked = fake.asked();
    assert_eq!(asked.len(), 1);
    let (method, p) = &asked[0];
    assert_eq!(method, "people.may");
    assert_eq!(
        (p["person"].as_str(), p["agent"].as_str(), p["via"].as_str()),
        (Some("sam"), Some("echo"), Some("kai"))
    );
    assert_eq!(
        (&p["once"], &p["dies"], &p["viaSwitch"]),
        (&json!(true), &json!(true), &json!(true))
    );
    assert_eq!(p["kernel"], json!(h.kernel().to_string_lossy()));
    assert!(p["env"].is_object());
    // not allowed: exit 1 with the core's why
    let fake = Fake::answering(json!({"allowed": false, "why": "sam may not use echo"}));
    test_client(fake.clone(), h.auth());
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (1, "sam may not use echo\n")
    );
    assert!(fake.asked()[0].1.get("via").is_none());
    assert_eq!(
        std::fs::read_to_string(h.auth()).unwrap(),
        NOTE,
        "the note is left as it is"
    );
}

#[test]
fn may_dies_with_the_core_s_line_when_the_rules_have_no_answer() {
    let fake = Fake::answering(json!({"died": 1, "why": "preset 'x' is gone"}));
    let h = tombstoned("may-died", &fake);
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", "kith-auth: preset 'x' is gone\n")
    );
}

#[test]
fn resolve_on_a_tombstone_prints_the_core_s_answer_as_json() {
    let answer = json!({"allowed": true, "held": false, "person": "sam", "why": "member", "mode": "default"});
    let fake = Fake::answering(answer.clone());
    let h = tombstoned("resolve", &fake);
    let mut c = h.ctx();
    c.person = Some("sam".to_owned());
    let out = run(&c, "resolve", &args(&["echo"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(serde_json::from_str::<Value>(&out.stdout).unwrap(), answer);
    assert_eq!(
        out.stdout,
        "{\"allowed\": true, \"held\": false, \"person\": \"sam\", \"why\": \"member\", \"mode\": \"default\"}\n"
    );
    let (method, p) = &fake.asked()[0];
    assert_eq!(method, "people.resolve");
    assert_eq!(
        (
            p["agent"].as_str(),
            p["person"].as_str(),
            p["dies"].as_bool()
        ),
        (Some("echo"), Some("sam"), Some(true))
    );
    assert!(p.get("via").is_none() && p.get("takeOnce").is_none());
    // a helper launch counts the "just this once" with its caller only
    let out = run(
        &c,
        "resolve",
        &args(&["echo", "--person", "ola", "--via", "kai", "--take-once"]),
    );
    assert_eq!(out.code, 0);
    let p = &fake.asked()[1].1;
    assert_eq!(
        (
            p["person"].as_str(),
            p["via"].as_str(),
            p["takeOnce"].as_bool()
        ),
        (Some("ola"), Some("kai"), Some(true))
    );
    // --take-once without a caller sends no takeOnce
    run(&c, "resolve", &args(&["echo", "--take-once"]));
    assert!(fake.asked()[2].1.get("takeOnce").is_none());
}

#[test]
fn a_program_with_no_core_client_is_exit_3_and_never_the_file() {
    let h = Home::new("no-client");
    h.write("home/.kith/secrets/auth.json", NOTE);
    test_no_client(h.auth());
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            3,
            "",
            "kith-auth: authority moved to the core, and it gave no answer: no kith program to ask\n"
        )
    );
}

#[test]
fn a_core_that_gives_nothing_usable_is_exit_3() {
    let cases: Vec<(std::sync::Arc<Fake>, &str)> = vec![
        (
            Fake::failing(false, "no core daemon answers"),
            "no core daemon answers",
        ),
        (Fake::failing(true, "no such method"), "no such method"),
        (
            Fake::answering(json!({"noCore": true})),
            "there is no core database",
        ),
        (
            Fake::answering(json!([1])),
            "the answer is not a JSON object",
        ),
        (
            Fake::answering(json!({"allowed": "yes", "why": "x"})),
            "the answer has no usable allowed",
        ),
        (
            Fake::answering(json!({"allowed": true})),
            "the answer has no usable why",
        ),
        (
            Fake::answering(json!({"died": "one", "why": "x"})),
            "the answer has no usable died",
        ),
    ];
    for (fake, why) in cases {
        let h = tombstoned("unusable", &fake);
        let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (
                3,
                "",
                format!("kith-auth: authority moved to the core, and it gave no answer: {why}\n")
                    .as_str()
            ),
            "{why}"
        );
    }
    // resolve wants allowed, held and person
    for answer in [
        json!({"allowed": true, "held": false}),
        json!({"allowed": true, "person": "sam"}),
    ] {
        let h = tombstoned("resolve-unusable", &Fake::answering(answer));
        let out = run(&h.ctx(), "resolve", &args(&["echo"]));
        assert_eq!(out.code, 3, "{}", out.stderr);
    }
}

#[test]
fn a_guest_sandbox_never_reaches_the_core() {
    let fake = Fake::answering(json!({"allowed": true, "why": "member"}));
    let h = tombstoned("guest", &fake);
    let mut c = h.ctx();
    c.sandbox = Sandbox::Fixed(true);
    let out = run(&c, "may", &args(&["sam", "echo"]));
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (
            3,
            "kith-auth: authority moved to the core, and it gave no answer: a guest run does not reach the core\n"
        )
    );
    assert_eq!(fake.asked().len(), 0);
}

#[test]
fn another_home_s_tombstone_is_the_old_refusal_and_asks_no_one() {
    let fake = Fake::answering(json!({"allowed": true, "why": "member"}));
    let h = Home::new("elsewhere");
    h.write("home/.kith/secrets/auth.json", NOTE);
    test_client(fake.clone(), h.0.join("not-this-home/auth.json"));
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!(out.code, 3);
    assert!(
        out.stderr
            .contains("moved to the Kith core (module authority); this reader must not use it"),
        "{}",
        out.stderr
    );
    assert_eq!(fake.asked().len(), 0);
    // a tombstone of another module is the same
    h.write(
        "home/.kith/secrets/auth.json",
        r#"{"kith_tombstone":1,"module":"claims"}"#,
    );
    test_client(fake.clone(), h.auth());
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!(out.code, 3);
    assert!(out.stderr.contains("(module claims)"), "{}", out.stderr);
    assert_eq!(fake.asked().len(), 0);
}

#[test]
fn the_wrong_shape_of_a_verb_is_usage_before_the_core_is_asked() {
    let fake = Fake::answering(json!({"allowed": true, "why": "member"}));
    let h = tombstoned("shape", &fake);
    for (verb, a) in [("may", args(&["sam"])), ("resolve", args(&["a", "b"]))] {
        let out = run(&h.ctx(), verb, &a);
        assert_eq!(
            (out.code, out.stderr.as_str()),
            (
                2,
                format!("kith-auth: {verb}: see kith-auth --help\n").as_str()
            )
        );
    }
    assert_eq!(fake.asked().len(), 0);
}

#[test]
fn the_tables_on_a_tombstone_are_the_core_s_view_with_every_table_present() {
    let view = json!({"profiles": {"p1": {}}, "grants": {}, "people": {"kai": {"owner": true}}});
    let fake = Fake::answering(json!({"auth": view}));
    let h = tombstoned("view", &fake);
    let c = h.ctx();
    let authority = Authority::open(&c).unwrap();
    assert!(authority.is_core());
    let t = authority.tables().unwrap();
    assert_eq!(t["profiles"], json!({"p1": {}}));
    for table in [
        "env",
        "presets",
        "members",
        "invites",
        "retired",
        "once",
        "agent_access",
    ] {
        assert_eq!(t[table], json!({}), "{table}");
    }
    assert_eq!(fake.asked()[0].0, "people.secretsView");
    // no `auth` object in the answer: closed
    test_client(Fake::answering(json!({"auth": []})), h.auth());
    let d = Authority::open(&c).unwrap().tables().unwrap_err();
    assert_eq!(d.code, 3);
    assert!(
        d.msg.ends_with("the answer has no usable auth"),
        "{}",
        d.msg
    );
}

#[test]
fn files_mode_never_asks_the_core() {
    let fake = Fake::answering(json!({"allowed": false, "why": "from the core"}));
    let h = Home::new("files");
    h.tables(&tables());
    test_client(fake.clone(), h.auth());
    let out = run(&h.ctx(), "may", &args(&["sam", "echo"]));
    assert_eq!((out.code, out.stdout.as_str()), (0, "member\n"));
    assert_eq!(fake.asked().len(), 0);
}
