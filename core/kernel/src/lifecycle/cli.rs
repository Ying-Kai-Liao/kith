//! The command line of `kernel/bin/kith-lifecycle`, with argparse's usage lines, error texts (exit 2, on stderr) and help:
//!
//! ```text
//! sweep [--session SID] [--at-sleep]   the tick: one line `SID DECISION WHAT` per session acted on
//! status SID [--json]                  one JSON object: how it ended, the decision, outstanding work
//! intent SID WHY PID                   writes an intent (before a signal)
//! compact SID                          for the difftest only: `compact()` for a session whose decision is on record
//! ```
//!
//! argparse takes `--name=value` and the unique prefix of a long option; so does this.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::sweep::Sweep;
use super::{decision, how_it_ended, state_dir, transcript};
use crate::secrets::pyjson;
use crate::threads::cli::Out;
use crate::threads::core::Core;
use crate::threads::registry::live_pid;
use crate::threads::writer::Taker;
use crate::threads::{Ctx, Host, intent};

/// The difftest sets this to reach the hidden `compact SID` (`lifecycle-new.py`); without it `compact` is no verb, as in the Python.
const TEST_VERB: &str = "KITH_LIFECYCLE_TEST_VERB";
const PROG: &str = "kith-lifecycle";
const USAGE: &str = "usage: kith-lifecycle [-h] {sweep,status,intent} ...\n";
const USAGE_SWEEP: &str = "usage: kith-lifecycle sweep [-h] [--session SESSION] [--at-sleep]\n";
const USAGE_STATUS: &str = "usage: kith-lifecycle status [-h] [--json] session\n";
const USAGE_INTENT: &str = "usage: kith-lifecycle intent [-h] session why pid\n";
const HELP: &str = include_str!("help.txt");
const HELP_SWEEP: &str = include_str!("help_sweep.txt");
const HELP_STATUS: &str = include_str!("help_status.txt");
const HELP_INTENT: &str = include_str!("help_intent.txt");

/// argparse's `parser.error`: usage and the message on stderr, exit 2.
fn usage_error(usage: &str, prog: &str, msg: &str) -> Out {
    Out {
        stderr: format!("{usage}{prog}: error: {msg}\n"),
        code: 2,
        ..Out::default()
    }
}

fn help(text: &str) -> Out {
    Out {
        stdout: text.to_owned(),
        ..Out::default()
    }
}

/// A word that cannot be a value of an option: argparse reads anything that starts with `-` (and is no negative number) as an option.
fn optionish(s: &str) -> bool {
    s.len() > 1 && s.starts_with('-') && s.parse::<f64>().is_err()
}

/// `name` for `--name`, `--name=value` or a unique prefix of it, among `longs`.
fn long_option<'a>(
    arg: &'a str,
    longs: &[&'static str],
) -> Option<(&'static str, Option<&'a str>)> {
    let body = arg.strip_prefix("--")?;
    let (name, value) = body
        .split_once('=')
        .map_or((body, None), |(n, v)| (n, Some(v)));
    if let Some(l) = longs.iter().find(|l| l.strip_prefix("--") == Some(name)) {
        return Some((l, value));
    }
    let hits: Vec<&&str> = longs
        .iter()
        .filter(|l| l[2..].starts_with(name) && !name.is_empty())
        .collect();
    (hits.len() == 1).then(|| (*hits[0], value))
}

struct Parsed {
    positionals: Vec<String>,
    session: Option<String>,
    flags: Vec<&'static str>,
    extra: Vec<String>,
}

