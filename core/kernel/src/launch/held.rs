//! `kith AGENT --held`: the same launch, handed to the core to start in the holder (`terminal.open`) and attached to, in this
//! process (the port of `kith_held_launch` and `kith_held_attach` of `kernel/lib/kith-core.sh`; thread-writers model I7).
//!
//! The order is the script's: the resumed session's claim is given back, then `terminal.open` takes it itself. Busy is exit 75.
//! Exit 5 from the call (lost after it went out) means "may have started": it says so and is never repeated. A holder that cannot be
//! reached, or that answers "run it here", falls back to a direct run with one line ([`Outcome::Direct`]).
//!
//! What the kernel crate does not have is the core's client and the attach client (they live in the host program): the caller gives
//! them as a [`Port`].

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::assemble::Env;
use super::session::lockable;
use crate::threads::run::say;
use crate::threads::writer::{Taker, claim_exit};

/// What a call of the core came to (`kith kernel core-call`'s ends).
pub enum Call {
    Answered(Value),
    /// The core said no (core-call's exit 4): nothing ran.
    Refused(String),
    /// No answer. 3: no daemon, nothing ran; 5: lost after the call went out, it may have run.
    NoAnswer(String, i32),
}

/// The core's client and the attach client.
pub trait Port {
    /// One owner-socket call of a `terminal.*` method.
    fn call(&self, method: &str, params: &Value) -> Call;
    /// Attaches once to the relay at `endpoint` with `ticket`: the client's exit code, and the notes it wrote (held back, so the
    /// caller can read them before saying anything).
    fn attach(
        &self,
        endpoint: &Path,
        ticket: &str,
        observer: bool,
        takeover: bool,
    ) -> (i32, String);
}

/// What the held launch came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The run is over (opened, busy, or the call may have run): end with this code.
    Exit(i32),
    /// Run it here (`--direct`); the reason is said already. `alone`: the claim was taken again from a core that gave no answer, so
    /// the lock alone holds it (the stub gets `--lock-alone`).
    Direct { alone: bool },
}

/// A held launch of `agent`'s session `sid`.
pub struct Held<'a> {
    pub agent: &'a str,
    pub sid: &'a str,
    /// The resumed session whose claim this process holds ("" for none).
    pub rsid: &'a str,
    /// Claude's arguments (without `claude` itself).
    pub args: &'a [String],
    /// The run's environment: what the held terminal starts with, less the window and the run's tokens.
    pub env: &'a Env,
    pub taker: &'a Taker<'a>,
    pub port: &'a dyn Port,
    pub pid: i64,
    /// `<machine>/core/attach.sock`.
    pub endpoint: PathBuf,
    /// Stdin and stdout are terminals.
    pub tty: bool,
}

/// What a terminal that outlives its window and the run must not carry: the window, and the run's tokens (the core drops the token
/// names too; both spellings, `KITH_` and `HQ_`).
pub fn held_env(env: &Env) -> serde_json::Map<String, Value> {
    const WINDOW: [&str; 8] = [
        "TERM_SESSION_ID",
        "ITERM_SESSION_ID",
        "TMUX",
        "TMUX_PANE",
        "SSH_TTY",
        "WINDOWID",
        "COLUMNS",
        "LINES",
    ];
    const TOKENS: [&str; 5] = [
        "RUN_TOKEN",
        "RUN_SOCKET",
        "CORE_RUN_SOCKET",
        "ASK_TOKEN",
        "ASK_SOCKET",
    ];
    let dropped = |k: &str| {
        WINDOW.contains(&k)
            || k == "SHLVL"
            || k == "_"
            || ["KITH_", "HQ_"].iter().any(|p| {
                k.strip_prefix(p).is_some_and(|n| {
                    TOKENS.contains(&n) || n.starts_with("RUN_") || n.starts_with("CALL_")
                })
            })
    };
    env.iter()
        .filter(|(k, _)| !dropped(k))
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect()
}

/// The terminal's size, from stderr, stdin or stdout, else the controlling terminal (`os.get_terminal_size` in the order the script
/// asked): `(cols, rows)`.
#[cfg(unix)]
#[allow(unsafe_code)]
fn terminal_size() -> Option<(u16, u16)> {
    use std::os::fd::AsRawFd as _;
    fn of(fd: i32) -> Option<(u16, u16)> {
        // SAFETY: TIOCGWINSZ writes a `winsize` and nothing else; `fd` is only read.
        let mut w: libc::winsize = unsafe { std::mem::zeroed() };
        // `ioctl`'s request argument is a different integer type on some targets: the cast is the libc crate's own advice.
        #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &raw mut w) };
        (rc == 0).then_some((w.ws_col, w.ws_row))
    }
    for fd in [2, 0, 1] {
        if let Some(s) = of(fd) {
            return Some(s);
        }
    }
    let tty = std::fs::File::open("/dev/tty").ok()?;
    of(tty.as_raw_fd())
}

