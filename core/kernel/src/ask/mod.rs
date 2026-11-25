//! `kith ask` as a thin client of the core's `ask.*` rows (retire-python P2-3, A3; design `2026-11-24-ask-ledger`, R8 and R9):
//! the words, the text and the exit codes are `kernel/bin/kith-ask`'s, which is the same client in Python until the ledger is
//! the core's (`kith-ask` forwards here only when the program lists `ask-ledger` and the `asks` module is switched).
//!
//! Like `wake`, this crate never reaches the socket itself: the program passes a [`Call`]. Nothing here is Unix-only except what
//! a Unix program owns by nature: the OS sandbox probe of a guest's shell (`guest_sandbox`). A foreground ask runs the callee
//! (`bin/kith AGENT -- task`) in this process, as before; a `--bg` ask starts nothing here: its row carries a turn that the host
//! runs (A2). What the rows do not carry yet is told where it is dropped (the TODOs below).
//!
//! Differences from the Python, all of them the ledger's rules and listed in `kernel/tests/difftest/intended.json`:
//! - `--answer` says `woke` for an answer (the asker is owed its wake in the same step, whether or not a writer holds the
//!   session: R3) and `kept` for a progress note; an unknown id is refused (the Python kept a stray file).
//! - `--wait` reads rows (`ask.read`): a row reaches the asker by `--wait` or by its wake, never both (R5), so an answer that
//!   was posted and not yet taken by a wake is still `--wait`'s.
//! - `--to` to a stopped or ended ask is refused at the send (`was stopped; ask anew`).
//! - `--sweep` does nothing: the host's tick settles and wakes (R4).

use std::io::Write;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::wake::{Call, Fail};

mod conv;
mod fg;
mod remote;
mod verbs;

#[cfg(test)]
mod tests;

/// The exit code of an ask a cap refused: not the agent failing, and not "still waiting" (3).
pub const REFUSED: i32 = crate::delegation::REFUSED;

/// The efforts `-e` takes.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// `kith kernel settings ARGS` run in this process: (exit code, stdout, stderr).
pub type SettingsFn<'a> = dyn Fn(&[String]) -> (i32, String, String) + 'a;

/// What the program lends this client: the core's door, the caller's folders and environment, and the few things that need
/// an OS call this crate does not make (`unsafe_code` is denied here).
pub struct Host<'a> {
    /// One owner call of the core (`wake_cmd`'s: `core_front::core_call_within`).
    pub call: &'a Call<'a>,
    /// The kernel folder (the one holding `bin/` and `lib/`): where `bin/kith`, `bin/kith-remote` and `bin/kith-auth` are.
    pub kernel: PathBuf,
    pub home: PathBuf,
    /// `<state>`: where `project-id` looks for the projects (`-p`).
    pub state: PathBuf,
    /// `<machine>`: where `ask.json` and `settings.json` live.
    pub machine: PathBuf,
    /// The caller's whole environment: the core reads its folders and `KITH_ASK_*` from it, and the callee gets it.
    pub env: Vec<(String, String)>,
    /// Whether SIGTERM (Windows: the console's control handler) arrived since [`Host::watch_term`] was called.
    pub terminated: &'a dyn Fn() -> bool,
    /// Starts watching for SIGTERM, which a foreground ask turns into an end of its row, as the Python's handler did.
    pub watch_term: &'a dyn Fn(),
    /// `kith kernel settings ARGS...` run in this process: (exit code, stdout, stderr). For `--cap N`.
    pub settings: &'a SettingsFn<'a>,
}

impl Host<'_> {
    /// `os.environ.get(name)`: None for a variable that is not set (an empty one is Some("")).
    pub(crate) fn var(&self, name: &str) -> Option<String> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// `os.environ.get(name) or None`: unset and empty are the same.
    pub(crate) fn set(&self, name: &str) -> Option<String> {
        self.var(name).filter(|v| !v.is_empty())
    }

    /// The caller's view for a row that reads folders and the lineage from it (`ask.open` and the rows with KERNEL, HOME, ENV).
    pub(crate) fn view(&self, mut params: Value) -> Value {
        let env: Map<String, Value> = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        params["kernel"] = json!(self.kernel.display().to_string());
        params["home"] = json!(self.home.display().to_string());
        params["env"] = Value::Object(env);
        params
    }

    /// One call of the core, or the reason it gave no answer, in the words `kith ask` says them.
    pub(crate) fn core(&self, method: &str, params: &Value) -> Result<Value, Stop> {
        (self.call)(method, params).map_err(|f| Stop::from_fail(&f))
    }
}

/// Where a verb stops: the exit code and what stderr says (no line when empty). `sys.exit(text)` is code 1 and the text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stop {
    pub code: i32,
    pub msg: String,
}

pub(crate) type R<T> = Result<T, Stop>;

/// `sys.exit(msg)`: the message on stderr, exit 1.
pub(crate) fn die(msg: impl Into<String>) -> Stop {
    Stop {
        code: 1,
        msg: msg.into(),
    }
}

/// The words of a refusal of the core without its own prefixes.
pub(crate) fn plain(why: &str) -> &str {
    let why = why.trim();
    let why = why.strip_prefix("kith-core: ").unwrap_or(why);
    why.strip_prefix("asks: ").unwrap_or(why)
}

