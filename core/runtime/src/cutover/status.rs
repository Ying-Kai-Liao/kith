//! `kith kernel cutover status MODULE | --json | --require FILE` (retire-python Step 0): the questions a release gate asks.
//!
//! Read-only and lock-free, like the plain `status`. A gate is stricter than the router: the router counts a damaged switch
//! file as switched (Python must not write), a gate must not -- so "ok" here means the record says `switched` AND every
//! tombstone is in place. `step` records `switched` before it tombstones, so a crash between the two leaves a record that
//! says switched while the old reader's file is still live (`unfinished`).
//!
//! The plain `status` (no arguments) is not handled here and stays as it was.

use serde_json::{Map, Value, json};

use super::state::{self, Phase};
use super::{Env, Module, tombstone};
use crate::error::{Error, Result};

/// What a command prints and how it ends: stdout lines, stderr lines, exit code. `--json` always goes to stdout, even when
/// the exit code says no (the publish gate and the installer read stdout).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub out: Vec<String>,
    pub err: Vec<String>,
}

enum Gate {
    Switched(i64),
    /// Recorded switched, but this path is still the old reader's file.
    Unfinished(String),
    Stepping(i64),
    RollingBack(i64),
    NotSwitched,
    /// The switch file cannot be read; the message names it.
    Damaged(String),
    /// Not a module of this program (only `--require` reaches this; `status MODULE` errors).
    Unknown,
}

impl Gate {
    fn of(m: &dyn Module, env: &Env) -> Self {
        match state::read(&env.layout, m.name()) {
            Err(e) => Self::Damaged(e.to_string()),
            Ok(None) => Self::NotSwitched,
            Ok(Some(s)) => match s.phase {
                Phase::Stepping => Self::Stepping(s.at),
                Phase::RollingBack => Self::RollingBack(s.at),
                Phase::Switched => m
                    .tombstones(env)
                    .iter()
                    .find(|p| tombstone::module_of(p).as_deref() != Some(m.name()))
                    .map_or(Self::Switched(s.at), |p| {
                        Self::Unfinished(p.display().to_string())
                    }),
            },
        }
    }

    fn ok(&self) -> bool {
        matches!(self, Self::Switched(_))
    }

    fn word(&self) -> &'static str {
        match self {
            Self::Switched(_) => "switched",
            Self::Unfinished(_) => "unfinished",
            Self::Stepping(_) => "stepping",
            Self::RollingBack(_) => "rolling-back",
            Self::NotSwitched => "not switched",
            Self::Damaged(_) => "damaged",
            Self::Unknown => "unknown",
        }
    }

    /// `at` is 0 when the file's time did not parse: say nothing rather than 1970.
    fn since(&self) -> Option<i64> {
        match self {
            Self::Switched(at) | Self::Stepping(at) | Self::RollingBack(at) => {
                Some(*at).filter(|a| *a > 0)
            }
            _ => None,
        }
    }

    /// One line; when the module is not ok it ends with the step to run.
    fn line(&self, name: &str) -> String {
        let since = self
            .since()
            .map_or(String::new(), |a| format!(" (since {a})"));
        match self {
            Self::Switched(_) => format!("{name}: switched{since}"),
            Self::Unfinished(p) => format!(
                "{name}: switched but not finished ({p} is not a tombstone); run: kith migrate step {name}"
            ),
            Self::Stepping(_) => {
                format!("{name}: stepping{since}; run: kith migrate step {name}")
            }
            Self::RollingBack(_) => {
                format!("{name}: rolling-back{since}; run: kith rollback {name}")
            }
            Self::NotSwitched => format!("{name}: not switched; run: kith migrate step {name}"),
            Self::Damaged(why) => format!("{name}: switch record damaged ({why})"),
            Self::Unknown => {
                format!("{name}: unknown module (this kith program lacks it; run: kith update)")
            }
        }
    }

    /// `state`, `since` (when known) and `ok`, in that order.
    fn fields(&self) -> Map<String, Value> {
        let mut o = Map::new();
        o.insert("state".into(), json!(self.word()));
        if let Some(a) = self.since() {
            o.insert("since".into(), json!(a));
        }
        o.insert("ok".into(), json!(self.ok()));
        o
    }

    fn json(&self, name: &str) -> Value {
        let mut o = Map::new();
        o.insert("module".into(), json!(name));
        o.extend(self.fields());
        Value::Object(o)
    }
}