#[cfg(not(unix))]
fn terminal_size() -> Option<(u16, u16)> {
    None
}

/// The `terminal.open` params: the window and the run's tokens are not passed to a terminal that outlives both.
fn open_params(h: &Held<'_>) -> Value {
    let mut argv = vec![json!("claude")];
    argv.extend(h.args.iter().map(|a| json!(a)));
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut p = json!({
        "agent": h.agent,
        "session": h.sid,
        "argv": argv,
        "cwd": cwd.to_string_lossy(),
        "env": held_env(h.env),
    });
    if let Some((cols, rows)) = terminal_size() {
        p["cols"] = json!(cols);
        p["rows"] = json!(rows);
    }
    p
}

/// A field of the answer: a string as it is, anything else as JSON, "" when absent.
fn field(ans: &Value, key: &str) -> String {
    match ans.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// `kith_held_call`: the call's answer, or core-call's exit code. A refusal and a lost call say why (`kith: ...`); "no daemon" is the
/// caller's one line.
fn call(h: &Held<'_>, method: &str, params: &Value) -> Result<Value, i32> {
    let said = |why: &str| {
        for l in why.lines() {
            say(&format!("kith: {l}"));
        }
    };
    match h.port.call(method, params) {
        Call::Answered(v) => Ok(v),
        Call::Refused(why) => {
            said(&why);
            Err(4)
        }
        Call::NoAnswer(why, code) => {
            if code == 5 {
                said(&why);
            }
            Err(code)
        }
    }
}

/// `kith_held_attach` for a ticket in hand: attaches; a client cut `slow` (76) is attached once more with a new ticket. Returns the
/// client's exit code and its last note.
fn attach(h: &Held<'_>, ticket: &str) -> (i32, String) {
    let mut ticket = ticket.to_owned();
    let mut again = false;
    loop {
        if ticket.is_empty() {
            let Ok(ans) = call(
                h,
                "terminal.attach",
                &json!({"session": h.sid, "role": "owner"}),
            ) else {
                say("kith: the core is not answering, so there is nothing to attach to");
                return (1, String::new());
            };
            if field(&ans, "status") != "open" {
                say(&format!(
                    "kith: nothing held for {} (it ended, or was never held here)",
                    h.sid
                ));
                return (1, String::new());
            }
            ticket = field(&ans, "ticket");
        }
        let (rc, note) = h.port.attach(&h.endpoint, &ticket, false, false);
        let note = note.trim_end().to_owned();
        if !note.is_empty() {
            say(&note);
        }
        ticket.clear();
        // 76: the relay cut this client as too slow (the client's own code for it; 77 taken over, 78 holder lost).
        if rc == 76 && !again {
            again = true;
            continue;
        }
        if note.contains("owner_present") {
            say(&format!(
                "kith: another window is attached to {} and types in it; kith attach --takeover {} takes over, --observer only watches",
                h.sid, h.sid
            ));
        }
        return (rc, note);
    }
}

/// Runs the script's `kith_held_launch`.
#[allow(clippy::if_not_else)] // the script's own order: the lock key first, the rest in the else
pub fn launch(h: &Held<'_>) -> Outcome {
    let sid = h.sid;
    let mut why = String::new();
    let mut released = false;
    if !lockable(sid) {
        why = "this conversation has no id the lock can use".to_owned();
    } else {
        // A --resume took the session's claim for this process in `resolve`; terminal.open takes it itself, so it is given back
        // first, and only when a holder is up: with no daemon or no holder (the common fallback) the claim is never let go.
        if !h.rsid.is_empty() {
            match call(h, "terminal.list", &json!({})) {
                Ok(ans) if field(&ans, "link") == "up" => {
                    if h.taker.release_for(h.rsid, h.pid) {
                        released = true;
                    } else {
                        why = "the session's claim is not this shell's to hand over".to_owned();
                    }
                }
                _ => why = "holder not reachable".to_owned(),
            }
            gap_hook(h, released);
        }
        let mut ans = Value::Null;
        if why.is_empty() {
            match call(h, "terminal.open", &open_params(h)) {
                Ok(a) => ans = a,
                Err(5) => {
                    // Not repeated: the call may have started it.
                    say(&format!(
                        "kith: the call may have started it; kith attach {sid} joins it if it is held"
                    ));
                    return Outcome::Exit(1);
                }
                // The refusal was said.
                Err(4) => why = "the core refused it".to_owned(),
                Err(_) => why = "holder not reachable".to_owned(),
            }
        }
        if why.is_empty() {
            let status = field(&ans, "status");
            let note = field(&ans, "note");
            match status.as_str() {
                "busy" => {
                    let note = if note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({note})")
                    };
                    say(&format!(
                        "kith: that conversation is already open in another writer{note}; kith attach {sid} joins it if it is held"
                    ));
                    return Outcome::Exit(75);
                }
                "open" => return opened(h, &ans),
                _ => {
                    let n = note.split(": use --direct").next().unwrap_or("");
                    why = if n.is_empty() {
                        "holder not reachable"
                    } else {
                        n
                    }
                    .to_owned();
                }
            }
        }
    }
    if h.tty {
        say(&format!("kith: {why}; running here (--direct)"));
    } else {
        say(&format!(
            "kith: {why}; an interactive session needs a terminal to run here"
        ));
    }
    let mut alone = false;
    if released {
        // The claim again; a core that gave no answer marks the run lock-alone (the stub gets --lock-alone).
        let claimed = h.taker.claim_start(
            h.rsid,
            h.pid,
            "terminal",
            std::time::Duration::ZERO,
            None,
            None,
        );
        eprint!("{}", h.taker.said());
        if let Err(m) = claimed {
            say(&format!("kith: {m}"));
            return Outcome::Exit(claim_exit(&m));
        }
        alone = h.taker.alone();
    }
    Outcome::Direct { alone }
}