impl Stop {
    fn from_fail(f: &Fail) -> Stop {
        match f {
            Fail::Refused(why) => die(format!("kith ask: {}", plain(why))),
            Fail::NoAnswer(why) => die(format!(
                "kith ask: the asks are the Kith core's on this Mac and it did not answer ({why}); the call may not have been made, and nothing was written here"
            )),
        }
    }
}

/// Python's `' '.join(...)`'s `.strip()`.
pub(crate) fn words(parts: &[String]) -> String {
    parts.join(" ").trim().to_owned()
}

/// `json.dumps(v, ensure_ascii=False)`: `", "` and `": "` between the items, non-ASCII text as it is.
pub(crate) fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_json(v, &mut out);
    out
}

fn write_json(v: &Value, out: &mut String) {
    match v {
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(x, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                write_json(x, out);
            }
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `kith ask ARGS...`; the exit code.
pub fn run(host: &Host, args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let result = go(host, args, out, err);
    let _ = out.flush();
    match result {
        Ok(code) => code,
        Err(stop) => {
            if !stop.msg.is_empty() {
                let _ = writeln!(err, "{}", stop.msg);
            }
            stop.code
        }
    }
}

fn go(host: &Host, args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> R<i32> {
    let Some(first) = args.first() else {
        let _ = writeln!(out, "{USAGE}");
        return Ok(0);
    };
    if first == "-h" || first == "--help" {
        let _ = writeln!(out, "{USAGE}");
        return Ok(0);
    }
    // A guest's shell cannot start a run (no network, login or agents folder in its sandbox): the helper its membership
    // grants is asked through a tool that runs outside, as the kith launcher says too.
    if guest_sandbox() {
        let _ = writeln!(
            err,
            "kith ask: from a guest's shell a helper is asked with the ask_helper tool (mcp__kithpermit__ask_helper: agent and task), for a helper your membership grants."
        );
        return Ok(2);
    }
    // `--bg` anywhere before the task's `--`.
    let bg = args.iter().any(|a| a == "--bg");
    let args: Vec<String> = if bg {
        args.iter().filter(|a| *a != "--bg").cloned().collect()
    } else {
        args.to_vec()
    };
    let Some(first) = args.first().cloned() else {
        return Ok(0);
    };
    let rest = &args[1..];
    match first.as_str() {
        "ls" => Ok(conv::ls(host, rest.iter().any(|a| a == "--json"), out)),
        "--wait" => verbs::wait(host, rest, out, err),
        "--to" => verbs::to(host, &args, out, err),
        "--answer" => verbs::answer(host, rest, out),
        "--reply" => verbs::reply(host, rest, err),
        "--cap" => verbs::cap(host, rest, out),
        "--cap-told" => verbs::cap_told(host, rest),
        "--end" => verbs::end(host, rest, out, err),
        // The only form is --json.
        "delegations" => verbs::delegations(host, out),
        // The host's tick settles what no one reported and wakes the askers (R4): there is nothing left to sweep.
        "--sweep" => Ok(0),
        _ if first.contains('@') => remote::ask(host, bg, &args, err),
        _ => fg::ask(host, bg, &first, rest, out, err),
    }
}

const USAGE: &str = "kith ask <agent> [options] \"task\"     run the agent on the task in this directory; print its answer
kith ask --bg <agent> \"task\"          the same run in the background: prints an id at once
kith ask ls [--json] | --wait ID | --to ID \"message\" | --answer ID [--as AGENT] [--progress] -- TEXT
kith ask --end [--session S]... [--person P]... | delegations --json | --cap [N|none]";

/// Whether this process is inside the OS sandbox `kernel/bin/kith` puts around the run of anyone but the owner: that sandbox
/// closes the home folder to reads and nothing of the owner's ever does (`kith_paths.guest_sandbox`). Any doubt is a guest.
#[cfg(unix)]
fn guest_sandbox() -> bool {
    match crate::threads::os::passwd_home() {
        Some(home) => std::fs::read_dir(home).is_err(),
        None => true,
    }
}

/// Nothing marks a guest's sandbox on Windows yet (its runs come with the Windows launch); the owner's shell is the only one.
#[cfg(not(unix))]
fn guest_sandbox() -> bool {
    false
}

/// Who is sending: a person's id, or None for the owner. A guest's shell that dropped `KITH_PERSON` is no one.
pub(crate) fn caller_person(host: &Host) -> R<Option<String>> {
    let person = host.set("KITH_PERSON");
    if person.is_none() && guest_sandbox() {
        return Err(die(
            "kith ask: this is inside a guest's run, which can't send as the owner",
        ));
    }
    Ok(person)
}

/// Whether this call comes from inside an agent's run: an agent doesn't lift its own caps.
pub(crate) fn from_agent_run(host: &Host) -> bool {
    ["KITH_ASK_ID", "KITH_AGENT", "CLAUDE_CODE_SESSION_ID"]
        .iter()
        .any(|k| host.set(k).is_some())
}

/// `datetime.now().strftime('%Y%m%d%H%M%S')` and three random bytes: an id as `new_id` makes them.
pub(crate) fn new_id(kind: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("{kind}-{stamp}-{:06x}", random & 0x00ff_ffff)
}

/// The last `n` characters of `text` (Python's `text[-n:]`).
pub(crate) fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}
