//! The MCP server's tools, with fakes for the host (a Unix socket), `kith-ask`, `kith-auth` and the Python helper. The golden
//! `permit-mcp.json` holds the Python's answers; these hold what it cannot replay (the 900 s wait, a door's sandbox, a card that
//! changes `auth.json` between two requests) and the Rust-side differences.
#![allow(clippy::unwrap_used)]

use super::{Env, Out, run};
use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn world(name: &str) -> PathBuf {
    let root = PathBuf::from(format!("/tmp/kpm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn script(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A host on a socket: answers the n-th request with the n-th reply (the last one after that) and returns the requests.
fn host(
    path: &Path,
    replies: &[&'static str],
    requests: usize,
) -> std::thread::JoinHandle<Vec<Value>> {
    let l = UnixListener::bind(path).unwrap();
    let replies: Vec<&'static str> = replies.to_vec();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        for n in 0..requests {
            let (s, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&s).read_line(&mut line).unwrap();
            seen.push(serde_json::from_str(&line).unwrap_or(Value::Null));
            (&s).write_all(replies[n.min(replies.len() - 1)].as_bytes())
                .unwrap();
        }
        seen
    })
}

fn guest(root: &Path, sock: &Path) -> Env {
    let mut e = Env::with(&[
        ("KITH_RUN_TOKEN", "run-tok"),
        ("KITH_RUN_SOCKET", sock.to_str().unwrap()),
        ("KITH_PERSON", "guest"),
        ("KITH_AGENT", "front"),
        ("KITH_AGENT_DIR", root.join("pkg").to_str().unwrap()),
        ("KITH_STATE", root.join(".kith").to_str().unwrap()),
    ]);
    e.python = root.join("py").to_str().unwrap().to_owned();
    e
}

fn serve(env: &Env, kernel: &Path, lines: &[Value]) -> Vec<Value> {
    let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let out = run(env, kernel, &[], &mut input.as_bytes());
    assert_eq!(out.code, 0, "{}", out.stderr);
    out.stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: u32, name: &str, args: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

fn said(reply: &Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

const ALLOW: &str = "{\"ok\": true, \"behavior\": \"allow\"}\n";
const DENY: &str = "{\"ok\": true, \"behavior\": \"deny\", \"message\": \"Not today\"}\n";

#[test]
fn the_tool_list_follows_the_environment() {
    let names = |env: &Env| -> Vec<String> {
        let r = serve(
            env,
            Path::new("/nonexistent"),
            &[json!({"id": 1, "method": "tools/list"})],
        );
        r[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        names(&Env::default()),
        ["approve", "request_access", "report_bug"]
    );
    let owner = Env::with(&[("KITH_ASK_TOKEN", "t"), ("KITH_ASK_SOCKET", "/s")]);
    assert_eq!(
        names(&owner),
        ["approve", "request_access", "report_bug", "request_secret"]
    );
    // a token with no socket is no way to ask
    assert_eq!(names(&Env::with(&[("KITH_ASK_TOKEN", "t")])).len(), 3);
    assert_eq!(
        names(&Env::with(&[("KITH_PERSON", "guest")])),
        [
            "approve",
            "request_access",
            "report_bug",
            "ask_helper",
            "run_door"
        ]
    );
    assert_eq!(names(&Env::with(&[("KITH_PERSON", "")])).len(), 3);
}

#[test]
fn ids_are_echoed_as_they_came_and_notifications_get_no_answer() {
    let r = serve(
        &Env::default(),
        Path::new("/nonexistent"),
        &[
            json!({"id": "café", "method": "ping"}),
            json!({"method": "ping"}),
            json!({"id": null, "method": "ping"}),
            json!({"id": 2.5, "method": "nope"}),
            json!({"id": 3, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
            json!({"id": 4, "method": "initialize", "params": {"protocolVersion": ""}}),
        ],
    );
    assert_eq!(r.len(), 4);
    assert_eq!(r[0], json!({"jsonrpc": "2.0", "id": "café", "result": {}}));
    assert_eq!(
        r[1]["error"],
        json!({"code": -32601, "message": "method not found"})
    );
    assert_eq!(r[2]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(r[3]["result"]["protocolVersion"], "2024-11-05");
    // the reply is ASCII, as json.dumps writes it
    let out = run(
        &Env::default(),
        Path::new("/nonexistent"),
        &[],
        &mut "{\"id\": \"café\", \"method\": \"ping\"}\n".as_bytes(),
    );
    assert_eq!(
        out.stdout,
        "{\"jsonrpc\": \"2.0\", \"id\": \"caf\\u00e9\", \"result\": {}}\n"
    );
}

#[test]
fn a_line_that_is_no_json_is_a_parse_error_and_no_object_is_skipped() {
    let input = "{oops\n[1]\n5\n\n   \n{\"id\": 1, \"method\": \"ping\"}\n";
    let out: Out = run(
        &Env::default(),
        Path::new("/nonexistent"),
        &[],
        &mut input.as_bytes(),
    );
    assert_eq!(
        out.stdout,
        "{\"jsonrpc\": \"2.0\", \"id\": null, \"error\": {\"code\": -32700, \"message\": \"parse error\"}}\n\
         {\"jsonrpc\": \"2.0\", \"id\": 1, \"result\": {}}\n"
    );
}

#[test]
fn params_that_are_no_object_end_the_server_where_python_dies() {
    for msg in [
        json!({"id": 1, "method": "initialize", "params": [1]}),
        json!({"id": 1, "method": "tools/call", "params": "x"}),
        json!({"id": 1, "method": "tools/call", "params": {"name": "report_bug", "arguments": [1]}}),
        json!({"id": 1, "method": "tools/call", "params": {"name": "approve", "arguments": "x"}}),
    ] {
        let out = run(
            &Env::default(),
            Path::new("/nonexistent"),
            &[],
            &mut format!("{msg}\n").as_bytes(),
        );
        assert_eq!(out.code, 1, "{msg}");
        assert!(
            out.stderr.starts_with("AttributeError: '"),
            "{}",
            out.stderr
        );
        assert_eq!(out.stdout, "");
    }
    // an unknown tool never reads its arguments
    let r = serve(
        &Env::default(),
        Path::new("/nonexistent"),
        &[json!({"id": 1, "method": "tools/call", "params": {"name": "other", "arguments": [1]}})],
    );
    assert_eq!(
        said(&r[0]),
        "{\"behavior\": \"deny\", \"message\": \"Malformed permission request.\"}"
    );
}

#[test]
fn approve_answers_allow_with_the_input_as_asked_and_denies_every_other_answer() {
    let root = world("approve");
    let sock = root.join("s");
    let h = host(
        &sock,
        &[
            ALLOW,
            DENY,
            "{\"ok\": false, \"error\": \"busy\"}\n",
            "[1]\n",
            "not json\n",
            "{\"ok\": true, \"behavior\": \"allow\", \"waiting\": true}\n",
        ],
        6,
    );
    let env = guest(&root, &sock);
    let input = json!({"command": "ls \"é\"", "n": 1.5, "z": [null, true]});
    let ask = |i| {
        call(
            i,
            "approve",
            &json!({"tool_name": "Bash", "input": input, "tool_use_id": "t1"}),
        )
    };
    let r = serve(&env, &root, &(1..=6).map(ask).collect::<Vec<_>>());
    assert_eq!(
        said(&r[0]),
        "{\"behavior\": \"allow\", \"updatedInput\": {\"command\": \"ls \\\"\\u00e9\\\"\", \"n\": 1.5, \"z\": [null, true]}}"
    );
    assert_eq!(
        said(&r[1]),
        "{\"behavior\": \"deny\", \"message\": \"Not today\"}"
    );
    assert_eq!(
        said(&r[2]),
        "{\"behavior\": \"deny\", \"message\": \"busy\"}"
    );
    assert_eq!(
        said(&r[3]),
        "{\"behavior\": \"deny\", \"message\": \"The approval service gave no clear answer; the action was refused.\"}"
    );
    assert!(said(&r[4]).contains("Could not reach the approval service"));
    assert!(said(&r[5]).contains("\"allow\""));
    let seen = h.join().unwrap();
    assert_eq!(seen[0]["tool"], "Bash");
    assert_eq!(seen[0]["method"], "requestPermission");
    assert_eq!(seen[0]["token"], "run-tok");
    // tool_use_id is not forwarded
    assert!(seen[0].get("tool_use_id").is_none());
    assert_eq!(seen[0]["input"], input);
}

#[test]
fn a_waiting_answer_parks_the_ask_that_started_the_run() {
    let root = world("park");
    let sock = root.join("s");
    let h = host(
        &sock,
        &[
            "{\"ok\": true, \"behavior\": \"deny\", \"waiting\": true, \"message\": \"waiting on kai\"}\n",
        ],
        2,
    );
    let mut env = guest(&root, &sock);
    env.set("KITH_ASK_ID", "ask-7");
    let ask = |i| call(i, "approve", &json!({"tool_name": "Bash", "input": {}}));
    let r = serve(&env, &root, &[ask(1)]);
    assert!(said(&r[0]).contains("waiting on kai"));
    assert!(root.join(".kith/live/ask/ask-7.parked").is_file());
    // an id that is no id parks nothing
    std::fs::remove_dir_all(root.join(".kith")).unwrap();
    env.set("KITH_ASK_ID", "bad id!");
    serve(&env, &root, &[ask(2)]);
    assert!(!root.join(".kith/live/ask").exists());
    h.join().unwrap();
}

#[test]
fn the_guests_pair_is_used_before_the_owners() {
    let root = world("pair");
    let (guest_sock, owner_sock) = (root.join("g"), root.join("o"));
    let h = host(&guest_sock, &[ALLOW], 1);
    let mut env = guest(&root, &guest_sock);
    env.set("KITH_ASK_TOKEN", "ask-tok");
    env.set("KITH_ASK_SOCKET", owner_sock.to_str().unwrap());
    let r = serve(
        &env,
        &root,
        &[call(
            1,
            "approve",
            &json!({"tool_name": "Read", "input": {}}),
        )],
    );
    assert!(said(&r[0]).contains("\"allow\""));
    assert_eq!(h.join().unwrap()[0]["token"], "run-tok");
}

#[test]
fn request_access_sends_a_path_or_a_domain_and_write_only_for_true() {
    let root = world("access");
    let sock = root.join("s");
    let h = host(&sock, &["{\"ok\": true, \"message\": \"Asked kai.\"}\n"], 3);
    let env = guest(&root, &sock);
    let r = serve(
        &env,
        &root,
        &[
            call(
                1,
                "request_access",
                &json!({"path": "/a/b", "reason": "r", "write": true}),
            ),
            call(
                2,
                "request_access",
                &json!({"domain": "pypi.org", "reason": "r", "write": "yes"}),
            ),
            call(
                3,
                "request_access",
                &json!({"path": "/a", "domain": "b.org", "reason": "r"}),
            ),
            call(
                4,
                "request_access",
                &json!({"path": "", "domain": "pypi.org", "reason": "r"}),
            ),
            call(5, "request_access", &json!({"path": "/a", "reason": 5})),
        ],
    );
    assert_eq!(said(&r[0]), "Asked kai.");
    assert_eq!(
        said(&r[2]),
        "Give either an absolute path or a domain, and a reason."
    );
    assert_eq!(
        said(&r[4]),
        "Give either an absolute path or a domain, and a reason."
    );
    let seen = h.join().unwrap();
    assert_eq!(
        seen[0],
        json!({"kind": "path", "target": "/a/b", "reason": "r", "write": true, "token": "run-tok", "method": "requestAccess"})
    );
    assert_eq!(seen[1]["write"], false);
    assert_eq!(seen[1]["kind"], "domain");
    assert_eq!(seen[2]["target"], "pypi.org");
}

#[test]
fn request_secret_strips_the_service_and_asks_the_host() {
    let root = world("secret");
    let sock = root.join("s");
    let h = host(
        &sock,
        &["{\"ok\": false, \"error\": \"that service is not declared\"}\n"],
        1,
    );
    let env = guest(&root, &sock);
    let r = serve(
        &env,
        &root,
        &[
            call(
                1,
                "request_secret",
                &json!({"service": "  github ", "purpose": " x "}),
            ),
            call(
                2,
                "request_secret",
                &json!({"service": "github", "purpose": "   "}),
            ),
        ],
    );
    assert_eq!(said(&r[0]), "that service is not declared");
    assert_eq!(said(&r[1]), "Give the service and what you need it for.");
    let seen = h.join().unwrap();
    assert_eq!(seen[0]["service"], "github");
    assert_eq!(seen[0]["purpose"], " x ");
    assert_eq!(seen[0]["method"], "requestSecret");
}

/// A kernel folder with fakes: `kith-auth may` says yes to what `granted` lists (one "PERSON AGENT" per line, `--via` ones as
/// "PERSON AGENT via CALLER"), and `kith-ask` runs `ask`.
fn kernel(root: &Path, granted: &str, ask: &str) -> PathBuf {
    let k = root.join("k");
    script(
        &k.join("bin/kith-auth"),
        &format!(
            "#!/bin/sh\n[ \"$1\" = may ] || exit 2\nshift\nif [ \"$3\" = --via ]; then key=\"$1 $2 via $4\"; else key=\"$1 $2\"; fi\n\
             grep -qx \"$key\" <<'EOF'\n{granted}\nEOF\n"
        ),
    );
    script(&k.join("bin/kith-ask"), &format!("#!/bin/sh\n{ask}\n"));
    k
}

#[test]
fn ask_helper_runs_a_granted_helper_without_the_owners_token_and_without_a_card() {
    let root = world("helper");
    let k = kernel(
        &root,
        "guest front\nguest helper via front",
        "echo \"answer from $1 about $3 ask=${KITH_ASK_TOKEN:-none} run=${KITH_RUN_TOKEN:-none}\"",
    );
    let mut env = guest(&root, &root.join("no-host"));
    env.set("KITH_ASK_TOKEN", "owner-secret");
    env.set("KITH_ASK_SOCKET", "/nowhere");
    let r = serve(
        &env,
        &k,
        &[call(
            1,
            "ask_helper",
            &json!({"agent": "helper", "task": "say pong"}),
        )],
    );
    assert_eq!(
        said(&r[0]),
        "answer from helper about say pong ask=none run=run-tok"
    );
}

#[test]
fn ask_helper_asks_the_owner_first_for_a_helper_the_membership_does_not_grant() {
    let root = world("helper-card");
    let sock = root.join("s");
    let h = host(&sock, &[ALLOW, DENY], 2);
    let k = kernel(&root, "guest front", "echo ran $1");
    let env = guest(&root, &sock);
    let task = "é".repeat(2500);
    let r = serve(
        &env,
        &k,
        &[
            call(1, "ask_helper", &json!({"agent": "third", "task": task})),
            call(2, "ask_helper", &json!({"agent": "third", "task": "again"})),
        ],
    );
    assert_eq!(said(&r[0]), "ran third");
    assert_eq!(said(&r[1]), "Not today");
    let seen = h.join().unwrap();
    assert_eq!(seen[0]["tool"], "mcp__kithpermit__ask_helper");
    assert_eq!(seen[0]["input"]["agent"], "third");
    // 2000 characters, not bytes
    assert_eq!(
        seen[0]["input"]["task"].as_str().unwrap().chars().count(),
        2000
    );
    assert_eq!(seen[1]["input"]["task"], "again");
}

#[test]
fn a_person_who_may_not_use_the_caller_gets_no_card() {
    let root = world("helper-nocard");
    let k = kernel(&root, "", "echo \"kith: guest may not use $1\" >&2; exit 1");
    // no host at all: a card would be refused with "Could not reach", which is not what comes back
    let env = guest(&root, &root.join("no-host"));
    let r = serve(
        &env,
        &k,
        &[call(
            1,
            "ask_helper",
            &json!({"agent": "third", "task": "x"}),
        )],
    );
    assert_eq!(
        said(&r[0]),
        "Could not ask third: kith: guest may not use third"
    );
}

#[test]
fn ask_helper_tells_why_there_is_no_answer() {
    let root = world("helper-fail");
    let granted = "guest front\nguest helper via front";
    let say = |ask: &str, task: &str| {
        let k = kernel(&root, granted, ask);
        let env = guest(&root, &root.join("no-host"));
        said(
            &serve(
                &env,
                &k,
                &[call(
                    1,
                    "ask_helper",
                    &json!({"agent": "helper", "task": task}),
                )],
            )[0],
        )
    };
    assert_eq!(say("exit 0", "x"), "helper gave no answer.");
    assert_eq!(say("echo '  fine \r\n'", "x"), "fine");
    assert_eq!(
        say("echo boom >&2; echo out; exit 1", "x"),
        "Could not ask helper: boom"
    );
    assert_eq!(say("echo out; exit 1", "x"), "Could not ask helper: out");
    assert_eq!(say("exit 1", "x"), "Could not ask helper: no answer");
    assert_eq!(
        say("echo '   ' >&2; echo out; exit 1", "x"),
        "Could not ask helper: "
    );
    assert_eq!(
        say("python3 -c \"print('e' * 3000)\" >&2; exit 1", "x"),
        format!("Could not ask helper: {}", "e".repeat(1500))
    );
    assert!(
        say(
            "echo 'this Mac is out of sync with the Kith cloud' >&2; exit 1",
            "x"
        )
        .ends_with("Try again in a minute.")
    );
    assert_eq!(say("exit 0", &"a".repeat(20001)), "The task is too long.");
    assert_eq!(say("echo ok", &"中".repeat(20000)), "ok");
    // no kith-ask to run
    let k = kernel(&root, granted, "exit 0");
    std::fs::remove_file(k.join("bin/kith-ask")).unwrap();
    let env = guest(&root, &root.join("no-host"));
    let r = serve(
        &env,
        &k,
        &[call(
            1,
            "ask_helper",
            &json!({"agent": "helper", "task": "x"}),
        )],
    );
    assert_eq!(
        said(&r[0]),
        "Could not ask helper (No such file or directory)."
    );
}

#[test]
fn ask_helper_gives_up_after_its_wait() {
    let root = world("helper-wait");
    let k = kernel(&root, "guest front\nguest helper via front", "sleep 30");
    let env = guest(&root, &root.join("no-host"));
    let a = json!({"agent": "helper", "task": "x"});
    let started = std::time::Instant::now();
    let said = super::helper::ask_helper_within(
        a.as_object().unwrap(),
        &env,
        &k,
        Duration::from_millis(300),
    );
    assert_eq!(said, "helper did not answer in time; carry on without it.");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn ask_helper_wait_ending_stops_the_whole_group() {
    let root = world("helper-group");
    let r = root.display();
    // the fake kith-ask starts a grandchild that outlives it, and notes the TERM it gets
    let body = format!(
        "trap 'echo termed > \"{r}/termed\"; exit 143' TERM\nsleep 60 &\necho $! > \"{r}/grandchild\"\nwait"
    );
    let k = kernel(&root, "guest front\nguest helper via front", &body);
    let env = guest(&root, &root.join("no-host"));
    let a = json!({"agent": "helper", "task": "x"});
    let said = super::helper::ask_helper_within(
        a.as_object().unwrap(),
        &env,
        &k,
        // long enough for the script to start on a busy machine
        Duration::from_secs(4),
    );
    assert_eq!(said, "helper did not answer in time; carry on without it.");
    let pid: i64 = std::fs::read_to_string(root.join("grandchild"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while crate::threads::os::pid_runs(pid) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!crate::threads::os::pid_runs(pid), "the grandchild lives");
    assert!(root.join("termed").exists(), "the helper got no TERM first");
}

#[test]
fn ask_helper_refuses_before_anything_runs() {
    let root = world("helper-args");
    let k = kernel(&root, "", "echo ran");
    let env = guest(&root, &root.join("no-host"));
    let say = |env: &Env, a: Value| said(&serve(env, &k, &[call(1, "ask_helper", &a)])[0]);
    for bad in [
        json!({}),
        json!({"agent": "helper"}),
        json!({"agent": "../x", "task": "t"}),
        json!({"agent": "a b", "task": "t"}),
        json!({"agent": "-x", "task": "t"}),
        json!({"agent": "helper\n", "task": "t"}),
        json!({"agent": "helper", "task": "  \t"}),
        json!({"agent": 5, "task": "t"}),
    ] {
        assert_eq!(
            say(&env, bad.clone()),
            "Give the helper agent and the task.",
            "{bad}"
        );
    }
    assert_eq!(
        say(&env, json!({"agent": "front", "task": "t"})),
        "You are front; ask another agent."
    );
    let owner = Env::with(&[("KITH_RUN_TOKEN", "t"), ("KITH_AGENT", "front")]);
    assert_eq!(
        say(&owner, json!({"agent": "helper", "task": "t"})),
        "Helpers are asked from a guest's run only."
    );
}

/// A Python that answers the door's two scripts from files: `check.json`, `prepare.json`, `run.json` in `root`, and logs the
/// request of every act.
fn fake_python(root: &Path, check: &str, prepare: &str, run: &str) {
    std::fs::write(root.join("check.json"), check).unwrap();
    std::fs::write(root.join("prepare.json"), prepare).unwrap();
    std::fs::write(root.join("run.json"), run).unwrap();
    let r = root.display();
    script(
        &root.join("py"),
        &format!(
            "#!/bin/sh\nreq=$(cat)\nprintf '%s\\n' \"$req\" >> {r}/py.log\ncase \"$3\" in\n  *'kith_door.check('*) cat {r}/check.json ;;\n  *) case \"$req\" in\n      *'\"act\":\"prepare\"'*) cat {r}/prepare.json ;;\n      *) cat {r}/run.json ;;\n    esac ;;\nesac\n"
        ),
    );
}

fn door_call(root: &Path, replies: &[&'static str], cards: usize) -> (String, Vec<Value>) {
    let sock = root.join("s");
    let h = (cards > 0).then(|| host(&sock, replies, cards));
    let env = guest(root, &sock);
    let r = serve(
        &env,
        root,
        &[call(
            1,
            "run_door",
            &json!({"door": "render", "path": "/files/people/guest/proj"}),
        )],
    );
    (said(&r[0]), h.map_or_else(Vec::new, |h| h.join().unwrap()))
}

#[test]
fn run_door_runs_a_ticked_prepared_door_and_tells_how_it_ended() {
    let root = world("door-run");
    let ok = "{\"ok\": true, \"real\": \"/files/people/guest/proj\", \"ready\": true, \"downloads\": \"d\"}\n";
    fake_python(
        &root,
        ok,
        "{}",
        "{\"code\": 0, \"tail\": \"done\\nall good\"}\n",
    );
    // no card is asked: the host would not be reached
    let (text, _) = door_call(&root, &[ALLOW], 0);
    assert_eq!(
        text,
        "render ended. Its output, last lines:\ndone\nall good"
    );
    for (run, want) in [
        (
            "{\"code\": 3, \"tail\": \"boom\"}",
            "render failed (exit 3). Its output, last lines:\nboom",
        ),
        (
            "{\"code\": null, \"tail\": \"the door was stopped\"}",
            "render was stopped. Its output, last lines:\nthe door was stopped",
        ),
        (
            "{\"end\": \"The door did not run: doors need macOS sandbox-exec\"}",
            "The door did not run: doors need macOS sandbox-exec",
        ),
        ("not json", "The door did not run: the door helper failed"),
    ] {
        fake_python(&root, ok, "{}", run);
        assert_eq!(door_call(&root, &[ALLOW], 0).0, want);
    }
    let log = std::fs::read_to_string(root.join("py.log")).unwrap();
    assert!(log.contains("\"person\":\"guest\"") && log.contains("\"agent\":\"front\""));
    assert!(log.contains("\"act\":\"run\""));
}

#[test]
fn run_door_asks_the_card_whose_yes_turns_the_job_on_then_the_prepare_card() {
    let root = world("door-cards");
    let card = "{\"card\": true, \"real\": \"/real/proj\", \"ready\": false, \"downloads\": \"Downloads x\"}\n";
    fake_python(
        &root,
        card,
        "{\"prepared\": true}",
        "{\"code\": 0, \"tail\": \"t\"}",
    );
    let (text, seen) = door_call(&root, &[ALLOW], 2);
    assert_eq!(text, "render ended. Its output, last lines:\nt");
    assert_eq!(seen[0]["tool"], "mcp__kithpermit__run_door");
    assert_eq!(
        seen[0]["input"],
        json!({"door": "render", "path": "/real/proj"})
    );
    assert_eq!(seen[1]["tool"], "mcp__kithpermit__prepare_door");
    assert_eq!(
        seen[1]["input"],
        json!({"door": "render", "downloads": "Downloads x"})
    );
    let log = std::fs::read_to_string(root.join("py.log")).unwrap();
    let acts: Vec<&str> = log.lines().filter(|l| l.contains("\"act\"")).collect();
    assert_eq!(acts.len(), 2);
    assert!(acts[0].contains("\"act\":\"prepare\"") && acts[1].contains("\"act\":\"run\""));
    // the acts run on the folder the card showed, not on the path the guest gave
    assert!(acts.iter().all(|l| l.contains("\"path\":\"/real/proj\"")));

    // a no on the first card ends it: nothing is prepared or run
    let root = world("door-no");
    fake_python(
        &root,
        card,
        "{\"prepared\": true}",
        "{\"code\": 0, \"tail\": \"t\"}",
    );
    let (text, seen) = door_call(&root, &[DENY], 1);
    assert_eq!(text, "Not today");
    assert_eq!(seen.len(), 1);
    assert!(
        !std::fs::read_to_string(root.join("py.log"))
            .unwrap()
            .contains("\"act\"")
    );

    // a yes, then a no to the download
    let root = world("door-no-prepare");
    fake_python(
        &root,
        card,
        "{\"prepared\": true}",
        "{\"code\": 0, \"tail\": \"t\"}",
    );
    let (text, _) = door_call(&root, &[ALLOW, DENY], 2);
    assert_eq!(text, "Not today");
    assert!(
        !std::fs::read_to_string(root.join("py.log"))
            .unwrap()
            .contains("\"act\"")
    );

    // preparing fails: its words reach the agent and nothing runs
    let root = world("door-prepare-fails");
    fake_python(
        &root,
        card,
        "{\"end\": \"The door did not run: npm is not installed\"}",
        "{\"code\": 0, \"tail\": \"t\"}",
    );
    let (text, _) = door_call(&root, &[ALLOW], 2);
    assert_eq!(text, "The door did not run: npm is not installed");
    assert!(
        !std::fs::read_to_string(root.join("py.log"))
            .unwrap()
            .contains("\"act\":\"run\"")
    );
}

#[test]
fn run_door_ends_on_what_the_check_says() {
    let root = world("door-end");
    for (check, want) in [
        (
            "{\"end\": \"Access is being changed right now; try again in a minute.\"}",
            "Access is being changed right now; try again in a minute.",
        ),
        (
            "{\"end\": \"bill's access was removed\"}",
            "bill's access was removed",
        ),
        ("", "The door did not run: the door helper failed"),
    ] {
        fake_python(&root, check, "{}", "{}");
        assert_eq!(door_call(&root, &[ALLOW], 0).0, want);
    }
}

#[test]
fn run_door_checks_its_arguments_and_whose_run_it_is() {
    let root = world("door-args");
    fake_python(&root, "{}", "{}", "{}");
    let env = guest(&root, &root.join("no-host"));
    let say = |env: &Env, a: Value| said(&serve(env, &root, &[call(1, "run_door", &a)])[0]);
    for bad in [
        json!({}),
        json!({"door": "render"}),
        json!({"door": "render", "path": "relative"}),
        json!({"door": "../x", "path": "/a"}),
        json!({"door": "a".repeat(65), "path": "/a"}),
        json!({"door": "render", "path": 5}),
    ] {
        assert_eq!(
            say(&env, bad.clone()),
            "Give the door and the absolute path of the project folder.",
            "{bad}"
        );
    }
    let mut no_dir = env.clone();
    no_dir.set("KITH_AGENT_DIR", "");
    assert_eq!(
        say(&no_dir, json!({"door": "render", "path": "/a"})),
        "Doors are run from a guest's run only."
    );
    // a 64-character name is fine: the helper is reached (and answers nothing)
    fake_python(&root, "", "{}", "{}");
    assert_eq!(
        say(&env, json!({"door": "a".repeat(64), "path": "/a"})),
        "The door did not run: the door helper failed"
    );
}

#[test]
fn report_bug_asks_the_card_with_the_draft_and_files_only_on_a_clear_allow() {
    let root = world("bug");
    let sock = root.join("s");
    let h = host(&sock, &[ALLOW, DENY, "{\"ok\": true}\n"], 3);
    let draft = json!({"repo": "r/k", "title": "t", "body": "b", "labels": ["bug", "from:agent"]});
    let first = json!({"draft": draft, "text": "Repo: r/k\nTitle: t", "card": {"repo": "r/k", "similar": [], "draft": "Repo: r/k\nTitle: t"}});
    // the fake Python: the first script is the draft step, the second the finish step; it answers from the yes it was given
    script(
        &root.join("py"),
        &format!(
            "#!/bin/sh\nreq=$(cat)\nprintf '%s\\n' \"$req\" >> {r}/py.log\necho \"$#\" \"$4\" >> {r}/argv.log\ncase \"$3\" in\n  *kith_bug.build_draft*) printf '%s\\n' '{first}' ;;\n  *) case \"$req\" in *'\"yes\":true'*) echo '{{\"text\": \"Filed: https://example.test/7\"}}' ;; *) echo '{{\"text\": \"Not filed (no yes).\"}}' ;; esac ;;\nesac\n",
            r = root.display(),
            first = first
        ),
    );
    let env = guest(&root, &sock);
    let ask = |i| {
        call(
            i,
            "report_bug",
            &json!({"title": "crash", "details": "it broke"}),
        )
    };
    let r = serve(&env, &root, &[ask(1), ask(2), ask(3)]);
    assert_eq!(
        said(&r[0]),
        "Redacted draft:\n\nRepo: r/k\nTitle: t\n\nFiled: https://example.test/7"
    );
    assert_eq!(
        said(&r[1]),
        "Redacted draft:\n\nRepo: r/k\nTitle: t\n\nNot filed (no yes)."
    );
    assert_eq!(
        said(&r[2]),
        "Redacted draft:\n\nRepo: r/k\nTitle: t\n\nNot filed (no yes)."
    );
    let seen = h.join().unwrap();
    assert_eq!(seen[0]["tool"], "mcp__kithpermit__report_bug");
    assert_eq!(seen[0]["input"]["repo"], "r/k");
    // what the helper was given: the title and details as they came
    let log = std::fs::read_to_string(root.join("py.log")).unwrap();
    assert!(log.contains("\"title\":\"crash\"") && log.contains("\"details\":\"it broke\""));
    // ... on stdin: the argv (what `ps` shows, and what E2BIG limits) holds the kernel folder and nothing of the report
    let argv = std::fs::read_to_string(root.join("argv.log")).unwrap();
    let lib = root.join("lib");
    assert!(argv.lines().count() >= 2);
    assert!(argv.lines().all(|l| l == format!("4 {}", lib.display())));
}

#[test]
fn report_bug_with_no_host_files_nothing_and_a_filed_draft_is_not_asked_again() {
    let root = world("bug-nohost");
    script(
        &root.join("py"),
        "#!/bin/sh\nreq=$(cat)\ncase \"$3\" in\n  *kith_bug.build_draft*) echo '{\"draft\": {}, \"text\": \"T\", \"card\": {}}' ;;\n  *) case \"$req\" in *'\"yes\":false'*) echo '{\"text\": \"kept\"}' ;; *) echo '{\"text\": \"FILED\"}' ;; esac ;;\nesac\n",
    );
    let env = guest(&root, &root.join("no-host"));
    let a = json!({"title": "crash"});
    assert_eq!(
        said(&serve(&env, &root, &[call(1, "report_bug", &a)])[0]),
        "Redacted draft:\n\nT\n\nkept"
    );
    script(
        &root.join("py"),
        "#!/bin/sh\ncase \"$3\" in\n  *kith_bug.build_draft*) echo '{\"draft\": {}, \"text\": \"T\", \"done\": \"https://example.test/1\"}' ;;\n  *) echo '{\"err\": \"Boom\"}' ;;\nesac\n",
    );
    assert_eq!(
        said(&serve(&env, &root, &[call(1, "report_bug", &a)])[0]),
        "Redacted draft:\n\nT\n\nAlready filed: https://example.test/1"
    );
    script(
        &root.join("py"),
        "#!/bin/sh\necho '{\"err\": \"OSError\"}'\n",
    );
    assert_eq!(
        said(&serve(&env, &root, &[call(1, "report_bug", &a)])[0]),
        "Could not prepare the bug report (OSError); nothing was filed."
    );
    for bad in [
        json!({}),
        json!({"title": ""}),
        json!({"title": " \n"}),
        json!({"title": 5}),
    ] {
        assert_eq!(
            said(&serve(&env, &root, &[call(1, "report_bug", &bad)])[0]),
            "Give a title."
        );
    }
}

#[test]
fn the_python_helpers_parse() {
    // each script is Python source that goes to `python3 -I -c`; a typo would only show when a door or a report is used
    for (name, src) in [
        ("bug draft", super::bug::DRAFT),
        ("bug finish", super::bug::FINISH),
        ("door check", super::door::CHECK),
        ("door act", super::door::ACT),
    ] {
        let ok = std::process::Command::new("python3")
            .args(["-I", "-c", "import ast, sys; ast.parse(sys.argv[1])", src])
            .status();
        if let Ok(st) = ok {
            assert!(st.success(), "{name} does not parse");
        }
    }
}

#[test]
fn an_output_pipe_held_by_a_grandchild_times_out_instead_of_hanging() {
    use super::proc::{Failed, run as run_proc};
    // the child exits at once, its sleeper keeps stdout open for 5 s
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", "sleep 5 & echo started"]);
    let t = std::time::Instant::now();
    let r = run_proc(
        cmd,
        &Env::with(&[("PATH", "/bin:/usr/bin")]),
        None,
        Duration::from_millis(400),
    );
    assert!(matches!(r, Err(Failed::Timeout)));
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[test]
fn a_child_that_outlives_the_limit_is_killed_and_stdin_reaches_a_child() {
    use super::proc::{Failed, run as run_proc};
    let env = Env::with(&[("PATH", "/bin:/usr/bin")]);
    let mut cmd = std::process::Command::new("/bin/sleep");
    cmd.arg("30");
    let t = std::time::Instant::now();
    assert!(matches!(
        run_proc(cmd, &env, None, Duration::from_millis(300)),
        Err(Failed::Timeout)
    ));
    assert!(t.elapsed() < Duration::from_secs(3));
    let mut cmd = std::process::Command::new("/bin/cat");
    cmd.arg("-");
    let Ok(r) = run_proc(cmd, &env, Some("{\"a\": 1}"), Duration::from_secs(5)) else {
        panic!("cat failed");
    };
    assert_eq!((r.code, r.stdout.as_str()), (0, "{\"a\": 1}"));
}

#[test]
fn the_python_helper_is_not_taken_from_the_guests_folders() {
    // guest_scrub drops a whole variable that has an entry in the guest's folders, so a PATH with one gives the fixed interpreter
    let root = world("py-path");
    let kernel = root.join("kernel");
    let agents = root.join("agents");
    let mine = agents.join("front/files/people/guest/bin");
    let good = root.join("good");
    script(&mine.join("python3"), "#!/bin/sh\necho guest\n");
    script(&good.join("python3"), "#!/bin/sh\necho trusted\n");
    let tainted = format!("{}:{}", mine.display(), good.display());
    let mut e = Env::with(&[
        ("KITH_PERSON", "guest"),
        ("KITH_AGENTS", agents.to_str().unwrap()),
        ("PATH", &tainted),
    ]);
    e.home = root.clone();
    assert_eq!(super::proc::interpreter(&e, &kernel), "/usr/bin/python3");
    // the owner's PATH is trusted as it is
    let owner = e.without(&["KITH_PERSON"]);
    assert_eq!(
        super::proc::interpreter(&owner, &kernel),
        mine.join("python3").to_str().unwrap()
    );
    // a guest's PATH without their folders is searched as the launch does
    let mut clean = e.clone();
    clean.set("PATH", &format!("/nonexistent:{}", good.display()));
    assert_eq!(
        super::proc::interpreter(&clean, &kernel),
        good.join("python3").to_str().unwrap()
    );
}
