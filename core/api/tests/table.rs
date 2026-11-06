#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::case_sensitive_file_extension_comparisons
)]

//! The table against its seeds and its generated documents.

use std::collections::BTreeSet;
use std::path::PathBuf;

use kith_api::cli::{ParseError, parse_args, parse_argv};
use kith_api::{
    Arg, METHODS, Principal, Served, Stability, compat, enums, method, named_types, openrpc, schema,
};
use serde_json::{Value, json};

/// The rows only the host may call (M2-10b): the principal check in `every_row_is_well_formed` pins Host to exactly
/// these, so a new row cannot gain Host unnoticed.
const HOST_ROWS: [&str; 7] = [
    "syncLease.link",
    "syncLease.unlink",
    "syncLease.changed",
    "syncLease.drop",
    "syncLease.pulled",
    "syncLease.renew",
    "schedule.fire",
];

/// The one row that hands a secret's value back (M2-10c): the host's, because its proof is process memory, where the
/// owner token is a file any process of the user can read. The owner keeps `secrets.open|has|put|delete`.
const SECRET_VALUE_ROW: &str = "secrets.get";

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
        // Until M2 only the owner may call anything, except the run socket's three rows (M0e): a run token reaches those
        // and nothing else, and no other principal reaches them.
        let on_the_run_socket = ["run.recall", "run.memorySearch", "run.projectId"];
        // The sync lease's writes are the host's (M2-10b); the sync-lease test below holds each of those rows.
        let want: &[Principal] = if on_the_run_socket.contains(&m.name) {
            &[Principal::RunToken]
        } else if HOST_ROWS.contains(&m.name)
            || m.name == SECRET_VALUE_ROW
            || m.name.starts_with("hostclaim.")
        {
            &[Principal::Host]
        // The host also reads two owner rows: the lease counter (`mark`) and the cutover gate (`cutover.mayServe`, which
        // the host-start guard calls). `kernel.cutover`, the write side, stays Owner only.
        } else if m.name == "syncLease.mark" || m.name == "cutover.mayServe" {
            &[Principal::Owner, Principal::Host]
        } else {
            &[Principal::Owner]
        };
        assert_eq!(m.principals, want, "{}", m.name);
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
            // A hidden (host-only) row has no command line, so it cannot collide.
            if m.cli.hidden {
                continue;
            }
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
        "kernel cutover status",
        "kernel core-status --json",
        "kernel core-daemon",
    ];
    for line in lines {
        let argv: Vec<&str> = line.split(' ').collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(parsed.method.served, Served::Core, "{line}");
    }
}

