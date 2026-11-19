//! Property tests for `runauth`, the host's half of the run tokens (M2(c)). A guest's token is minted and resolved by the
//! core (`core/runtime/src/runs.rs`; `runs_props.rs` there holds the generated P1 to P7 of what a guest may read, the
//! argument allow-lists included, and `runsock_tools_tests.rs` the real-socket runs). What stays on host.sock is the
//! cards a run may ask: the real `authorize`, `TokenTable::resolve` and `RunGrant` run here on generated token tables,
//! request lines and peers, with a guest's identity standing for what the core's `run.resolve` answers; the
//! peer-descends-from-run check is the table's own injectable `ancestry` (a pid window instead of `ps`). Nothing here
//! models the code: every property calls it. The line used is `consumeAccess`, the one guest method that needs no
//! fields; recall, memory search and project-id are refused here (P5).
//! Method: `.claude/skills/run-tokens/SKILL.md`. Run only these: `cargo test --bin kith runauth_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use prop::sample::select;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

const MOVED: [&str; 3] = ["recall", "memorySearch", "projectId"];
const PEOPLE: [&str; 4] = ["sam", "eve", "bot-ab12", "kai"];
const AGENTS: [&str; 2] = ["echo", "fox"];
const PROJECTS: [Option<&str>; 3] = [None, Some("alpha"), Some("beta")];
const ROOMS: [Option<&str>; 4] = [None, Some("1:1:echo:sam"), Some("1:1:echo:eve"), Some("g1")];
/// Run `i` is started as pid `BASE + i * SPACING`; a peer descends from it when it is within `WINDOW` above it.
const BASE: u32 = 1000;
const SPACING: u32 = 100;
const WINDOW: u32 = 10;

fn descends_window(peer: u32, run: u32) -> bool {
    peer >= run && peer - run < WINDOW
}

/// Cases per property; `PROPTEST_CASES` overrides it.
fn runner() -> TestRunner {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    TestRunner::new(config)
}

fn check<S: Strategy>(s: S, f: impl Fn(S::Value) -> Result<(), TestCaseError>) -> Result<(), String> {
    let mut r = runner();
    r.run(&s, f).map_err(|e| e.to_string())
}

fn refused() -> Response {
    Response::refused("not authorized")
}

// ---- the world: a token table with several runs

struct Live {
    identity: RunIdentity,
    token: String,
    pid: u32,
    grant: Option<RunGrant>,
}

/// One run per spec `(person, agent, room)`, all issued on one table; bound to their pids unless `bind` is false.
fn build(specs: &[(usize, usize, usize)], bind: bool) -> (Arc<TokenTable>, Vec<Live>) {
    let table = Arc::new(TokenTable::with_ancestry(descends_window));
    let runs = specs
        .iter()
        .enumerate()
        .map(|(i, &(p, a, r))| {
            let identity = RunIdentity {
                person: PEOPLE[p].into(),
                agent: AGENTS[a].into(),
                room: ROOMS[r].map(Into::into),
                run_id: format!("r{i}"),
                scope: TokenScope::Guest,
                session: format!("run:s{i}"),
                project: PROJECTS[(p + i) % PROJECTS.len()].map(Into::into),
                reply: None,
                members: Vec::new(),
            };
            let grant = RunGrant::issue(&table, identity.clone()).unwrap();
            let pid = BASE + u32::try_from(i).unwrap() * SPACING;
            if bind {
                grant.bind_pid(pid);
            }
            let token = grant.env(Path::new("/s"))[0].1.clone();
            Live { identity, token, pid, grant: Some(grant) }
        })
        .collect();
    (table, runs)
}

fn specs() -> impl Strategy<Value = Vec<(usize, usize, usize)>> {
    // Small pools so the same person, agent or room repeats across runs.
    prop::collection::vec((0..PEOPLE.len(), 0..AGENTS.len(), 0..ROOMS.len()), 1..6)
}

fn peer_in(run: &Live, off: u32) -> PeerInfo {
    PeerInfo { pid: Some(run.pid + off % WINDOW) }
}

/// A pid that is not in `run_pid`'s window (the shift never lands back inside it).
fn outside(raw: u32, run_pid: u32) -> u32 {
    if descends_window(raw, run_pid) { raw + WINDOW } else { raw }
}

fn raw_pid() -> impl Strategy<Value = u32> {
    prop_oneof![0u32..2000, Just(u32::MAX), Just(0u32), Just(1u32)]
}

