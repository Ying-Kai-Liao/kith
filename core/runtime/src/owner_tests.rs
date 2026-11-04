//! The owner socket's M1-0 follow-ups: compatibility by the rows the client calls, and a `stop` that cannot hang.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kith_api::Principal;
use serde_json::{Map, Value, json};

use super::{Handler, Reached, Server, reach_with, row_hash};
use crate::layout::Layout;
use crate::tests::machine;

fn serving(m: &std::path::Path, handler: Arc<Handler>) -> (Layout, Server) {
    let l = Layout::new(m);
    crate::perms::ensure_dir(&l.dir).expect("dir");
    let server = Server::bind(&l, 1, handler).expect("bind");
    (l, server)
}

fn echo() -> Arc<Handler> {
    Arc::new(|_, method, _| Ok(json!({"ran": method})))
}

fn rows(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect()
}

#[test]
fn a_daemon_with_extra_rows_still_serves_and_one_missing_a_row_or_shaped_differently_does_not() {
    let (_t, m) = machine();
    let (l, _server) = serving(&m, echo());
    // A client of another build: its whole table differs (it does not know this daemon's extra rows), but the one row
    // it calls is the same here.
    let paths_row = row_hash("paths").expect("paths is a row");
    match reach_with(
        &l,
        "paths",
        &json!({}),
        true,
        (super::PROTOCOL, "0000000000000000"),
        Some(&rows(&[("paths", paths_row)])),
        Principal::Owner,
    ) {
        Reached::Answered(v) => assert_eq!(v["ran"], "paths"),
        other => panic!("{other:?}"),
    }
    // A row this daemon lacks (an older daemon, from the client's side).
    match reach_with(
        &l,
        "frobnicate",
        &json!({}),
        true,
        (super::PROTOCOL, kith_api::schema_hash()),
        Some(&rows(&[("frobnicate", "1234567812345678")])),
        Principal::Owner,
    ) {
        Reached::Mismatch(note) => assert!(note.contains("frobnicate"), "{note}"),
        other => panic!("{other:?}"),
    }
    // A row both have, read differently.
    match reach_with(
        &l,
        "paths",
        &json!({}),
        true,
        (super::PROTOCOL, kith_api::schema_hash()),
        Some(&rows(&[("paths", "1234567812345678")])),
        Principal::Owner,
    ) {
        Reached::Mismatch(note) => assert!(note.contains("paths"), "{note}"),
        other => panic!("{other:?}"),
    }
    // The honest client names its row and gets through.
    assert!(matches!(
        super::reach(&l, "paths", &json!({}), true),
        Reached::Answered(_)
    ));
}

fn within(what: &str, limit: Duration, f: impl FnOnce()) {
    let begun = Instant::now();
    f();
    assert!(begun.elapsed() < limit, "{what} took {:?}", begun.elapsed());
}

#[test]
fn stop_is_quick_when_idle_and_twice() {
    let (_t, m) = machine();
    let (l, server) = serving(&m, echo());
    within("an idle stop", Duration::from_secs(1), || server.stop());
    within("a second stop", Duration::from_secs(1), || server.stop());
    assert!(matches!(
        super::reach(&l, "paths", &json!({}), true),
        Reached::NoDaemon
    ));
}

#[test]
fn stop_does_not_wait_for_a_client_in_the_middle_of_a_call() {
    let (_t, m) = machine();
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let entered_tx = std::sync::Mutex::new(entered_tx);
    let released = std::sync::Mutex::new(released);
    let handler: Arc<Handler> = Arc::new(move |_, method, _| {
        let _ = entered_tx.lock().expect("lock").send(());
        let _ = released
            .lock()
            .expect("lock")
            .recv_timeout(Duration::from_secs(20));
        Ok(json!({"ran": method}))
    });
    let (l, server) = serving(&m, handler);
    let client = std::thread::spawn(move || super::reach(&l, "paths", &json!({}), false));
    entered
        .recv_timeout(Duration::from_secs(10))
        .expect("the call is in the handler");
    within(
        "a stop with a call in flight",
        Duration::from_secs(5),
        || {
            server.stop();
        },
    );
    // The call in flight finishes with its answer or a lost connection; either way it ends.
    let _ = release.send(());
    let r = client.join().expect("client");
    assert!(
        matches!(r, Reached::Answered(_) | Reached::Lost(_)),
        "{r:?}"
    );
}