enum Want {
    One(String),
    All,
    Require(String),
}

fn usage() -> Error {
    Error::BadParams(
        "usage: cutover status [--json] | status MODULE [--json] | status --require FILE [--json]"
            .into(),
    )
}

/// `rest` is what follows `status` (never empty: the plain status is the caller's).
pub(crate) fn run(reg: &[Box<dyn Module>], env: &Env, rest: &[String]) -> Result<Outcome> {
    let (mut json, mut require, mut words) = (false, None, Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--require" => match it.next() {
                Some(f) if !f.starts_with("--") && require.is_none() => {
                    require = Some(f.clone());
                }
                _ => return Err(usage()),
            },
            w if w.starts_with("--") => return Err(usage()),
            w => words.push(w.to_owned()),
        }
    }
    let want = match (words.as_slice(), require) {
        ([], None) => Want::All,
        ([m], None) => Want::One(m.clone()),
        ([], Some(f)) => Want::Require(f),
        _ => return Err(usage()),
    };
    let answer = match want {
        Want::All => return Ok(all(reg, env)),
        Want::One(m) => one(reg, env, &m, json),
        Want::Require(f) => require_file(reg, env, &f, json),
    };
    // With --json a caller reads stdout, so an unknown module or an unreadable file is JSON there too (exit 1).
    match answer {
        Err(e) if json => Ok(Outcome {
            code: 1,
            out: vec![json!({ "ok": false, "error": e.to_string() }).to_string()],
            err: Vec::new(),
        }),
        other => other,
    }
}

fn all(reg: &[Box<dyn Module>], env: &Env) -> Outcome {
    // `--json` is the only way here (the plain status never gets this far), so it always prints JSON.
    let mut mods = Map::new();
    for m in reg {
        mods.insert(
            m.name().to_owned(),
            Value::Object(Gate::of(m.as_ref(), env).fields()),
        );
    }
    Outcome {
        code: 0,
        out: vec![json!({ "modules": mods }).to_string()],
        err: Vec::new(),
    }
}

fn one(reg: &[Box<dyn Module>], env: &Env, name: &str, json: bool) -> Result<Outcome> {
    let m = super::find(reg, name)?;
    let g = Gate::of(m, env);
    let code = i32::from(!g.ok());
    Ok(if json {
        Outcome {
            code,
            out: vec![g.json(name).to_string()],
            err: Vec::new(),
        }
    } else if g.ok() {
        Outcome {
            code,
            out: vec![g.line(name)],
            err: Vec::new(),
        }
    } else {
        Outcome {
            code,
            out: Vec::new(),
            err: vec![g.line(name)],
        }
    })
}

/// One module name per line; `#` starts a comment (whole line or after the name); blank lines are skipped.
fn module_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let name = line.split('#').next().unwrap_or("").trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

fn require_file(reg: &[Box<dyn Module>], env: &Env, file: &str, json: bool) -> Result<Outcome> {
    let text =
        std::fs::read_to_string(file).map_err(|e| Error::Io(format!("cutover: {file}: {e}")))?;
    let names = module_list(&text);
    let gates: Vec<(String, Gate)> = names
        .into_iter()
        .map(|n| {
            let g = reg
                .iter()
                .find(|m| m.name() == n)
                .map_or(Gate::Unknown, |m| Gate::of(m.as_ref(), env));
            (n, g)
        })
        .collect();
    let ok = gates.iter().all(|(_, g)| g.ok());
    let code = i32::from(!ok);
    if json {
        let required: Vec<Value> = gates.iter().map(|(n, g)| g.json(n)).collect();
        return Ok(Outcome {
            code,
            out: vec![json!({ "ok": ok, "required": required }).to_string()],
            err: Vec::new(),
        });
    }
    Ok(if ok {
        Outcome {
            code,
            out: vec![format!("all {} required modules switched", gates.len())],
            err: Vec::new(),
        }
    } else {
        Outcome {
            code,
            out: Vec::new(),
            err: gates
                .iter()
                .filter(|(_, g)| !g.ok())
                .map(|(n, g)| g.line(n))
                .collect(),
        }
    })
}
