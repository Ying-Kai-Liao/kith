//! The CLI, derived from the table: a command line in, a method and its params out.
//!
//! This crate only parses. The real `kith` binary still parses with clap and the kernel's own scripts; M0c wires
//! the daemon and the CLI to this parser. Until then it is the row's CLI shape made executable, and
//! `tests/table.rs` holds it to `contract.json` and to the B6 difftest corpus.

use serde_json::{Map, Value, json};

use crate::{Arg, METHODS, Method, Param};

/// A command line read against the table.
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub method: &'static Method,
    pub params: Value,
    /// The presentation switch given (`--json`, `--sh`), if the verb takes one.
    pub format: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// No row spells this command: the caller falls back to what it did before the table.
    NotInTable,
    /// A row matched and the rest of the line does not fit it.
    Usage { method: &'static str, why: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotInTable => f.write_str("no such method"),
            ParseError::Usage { method, why } => write!(f, "{method}: {why}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The row a command line is for: the one whose spelling (path or alias) is the longest prefix of `argv` and whose
/// required switches are all there. `argv` leaves out the program name.
pub fn find<S: AsRef<str>>(argv: &[S]) -> Option<(&'static Method, usize)> {
    let mut best: Option<(&'static Method, usize)> = None;
    for m in METHODS {
        for spelling in std::iter::once(m.cli.path).chain(m.cli.aliases.iter().copied()) {
            let n = spelling.len();
            let starts =
                argv.len() >= n && spelling.iter().zip(argv).all(|(w, a)| *w == a.as_ref());
            let has_required = m
                .cli
                .requires
                .iter()
                .all(|r| argv[n.min(argv.len())..].iter().any(|a| a.as_ref() == *r));
            if starts && has_required && best.is_none_or(|(_, len)| n > len) {
                best = Some((m, n));
            }
        }
    }
    best
}

/// Reads a command line (without the program name) as a method and its params.
pub fn parse_argv<S: AsRef<str>>(argv: &[S]) -> Result<Parsed, ParseError> {
    let (method, skip) = find(argv).ok_or(ParseError::NotInTable)?;
    parse_args(method, argv[skip..].iter().map(AsRef::as_ref))
}

/// Reads the words after a row's path against that row.
pub fn parse_args<'a>(
    method: &'static Method,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Parsed, ParseError> {
    let usage = |why: String| ParseError::Usage {
        method: method.name,
        why,
    };
    let mut out = Map::new();
    let mut format = None;
    let mut seen_required = vec![false; method.cli.requires.len()];
    let mut next_pos = method.params.iter().filter(|p| p.arg == Arg::Pos);
    let mut tail: Vec<Value> = Vec::new();
    let mut tail_param: Option<&Param> = None;
    let mut args = args.into_iter();

    while let Some(a) = args.next() {
        if let Some(i) = method.cli.requires.iter().position(|r| *r == a) {
            seen_required[i] = true;
        } else if method.cli.formats.contains(&a) {
            format = Some(a.to_owned());
        } else if a == "--" {
            let p = method
                .params
                .iter()
                .find(|p| p.arg == Arg::After)
                .ok_or_else(|| usage("takes no arguments after `--`".into()))?;
            tail.extend(args.by_ref().map(Value::from));
            tail_param = Some(p);
        } else if a.starts_with('-') && a.len() > 1 {
            let (name, inline) = match a.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v)),
                _ => (a, None),
            };
            let p = method
                .params
                .iter()
                .find(|p| match p.arg {
                    Arg::Flag(s) | Arg::Switch(s) => s.contains(&name),
                    _ => false,
                })
                .ok_or_else(|| usage(format!("unknown flag {name}")))?;
            let value = match p.arg {
                Arg::Switch(_) => Value::Bool(true),
                _ => {
                    let raw = match inline {
                        Some(v) => v,
                        None => args
                            .next()
                            .ok_or_else(|| usage(format!("{name} needs a value")))?,
                    };
                    typed(p, raw).map_err(|why| usage(format!("{name}: {why}")))?
                }
            };
            out.insert(p.name.to_owned(), value);
        } else if let Some(p) = next_pos.next() {
            out.insert(
                p.name.to_owned(),
                typed(p, a).map_err(|why| usage(format!("{}: {why}", p.name)))?,
            );
        } else if let Some(p) = method.params.iter().find(|p| p.arg == Arg::Words) {
            tail_param = Some(p);
            tail.push(a.into());
        } else if let Some(p) = method.params.iter().find(|p| p.arg == Arg::Rest) {
            // Passed through as written, flags and all.
            tail_param = Some(p);
            tail.push(a.into());
            tail.extend(args.by_ref().map(Value::from));
        } else {
            return Err(usage(format!("unexpected argument {a}")));
        }
    }
    if let Some(p) = tail_param {
        out.insert(p.name.to_owned(), Value::Array(tail));
    }
    if let Some(i) = seen_required.iter().position(|s| !s) {
        return Err(usage(format!("needs {}", method.cli.requires[i])));
    }
    if let Some(p) = method
        .params
        .iter()
        .find(|p| p.required && !out.contains_key(p.name))
    {
        return Err(usage(format!("needs {}", p.name)));
    }
    Ok(Parsed {
        method,
        params: Value::Object(out),
        format,
    })
}

