#![allow(
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::match_same_arms,
    clippy::unnecessary_wraps
)]

//! The run socket on a real socket with real child processes (Unix): the run-tokens properties of the host path,
//! exercised against the core's. A client is a `python3` process started under a shell, so "descends from the run's
//! process" is the kernel's process tree and `ps`, not an injected window. The generated properties are in
//! `runs_props.rs`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::Error;
use crate::layout::Layout;
use crate::runs::{RunIdentity, RunTokens};
use crate::runsock::{RunHandler, Server, endpoint};
use crate::tests::{machine, paths};

/// Waits for a go-file, connects, sends the request from the environment, prints the answer line.
const CLIENT: &str = "import os,socket,sys,time\n\
while not os.path.exists(os.environ['KITH_TEST_GO']): time.sleep(0.01)\n\
s=socket.socket(socket.AF_UNIX);s.connect(os.environ['KITH_TEST_SOCK'])\n\
s.sendall((os.environ['KITH_TEST_REQ']+'\\n').encode());sys.stdout.write(s.makefile().readline());sys.stdout.flush()";

/// How the clients sit below the shell that is the run's process.
enum Tree {
    /// The shell's child.
    Child,
    /// The shell's grandchild.
    Grandchild,
    /// Two children at once, with the same token.
    Two,
}

fn script(tree: &Tree) -> &'static str {
    match tree {
        Tree::Child => "python3 -c \"$0\"; true",
        Tree::Grandchild => "sh -c 'python3 -c \"$0\"; true' \"$0\"; true",
        Tree::Two => "python3 -c \"$0\" & python3 -c \"$0\" & wait",
    }
}

fn id(person: &str, project: Option<&str>) -> RunIdentity {
    RunIdentity {
        run_id: String::new(),
        agent: "echo".into(),
        person: person.into(),
        conversation: "thread-1".into(),
        claim: Some("claim-1".into()),
        room: Some("1:1:echo:sam".into()),
        project: project.map(Into::into),
    }
}

/// The handler a test serves with: answers with what the call was made as, so a test sees whose it was.
fn echo() -> Arc<RunHandler> {
    Arc::new(|who, row, params| {
        Ok(
            json!({"row": row, "person": who.person, "agent": who.agent, "conversation": who.conversation,
                   "claim": who.claim, "args": params["args"]}),
        )
    })
}

struct Fixture {
    _t: tempfile::TempDir,
    layout: Layout,
    tokens: Arc<RunTokens>,
    server: Server,
}

fn fixture(handler: Arc<RunHandler>) -> Fixture {
    let (t, m) = machine();
    let layout = Layout::new(&m);
    std::fs::create_dir_all(&layout.dir).expect("core folder");
    let tokens = Arc::new(RunTokens::new());
    let server = Server::bind(&layout, tokens.clone(), handler).expect("run socket");
    Fixture {
        _t: t,
        layout,
        tokens,
        server,
    }
}

impl Fixture {
    fn sock(&self) -> std::path::PathBuf {
        endpoint(&self.layout)
    }

    /// Mints a run and starts clients below `tree`, binds the run to `bind_to(shell pid)`, lets the clients go, and
    /// returns every answer line. `token` is what the clients send (default: the minted one).
    fn ask(
        &self,
        who: RunIdentity,
        tree: &Tree,
        bind: Bind,
        request: impl Fn(&str) -> Value,
    ) -> Vec<Value> {
        let (token, who) = self.tokens.mint(who).expect("mint");
        let go = self._t.path().join(format!("go-{}", who.run_id));
        let sh = shell(&self.sock(), tree, &request(&token), &go);
        let mut sh = sh;
        let other = match bind {
            Bind::Shell => None,
            Bind::Other => Some(
                Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("a process of another tree"),
            ),
            Bind::Nobody => None,
        };
        match (&bind, &other) {
            (Bind::Shell, _) => assert!(self.tokens.bind(&who.run_id, sh.id())),
            (Bind::Other, Some(o)) => assert!(self.tokens.bind(&who.run_id, o.id())),
            _ => {}
        }
        std::fs::write(&go, "go").expect("go");
        let out = lines(&mut sh);
        let _ = sh.wait();
        if let Some(mut o) = other {
            let _ = o.kill();
            let _ = o.wait();
        }
        out
    }
}

