#![allow(clippy::unwrap_used)]

use super::*;
use std::os::unix::fs::PermissionsExt as _;

const SECS: Duration = Duration::from_secs(5);

#[test]
fn a_plain_and_a_chunked_answer_both_read() {
    let plain: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    let r = read_reply(&mut &*plain).unwrap();
    assert_eq!((r.status, r.body), (200, b"{}".to_vec()));
    let chunked: &[u8] =
        b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4;x=1\r\n\":1}\r\n0\r\n\r\n";
    let r = read_reply(&mut &*chunked).unwrap();
    assert_eq!((r.status, r.body), (404, b"{\"a\":1}".to_vec()));
    // no length and not chunked: to the end of the stream
    let open: &[u8] = b"HTTP/1.1 200 OK\r\n\r\nabc";
    assert_eq!(read_reply(&mut &*open).unwrap().body, b"abc");
    // a short body, and no answer at all, are no answer
    let short: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc";
    assert!(read_reply(&mut &*short).is_none());
    assert!(read_reply(&mut &b"garbage"[..]).is_none());
}

#[test]
fn a_call_goes_to_the_hosts_socket_with_the_token() {
    let data = testkit::data_dir("hl-ok");
    let h = testkit::serve_once(&data, "200 OK", r#"{"ok":true}"#);
    let r = post(&data, "/api/ping", "tok", r#"{"a":1}"#, SECS).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &br#"{"ok":true}"#[..]));
    let sent = h.join().unwrap();
    assert!(sent.starts_with("POST /api/ping HTTP/1.1\r\n"));
    assert!(sent.contains("Authorization: Bearer tok\r\n"));
    assert!(sent.ends_with(r#"{"a":1}"#));
}

#[test]
fn a_host_that_is_down_is_refused_never_tried_over_tcp() {
    let data = testkit::data_dir("hl-down");
    assert!(matches!(
        post(&data, "/api/x", "t", "{}", SECS),
        Err(LocalErr::Down)
    ));
    // a folder with no socket in it, and a socket nobody listens on (a crashed host's)
    std::fs::create_dir_all(folder(&data)).unwrap();
    std::fs::set_permissions(folder(&data), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        post(&data, "/api/x", "t", "{}", SECS),
        Err(LocalErr::Down)
    ));
    drop(testkit::listen(&data));
    assert!(matches!(
        post(&data, "/api/x", "t", "{}", SECS),
        Err(LocalErr::Down)
    ));
}

#[test]
fn a_folder_or_socket_others_can_open_is_not_vouched_for() {
    let data = testkit::data_dir("hl-perm");
    let _l = testkit::listen(&data);
    let untrusted = |data: &Path| {
        matches!(
            post(data, "/api/x", "t", "{}", SECS),
            Err(LocalErr::Untrusted(_))
        )
    };
    // others can open the socket
    std::fs::set_permissions(endpoint(&data), std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(untrusted(&data));
    std::fs::set_permissions(endpoint(&data), std::fs::Permissions::from_mode(0o600)).unwrap();
    // others can enter the folder
    std::fs::set_permissions(folder(&data), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(untrusted(&data));
    std::fs::set_permissions(folder(&data), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        post(&data, "/api/x", "t", "{}", Duration::from_millis(200)),
        Err(LocalErr::TimedOut | LocalErr::Io(_))
    ));
}

#[test]
fn a_link_in_place_of_the_socket_is_not_followed() {
    let data = testkit::data_dir("hl-link");
    let real = testkit::data_dir("hl-link-real");
    let _l = testkit::listen(&real);
    std::fs::create_dir_all(folder(&data)).unwrap();
    std::fs::set_permissions(folder(&data), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(endpoint(&real), endpoint(&data)).unwrap();
    assert!(matches!(
        post(&data, "/api/x", "t", "{}", SECS),
        Err(LocalErr::Untrusted(_))
    ));
}

#[test]
fn a_token_with_a_line_break_sends_nothing() {
    let data = testkit::data_dir("hl-crlf");
    let l = testkit::listen(&data);
    l.set_nonblocking(true).unwrap();
    assert!(matches!(
        post(&data, "/api/x", "t\r\nX: y", "{}", SECS),
        Err(LocalErr::Io(_))
    ));
    assert!(l.accept().is_err(), "no connection was made");
}

#[test]
fn many_callers_at_once_each_get_their_own_answer() {
    let data = testkit::data_dir("hl-many");
    let l = testkit::listen(&data);
    let server = std::thread::spawn(move || {
        for _ in 0..8 {
            let (mut c, _) = l.accept().unwrap();
            std::thread::spawn(move || {
                let mut got = vec![0u8; 4096];
                let n = c.read(&mut got).unwrap();
                let text = String::from_utf8_lossy(&got[..n]).into_owned();
                let n = text.rsplit('=').next().unwrap().to_owned();
                let body = format!("{{\"n\":{n}}}");
                write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
        }
    });
    let callers: Vec<_> = (0..8)
        .map(|i| {
            let data = data.clone();
            std::thread::spawn(move || {
                let r = post(&data, "/api/x", "t", &format!("n={i}"), SECS).unwrap();
                assert_eq!(r.body, format!("{{\"n\":{i}}}").into_bytes());
            })
        })
        .collect();
    for c in callers {
        c.join().unwrap();
    }
    server.join().unwrap();
}

#[test]
fn up_older_and_down_are_told_apart() {
    let data = testkit::data_dir("hl-status");
    let old = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = old.local_addr().unwrap().port();
    // a port that answers and no endpoint: an older host
    assert_eq!(status(&data, port), Status::Older);
    let l = testkit::listen(&data);
    assert_eq!(status(&data, port), Status::Up);
    drop(l);
    drop(old);
    assert_eq!(status(&data, port), Status::Down);
}

#[test]
fn a_path_too_long_for_a_socket_is_said_not_bound() {
    assert!(path_fits(Path::new("/tmp/kith/serve/owner/host.sock")));
    assert!(!path_fits(&PathBuf::from(format!(
        "/tmp/{}/host.sock",
        "a".repeat(100)
    ))));
}