fn typed(p: &Param, raw: &str) -> Result<Value, String> {
    match p.ty {
        "int" => raw
            .parse::<i64>()
            .map(|n| json!(n))
            .map_err(|_| format!("{raw} is not a whole number")),
        _ => Ok(raw.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Parsed, ParseError> {
        parse_argv(&line.split(' ').collect::<Vec<_>>())
    }

    #[test]
    fn the_longest_path_wins_and_aliases_agree() {
        assert_eq!(parse("paths --json").unwrap().method.name, "paths");
        assert_eq!(parse("kernel paths --sh").unwrap().method.name, "paths");
        assert_eq!(
            parse("kernel engines install claude").unwrap().method.name,
            "kernel.engines.install"
        );
        assert_eq!(
            parse("kernel engines --json").unwrap().method.name,
            "kernel.engines.list"
        );
        assert_eq!(parse("ls --json").unwrap().method.name, "agents.list");
    }

    #[test]
    fn a_verb_not_in_the_table_is_not_an_error() {
        assert_eq!(parse("frobnicate now"), Err(ParseError::NotInTable));
        // `ls` without its `--json` is the human listing, which no row spells.
        assert_eq!(parse("ls"), Err(ParseError::NotInTable));
    }

    #[test]
    fn params_flags_switches_and_formats() {
        let p =
            parse("kernel launch settings echo --person p1 --snapshot --kith-bin=/bin/kith --json")
                .unwrap();
        assert_eq!(p.method.name, "kernel.launch.settings");
        assert_eq!(p.format.as_deref(), Some("--json"));
        assert_eq!(
            p.params,
            json!({"person": "p1", "snapshot": true, "kithBin": "/bin/kith", "agent": "echo"})
        );
    }

    #[test]
    fn typed_flags_and_usage_errors() {
        let p = parse("backup add codex --at 1 --json").unwrap();
        assert_eq!(p.params, json!({"entry": "codex", "at": 1}));
        assert!(matches!(
            parse("backup add codex --at x"),
            Err(ParseError::Usage { .. })
        ));
        assert!(
            matches!(parse("kernel launch prompt"), Err(ParseError::Usage { .. })),
            "agent is required"
        );
        assert!(matches!(
            parse("kernel launch prompt echo --nope"),
            Err(ParseError::Usage { .. })
        ));
        assert!(matches!(parse("pin a b c"), Err(ParseError::Usage { .. })));
    }

    #[test]
    fn rest_is_passed_through_and_after_takes_the_task() {
        let p = parse("kernel secrets --kernel /k resolve echo --person p1 --json").unwrap();
        assert_eq!(
            p.params,
            json!({"kernel": "/k", "args": ["resolve", "echo", "--person", "p1", "--json"]})
        );
        let p = parse("kernel run-once claude --approve -- say hi").unwrap();
        assert_eq!(
            p.params,
            json!({"engine": "claude", "approve": true, "task": ["say", "hi"]})
        );
        let p = parse("bug A bug --details it --json").unwrap();
        assert_eq!(p.params, json!({"title": ["A", "bug"], "details": "it"}));
    }
}