enum Bind {
    /// To the shell the clients sit under.
    Shell,
    /// To a process the clients do not descend from.
    Other,
    /// Never bound.
    Nobody,
}

fn shell(sock: &Path, tree: &Tree, request: &Value, go: &Path) -> Child {
    Command::new("sh")
        .args(["-c", script(tree), CLIENT])
        .env("KITH_TEST_SOCK", sock)
        .env("KITH_TEST_GO", go)
        .env("KITH_TEST_REQ", request.to_string())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sh")
}

fn lines(sh: &mut Child) -> Vec<Value> {
    let out = sh.stdout.take().expect("piped");
    BufReader::new(out)
        .lines()
        .map(|l| l.expect("line"))
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(&l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
        .collect()
}

fn recall(token: &str) -> Value {
    json!({"token": token, "method": "recall", "args": ["zebra", "--json"]})
}

fn not_authorized(v: &Value) -> bool {
    v == &json!({"ok": false, "error": "not authorized"})
}

/// One request from this test process, which is not below any run.
fn direct(sock: &Path, line: &str) -> Value {
    let mut s = UnixStream::connect(sock).expect("connect");
    s.write_all(format!("{line}\n").as_bytes()).expect("send");
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).expect("reply");
    serde_json::from_str(&reply).unwrap_or_else(|e| panic!("{reply:?}: {e}"))
}

#[test]
fn a_child_and_a_grandchild_of_the_run_are_answered_as_the_runs_identity() {
    let f = fixture(echo());
    for tree in [Tree::Child, Tree::Grandchild] {
        let got = f.ask(id("sam", None), &tree, Bind::Shell, recall);
        assert_eq!(got.len(), 1);
        let a = &got[0];
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(a["row"], "run.recall");
        // Who is asking comes from the token, whatever the line claims.
        assert_eq!(a["person"], "sam");
        assert_eq!(a["conversation"], "thread-1");
        assert_eq!(a["claim"], "claim-1");
        assert_eq!(a["args"], json!(["--tier", "2", "--json", "zebra"]));
    }
    f.server.stop();
}

#[test]
fn a_forged_identity_in_the_request_changes_nothing() {
    let f = fixture(echo());
    let got = f.ask(id("sam", None), &Tree::Child, Bind::Shell, |t| {
        json!({"token": t, "method": "recall", "args": ["q"], "person": "kai", "agent": "owner",
               "audience": ["shared"], "env": {"KITH_PERSON": "kai"}})
    });
    assert_eq!(got[0]["person"], "sam");
    assert_eq!(got[0]["agent"], "echo");
    // A flag that would name the asker is refused, not passed on.
    let got = f.ask(
        id("sam", None),
        &Tree::Child,
        Bind::Shell,
        |t| json!({"token": t, "method": "recall", "args": ["q", "--person", "kai"]}),
    );
    assert_eq!(got[0]["ok"], false);
    f.server.stop();
}

#[test]
fn a_peer_that_is_not_below_the_runs_process_is_refused() {
    let f = fixture(echo());
    // A sibling tree (the token bound to another process).
    let got = f.ask(id("sam", None), &Tree::Child, Bind::Other, recall);
    assert!(not_authorized(&got[0]), "{got:?}");
    // This process itself: connected, token valid and bound to a real child, but it is nobody's descendant.
    let (token, who) = f.tokens.mint(id("sam", None)).unwrap();
    let mut other = Command::new("sleep").arg("60").spawn().unwrap();
    assert!(f.tokens.bind(&who.run_id, other.id()));
    assert!(not_authorized(&direct(
        &f.sock(),
        &recall(&token).to_string()
    )));
    let _ = other.kill();
    let _ = other.wait();
    f.server.stop();
}

#[test]
fn a_token_that_was_never_bound_is_refused_to_its_own_children() {
    let f = fixture(echo());
    let got = f.ask(id("sam", None), &Tree::Child, Bind::Nobody, recall);
    assert!(not_authorized(&got[0]), "{got:?}");
    f.server.stop();
}

