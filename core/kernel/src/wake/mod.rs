//! `kith wake` once the wake queue is the Kith core's (retire-python S4, R9 and R8): a thin client of the core's `wake.*`
//! rows, with no Python, so a Mac without it (Windows) still has `kith wake`. The words, the text and the exit codes are
//! `kernel/bin/kith-wake`'s (see its help), which is the same client in Python.
//!
//! Like `threads`, this crate never reaches the socket itself: the program passes a [`Call`]. Unlike `threads` it is not
//! Unix-only: nothing here forks, locks a file or reads `ps`. `drain` and `deliver` say nothing and do nothing: the host
//! ticks the queue (`wake.tick`), and a second runner here would be the double start the tick exists to prevent.
//!
//! The queue is the core's when `<state>/sessions/queue/.tombstone` exists, or `<machine>/core/cutover/wake.switch` says
//! more than `state=stepping` (fail closed: a file we cannot even look for counts as switched, as for the claims). With
//! the queue the core's a core that does not answer is a refusal: nothing is written, there is no file to fall back to.

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

/// Why a call of the core gave no answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    /// The core rejected the call (a bad param, a row that is not open to the caller); nothing ran.
    Refused(String),
    /// Nothing answered, or the answer was lost.
    NoAnswer(String),
}

/// One owner call of the core: the answer object.
pub type Call<'a> = dyn Fn(&str, &Value) -> Result<Value, Fail> + 'a;

/// The marker the cutover step writes in `<state>/sessions/queue/`.
pub const TOMBSTONE: &str = ".tombstone";

fn lexists(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// `kith_wake_core.switched`: the wake queue is the core's on this Mac.
pub fn switched(state: Option<&Path>, machine: Option<&Path>) -> bool {
    if state.is_some_and(|s| lexists(&s.join("sessions").join("queue").join(TOMBSTONE))) {
        return true;
    }
    let Some(f) = machine.map(|m| m.join("core").join("cutover").join("wake.switch")) else {
        return false;
    };
    if !lexists(&f) {
        return false;
    }
    std::fs::read_to_string(&f).map_or(true, |t| !t.lines().any(|l| l == "state=stepping"))
}

/// R8: a terminal let go of `session`; the core unparks what waited for it. Lost is fine, the core's next tick does it.
pub fn released(call: &Call, session: &str) {
    let _ = call("wake.released", &json!({ "session": session }));
}

struct Args {
    flags: Vec<(String, String)>,
    switches: Vec<String>,
    rest: Vec<String>,
}

/// `--name value` or `--name=value` for the names in `valued`, the names in `plain` alone; the rest are words. After `--`
/// everything is a word.
fn parse(args: &[String], valued: &[&str], plain: &[&str]) -> Result<Args, String> {
    let mut out = Args {
        flags: Vec::new(),
        switches: Vec::new(),
        rest: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            out.rest.extend(it.by_ref().cloned());
        } else if let Some((name, val)) = a.split_once('=').filter(|(n, _)| valued.contains(n)) {
            out.flags.push((name.to_owned(), val.to_owned()));
        } else if valued.contains(&a.as_str()) {
            let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
            out.flags.push((a.clone(), v.clone()));
        } else if plain.contains(&a.as_str()) {
            out.switches.push(a.clone());
        } else if a.starts_with("--") {
            return Err(format!("unrecognized arguments: {a}"));
        } else {
            out.rest.push(a.clone());
        }
    }
    Ok(out)
}

impl Args {
    fn get(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }
}

fn text_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn lines(answer: &Value, out: &mut dyn Write) {
    for l in answer
        .get("lines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let _ = writeln!(out, "{l}");
    }
}

enum Stop {
    Usage(String),
    Core(Fail),
}

impl From<Fail> for Stop {
    fn from(f: Fail) -> Self {
        Self::Core(f)
    }
}

/// `kith wake VERB ...`; the exit code. `env` is the caller's environment (`KITH_PERSON`, `KITH_VIA`, `KITH_AGENT` and
/// `CLAUDE_CODE_SESSION_ID`, which `wait` reads).
pub fn run(
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    call: &Call,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let (verb, rest) = args
        .split_first()
        .map_or(("ls", &[][..]), |(v, r)| (v.as_str(), r));
    match go(verb, rest, env, call, out) {
        Ok(code) => code,
        Err(Stop::Usage(why)) => {
            let _ = writeln!(err, "kith-wake: {why}");
            2
        }
        Err(Stop::Core(Fail::Refused(why))) => {
            let _ = writeln!(err, "kith-wake: {}", why.trim_start_matches("kith-core: "));
            1
        }
        Err(Stop::Core(Fail::NoAnswer(why))) => {
            let _ = writeln!(
                err,
                "kith-wake: the wake queue is the Kith core's on this Mac and it did not answer ({why}); nothing was done"
            );
            1
        }
    }
}

fn need<'a>(a: &'a Args, name: &str) -> Result<&'a str, Stop> {
    a.get(name)
        .ok_or_else(|| Stop::Usage(format!("the following arguments are required: {name}")))
}