/// terminal.open said `open`: attach, or (no terminal on this side) leave it held and headless.
fn opened(h: &Held<'_>, ans: &Value) -> Outcome {
    let (agent, sid) = (h.agent, h.sid);
    let ticket = field(ans, "ticket");
    if !h.tty {
        // No terminal on this side: the engine is held and runs with nobody attached (no raw mode, nothing read).
        println!("kith: {agent} is held as session {sid}; attach with: kith attach {sid}");
        return Outcome::Exit(0);
    }
    let (rc, note) = attach(h, &ticket);
    // The way back, unless the engine itself ended: a detach, or the attach failing while the terminal runs on.
    if note.contains("keeps running") || (rc != 0 && rc != 75 && rc < 79 && !note.is_empty()) {
        say(&format!("kith: reattach with: kith attach {sid}"));
    }
    Outcome::Exit(rc)
}

/// Tests only: something that happens in the gap between the give-back and open (a third writer).
#[cfg(unix)]
fn gap_hook(h: &Held<'_>, released: bool) {
    let hook = h.env.get("KITH_HELD_GAP_HOOK").cloned().unwrap_or_default();
    if hook.is_empty() || !released {
        return;
    }
    let env: Vec<(&String, &String)> = h.env.iter().collect();
    let _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(hook)
        .env_clear()
        .envs(env)
        .status();
}

