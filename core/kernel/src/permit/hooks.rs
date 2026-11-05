//! Auto mode: a card for a call the classifier blocked (`denied`, the PermissionDenied hook), and the one-shot that lets its
//! retry through (`once`, the PreToolUse hook). Owner runs only: the ask token, never a guest's.

use super::Env;
use super::host::call_host;
use super::pyval::{dumps_sorted, strip, truthy};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The host waits its live window (default 90 s, at most 600) before it answers; a little longer than that.
pub const WAIT_S: u64 = 660;
const ONCE_KEEP_S: f64 = 24.0 * 3600.0;
/// What the classifier said when it gave no verdict. Claude Code ignores retry for these.
const NO_VERDICT: [&str; 2] = ["Auto mode could not evaluate", "Classifier unavailable"];
/// The note for a refused retry lives a minute; one call is told at most once in 10 minutes.
const NOTE_S: f64 = 60.0;
/// A yes to a no-verdict block is only good for the next few minutes.
const SHORT_ONCE_S: f64 = 600.0;
const TOLD_S: f64 = 600.0;

fn no_card(why: &str) -> String {
    format!(
        "Auto mode blocked this, and no card could be shown: {why}. Don't retry it or try another way around; tell the owner \
         what you needed and why."
    )
}

/// `json.dumps({'hookSpecificOutput': {...}})` of the PermissionDenied hook's retry.
const RETRY: &str =
    r#"{"hookSpecificOutput": {"hookEventName": "PermissionDenied", "retry": true}}"#;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn mtime(p: &Path) -> std::io::Result<f64> {
    let t = fs::metadata(p)?.modified()?;
    Ok(t.duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64()))
}