fn go(
    verb: &str,
    rest: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    call: &Call,
    out: &mut dyn Write,
) -> Result<i32, Stop> {
    let usage = Stop::Usage;
    match verb {
        "enqueue" => {
            let valued = ["--id", "--agent", "--person", "--room", "--door", "--why"];
            let a = parse(rest, &valued, &[]).map_err(usage)?;
            let (session, text) = a
                .rest
                .split_first()
                .filter(|(_, t)| !t.is_empty())
                .ok_or_else(|| {
                    Stop::Usage("enqueue SESSION --id ID --agent AGENT -- TEXT".into())
                })?;
            let mut p = json!({
                "session": session, "id": need(&a, "--id")?, "agent": need(&a, "--agent")?,
                "text": [text.join(" ")], "door": a.get("--door").unwrap_or("run"),
                "why": a.get("--why").unwrap_or_default(),
            });
            for (flag, key) in [("--person", "person"), ("--room", "room")] {
                if let Some(v) = a.get(flag).filter(|v| !v.is_empty()) {
                    p[key] = json!(v);
                }
            }
            let _ = writeln!(out, "{}", text_of(&call("wake.enqueue", &p)?, "result"));
        }
        "withdraw" => {
            let a = parse(rest, &["--id"], &[]).map_err(usage)?;
            let session = a
                .rest
                .first()
                .ok_or_else(|| Stop::Usage("withdraw SESSION [--id ID]".into()))?;
            let mut p = json!({ "session": session });
            if let Some(id) = a.get("--id") {
                p["id"] = json!(id);
            }
            lines(&call("wake.withdraw", &p)?, out);
        }
        "withdraw-person" => {
            if rest.is_empty() {
                return Err(Stop::Usage("withdraw-person PERSON...".into()));
            }
            lines(
                &call("wake.withdrawPerson", &json!({ "persons": rest }))?,
                out,
            );
        }
        "take" => {
            let a = parse(rest, &["--holder", "--session", "--sid"], &[]).map_err(usage)?;
            let holder: i64 = need(&a, "--holder")?
                .parse()
                .map_err(|_| Stop::Usage("--holder needs a process id".into()))?;
            let mut p = json!({ "holder": holder });
            for (flag, key) in [("--session", "session"), ("--sid", "sid")] {
                if let Some(v) = a.get(flag) {
                    p[key] = json!(v);
                }
            }
            let got = call("wake.take", &p)?;
            if got.get("turn").is_none_or(Value::is_null) {
                return Ok(1);
            }
            lines(&got, out);
        }
        "done" | "back" => {
            let a = parse(rest, &["--how"], &["--held"]).map_err(usage)?;
            let run = a
                .rest
                .first()
                .ok_or_else(|| Stop::Usage(format!("{verb} RUN")))?;
            let p = if verb == "done" {
                json!({ "run": run, "how": a.get("--how").unwrap_or("answered") })
            } else {
                json!({ "run": run, "held": a.has("--held") })
            };
            let got = call(&format!("wake.{verb}"), &p)?;
            return Ok(i32::from(got.get("done") != Some(&json!(true))));
        }
        // The host ticks the queue; see the module doc.
        "drain" | "deliver" => {}
        "wait" => {
            let valued = ["--on", "--timeout", "--why", "--session", "--agent"];
            let a = parse(rest, &valued, &[]).map_err(usage)?;
            let mut p = json!({
                "on": need(&a, "--on")?, "timeout": a.get("--timeout").unwrap_or("6h"),
                "why": a.get("--why").unwrap_or_default(), "note": a.rest,
                "via": env("KITH_VIA").filter(|v| !v.is_empty()).unwrap_or_else(|| "terminal".into()),
            });
            let by = [
                (
                    "session",
                    a.get("--session")
                        .map(str::to_owned)
                        .or_else(|| env("CLAUDE_CODE_SESSION_ID")),
                ),
                (
                    "agent",
                    a.get("--agent")
                        .map(str::to_owned)
                        .or_else(|| env("KITH_AGENT")),
                ),
                ("person", env("KITH_PERSON")),
            ];
            for (key, val) in by {
                if let Some(v) = val.filter(|v| !v.is_empty()) {
                    p[key] = json!(v);
                }
            }
            lines(&call("wake.wait", &p)?, out);
        }
        "status" => {
            let a = parse(rest, &[], &["--json"]).map_err(usage)?;
            let id = a
                .rest
                .first()
                .ok_or_else(|| Stop::Usage("status ID [--json]".into()))?;
            let got = call("wake.queueStatus", &json!({ "id": id }))?;
            if a.has("--json") {
                // kith-wake's json.dumps: ", " and ": " between the keys
                let run = got
                    .get("run")
                    .filter(|r| !r.is_null())
                    .map_or_else(|| "null".to_owned(), Value::to_string);
                let state = json!(text_of(&got, "state"));
                let _ = writeln!(
                    out,
                    "{{\"id\": {}, \"state\": {state}, \"run\": {run}}}",
                    json!(id)
                );
            } else {
                lines(&got, out);
            }
        }
        "ls" => {
            let a = parse(rest, &[], &["--json"]).map_err(usage)?;
            let got = call("wake.queueList", &json!({}))?;
            if a.has("--json") {
                let items = got.get("items").cloned().unwrap_or_else(|| json!([]));
                let _ = writeln!(out, "{}", pretty(&items));
            } else {
                lines(&got, out);
            }
        }
        other => return Err(Stop::Usage(format!("{other} is not a kith-wake verb"))),
    }
    Ok(0)
}

/// `json.dumps(v, indent=1)`.
fn pretty(v: &Value) -> String {
    use serde::Serialize;
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    if v.serialize(&mut ser).is_err() {
        return v.to_string();
    }
    String::from_utf8(buf).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests;