#[cfg(not(unix))]
fn gap_hook(_: &Held<'_>, _: bool) {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::threads::Ctx;

    #[test]
    fn a_terminal_that_outlives_the_run_carries_no_window_and_no_token() {
        let mut env = Env::new();
        for (k, v) in [
            ("HOME", "/h"),
            ("TMUX", "x"),
            ("COLUMNS", "80"),
            ("SHLVL", "2"),
            ("_", "/bin/kith"),
            ("KITH_RUN_TOKEN", "t"),
            ("HQ_RUN_SOCKET", "s"),
            ("KITH_CORE_RUN_SOCKET", "s"),
            ("KITH_ASK_TOKEN", "a"),
            ("KITH_RUN_OUTBOX", "o"),
            ("KITH_CALL_KEY", "k"),
            ("HQ_CALL_CHAT", "c"),
            ("KITH_AGENT", "designer"),
            ("KITH_TERMINAL_SESSION", "1"),
        ] {
            env.insert(k.to_owned(), v.to_owned());
        }
        let mut kept: Vec<String> = held_env(&env).keys().cloned().collect();
        kept.sort();
        assert_eq!(kept, ["HOME", "KITH_AGENT", "KITH_TERMINAL_SESSION"]);
    }

    /// A core that answers from a list and notes what it was asked.
    struct Fake {
        answers: std::cell::RefCell<Vec<Call>>,
        asked: std::cell::RefCell<Vec<String>>,
        attach_code: i32,
    }

    impl Port for Fake {
        fn call(&self, method: &str, _: &Value) -> Call {
            self.asked.borrow_mut().push(method.to_owned());
            self.answers.borrow_mut().remove(0)
        }
        fn attach(&self, _: &Path, _: &str, _: bool, _: bool) -> (i32, String) {
            (self.attach_code, String::new())
        }
    }

    fn launched(sid: &str, tty: bool, answers: Vec<Call>) -> (Outcome, Vec<String>) {
        let none = |_: &str| -> Option<String> { None };
        let ctx = Ctx {
            state: None,
            machine: None,
            home: PathBuf::new(),
            env: &none,
        };
        let taker = Taker::new(&ctx, None);
        let port = Fake {
            answers: std::cell::RefCell::new(answers),
            asked: std::cell::RefCell::new(vec![]),
            attach_code: 7,
        };
        let env = Env::new();
        let h = Held {
            agent: "designer",
            sid,
            rsid: "",
            args: &[],
            env: &env,
            taker: &taker,
            port: &port,
            pid: 1,
            endpoint: PathBuf::from("/x/attach.sock"),
            tty,
        };
        let out = launch(&h);
        (out, port.asked.take())
    }

    const SID: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn busy_is_75_and_nothing_else_is_asked() {
        let busy = Call::Answered(json!({"status": "busy", "note": "held"}));
        let (out, asked) = launched(SID, true, vec![busy]);
        assert_eq!(out, Outcome::Exit(75));
        assert_eq!(asked, ["terminal.open"]);
    }

    #[test]
    fn a_call_lost_after_it_went_out_is_never_repeated() {
        let (out, asked) = launched(SID, true, vec![Call::NoAnswer("lost".to_owned(), 5)]);
        assert_eq!(out, Outcome::Exit(1));
        assert_eq!(asked, ["terminal.open"], "one call, no second try");
    }

    #[test]
    fn no_holder_or_a_refusal_runs_it_here() {
        for answer in [
            Call::NoAnswer("no daemon".to_owned(), 3),
            Call::Refused("no".to_owned()),
            Call::Answered(json!({"status": "direct", "note": "no stub: use --direct"})),
        ] {
            let (out, _) = launched(SID, true, vec![answer]);
            assert_eq!(out, Outcome::Direct { alone: false });
        }
    }

    #[test]
    fn a_session_the_lock_cannot_key_is_not_asked_of_the_core() {
        let (out, asked) = launched("", true, vec![]);
        assert_eq!(out, Outcome::Direct { alone: false });
        assert_eq!(asked.len(), 0);
    }

    #[test]
    fn opened_without_a_terminal_stays_held_and_with_one_ends_with_the_clients_code() {
        let open = || Call::Answered(json!({"status": "open", "ticket": "t"}));
        assert_eq!(launched(SID, false, vec![open()]).0, Outcome::Exit(0));
        let (out, asked) = launched(SID, true, vec![open()]);
        assert_eq!(out, Outcome::Exit(7));
        assert_eq!(
            asked,
            ["terminal.open"],
            "a ticket in hand needs no attach call"
        );
    }

    #[test]
    fn the_resumed_claim_is_given_back_by_this_process_before_open() {
        let dir = std::env::temp_dir().join(format!("kith-held-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let none = |_: &str| -> Option<String> { None };
        let ctx = Ctx {
            state: Some(dir.clone()),
            machine: None,
            home: dir.clone(),
            env: &none,
        };
        let taker = Taker::new(&ctx, None);
        let pid = i64::from(std::process::id());
        taker
            .claim_start(SID, pid, "terminal", std::time::Duration::ZERO, None, None)
            .unwrap();
        let claim = dir.join("live/terminal").join(format!("{SID}.claim"));
        assert!(claim.exists(), "the resume took the claim for this process");
        let port = Fake {
            answers: std::cell::RefCell::new(vec![
                Call::Answered(json!({"link": "up"})),
                Call::Answered(json!({"status": "busy"})),
            ]),
            asked: std::cell::RefCell::new(vec![]),
            attach_code: 0,
        };
        let env = Env::new();
        let h = Held {
            agent: "designer",
            sid: SID,
            rsid: SID,
            args: &[],
            env: &env,
            taker: &taker,
            port: &port,
            pid,
            endpoint: PathBuf::new(),
            tty: true,
        };
        assert_eq!(launch(&h), Outcome::Exit(75));
        assert_eq!(port.asked.take(), ["terminal.list", "terminal.open"]);
        assert!(!claim.exists(), "given back before terminal.open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_field_reads_as_the_script_prints_it() {
        let a = json!({"status": "open", "n": 3, "none": null});
        assert_eq!(field(&a, "status"), "open");
        assert_eq!(field(&a, "n"), "3");
        assert_eq!(field(&a, "none"), "");
        assert_eq!(field(&a, "missing"), "");
    }
}
