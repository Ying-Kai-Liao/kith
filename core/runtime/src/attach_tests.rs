#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

//! The attach relay through a real daemon, holder and attach socket, with a shell script as the engine.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use kith_holder::proto::HeldId;
use kith_holder::{attach, wire};
use serde_json::{Value, json};

use crate::layout::Layout;
use crate::terminal::Role;
use crate::terminal::tests::daemon::{RUNNING, Rig, answered, open, plain, until};

/// One client of the attach socket.
struct Viewer {
    s: UnixStream,
}

impl Viewer {
    fn connect(m: &Path) -> Viewer {
        let s = UnixStream::connect(Layout::new(m).dir.join("attach.sock")).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        Viewer { s }
    }

    fn send(&mut self, kind: u8, payload: &[u8]) {
        wire::write_frame(&mut self.s, kind, 0, payload).unwrap();
        self.s.flush().unwrap();
    }

    fn attach(&mut self, ticket: &str, role: &str, extra: &Value) {
        let mut v = json!({"v": 1, "ticket": ticket, "role": role});
        for (k, x) in extra.as_object().into_iter().flatten() {
            v[k] = x.clone();
        }
        self.send(attach::ATTACH, v.to_string().as_bytes());
    }

    /// The next frame; `None` when the relay closed the connection (or nothing came in time).
    fn frame(&mut self) -> Option<wire::Frame> {
        wire::read(&mut self.s).ok().flatten()
    }

    fn frame_within(&mut self, wait: Duration) -> Option<wire::Frame> {
        self.s.set_read_timeout(Some(wait)).unwrap();
        let got = wire::read(&mut self.s).ok().flatten();
        self.s
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        got
    }

    /// The ATTACHED answer (or the ERROR that refused it) as JSON, with its kind.
    fn answer(&mut self) -> (u8, Value) {
        let (kind, _, payload) = self.frame().expect("an answer");
        (
            kind,
            serde_json::from_slice(&payload).unwrap_or(Value::Null),
        )
    }

    /// Reads OUTPUT until the text holds `needle`; returns everything read.
    fn read_until(&mut self, needle: &str) -> String {
        let mut text = Vec::new();
        loop {
            let (kind, _, payload) = self
                .frame()
                .unwrap_or_else(|| panic!("closed before {needle:?}; got {:?}", lossy(&text)));
            match kind {
                attach::OUTPUT => text.extend(payload),
                attach::EXIT | attach::DETACHED | attach::ERROR => {
                    panic!("{kind} before {needle:?}: {}", lossy(&payload))
                }
                _ => {}
            }
            if lossy(&text).contains(needle) {
                return lossy(&text);
            }
        }
    }

    /// Everything until the relay closes: (OUTPUT bytes, the control frames in order).
    fn read_to_the_end(&mut self) -> (Vec<u8>, Vec<(u8, Value)>) {
        let (mut out, mut control) = (Vec::new(), Vec::new());
        while let Some((kind, _, payload)) = self.frame() {
            if kind == attach::OUTPUT {
                out.extend(payload);
            } else {
                control.push((
                    kind,
                    serde_json::from_slice(&payload).unwrap_or(Value::Null),
                ));
            }
        }
        (out, control)
    }
}

