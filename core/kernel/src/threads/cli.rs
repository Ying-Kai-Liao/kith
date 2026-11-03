//! `kith kernel threads <verb> ...`: the verbs of `kernel/bin/kith-sessions` that touch the writer claim and the registry,
//! with its arguments, output and exit codes (`sys.exit("text")` is the text on stderr and exit 1):
//!
//! ```text
//! record                                   SessionStart hook; stdin is the hook's JSON; always exit 0
//! resolve AGENT THREAD [PID [oneshot]]     shell assignments r_sid, r_cwd, r_from; takes the claim for PID (default: our parent)
//! claim SID PID [PERSON [AGENT]]           the one-shot's claim, waiting KITH_CLAIM_WAIT s while another writer holds it
//! terminal SID PID [PERSON [AGENT]]        an interactive run's claim, refused at once when held
//! proc PID PERSON AGENT                    the session-less claim the revoke sweep finds
//! sweep PERSON...                          `swept N`: ends the live holders of these people's claims
//! ```
//!
//! and two this port adds for the tests and the two-actor race (`kith-sessions` reaches them only through its functions):
//!
//! ```text
//! release PID                              gives back every claim PID holds (`release_claims`)
//! holder SID                               the live claim on SID under either name, as its file says; exit 1 when none
//! ```
//!
//! and the three that name a session or wrap claude (`latest` is a plain verb; `pick` and `run` talk to the terminal as they
//! go, so they are [`run_live`]):
//!
//! ```text
//! latest AGENT DIR                         the id of the agent's newest session begun in DIR
//! pick AGENT                               lists recent sessions on stderr, reads a number on stdin, prints that session id
//! run AGENT -- CMD...                      runs CMD (claude) as a child, passes signals and the exit code on, gives the claims back
//! ```

use std::time::Duration;

use serde_json::Value;

use super::recent;
use super::resolve::{Host, resolve};
use super::{
    Claim, Ctx, claim_path, claim_start, claim_wait, proc_claim, registry, release_claims,
    sweep_persons,
};
use crate::secrets::pyjson;

/// What a verb printed and how it ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Out {
    fn die(msg: impl std::fmt::Display) -> Self {
        Self {
            stderr: format!("{msg}\n"),
            code: 1,
            ..Self::default()
        }
    }

    fn ok(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            ..Self::default()
        }
    }
}

const USAGE: &str = "usage: kith kernel threads record | resolve AGENT THREAD [PID [oneshot]] | claim SID PID [PERSON [AGENT]] | \
terminal SID PID [PERSON [AGENT]] | proc PID PERSON AGENT | sweep PERSON... | release PID | holder SID | \
latest AGENT DIR | pick AGENT | run AGENT -- CMD...";

fn int(s: &str) -> Result<i64, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("invalid literal for int() with base 10: '{s}'"))
}

/// Runs one verb. `stdin` is read only by `record`.
pub fn run(c: &Ctx, host: &dyn Host, args: &[String], stdin: &dyn Fn() -> String) -> Out {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let n = a.len();
    let opt = |i: usize| a.get(i).copied();
    let done =
        |r: Result<Out, String>| r.unwrap_or_else(|m| Out::die(format!("kith-sessions: {m}")));
    match a.first().copied() {
        Some("record") => {
            if let Err(e) = registry::record(c, &stdin()) {
                // Listing must never block a session: said, and exit 0.
                return Out {
                    stderr: format!("kith-sessions record: {e}\n"),
                    ..Out::default()
                };
            }
            Out::default()
        }
        Some("resolve") if (3..=5).contains(&n) => done((|| {
            let claimer = if n > 3 {
                int(a[3])?
            } else {
                super::os::parent_pid()
            };
            // A fifth word "oneshot": a `-p` run, which waits a little for a held session instead of refusing at once.
            let wait = if a.get(4) == Some(&"oneshot") {
                claim_wait(c.env)
            } else {
                Duration::ZERO
            };
            Ok(match resolve(c, host, a[1], a[2], claimer, wait) {
                Ok(r) => Out::ok(format!("{}\n", r.shell())),
                Err(m) => Out::die(format!("kith: {m}")),
            })
        })()),
        Some(verb @ ("claim" | "terminal")) if (3..=5).contains(&n) => done((|| {
            let (kind, wait) = if verb == "claim" {
                ("oneshot", claim_wait(c.env))
            } else {
                ("terminal", Duration::ZERO)
            };
            Ok(
                match claim_start(c, a[1], int(a[2])?, kind, wait, opt(3), opt(4)) {
                    Ok(()) => Out::default(),
                    Err(m) => Out::die(format!("kith: {m}")),
                },
            )
        })()),
        Some("proc") if n == 4 => done((|| {
            proc_claim(c, int(a[1])?, a[2], a[3]).map_err(|e| e.to_string())?;
            Ok(Out::default())
        })()),
        Some("latest") if n == 3 => match recent::latest(c, a[1], a[2]) {
            Ok(sid) => Out::ok(format!("{sid}\n")),
            Err(m) => Out::die(m),
        },
        Some("sweep") if n > 1 => {
            let persons: Vec<String> = a[1..].iter().map(|s| (*s).to_owned()).collect();
            Out::ok(format!(
                "swept {}\n",
                sweep_persons(c, &persons, Duration::from_secs(2)).len()
            ))
        }
        Some("release") if n == 2 => done((|| {
            release_claims(c, int(a[1])?);
            Ok(Out::default())
        })()),
        Some("holder") if n == 2 => {
            let Some(dir) = c.reg_dir() else {
                return Out::die("kith: KITH_STATE is not set");
            };
            for takeover in [false, true] {
                let Some(raw) = std::fs::read_to_string(claim_path(&dir, a[1], takeover))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                else {
                    continue;
                };
                if Claim::from_value(&raw).is_some_and(|c| c.is_live()) {
                    return Out::ok(format!("{}\n", pyjson::dumps(&raw)));
                }
            }
            Out {
                code: 1,
                ..Out::default()
            }
        }
        _ => Out::die(USAGE),
    }
}

/// What the live verbs talk to: the terminal, as it goes.
pub struct Io<'a> {
    pub out: &'a dyn Fn(&str),
    pub err: &'a dyn Fn(&str),
    /// One line of standard input (`sys.stdin.readline()`).
    pub line: &'a dyn Fn() -> String,
    /// The kernel folder: `run` starts `kith-lifecycle`, `kith-ask` and `kith-wake` from its `bin/`.
    pub kernel: &'a std::path::Path,
}

/// `pick` and `run`, which write to the terminal while they work (a prompt is read after it is shown; the wrapper's notes
/// come after the child ran). `None` when the verb is not one of them: [`run`] has it. Otherwise the exit code.
pub fn run_live(c: &Ctx, host: &dyn Host, args: &[String], io: &Io) -> Option<i32> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.first().copied() {
        Some("pick") if a.len() == 2 => Some(match recent::pick(c, a[1], io.err, io.line) {
            Ok(sid) => {
                (io.out)(&format!("{sid}\n"));
                0
            }
            Err(None) => 1,
            Err(Some(m)) => {
                (io.err)(&format!("{m}\n"));
                1
            }
        }),
        Some("run") if a.len() > 3 && a[2] == "--" => {
            let cmd: Vec<String> = a[3..].iter().map(|s| (*s).to_owned()).collect();
            Some(super::run::run(c, host, io.kernel, a[1], &cmd, io.err))
        }
        _ => None,
    }
}
