#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use kith_holder::client::Client;
use kith_holder::{launch, proto, wire};

use super::{Answered, TurnLock, TurnRelease, release_turn_lock, take_turn_lock};
use crate::terminal::tests::daemon::{holder_bin, sandbox};

const SECRET: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);

/// The built `kith-stub`, next to the holder binary.
fn stub_bin() -> PathBuf {
    let beside = holder_bin().with_file_name("kith-stub");
    if !beside.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = std::process::Command::new(cargo)
            .args(["build", "--manifest-path"])
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml"))
            .status()
            .unwrap();
        assert!(
            built.success() && beside.is_file(),
            "kith-stub is not built"
        );
    }
    beside
}

fn warm_spawn(
    conv: &str,
    claim: u64,
    stub: Option<&PathBuf>,
    locks: &std::path::Path,
) -> proto::Spawn {
    proto::Spawn {
        conv: conv.into(),
        claim,
        generation: 1,
        argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 300".into()],
        env: std::env::vars()
            .filter(|(k, _)| k == "PATH")
            .collect::<BTreeMap<_, _>>(),
        cwd: "/".into(),
        mode: proto::Mode::Pipes,
        cols: 80,
        rows: 24,
        kill_on_lease_loss: false,
        stub: stub.map(|s| s.display().to_string()),
        lock_dir: Some(locks.display().to_string()),
    }
}

fn start(c: &mut Client, spec: &proto::Spawn) -> Result<proto::HeldId, String> {
    c.spawn(spec).unwrap();
    let (kind, _, payload) = c
        .take(WAIT, |f| {
            matches!(f.0, proto::SPAWNED | proto::SPAWN_FAILED)
        })
        .expect("an answer to the spawn");
    if kind == proto::SPAWN_FAILED {
        return Err(wire::parse::<proto::SpawnFailed>(&payload).unwrap().error);
    }
    Ok(wire::parse::<proto::Spawned>(&payload).unwrap().held_id())
}

fn ok<T>(claim: u64, result: T) -> Answered<T> {
    Answered { claim, result }
}

fn ended(c: &mut Client, held: &proto::HeldId) {
    for _ in 0..40 {
        if c.kill(held).unwrap() == proto::KillState::Gone {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = c.ack(held);
}

/// The core's calls against a real holder and a real stub: typed answers that carry the claim, and a missing stub is a
/// failed spawn with the hint, never a bare engine.
#[test]
fn the_cores_take_and_release_return_typed_answers_with_the_claim() {
    let dir = sandbox();
    let holder_dir = dir.path().join("h");
    launch::ensure(&holder_bin(), &holder_dir, SECRET, Duration::from_secs(120)).unwrap();
    let mut c = Client::connect(&holder_dir, SECRET, 1).unwrap();
    let locks = dir.path().join("locks");
    let stub = stub_bin();

    let missing = PathBuf::from("/no/such/kith-stub");
    let why = start(&mut c, &warm_spawn("conv-x", 1, Some(&missing), &locks)).unwrap_err();
    assert!(
        why.contains("kith-stub") && why.contains("Install"),
        "{why}"
    );
    let why = start(&mut c, &warm_spawn("conv-x", 1, None, &locks)).unwrap_err();
    assert!(why.contains("Install"), "{why}");

    let held = start(&mut c, &warm_spawn("conv-x", 1, Some(&stub), &locks)).unwrap();
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnLock::Locked)
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnLock::Locked),
        "a retried take"
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnLock::Busy)
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnRelease::NotHeld),
        "another claim's release frees nothing"
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnRelease::Released)
    );
    assert_eq!(
        release_turn_lock(&mut c, &held, "conv-x", 1).unwrap(),
        ok(1, TurnRelease::NotHeld)
    );
    let again = take_turn_lock(&mut c, &held, "conv-x", 1).unwrap();
    assert_eq!(again.claim, 1);
    assert!(
        matches!(again.result, TurnLock::Error(ref why) if why == "claim-released"),
        "{again:?}"
    );
    assert_eq!(
        take_turn_lock(&mut c, &held, "conv-x", 2).unwrap(),
        ok(2, TurnLock::Locked)
    );
    ended(&mut c, &held);
    let _ = c.send(proto::SHUTDOWN, 0, &[]);
}