fn bytecount_x(b: &[u8]) -> usize {
    b.iter().fold(0, |n, c| n + usize::from(*c == b'x'))
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn held_of(open: &Value) -> HeldId {
    HeldId {
        instance: open["held_id"]["instance"].as_str().unwrap().to_owned(),
        id: u32::try_from(open["held_id"]["id"].as_u64().unwrap()).unwrap(),
    }
}

/// A fresh ticket for a terminal that is already open.
fn ticket(rig: &Rig, open: &Value, role: Role) -> String {
    rig.d
        .shared()
        .terminals
        .tickets
        .issue(
            role,
            held_of(open),
            open["session"].as_str().unwrap(),
            "kai",
        )
        .unwrap()
}

fn start(rig: &Rig, session: &str, script: &str) -> Value {
    let a = answered(open(&rig.m, session, &["sh", "-c", script], &plain()));
    assert_eq!(a["status"], "open", "{a}");
    a
}

fn listed(rig: &Rig) -> Vec<Value> {
    match crate::reach(
        &crate::tests::paths(&rig.m),
        "terminal.list",
        &json!({}),
        false,
    ) {
        crate::Reached::Answered(v) => v["terminals"].as_array().unwrap().clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_attach_gets_the_tail_then_live_output_and_its_keystrokes_reach_the_engine() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-echo", "echo before-attach; cat");
    // Let the first line be printed, so it is in the holder's tail and not in the live stream.
    std::thread::sleep(Duration::from_millis(500));
    let mut v = Viewer::connect(&rig.m);
    v.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    let (kind, attached) = v.answer();
    assert_eq!(kind, attach::ATTACHED, "{attached}");
    assert_eq!(
        (attached["v"].as_u64(), attached["role"].as_str()),
        (Some(1), Some("owner"))
    );
    assert_eq!(attached["held_id"]["id"], a["held_id"]["id"]);
    assert!(attached["replay"].as_u64().unwrap() >= 14, "{attached}");
    // The replay comes as OUTPUT right after ATTACHED, before anything live.
    let first = v.read_until("before-attach");
    assert_eq!(
        first.matches("before-attach").count(),
        1,
        "once: no duplicate between the replay and the live output: {first:?}"
    );
    v.send(attach::INPUT, b"typed-line\n");
    v.read_until("typed-line");
    // The list shows it with one owner attached.
    let rows = listed(&rig);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["session"], "s-echo");
    assert_eq!(rows[0]["agent"], "kai");
    assert_eq!(rows[0]["owners"], 1);
    assert_eq!(rows[0]["observers"], 0);
    assert_eq!(rows[0]["pid"], a["pid"]);
    assert_eq!(
        (rows[0]["cols"].as_u64(), rows[0]["rows"].as_u64()),
        (Some(80), Some(24))
    );
}

#[test]
fn detach_leaves_the_engine_running_and_a_new_ticket_gets_the_tail() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-detach", "echo marker-one; cat");
    let mut v = Viewer::connect(&rig.m);
    v.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(v.answer().0, attach::ATTACHED);
    v.read_until("marker-one");
    v.send(attach::INPUT, b"marker-two\n");
    v.read_until("marker-two");
    v.send(attach::DETACH, b"");
    let (_, control) = v.read_to_the_end();
    assert_eq!(control.last().unwrap().0, attach::DETACHED);
    assert_eq!(control.last().unwrap().1["why"], "detach");
    // The engine is still held, and nobody is attached.
    let rows = listed(&rig);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["owners"], 0);

    let mut again = Viewer::connect(&rig.m);
    again.attach(&ticket(&rig, &a, Role::Owner), "owner", &json!({}));
    let (kind, attached) = again.answer();
    assert_eq!(kind, attach::ATTACHED, "{attached}");
    let seen = again.read_until("marker-two");
    assert!(
        seen.contains("marker-one"),
        "the tail has all of it: {seen:?}"
    );
    // And it is live again.
    again.send(attach::INPUT, b"marker-three\n");
    again.read_until("marker-three");
}

#[test]
fn a_ticket_works_once_and_a_bad_attach_is_refused_with_the_reason() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-refuse", "echo up; cat");
    let t = a["ticket"].as_str().unwrap().to_owned();
    let mut first = Viewer::connect(&rig.m);
    first.attach(&t, "observer", &json!({}));
    assert_eq!(first.answer().0, attach::ATTACHED);

    // The same ticket again: refused, with no ATTACHED.
    let mut replay = Viewer::connect(&rig.m);
    replay.attach(&t, "observer", &json!({}));
    let (kind, err) = replay.answer();
    assert_eq!(
        (kind, err["error"].as_str()),
        (attach::ERROR, Some("ticket_unknown"))
    );
    assert!(replay.frame().is_none(), "then it closes");

    // An observer ticket cannot ask for owner.
    let mut greedy = Viewer::connect(&rig.m);
    greedy.attach(&ticket(&rig, &a, Role::Observer), "owner", &json!({}));
    let (kind, err) = greedy.answer();
    assert_eq!(
        (kind, err["error"].as_str()),
        (attach::ERROR, Some("role_not_allowed"))
    );

    // A version the relay does not speak does not use the ticket up.
    let fresh = ticket(&rig, &a, Role::Owner);
    let mut old = Viewer::connect(&rig.m);
    old.send(
        attach::ATTACH,
        json!({"v": 2, "ticket": fresh, "role": "owner"})
            .to_string()
            .as_bytes(),
    );
    let (kind, err) = old.answer();
    assert_eq!(
        (kind, err["error"].as_str()),
        (attach::ERROR, Some("unsupported_version"))
    );
    let mut retry = Viewer::connect(&rig.m);
    retry.attach(&fresh, "owner", &json!({}));
    assert_eq!(retry.answer().0, attach::ATTACHED);

    // A kind the relay does not know, first or later; ATTACH twice.
    let mut odd = Viewer::connect(&rig.m);
    odd.send(77, b"");
    let (kind, err) = odd.answer();
    assert_eq!(
        (kind, err["error"].as_str()),
        (attach::ERROR, Some("unknown_kind"))
    );
    let mut twice = Viewer::connect(&rig.m);
    twice.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(twice.answer().0, attach::ATTACHED);
    twice.attach("again", "observer", &json!({}));
    let (_, control) = twice.read_to_the_end();
    assert_eq!(control.last().unwrap().1["error"], "already_attached");
    let mut later = Viewer::connect(&rig.m);
    later.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(later.answer().0, attach::ATTACHED);
    later.send(55, b"");
    let (_, control) = later.read_to_the_end();
    assert_eq!(control.last().unwrap().1["error"], "unknown_kind");
}

