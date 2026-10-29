//! Property tests for `runauth`: a guest run reads nothing beyond its token's audience. The real `authorize`,
//! `TokenTable::resolve`, `RunGrant` and `recall_args` run on generated token tables, request lines and peers; the
//! peer-descends-from-run check is the table's own injectable `ancestry` (a pid window instead of `ps`), which is
//! the seam `runauth` keeps for tests. Nothing here models the code: every property calls it.
//! Method: `.claude/skills/run-tokens/SKILL.md`. Run only these: `cargo test --bin kith runauth_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use prop::sample::select;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};

const ALLOWED: [&str; 3] = ["recall", "memorySearch", "projectId"];
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

fn recall_line(token: &str, args: Vec<Value>, extras: &[(&str, Value)]) -> Vec<u8> {
    request(&json!(token), &json!("recall"), args, extras)
}

// ---- recall arguments

#[derive(Clone, Debug)]
enum Item {
    Word(String),
    Json,
    Flag(&'static str, String),
    Bad(String),
}

const WORD_CH: &[char] = &['a', 'b', 'Z', '0', '9', ' ', '.', '/', ':', '=', '_', '~'];
const DASH_CH: &[char] = &['a', 'p', 'n', '-', '=', 'J', '1', '_'];
const VALUE_FLAGS_UNDER_TEST: &[&str] = &["--project", "--room", "--thread", "--tier", "-n", "--limit"];
const BAD_FLAGS: &[&str] = &[
    "--person",
    "--agent",
    "--audience",
    "--env",
    "--db",
    "--token",
    "--KITH_PERSON",
    "-p",
    "--project=x",
    "--room=x",
    "--json=1",
    "-n5",
    "-",
    "--",
    "-h",
    "--help",
    "--tier=all",
    "--Project",
    "--JSON",
    "--limit=3",
    "--sock",
    "--index",
    "--forget",
];

fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        select(
            &["zebra", "index", "forget", "summarise", "KITH_PERSON=kai", "../etc/passwd", "/etc/passwd", "~/x", "a b"]
                [..]
        )
        .prop_map(String::from),
        prop::collection::vec(select(WORD_CH), 1..12).prop_map(|c| c.into_iter().collect::<String>()),
    ]
}

fn flag_value() -> impl Strategy<Value = String> {
    prop_oneof![
        select(&["--person", "--json", "kai", "--room", "-n", "1", ""][..]).prop_map(String::from),
        prop::collection::vec(select(DASH_CH), 0..8).prop_map(|c| c.into_iter().collect::<String>()),
    ]
}

fn good_item() -> impl Strategy<Value = Item> {
    prop_oneof![
        word().prop_map(Item::Word),
        Just(Item::Json),
        (select(VALUE_FLAGS_UNDER_TEST), flag_value()).prop_map(|(n, v)| Item::Flag(n, v)),
    ]
}

fn bad_item() -> impl Strategy<Value = Item> {
    prop_oneof![
        select(BAD_FLAGS).prop_map(|s| Item::Bad(s.to_owned())),
        prop::collection::vec(select(DASH_CH), 0..8)
            .prop_map(|c| format!("-{}", c.into_iter().collect::<String>()))
            // `-n` is a real flag (a value flag, taking the next argument): not a bad one.
            .prop_filter("an allowed flag", |f| f != "--json" && !VALUE_FLAGS_UNDER_TEST.contains(&f.as_str()))
            .prop_map(Item::Bad),
    ]
}

fn render(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .flat_map(|i| match i {
            Item::Word(w) => vec![json!(w)],
            Item::Json => vec![json!("--json")],
            Item::Flag(n, v) => vec![json!(n), json!(v)],
            Item::Bad(b) => vec![json!(b)],
        })
        .collect()
}

fn words_of(items: &[Item]) -> Vec<String> {
    items.iter().filter_map(|i| if let Item::Word(w) = i { Some(w.clone()) } else { None }).collect()
}

/// Good items only, always at least one query word.
fn valid_args() -> impl Strategy<Value = Vec<Item>> {
    (prop::collection::vec(good_item(), 0..10), word()).prop_map(|(mut v, w)| {
        v.push(Item::Word(w));
        v
    })
}

