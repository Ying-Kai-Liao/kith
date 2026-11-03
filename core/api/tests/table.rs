#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::case_sensitive_file_extension_comparisons
)]

//! The table against its seeds and its generated documents.

use std::collections::BTreeSet;
use std::path::PathBuf;

use kith_api::cli::{ParseError, parse_argv};
use kith_api::{Arg, METHODS, Principal, compat, enums, method, named_types, openrpc, schema};
use serde_json::{Value, json};

fn repo(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn read_json(path: &str) -> Value {
    let text = std::fs::read_to_string(repo(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn every_row_is_well_formed() {
    let mut names = BTreeSet::new();
    let mut spellings = BTreeSet::new();
    let known: BTreeSet<&str> = enums()
        .iter()
        .map(|e| e.name)
        .chain(named_types().map(|t| t.name))
        .collect();
    let in_known = |expr: &str, what: &str| {
        assert!(
            schema::parse(expr).is_some(),
            "{what}: `{expr}` is not a type"
        );
        let mut refs = Vec::new();
        schema::names(expr, &mut refs);
        for r in refs {
            assert!(known.contains(r), "{what}: `{r}` is not a named type");
        }
    };
    for m in METHODS {
        assert!(names.insert(m.name), "{} is twice in the table", m.name);
        assert!(
            m.name
                .split('.')
                .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric())),
            "{}",
            m.name
        );
        assert!(!m.summary.is_empty(), "{} has no summary", m.name);
        // Until M2 only the owner may call anything.
        assert_eq!(m.principals, &[Principal::Owner], "{}", m.name);
        in_known(m.result, m.name);
        let mut pnames = BTreeSet::new();
        for p in m.params {
            assert!(pnames.insert(p.name), "{}: param {} twice", m.name, p.name);
            assert!(
                p.name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase())
                    && !p.name.contains(['_', '-']),
                "{}: {} is not camelCase",
                m.name,
                p.name
            );
            in_known(p.ty, &format!("{}.{}", m.name, p.name));
            if matches!(p.arg, Arg::Rest | Arg::Words | Arg::After) {
                assert_eq!(p.ty, "[string]", "{}.{}", m.name, p.name);
            }
        }
        for spelling in std::iter::once(m.cli.path).chain(m.cli.aliases.iter().copied()) {
            let key = format!("{spelling:?}+{:?}", m.cli.requires);
            assert!(
                spellings.insert(key),
                "{}: another row has the same command",
                m.name
            );
        }
    }
}

#[test]
fn the_contract_is_what_types_rs_says() {
    let c = read_json("serve/contract/kernel/contract.json");
    let want: BTreeSet<String> = c["enums"].as_object().unwrap().keys().cloned().collect();
    let have: BTreeSet<String> = enums().iter().map(|e| e.name.to_owned()).collect();
    assert_eq!(
        have, want,
        "enums: regenerate core/api/src/types.rs from contract.json"
    );
    for e in enums() {
        let w = &c["enums"][e.name];
        assert_eq!(json!(e.values), w["values"], "enum {}", e.name);
        assert_eq!(json!(e.unknown), w["unknown"], "enum {}", e.name);
        assert_eq!(json!(e.doc), w["doc"], "enum {}", e.name);
    }
    for (name, t) in c["types"].as_object().unwrap() {
        let have = named_types()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("type {name} is not in types.rs"));
        let fields: Vec<(String, String)> = t["fields"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect();
        let mine: Vec<(String, String)> = have
            .fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        assert_eq!(mine, fields, "type {name}");
        assert_eq!(
            json!(have.may_omit),
            t.get("mayOmit").cloned().unwrap_or_else(|| json!([])),
            "type {name}"
        );
        assert_eq!(json!(have.doc), t["doc"], "type {name}");
    }
}

#[test]
fn every_contract_verb_parses_to_its_row() {
    let c = read_json("serve/contract/kernel/contract.json");
    let rows = [
        ("ls", "agents.list"),
        ("projectLs", "projects.list"),
        ("launch", "agents.launch"),
        ("new", "agents.create"),
        ("delegations", "delegations.list"),
    ];
    let commands = c["commands"].as_object().unwrap();
    assert_eq!(
        commands.len(),
        rows.len(),
        "a contract verb has no row: {:?}",
        commands.keys().collect::<Vec<_>>()
    );
    for (key, name) in rows {
        let cmd = &commands[key];
        let argv: Vec<String> = cmd["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                a.as_str()
                    .unwrap()
                    .replace("{agent}", "echo")
                    .replace("{name}", "scout")
                    .replace("{description}", "scouts")
            })
            .collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{key} {argv:?}: {e}"));
        assert_eq!(parsed.method.name, name, "{key}");
        assert_eq!(
            json!(parsed.method.result),
            cmd["result"],
            "{key}: result type"
        );
        // Each option the contract names is a flag of the row, under the same spelling.
        for (param, flag) in cmd
            .get("options")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let flag = flag.as_str().unwrap();
            let p = parsed
                .method
                .params
                .iter()
                .find(|p| p.name == param)
                .unwrap_or_else(|| panic!("{key}: no param {param}"));
            assert!(
                matches!(p.arg, Arg::Flag(s) if s.contains(&flag)),
                "{key}: {param} is not {flag}"
            );
            let mut with = argv.clone();
            with.extend([flag.to_owned(), "v".to_owned()]);
            assert_eq!(
                parse_argv(&with).unwrap().params[param],
                "v",
                "{key} {flag}"
            );
        }
        assert!(!cmd["doc"].as_str().unwrap().is_empty());
    }
}

