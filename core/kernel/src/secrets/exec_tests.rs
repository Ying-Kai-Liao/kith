#![allow(clippy::unwrap_used)]

use serde_json::json;

use super::authority::test_client;
use super::exec::{prepare, run};
use super::kit::{Fake, Home, NOTE, path_str, svca, tables};
use super::session::Env;

fn home(name: &str) -> (Home, Env) {
    let h = Home::new(name);
    h.agent("echo", &["svca"]);
    h.service("svca", &svca());
    h.tables(&tables());
    let mut env = h.env();
    env.insert("SVC_TOKEN".to_owned(), "tok".to_owned());
    (h, env)
}

fn cmd() -> Vec<String> {
    vec!["env".to_owned()]
}

#[test]
fn the_server_gets_its_settings_and_secrets_and_one_line_goes_to_the_log() {
    let (h, mut env) = home("exec");
    env.insert("KITH_AGENT".to_owned(), "other".to_owned());
    env.insert("KITH_PERSON".to_owned(), String::new());
    let s = prepare(&h.ctx(), env, "echo", "svca", &cmd()).unwrap();
    assert_eq!(s.cmd, ["env"]);
    // what the run had, then the service's settings (expanded) and the secret; the owner's run is not scrubbed
    assert_eq!(s.env["TOKEN"], "tok");
    assert_eq!(s.env["WHO"], "agent-echo");
    assert_eq!(s.env["SVC_TOKEN"], "tok");
    assert_eq!(
        s.env["KITH_AGENT"], "other",
        "the child inherits the run's own environment, not the edits of settings()"
    );
    assert_eq!(s.cwd, None);
    // the line: time, how, service, profile, for, run (the agent settings() named), the parent, the person
    let fields: Vec<&str> = s.audit.trim_end_matches('\n').split('\t').collect();
    assert_eq!(fields.len(), 8, "{:?}", s.audit);
    assert_eq!(&fields[1..5], ["exec", "svca", "p1", "for=echo"]);
    assert_eq!(fields[5], "run=echo");
    assert!(fields[6].starts_with("ppid="), "{}", fields[6]);
    assert_eq!(fields[7], "person=owner");
    assert_eq!(fields[0].len(), 19, "{}", fields[0]);
    assert!(s.audit.ends_with('\n'));
}

#[test]
fn a_refusal_is_exit_1_with_the_script_s_words() {
    let (h, env) = home("refuse");
    let ask =
        |env: Env, agent: &str, svc: &str| prepare(&h.ctx(), env, agent, svc, &cmd()).unwrap_err();
    let d = ask(env.clone(), "", "svca");
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            1,
            "who is asking for svca? (KITH_AGENT is not set; tools pass --for <agent>)"
        )
    );
    let d = ask(env.clone(), "echo", "svcz");
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            1,
            "echo has no grant for svcz (kith-auth grant echo svcz <profile>)"
        )
    );
    let mut t = tables();
    t["grants"]["echo"]["svcz"] = json!("p1");
    h.tables(&t);
    let d = ask(env.clone(), "echo", "svcz");
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            1,
            "no service 'svcz' (kernel/services/, or an agent's services/)"
        )
    );
    let mut bare = env.clone();
    bare.remove("SVC_TOKEN");
    let d = ask(bare, "echo", "svca");
    assert_eq!(
        (d.code, d.msg.as_str()),
        (
            1,
            "p1: TOKEN does not resolve (env:SVC_TOKEN; kith-auth set p1 TOKEN)"
        )
    );
    let mut t = tables();
    t["profiles"] = json!({});
    h.tables(&t);
    let d = ask(env, "echo", "svca");
    assert_eq!(d.msg, format!("no profile 'p1' in {}", path_str(&h.auth())));
}

#[test]
fn a_guest_s_server_loses_what_leads_into_their_folders() {
    let (h, mut env) = home("scrub");
    let files = h.agents().join("echo/files/people/sam");
    std::fs::create_dir_all(&files).unwrap();
    env.insert("KITH_PERSON".to_owned(), "sam".to_owned());
    env.insert("LEAD".to_owned(), path_str(&files.join("bin")));
    env.insert(
        "PATHISH".to_owned(),
        format!("/usr/bin:{}", path_str(&files.join("x"))),
    );
    env.insert("GRANTED".to_owned(), "/granted/dir/tool".to_owned());
    env.insert("FINE".to_owned(), "/usr/bin".to_owned());
    // a profile of their own, and a row that was granted a folder to write
    let mut t = tables();
    t["profiles"]["p2"] = json!({"TOKEN": "env:SVC_TOKEN", "person": "sam"});
    t["grants"]["echo@sam"] = json!({"svca": "p2"});
    t["members"]["echo"]["sam"] = json!({"access": {"write": ["/granted/dir"]}});
    h.tables(&t);
    let mut c = h.ctx();
    c.person = Some("sam".to_owned());
    let s = prepare(&c, env, "echo", "svca", &cmd()).unwrap();
    assert!(!s.env.contains_key("LEAD"));
    assert!(
        !s.env.contains_key("PATHISH"),
        "one entry of a list is enough"
    );
    assert!(
        !s.env.contains_key("GRANTED"),
        "a folder granted for writing counts"
    );
    assert_eq!(s.env["FINE"], "/usr/bin");
    assert_eq!(s.env["TOKEN"], "tok");
    assert!(s.env.contains_key("PWD"));
    assert!(s.audit.contains("\tperson=sam\n"), "{}", s.audit);
}

#[test]
fn on_the_tombstone_the_tables_are_the_core_s() {
    let (h, env) = home("tombstone");
    h.write("home/.kith/secrets/auth.json", NOTE);
    let fake = Fake::answering(json!({"auth": tables()}));
    test_client(fake.clone(), h.auth());
    let s = prepare(&h.ctx(), env.clone(), "echo", "svca", &cmd()).unwrap();
    assert_eq!(s.env["TOKEN"], "tok");
    assert_eq!(fake.asked().len(), 1);
    // no core: refused before any secret is read
    test_client(Fake::failing(false, "no core daemon answers"), h.auth());
    let d = prepare(&h.ctx(), env, "echo", "svca", &cmd()).unwrap_err();
    assert_eq!(d.code, 3);
}

fn words(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_command_line_is_the_scripts() {
    let (h, _) = home("usage");
    let c = h.ctx();
    for (a, code, msg) in [
        (
            words(&["svca"]),
            2,
            "exec [--for AGENT] <service> -- CMD...",
        ),
        (
            words(&["svca", "env"]),
            2,
            "exec [--for AGENT] <service> -- CMD...",
        ),
        (
            words(&["svca", "--"]),
            2,
            "exec [--for AGENT] <service> -- CMD...",
        ),
        (
            words(&["--for"]),
            2,
            "exec [--for AGENT] <service> -- CMD...",
        ),
        (
            words(&["--for", "echo", "--secret", "rs_1", "--", "env"]),
            2,
            "exec --secret is not in this program: use kith-auth exec --secret",
        ),
    ] {
        let d = run(&c, &a).map(|_| ()).unwrap_err();
        assert_eq!((d.code, d.msg.as_str()), (code, msg), "{a:?}");
    }
}