#[test]
fn a_second_owner_is_refused_unless_it_takes_over() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-owner", "echo up; cat");
    let mut one = Viewer::connect(&rig.m);
    one.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(one.answer().0, attach::ATTACHED);
    one.read_until("up");

    let mut two = Viewer::connect(&rig.m);
    two.attach(&ticket(&rig, &a, Role::Owner), "owner", &json!({}));
    let (kind, err) = two.answer();
    assert_eq!(
        (kind, err["error"].as_str()),
        (attach::ERROR, Some("owner_present"))
    );

    // An observer may come beside the owner; it never counts as one.
    let mut watcher = Viewer::connect(&rig.m);
    watcher.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(watcher.answer().0, attach::ATTACHED);

    let mut taker = Viewer::connect(&rig.m);
    taker.attach(
        &ticket(&rig, &a, Role::Owner),
        "owner",
        &json!({"takeover": true}),
    );
    assert_eq!(taker.answer().0, attach::ATTACHED);
    let (_, control) = one.read_to_the_end();
    assert_eq!(control.last().unwrap().0, attach::DETACHED);
    assert_eq!(control.last().unwrap().1["why"], "taken_over");
    // The new owner types; the old one's keystrokes (if its socket were still open) would have been dropped.
    taker.send(attach::INPUT, b"from-new-owner\n");
    taker.read_until("from-new-owner");
    let rows = listed(&rig);
    assert_eq!(
        (rows[0]["owners"].as_u64(), rows[0]["observers"].as_u64()),
        (Some(1), Some(1))
    );

    // A takeover needs an owner ticket.
    let mut sneaky = Viewer::connect(&rig.m);
    sneaky.attach(
        &ticket(&rig, &a, Role::Observer),
        "owner",
        &json!({"takeover": true}),
    );
    assert_eq!(sneaky.answer().1["error"], "role_not_allowed");
}

const SIZES: &str =
    r#"trap 'echo "size:$(stty size)"' WINCH; echo up; while :; do sleep 0.1; done"#;

