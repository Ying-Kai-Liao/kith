//! A package folder that does not answer, seen from the authority rules: the narrower answer, in its own words, and
//! nothing written for it. (The rest of the authority suite runs through the package cache too: see `Rig::call`.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::FUTURE;
use super::writes::{Rig, base};
use crate::packages::PackageCache;

/// Makes the rig's refresher one whose read never ends (until the flag is cleared).
fn stuck(rig: &mut Rig) -> Arc<AtomicBool> {
    let hold = Arc::new(AtomicBool::new(true));
    let h = hold.clone();
    rig.cache = PackageCache::start_with(
        Some(Arc::new(move || {
            while h.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        })),
        Duration::from_millis(200),
    );
    hold
}

fn world() -> Value {
    let mut w = base();
    w["members"] =
        json!({"house": {"sam": {"preset": "member", "helper_once": {"garden": FUTURE}}}});
    w
}

#[test]
fn an_owner_write_naming_an_agent_that_cannot_be_read_is_refused_and_writes_nothing() {
    let mut rig = Rig::new(&base());
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let said = rig
        .call(
            "people.grant",
            &json!({"person": "sam", "agent": "house", "preset": "member"}),
        )
        .unwrap_err();
    assert!(said.contains("can't read house's package"), "{said}");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert!(rig.tables()["members"].get("house").is_none());
    // Once the disk answers the same call goes through.
    hold.store(false, Ordering::SeqCst);
    rig.cache = PackageCache::start(None);
    rig.call(
        "people.grant",
        &json!({"person": "sam", "agent": "house", "preset": "member"}),
    )
    .unwrap();
    assert!(rig.tables()["members"]["house"].get("sam").is_some());
}

#[test]
fn a_guests_agent_that_cannot_be_read_is_owner_only_and_the_owner_is_unchanged() {
    let mut rig = Rig::new(&world());
    let hold = stuck(&mut rig);
    let began = Instant::now();
    let guest = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "sam"}),
        )
        .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(guest["allowed"], false, "{guest}");
    assert!(
        guest.to_string().contains("can't read house's package"),
        "{guest}"
    );
    let owner = rig
        .call(
            "people.resolve",
            &json!({"agent": "house", "person": "kai"}),
        )
        .unwrap();
    assert_eq!(owner["allowed"], true, "{owner}");
    hold.store(false, Ordering::SeqCst);
}