/// The one-shot's name: a hash of the tool and its input, so only the same call matches. A Bash call's description is the
/// model's own wording of it and changes between tries; it is not part of what was approved.
pub fn once_key(tool: &str, input: &Value) -> String {
    let mut ti = input.as_object().cloned().unwrap_or_default();
    if tool == "Bash" {
        ti.shift_remove("description");
    }
    let text = dumps_sorted(&json!([tool, Value::Object(ti)]));
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hook's JSON when it has a tool name and a tool input.
pub fn hook_input(raw: &str) -> Option<Map<String, Value>> {
    let Value::Object(d) = serde_json::from_str::<Value>(raw).ok()? else {
        return None;
    };
    let named = d
        .get("tool_name")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    (named && d.get("tool_input").is_some_and(Value::is_object)).then_some(d)
}

/// Records the one-shot in the run's own folder (made 0700). Files older than a day, and the folders of other runs that
/// are, are cleaned up on the way; the first thing that fails in that cleanup ends it, as in Python.
pub fn keep_once(once_dir: &Path, key: &str, now: f64, short: bool) -> std::io::Result<()> {
    // makedirs(mode=0o700): only the last folder gets the mode, the ones above it the default
    if let Some(parent) = once_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(once_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && once_dir.is_dir() => {}
        other => other?,
    }
    fs::set_permissions(once_dir, fs::Permissions::from_mode(0o700))?;
    let _ = clean_up(once_dir, now);
    let stamp = now.trunc() as i64;
    let mut f = fs::File::create(once_dir.join(key))?;
    f.write_all(
        if short {
            format!("{stamp} short\n")
        } else {
            format!("{stamp}\n")
        }
        .as_bytes(),
    )
}

fn clean_up(once_dir: &Path, now: f64) -> std::io::Result<()> {
    let trimmed = once_dir.to_string_lossy();
    let trimmed = trimmed.trim_end_matches('/');
    let parent = Path::new(trimmed).parent().unwrap_or(Path::new(""));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let mine = crate::paths::realpath(once_dir);
    for e in fs::read_dir(parent)? {
        let sib = e?.path();
        if crate::paths::realpath(&sib) != mine && now - mtime(&sib)? > ONCE_KEEP_S {
            // rmtree(ignore_errors=True): a plain file or a link is left alone
            if fs::symlink_metadata(&sib).is_ok_and(|m| m.is_dir()) {
                let _ = fs::remove_dir_all(&sib);
            }
        }
    }
    for e in fs::read_dir(once_dir)? {
        let f = e?.path();
        if now - mtime(&f)? > ONCE_KEEP_S {
            fs::remove_file(&f)?;
        }
    }
    Ok(())
}

/// Leaves `text` for the PreToolUse hook to refuse the retry with, and invites the retry: RETRY, or '' when this call was
/// told within TOLD_S (a model that keeps retrying is not told again and again) or there is nowhere to write.
pub fn tell(once_dir: &Path, key: &str, text: &str, now: f64) -> String {
    if mtime(&once_dir.join(format!("told-{key}"))).is_ok_and(|t| now - t < TOLD_S) {
        return String::new();
    }
    let wrote = keep_once(once_dir, &format!("told-{key}"), now, false)
        .and_then(|()| fs::write(once_dir.join(format!("note-{key}")), text));
    if wrote.is_err() {
        return String::new();
    }
    RETRY.to_owned()
}

/// A reply that is a non-empty object (Python: `reply and ...`).
fn present(reply: Option<&Map<String, Value>>) -> Option<&Map<String, Value>> {
    reply.filter(|m| !m.is_empty())
}

/// The PermissionDenied hook's output (a JSON line, or '' for no opinion). A yes records a one-shot and invites the retry;
/// anything else is told to the model on that retry (see `tell`).
pub fn denied(raw: &str, once_dir: &Path, env: &Env) -> String {
    let Some(d) = hook_input(raw) else {
        return String::new();
    };
    let reason = match d.get("reason") {
        Some(Value::String(r)) if !strip(r).is_empty() => r.as_str(),
        _ => return String::new(),
    };
    if d.get("hook_event_name").and_then(Value::as_str) != Some("PermissionDenied") {
        return String::new();
    }
    if env.get("KITH_RUN_TOKEN").is_some() {
        return String::new();
    }
    let no_verdict = NO_VERDICT.iter().any(|p| reason.starts_with(p));
    let tool = d["tool_name"].as_str().unwrap_or_default();
    let key = once_key(tool, &d["tool_input"]);
    let (Some(token), Some(socket)) = (env.get("KITH_ASK_TOKEN"), env.get("KITH_ASK_SOCKET"))
    else {
        return if no_verdict {
            String::new()
        } else {
            tell(
                once_dir,
                &key,
                &no_card("this run has no way to reach the Kith app"),
                now(),
            )
        };
    };
    let mut mode = Map::new();
    mode.insert("reason".to_owned(), json!(reason));
    if let Some(Value::String(s)) = d.get("session_id").filter(|v| truthy(v)) {
        mode.insert("session".to_owned(), json!(s));
    }
    let mut body = Map::new();
    body.insert("tool".to_owned(), json!(tool));
    body.insert("input".to_owned(), d["tool_input"].clone());
    body.insert("autoMode".to_owned(), Value::Object(mode));
    // only the owner's pair is used, never a guest's
    let asker = Env::with(&[("KITH_ASK_TOKEN", token), ("KITH_ASK_SOCKET", socket)]);
    let reply = call_host(
        &asker,
        "requestPermission",
        &body,
        Duration::from_secs(WAIT_S),
    );
    let reply = present(reply.as_ref());
    let allowed = reply.is_some_and(|r| {
        r.get("ok") == Some(&Value::Bool(true))
            && r.get("behavior").and_then(Value::as_str) == Some("allow")
    });
    if !allowed {
        if no_verdict {
            return String::new();
        }
        let Some(r) = reply else {
            return tell(
                once_dir,
                &key,
                &no_card("the Kith host didn't answer"),
                now(),
            );
        };
        let said = [r.get("message"), r.get("error")]
            .into_iter()
            .flatten()
            .find(|v| truthy(v))
            .or_else(|| r.get("error"));
        let said = match said {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => "It was not approved.",
        };
        return tell(
            once_dir,
            &key,
            &format!("Auto mode blocked this. {said}"),
            now(),
        );
    }
    if keep_once(once_dir, &key, now(), no_verdict).is_err() {
        // nowhere to record it: the retry would be refused again, so don't invite it
        return String::new();
    }
    RETRY.to_owned()
}

/// The PreToolUse hook's output: allow (and use up the one-shot) when this exact call was approved; refuse it with the reason
/// when `denied` left a note for its retry a moment ago; else ''.
pub fn once(raw: &str, once_dir: &Path) -> String {
    let Some(d) = hook_input(raw) else {
        return String::new();
    };
    if d.get("hook_event_name").and_then(Value::as_str) != Some("PreToolUse") {
        return String::new();
    }
    let key = once_key(
        d["tool_name"].as_str().unwrap_or_default(),
        &d["tool_input"],
    );
    let path = once_dir.join(&key);
    match take_one_shot(&path) {
        Ok(true) => {}
        Ok(false) => return String::new(),
        Err(()) => {
            let note = once_dir.join(format!("note-{key}"));
            let read = || -> std::io::Result<(bool, String)> {
                let fresh = now() - mtime(&note)? <= NOTE_S;
                let text = fs::read_to_string(&note)?;
                fs::remove_file(&note)?;
                Ok((fresh, text))
            };
            let Ok((fresh, text)) = read() else {
                return String::new();
            };
            if !fresh || text.is_empty() {
                return String::new();
            }
            return crate::secrets::pyjson::dumps(&json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": text,
            }}));
        }
    }
    crate::secrets::pyjson::dumps(&json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "permissionDecisionReason": "kith: you allowed this once",
    }}))
}

/// Reads and removes the one-shot file: `Ok(true)` when it is good to use, `Ok(false)` when it was a short one that has
/// expired, `Err` when there is none (or it can't be read, removed or understood).
fn take_one_shot(path: &Path) -> Result<bool, ()> {
    let text = fs::read_to_string(path).map_err(|_| ())?;
    fs::remove_file(path).map_err(|_| ())?;
    let body: Vec<&str> = text.split_whitespace().collect();
    if body.len() == 2 && body[1] == "short" {
        let at: f64 = body[0].parse().map_err(|_| ())?;
        if now() - at > SHORT_ONCE_S {
            return Ok(false);
        }
    }
    Ok(true)
}