// ---- request lines

const EXTRA_KEYS: [&str; 10] =
    ["person", "agent", "room", "project", "audience", "identity", "run_id", "KITH_PERSON", "env", "who"];

fn forged_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        select(&["sam", "eve", "kai", "owner", "*", "1:1:echo:eve", "shared", "person:kai"][..]).prop_map(Value::from),
        Just(json!({"person": "kai", "audience": ["shared", "person:kai"]})),
        Just(json!(["kai", "eve"])),
    ]
}

fn extras() -> impl Strategy<Value = Vec<(&'static str, Value)>> {
    prop::collection::vec((select(&EXTRA_KEYS[..]), forged_value()), 0..6)
}

fn request(token: &Value, method: &Value, args: Vec<Value>, extras: &[(&str, Value)]) -> Vec<u8> {
    let mut m = serde_json::Map::new();
    for (k, v) in extras {
        m.insert((*k).into(), v.clone());
    }
    m.insert("token".into(), token.clone());
    m.insert("method".into(), method.clone());
    m.insert("args".into(), Value::Array(args));
    Value::Object(m).to_string().into_bytes()
}

/// A guest's call on host.sock that needs no field of its own.
fn card_line(token: &str, extras: &[(&str, Value)]) -> Vec<u8> {
    request(&json!(token), &json!("consumeAccess"), Vec::new(), extras)
}

// ---- properties (each returns Err(counterexample) so the break meta-test can read it)

/// P1: whatever else a request claims, the identity is exactly what the token resolves to.
pub fn prop_identity_is_the_tokens() -> Result<(), String> {
    check((specs(), extras(), 0u32..WINDOW), |(specs, extras, off)| {
        let (t, runs) = build(&specs, true);
        for r in &runs {
            let peer = peer_in(r, off);
            let got = authorize(&t, &card_line(&r.token, &extras), peer)
                .map_err(|e| TestCaseError::fail(format!("refused: {e:?}")))?;
            prop_assert_eq!(&got.identity, &r.identity);
            prop_assert_eq!(Some(got.identity), t.resolve(&r.token, peer));
        }
        Ok(())
    })
}

/// P2: a token from a peer that is not the run's own process (or below it), or before it is bound, resolves to nothing.
pub fn prop_foreign_peer_is_denied() -> Result<(), String> {
    check(
        (specs(), extras(), prop::collection::vec(prop::option::of(raw_pid()), 6), any::<bool>()),
        |(specs, extras, raws, bound)| {
            let (t, runs) = build(&specs, bound);
            for (i, r) in runs.iter().enumerate() {
                let peer = PeerInfo { pid: raws[i].map(|p| if bound { outside(p, r.pid) } else { p }) };
                let line = card_line(&r.token, &extras);
                prop_assert_eq!(t.resolve(&r.token, peer), None);
                prop_assert_eq!(authorize(&t, &line, peer), Err(refused()));
                if bound {
                    // Another run's own process is just as foreign.
                    for o in runs.iter().filter(|o| o.pid != r.pid) {
                        let line = card_line(&r.token, &extras);
                        prop_assert_eq!(authorize(&t, &line, peer_in(o, 0)), Err(refused()));
                    }
                }
            }
            Ok(())
        },
    )
}

const MUTATIONS: u8 = 9;

fn mutate(token: &str, k: u8, junk: &str) -> String {
    match k {
        0 => String::new(),
        1 => token.to_uppercase(),
        2 => token[..63].into(),
        3 => format!("{token} "),
        4 => format!("x{token}"),
        5 => junk.into(),
        6 => format!("{}z", &token[..63]),
        7 => token.repeat(2),
        _ => "0".repeat(64),
    }
}