// ---- properties (each returns Err(counterexample) so the break meta-test can read it)

/// P1: whatever else a request or its args claim, the identity is exactly what the token resolves to.
pub fn prop_identity_is_the_tokens() -> Result<(), String> {
    check((specs(), extras(), valid_args(), 0u32..WINDOW), |(specs, extras, items, off)| {
        let (t, runs) = build(&specs, true);
        for r in &runs {
            let peer = peer_in(r, off);
            let line = recall_line(&r.token, render(&items), &extras);
            let got = authorize(&t, &line, peer).map_err(|e| TestCaseError::fail(format!("refused: {e:?}")))?;
            prop_assert_eq!(&got.identity, &r.identity);
            prop_assert_eq!(Some(got.identity), t.resolve(&r.token, peer));
            prop_assert_eq!(Ok(got.args), recall_args(&render(&items)).map_err(String::from));
            // The same for the two tool methods: who, and which project, is the token's whatever the line claims.
            let line = request(&json!(r.token), &json!("memorySearch"), render(&items), &extras);
            let got = authorize(&t, &line, peer).map_err(|e| TestCaseError::fail(format!("refused: {e:?}")));
            if let Ok(got) = got {
                prop_assert_eq!(&got.identity, &r.identity);
                prop_assert_eq!(got.run, Run::MemorySearch);
            } else {
                // Only a dash word among the generated items may refuse it.
                prop_assert!(memory_args(&render(&items)).is_err());
            }
            let own = r.identity.project.clone();
            let info = own.iter().flat_map(|p| [json!("--info"), json!(p)]).collect::<Vec<_>>();
            for args in [vec![], info] {
                let line = request(&json!(r.token), &json!("projectId"), args.clone(), &extras);
                let got = authorize(&t, &line, peer).map_err(|e| TestCaseError::fail(format!("refused: {e:?}")))?;
                prop_assert_eq!(&got.identity, &r.identity);
                prop_assert_eq!(got.identity.project, own.clone());
                prop_assert_eq!(got.run, Run::ProjectId);
            }
        }
        Ok(())
    })
}

