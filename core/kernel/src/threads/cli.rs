//! `kith kernel threads <verb> ...`: the verbs of `kernel/bin/kith-sessions` that touch the writer claim and the registry,
//! with its arguments, output and exit codes (`sys.exit("text")` is the text on stderr and exit 1):
//!
//! ```text
//! record                                   SessionStart hook; stdin is the hook's JSON; always exit 0
//! resolve AGENT THREAD [PID [oneshot]]     shell assignments r_sid, r_cwd, r_from[, r_alone]; takes the claim for PID (default: our parent)
//! claim SID PID [PERSON [AGENT]]           the one-shot's claim, waiting KITH_CLAIM_WAIT s while another writer holds it
//! terminal SID PID [PERSON [AGENT]]        an interactive run's claim, refused at once when held
//! proc PID PERSON AGENT                    the session-less claim the revoke sweep finds; exit 9 when the core cannot take it
//! epoch PERSON                             Writer mode: `PERSON:N`, the core's revoke epoch for them (KITH_REVOKE_SEEN)
//! release SID                              gives back SID's claim when its holder is our parent (the launcher's shell); exit 1 else
//! sweep PERSON...                          `swept N`: ends the live holders of these people's claims
//! pick AGENT                               the agent's recent sessions numbered on stderr; reads a number; prints the session id
//! latest AGENT DIR                         the id of the agent's newest session begun in DIR
//! ```
//!
//! `claim` and `terminal` print `lock-alone` (and `resolve` adds `r_alone=1`) when the owner's run goes on under the
//! conversation lock alone because the core gave no answer. In Writer mode (the claims are the core's) every claim,
//! release and sweep is a call of the core; see [`super::writer`].
//!
//! and two this port adds for the tests and the two-actor race (`kith-sessions` reaches them only through its functions):
//!
//! ```text
//! release-pid PID                          gives back every claim PID holds (`release_claims`)
//! holder SID                               the live claim on SID under either name, as its file says; exit 1 when none
//! ```
//!
//! Not here: `run` (the wrapper that runs claude and writes the idle intent), which is the launch's.

use std::time::Duration;

use serde_json::Value;

use super::core::Core;
use super::resolve::{Host, resolve_taking};
use super::writer::{ProcErr, Taker};
use super::{Claim, Ctx, claim_path, claim_wait, pick, release_claims, spool};
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

/// `kith-sessions -h`: the Python's own usage text, which it also dies with for a command line it does not know.
const HELP: &str = include_str!("usage.txt");

fn int(s: &str) -> Result<i64, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("invalid literal for int() with base 10: '{s}'"))
}

/// Runs one verb. `stdin` is read (whole) only by `record`.
///
/// `core` is the owner's door onto the Kith core. With none, Writer mode (the claims are the core's) reads as a core that
/// does not answer, on every verb: never as the files.
pub fn run(
    c: &Ctx,
    host: &dyn Host,
    core: Option<&dyn Core>,
    args: &[String],
    stdin: &dyn Fn() -> String,
) -> Out {
    run_with(c, host, core, args, stdin, &|_| stdin())
}

