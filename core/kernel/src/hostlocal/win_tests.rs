#![allow(clippy::unwrap_used)]

use super::*;
use std::io::{Read as _, Write as _};

const SECS: Duration = Duration::from_secs(10);

/// A data folder name nobody else has: the pipe's name is made from it.
fn data(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kith-hostlocal-{tag}-{}", std::process::id()))
}

#[test]
fn a_call_goes_to_the_hosts_pipe_with_the_token() {
    let data = data("ok");
    let listener = kith_winsys::pipe::listen(&endpoint(&data).to_string_lossy()).unwrap();
    let server = std::thread::spawn(move || {
        let mut c = listener.accept().unwrap();
        let mut got = vec![0u8; 4096];
        let n = c.read(&mut got).unwrap();
        let body = r#"{"ok":true}"#;
        write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        String::from_utf8_lossy(&got[..n]).into_owned()
    });
    let r = post(&data, "/api/ping", "tok", r#"{"a":1}"#, SECS).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &br#"{"ok":true}"#[..]));
    let sent = server.join().unwrap();
    assert!(sent.starts_with("POST /api/ping HTTP/1.1\r\n"), "{sent}");
    assert!(sent.contains("Authorization: Bearer tok\r\n"));
}

#[test]
fn a_host_that_is_down_is_refused_never_tried_over_tcp() {
    let data = data("down");
    assert!(matches!(post(&data, "/api/x", "t", "{}", SECS), Err(LocalErr::Down)));
    assert_eq!(status(&data, 1), Status::Down);
}
