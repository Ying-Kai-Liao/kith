#![allow(clippy::unwrap_used)]

use super::*;
use axum::extract::Path as RoutePath;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use std::os::unix::fs::PermissionsExt as _;

const SECS: Duration = Duration::from_secs(5);

/// A short data folder (a socket path is short on macOS).
fn data(tag: &str) -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/kl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A router that says what reached it: the method, whether the peer looked like loopback, and the bearer it was sent.
fn stub() -> Router {
    async fn echo(
        RoutePath(method): RoutePath<String>,
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        headers: HeaderMap,
        body: String,
    ) -> axum::Json<serde_json::Value> {
        let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
        axum::Json(
            serde_json::json!({"method": method, "loopback": peer.ip().is_loopback(), "auth": auth, "body": body}),
        )
    }
    Router::new()
        .route("/api/{method}", post(echo))
        .route("/ingest/statusline", post(|| async { StatusCode::NO_CONTENT }))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_router_answers_a_call_on_the_socket_with_the_bearer_it_was_sent() {
    let data = data("ok");
    let endpoint = Arc::new(bind(&data).unwrap());
    serve(endpoint.clone(), stub());
    let (ok, v) = call(data.clone(), 1, "tok".into(), "ping", &serde_json::json!({"a": 1}), SECS).await.unwrap();
    assert!(ok);
    assert_eq!(v["method"], "ping");
    assert_eq!(v["loopback"], true);
    assert_eq!(v["auth"], "Bearer tok");
    assert_eq!(v["body"], r#"{"a":1}"#);
    // the statusline's POST is served too
    let (ok, _) = post_to(data.clone(), 1, "/ingest/statusline".into(), "tok".into(), "{}".into(), SECS).await.unwrap();
    assert!(ok);
    endpoint.close();
    assert!(!hostlocal::endpoint(&data).exists(), "the socket goes with the host");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_api_and_the_statusline_are_served_here() {
    let data = data("routes");
    let endpoint = Arc::new(bind(&data).unwrap());
    serve(endpoint, stub());
    for path in ["/channel", "/term/x", "/api/a/b", "/api/", "/api/x?y=1", "/health"] {
        let (ok, _) = post_to(data.clone(), 1, path.into(), "t".into(), "{}".into(), SECS).await.unwrap();
        assert!(!ok, "{path}");
    }
}

#[test]
fn a_request_is_read_by_its_length_and_a_chunked_one_is_refused() {
    let mut raw: &[u8] =
        b"POST /api/x HTTP/1.1\r\nAuthorization: Bearer t\r\nX-Other: 1\r\nContent-Length: 3\r\n\r\nabcEXTRA";
    let r = read_request(&mut raw).unwrap();
    assert_eq!((r.method.as_str(), r.path.as_str(), r.body.as_slice()), ("POST", "/api/x", &b"abc"[..]));
    assert_eq!(r.headers, vec![("authorization".to_owned(), "Bearer t".to_owned())], "only what the handlers read");
    let mut chunked: &[u8] = b"POST /api/x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
    assert_eq!(read_request(&mut chunked).err(), Some(StatusCode::NOT_IMPLEMENTED));
    let mut big = format!("POST /api/x HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1).into_bytes();
    assert_eq!(read_request(&mut &big[..]).err(), Some(StatusCode::PAYLOAD_TOO_LARGE));
    big.clear();
    assert_eq!(
        read_request(&mut &b"POST /api/x HTTP/1.1\r\nContent-Length: 9\r\n\r\nabc"[..]).err(),
        Some(StatusCode::BAD_REQUEST)
    );
}

#[test]
fn a_stale_socket_is_replaced_and_something_else_in_the_way_is_not_touched() {
    let data = data("stale");
    // a crashed host's socket
    drop(bind(&data).unwrap());
    assert!(hostlocal::endpoint(&data).exists());
    let again = bind(&data).unwrap();
    again.close();
    // a regular file at the path is not ours to remove
    std::fs::write(hostlocal::endpoint(&data), "x").unwrap();
    assert!(bind(&data).is_err());
    assert!(hostlocal::endpoint(&data).exists());
}

#[test]
fn the_socket_and_its_folder_are_private_to_this_account() {
    let data = data("modes");
    let endpoint = bind(&data).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&hostlocal::folder(&data)), 0o700);
    assert_eq!(mode(&hostlocal::endpoint(&data)), 0o600);
    endpoint.close();
}

#[test]
fn a_data_folder_too_deep_for_a_socket_is_refused_by_name() {
    let deep = PathBuf::from(format!("/tmp/kl-{}/{}", std::process::id(), "d".repeat(100)));
    let err = bind(&deep).err().unwrap();
    assert!(format!("{err:#}").contains("too long"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_says_only_half_a_request_does_not_hold_up_others() {
    let data = data("idle");
    let endpoint = Arc::new(bind(&data).unwrap());
    serve(endpoint, stub());
    let mut idle = std::os::unix::net::UnixStream::connect(hostlocal::endpoint(&data)).unwrap();
    idle.write_all(b"POST /api/x HT").unwrap();
    let (ok, _) = call(data.clone(), 1, "t".into(), "ping", &serde_json::json!({}), SECS).await.unwrap();
    assert!(ok);
    drop(idle);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_event_stream_is_sent_until_it_ends_and_a_refusal_is_a_plain_answer() {
    use axum::response::IntoResponse as _;
    async fn events(headers: HeaderMap) -> axum::response::Response {
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer tok") {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let chunks = futures::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from("data: one\n\n")),
            Ok(axum::body::Bytes::from("data: two\n\n")),
        ]);
        ([("content-type", "text/event-stream")], Body::from_stream(chunks)).into_response()
    }
    let data = data("events");
    let endpoint = Arc::new(bind(&data).unwrap());
    serve(endpoint, stub().route("/events", get(events)));
    let ask = |auth: &str| {
        let mut c = std::os::unix::net::UnixStream::connect(hostlocal::endpoint(&data)).unwrap();
        write!(c, "GET /events?since=0 HTTP/1.1\r\nAuthorization: {auth}\r\nConnection: close\r\n\r\n").unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        got
    };
    let got = ask("Bearer tok");
    assert!(got.starts_with("HTTP/1.1 200 OK\r\n") && got.ends_with("data: one\n\ndata: two\n\n"), "{got}");
    assert!(ask("Bearer no").starts_with("HTTP/1.1 401 "));
}
