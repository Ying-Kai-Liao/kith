//! Drives the real binary over plain HTTP, no Claude Code needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

struct Fake {
    child: Child,
    addr: String,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(script: &str, log: &std::path::Path) -> Fake {
    let dir = log.parent().unwrap();
    let sp = dir.join("script.json");
    std::fs::write(&sp, script).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kith-fakemodel"))
        .args(["--script", sp.to_str().unwrap(), "--log", log.to_str().unwrap()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut addr = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut addr).unwrap();
    Fake { child, addr: addr.trim().to_string() }
}

/// POST and read the whole reply (the server closes streams, and answers JSON with a length; ask for close either way).
fn post(addr: &str, path: &str, body: &str) -> String {
    let mut c = TcpStream::connect(addr).unwrap();
    write!(c, "POST {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}", body.len())
        .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn script_turns_stream_match_and_fall_back() {
    let dir = std::env::temp_dir().join(format!("fakemodel-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("req.jsonl");
    let script = r#"{"turns":[
        {"text":"first"},
        {"match":"special","text":"matched"},
        {"text":"running","tool_use":[{"name":"Bash","input":{"command":"echo hi"}}]}
    ],"fallback":"out of script"}"#;
    let f = start(script, &log);
    let req = |text: &str, stream: bool| {
        format!(r#"{{"model":"claude-sonnet-4","stream":{stream},"messages":[{{"role":"user","content":"{text}"}}]}}"#)
    };

    let r = post(&f.addr, "/v1/messages", &req("hello", true));
    assert!(r.contains("text/event-stream"), "{r}");
    for e in [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "content_block_stop",
        "message_delta",
        "message_stop",
    ] {
        assert!(r.contains(&format!("event: {e}")), "{e} in {r}");
    }
    assert!(r.contains("first") && r.contains("end_turn"));

    // The matching turn is skipped until the user message has its substring; the unmatched one goes first.
    let r = post(&f.addr, "/v1/messages", &req("hello again", false));
    assert!(r.contains("running") && r.contains("\"stop_reason\":\"tool_use\"") && r.contains("echo hi"), "{r}");
    let r = post(&f.addr, "/v1/messages", &req("a special day", false));
    assert!(r.contains("matched"), "{r}");
    let r = post(&f.addr, "/v1/messages", &req("more", false));
    assert!(r.contains("out of script"), "{r}");

    // A haiku side call and count_tokens never consume a turn.
    let r = post(&f.addr, "/v1/messages/count_tokens", "{}");
    assert!(r.contains("input_tokens"), "{r}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert_eq!(logged.lines().count(), 5);
    assert!(logged.contains("a special day"));
    let _ = std::fs::remove_dir_all(&dir);
}
