//! Unit tests with a stand-in for kith-auth and the host. The Python's answers are the difftest golden `people.json`.

use super::{Backend, Out, run};
use serde_json::{Value, json};
use std::cell::RefCell;

struct Fake {
    people: Value,
    host: Result<Value, String>,
    calls: RefCell<Vec<(String, Value)>>,
}

impl Fake {
    fn new(people: Value, host: Result<Value, String>) -> Fake {
        Fake {
            people,
            host,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Backend for Fake {
    fn people(&self) -> Result<Value, String> {
        Ok(self.people.clone())
    }

    fn host_call(&self, method: &str, body: &Value) -> Result<Value, String> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), body.clone()));
        self.host.clone()
    }
}

fn line(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

fn table() -> Value {
    json!([
        {"id": "kai", "name": "Kai", "owner": true, "agents": []},
        {"id": "sam", "name": "Sam", "agents": [
            {"agent": "house", "role": "member", "via": "own"},
            {"agent": "garden", "role": "guest", "via": "everyone"}]},
        {"id": "ola", "revoked": true, "revoked_at": "2026-11-01", "agents": []},
        {"id": "bot-s", "name": "Scout", "bot_of": "sam", "agents": [{"agent": "house", "role": "guest", "via": "own"}]},
    ])
}

fn run_with(f: &Fake, l: &str) -> Out {
    run(f, &line(l))
}

#[test]
fn ls_lists_invitees_only_with_their_bots() {
    let f = Fake::new(table(), Ok(json!({})));
    let o = run_with(&f, "people ls");
    assert_eq!(o.code, 0);
    assert_eq!(
        o.stdout,
        "sam  Sam  ·  house=member  ·  bots: bot-s\nola (access removed)    ·  no agents\n"
    );
    let j: Value = serde_json::from_str(&run_with(&f, "people --json").stdout).expect("json");
    assert_eq!(j["people"][1]["revoked_at"], "2026-11-01");
    assert_eq!(j["people"][0]["everyone_agents"][0]["agent"], "garden");
}

#[test]
fn show_missing_and_empty() {
    let f = Fake::new(table(), Ok(json!({})));
    let o = run_with(&f, "people show kai --json");
    assert_eq!(o.code, 1);
    assert_eq!(
        o.stdout,
        "{\"ok\": false, \"error\": \"no invited person 'kai' (kith people ls)\"}\n"
    );
    let none = Fake::new(json!([]), Ok(json!({})));
    assert_eq!(
        run_with(&none, "people").stdout,
        "nobody invited yet (kith invite add)\n"
    );
}

#[test]
fn usage_errors_exit_2() {
    let f = Fake::new(table(), Ok(json!({})));
    for l in [
        "people ls extra",
        "people show",
        "invite",
        "invite add Sam",
        "invite add --agent house",
        "invite add Sam --agent house=admin",
        "invite add Sam --agent",
        "invite revoke",
        "invite ls extra",
        "people --bogus",
    ] {
        assert_eq!(run_with(&f, l).code, 2, "{l}");
    }
    assert!(f.calls.borrow().is_empty());
}

#[test]
fn add_sends_what_the_python_sends_and_prints_the_app_fields() {
    let host = json!({"inviteId": "inv_1", "personId": "sam", "link": "https://x/j#t", "expiresAt": 5, "tokenHash": "h", "token": "t"});
    let f = Fake::new(table(), Ok(host));
    let o = run_with(
        &f,
        "invite add Sam --agent house=member --agent garden --json",
    );
    assert_eq!(o.code, 0);
    assert_eq!(
        o.stdout,
        "{\"ok\": true, \"inviteId\": \"inv_1\", \"personId\": \"sam\", \"link\": \"https://x/j#t\", \"expiresAt\": 5}\n"
    );
    let calls = f.calls.borrow();
    assert_eq!(calls[0].0, "createInvite");
    assert_eq!(
        calls[0].1,
        json!({"name": "Sam", "agents": {"house": "member", "garden": "guest"}})
    );
    drop(calls);
    run_with(&f, "invite add --person sam --agent house");
    assert_eq!(f.calls.borrow()[1].1["personId"], "sam");
}

#[test]
fn ls_strips_secrets_at_any_depth() {
    let host = json!({"invites": [
        {"inviteId": "i1", "personId": "sam", "tokenHash": "x", "agents": {"house": "guest"}, "more": [{"proof": "p", "ok": 1}]},
        {"inviteId": "i2", "personId": "ola", "usedAt": 3},
        {"inviteId": "i3", "personId": "bo", "revokedAt": 4}]});
    let f = Fake::new(table(), Ok(host));
    let o = run_with(&f, "invite ls");
    assert_eq!(
        o.stdout,
        "i1  sam  open  house=guest\ni2  ola  used  \ni3  bo  revoked  \n"
    );
    let j = run_with(&f, "invite ls --json").stdout;
    assert!(!j.contains("tokenHash") && !j.contains("proof"), "{j}");
}

#[test]
fn revoke_twice_is_changed_false() {
    let f = Fake::new(table(), Ok(json!({"revoked": false})));
    let o = run_with(&f, "invite revoke inv_1 --json");
    assert_eq!(
        o.stdout,
        "{\"ok\": true, \"inviteId\": \"inv_1\", \"changed\": false}\n"
    );
    assert_eq!(
        run_with(&f, "invite revoke inv_1").stdout,
        "inv_1 was already used, revoked or gone\n"
    );
}

#[test]
fn a_host_that_cannot_be_reached_is_exit_1() {
    let f = Fake::new(
        table(),
        Err("kith serve is not reachable (FileNotFoundError)".into()),
    );
    let o = run_with(&f, "invite ls");
    assert_eq!(
        (o.code, o.stderr.as_str()),
        (1, "kith: kith serve is not reachable (FileNotFoundError)\n")
    );
}

#[test]
fn help_and_no_arguments() {
    let f = Fake::new(table(), Ok(json!({})));
    assert_eq!(run(&f, &[]).code, 2);
    let o = run_with(&f, "invite --help");
    assert_eq!(o.code, 0);
    assert!(o.stdout.starts_with("kith-people — the people you invited"));
}