#[test]
fn an_observers_resize_and_input_are_ignored_and_the_owners_reach_the_terminal() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-size", SIZES);
    let mut owner = Viewer::connect(&rig.m);
    owner.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(owner.answer().0, attach::ATTACHED);
    owner.read_until("up");

    let mut watcher = Viewer::connect(&rig.m);
    watcher.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(watcher.answer().0, attach::ATTACHED);
    watcher.send(attach::RESIZE, br#"{"cols": 33, "rows": 11}"#);
    watcher.send(attach::INPUT, b"ignored\n");
    // Nothing happens to the terminal: no WINCH, no size line, no echo.
    assert!(
        owner.frame_within(Duration::from_millis(800)).is_none(),
        "an observer changed the terminal"
    );
    assert_eq!(
        (
            listed(&rig)[0]["cols"].as_u64(),
            listed(&rig)[0]["rows"].as_u64()
        ),
        (Some(80), Some(24))
    );

    owner.send(attach::RESIZE, br#"{"cols": 100, "rows": 40}"#);
    owner.read_until("size:40 100");
    assert_eq!(listed(&rig)[0]["cols"], 100);

    // An owner's ATTACH size counts too: the next owner's cols and rows are the terminal's.
    owner.send(attach::DETACH, b"");
    let _ = owner.read_to_the_end();
    let mut next = Viewer::connect(&rig.m);
    next.attach(
        &ticket(&rig, &a, Role::Owner),
        "owner",
        &json!({"cols": 90, "rows": 30}),
    );
    assert_eq!(next.answer().0, attach::ATTACHED);
    next.read_until("size:30 90");
}

/// 12 MB of output on a signal from the owner, a megabyte at a time (a reader that keeps up must never be cut, even on a
/// slow CI machine), then a marker.
const FLOOD: &str = r"echo up; read go; i=0; while [ $i -lt 12 ]; do head -c 1000000 /dev/zero | tr '\0' x; i=$((i+1)); sleep 0.1; done; echo; echo flood-done; cat";

#[test]
fn a_stalled_viewer_is_cut_while_the_engine_and_the_other_viewer_carry_on() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-slow", FLOOD);
    let mut owner = Viewer::connect(&rig.m);
    owner.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(owner.answer().0, attach::ATTACHED);
    owner.read_until("up");
    let mut stalled = Viewer::connect(&rig.m);
    stalled.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(stalled.answer().0, attach::ATTACHED);

    owner.send(attach::INPUT, b"go\n");
    // The owner reads on and gets every byte, through to the marker the engine prints last.
    let begun = Instant::now();
    let mut total = 0usize;
    let mut tail = String::new();
    loop {
        let (kind, _, payload) = owner.frame().expect("the owner keeps its connection");
        assert_eq!(kind, attach::OUTPUT);
        total += bytecount_x(&payload);
        tail = format!("{tail}{}", lossy(&payload));
        tail = tail[tail.len().saturating_sub(64)..].to_owned();
        if tail.contains("flood-done") {
            break;
        }
    }
    assert_eq!(total, 12_000_000, "no byte lost for the viewer that reads");
    assert!(begun.elapsed() < Duration::from_secs(60));

    // The viewer that did not read now reads what it was sent, and the last thing it hears is why it was cut.
    let (out, control) = stalled.read_to_the_end();
    assert!(!out.is_empty() && out.len() < 12_000_000, "{}", out.len());
    let (kind, why) = control.last().unwrap();
    assert_eq!(
        (*kind, why["why"].as_str()),
        (attach::DETACHED, Some("slow"))
    );
    // It may attach again with a new ticket, and gets the tail.
    let mut again = Viewer::connect(&rig.m);
    again.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(again.answer().0, attach::ATTACHED);
    again.read_until("flood-done");
    // The engine was never held up: it is alive and answers.
    owner.send(attach::INPUT, b"still-here\n");
    owner.read_until("still-here");
}

#[test]
fn the_end_reaches_every_viewer_then_is_acked_and_a_late_attach_is_told_how_it_ended() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-exit", "echo up; read go; echo bye-bye; exit 3");
    let mut owner = Viewer::connect(&rig.m);
    owner.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(owner.answer().0, attach::ATTACHED);
    owner.read_until("up");
    let mut watcher = Viewer::connect(&rig.m);
    watcher.attach(&ticket(&rig, &a, Role::Observer), "observer", &json!({}));
    assert_eq!(watcher.answer().0, attach::ATTACHED);

    owner.send(attach::INPUT, b"go\n");
    for viewer in [&mut owner, &mut watcher] {
        let (out, control) = viewer.read_to_the_end();
        assert!(lossy(&out).contains("bye-bye"), "{:?}", lossy(&out));
        let (kind, exit) = control.last().unwrap();
        assert_eq!(*kind, attach::EXIT, "{control:?}");
        assert_eq!(exit["code"], 3);
    }

    // The holder forgot it: a new link to it lists nothing unacked. (It supersedes the core's link; the rig ends it.)
    let layout = Layout::new(&rig.m);
    let secret = crate::holder_link::secret(&layout).unwrap();
    let generation = crate::reach(
        &crate::tests::paths(&rig.m),
        "kernel.coreStatus",
        &json!({}),
        true,
    );
    let generation = match generation {
        crate::Reached::Answered(v) => v["daemon"]["generation"].as_u64().unwrap(),
        other => panic!("{other:?}"),
    };
    until("the exit to be acked", || {
        kith_holder::client::Client::connect(&layout.holder_dir, &secret, generation + 1)
            .is_ok_and(|c| c.welcome.procs.is_empty())
    });

    // A ticket redeemed after the end: ATTACHED, what it printed, EXIT, closed.
    let mut late = Viewer::connect(&rig.m);
    late.attach(&ticket(&rig, &a, Role::Owner), "owner", &json!({}));
    let (kind, attached) = late.answer();
    assert_eq!(kind, attach::ATTACHED, "{attached}");
    let (out, control) = late.read_to_the_end();
    assert!(lossy(&out).contains("bye-bye"));
    assert_eq!(attached["replay"].as_u64().unwrap(), out.len() as u64);
    assert_eq!(control.last().unwrap().0, attach::EXIT);
    assert_eq!(control.last().unwrap().1["code"], 3);
}

