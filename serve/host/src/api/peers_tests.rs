//! A paired computer (Mac link) against a temp store and a fake cloud: who its key is here, and that it reaches the
//! peer methods and nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::{Caller, Forbidden, LOCAL_ONLY, NotAllowed, Principal, principal};
use super::rig::Rig;
use super::{dispatch, events_stream, roles};
use crate::store::{BotConfig, PeerIn, Scope, now_ms};
use kith_wire::rpc::PEER_METHODS;
use serde_json::json;

/// `cmp_a` paired here, calling with `key`.
fn paired(r: &Rig, key: &str) -> Caller {
    let peer = PeerIn {
        computer_id: "cmp_a".into(),
        name: "Studio".into(),
        device_key: key.into(),
        epoch: "epoch".into(),
        grant_id: "pgr_a".into(),
        paired_at: now_ms(),
    };
    r.hub.store.put_peer_in(&peer).unwrap();
    Caller::Device { key: key.into(), scopes: vec![Scope::Control] }
}

#[tokio::test]
async fn a_paired_computer_is_a_peer_never_the_owner() {
    let r = Rig::new().await;
    let stranger = Caller::Device { key: "mac-b".into(), scopes: vec![Scope::Control] };
    assert_eq!(principal(&r.hub.store, &stranger), Principal::Nobody);
    let peer = paired(&r, "mac-b");
    assert_eq!(principal(&r.hub.store, &peer), Principal::Peer("cmp_a".into()));
    // A device row with no person is the owner's; a paired key is decided before that default.
    let device = r.device("mac-b", None);
    assert_eq!(principal(&r.hub.store, &device), Principal::Peer("cmp_a".into()));
    r.hub.store.remove_peer_in("cmp_a").unwrap();
    assert_eq!(principal(&r.hub.store, &device), Principal::Owner, "the device row's own answer once unpaired");
    r.hub.store.remove_device("mac-b").unwrap();
    assert_eq!(principal(&r.hub.store, &peer), Principal::Nobody, "unpaired: nobody");
}

#[tokio::test]
async fn every_router_method_but_the_peer_ones_is_denied_to_a_peer() {
    let r = Rig::new().await;
    let peer = paired(&r, "mac-b");
    let known = super::roles_tests::router_methods();
    for m in PEER_METHODS {
        assert!(known.contains(*m), "{m} is a peer method but nothing answers it");
        assert!(!roles::member_may(m) && !roles::bot_may(m), "{m} is for paired computers only");
    }
    let mut refused = 0;
    for m in known.iter().filter(|m| !PEER_METHODS.contains(&m.as_str())) {
        let e = dispatch(&r.hub, &peer, m, json!({})).await.expect_err(m);
        assert!(e.is::<NotAllowed>() || e.is::<Forbidden>(), "{m}: {e:#}");
        refused += 1;
    }
    assert!(refused > 100, "the walk refused {refused}");
    for m in LOCAL_ONLY {
        assert!(dispatch(&r.hub, &peer, m, json!({})).await.is_err(), "{m} is this computer's only");
    }
    assert!(events_stream(&r.hub, 0, None, &peer).is_err(), "no event stream");
    let rows = r.hub.store.audit_list(0, Some("peer:cmp_a")).unwrap();
    assert!(!rows.is_empty() && rows.iter().all(|row| !row.allowed && row.device_key == "mac-b"));
}

#[tokio::test]
async fn a_peer_sees_the_owners_agents_only() {
    let r = Rig::new().await;
    r.owner_bot("money");
    r.owner_bot("house");
    r.owner_bot("house");
    let cfg: BotConfig = serde_json::from_value(json!({
        "id": "", "name": "Sam's house", "backend": "claude", "cwd": r.dir.to_string_lossy(),
        "hqAgent": "house", "person": "sam", "permission": "default", "tasks": false, "connectors": [], "skills": [],
    }))
    .unwrap();
    r.hub.create_bot(cfg).unwrap();
    let peer = paired(&r, "mac-b");
    let got = dispatch(&r.hub, &peer, "peerAgents", json!({})).await.unwrap();
    assert_eq!(got, json!({"agents": [{"agent": "house", "name": "house"}, {"agent": "money", "name": "money"}]}));
    let rows = r.hub.store.audit_list(0, Some("peer:cmp_a")).unwrap();
    assert_eq!(rows.iter().map(|row| (row.method.as_str(), row.allowed)).collect::<Vec<_>>(), [("peerAgents", true)]);
}