#[test]
fn every_b6_json_line_of_the_difftest_corpus_parses() {
    let dir = repo("kernel/tests/difftest/corpus");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !(name.starts_with("json-") && name.ends_with(".json")) {
            continue;
        }
        let cases: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for case in cases.as_array().unwrap() {
            for line in case["run"].as_array().unwrap() {
                let argv: Vec<&str> = line
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap())
                    .collect();
                if !argv.contains(&"--json") || NO_ROW.contains(&argv[0]) {
                    continue;
                }
                seen += 1;
                match parse_argv(&argv) {
                    Ok(_) => {}
                    Err(ParseError::NotInTable) => panic!("{} {argv:?}: no row", case["name"]),
                    Err(e) => panic!("{} {argv:?}: {e}", case["name"]),
                }
            }
        }
    }
    assert!(seen > 50, "the corpus lines moved? only {seen} read");
}

/// Verbs of the corpus with `--json` that have no row yet, and why.
const NO_ROW: &[&str] = &[];

#[test]
fn every_switched_verb_has_a_row() {
    // The clap definitions in serve/host/src/main.rs (`Sub::Paths`, `KernelCmd`, and its sub-enums), by CLI line.
    let lines: &[&str] = &[
        "paths",
        "paths --json",
        "kernel paths --sh",
        "kernel version",
        "kernel engines",
        "kernel engines --json",
        "kernel engines install claude",
        "kernel engines login claude",
        "kernel engines launch claude",
        "kernel launch prompt echo",
        "kernel launch settings echo",
        "kernel launch mcp echo",
        "kernel launch echo --launch-json",
        "kernel accounts backups-json echo /a/agent.md",
        "kernel accounts launch-env echo claude:work",
        "kernel accounts main-env echo /a/agent.md",
        "kernel accounts session-run s1",
        "kernel accounts session-account s1",
        "kernel accounts list",
        "kernel accounts ls --json",
        "kernel delegation limits",
        "kernel delegation cap",
        "kernel delegation record a1",
        "kernel delegation check",
        "kernel delegation parked a1",
        "kernel secrets resolve echo",
        "kernel threads resolve s1",
        "kernel run-once claude -- hi",
        "kernel core-migrate",
    ];
    for line in lines {
        let argv: Vec<&str> = line.split(' ').collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(parsed.method.served, kith_api::Served::Core, "{line}");
    }
}

#[test]
fn the_openrpc_file_is_the_table() {
    let fresh = openrpc::render(&openrpc::generate());
    let kept = std::fs::read_to_string(repo(openrpc::OPENRPC_PATH))
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        kept == fresh,
        "{} is out of date with core/api/src/table.rs. Regenerate: {}",
        openrpc::OPENRPC_PATH,
        openrpc::REGENERATE
    );
}

#[test]
fn generation_is_deterministic() {
    assert_eq!(
        openrpc::render(&openrpc::generate()),
        openrpc::render(&openrpc::generate())
    );
}

#[test]
fn the_table_breaks_nothing_released() {
    let released = read_json(openrpc::RELEASED_PATH);
    let broken = compat::breaks(&released, &openrpc::generate());
    assert!(
        broken.is_empty(),
        "the table breaks {}:\n  {}\nAdd instead of change, or cut a new major version first.",
        openrpc::RELEASED_PATH,
        broken.join("\n  ")
    );
}

#[test]
fn a_released_method_cannot_be_dropped_unseen() {
    // The break test is live against the real snapshot: take a stable method out of a fresh document and it fails.
    let released = read_json(openrpc::RELEASED_PATH);
    let mut doc = openrpc::generate();
    let methods = doc["methods"].as_array_mut().unwrap();
    let at = methods.iter().position(|m| m["name"] == "paths").unwrap();
    methods.remove(at);
    assert!(
        compat::breaks(&released, &doc)
            .iter()
            .any(|b| b.contains("method paths was removed"))
    );
}

#[test]
fn lookup_by_name() {
    assert!(method("kernel.version").is_some());
    assert!(method("nope").is_none());
}
