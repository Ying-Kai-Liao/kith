//! `run_door(door, path)`: runs a door of the run's pinned package (a fixed command in a sandbox of its own, e.g. video's render)
//! on one folder of the person's files. A door is granted only by a ticked job: one not ticked, or ticked at another version of
//! the door, is asked of the owner on a card whose only yes turns the job on, and the start checks the tick again under the
//! kith-auth lock. A door not prepared on this Mac asks the owner a second card before it downloads anything.
//!
//! The door itself (`kernel/lib/kith_door.py`: the declaration, the cache, the `sandbox-exec` profile, the process sweep) stays
//! Python's; this module is the tool around it. Each step that needs it is one `python3 -I -c` call with the run's own
//! environment, scrubbed there of anything that names a path in the guest's writable folders (`kith_paths.guest_scrub`), and the
//! cards are asked from here.

use super::Env;
use super::decide::{allowed, decide, message_of};
use super::proc::python;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Everything a step does first: the declaration, the folder, the tick. What the script prints is one of
/// `{"end": text}` (nothing more to do), `{"card": true, ...}` (the door's card is due) or `{"ok": true, ...}`.
pub(super) const CHECK: &str = r#"
import json, os, sys
sys.path.insert(0, sys.argv[1])
req = json.loads(sys.argv[2])
import kith_door
from kith_paths import guest_scrub
name, path, person, agent = req['door'], req['path'], req['person'], req['agent']
env = guest_scrub(dict(os.environ), person)
try:
    decl = kith_door.declared(os.path.join(env['KITH_AGENT_DIR'], 'capabilities.yaml'), name)
    real = os.path.realpath(path)
    if not (kith_door.in_slice(real, agent, person) and os.path.isdir(real)):
        out = {'end': "The folder must be one of %s's folders under files/people/%s." % (person, person)}
    else:
        v = kith_door.check(agent, person, kith_door.piece(decl), env)
        state = v.get('state')
        if state == 'held':
            out = {'end': 'Access is being changed right now; try again in a minute.'}
        elif state in ('not-ticked', 'widened'):
            out = {'card': True, 'real': real}
        elif not v.get('ok'):
            out = {'end': v.get('why') or 'This door is not on for %s.' % person}
        else:
            out = {'ok': True, 'real': real}
        if 'end' not in out:
            out.update(ready=bool(kith_door.ready(agent, decl)), downloads=kith_door.describe(decl))
except kith_door.DoorError as e:
    out = {'end': 'The door did not run: %s' % e}
print(json.dumps(out))
"#;

/// Prepares the door's cache (owner only), or runs it and says how it ended.
pub(super) const ACT: &str = r"
import json, os, sys
sys.path.insert(0, sys.argv[1])
req = json.loads(sys.argv[2])
import kith_door
from kith_paths import guest_scrub
name, path, person, agent = req['door'], req['path'], req['person'], req['agent']
env = guest_scrub(dict(os.environ), person)
try:
    decl = kith_door.declared(os.path.join(env['KITH_AGENT_DIR'], 'capabilities.yaml'), name)
    if req['act'] == 'prepare':
        kith_door.prepare(agent, decl, env=env)
        out = {'prepared': True}
    else:
        code, tail = kith_door.run(agent, person, decl, os.path.realpath(path), env)
        out = {'code': code, 'tail': tail}
except kith_door.DoorError as e:
    out = {'end': 'The door did not run: %s' % e}
print(json.dumps(out))
";

const CHECK_S: Duration = Duration::from_secs(120);
/// A door runs for an hour at most (`kith_door.RUN_LIMIT_S`), plus its clean-up.
const RUN_S: Duration = Duration::from_secs(3 * 3600);

/// `re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,63}', s)`.
fn is_door(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn step(env: &Env, kernel: &Path, script: &str, req: &Value, limit: Duration) -> Option<Value> {
    python(env, kernel, script, &req.to_string(), limit)
}

fn said(v: &Value) -> Option<String> {
    v.get("end").and_then(Value::as_str).map(str::to_owned)
}

const BROKEN: &str = "The door did not run: the door helper failed";

/// The text the agent is told for one `run_door` call: how the door ended and its output's tail, or why it didn't run.
pub fn run_door(a: &serde_json::Map<String, Value>, env: &Env, kernel: &Path) -> String {
    let (name, path) = (a.get("door"), a.get("path"));
    let (Some(Value::String(name)), Some(Value::String(path))) = (name, path) else {
        return "Give the door and the absolute path of the project folder.".to_owned();
    };
    if !(is_door(name) && path.starts_with('/')) {
        return "Give the door and the absolute path of the project folder.".to_owned();
    }
    let (Some(person), Some(agent), Some(_)) = (
        env.get("KITH_PERSON"),
        env.get("KITH_AGENT"),
        env.get("KITH_AGENT_DIR"),
    ) else {
        return "Doors are run from a guest's run only.".to_owned();
    };
    let mut req = json!({"door": name, "path": path, "person": person, "agent": agent});
    let Some(first) = step(env, kernel, CHECK, &req, CHECK_S) else {
        return BROKEN.to_owned();
    };
    if let Some(text) = said(&first) {
        return text;
    }
    let real = first.get("real").and_then(Value::as_str).unwrap_or(path);
    if first.get("card") == Some(&Value::Bool(true)) {
        let d = decide(
            env,
            kernel,
            "mcp__kithpermit__run_door",
            &json!({"door": name, "path": real}),
        );
        if !allowed(&d) {
            return message_of(&d);
        }
    }
    req["act"] = Value::String("prepare".to_owned());
    if first.get("ready") != Some(&Value::Bool(true)) {
        let downloads = first.get("downloads").cloned().unwrap_or(Value::Null);
        let d = decide(
            env,
            kernel,
            "mcp__kithpermit__prepare_door",
            &json!({"door": name, "downloads": downloads}),
        );
        if !allowed(&d) {
            return message_of(&d);
        }
        match step(env, kernel, ACT, &req, RUN_S) {
            Some(v) => {
                if let Some(text) = said(&v) {
                    return text;
                }
            }
            None => return BROKEN.to_owned(),
        }
    }
    req["act"] = Value::String("run".to_owned());
    let Some(done) = step(env, kernel, ACT, &req, RUN_S) else {
        return BROKEN.to_owned();
    };
    if let Some(text) = said(&done) {
        return text;
    }
    let tail = done.get("tail").and_then(Value::as_str).unwrap_or_default();
    match done.get("code").and_then(Value::as_i64) {
        Some(0) => format!("{name} ended. Its output, last lines:\n{tail}"),
        Some(code) => format!("{name} failed (exit {code}). Its output, last lines:\n{tail}"),
        None => format!("{name} was stopped. Its output, last lines:\n{tail}"),
    }
}
