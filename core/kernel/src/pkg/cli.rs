//! `kith kernel pkg <verb> ...`: the verbs of `kernel/bin/kith-pkg` that run in Rust, with its arguments, output and exit codes.
//!
//! ```text
//! resolve <agent>|--all [--json]   the pinned package dir to run (makes the snapshot if the agent changed)
//! ```
//!
//! One line of stdout per agent (the folder), or with `--json` one object, `{agent: {dir, version, note}}`; the note of an
//! agent that does not run the latest edits goes to stderr as `kith: AGENT: NOTE` (not with `--json`). An agent whose snapshot
//! throws runs from its live folder instead, so one broken package never blocks the others. Exit 2 for no agent given, 1 for an
//! agent that is not installed.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::py::{Kind, PyErr};
use super::resolve::resolve;
use super::{Ctx, installed, name_of};
use crate::secrets::pyjson;

/// What a verb printed and how it ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

const USAGE: &str = "usage: kith kernel pkg resolve <agent>|--all [--json]";

/// `kith kernel pkg ARGS...`.
pub fn run(ctx: &Ctx, args: &[String]) -> Out {
    let Some((cmd, rest)) = args.split_first() else {
        return Out {
            stderr: format!("{USAGE}\n"),
            code: 2,
            ..Out::default()
        };
    };
    if cmd != "resolve" {
        return Out {
            stderr: format!("kith kernel pkg: {cmd} is not in the core yet (kernel/bin/kith-pkg answers it)\n{USAGE}\n"),
            code: 2,
            ..Out::default()
        };
    }
    let as_json = rest.iter().any(|a| a == "--json");
    let rest: Vec<&String> = rest.iter().filter(|a| *a != "--json").collect();
    let mut out = Out::default();
    let all = rest.len() == 1 && rest[0] == "--all";
    let names: Vec<String> = if all {
        installed(ctx).into_keys().collect()
    } else {
        rest.iter().take(1).map(|s| (*s).clone()).collect()
    };
    if names.is_empty() {
        return Out {
            stderr: format!("{}\n", PyErr::die("resolve <agent>|--all", 2)),
            code: 2,
            ..Out::default()
        };
    }
    let mut found = Map::new();
    let mut dirs: Vec<String> = vec![];
    for name in &names {
        let mut err = String::new();
        let (pkg, note) = match resolve(ctx, name, &mut err) {
            Ok(r) => r,
            Err(e) => {
                out.stderr.push_str(&err);
                if let Kind::Exit(code) = e.kind {
                    out.stderr.push_str(&format!("{e}\n"));
                    out.code = code;
                    return out;
                }
                // one broken agent must not block the others; run it live instead
                let live = installed(ctx).remove(name).unwrap_or_default();
                (
                    live,
                    format!("snapshot failed ({e}); running the live folder"),
                )
            }
        };
        out.stderr.push_str(&err);
        let dir = pkg.to_string_lossy().into_owned();
        let version = version_of(&pkg);
        if !note.is_empty() && !as_json {
            out.stderr.push_str(&format!("kith: {name}: {note}\n"));
        }
        found.insert(name.clone(), json!({"dir": dir, "version": version, "note": note}));
        dirs.push(dir);
    }
    out.stdout = if as_json {
        format!("{}\n", pyjson::dumps(&Value::Object(found)))
    } else {
        format!("{}\n", dirs.join("\n"))
    };
    out
}

/// `pkg.name.split('@', 1)[-1]`.
fn version_of(pkg: &Path) -> String {
    let n = name_of(pkg);
    n.split_once('@').map_or(n.clone(), |(_, v)| v.to_owned())
}
