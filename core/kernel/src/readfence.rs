//! `kith kernel readfence --root DIR ...`: `kernel/lib/kith_readfence.py` in Rust, the `PreToolUse` hook that keeps the file
//! tools (Read, Grep, Glob, LS, `NotebookRead`) of a guest run inside its folders. The hook JSON comes on stdin; the output is
//! a deny decision (one JSON line) or nothing, byte for byte what the Python prints. Fails closed like it: a payload that
//! cannot be read, or a path that cannot be placed, is refused. The Python stays for runs that already started with it.
//!
//! Known gaps against the Python, all on the side of refusing: a payload with a lone surrogate or a number Python reads as
//! `inf` is refused here (Python would check it); a `~` or `~user` with no home in the password database (Python keeps the
//! text and reads it as a plain name), a symlink chain past 40 links and a NUL byte in a path are refused.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::paths::realpath_strict;

/// What the check needs from its process.
pub struct Ctx {
    /// `$HOME` (None when unset).
    pub home: Option<String>,
    /// `$CLAUDE_CONFIG_DIR` (None or empty: `~/.claude`).
    pub config_dir: Option<String>,
    /// The working directory, None when it cannot be read.
    pub cwd: Option<String>,
}

impl Ctx {
    pub fn from_process() -> Self {
        Ctx {
            home: std::env::var("HOME").ok(),
            config_dir: std::env::var("CLAUDE_CONFIG_DIR").ok(),
            cwd: std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

const COULD_NOT_RUN: &str = "the read check could not run, so this read is refused";

fn param_of(tool: &str) -> Option<&'static str> {
    match tool {
        "Read" => Some("file_path"),
        "Grep" | "Glob" | "LS" => Some("path"),
        "NotebookRead" => Some("notebook_path"),
        _ => None,
    }
}

/// `json.dumps` of a deny decision (ASCII only, `", "` and `": "` separators), with the newline `print` adds.
fn deny(why: &str) -> String {
    let mut s = String::from(
        "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"deny\", \"permissionDecisionReason\": \"",
    );
    for c in why.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            '\u{8}' => s.push_str("\\b"),
            '\u{c}' => s.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e && c != '\u{7f}' => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    let _ = write!(s, "\\u{u:04x}");
                }
            }
            c => s.push(c),
        }
    }
    s.push_str("\"}}\n");
    s
}

fn inside(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{}/", root.trim_end_matches('/')))
}

#[cfg(unix)]
fn passwd_home() -> Option<String> {
    crate::threads::os::passwd_home().map(|h| h.to_string_lossy().into_owned())
}
#[cfg(unix)]
fn passwd_home_of(name: &str) -> Option<String> {
    crate::threads::os::passwd_home_of(name).map(|h| h.to_string_lossy().into_owned())
}
// No password database off unix: a `~` without a known home is refused.
#[cfg(not(unix))]
fn passwd_home() -> Option<String> {
    None
}
#[cfg(not(unix))]
fn passwd_home_of(_name: &str) -> Option<String> {
    None
}

/// `os.path.expanduser`, except that a `~` whose home cannot be found is an Err (Python keeps the text as a plain name).
fn expanduser(p: &str, ctx: &Ctx) -> Result<String, ()> {
    if !p.starts_with('~') {
        return Ok(p.to_owned());
    }
    let i = p.find('/').unwrap_or(p.len());
    let home = if i == 1 {
        match &ctx.home {
            Some(h) => Some(h.clone()),
            None => passwd_home(),
        }
    } else {
        passwd_home_of(&p[1..i])
    };
    let h = home.ok_or(())?;
    let r = format!("{}{}", h.trim_end_matches('/'), &p[i..]);
    Ok(if r.is_empty() { "/".to_owned() } else { r })
}

