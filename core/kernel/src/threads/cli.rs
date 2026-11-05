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
//! Not here: `run` (the wrapper that runs claude and writes the idle intent), `pick` and `latest` (they read the cost
//! ledger to name a session). `run` is the launch's, and they take no claim.

use std::time::Duration;

use serde_json::Value;

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

/// `kith-sessions -h`: the Python's own usage text.
const HELP: &str = include_str!("usage.txt");

const USAGE: &str = "usage: kith kernel threads record | resolve AGENT THREAD [PID [oneshot]] | claim SID PID [PERSON [AGENT]] | \
terminal SID PID [PERSON [AGENT]] | proc PID PERSON AGENT | sweep PERSON... | release PID | holder SID";

fn int(s: &str) -> Result<i64, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("invalid literal for int() with base 10: '{s}'"))
}

/// Runs one verb. `stdin` is read only by `record`.
pub fn run(c: &Ctx, host: &dyn Host, args: &[String], stdin: &dyn Fn() -> String) -> Out {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // `kith-sessions`' own usage text, on stderr with exit 1 (the Python's `sys.exit(__doc__)`).
    if matches!(a.first().copied(), Some("-h" | "--help")) {
        return Out {
            stderr: HELP.to_owned(),
            code: 1,
            ..Out::default()
        };
    }
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
