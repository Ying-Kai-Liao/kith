//! The client against a fake relay that speaks the spec (terminal.rs "Attach frames") over a socket pair.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::*;

type Size = Arc<Mutex<Option<(u16, u16)>>>;

struct Keys(Receiver<Vec<u8>>);

impl Read for Keys {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.recv() {
            Ok(b) => {
                let n = b.len().min(buf.len());
                buf[..n].copy_from_slice(&b[..n]);
                Ok(n)
            }
            Err(_) => Ok(0),
        }
    }
}

struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Restores(Arc<Mutex<Vec<&'static str>>>);

impl Drop for Restores {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("restored");
    }
}

struct Rig {
    relay: UnixStream,
    keys: Sender<Vec<u8>>,
    out: Arc<Mutex<Vec<u8>>>,
    notes: Arc<Mutex<Vec<u8>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    size: Size,
    client: Option<JoinHandle<i32>>,
}

fn opts(role: Role) -> Opts {
    Opts { ticket: "t0".into(), role, takeover: false, detach_key: 0x1c, tty_input: false }
}

fn rig(o: Opts) -> Rig {
    let (client_side, relay) = UnixStream::pair().unwrap();
    let (keys, rx) = channel();
    let out = Arc::new(Mutex::new(Vec::new()));
    let notes = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let size: Size = Arc::new(Mutex::new(Some((80, 24))));
    let io = Io {
        input: Box::new(Keys(rx)),
        output: Box::new(Shared(out.clone())),
        notes: Box::new(Shared(notes.clone())),
        size: {
            let s = size.clone();
            Arc::new(move || *s.lock().unwrap())
        },
        raw: {
            let ev = events.clone();
            Box::new(move || {
                ev.lock().unwrap().push("raw");
                Some(Box::new(Restores(ev)) as Box<dyn Send>)
            })
        },
    };
    let client = thread::spawn(move || session(client_side, io, &o));
    Rig { relay, keys, out, notes, events, size, client: Some(client) }
}

impl Rig {
    fn frame(&mut self) -> (u8, Vec<u8>) {
        self.relay.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let (k, id, p) = wire::read(&mut self.relay).unwrap().expect("a frame");
        assert_eq!(id, 0);
        (k, p)
    }

    fn attach_frame(&mut self) -> Value {
        let (k, p) = self.frame();
        assert_eq!(k, kind::ATTACH);
        serde_json::from_slice(&p).unwrap()
    }

    fn send(&mut self, k: u8, p: &[u8]) {
        wire::write_frame(&mut self.relay, k, 0, p).unwrap();
    }

    fn attached(&mut self) {
        self.send(kind::ATTACHED, br#"{"v":1,"held_id":7,"role":"owner","cols":80,"rows":24,"replay":0}"#);
    }

    fn finish(&mut self) -> i32 {
        self.client.take().unwrap().join().unwrap()
    }

    fn note(&self) -> String {
        String::from_utf8(self.notes.lock().unwrap().clone()).unwrap()
    }
}

#[test]
fn attach_frame_carries_ticket_role_size_and_takeover() {
    let mut r = rig(Opts { takeover: true, ..opts(Role::Owner) });
    let a = r.attach_frame();
    assert_eq!(a, json!({"v": 1, "ticket": "t0", "role": "owner", "cols": 80, "rows": 24, "takeover": true}));
    r.send(kind::EXIT, br#"{"code":0,"signal":null}"#);
    assert_eq!(r.finish(), 0);
}

#[test]
fn replay_then_live_output_then_exit_code_passes_through() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::OUTPUT, b"tail ");
    // A kind this client does not know is a later addition: ignored.
    r.send(150, b"{}");
    r.send(kind::OUTPUT, b"live");
    r.send(kind::EXIT, br#"{"code":3,"signal":null}"#);
    assert_eq!(r.finish(), 3);
    assert_eq!(r.out.lock().unwrap().as_slice(), b"tail live");
    // Raw mode went on after ATTACHED and was restored before the client returned.
    assert_eq!(*r.events.lock().unwrap(), ["raw", "restored"]);
    assert_eq!(r.note(), "");
}

#[test]
fn a_signal_exit_is_128_plus_the_signal() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::EXIT, br#"{"code":null,"signal":9}"#);
    assert_eq!(r.finish(), 137);
}

#[test]
fn exit_in_the_middle_of_the_replay_ends_it_there() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::OUTPUT, b"part");
    r.send(kind::EXIT, br#"{"code":0,"signal":null,"error":"killed by the holder"}"#);
    r.send(kind::OUTPUT, b"never read");
    assert_eq!(r.finish(), 0);
    assert_eq!(r.out.lock().unwrap().as_slice(), b"part");
    assert_eq!(r.note(), "error: killed by the holder\n");
}

#[test]
fn code_75_right_after_attached_is_busy_and_says_so() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::EXIT, br#"{"code":75,"signal":null}"#);
    assert_eq!(r.finish(), 75);
    assert_eq!(r.note(), "busy: another writer has this conversation\n");
}

#[test]
fn code_75_after_output_is_just_the_engines_code() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::OUTPUT, b"x");
    r.send(kind::EXIT, br#"{"code":75,"signal":null}"#);
    assert_eq!(r.finish(), 75);
    assert_eq!(r.note(), "");
}

