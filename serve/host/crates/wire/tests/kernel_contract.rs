//! What the kernel prints for the host (serve/contract/kernel/contract.json) as the host reads it: `src/kernel.rs` is
//! made from the contract as it is now, the kernel's real output (fixtures/, written by kernel/tests/kith-contract-check)
//! and an older kernel's (compat/) decode, fields this version doesn't know are ignored, and a word it doesn't know
//! decodes as its enum declares (W3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_wire::check::Mode;
use kith_wire::kernel::{self, Agent, DelegationState, Delegations, Harness, Launch, PermissionMode, Projects, SCHEMA};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::PathBuf;

const CONTRACT: &[u8] = include_bytes!("../../../../contract/kernel/contract.json");

/// Each fixture in `dir`: its path, command, the spec of what the command prints, and the body.
fn fixtures(dir: &str) -> Vec<(String, String, String, Value)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/kernel").join(dir);
    let mut out: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let f: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            let s = |k: &str| f[k].as_str().unwrap().to_owned();
            (p.display().to_string(), s("command"), s("type"), f["body"].clone())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn fixture<T: serde::de::DeserializeOwned>(dir: &str, name: &str) -> T {
    let (_, _, _, body) = fixtures(dir).into_iter().find(|(p, ..)| p.ends_with(name)).unwrap();
    serde_json::from_value(body).unwrap()
}

#[test]
fn kernel_rs_is_made_from_the_contract_as_it_is_now() {
    let hex = kith_wire::crypto::sha256(&[CONTRACT]).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    assert_eq!(hex, kernel::CONTRACT_SHA256, "the contract changed: run `node serve/contract/gen.mjs`");
}

#[test]
fn every_output_decodes_today_from_an_older_kernel_and_from_a_newer_one() {
    for dir in ["fixtures", "compat", "unknown"] {
        let all = fixtures(dir);
        assert_ne!(all.len(), 0, "{dir}");
        for (p, command, spec, body) in all {
            let (_, result) = kernel::COMMANDS
                .iter()
                .find(|(c, _)| *c == command)
                .unwrap_or_else(|| panic!("{p}: no command {command}"));
            assert_eq!(*result, spec, "{p}");
            // A list's items decode one by one, as its type.
            let items = match spec.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                Some(ty) => body.as_array().unwrap().iter().map(|v| (ty, v.clone())).collect(),
                None => vec![(spec.as_str(), body.clone())],
            };
            for (ty, v) in items {
                let decoded = kernel::decode(ty, v).unwrap_or_else(|| panic!("{p}: no contract type {ty}"));
                assert!(decoded.is_ok(), "{p}: {decoded:?}");
            }
            assert_eq!(SCHEMA.check_spec(&spec, &body, Mode::Consumer), Vec::<String>::new(), "{p}");
        }
    }
}

#[test]
fn the_kernels_own_output_is_what_a_producer_may_print() {
    for (p, _, spec, body) in fixtures("fixtures") {
        assert_eq!(SCHEMA.check_spec(&spec, &body, Mode::Producer), Vec::<String>::new(), "{p}");
    }
    // A newer kernel's words are not this version's to print.
    for (p, _, spec, body) in fixtures("unknown") {
        assert_ne!(SCHEMA.check_spec(&spec, &body, Mode::Producer), Vec::<String>::new(), "{p}");
    }
}

#[test]
fn the_kernels_own_output_reads_as_it_was_printed() {
    let agents: Vec<Agent> = fixture("fixtures", "ls.json");
    assert_eq!(agents.iter().map(|a| a.harness).collect::<Vec<_>>(), [Some(Harness::Claude), Some(Harness::Codex)]);
    let projects: Projects = fixture("fixtures", "project-ls.json");
    assert_eq!(projects.projects[1].parent.as_deref(), Some("alpha"));
    let owner: Launch = fixture("fixtures", "launch.json");
    assert_eq!(
        (owner.permission_mode, owner.harness, owner.cost_tag.person),
        (Some(PermissionMode::Auto), Some(Harness::Claude), None)
    );
    let delegations: Delegations = fixture("fixtures", "delegations.json");
    let capped = &delegations.delegations[0];
    assert_eq!(
        (capped.state, capped.chain_cost_micro_usd, capped.cap_micro_usd),
        (DelegationState::Capped, Some(5_250_000), Some(5_000_000))
    );
    assert_eq!(delegations.delegations[1].cost_micro_usd, None, "an ask from before the record has no cost");
    let person: Launch = fixture("fixtures", "launch-as-person.json");
    assert_eq!(
        (person.permission_mode, person.cost_tag.person.as_deref()),
        (Some(PermissionMode::AcceptEdits), Some("sam"))
    );
}

#[test]
fn a_newer_kernels_words_read_as_their_enum_declares() {
    let agents: Vec<Agent> = fixture("unknown", "ls-new-harness.json");
    assert_eq!(agents[0].harness, Some(Harness::Unknown), "a harness this version doesn't know is never started");
    let launch: Launch = fixture("unknown", "launch-new-words.json");
    assert_eq!(
        launch.permission_mode,
        Some(PermissionMode::Default),
        "a mode this version doesn't know asks before every change"
    );
    assert_eq!((launch.harness, launch.provider), (Some(Harness::Unknown), Some(Harness::Unknown)));
    let delegations: Delegations = fixture("unknown", "delegations-new-state.json");
    assert_eq!(
        delegations.delegations[0].state,
        DelegationState::Unknown,
        "a state this version doesn't know is left alone"
    );
}

#[test]
fn an_older_kernels_output_leaves_out_what_it_didnt_have() {
    let agents: Vec<Agent> = fixture("compat", "ls-before-harness.json");
    assert_eq!((agents[0].harness, agents[0].model.as_deref()), (None, None));
    let launch: Launch = fixture("compat", "launch-before-backups.json");
    assert_eq!((launch.harness, launch.provider, launch.backup, launch.call_model), (None, None, None, None));
    let projects: Projects = fixture("compat", "project-ls-before-parents.json");
    assert_eq!((projects.projects[0].home.as_deref(), projects.projects[0].parent.as_deref()), (None, None));
    let delegations: Delegations = fixture("compat", "delegations-optional-left-out.json");
    let d = &delegations.delegations[0];
    assert_eq!(
        (d.cost_micro_usd, d.cap_micro_usd, d.reply_to.session.as_deref(), d.caller.agent.as_deref()),
        (None, None, None, None)
    );
}

#[test]
fn the_commands_are_the_kernels_arguments() {
    assert_eq!(kernel::command::ls(), ["ls", "--json"]);
    assert_eq!(kernel::command::project_ls(), ["project", "ls", "--json"]);
    assert_eq!(kernel::command::delegations(), ["ask", "delegations", "--json"]);
    assert_eq!(kernel::command::launch("echo", None, None), ["echo", "--launch-json"]);
    assert_eq!(
        kernel::command::launch("echo", Some("codex-1"), Some("alpha")),
        ["echo", "--launch-json", "--backup", "codex-1", "-p", "alpha"]
    );
}