/// M3-10a: the cutover verbs switch whole modules and roll them back, so only the owner may call them; never a host
/// bridge, a run token or a guest.
#[test]
fn the_cutover_row_is_owner_only_and_experimental() {
    let m = method("kernel.cutover").expect("kernel.cutover has a row");
    assert_eq!(m.principals, &[Principal::Owner]);
    assert_eq!(m.stability, Stability::Experimental);
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

/// The M2 rows (M2-0b): experimental, owner only, and not in `released.json` until M2 cuts them. Answered by the Python
/// kernel unless the core serves them (the people rows of M2-2 and the access requests of M2-3, which the daemon answers).
const M2_STUBS: &[&str] = &[
    "people.grant",
    "people.revoke",
    "people.list",
    "people.requests",
    "people.approve",
    "people.deny",
];

#[test]
fn the_m2_stubs_are_experimental_owner_only_and_unreleased() {
    let released = read_json(openrpc::RELEASED_PATH);
    let released: BTreeSet<&str> = released["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for name in M2_STUBS {
        let m = method(name).unwrap_or_else(|| panic!("{name}: no row"));
        assert_eq!(m.stability, Stability::Experimental, "{name}");
        let core = [
            "people.grant",
            "people.revoke",
            "people.list",
            "people.requests",
            "people.approve",
            "people.deny",
        ]
        .contains(name);
        assert_eq!(
            m.served,
            if core { Served::Core } else { Served::Kernel },
            "{name}: who serves it"
        );
        assert_eq!(m.principals, &[Principal::Owner], "{name}");
        assert!(!released.contains(name), "{name} is in released.json");
    }
}

#[test]
fn the_people_rows_of_m2_2_are_served_experimental_owner_only_unreleased_and_parse() {
    let released = read_json(openrpc::RELEASED_PATH);
    let released: BTreeSet<&str> = released["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for (line, name) in [
        (
            "people bot add bot-a --of ann --name Scout",
            "people.addBot",
        ),
        ("people bot remove bot-a", "people.removeBot"),
        ("people bots-cap 50", "people.botsCap"),
        ("people bots-cap", "people.botsCap"),
        ("people invite ann --minutes 30", "people.invite"),
        ("people invites", "people.invites"),
        ("people invite-remove ABC234", "people.removeInvite"),
        ("people preset pro --mode acceptEdits", "people.preset"),
        ("people preset-remove pro", "people.removePreset"),
        (
            "people person add ann --name Ann --owner",
            "people.addPerson",
        ),
        ("people person rename ann anna", "people.renamePerson"),
        ("people person remove ann --unused", "people.removePerson"),
        ("people suspend ann", "people.suspend"),
        ("people undo ann", "people.undo"),
        ("people person clear-temp ann", "people.clearTemp"),
        ("people retired ann", "people.retired"),
        ("people budgets echo", "people.budgets"),
        ("people ledger --person ann", "people.ledger"),
        ("people ledger-remove read:echo:ann:/tmp/x", "people.removeGrant"),
        ("people handle ann telegram 12345", "people.handle"),
        ("people handle ann telegram --rm", "people.handle"),
        (
            "people agent-access echo api.example.com --rm",
            "people.agentAccess",
        ),
        ("people may ann echo", "people.may"),
        ("people resolve echo --person ann", "people.resolve"),
        ("people bridge-held ann", "people.bridgeHeld"),
    ] {
        let argv: Vec<&str> = line.split(' ').collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(parsed.method.name, name, "{line}");
        let m = method(name).unwrap();
        assert_eq!(m.served, Served::Core, "{name}");
        assert_eq!(m.stability, Stability::Experimental, "{name}");
        assert_eq!(m.principals, &[Principal::Owner], "{name}");
        assert!(!released.contains(name), "{name} is in released.json");
    }
}

#[test]
fn the_m2_stubs_parse_from_their_command_lines() {
    for (line, name) in [
        (
            "people grant ann echo --preset member --mode default --budget 20 --json",
            "people.grant",
        ),
        ("people revoke ann echo", "people.revoke"),
        ("people access --agent echo --json", "people.list"),
        ("people requests --json", "people.requests"),
        ("people approve r1", "people.approve"),
        ("people deny r1", "people.deny"),
    ] {
        let argv: Vec<&str> = line.split(' ').collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(parsed.method.name, name, "{line}");
    }
    let grant = parse_argv(&[
        "people", "grant", "ann", "echo", "--preset", "member", "--budget", "20",
    ])
    .unwrap();
    assert_eq!(
        grant.params,
        json!({"person": "ann", "agent": "echo", "preset": "member", "budget": "20"})
    );
}

#[test]
fn the_cards_rows_are_experimental_owner_only_unreleased_and_served_by_the_core() {
    let released = read_json(openrpc::RELEASED_PATH);
    let released: BTreeSet<&str> = released["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for (line, name) in [
        ("cards ls --json", "cards.list"),
        ("cards show c1", "cards.show"),
        ("cards answer c1 yes", "cards.answer"),
        ("cards provide c1 --account work", "cards.provide"),
    ] {
        let m = method(name).unwrap_or_else(|| panic!("{name}: no row"));
        assert_eq!(m.stability, Stability::Experimental, "{name}");
        assert_eq!(m.served, Served::Core, "{name}: the daemon serves it");
        assert_eq!(m.principals, &[Principal::Owner], "{name}");
        assert!(!released.contains(name), "{name} is in released.json");
        let argv: Vec<&str> = line.split(' ').collect();
        let parsed = parse_argv(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(parsed.method.name, name, "{line}");
    }
    // Every param is required in the row, as the call insists on.
    for name in ["cards.show", "cards.answer"] {
        assert!(method(name).unwrap().params.iter().all(|p| p.required));
    }
    // A key is never typed on a command line: the values come from the client (an app), and nothing else takes them.
    let provide = method("cards.provide").unwrap();
    let values = provide.params.iter().find(|p| p.name == "values").unwrap();
    assert!(matches!(values.arg, Arg::Caller));
    assert!(
        provide
            .params
            .iter()
            .filter(|p| matches!(p.arg, Arg::Flag(_)))
            .all(|p| p.name == "account")
    );
    assert!(parse_argv(&["cards", "provide", "c1", "--values", "x"]).is_err());
}

#[test]
fn the_m2_stubs_claim_no_command_line_python_serves_today() {
    // `kith people ls|show` and `kith cards history` are kith-people's and kith-cards's; the stubs' spellings differ, so
    // those lines stay NotInTable and the caller does what it did before the table.
    for line in [
        "people ls",
        "people ls --json",
        "people show ann",
        "cards history",
        "cards history --json",
        "people",
        "cards",
    ] {
        let argv: Vec<&str> = line.split(' ').collect();
        assert_eq!(
            parse_argv(&argv).unwrap_err(),
            ParseError::NotInTable,
            "{line}"
        );
    }
}

#[test]
fn the_gate_rows_are_experimental_owner_only_unreleased_and_parse() {
    let released = read_json(openrpc::RELEASED_PATH);
    let released: BTreeSet<&str> = released["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for name in [
        "gate.list",
        "gate.take",
        "gate.release",
        "gate.park",
        "gate.unpark",
    ] {
        let m = method(name).unwrap_or_else(|| panic!("{name}: no row"));
        assert_eq!(m.stability, Stability::Experimental, "{name}");
        assert_eq!(m.principals, &[Principal::Owner], "{name}");
        assert!(!released.contains(name), "{name} is in released.json");
    }
    let take = parse_argv(&[
        "gate",
        "take",
        "run1",
        "--max",
        "2",
        "--invitee",
        "--pid",
        "42",
    ])
    .unwrap();
    assert_eq!(take.method.name, "gate.take");
}

#[test]
fn the_sync_lease_rows_are_experimental_unreleased_and_parse_and_the_writes_are_the_hosts() {
    let released = read_json(openrpc::RELEASED_PATH);
    let released: BTreeSet<&str> = released["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for name in [
        "syncLease.status",
        "syncLease.link",
        "syncLease.unlink",
        "syncLease.mark",
        "syncLease.changed",
        "syncLease.drop",
        "syncLease.pulled",
        "syncLease.renew",
    ] {
        let m = method(name).unwrap_or_else(|| panic!("{name}: no row"));
        assert_eq!(m.stability, Stability::Experimental, "{name}");
        // The six writes report facts only the relay's process can know, so the owner (a terminal, an agent) must not
        // be able to forge them. `status` is the owner's read; `mark` reads the counter `status` already shows, and the
        // host takes it on its own connection.
        let want: &[Principal] = match name {
            "syncLease.status" => &[Principal::Owner],
            "syncLease.mark" => &[Principal::Owner, Principal::Host],
            _ => &[Principal::Host],
        };
        assert_eq!(m.principals, want, "{name}");
        assert!(!released.contains(name), "{name} is in released.json");
    }
    // The CLI always connects as the owner, so the host-only rows have no command line; `mark` and `status` keep theirs.
    for name in HOST_ROWS {
        let verb = name.trim_start_matches("syncLease.");
        assert!(method(name).unwrap().cli.hidden, "{name}");
        assert!(parse_argv(&["sync-lease", verb]).is_err(), "{name}");
    }
    assert!(parse_argv(&["sync-lease", "mark"]).is_ok());
    let pulled = parse_args(method("syncLease.pulled").unwrap(), ["3", "--ms", "5000"]).unwrap();
    assert_eq!(pulled.method.name, "syncLease.pulled");
    // The mark a pull started at is required.
    assert!(
        method("syncLease.pulled")
            .unwrap()
            .params
            .iter()
            .any(|p| p.name == "mark" && p.required)
    );
}

#[test]
fn the_row_order_is_the_order_of_the_families() {
    // The generated document follows this order; the split into area files must not move a row.
    let names: Vec<&str> = METHODS.iter().map(|m| m.name).collect();
    let at = |n: &str| {
        names
            .iter()
            .position(|x| *x == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    assert_eq!(at("paths"), 0);
    assert!(at("kernel.version") < at("agents.list"));
    assert!(at("delegations.list") < at("auth.list"));
    assert!(at("bug.show") < at("run.recall"));
    assert!(at("run.projectId") < at("backup.list"));
    assert!(at("schedule.log") < at("people.grant"));
    assert!(at("people.deny") < at("cards.list"));
}

#[test]
fn only_the_host_reads_a_secrets_value_and_the_owner_keeps_the_rest() {
    let get = method(SECRET_VALUE_ROW).unwrap();
    assert_eq!(get.principals, &[Principal::Host]);
    // The CLI always connects as the owner, so the row has no command line.
    assert!(get.cli.hidden);
    assert!(parse_argv(&["secrets", "get", "a"]).is_err());
    for name in [
        "secrets.open",
        "secrets.has",
        "secrets.put",
        "secrets.delete",
    ] {
        assert_eq!(
            method(name).unwrap().principals,
            &[Principal::Owner],
            "{name}"
        );
    }
    // No other secrets row hands a value back: `get` is the only one the table may give to Host.
    let host_secrets: Vec<_> = METHODS
        .iter()
        .filter(|m| m.name.starts_with("secrets.") && m.principals.contains(&Principal::Host))
        .map(|m| m.name)
        .collect();
    assert_eq!(host_secrets, [SECRET_VALUE_ROW]);
}

#[test]
fn the_launch_keys_row_is_the_owners_alone() {
    let row = method("secrets.serviceKeys").unwrap();
    assert_eq!(row.principals, &[Principal::Owner]);
    for who in [
        Principal::Host,
        Principal::Member,
        Principal::Bot,
        Principal::RunToken,
    ] {
        assert!(!row.principals.contains(&who), "{who:?}");
    }
    assert!(row.cli.hidden);
}

#[test]
fn the_launch_keys_person_is_required_in_the_contract() {
    let doc = openrpc::generate();
    let m = doc["methods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "secrets.serviceKeys")
        .unwrap();
    let req = |n: &str| {
        m["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == n)
            .unwrap()["required"]
            .clone()
    };
    assert!(req("person").as_bool().unwrap());
    assert!(!req("vars").as_bool().unwrap());
}