/// Python's `repr` of a float.
fn float_repr(f: f64) -> String {
    let e = format!("{f:e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let s = format!("{f}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        let mant = if mant.contains('.') {
            mant.to_owned()
        } else {
            format!("{mant}.0")
        };
        let mant = mant.strip_suffix(".0").unwrap_or(&mant).to_owned();
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut o = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c == q => {
                o.push('\\');
                o.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(o, "\\x{:02x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push(q);
    o
}

/// Python's `str()` of a decoded JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n
            .as_f64()
            .filter(|_| !n.is_i64() && !n.is_u64())
            .map_or_else(|| n.to_string(), float_repr),
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::Bool(true) => true,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The file Claude Code itself wrote for a long tool result: `<config>/projects/<cwd>/<session>/tool-results/<file>`.
fn own_results(path: &str, ctx: &Ctx) -> Result<bool, ()> {
    let config = match ctx.config_dir.as_deref() {
        Some(c) if !c.is_empty() => c.to_owned(),
        _ => expanduser("~/.claude", ctx)?,
    };
    let cfg = realpath_strict(Path::new(&config))
        .ok_or(())?
        .to_string_lossy()
        .into_owned();
    let prefix = if cfg == "/" { cfg } else { format!("{cfg}/") };
    let Some(rel) = path.strip_prefix(&prefix) else {
        return Ok(false);
    };
    let parts: Vec<&str> = rel.split('/').collect();
    Ok(parts.len() == 5
        && parts[0] == "projects"
        && parts[3] == "tool-results"
        && parts.iter().all(|p| !p.is_empty()))
}

/// Err is the "could not run" case; Ok(None) lets the call through.
fn check(payload: &Map<String, Value>, roots: &[String], ctx: &Ctx) -> Result<Option<String>, ()> {
    let param = match payload.get("tool_name") {
        // An unhashable tool name raises in the Python.
        Some(Value::Array(_) | Value::Object(_)) => return Err(()),
        Some(Value::String(t)) => param_of(t),
        _ => None,
    };
    let Some(param) = param else {
        return Ok(None);
    };
    let input = match payload.get("tool_input") {
        None => Map::new(),
        Some(v) if !truthy(v) => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err(()),
    };
    let given = input.get(param).cloned().unwrap_or(Value::Null);
    let cwd_val = payload.get("cwd").filter(|v| truthy(v)).cloned();
    let cwd_str = |v: &Option<Value>| -> Result<String, ()> {
        match v {
            Some(v) => Ok(py_str(v)),
            None => ctx.cwd.clone().ok_or(()),
        }
    };
    let given = if given.is_null() || given == Value::String(String::new()) {
        if param != "path" {
            return Ok(Some(deny("no path to check")));
        }
        cwd_str(&cwd_val)?
    } else {
        py_str(&given)
    };
    let given = expanduser(&given, ctx)?;
    let joined = if given.starts_with('/') {
        given
    } else {
        match &cwd_val {
            Some(Value::String(_)) | None => {}
            Some(_) => return Err(()),
        }
        let cwd = cwd_str(&cwd_val)?;
        if cwd.ends_with('/') {
            format!("{cwd}{given}")
        } else {
            format!("{cwd}/{given}")
        }
    };
    if joined.contains('\0') {
        return Err(());
    }
    let path = realpath_strict(Path::new(&joined))
        .ok_or(())?
        .to_string_lossy()
        .into_owned();
    for r in roots {
        if r.contains('\0') {
            return Err(());
        }
        let root = realpath_strict(&PathBuf::from(r)).ok_or(())?;
        if inside(&path, &root.to_string_lossy()) {
            return Ok(None);
        }
    }
    if own_results(&path, ctx)? {
        return Ok(None);
    }
    Ok(Some(deny(&format!(
        "outside the folders this run was given ({path})"
    ))))
}

/// `--root DIR` pairs, read the way the Python's `enumerate` does (so `--root --root x` has two).
pub fn roots_of(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(i, a)| a.as_str() == "--root" && i + 1 < args.len())
        .map(|(i, _)| args[i + 1].clone())
        .collect()
}

/// The hook: the command-line arguments and stdin in, what to print out.
pub fn run(ctx: &Ctx, args: &[String], stdin: &[u8]) -> String {
    let roots = roots_of(args);
    let decision = match serde_json::from_slice::<Value>(stdin) {
        Ok(Value::Object(p)) => check(&p, &roots, ctx),
        _ => Err(()),
    };
    match decision {
        Ok(d) => d.unwrap_or_default(),
        Err(()) => deny(COULD_NOT_RUN),
    }
}
