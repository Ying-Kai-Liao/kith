//! `feed_post` and its `feed-post` verb, against a fake host on a socket: the line it sends, the socket it sends it on,
//! and what it says when there is no owner token.
#![allow(clippy::unwrap_used)]

use super::{Env, run};
use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::Path;

fn world(name: &str) -> std::path::PathBuf {
    let root = std::path::PathBuf::from(format!("/tmp/kpf-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A host answering one request with `reply`; returns the request.
fn host(path: &Path, reply: &'static str) -> std::thread::JoinHandle<Value> {
    let l = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        (&s).write_all(reply.as_bytes()).unwrap();
        serde_json::from_str(&line).unwrap()
    })
}

fn owner(sock: &Path) -> Env {
    Env::with(&[
        ("KITH_ASK_TOKEN", "ask-tok"),
        ("KITH_ASK_SOCKET", sock.to_str().unwrap()),
    ])
}

fn call(args: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "feed_post", "arguments": args}}).to_string()
}

fn said(out: &str) -> String {
    let r: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_tool_sends_feed_post_on_the_owner_socket_and_says_what_the_host_said() {
    let root = world("tool");
    let sock = root.join("ask.sock");
    let h = host(
        &sock,
        "{\"ok\": true, \"message\": \"Posted to the Feed: Done\"}\n",
    );
    let args = json!({"kind": "file", "title": " Done ", "line": "x", "file": "/abs/report.pdf", "fold": "daily"});
    let out = run(
        &owner(&sock),
        &root,
        &[],
        &mut format!("{}\n", call(&args)).as_bytes(),
    );
    assert_eq!(said(&out.stdout), "Posted to the Feed: Done");
    let seen = h.join().unwrap();
    assert_eq!(seen["method"], "feedPost");
    assert_eq!(seen["token"], "ask-tok");
    assert_eq!(seen["title"], "Done");
    assert_eq!(seen["file"], "/abs/report.pdf");
    assert_eq!(seen["fold"], "daily");
}

#[test]
fn a_relative_file_is_made_absolute_from_the_runs_folder() {
    let a = json!({"kind": "file", "title": "t", "file": "out/report.pdf"});
    let body = super::feed::body(a.as_object().unwrap()).unwrap();
    let file = body["file"].as_str().unwrap();
    assert!(
        Path::new(file).is_absolute() && file.ends_with("out/report.pdf"),
        "{file}"
    );
}

#[test]
fn a_guest_run_never_sends_it_and_two_attachments_are_refused_before_asking() {
    let root = world("guest");
    let sock = root.join("run.sock");
    let l = UnixListener::bind(&sock).unwrap();
    l.set_nonblocking(true).unwrap();
    let env = Env::with(&[
        ("KITH_RUN_TOKEN", "run-tok"),
        ("KITH_RUN_SOCKET", sock.to_str().unwrap()),
    ]);
    let args = json!({"kind": "file", "title": "t"});
    let out = run(
        &env,
        &root,
        &[],
        &mut format!("{}\n", call(&args)).as_bytes(),
    );
    assert!(
        said(&out.stdout).starts_with("No Feed for this run"),
        "{}",
        out.stdout
    );
    assert_eq!(
        l.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the guest socket never hears of it"
    );
    let both = json!({"kind": "file", "title": "t", "url": "https://x.y", "number": {"value": 1}});
    assert_eq!(
        super::feed::body(both.as_object().unwrap()).unwrap_err(),
        "Give at most one of file, url and number."
    );
}

#[test]
fn the_verb_posts_and_prints_the_answer_or_its_json() {
    let root = world("verb");
    let sock = root.join("ask.sock");
    let h = host(&sock, "{\"ok\": true, \"message\": \"Posted\"}\n");
    let args: Vec<String> = [
        "feed-post",
        "--kind",
        "number",
        "--title",
        "Users",
        "--number",
        "12",
        "--unit",
        "k",
        "--json",
    ]
    .map(String::from)
    .to_vec();
    assert!(super::is_verb(&args));
    let out = run(&owner(&sock), &root, &args, &mut std::io::empty());
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(out.stdout.trim()).unwrap()["ok"],
        true
    );
    let seen = h.join().unwrap();
    assert_eq!(seen["number"], json!({"value": 12.0, "unit": "k"}));
    // no owner token: exit 1 and the reason, nothing sent
    let none = run(&Env::default(), &root, &args[..5], &mut std::io::empty());
    assert_eq!(none.code, 1);
    assert!(none.stdout.starts_with("No Feed for this run"));
}