#[test]
fn unknown_empty_and_mangled_tokens_are_refused_the_same_way() {
    let f = fixture(echo());
    let (token, who) = f.tokens.mint(id("sam", None)).unwrap();
    let mut sh = Command::new("sleep").arg("60").spawn().unwrap();
    assert!(f.tokens.bind(&who.run_id, sh.id()));
    for bad in [
        String::new(),
        "x".into(),
        token.to_uppercase(),
        token[..63].into(),
        format!("{token} "),
        "0".repeat(64),
    ] {
        let a = direct(&f.sock(), &recall(&bad).to_string());
        assert!(not_authorized(&a), "{bad:?}: {a}");
    }
    let _ = sh.kill();
    let _ = sh.wait();
    f.server.stop();
}

#[test]
fn an_ended_token_is_refused_and_the_other_runs_go_on() {
    let f = fixture(echo());
    // Two runs; the first is ended before its client asks.
    let (t1, a) = f.tokens.mint(id("sam", None)).unwrap();
    let go = f._t.path().join("go-ended");
    let mut sh = shell(&f.sock(), &Tree::Child, &recall(&t1), &go);
    assert!(f.tokens.bind(&a.run_id, sh.id()));
    assert!(f.tokens.end(&a.run_id));
    std::fs::write(&go, "go").unwrap();
    let got = lines(&mut sh);
    let _ = sh.wait();
    assert!(not_authorized(&got[0]), "{got:?}");
    // Another run is untouched.
    let ok = f.ask(id("eve", None), &Tree::Child, Bind::Shell, recall);
    assert_eq!(ok[0]["person"], "eve");
    assert_eq!(f.tokens.len(), 1);
    f.server.stop();
}

#[test]
fn a_method_outside_the_subset_is_refused_with_a_valid_token() {
    let f = fixture(echo());
    for method in [
        "requestPermission",
        "requestAccess",
        "consumeAccess",
        "requestSecret",
        "cardsHistory",
        "Recall",
        "recall ",
        "forget",
        "run.recall",
        "run.mint",
        "",
    ] {
        let got = f.ask(
            id("sam", None),
            &Tree::Child,
            Bind::Shell,
            |t| json!({"token": t, "method": method, "args": ["q"], "tool": "Bash", "input": {}}),
        );
        assert_eq!(
            got[0],
            json!({"ok": false, "error": "unknown method"}),
            "{method:?}"
        );
    }
    // The three it has are answered.
    for (method, args) in [
        ("recall", json!(["q"])),
        ("memorySearch", json!(["q"])),
        ("projectId", json!([])),
    ] {
        let got = f.ask(
            id("sam", Some("alpha")),
            &Tree::Child,
            Bind::Shell,
            |t| json!({"token": t, "method": method, "args": args}),
        );
        assert_eq!(got[0]["ok"], true, "{method}: {got:?}");
    }
    f.server.stop();
}

#[test]
fn one_token_works_for_two_descendants_at_once() {
    let f = fixture(echo());
    let got = f.ask(id("sam", None), &Tree::Two, Bind::Shell, recall);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|a| a["ok"] == true && a["person"] == "sam"));
    f.server.stop();
}

#[test]
fn an_end_during_a_call_lets_that_call_finish_and_refuses_the_next() {
    // The call is past the check when the run ends: it finishes as the run it was (no escalation, same as host.sock).
    let gate = Arc::new(Barrier::new(2));
    let seen = Arc::new(Mutex::new(0u32));
    let handler: Arc<RunHandler> = {
        let (gate, seen) = (gate.clone(), seen.clone());
        Arc::new(move |who, _, _| {
            *seen.lock().unwrap() += 1;
            gate.wait(); // in flight
            gate.wait(); // the run has ended
            Ok(json!({"person": who.person}))
        })
    };
    let f = fixture(handler);
    let (token, who) = f.tokens.mint(id("sam", None)).unwrap();
    let go = f._t.path().join("go-flight");
    let mut sh = shell(&f.sock(), &Tree::Child, &recall(&token), &go);
    assert!(f.tokens.bind(&who.run_id, sh.id()));
    std::fs::write(&go, "go").unwrap();
    gate.wait();
    assert!(f.tokens.end(&who.run_id));
    gate.wait();
    let got = lines(&mut sh);
    let _ = sh.wait();
    assert_eq!(got[0]["person"], "sam");
    assert_eq!(*seen.lock().unwrap(), 1);
    // The next request with that token is refused.
    let go2 = f._t.path().join("go-flight-2");
    let mut again = shell(&f.sock(), &Tree::Child, &recall(&token), &go2);
    std::fs::write(&go2, "go").unwrap();
    let got = lines(&mut again);
    let _ = again.wait();
    assert!(not_authorized(&got[0]));
    f.server.stop();
}