/// P2: a token from a peer that is not the run's own process (or below it), or before it is bound, resolves to nothing.
pub fn prop_foreign_peer_is_denied() -> Result<(), String> {
    check(
        (specs(), extras(), valid_args(), prop::collection::vec(prop::option::of(raw_pid()), 6), any::<bool>()),
        |(specs, extras, items, raws, bound)| {
            let (t, runs) = build(&specs, bound);
            for (i, r) in runs.iter().enumerate() {
                let peer = PeerInfo { pid: raws[i].map(|p| if bound { outside(p, r.pid) } else { p }) };
                let line = recall_line(&r.token, render(&items), &extras);
                prop_assert_eq!(t.resolve(&r.token, peer), None);
                prop_assert_eq!(authorize(&t, &line, peer), Err(refused()));
                if bound {
                    // Another run's own process is just as foreign.
                    for o in runs.iter().filter(|o| o.pid != r.pid) {
                        let line = recall_line(&r.token, render(&items), &extras);
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
        (
            specs(),
            extras(),
            valid_args(),
            prop::collection::vec(any::<bool>(), 1..6),
            0u32..WINDOW,
            0..MUTATIONS,
            ".{0,40}",
        ),
        |(specs, extras, items, mask, off, k, junk)| {
            let (t, mut runs) = build(&specs, true);
            for (i, r) in runs.iter_mut().enumerate() {
                if mask[i % mask.len()] {
                    drop(r.grant.take());
                }
            }
            prop_assert_eq!(t.len(), runs.iter().filter(|r| r.grant.is_some()).count());
            for r in &runs {
                let peer = peer_in(r, off);
                let got = authorize(&t, &recall_line(&r.token, render(&items), &extras), peer);
                if r.grant.is_some() {
                    prop_assert_eq!(got.map(|a| a.identity), Ok(r.identity.clone()));
                } else {
                    prop_assert_eq!(got, Err(refused()));
                    prop_assert_eq!(t.resolve(&r.token, peer), None);
                }
                let bad = mutate(&r.token, k, &junk);
                prop_assume!(!runs.iter().any(|o| o.grant.is_some() && o.token == bad));
                prop_assert_eq!(t.resolve(&bad, peer), None);
                prop_assert_eq!(authorize(&t, &recall_line(&bad, render(&items), &extras), peer), Err(refused()));
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
        prop_assert_eq!(authorize(&t, &recall_line(&x.token, vec![json!("q")], &[]), peer_in(y, 0)), Err(refused()));
        prop_assert_eq!(authorize(&t, &recall_line(&y.token, vec![json!("q")], &[]), peer_in(x, 0)), Err(refused()));
        // Run 0 exits; its pid is handed to a new run of the same person in the other room.
        let old = runs.remove(0);
        let (old_token, old_pid) = (old.token.clone(), old.pid);
        drop(old);
        let fresh = RunGrant::issue(&t, RunIdentity { run_id: "fresh".into(), ..runs[0].identity.clone() }).unwrap();
        fresh.bind_pid(old_pid);
        let fresh_token = fresh.env(Path::new("/s"))[0].1.clone();
        let reused = PeerInfo { pid: Some(old_pid) };
        prop_assert_eq!(authorize(&t, &recall_line(&old_token, vec![json!("q")], &[]), reused), Err(refused()));
        let ok = authorize(&t, &recall_line(&fresh_token, vec![json!("q")], &[]), reused).unwrap();
        prop_assert_eq!(ok.identity.run_id, "fresh");
        Ok(())
    })
}

/// P4: `recall_args` passes only the query words and the allow-listed narrowing flags; any other dash argument is
/// refused, and nothing is dropped or invented.
pub fn prop_recall_args_allow_list() -> Result<(), String> {
    check(
        (prop::collection::vec(prop_oneof![3 => good_item(), 1 => bad_item()], 0..10), any::<bool>()),
        |(items, tail_word)| {
            let mut items = items;
            if tail_word {
                items.push(Item::Word("zebra".into()));
            }
            let args = render(&items);
            let bad = items.iter().any(|i| matches!(i, Item::Bad(_)));
            let words = words_of(&items);
            let got = recall_args(&args);
            if bad || words.is_empty() {
                prop_assert!(got.is_err(), "{:?} should be refused, got {:?}", args, got);
                return Ok(());
            }
            let out = got.map_err(|e| TestCaseError::fail(format!("refused a good request: {e}")))?;
            prop_assert_eq!(out[0].as_str(), "--tier", "a query word can never reach a subcommand");
            let mut i = 0;
            while i < out.len() && out[i].starts_with('-') {
                match out[i].as_str() {
                    "--json" => i += 1,
                    "--tier" | "-n" | "--project" | "--room" | "--thread" => {
                        prop_assert!(i + 1 < out.len());
                        i += 2;
                    }
                    other => prop_assert!(false, "{other} was passed through"),
                }
            }
            let mut rest: Vec<String> = out[i..].to_vec();
            let mut want = words;
            rest.sort();
            want.sort();
            prop_assert_eq!(rest, want, "the query words are exactly the input's, none starting with a dash");
            Ok(())
        },
    )
}

/// P4b: caps and types.
pub fn prop_recall_args_caps_and_types() -> Result<(), String> {
    check((MAX_ARGS + 1..MAX_ARGS + 20, 0..4usize), |(n, kind)| {
        prop_assert!(recall_args(&vec![json!("a"); n]).is_err());
        let odd = [json!(1), json!(null), json!(["a"]), json!({"a": 1})][kind].clone();
        prop_assert!(recall_args(&[json!("a"), odd]).is_err());
        prop_assert!(recall_args(&[json!("x".repeat(MAX_ARG_LEN + 1))]).is_err());
        Ok(())
    })
}

/// P4c: `memory_args` passes only the query words, `-n N` and `--json`; `--all`, `--agent` and every other dash argument
/// are refused; the result starts with `search`, so no word reaches another subcommand.
pub fn prop_memory_args_allow_list() -> Result<(), String> {
    check(
        (prop::collection::vec(prop_oneof![3 => good_item(), 1 => bad_item()], 0..10), any::<bool>()),
        |(items, tail_word)| {
            let mut items = items;
            if tail_word {
                items.push(Item::Word("zebra".into()));
            }
            let args = render(&items);
            let got = memory_args(&args);
            // Here only `--json` and `-n <digits>` are allowed, so a generated flag with a value is mostly refused.
            let allowed = |a: &Value| a == "--json";
            let mut ok = true;
            let mut words = Vec::new();
            let mut it = args.iter();
            while let Some(a) = it.next() {
                if allowed(a) {
                    continue;
                }
                if a == "-n" {
                    ok &= it
                        .next()
                        .and_then(Value::as_str)
                        .is_some_and(|v| !v.is_empty() && v.len() <= 6 && v.bytes().all(|b| b.is_ascii_digit()));
                } else if a.as_str().is_some_and(|s| s.starts_with('-')) {
                    ok = false;
                } else {
                    words.push(a.as_str().unwrap().to_owned());
                }
            }
            if !ok || words.is_empty() {
                prop_assert!(got.is_err(), "{:?} should be refused, got {:?}", args, got);
                return Ok(());
            }
            let out = got.map_err(|e| TestCaseError::fail(format!("refused a good request: {e}")))?;
            prop_assert_eq!(out[0].as_str(), "search");
            let mut i = 1;
            while out[i].starts_with('-') {
                i += if out[i] == "-n" { 2 } else { 1 };
            }
            for flag in &out[1..i] {
                prop_assert!(
                    flag == "-n" || flag == "--json" || flag.bytes().all(|b| b.is_ascii_digit()),
                    "{flag} passed"
                );
            }
            let (mut rest, mut want) = (out[i..].to_vec(), words);
            rest.sort();
            want.sort();
            prop_assert_eq!(rest, want, "the query words are exactly the input's");
            Ok(())
        },
    )
}

/// P4d: `project_args` allows nothing, or `--info` of the token's own project; every other argument list is refused.
pub fn prop_project_args_allow_list() -> Result<(), String> {
    let verbs =
        &["--list", "--children", "--where", "--register", "--root", "--info", "--all", "--home", "--in", "."][..];
    check(
        (
            prop_oneof![Just(None), Just(Some("alpha")), Just(Some("beta"))],
            prop::collection::vec(
                prop_oneof![
                    select(verbs).prop_map(String::from),
                    word(),
                    Just("alpha".to_owned()),
                    Just("beta".to_owned())
                ],
                0..4,
            ),
        ),
        |(project, args)| {
            let given: Vec<Value> = args.iter().map(|a| json!(a)).collect();
            let got = project_args(&given, project);
            let allowed =
                args.is_empty() || (args.len() == 2 && args[0] == "--info" && Some(args[1].as_str()) == project);
            prop_assert_eq!(got.is_ok(), allowed, "{:?} for {:?}", args, project);
            if let Ok(out) = got {
                prop_assert_eq!(out, args);
            }
            Ok(())
        },
    )
}

/// P4e: caps and types of both new parsers.
pub fn prop_tool_args_caps_and_types() -> Result<(), String> {
    check((MAX_ARGS + 1..MAX_ARGS + 20, 0..4usize), |(n, kind)| {
        prop_assert!(memory_args(&vec![json!("a"); n]).is_err());
        let odd = [json!(1), json!(null), json!(["a"]), json!({"a": 1})][kind].clone();
        prop_assert!(memory_args(&[json!("a"), odd.clone()]).is_err());
        prop_assert!(memory_args(&[json!("x".repeat(MAX_ARG_LEN + 1))]).is_err());
        prop_assert!(project_args(&[odd], Some("alpha")).is_err());
        prop_assert!(project_args(&[json!("--info"), json!("a".repeat(MAX_ARG_LEN + 1))], Some("alpha")).is_err());
        Ok(())
    })
}

/// P5: only `recall`, `memorySearch` and `projectId` are ever authorized for a guest token (the card methods are
/// refused here for want of their fields), whatever the token, peer and args; an owner token gets none of the three.
pub fn prop_only_recall_is_authorized() -> Result<(), String> {
    check(
        (
            specs(),
            valid_args(),
            prop_oneof![
                select(
                    &[
                        "forget",
                        "index",
                        "summarise",
                        "shell",
                        "Recall",
                        " recall",
                        "recall ",
                        "recall\0",
                        "",
                        "RECALL",
                        "recal",
                        "exec",
                        "send",
                        // The other methods: refused without their fields (a tool; a kind, target and reason), and checked
                        // on their own in runauth.rs and cards/access_tests.rs (`consumeAccess` takes none, so it isn't here).
                        "requestPermission",
                        "requestAccess"
                    ][..]
                )
                .prop_map(|m| json!(m)),
                ".{0,12}"
                    .prop_filter("not an allowed method", |m| !ALLOWED.contains(&m.as_str()))
                    .prop_map(|m| json!(m)),
                select(
                    &[
                        "memorysearch",
                        "MemorySearch",
                        "memory_search",
                        "projectid",
                        "ProjectId",
                        "project-id",
                        "memorySearch "
                    ][..]
                )
                .prop_map(|m| json!(m)),
                Just(json!(1)),
                Just(json!(null)),
                Just(json!(["recall"])),
            ],
        ),
        |(specs, items, method)| {
            let (t, runs) = build(&specs, true);
            for r in &runs {
                let line = request(&json!(r.token), &method, render(&items), &[]);
                prop_assert!(authorize(&t, &line, peer_in(r, 0)).is_err(), "method {method} was authorized");
                let line = request(&json!(r.token), &json!("recall"), render(&items), &[]);
                prop_assert!(authorize(&t, &line, peer_in(r, 0)).is_ok(), "the allowed method is refused");
                // The new methods are authorized too (so refusing everything cannot pass), with args each accepts.
                let line = request(&json!(r.token), &json!("memorySearch"), vec![json!("zebra")], &[]);
                prop_assert!(authorize(&t, &line, peer_in(r, 0)).is_ok(), "memorySearch is refused");
                let line = request(&json!(r.token), &json!("projectId"), vec![], &[]);
                prop_assert!(authorize(&t, &line, peer_in(r, 0)).is_ok(), "projectId is refused");
            }
            // An owner token (cards only) is refused all three.
            let owner = Arc::new(TokenTable::with_ancestry(descends_window));
            let token = owner.insert_ask(ask_identity("echo", "s", None), BASE, Duration::from_secs(60)).unwrap();
            for m in ["recall", "memorySearch", "projectId"] {
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
            let good = recall_line(&live.token, vec![json!("q")], &[]);
            prop_assert!(authorize(&t, &good, peer).is_ok());
            prop_assert!(authorize(&t, &junk, peer).is_err(), "random bytes were accepted");
            let bad: Vec<u8> = match shape {
                // Valid JSON, valid token, but too large.
                0 => [good.clone(), vec![b' '; MAX_REQUEST - good.len() + pad]].concat(),
                1 => format!("{{\"token\":\"{}\",\"method\":\"recall\",\"args\":[", live.token).into_bytes(),
                2 => json!({"method": "recall", "args": ["q"]}).to_string().into_bytes(),
                3 => json!({"token": live.token, "args": ["q"]}).to_string().into_bytes(),
                4 => request(&odd, &json!("recall"), vec![json!("q")], &[]),
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
fn p4_recall_args_allow_list() {
    assert_eq!(prop_recall_args_allow_list(), Ok(()));
    assert_eq!(prop_recall_args_caps_and_types(), Ok(()));
}

#[test]
fn p4_tool_args_allow_lists() {
    assert_eq!(prop_memory_args_allow_list(), Ok(()));
    assert_eq!(prop_project_args_allow_list(), Ok(()));
    assert_eq!(prop_tool_args_caps_and_types(), Ok(()));
}

#[test]
fn p5_only_recall_is_authorized() {
    assert_eq!(prop_only_recall_is_authorized(), Ok(()));
}

#[test]
fn p6_malformed_lines_are_denied() {
    assert_eq!(prop_malformed_lines_are_denied(), Ok(()));
}

#[test]
fn p7_pid_reuse_and_sibling_runs() {
    assert_eq!(prop_pid_reuse_and_sibling_runs(), Ok(()));
}