/// P3: a revoked token (run exited), and an unknown, empty or garbage one, are denied the same way; revoking one run
/// leaves the others working.
pub fn prop_revoked_and_unknown_are_denied() -> Result<(), String> {
    check(
        (specs(), extras(), prop::collection::vec(any::<bool>(), 1..6), 0u32..WINDOW, 0..MUTATIONS, ".{0,40}"),
        |(specs, extras, mask, off, k, junk)| {
            let (t, mut runs) = build(&specs, true);
            for (i, r) in runs.iter_mut().enumerate() {
                if mask[i % mask.len()] {
                    drop(r.grant.take());
                }
            }
            prop_assert_eq!(t.len(), runs.iter().filter(|r| r.grant.is_some()).count());
            for r in &runs {
                let peer = peer_in(r, off);
                let got = authorize(&t, &card_line(&r.token, &extras), peer);
                if r.grant.is_some() {
                    prop_assert_eq!(got.map(|a| a.identity), Ok(r.identity.clone()));
                } else {
                    prop_assert_eq!(got, Err(refused()));
                    prop_assert_eq!(t.resolve(&r.token, peer), None);
                }
                let bad = mutate(&r.token, k, &junk);
                prop_assume!(!runs.iter().any(|o| o.grant.is_some() && o.token == bad));
                prop_assert_eq!(t.resolve(&bad, peer), None);
                prop_assert_eq!(authorize(&t, &card_line(&bad, &extras), peer), Err(refused()));
            }
            Ok(())
        },
    )
}

/// P7: a dead run's pid reused by a new run does not revive the dead token; two runs of one person in different rooms
/// cannot use each other's tokens.
pub fn prop_pid_reuse_and_sibling_runs() -> Result<(), String> {
    check((0..PEOPLE.len(), 0..AGENTS.len(), (0..ROOMS.len(), 0..ROOMS.len())), |(p, a, (r1, r2))| {
        prop_assume!(r1 != r2);
        let (t, mut runs) = build(&[(p, a, r1), (p, a, r2)], true);
        let (x, y) = (&runs[0], &runs[1]);
        prop_assert_eq!(authorize(&t, &card_line(&x.token, &[]), peer_in(y, 0)), Err(refused()));
        prop_assert_eq!(authorize(&t, &card_line(&y.token, &[]), peer_in(x, 0)), Err(refused()));
        // Run 0 exits; its pid is handed to a new run of the same person in the other room.
        let old = runs.remove(0);
        let (old_token, old_pid) = (old.token.clone(), old.pid);
        drop(old);
        let fresh = RunGrant::issue(&t, RunIdentity { run_id: "fresh".into(), ..runs[0].identity.clone() }).unwrap();
        fresh.bind_pid(old_pid);
        let fresh_token = fresh.env(Path::new("/s"))[0].1.clone();
        let reused = PeerInfo { pid: Some(old_pid) };
        prop_assert_eq!(authorize(&t, &card_line(&old_token, &[]), reused), Err(refused()));
        let ok = authorize(&t, &card_line(&fresh_token, &[]), reused).unwrap();
        prop_assert_eq!(ok.identity.run_id, "fresh");
        Ok(())
    })
}

/// P5: recall, memory search and project-id are not host.sock methods for anyone (the core's run socket answers them), and
/// neither is any other word; a guest's token is authorized for the card methods only, an owner token for none of the
/// guest ones (`consumeAccess`, `requestAccess`).
pub fn prop_only_cards_are_authorized() -> Result<(), String> {
    check(
        (
            specs(),
            prop_oneof![
                select(
                    &[
                        "recall",
                        "memorySearch",
                        "projectId",
                        "forget",
                        "index",
                        "summarise",
                        "shell",
                        "Recall",
                        " recall",
                        "recall\0",
                        "",
                        "RECALL",
                        "exec",
                        "send",
                        "memorysearch",
                        "project-id",
                    ][..]
                )
                .prop_map(|m| json!(m)),
                ".{0,12}"
                    .prop_filter("not a card method", |m| {
                        ![
                            "requestPermission",
                            "requestAccess",
                            "consumeAccess",
                            "requestSecret",
                            "consumeOnce",
                            "cardsHistory",
                        ]
                        .contains(&m.as_str())
                    })
                    .prop_map(|m| json!(m)),
                Just(json!(1)),
                Just(json!(null)),
                Just(json!(["recall"])),
            ],
        ),
        |(specs, method)| {
            let (t, runs) = build(&specs, true);
            for r in &runs {
                let line = request(&json!(r.token), &method, vec![json!("zebra")], &[]);
                prop_assert!(authorize(&t, &line, peer_in(r, 0)).is_err(), "method {method} was authorized");
                // The card method is authorized too (so refusing everything cannot pass).
                prop_assert!(
                    authorize(&t, &card_line(&r.token, &[]), peer_in(r, 0)).is_ok(),
                    "consumeAccess is refused"
                );
            }
            // An owner token (cards only) is refused the three moved methods and the guest's access calls.
            let owner = Arc::new(TokenTable::with_ancestry(descends_window));
            let token = owner.insert_ask(ask_identity("echo", "s", None), BASE, Duration::from_secs(60)).unwrap();
            for m in MOVED.iter().chain(&["consumeAccess"]) {
                let line = request(&json!(token), &json!(m), vec![json!("zebra")], &[]);
                prop_assert!(authorize(&owner, &line, PeerInfo { pid: Some(BASE) }).is_err(), "owner token got {m}");
            }
            Ok(())
        },
    )
}