#[test]
fn a_run_whose_process_is_gone_has_a_dead_token() {
    fn yes(_: u32, _: u32) -> bool {
        true
    }
    let t = RunTokens::with(yes, kith_kernel::procstart::start_stamp);
    let (token, who) = t.mint(id("sam", None)).unwrap();
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    assert!(t.bind(&who.run_id, child.id()));
    assert!(t.resolve(&token, Some(1234)).is_some());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        t.resolve(&token, Some(1234)).is_none(),
        "its process is gone"
    );
    assert_eq!(t.len(), 0, "and the entry went with it");
}

#[test]
fn a_reused_pid_does_not_revive_a_token() {
    static EPOCH: AtomicU32 = AtomicU32::new(0);
    fn yes(_: u32, _: u32) -> bool {
        true
    }
    // The start time of pid 4242 changes when another process takes the pid.
    fn started(_: u32) -> Option<String> {
        Some(format!("boot {}", EPOCH.load(Ordering::SeqCst)))
    }
    let t = RunTokens::with(yes, started);
    let (token, who) = t.mint(id("sam", None)).unwrap();
    assert!(t.bind(&who.run_id, 4242));
    assert!(t.resolve(&token, Some(4242)).is_some());
    EPOCH.store(1, Ordering::SeqCst);
    assert!(t.resolve(&token, Some(4242)).is_none());
    EPOCH.store(0, Ordering::SeqCst);
    assert!(
        t.resolve(&token, Some(4242)).is_none(),
        "gone for good, not for a moment"
    );
}

#[test]
fn a_big_line_is_refused_unread() {
    let f = fixture(echo());
    // An oversized line is answered without being read to the end (the slow peer is cut off by the read budget).
    let mut s = UnixStream::connect(f.sock()).unwrap();
    let _ = s.write_all(&vec![b'a'; 70_000]);
    let mut reply = String::new();
    let _ = BufReader::new(&s).read_line(&mut reply);
    assert!(
        reply.contains("malformed") || reply.contains("too large"),
        "{reply:?}"
    );
    f.server.stop();
}

#[test]
fn the_run_socket_is_owner_only_and_leaves_with_the_server() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = fixture(echo());
    let mode = std::fs::metadata(f.sock()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(f.sock().parent(), Some(f.layout.dir.as_path()));
    let sock = f.sock();
    f.server.stop();
    assert!(!sock.exists());
}

// --- through the daemon --------------------------------------------------------------------------------------------