fn attach_call(rig: &Rig, params: &Value) -> Value {
    match crate::reach(
        &crate::tests::paths(&rig.m),
        "terminal.attach",
        params,
        false,
    ) {
        crate::Reached::Answered(v) => v,
        other => panic!("{other:?}"),
    }
}

#[test]
fn terminal_attach_mints_a_ticket_to_come_back_with_after_a_detach_or_a_cut() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-back", "echo first-words; cat");
    let mut v = Viewer::connect(&rig.m);
    v.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(v.answer().0, attach::ATTACHED);
    v.read_until("first-words");
    v.send(attach::DETACH, b"");
    let _ = v.read_to_the_end();

    // By session, and by held id; the ticket is single-use like the first.
    let by_session = attach_call(&rig, &json!({"session": "s-back"}));
    assert_eq!(by_session["status"], "open", "{by_session}");
    assert_eq!(by_session["held_id"], a["held_id"]);
    let by_held = attach_call(&rig, &json!({"heldId": a["held_id"], "role": "observer"}));
    assert_eq!(by_held["status"], "open", "{by_held}");
    let mut back = Viewer::connect(&rig.m);
    back.attach(by_session["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(back.answer().0, attach::ATTACHED);
    back.read_until("first-words");
    // The observer ticket cannot be an owner's.
    let mut greedy = Viewer::connect(&rig.m);
    greedy.attach(by_held["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(greedy.answer().1["error"], "role_not_allowed");
    // Nothing to attach to: gone, and nothing minted.
    assert_eq!(
        attach_call(&rig, &json!({"session": "nope"}))["status"],
        "gone"
    );
    assert_eq!(
        attach_call(&rig, &json!({"heldId": {"instance": "old", "id": 1}}))["status"],
        "gone"
    );
}

#[test]
fn a_refusal_that_needs_no_ticket_leaves_the_ticket_to_try_again() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-keep", "echo up; cat");
    let mut one = Viewer::connect(&rig.m);
    one.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(one.answer().0, attach::ATTACHED);

    let spare = ticket(&rig, &a, Role::Owner);
    let mut two = Viewer::connect(&rig.m);
    two.attach(&spare, "owner", &json!({}));
    assert_eq!(two.answer().1["error"], "owner_present");
    // The owner leaves; the same ticket now works.
    one.send(attach::DETACH, b"");
    let _ = one.read_to_the_end();
    let mut again = Viewer::connect(&rig.m);
    again.attach(&spare, "owner", &json!({}));
    assert_eq!(again.answer().0, attach::ATTACHED);
}

#[test]
fn a_ticket_from_another_holder_instance_never_reaches_this_ones_process() {
    let rig = Rig::new(RUNNING);
    let a = start(&rig, "s-stale", "echo up; cat");
    // Same process id, an older instance: what a ticket from before a holder restart looks like.
    let stale = HeldId {
        instance: "an-older-holder".into(),
        id: held_of(&a).id,
    };
    let t = rig
        .d
        .shared()
        .terminals
        .tickets
        .issue(Role::Owner, stale, "s-stale", "kai")
        .unwrap();
    let mut v = Viewer::connect(&rig.m);
    v.attach(&t, "owner", &json!({}));
    assert_eq!(v.answer().1["error"], "terminal_gone");
}

#[test]
fn a_burst_does_not_cut_an_owner_that_keeps_reading() {
    let rig = Rig::new(RUNNING);
    // Three megabytes at once: more than an observer's buffer, less than the owner's.
    let a = start(
        &rig,
        "s-burst",
        r"echo up; read go; head -c 3000000 /dev/zero | tr '\0' x; echo; echo burst-done; cat",
    );
    let mut owner = Viewer::connect(&rig.m);
    owner.attach(a["ticket"].as_str().unwrap(), "owner", &json!({}));
    assert_eq!(owner.answer().0, attach::ATTACHED);
    owner.read_until("up");
    owner.send(attach::INPUT, b"go\n");
    owner.read_until("burst-done");
}