/// P6: malformed, oversized and mistyped lines are denied, never a panic (a panic fails the case).
pub fn prop_malformed_lines_are_denied() -> Result<(), String> {
    check(
        (
            specs(),
            prop::collection::vec(any::<u8>(), 0..300),
            0..8usize,
            prop_oneof![Just(json!(null)), Just(json!(7)), Just(json!(true)), Just(json!([])), Just(json!({}))],
            1..200usize,
        ),
        |(specs, junk, shape, odd, pad)| {
            let (t, runs) = build(&specs, true);
            let live = &runs[0];
            let peer = peer_in(live, 0);
            let good = card_line(&live.token, &[]);
            prop_assert!(authorize(&t, &good, peer).is_ok());
            prop_assert!(authorize(&t, &junk, peer).is_err(), "random bytes were accepted");
            let bad: Vec<u8> = match shape {
                // Valid JSON, valid token, but too large.
                0 => [good.clone(), vec![b' '; MAX_REQUEST - good.len() + pad]].concat(),
                1 => format!("{{\"token\":\"{}\",\"method\":\"consumeAccess\",\"args\":[", live.token).into_bytes(),
                2 => json!({"method": "consumeAccess", "args": ["q"]}).to_string().into_bytes(),
                3 => json!({"token": live.token, "args": ["q"]}).to_string().into_bytes(),
                4 => request(&odd, &json!("consumeAccess"), vec![json!("q")], &[]),
                5 => request(&json!(live.token), &odd, vec![json!("q")], &[]),
                6 => vec![b'['; 60_000],
                _ => [good.clone(), vec![0xff, 0xfe]].concat(),
            };
            prop_assert!(authorize(&t, &bad, peer).is_err(), "shape {shape} was accepted");
            // Not an object at all.
            for v in [json!([1, 2]), json!("s"), json!(1), json!(null)] {
                prop_assert!(authorize(&t, v.to_string().as_bytes(), peer).is_err());
            }
            Ok(())
        },
    )
}

#[test]
fn p1_identity_is_the_tokens() {
    assert_eq!(prop_identity_is_the_tokens(), Ok(()));
}

#[test]
fn p2_foreign_peer_is_denied() {
    assert_eq!(prop_foreign_peer_is_denied(), Ok(()));
}

#[test]
fn p3_revoked_and_unknown_are_denied() {
    assert_eq!(prop_revoked_and_unknown_are_denied(), Ok(()));
}

#[test]
fn p5_only_cards_are_authorized() {
    assert_eq!(prop_only_cards_are_authorized(), Ok(()));
}

#[test]
fn p6_malformed_lines_are_denied() {
    assert_eq!(prop_malformed_lines_are_denied(), Ok(()));
}

/// P8: a card yes kept for one run is taken by that run alone, once, whichever other runs and calls are asked about.
fn prop_once_is_the_askers_alone() -> Result<(), String> {
    check((0usize..4, 0usize..4, 0usize..3, 0usize..3), |(owner, other, call, other_call)| {
        let t = TokenTable::with_ancestry(descends_window);
        let (run, call_key) = (format!("run{owner}"), format!("call{call}"));
        t.keep_once(&run, call_key.clone(), false);
        let (other_run, other_key) = (format!("run{other}"), format!("call{other_call}"));
        if other_run != run || other_key != call_key {
            prop_assert!(!t.take_once(&other_run, &other_key), "{other_run}/{other_key} took {run}/{call_key}");
        }
        prop_assert!(t.take_once(&run, &call_key), "the asker lost its yes");
        prop_assert!(!t.take_once(&run, &call_key), "the yes was taken twice");
        Ok(())
    })
}

#[test]
fn p8_once_is_the_askers_alone() {
    assert_eq!(prop_once_is_the_askers_alone(), Ok(()));
}

#[test]
fn p7_pid_reuse_and_sibling_runs() {
    assert_eq!(prop_pid_reuse_and_sibling_runs(), Ok(()));
}
