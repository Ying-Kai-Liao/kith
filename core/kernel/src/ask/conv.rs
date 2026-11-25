//! The conversations `kith ask` keeps (`<machine>/ask.json`: agent + directory, or `--as NAME` → session id), as
//! `kernel/bin/kith-ask` keeps them. Not rows: `-c` resumes what the caller's directory last said to an agent, which is the
//! caller's own file and not the ledger's (the design keeps `ls` and `-c`).

use std::io::Write;

use fs2::FileExt as _;
use serde_json::{Map, Value, json};

use super::{Host, dumps};
use crate::secrets::pyjson;

fn file(host: &Host) -> std::path::PathBuf {
    host.machine.join("ask.json")
}

/// `load()`: the kept conversations, nothing when the file is missing or damaged.
pub(super) fn load(host: &Host) -> Map<String, Value> {
    std::fs::read_to_string(file(host))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

/// `keep(key, value)`: one conversation set, under `ask.lock`, the file replaced whole.
pub(super) fn keep(host: &Host, key: &str, value: Value) -> std::io::Result<()> {
    std::fs::create_dir_all(&host.machine)?;
    let lock = std::fs::File::create(host.machine.join("ask.lock"))?;
    lock.lock_exclusive()?;
    let mut kept = load(host);
    kept.insert(key.to_owned(), value);
    let tmp = host.machine.join("ask.tmp");
    std::fs::write(&tmp, pyjson::dumps_indent_n(&Value::Object(kept), 1))?;
    std::fs::rename(&tmp, file(host))?;
    drop(lock);
    Ok(())
}

fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

#[allow(clippy::cast_possible_truncation)]
fn turns(v: &Value) -> i64 {
    num(v, "turns") as i64
}

/// `kith ask ls [--json]`: the conversations kept, newest first.
pub(super) fn ls(host: &Host, as_json: bool, out: &mut dyn Write) -> i32 {
    let mut kept: Vec<(String, Value)> = load(host).into_iter().collect();
    let at = |v: &Value| v.get("at").and_then(Value::as_str).unwrap_or("").to_owned();
    // `sorted(..., reverse=True)` keeps equal keys in their first order.
    kept.sort_by_key(|a| std::cmp::Reverse(at(&a.1)));
    if as_json {
        let rows: Vec<Value> = kept
            .iter()
            .map(|(key, v)| {
                json!({
                    "key": key,
                    "agent": v.get("agent").cloned().unwrap_or(Value::Null),
                    "dir": key.split_once('|').map_or(key.as_str(), |(_, d)| d),
                    "at": v.get("at").cloned().unwrap_or(Value::Null),
                    "turns": v.get("turns").cloned().unwrap_or(json!(0)),
                    "cost": v.get("cost").cloned().unwrap_or(json!(0)),
                    "last": v.get("last").cloned().unwrap_or(json!("")),
                })
            })
            .collect();
        let _ = writeln!(out, "{}", dumps(&json!({ "conversations": rows })));
        return 0;
    }
    if kept.is_empty() {
        let _ = writeln!(out, "no conversations yet: kith ask <agent> \"task\"");
    }
    let home = host.home.display().to_string();
    for (key, v) in &kept {
        let dir = key.split_once('|').map_or(key.as_str(), |(_, d)| d);
        let dir = if home.is_empty() {
            dir.to_owned()
        } else {
            dir.replacen(&home, "~", 1)
        };
        let text = |k: &str, d: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map_or_else(|| d.to_owned(), str::to_owned)
        };
        let _ = writeln!(
            out,
            "{}  {:<10} {:>3} turn(s)  ${:<6.2} {}",
            text("at", "?"),
            text("agent", "?"),
            turns(v),
            num(v, "cost"),
            dir
        );
        let last: String = text("last", "").chars().take(110).collect();
        let _ = writeln!(out, "    {last}");
    }
    0
}