#[test]
fn detached_says_why_in_one_line() {
    for (why, code, text) in [
        ("detach", 0, "detached; the agent keeps running"),
        ("taken_over", 1, "detached: another client took over this terminal"),
        ("slow", 1, "detached: this client could not keep up with the output; attach again"),
        ("holder_lost", 1, "detached: the process holder is gone"),
        ("refused", 1, "detached: the relay refused this attach"),
        ("later_addition", 1, "detached: later_addition"),
    ] {
        let mut r = rig(opts(Role::Owner));
        r.attach_frame();
        r.attached();
        r.send(kind::DETACHED, format!(r#"{{"why":"{why}"}}"#).as_bytes());
        assert_eq!(r.finish(), code, "{why}");
        assert_eq!(r.note(), format!("{text}\n"), "{why}");
        assert_eq!(*r.events.lock().unwrap(), ["raw", "restored"], "{why}");
    }
}

#[test]
fn a_refused_ticket_prints_the_error_and_never_goes_raw() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.send(kind::ERROR, br#"{"error":"ticket_invalid"}"#);
    assert_eq!(r.finish(), 1);
    assert_eq!(r.note(), "error: ticket_invalid\n");
    assert!(r.events.lock().unwrap().is_empty());
}

#[test]
fn a_relay_that_closes_without_a_frame_is_reported() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.relay.shutdown(std::net::Shutdown::Both).unwrap();
    assert_eq!(r.finish(), 1);
    assert_eq!(r.note(), "the terminal relay closed the connection without an answer\n");
}

#[test]
fn a_dropped_connection_after_attached_restores_the_terminal() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.send(kind::OUTPUT, b"hi");
    r.relay.shutdown(std::net::Shutdown::Both).unwrap();
    assert_eq!(r.finish(), 1);
    assert_eq!(r.note(), "the connection to the terminal closed\n");
    assert_eq!(*r.events.lock().unwrap(), ["raw", "restored"]);
}

#[test]
fn output_before_attached_is_a_protocol_error() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.send(kind::OUTPUT, b"early");
    assert_eq!(r.finish(), 1);
    assert!(r.out.lock().unwrap().is_empty());
}

#[test]
fn a_full_size_payload_is_accepted() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    let big = vec![b'z'; 64 * 1024];
    r.send(kind::OUTPUT, &big);
    r.send(kind::EXIT, br#"{"code":0,"signal":null}"#);
    assert_eq!(r.finish(), 0);
    assert_eq!(r.out.lock().unwrap().len(), 64 * 1024);
}

#[test]
fn keys_go_as_input_and_the_detach_key_sends_detach_and_is_not_forwarded() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    r.keys.send(b"ls\r".to_vec()).unwrap();
    assert_eq!(r.frame(), (kind::INPUT, b"ls\r".to_vec()));
    // Typed ahead of the key, the key itself, and what follows it in the same read.
    r.keys.send(b"ab\x1ccd".to_vec()).unwrap();
    assert_eq!(r.frame(), (kind::INPUT, b"ab".to_vec()));
    assert_eq!(r.frame(), (kind::DETACH, Vec::new()));
    // A second press changes nothing: the input thread is done.
    let _ = r.keys.send(vec![0x1c]);
    assert_eq!(r.finish(), 0);
    assert_eq!(r.note(), "detached; the agent keeps running\n");
    assert_eq!(*r.events.lock().unwrap(), ["raw", "restored"]);
    // Nothing else came: the relay sees the connection close.
    assert!(wire::read(&mut r.relay).unwrap().is_none());
}

#[test]
fn a_configured_detach_key_is_used() {
    let mut r = rig(Opts { detach_key: parse_detach_key("ctrl-a").unwrap(), ..opts(Role::Owner) });
    r.attach_frame();
    r.attached();
    // Ctrl+\ is now an ordinary key.
    r.keys.send(vec![0x1c]).unwrap();
    assert_eq!(r.frame(), (kind::INPUT, vec![0x1c]));
    r.keys.send(vec![0x01]).unwrap();
    assert_eq!(r.frame(), (kind::DETACH, Vec::new()));
    assert_eq!(r.finish(), 0);
}

#[test]
fn a_resize_sends_resize_once_for_a_storm() {
    let mut r = rig(opts(Role::Owner));
    r.attach_frame();
    r.attached();
    for n in 90..100 {
        *r.size.lock().unwrap() = Some((n, 30));
    }
    let (k, p) = r.frame();
    assert_eq!(k, kind::RESIZE);
    let v: Value = serde_json::from_slice(&p).unwrap();
    assert_eq!(v["rows"], 30);
    assert!(v["cols"].as_u64().unwrap() >= 90);
    r.send(kind::EXIT, br#"{"code":0,"signal":null}"#);
    assert_eq!(r.finish(), 0);
}

#[test]
fn an_observer_sends_no_input_and_no_resize_but_can_detach() {
    let mut r = rig(opts(Role::Observer));
    let a = r.attach_frame();
    assert_eq!(a["role"], "observer");
    r.attached();
    r.keys.send(b"typed".to_vec()).unwrap();
    *r.size.lock().unwrap() = Some((100, 40));
    thread::sleep(RESIZE_POLL * 4);
    r.keys.send(vec![0x1c]).unwrap();
    // The first frame the relay gets after ATTACH is the DETACH: no INPUT, no RESIZE before it.
    assert_eq!(r.frame(), (kind::DETACH, Vec::new()));
    assert_eq!(r.finish(), 0);
}

#[test]
fn detach_key_parsing() {
    assert_eq!(parse_detach_key("ctrl-\\").unwrap(), 0x1c);
    assert_eq!(parse_detach_key("ctrl-a").unwrap(), 1);
    assert_eq!(parse_detach_key("Ctrl-]").unwrap(), 0x1d);
    assert_eq!(parse_detach_key("^_").unwrap(), 0x1f);
    for bad in ["", "ctrl-", "ctrl-ab", "a", "ctrl-1", "ctrl-m", "ctrl-i", "ctrl-@"] {
        assert!(parse_detach_key(bad).is_err(), "{bad}");
    }
}