/// One subcommand's arguments: its long options (`takes`: the ones with a value), then positionals.
fn parse_sub(
    args: &[&str],
    usage: &str,
    prog: &str,
    helptext: &str,
    longs: &[&'static str],
    takes: &[&'static str],
) -> Result<Parsed, Out> {
    let mut p = Parsed {
        positionals: Vec::new(),
        session: None,
        flags: Vec::new(),
        extra: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        i += 1;
        if a == "-h" || a == "--help" {
            return Err(help(helptext));
        }
        if a == "--" {
            p.positionals
                .extend(args[i..].iter().map(|s| (*s).to_owned()));
            break;
        }
        if a.starts_with("--") {
            match long_option(a, longs) {
                Some((name, value)) if takes.contains(&name) => {
                    let v = match value {
                        Some(v) => v.to_owned(),
                        None => match args.get(i) {
                            Some(v) if !optionish(v) => {
                                i += 1;
                                (*v).to_owned()
                            }
                            _ => {
                                return Err(usage_error(
                                    usage,
                                    prog,
                                    &format!("argument {name}: expected one argument"),
                                ));
                            }
                        },
                    };
                    p.session = Some(v);
                }
                Some((name, None)) => p.flags.push(name),
                Some((name, Some(_))) => {
                    return Err(usage_error(
                        usage,
                        prog,
                        &format!("argument {name}: ignored explicit argument"),
                    ));
                }
                None => p.extra.push(a.to_owned()),
            }
        } else if optionish(a) {
            p.extra.push(a.to_owned());
        } else {
            p.positionals.push(a.to_owned());
        }
    }
    Ok(p)
}

/// The command line as `kith-lifecycle` parses it: the verb and what it needs.
enum Cmd {
    Sweep {
        session: Option<String>,
        at_sleep: bool,
    },
    Status {
        session: String,
    },
    Intent {
        session: String,
        why: String,
        pid: i64,
    },
    Compact {
        session: String,
    },
}

fn parse(args: &[&str]) -> Result<Cmd, Out> {
    let top = |msg: &str| usage_error(USAGE, PROG, msg);
    let Some((&verb, rest)) = args.split_first() else {
        return Err(top("the following arguments are required: cmd"));
    };
    if verb == "-h" || verb == "--help" {
        return Err(help(HELP));
    }
    let unrecognized =
        |extra: &[String]| top(&format!("unrecognized arguments: {}", extra.join(" ")));
    match verb {
        "sweep" => {
            let p = parse_sub(
                rest,
                USAGE_SWEEP,
                "kith-lifecycle sweep",
                HELP_SWEEP,
                &["--session", "--at-sleep"],
                &["--session"],
            )?;
            let mut extra = p.extra;
            extra.extend(p.positionals);
            if !extra.is_empty() {
                return Err(unrecognized(&extra));
            }
            Ok(Cmd::Sweep {
                session: p.session,
                at_sleep: p.flags.contains(&"--at-sleep"),
            })
        }
        "status" => {
            let mut p = parse_sub(
                rest,
                USAGE_STATUS,
                "kith-lifecycle status",
                HELP_STATUS,
                &["--json"],
                &[],
            )?;
            if p.positionals.is_empty() {
                return Err(usage_error(
                    USAGE_STATUS,
                    "kith-lifecycle status",
                    "the following arguments are required: session",
                ));
            }
            let session = p.positionals.remove(0);
            let mut extra = p.extra;
            extra.extend(p.positionals);
            if !extra.is_empty() {
                return Err(unrecognized(&extra));
            }
            Ok(Cmd::Status { session })
        }
        "intent" => {
            let mut p = parse_sub(
                rest,
                USAGE_INTENT,
                "kith-lifecycle intent",
                HELP_INTENT,
                &[],
                &[],
            )?;
            let prog = "kith-lifecycle intent";
            let pid = match p.positionals.get(2) {
                Some(raw) => Some(raw.trim().parse::<i64>().map_err(|_| {
                    usage_error(
                        USAGE_INTENT,
                        prog,
                        &format!("argument pid: invalid int value: '{raw}'"),
                    )
                })?),
                None => None,
            };
            let names = ["session", "why", "pid"];
            if p.positionals.len() < 3 {
                return Err(usage_error(
                    USAGE_INTENT,
                    prog,
                    &format!(
                        "the following arguments are required: {}",
                        names[p.positionals.len()..].join(", ")
                    ),
                ));
            }
            let extra: Vec<String> = p
                .extra
                .into_iter()
                .chain(p.positionals.split_off(3))
                .collect();
            if !extra.is_empty() {
                return Err(unrecognized(&extra));
            }
            Ok(Cmd::Intent {
                session: p.positionals.remove(0),
                why: p.positionals.remove(0),
                pid: pid.unwrap_or(0),
            })
        }
        "compact" if rest.len() == 1 && std::env::var_os(TEST_VERB).is_some_and(|v| v == "1") => {
            Ok(Cmd::Compact {
                session: rest[0].to_owned(),
            })
        }
        other => Err(top(&format!(
            "argument cmd: invalid choice: '{other}' (choose from 'sweep', 'status', 'intent')"
        ))),
    }
}

/// Runs one verb. `core` is the owner's door onto the Kith core (the claims in Writer mode); `kith_bin` the kith program
/// kith-stub is looked for beside.
pub fn run(
    c: &Ctx,
    host: &dyn Host,
    core: Option<&dyn Core>,
    kith_bin: Option<PathBuf>,
    args: &[String],
) -> Out {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let cmd = match parse(&a) {
        Ok(cmd) => cmd,
        Err(out) => return out,
    };
    let taker = Taker::new(c, core);
    let sweep = Sweep {
        c,
        host,
        taker: &taker,
        kith_bin,
        said: RefCell::new(String::new()),
    };
    let finish = |stdout: String, sweep: &Sweep| Out {
        stdout,
        stderr: format!("{}{}", sweep.said.borrow(), taker.said()),
        code: 0,
    };
    match cmd {
        Cmd::Sweep { session, at_sleep } => {
            let only = session.filter(|s| !s.is_empty());
            let mut lines = String::new();
            for (sid, want, did) in sweep.sweep(only.as_deref(), at_sleep) {
                let _ = writeln!(lines, "{sid} {want} {did}");
            }
            finish(lines, &sweep)
        }
        Cmd::Status { session } => finish(status(&sweep, &session), &sweep),
        Cmd::Intent { session, why, pid } => {
            if let Some(state) = state_dir(c) {
                intent::write_intent(&state, &session, &why, pid);
            }
            finish(String::new(), &sweep)
        }
        Cmd::Compact { session } => finish(compact_for_test(&sweep, c, &session), &sweep),
    }
}

fn status(sweep: &Sweep, sid: &str) -> String {
    let c = sweep.c;
    let e = sweep
        .entries()
        .into_iter()
        .find(|x| x.get("sessionId").and_then(Value::as_str) == Some(sid));
    let state = state_dir(c);
    let had = e.as_ref().is_some_and(|e| live_pid(c, e).is_none());
    let ended = state
        .as_deref()
        .and_then(|s| how_it_ended(s, sid, had, e.as_ref()));
    let rec = state.as_deref().and_then(|s| decision::read(s, sid));
    let outstanding = e
        .as_ref()
        .and_then(|e| e.get("transcriptPath").and_then(Value::as_str))
        .map_or_else(Vec::new, |t| {
            transcript::outstanding(std::path::Path::new(t))
        });
    let body = json!({
        "session": sid,
        "ended": ended,
        "decision": rec.map(Value::Object),
        "outstanding": outstanding,
    });
    format!("{}\n", pyjson::dumps(&body))
}

/// `compact SID`, for the difftest (`lifecycle-new.py` does the setup the case names, as `lifecycle-old.py` does, then runs this):
/// `compact()` for a session whose decision is already on record, since the sweep cannot reach its own refusals (`held`,
/// `skipped`) from outside. The idle period is the decision's own, not the transcript's now.
fn compact_for_test(sweep: &Sweep, c: &Ctx, sid: &str) -> String {
    let Some(state) = state_dir(c) else {
        return String::new();
    };
    let Some(e) = sweep
        .entries()
        .into_iter()
        .find(|x| x.get("sessionId").and_then(Value::as_str) == Some(sid))
    else {
        return format!("no session {sid}\n");
    };
    let since = decision::read(&state, sid)
        .and_then(|d| d.get("idleSince").and_then(Value::as_f64))
        .or_else(|| {
            transcript::last_turn_at(std::path::Path::new(
                e.get("transcriptPath")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            ))
        });
    let got = sweep
        .compact(&e, since.unwrap_or(0.0), None)
        .unwrap_or_else(|x| format!("error: {x}"));
    format!("{got}\n")
}