#[test]
fn the_daemon_mints_on_the_owner_socket_and_serves_the_run_on_the_run_socket() {
    use crate::daemon::{Config, Daemon};
    let (_t, m) = machine();
    let p = paths(&m);
    let d = Daemon::spawn_with(
        &p,
        Config {
            holder_on: false,
            key_load: None,
            host: false,
            ..Config::default()
        },
    );
    d.ready(Duration::from_secs(10)).expect("serving");
    let layout = Layout::new(&m);
    let sock = endpoint(&layout);
    let owner = |method: &str, params: &Value| match crate::reach(&p, method, params, false) {
        crate::Reached::Answered(v) => v,
        other => panic!("{method}: {other:?}"),
    };
    owner("people.addPerson", &json!({"id": "sam"}));
    let minted = owner(
        "run.mint",
        &json!({"agent": "echo", "person": "sam", "conversation": "t1", "project": "alpha", "claim": "c1"}),
    );
    let (token, run_id) = (
        minted["token"].as_str().unwrap().to_owned(),
        minted["runId"].as_str().unwrap().to_owned(),
    );
    // The row takes params that fit, and says so when they do not.
    assert!(matches!(
        crate::reach(&p, "run.mint", &json!({"agent": "echo"}), false),
        crate::Reached::Refused(_)
    ));

    // One go file per ask. A name taken from a clock would repeat, and a go file left by the earlier ask lets the next
    // shell run before its bind, which the socket rightly refuses.
    let asks = AtomicU32::new(0);
    let ask = |tree: &Tree, request: Value| {
        let go = _t
            .path()
            .join(format!("go-{}", asks.fetch_add(1, Ordering::SeqCst)));
        let mut sh = shell(&sock, tree, &request, &go);
        let bound = owner("run.bind", &json!({"runId": run_id, "pid": sh.id()}));
        assert_eq!(bound["bound"], true);
        std::fs::write(&go, "go").unwrap();
        let out = lines(&mut sh);
        let _ = sh.wait();
        out
    };
    // projectId is the core's own answer, from the token.
    let got = ask(
        &Tree::Grandchild,
        json!({"token": token, "method": "projectId", "args": []}),
    );
    assert_eq!(got[0]["ok"], true, "{got:?}");
    assert_eq!(got[0]["stdout"], "alpha\n");
    // `--info` of its own project is the kernel's tool: not answered here yet, and nothing else is either.
    let got = ask(
        &Tree::Child,
        json!({"token": token, "method": "projectId", "args": ["--info", "alpha"]}),
    );
    assert_eq!(got[0]["ok"], false);
    let got = ask(
        &Tree::Child,
        json!({"token": token, "method": "recall", "args": ["q"]}),
    );
    assert_eq!(
        got[0],
        json!({"ok": false, "error": "that isn't answered by the core yet"})
    );
    let got = ask(
        &Tree::Child,
        json!({"token": token, "method": "projectId", "args": ["--info", "beta"]}),
    );
    assert_eq!(
        got[0]["error"],
        "only this run's own project is available here"
    );

    // The owner ends it; the next ask is refused.
    assert_eq!(owner("run.end", &json!({"runId": run_id}))["ended"], true);
    assert_eq!(owner("run.end", &json!({"runId": run_id}))["ended"], false);
    let go = _t.path().join("go-after");
    let mut sh = shell(
        &sock,
        &Tree::Child,
        &json!({"token": token, "method": "projectId", "args": []}),
        &go,
    );
    std::fs::write(&go, "go").unwrap();
    let got = lines(&mut sh);
    let _ = sh.wait();
    assert!(not_authorized(&got[0]), "{got:?}");

    // The run table is not reachable by a run: the owner rows are not run-token rows, and the run rows are not the
    // owner's.
    assert!(matches!(
        crate::reach(&p, "run.recall", &json!({"args": ["q"]}), false),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    drop(d);
    assert!(!sock.exists(), "the run socket leaves with the daemon");
}

#[test]
fn a_process_without_a_readable_start_time_is_never_bound() {
    // Windows until `GetProcessTimes` lands, or `ps` failing: without a start time a reused pid could not be told
    // from the run, so the token is refused at bind and stays unbound.
    fn yes(_: u32, _: u32) -> bool {
        true
    }
    fn blind(_: u32) -> Option<String> {
        None
    }
    let t = RunTokens::with(yes, blind);
    let (token, who) = t.mint(id("sam", None)).unwrap();
    assert!(!t.bind(&who.run_id, 4242), "bind says no");
    assert!(t.resolve(&token, Some(4242)).is_none());
    assert_eq!(t.len(), 1, "the token is still the owner's to end");
    assert!(t.end(&who.run_id));
}

// --- revoke through the daemon (M2-13) -------------------------------------------------------------------------------

/// A person's run and a bot's run answer on the run socket; the owner suspends the person and both stop at once; a
/// mint for the suspended person is refused; undo revives neither token and a new mint works; a removed person's
/// token ends too; the token is nowhere on disk but in the mint's answer.
#[test]
fn revoke_through_the_daemon_ends_the_tokens_and_a_revoked_person_cannot_mint() {
    use crate::daemon::{Config, Daemon};
    let (_t, m) = machine();
    let p = paths(&m);
    let d = Daemon::spawn_with(
        &p,
        Config {
            holder_on: false,
            ..Config::default()
        },
    );
    d.ready(Duration::from_secs(10)).expect("serving");
    let sock = endpoint(&Layout::new(&m));
    let owner = |method: &str, params: &Value| match crate::reach(&p, method, params, false) {
        crate::Reached::Answered(v) => v,
        other => panic!("{method}: {other:?}"),
    };
    owner("people.addPerson", &json!({"id": "sam"}));
    owner("people.addPerson", &json!({"id": "amy"}));
    owner("people.addBot", &json!({"id": "bot-a", "of": "sam"}));
    let mint = |person: &str| {
        let v = owner(
            "run.mint",
            &json!({"agent": "echo", "person": person, "conversation": "t1", "project": "alpha"}),
        );
        (
            v["token"].as_str().unwrap().to_owned(),
            v["runId"].as_str().unwrap().to_owned(),
            v["socket"].as_str().unwrap().to_owned(),
        )
    };
    let asks = AtomicU32::new(0);
    // One request from a shell the run was bound to; true when the core answered as the run.
    let works = |(token, run_id, _): &(String, String, String)| {
        let go = _t
            .path()
            .join(format!("go-{}", asks.fetch_add(1, Ordering::SeqCst)));
        let mut sh = shell(
            &sock,
            &Tree::Child,
            &json!({"token": token, "method": "projectId", "args": []}),
            &go,
        );
        // A run that already ended cannot be bound: the answer says so, and the ask below is refused anyway.
        let _ = owner("run.bind", &json!({"runId": run_id, "pid": sh.id()}));
        std::fs::write(&go, "go").unwrap();
        let out = lines(&mut sh);
        let _ = sh.wait();
        out[0]["ok"] == true
    };
    // Nobody is minted for an id that is not a person here.
    assert!(matches!(
        crate::reach(
            &p,
            "run.mint",
            &json!({"agent": "echo", "person": "ghost", "conversation": "t1"}),
            false
        ),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    let (sam, bot, amy) = (mint("sam"), mint("bot-a"), mint("amy"));
    assert_eq!(sam.2, sock.to_string_lossy(), "the mint says where to ask");
    assert!(works(&sam) && works(&bot) && works(&amy));

    // The suspension commits and the tokens of sam and their bot are gone in that same call.
    owner("people.suspend", &json!({"id": "sam"}));
    assert!(!works(&sam), "the person's token died with the revoke");
    assert!(!works(&bot), "the bot's token died with it");
    assert!(works(&amy), "another person's token is untouched");
    // No run starts for a revoked person or for their bot.
    for person in ["sam", "bot-a"] {
        match crate::reach(
            &p,
            "run.mint",
            &json!({"agent": "echo", "person": person, "conversation": "t1"}),
            false,
        ) {
            crate::Reached::Refused(Error::NotAllowed(why)) => {
                assert!(why.contains("is revoked or not a person"), "{why}");
            }
            other => panic!("{person}: {other:?}"),
        }
    }
    // Undo gives the person their access back, not the old tokens; a new run mints a new one.
    owner("people.undo", &json!({"id": "sam"}));
    assert!(!works(&sam) && !works(&bot));
    let again = mint("sam");
    assert!(works(&again) && again.0 != sam.0);

    // A removal ends the token too.
    owner("people.removePerson", &json!({"id": "amy"}));
    assert!(!works(&amy));

    // The token is shown once: no file of the machine folder holds any of them.
    let mut dirs = vec![m.clone()];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                for (token, ..) in [&sam, &bot, &amy, &again] {
                    assert!(
                        !bytes.windows(token.len()).any(|w| w == token.as_bytes()),
                        "{} holds a token",
                        path.display()
                    );
                }
            }
        }
    }
    drop(d);
}
