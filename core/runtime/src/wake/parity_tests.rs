//! Write parity with `kernel/bin/kith-wake`, with no Python at test time: every case of
//! `kernel/tests/difftest/golden/wake.json` is replayed as core calls against a test core (its `setup` files imported first,
//! as `kith migrate` would, then each `run` step as the `wake.*` row the verb maps to), and what the step prints (stdout,
//! stderr, exit) is compared with the frozen facets in `golden/facets/<case>.json`, normalized as `kith-difftest` does
//! (timestamps, ids, the case's `mask_keys`).
//!
//! The second check is import fidelity: the state files a case ended with (the facets' `state:` entries, un-masked) are
//! imported into a second core, and `ls`, `ls --json` and `status` of every id there must print what the replayed core
//! prints.
//!
//! Cases that cannot map are listed in [`UNMAPPED`] with the reason; the one that differs on purpose is in [`INTENDED`].
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::unused_self,
    clippy::case_sensitive_file_extension_comparisons
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::call::{call_with, tick};
use super::outside::{Check, Files, PrView, Transcripts};
use super::tests::{Fake, core};
use crate::db::Core;
use crate::error::Error;
use crate::import::wake::run_in;

/// Cases the core cannot replay, and why. Argparse usage errors are the CLI's, not the core's: the table's params are
/// checked before a row runs (`kith_api::params::validate`), and a missing one is a `BadParams` there.
const UNMAPPED: &[(&str, &str)] = &[
    (
        "wake-enqueue-usage",
        "argparse usage errors belong to the CLI",
    ),
    (
        "wake-withdraw-person-usage",
        "argparse usage errors belong to the CLI",
    ),
    ("wake-take-usage", "argparse usage errors belong to the CLI"),
    (
        "wake-done-back-usage",
        "argparse usage errors belong to the CLI",
    ),
    ("wake-wait-usage", "argparse usage errors belong to the CLI"),
    (
        "wake-deliver-ok-commits",
        "deliver runs `kith AGENT --resume` as a child process; in the core a turn starts through the conversations lane (M3-2)",
    ),
    (
        "wake-deliver-nonzero-with-marker-seen-commits",
        "deliver runs a child process; the marker rule it ends with is `recover`, covered by the take-recovers cases",
    ),
    (
        "wake-deliver-held-is-not-a-failed-try",
        "deliver runs a child process; the held rule is `back --held`, covered by the back cases",
    ),
    (
        "wake-deliver-failed-try-backs-off",
        "deliver runs a child process; the back-off is `back`, covered by the back cases",
    ),
    (
        "wake-deliver-scrubs-the-run-env",
        "deliver scrubs the environment of a child process; the core starts no process here",
    ),
];

/// Cases that differ on purpose.
const INTENDED: &[(&str, &str)] = &[(
    "wake-deliver-not-owed-or-held-does-nothing",
    "q217: a message for a session a terminal holds queues until the session ends. The Python drain skips the session and \
     leaves its item owed; in the core the turn is created through the conversations lane and parks as `terminal_live`, never \
     typed in. Here that is the conversations lane's (M3-2), and `deliver` is a process start: not replayed. (`take` keeps the \
     queue's side of it: a session a live writer holds is not offered, see `a_session_a_live_writer_holds_is_not_offered`.)",
)];

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/tests/difftest/golden")
}

fn cases() -> Vec<Value> {
    let text = std::fs::read_to_string(golden_dir().join("wake.json")).expect("golden/wake.json");
    serde_json::from_str(&text).expect("wake.json is JSON")
}

fn facets(name: &str) -> Map<String, Value> {
    let text = std::fs::read_to_string(golden_dir().join("facets").join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("facets/{name}.json: {e}"));
    serde_json::from_str(&text).expect("a facet file is a JSON object")
}

// --- the normalizer of kith-difftest -----------------------------------------------------------------------------------