/// [`run`] with the way `pick` asks its question kept apart from the whole of stdin `record` reads: `ask` shows the table
/// and the prompt on stderr at once and returns the line typed (a terminal's stdin never ends, and nothing may wait for it).
#[allow(clippy::too_many_lines)]
pub fn run_with(
    c: &Ctx,
    host: &dyn Host,
    core: Option<&dyn Core>,
    args: &[String],
    stdin: &dyn Fn() -> String,
    ask: &dyn Fn(&str) -> String,
) -> Out {
    let taker = Taker::new(c, core);
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
            // Listing must never block a session: said, and exit 0.
            let lines = spool::record(c, core, &stdin());
            Out {
                stderr: lines.iter().fold(String::new(), |mut all, l| {
                    all.push_str("kith-sessions record: ");
                    all.push_str(l);
                    all.push('\n');
                    all
                }),
                ..Out::default()
            }
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
            Ok(
                match resolve_taking(c, host, core, a[1], a[2], claimer, wait) {
                    Ok(r) => Out {
                        stdout: format!("{}\n", r.shell()),
                        stderr: r.said,
                        code: 0,
                    },
                    Err(m) => Out::die(format!("kith: {m}")),
                },
            )
        })()),
        Some(verb @ ("claim" | "terminal")) if (3..=5).contains(&n) => done((|| {
            let (kind, wait) = if verb == "claim" {
                ("oneshot", claim_wait(c.env))
            } else {
                ("terminal", Duration::ZERO)
            };
            let pid = int(a[2])?;
            let done = taker.claim_start(a[1], pid, kind, wait, opt(3), opt(4));
            let said = taker.said();
            Ok(match done {
                Ok(()) => Out {
                    stdout: if taker.alone() {
                        "lock-alone\n".to_owned()
                    } else {
                        String::new()
                    },
                    stderr: said,
                    code: 0,
                },
                Err(m) => Out {
                    stderr: format!("{said}kith: {m}\n"),
                    code: 1,
                    ..Out::default()
                },
            })
        })()),
        Some("proc") if n == 4 => done((|| {
            Ok(match taker.proc(int(a[1])?, a[2], a[3]) {
                Ok(()) => Out::default(),
                // The core cannot take this person's run: exit 9 tells the launcher not to start it.
                Err(ProcErr::Core(m)) => Out {
                    stderr: format!("{m}\n"),
                    code: 9,
                    ..Out::default()
                },
                Err(ProcErr::Io(m)) => return Err(m),
            })
        })()),
        // What this copy knows, one per line: the transcripts step checks the installed copy for registry-core.
        Some("caps") if n == 1 => Out::ok("registry-core\n"),
        Some("epoch") if n == 2 => match taker.epoch(a[1]) {
            Ok(text) => Out::ok(text),
            Err(m) => Out::die(format!("kith: {m}")),
        },
        Some("sweep") if n > 1 => {
            let persons: Vec<String> = a[1..].iter().map(|s| (*s).to_owned()).collect();
            match taker.sweep(&persons, Duration::from_secs(2)) {
                Ok(pids) => Out::ok(format!("swept {}\n", pids.len())),
                Err(m) => Out {
                    stderr: format!("{m}\n"),
                    code: 3,
                    ..Out::default()
                },
            }
        }
        // `--held` gives back the claim a resume took for the launcher's shell: terminal.open takes the session's claim itself.
        Some("release") if n == 2 => Out {
            code: i32::from(!taker.release_own(a[1])),
            ..Out::default()
        },
        Some("release-pid") if n == 2 => done((|| {
            release_claims(c, int(a[1])?);
            Ok(Out::default())
        })()),
        Some("pick") if n == 2 => done((|| {
            let rows = pick::recent(c, a[1], 15, None)?;
            if rows.is_empty() {
                return Ok(Out::die(format!(
                    "kith: no {} sessions to resume (none in the cost ledger with a transcript on this Mac)",
                    a[1]
                )));
            }
            let said = ask(&pick::table(&rows));
            let said = said.lines().next().unwrap_or("").trim().to_owned();
            if said.is_empty() {
                return Ok(Out {
                    code: 1,
                    ..Out::default()
                });
            }
            let chosen = said
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| said.parse::<usize>().ok())
                .flatten()
                .filter(|i| (1..=rows.len()).contains(i));
            let Some(i) = chosen else {
                return Ok(Out::die(format!("kith: no session {said} in the list")));
            };
            Ok(Out {
                stdout: format!("{}\n", rows[i - 1].sid),
                ..Out::default()
            })
        })()),
        Some("latest") if n == 3 => done((|| {
            let rows = pick::recent(c, a[1], 1, Some(a[2]))?;
            Ok(match rows.first() {
                Some(r) => Out::ok(format!("{}\n", r.sid)),
                None => Out::die(format!(
                    "kith: no {0} session started in {1} to continue (kith {0} -r lists them all)",
                    a[1], a[2]
                )),
            })
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
        _ => Out {
            stderr: HELP.to_owned(),
            code: 1,
            ..Out::default()
        },
    }
}
