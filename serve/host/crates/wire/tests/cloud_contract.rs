//! The cloud contract (serve/contract/cloud/contract.json) as the host and the guest read it: `src/cloud.rs` is made
//! from the contract as it is now, today's and older clouds' answers decode (W4), fields this version doesn't know are
//! ignored, and a word it doesn't know decodes as its enum declares (W3): the least value, or the explicit `Unknown`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kith_wire::cloud::{
    self, AccessState, AccessStatus, ComputerAccess, ComputerList, ComputerRole, Decided, ErrorCode, HostState, path,
};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::PathBuf;

const CONTRACT: &[u8] = include_bytes!("../../../../contract/cloud/contract.json");

fn fixtures(dir: &str) -> Vec<(String, String, Value)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../contract/cloud/fixtures").join(dir);
    let mut out: Vec<(String, String, Value)> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let f: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            (p.display().to_string(), f["type"].as_str().unwrap().to_owned(), f["body"].clone())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn fixture<T: serde::de::DeserializeOwned>(dir: &str, name: &str) -> T {
    let (_, _, body) = fixtures(dir).into_iter().find(|(p, _, _)| p.ends_with(name)).unwrap();
    serde_json::from_value(body).unwrap()
}

#[test]
fn cloud_rs_is_made_from_the_contract_as_it_is_now() {
    let hex = kith_wire::crypto::sha256(&[CONTRACT]).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    assert_eq!(hex, cloud::CONTRACT_SHA256, "the contract changed: run `node serve/contract/gen.mjs`");
}

#[test]
fn every_answer_decodes_today_from_an_older_cloud_and_from_a_newer_one() {
    for dir in ["", "compat", "unknown"] {
        let all = fixtures(dir);
        assert!(!all.is_empty(), "{dir}");
        for (p, ty, body) in all {
            let decoded = cloud::decode(&ty, body).unwrap_or_else(|| panic!("{p}: no contract type {ty}"));
            assert!(decoded.is_ok(), "{p}: {decoded:?}");
        }
    }
}

#[test]
fn a_newer_clouds_words_read_as_their_enum_declares() {
    let list: ComputerList = fixture("unknown", "computer-list-new-words.json");
    let c = &list.computers[0];
    // ComputerRole and ComputerAccess read a word they don't know as their least value.
    assert_eq!(c.role, Some(ComputerRole::Member));
    // #328: a computer's access is "granted"; "approved" is a word this version doesn't know, so no access.
    assert_eq!(c.access, Some(ComputerAccess::NoAccess));
    // AccessState's unknown is a state of its own.
    let status: AccessStatus = fixture("unknown", "access-status-new-state.json");
    assert_eq!(status.status, AccessState::Unknown);
    let decided: Decided = fixture("unknown", "decided-new-state.json");
    assert_eq!(decided.status, AccessState::Unknown);
    assert_eq!(ErrorCode::from_code("somethingNew"), ErrorCode::Unknown);
    assert_eq!(ErrorCode::from_code("requestExpired").status(), Some(410));
}

#[test]
fn fields_this_version_doesnt_know_are_ignored() {
    let newer: HostState = fixture("compat", "host-state-with-newer-fields.json");
    let today: HostState = fixture("", "host-state.json");
    assert_eq!(newer, today);
}

#[test]
fn an_enums_default_is_what_it_reads_a_word_it_doesnt_know_as() {
    assert_eq!(ComputerRole::default(), ComputerRole::Member);
    assert_eq!(ComputerAccess::default(), ComputerAccess::NoAccess);
    assert_eq!(AccessState::default(), AccessState::Unknown);
    assert_eq!(serde_json::from_value::<ComputerRole>("admin".into()).unwrap(), ComputerRole::Member);
}

#[test]
fn an_older_clouds_host_state_leaves_what_the_host_knows() {
    let state: HostState = fixture("compat", "host-state-before-invites.json");
    assert_eq!((state.memberships, state.bot_devices, state.revoked), (None, None, None));
    assert_eq!(state.grants[0].user_id, None, "no account named: the owner's");
    assert_eq!(state.owner.unwrap().email, None);
}

#[test]
fn paths_are_signed_with_their_slash_and_query() {
    assert_eq!(path::HOST_STATE, "/v1/host/state");
    assert_eq!(path::RELAY_HOST, "/v1/relay/host?v=1");
    assert_eq!(path::relay_device("cmp_1"), "/v1/relay/device/cmp_1?v=1");
    assert_eq!(path::host_decision("req_1"), "/v1/host/access-requests/req_1/decision");
    assert_eq!(path::accept_bot_invite("bi_1"), "/v1/bot-invites/bi_1/accept");
}