struct Norm {
    mask: BTreeSet<String>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `f` over each maximal word of `s` (what `\b...\b` sees as one token); `None` keeps the word.
fn on_words(s: &str, f: impl Fn(&str) -> Option<&'static str>) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str(f(word).unwrap_or(word));
            word.clear();
        }
    };
    for c in s.chars() {
        if is_word(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn is_hex(w: &str) -> bool {
    w.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `\d{4}-\d\d-\d\d[T ]\d\d:\d\d(?::\d\d(?:\.\d+)?)?(?:Z|[+-]\d\d:?\d\d)?`: the length of a match at the start of `b`.
fn ts_len(b: &[u8]) -> Option<usize> {
    let d = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let is = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(d(0) && d(1) && d(2) && d(3) && is(4, b'-') && d(5) && d(6) && is(7, b'-') && d(8) && d(9))
    {
        return None;
    }
    if !(is(10, b'T') || is(10, b' ')) || !(d(11) && d(12) && is(13, b':') && d(14) && d(15)) {
        return None;
    }
    let mut n = 16;
    if is(n, b':') && d(n + 1) && d(n + 2) {
        n += 3;
        if is(n, b'.') && d(n + 1) {
            n += 1;
            while d(n) {
                n += 1;
            }
        }
    }
    if is(n, b'Z') {
        n += 1;
    } else if (is(n, b'+') || is(n, b'-')) && d(n + 1) && d(n + 2) {
        let m = if is(n + 3, b':') { n + 4 } else { n + 3 };
        if d(m) && d(m + 1) {
            n = m + 2;
        }
    }
    Some(n)
}

fn timestamps(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if let Some(n) = ts_len(&b[i..]) {
            out.push_str("<TS>");
            i += n;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

impl Norm {
    fn of(case: &Value) -> Self {
        let mask = case
            .get("mask_keys")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self { mask }
    }

    fn text(&self, s: &str) -> String {
        // The case's own rule first (`\b[0-9a-f]{12}\b`), then the generic ones.
        let s = on_words(s, |w| {
            (w.len() == 12 && is_hex(w) && !w.bytes().any(|b| b.is_ascii_uppercase()))
                .then_some("<ID12>")
        });
        let s = timestamps(&s);
        on_words(&s, |w| {
            if w.len() == 40 && is_hex(w) {
                Some("<SHA>")
            } else if w.len() >= 32 && is_hex(w) {
                Some("<HEX>")
            } else if (w.len() == 10 || w.len() == 13)
                && w.bytes().all(|b| b.is_ascii_digit())
                && w.starts_with('1')
                && matches!(w.as_bytes()[1], b'5'..=b'9')
            {
                Some("<EPOCH>")
            } else {
                None
            }
        })
    }

    fn value(&self, k: &str, x: &Value) -> Value {
        if self.mask.contains(k) && x.is_number() {
            return json!("<EPOCH>");
        }
        if self.mask.contains(k) && !x.is_object() && !x.is_array() {
            return json!("<MASKED>");
        }
        self.data(x)
    }

    fn data(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.data(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (self.text(k), self.value(k, x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

// --- the case's world --------------------------------------------------------------------------------------------------

/// What the fake `gh` of a case answers: `view) echo '...'` and `checks) echo '...'` of its script; a script that prints
/// something else (gh down) answers nothing.
fn gh_answers(script: &str) -> (Option<PrView>, Option<Vec<Check>>) {
    let quoted = |label: &str| -> Option<Value> {
        let line = script.lines().find(|l| l.trim_start().starts_with(label))?;
        let (a, b) = (line.find('\'')?, line.rfind('\'')?);
        serde_json::from_str(line.get(a + 1..b)?).ok()
    };
    let view = quoted("view)").map(|v| PrView {
        sha: v
            .get("headRefOid")
            .and_then(Value::as_str)
            .map(str::to_owned),
        state: v.get("state").and_then(Value::as_str).map(str::to_owned),
    });
    let checks = quoted("checks)").and_then(|v| {
        v.as_array().map(|a| {
            a.iter()
                .map(|c| Check {
                    name: c["name"].as_str().unwrap_or_default().to_owned(),
                    bucket: c["bucket"].as_str().unwrap_or_default().to_owned(),
                })
                .collect()
        })
    });
    (view, checks)
}

/// A case laid out the way its setup says, under `root/home`.
struct World {
    _root: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    gh: (Option<PrView>, Option<Vec<Check>>),
}

fn world(case: &Value) -> World {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let mut gh = (None, None);
    for s in case["setup"].as_array().unwrap() {
        let Some(path) = s.get("write").and_then(Value::as_str) else {
            continue;
        };
        let text = s["text"]
            .as_str()
            .unwrap()
            .replace("{HOME}", home.to_str().unwrap());
        if path == "home/bin/gh-fake" {
            gh = gh_answers(&text);
            continue;
        }
        let p = root.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let state = home.join(".kith");
    std::fs::create_dir_all(state.join("sessions")).unwrap();
    World {
        home,
        state,
        gh,
        _root: root,
    }
}

fn fake(w: &World) -> Fake {
    let f = Fake::new(&w.state);
    *f.view.borrow_mut() = w.gh.0.clone();
    *f.checks.borrow_mut() = w.gh.1.clone();
    f
}

// --- a command line as a row -------------------------------------------------------------------------------------------

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    flags: BTreeMap<String, String>,
    on: BTreeSet<String>,
    after: Vec<String>,
}

fn parse(argv: &[String], with_value: &[&str], switches: &[&str]) -> Args {
    let mut a = Args::default();
    let mut it = argv.iter();
    while let Some(x) = it.next() {
        if x == "--" {
            a.after = it.by_ref().cloned().collect();
        } else if with_value.contains(&x.as_str()) {
            a.flags
                .insert(x.clone(), it.next().cloned().unwrap_or_default());
        } else if switches.contains(&x.as_str()) {
            a.on.insert(x.clone());
        } else {
            a.pos.push(x.clone());
        }
    }
    a
}

#[derive(Debug, Default)]
struct Out {
    stdout: String,
    stderr: String,
    exit: i32,
}

fn lines(v: &Value) -> String {
    v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .fold(String::new(), |s, l| s + l.as_str().unwrap() + "\n")
}

fn put(p: &mut Map<String, Value>, k: &str, v: Option<&String>) {
    if let Some(v) = v {
        p.insert(k.into(), json!(v));
    }
}

/// One command line of `kith-wake`, as the row it maps to, answered the way the verb prints.
fn step(
    core: &mut Core,
    f: &Fake,
    tr: &dyn Transcripts,
    env: &BTreeMap<String, String>,
    argv: &[String],
) -> Out {
    let rest = &argv[1..];
    let mut p = Map::new();
    let (method, a) = match argv[0].as_str() {
        "enqueue" => {
            let a = parse(
                rest,
                &["--id", "--agent", "--person", "--room", "--door", "--why"],
                &[],
            );
            put(&mut p, "session", a.pos.first());
            for (flag, key) in [
                ("--id", "id"),
                ("--agent", "agent"),
                ("--person", "person"),
                ("--room", "room"),
                ("--door", "door"),
                ("--why", "why"),
            ] {
                put(&mut p, key, a.flags.get(flag));
            }
            p.insert("text".into(), json!(a.after));
            ("wake.enqueue", a)
        }
        "withdraw" => {
            let a = parse(rest, &["--id"], &[]);
            put(&mut p, "session", a.pos.first());
            put(&mut p, "id", a.flags.get("--id"));
            ("wake.withdraw", a)
        }
        "withdraw-person" => {
            let a = parse(rest, &[], &[]);
            p.insert("persons".into(), json!(a.pos));
            ("wake.withdrawPerson", a)
        }
        "take" => {
            let a = parse(rest, &["--holder", "--session", "--sid"], &[]);
            p.insert(
                "holder".into(),
                json!(a.flags["--holder"].parse::<i64>().unwrap()),
            );
            put(&mut p, "session", a.flags.get("--session"));
            put(&mut p, "sid", a.flags.get("--sid"));
            ("wake.take", a)
        }
        "done" => {
            let a = parse(rest, &["--how"], &[]);
            put(&mut p, "run", a.pos.first());
            put(&mut p, "how", a.flags.get("--how"));
            ("wake.done", a)
        }
        "back" => {
            let a = parse(rest, &[], &["--held"]);
            put(&mut p, "run", a.pos.first());
            if a.on.contains("--held") {
                p.insert("held".into(), json!(true));
            }
            ("wake.back", a)
        }
        "ls" => ("wake.queueList", parse(rest, &[], &["--json"])),
        "status" => {
            let a = parse(rest, &[], &["--json"]);
            put(&mut p, "id", a.pos.first());
            ("wake.queueStatus", a)
        }
        "wait" => {
            let a = parse(
                rest,
                &["--on", "--session", "--agent", "--why", "--timeout"],
                &[],
            );
            put(&mut p, "on", a.flags.get("--on"));
            // The client fills what the caller's environment says.
            put(
                &mut p,
                "session",
                a.flags
                    .get("--session")
                    .or_else(|| env.get("CLAUDE_CODE_SESSION_ID")),
            );
            put(
                &mut p,
                "agent",
                a.flags.get("--agent").or_else(|| env.get("KITH_AGENT")),
            );
            put(&mut p, "why", a.flags.get("--why"));
            put(&mut p, "timeout", a.flags.get("--timeout"));
            put(&mut p, "person", env.get("KITH_PERSON"));
            put(&mut p, "via", env.get("KITH_VIA"));
            p.insert("note".into(), json!(a.pos));
            ("wake.wait", a)
        }
        "drain" => {
            // The tick without its process starts: fire the waiters, recover the dead turns.
            tick(core, f, tr).unwrap();
            return Out::default();
        }
        other => panic!("a verb the replay does not know: {other}"),
    };
    let params = Value::Object(p);
    let answer = crate::call::check(kith_api::Principal::Owner, method, &params)
        .and_then(|_| call_with(core, method, &params, f, tr));
    let v = match answer {
        Ok(v) => v,
        Err(Error::Refused(m)) => {
            return Out {
                stderr: format!("kith-wake: {m}\n"),
                exit: 1,
                ..Out::default()
            };
        }
        Err(e) => panic!("{method}: {e}"),
    };
    let json = a.on.contains("--json");
    let mut out = Out::default();
    match method {
        "wake.done" | "wake.back" => out.exit = i32::from(v["done"] != true),
        "wake.take" => {
            out.exit = i32::from(v["turn"].is_null());
            out.stdout = lines(&v);
        }
        "wake.queueList" if json => out.stdout = serde_json::to_string(&v["items"]).unwrap(),
        "wake.queueStatus" if json => {
            out.stdout = json!({"id": v["id"], "state": v["state"], "run": v["run"]}).to_string();
        }
        _ => out.stdout = lines(&v),
    }
    out
}

// --- comparing ---------------------------------------------------------------------------------------------------------

/// What differs between a step's output and its facets, one line each.
fn compare(norm: &Norm, facets: &Map<String, Value>, i: usize, out: &Out) -> Vec<String> {
    let mut diffs = Vec::new();
    let get = |k: &str| facets.get(&format!("step{i}.{k}")).and_then(Value::as_str);
    let code = format!("{}\n", out.exit);
    if get("exit") != Some(code.as_str()) {
        diffs.push(format!(
            "step{i}: exit {} but the facet says {:?}",
            out.exit,
            get("exit")
        ));
    }
    if get("stderr").is_some_and(|e| e != norm.text(&out.stderr)) {
        diffs.push(format!(
            "step{i}: stderr {:?} but the facet says {:?}",
            norm.text(&out.stderr),
            get("stderr")
        ));
    }
    if let Some(want) = get("json") {
        let want: Value = serde_json::from_str(want).unwrap();
        match serde_json::from_str::<Value>(&out.stdout) {
            Ok(got) if norm.data(&got) == want => {}
            Ok(got) => diffs.push(format!(
                "step{i}: json {} but the facet says {want}",
                norm.data(&got)
            )),
            Err(_) => diffs.push(format!(
                "step{i}: stdout {:?} is not the JSON the facet holds",
                out.stdout
            )),
        }
    } else if let Some(want) = get("stdout")
        && norm.text(&out.stdout) != want
    {
        diffs.push(format!(
            "step{i}: stdout {:?} but the facet says {want:?}",
            norm.text(&out.stdout)
        ));
    }
    diffs
}

fn argv_of(step: &Value) -> Vec<String> {
    step.as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect()
}

fn env_of(case: &Value) -> BTreeMap<String, String> {
    case["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect()
}

/// A case's frozen state files with the masks put back as values that mean the same to the import.
fn unmask(text: &str) -> String {
    text.replace("\"<EPOCH>\"", "4000000000.0")
        .replace("\"<MASKED>\"", "null")
        .replace("<TS>", "2026-01-01T00:00:00Z")
        .replace("<ID12>", "aaaaaaaaaaaa")
        .replace("<SHA>", &"a".repeat(40))
}

/// What `ls`, `ls --json` and `status` of each id print on this core, normalized.
fn printed(
    core: &mut Core,
    f: &Fake,
    tr: &dyn Transcripts,
    norm: &Norm,
    ids: &BTreeSet<String>,
) -> (String, Value, Vec<String>) {
    let env = BTreeMap::new();
    let a = step(core, f, tr, &env, &["ls".to_owned()]);
    let j = step(core, f, tr, &env, &["ls".to_owned(), "--json".to_owned()]);
    // The ids a mask hides (a CI wake's names a commit) differ between the two cores; what is asked for each is compared as a
    // sorted list.
    let mut statuses: Vec<String> = ids
        .iter()
        .map(|id| {
            let s = step(core, f, tr, &env, &["status".to_owned(), id.clone()]);
            norm.text(&s.stdout)
        })
        .collect();
    statuses.sort();
    (
        norm.text(&a.stdout),
        norm.data(&serde_json::from_str(&j.stdout).unwrap()),
        statuses,
    )
}

fn ids_of(core: &mut Core) -> BTreeSet<String> {
    let st = super::read(core, super::tests::T0, Clone::clone).unwrap();
    st.items
        .values()
        .flat_map(|i| i.requests.iter().map(|r| r.id.clone()))
        .chain(st.answered.keys().cloned())
        .collect()
}

fn replay(case: &Value) -> Vec<String> {
    let name = case["name"].as_str().unwrap();
    let norm = Norm::of(case);
    let w = world(case);
    let f = fake(&w);
    let tr = Files {
        roots: vec![w.home.join(".claude")],
    };
    let (_t, _l, mut c) = core();
    run_in(&mut c, &w.state, false).unwrap_or_else(|e| panic!("{name}: import: {e}"));
    let facets = facets(name);
    let env = env_of(case);
    let mut diffs = Vec::new();
    let mut asked = BTreeSet::new();
    for (i, s) in case["run"].as_array().unwrap().iter().enumerate() {
        let argv = argv_of(s);
        if argv[0] == "status" {
            asked.insert(argv[1].clone());
        }
        let out = step(&mut c, &f, &tr, &env, &argv);
        diffs.extend(compare(&norm, &facets, i, &out));
    }

    // Import fidelity: the state files the Python ended with, imported into a second core, print the same.
    let second = tempfile::tempdir().unwrap();
    let state2 = second.path().join("state");
    let prefix = "state:home/.kith/";
    for (k, v) in &facets {
        let Some(rel) = k.strip_prefix(prefix) else {
            continue;
        };
        let is_state = (rel.starts_with("sessions/queue/") || rel.starts_with("sessions/waits/"))
            && rel.ends_with(".json");
        if !is_state {
            continue;
        }
        let p = state2.join(rel.replace("<ID12>", "aaaaaaaaaaaa"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, unmask(v.as_str().unwrap())).unwrap();
    }
    let (_t2, _l2, mut c2) = core();
    if let Err(e) = run_in(&mut c2, &state2, false) {
        diffs.push(format!("import of the final state files: {e}"));
        return diffs;
    }
    let mut ids = ids_of(&mut c);
    ids.extend(ids_of(&mut c2));
    ids.extend(asked);
    let f2 = Fake::new(&state2);
    let (a, b) = (
        printed(&mut c, &f, &tr, &norm, &ids),
        printed(&mut c2, &f2, &tr, &norm, &ids),
    );
    if a.0 != b.0 {
        diffs.push(format!(
            "import fidelity: ls prints {:?} after the replay but {:?} after importing the files",
            a.0, b.0
        ));
    }
    if a.1 != b.1 {
        diffs.push(format!("import fidelity: ls --json prints {} after the replay but {} after importing the files", a.1, b.1));
    }
    if a.2 != b.2 {
        diffs.push(format!("import fidelity: status prints {:?} after the replay but {:?} after importing the files", a.2, b.2));
    }
    diffs
}

#[test]
fn every_golden_wake_case_is_replayed_or_listed() {
    let names: BTreeSet<String> = cases()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    for (n, why) in UNMAPPED.iter().chain(INTENDED) {
        assert!(
            names.contains(*n),
            "{n} is listed but is not in golden/wake.json"
        );
        assert_ne!(*why, "");
    }
    let listed = UNMAPPED.len() + INTENDED.len();
    let replayed = names.len() - listed;
    assert!(
        replayed >= 70,
        "only {replayed} of {} cases are replayed",
        names.len()
    );
}

#[test]
fn the_core_prints_what_kith_wake_prints_for_every_golden_case() {
    let skip: BTreeSet<&str> = UNMAPPED.iter().chain(INTENDED).map(|(n, _)| *n).collect();
    let mut failures = Vec::new();
    let mut ran = 0;
    for case in cases() {
        let name = case["name"].as_str().unwrap().to_owned();
        if skip.contains(name.as_str()) {
            continue;
        }
        ran += 1;
        for d in replay(&case) {
            failures.push(format!("{name}: {d}"));
        }
    }
    assert!(ran >= 70);
    assert!(
        failures.is_empty(),
        "{} difference(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
