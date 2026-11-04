#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::unused_self,
    clippy::needless_pass_by_value
)]

use std::path::{Path, PathBuf};

use kith_kernel::threads::Ctx;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::place::{Choice, apply, decide, step};
use super::read::{FsReader, TranscriptReader, latest, pick, recent, resolve_read};
use super::{
    Located, SessionPtr, config_dir, engine_of, find_everywhere, hash_session, locate, slug,
};
use crate::layout::Layout;
use crate::tests::{machine, open_all};

fn no_env(_: &str) -> Option<String> {
    None
}

/// A home with Claude's login in it and a machine folder; `ctx` looks at both.
struct World {
    _t: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("kith/machine")).unwrap();
        Self { _t: t, root }
    }
    fn ctx(&self) -> Ctx<'static> {
        Ctx {
            state: Some(self.root.join("home/.kith")),
            machine: Some(self.root.join("kith/machine")),
            home: self.root.join("home"),
            env: &no_env,
        }
    }
    fn main(&self) -> PathBuf {
        self.root.join("home/.claude")
    }
    fn session(&self, config: &Path, project: &str, sid: &str) -> SessionPtr {
        SessionPtr {
            config_dir: config.to_path_buf(),
            project: project.to_owned(),
            sid: sid.to_owned(),
        }
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

const SID: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn the_migration_makes_engine_sessions_keyed_by_conversation_and_account() {
    let (_t, m) = machine();
    let core = open_all(&Layout::new(&m));
    let c = core.conn();
    let ins = |conv: &str, account: &str| {
        c.execute(
            "INSERT INTO engine_sessions (conversation, account, engine, session_id, created_at) VALUES (?1, ?2, 'claude', 's', 1)",
            [conv, account],
        )
    };
    ins("c", "claude").unwrap();
    ins("c", "claude:work").unwrap();
    assert!(
        ins("c", "claude").is_err(),
        "one row per conversation and account"
    );
}

#[test]
fn the_slug_is_what_claude_names_a_project_folder() {
    assert_eq!(slug("/Users/k/kith"), "-Users-k-kith");
    assert_eq!(slug(r"C:\Users\k\my app"), "C--Users-k-my-app");
}

#[test]
fn a_pointer_round_trips_and_other_shapes_are_refused() {
    let w = World::new();
    let p = w.session(&w.main(), "-proj", SID);
    assert_eq!(SessionPtr::from_text(&p.text()), Some(p.clone()));
    assert_eq!(p.sidecar(), w.main().join("projects/-proj").join(SID));
    assert_eq!(SessionPtr::from_text("/x/y/z.jsonl"), None);
    assert_eq!(SessionPtr::from_text("/x/projects/p/z.txt"), None);
    assert_eq!(SessionPtr::from_text(""), None);
}

#[test]
fn the_locator_finds_claude_sessions_per_account_and_holds_no_other_engine() {
    let w = World::new();
    let ctx = w.ctx();
    let work = kith_kernel::accounts::account_dir(
        &kith_kernel::accounts::Ctx {
            machine: w.root.join("kith/machine"),
            agents: PathBuf::new(),
            kernel: PathBuf::new(),
            home: w.root.join("home"),
            claude_config_dir: None,
            out: &super::Silent,
        },
        "claude",
        "work",
    );
    assert_eq!(config_dir(&ctx, "claude:work"), Some(work.clone()));
    assert_eq!(config_dir(&ctx, "claude"), Some(w.main()));
    assert_eq!(config_dir(&ctx, "codex:deep"), None);
    assert_eq!(engine_of("claude:work"), "claude");

    write(&w.main().join(format!("projects/p/{SID}.jsonl")), "{}\n");
    write(&work.join(format!("projects/q/{SID}.jsonl")), "{}\n");
    assert_eq!(
        locate(&ctx, "claude", SID),
        Located::Found(w.session(&w.main(), "p", SID))
    );
    assert_eq!(
        locate(&ctx, "claude:work", SID),
        Located::Found(w.session(&work, "q", SID))
    );
    assert_eq!(locate(&ctx, "claude:other", SID), Located::Missing);
    assert_eq!(locate(&ctx, "claude", "nope"), Located::Missing);
    assert_eq!(locate(&ctx, "claude", "../x"), Located::Missing);
    assert_eq!(locate(&ctx, "codex:deep", SID), Located::NotHeld);
    assert_eq!(find_everywhere(&ctx, SID).len(), 2);
}

#[test]
fn the_hash_is_stable_covers_the_sidecar_and_moves_when_a_byte_does() {
    let w = World::new();
    let p = w.session(&w.main(), "p", SID);
    assert_eq!(hash_session(&p).unwrap(), None, "no main file: missing");
    write(&p.jsonl(), "one\n");
    let bare = hash_session(&p).unwrap().unwrap();
    assert_eq!(bare.files, 1, "a missing sidecar is an empty one");
    assert_eq!(hash_session(&p).unwrap().unwrap(), bare, "twice, same hash");

    // Creation order does not matter, the sorted names do.
    write(&p.sidecar().join("b/two.txt"), "2");
    write(&p.sidecar().join("a.txt"), "1");
    let full = hash_session(&p).unwrap().unwrap();
    assert_eq!((full.files, full.size), (3, 6));
    assert_ne!(full.hash, bare.hash);
    let q = w.session(&w.root.join("elsewhere"), "other-project", SID);
    write(&q.sidecar().join("a.txt"), "1");
    write(&q.sidecar().join("b/two.txt"), "2");
    write(&q.jsonl(), "one\n");
    assert_eq!(
        hash_session(&q).unwrap().unwrap(),
        full,
        "same files, same hash, wherever they sit"
    );

    write(&p.jsonl(), "one\nmore\n");
    assert_ne!(hash_session(&p).unwrap().unwrap().hash, full.hash);
}

#[test]
fn the_rule_places_only_over_a_missing_or_unchanged_engine_copy() {
    // engine, last read, ours
    assert_eq!(decide(None, None, None), Choice::Nothing);
    assert_eq!(decide(None, Some("a"), Some("b")), Choice::Place);
    assert_eq!(decide(Some("a"), Some("a"), Some("b")), Choice::Place);
    assert_eq!(decide(Some("a"), Some("a"), Some("a")), Choice::InSync);
    assert_eq!(decide(Some("c"), Some("a"), Some("b")), Choice::Adopt);
    assert_eq!(decide(Some("c"), None, Some("b")), Choice::Adopt);
    assert_eq!(decide(Some("c"), None, None), Choice::Adopt);
    assert_eq!(decide(Some("c"), Some("c"), None), Choice::Adopt);
}

#[test]
fn a_step_places_adopts_or_leaves_the_files_alone() {
    let w = World::new();
    let ours = w.session(&w.root.join("store"), "p", SID);
    let engine = w.session(&w.main(), "p", SID);
    write(&ours.jsonl(), "ours\n");
    write(&ours.sidecar().join("t/r.txt"), "r");

    // The engine has nothing: place, sidecar included.
    let o = step(&ours, &engine, None).unwrap();
    assert_eq!(o.choice, Choice::Place);
    assert_eq!(std::fs::read_to_string(engine.jsonl()).unwrap(), "ours\n");
    assert_eq!(
        std::fs::read_to_string(engine.sidecar().join("t/r.txt")).unwrap(),
        "r"
    );
    let recorded = o.engine.unwrap().hash;
    assert_eq!(
        Some(recorded.clone()),
        hash_session(&ours).unwrap().map(|d| d.hash)
    );

    // Both the same.
    assert_eq!(
        step(&ours, &engine, Some(&recorded)).unwrap().choice,
        Choice::InSync
    );

    // The core's copy moved on, the engine's is as last read: place, and a sidecar file only the engine had is removed.
    write(&engine.sidecar().join("stale.txt"), "x");
    let recorded = hash_session(&engine).unwrap().unwrap().hash;
    write(&ours.jsonl(), "ours\nnewer\n");
    assert_eq!(
        step(&ours, &engine, Some(&recorded)).unwrap().choice,
        Choice::Place
    );
    assert_eq!(
        std::fs::read_to_string(engine.jsonl()).unwrap(),
        "ours\nnewer\n"
    );
    assert!(!engine.sidecar().join("stale.txt").exists());

    // The engine's copy was written since the last read (a transcript still being written): adopt, ours is overwritten.
    write(&engine.jsonl(), "ours\nnewer\nlive\n");
    let o = step(&ours, &engine, Some(&recorded)).unwrap();
    assert_eq!(o.choice, Choice::Adopt);
    assert_eq!(
        std::fs::read_to_string(ours.jsonl()).unwrap(),
        "ours\nnewer\nlive\n"
    );
    assert_eq!(o.engine, hash_session(&ours).unwrap());

    // Neither has one.
    let none = w.session(&w.root.join("store2"), "p", "other");
    let none_e = w.session(&w.root.join("e2"), "p", "other");
    let o = step(&none, &none_e, None).unwrap();
    assert_eq!((o.choice, o.engine), (Choice::Nothing, None));
}

#[test]
fn a_session_with_a_jsonl_but_no_sidecar_is_placed_and_adopted_whole() {
    let w = World::new();
    let ours = w.session(&w.root.join("store"), "p", SID);
    let engine = w.session(&w.main(), "p", SID);
    write(&engine.jsonl(), "only the file\n");
    assert_eq!(step(&ours, &engine, None).unwrap().choice, Choice::Adopt);
    assert!(!ours.sidecar().exists());
}

#[test]
fn answered_looks_for_the_wake_marker_in_the_conversations_transcript() {
    let w = World::new();
    let ctx = w.ctx();
    let (_t, m) = machine();
    let core = open_all(&Layout::new(&m));
    let p = w.session(&w.main(), "p", SID);
    write(
        &p.jsonl(),
        "{\"a\":1}\n{\"text\":\"[kith wake run-7] go\"}\n",
    );
    let r = FsReader {
        conn: core.conn(),
        ctx: &ctx,
    };
    // Not imported: the conversation id is the Claude session id.
    assert!(r.answered(SID, "run-7"));
    assert!(!r.answered(SID, "run-8"));
    assert!(!r.answered("not-there", "run-7"));
    assert!(!r.answered("../etc", "run-7"));
    // Imported under another account: the recorded pointer is read.
    let work = w.root.join("kith/machine/accounts/claude/work");
    let q = w.session(&work, "q", "conv-sid");
    write(&q.jsonl(), "[kith wake run-9]\n");
    core.conn()
        .execute(
            "INSERT INTO engine_sessions (conversation, account, engine, session_id, pointer, created_at) VALUES ('c1', 'claude:work', 'claude', 'conv-sid', ?1, 1)",
            [q.text()],
        )
        .unwrap();
    assert!(r.answered("c1", "run-9"));
    // The account's folder is removed: unanswered, not an error.
    std::fs::remove_dir_all(&work).unwrap();
    assert!(!r.answered("c1", "run-9"));
}

#[test]
fn the_conversation_transcript_row_reads_the_stored_sessions() {
    use kith_api::Principal;
    let w = World::new();
    let (_t, m) = machine();
    let core = open_all(&Layout::new(&m));
    let p = w.session(&w.main(), "p", SID);
    write(&p.jsonl(), "x\n");
    let d = hash_session(&p).unwrap().unwrap();
    let c = core.conn();
    c.execute(
        "INSERT INTO conversations (id, agent, engine, created_at, updated_at) VALUES ('c1', 'a', 'claude', 1, 1)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO engine_sessions (conversation, account, engine, session_id, pointer, hash, size, created_at) VALUES ('c1', 'claude', 'claude', ?1, ?2, ?3, 2, 1)",
        rusqlite::params![SID, p.text(), d.hash],
    )
    .unwrap();
    c.execute(
        "INSERT INTO engine_sessions (conversation, account, engine, session_id, created_at) VALUES ('c1', 'codex:deep', 'codex', 'k', 1)",
        [],
    )
    .unwrap();
    let v = core
        .call(
            Principal::Owner,
            "conversation.transcript",
            &json!({"id": "c1"}),
        )
        .unwrap();
    let s = v["sessions"].as_array().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(
        (
            s[0]["account"].as_str(),
            s[0]["present"].as_bool(),
            s[0]["changed"].as_bool()
        ),
        (Some("claude"), Some(true), Some(false))
    );
    assert_eq!(s[1]["held"], false);
    write(&p.jsonl(), "x\ny\n");
    let v = core
        .call(
            Principal::Owner,
            "conversation.transcript",
            &json!({"id": "c1", "account": "claude"}),
        )
        .unwrap();
    assert_eq!(v["sessions"][0]["changed"], true);
    assert!(
        core.call(
            Principal::Owner,
            "conversation.transcript",
            &json!({"id": "nope"})
        )
        .is_err()
    );
}

// ---- read parity with kernel/tests/difftest/golden/sessions.json -------------------------------------------------

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/tests/difftest/golden")
}

/// `<UUID>` for every 8-4-4-4-12 hex run, as the golden facets are masked.
fn mask_uuids(s: &str) -> String {
    let b = s.as_bytes();
    let shape = [8, 4, 4, 4, 12];
    let (mut out, mut i) = (String::new(), 0);
    'outer: while i < b.len() {
        let mut j = i;
        for (k, n) in shape.iter().enumerate() {
            if j + n > b.len() || !b[j..j + n].iter().all(u8::is_ascii_hexdigit) {
                out.push(char::from(b[i]));
                i += 1;
                continue 'outer;
            }
            j += n;
            if k < 4 {
                if b.get(j) != Some(&b'-') {
                    out.push(char::from(b[i]));
                    i += 1;
                    continue 'outer;
                }
                j += 1;
            }
        }
        out.push_str("<UUID>");
        i = j;
    }
    out
}

/// Steps of cases the core's reading cannot answer, `(case, step or None for all, why)`.
const UNMAPPED: &[(&str, Option<usize>, &str)] = &[
    (
        "sessions-resolve-free-then-busy-for-another",
        Some(1),
        "the second claimer is refused because a claim file holds the session: claims are M3-2 / M3-11",
    ),
    (
        "sessions-resolve-busy-held-by-live-writer",
        None,
        "refusal comes from a live writer's .claim file (M3-2 / M3-11)",
    ),
    (
        "sessions-resolve-busy-held-by-oneshot-and-app",
        None,
        "refusal comes from .claim files (M3-2 / M3-11)",
    ),
    (
        "sessions-resolve-taken-over-in-the-app",
        None,
        "refusal comes from the host's takeover claim (M3-2 / M3-11)",
    ),
    (
        "sessions-resolve-usage",
        None,
        "the usage text of the Python script has no core counterpart (the CLI's)",
    ),
];

const MAPPED_CASES: usize = 11;
const MAPPED_STEPS: usize = 15;

fn run_line(w: &World, conn: &Connection, line: &[String]) -> (i32, String, String) {
    let ctx = w.ctx();
    let home = w.root.join("home").to_string_lossy().replace('\\', "/");
    let arg = |s: &str| s.replace("{HOME}", &home);
    let out = |r: Result<String, String>| match r {
        Ok(s) => (0, format!("{s}\n"), String::new()),
        Err(m) => (1, String::new(), format!("kith: {m}\n")),
    };
    match line[0].as_str() {
        "latest" => out(latest(&ctx, &line[1], &arg(&line[2]))),
        "pick" => out(pick(&line[1], &recent(&ctx, &line[1], 15, ""), "")),
        "resolve" => out(resolve_read(&ctx, conn, &line[1], &line[2])
            .unwrap()
            .map(|r| r.shell())),
        other => panic!("unmapped verb {other}"),
    }
}

#[test]
fn the_core_reads_the_golden_latest_pick_and_resolve_cases_as_kith_sessions_does() {
    let corpus: Vec<Value> =
        serde_json::from_slice(&std::fs::read(golden_dir().join("sessions.json")).unwrap())
            .unwrap();
    let (mut mapped_cases, mut mapped_steps) = (0, 0);
    for case in &corpus {
        let name = case["name"].as_str().unwrap();
        if !["latest", "pick", "resolve"]
            .iter()
            .any(|v| name.starts_with(&format!("sessions-{v}")))
        {
            continue;
        }
        let w = World::new();
        let home = w.root.join("home").to_string_lossy().replace('\\', "/");
        let sub = |s: &str| {
            s.replace("{HOME}", &home).replace(
                "{KITH_HOME}",
                &w.root.join("kith").to_string_lossy().replace('\\', "/"),
            )
        };
        for st in case["setup"].as_array().unwrap() {
            if let Some(p) = st["write"].as_str() {
                write(&w.root.join(p), &sub(st["text"].as_str().unwrap()));
            } else if let Some(p) = st["mkdir"].as_str() {
                std::fs::create_dir_all(w.root.join(p)).unwrap();
            } else if let Some(p) = st["sqlite"].as_str() {
                std::fs::create_dir_all(w.root.join(p).parent().unwrap()).unwrap();
                Connection::open(w.root.join(p))
                    .unwrap()
                    .execute_batch(st["sql"].as_str().unwrap())
                    .unwrap();
            } else {
                panic!("{name}: setup step {st} is not mapped");
            }
        }
        // The registry goes in through the import, as it will for a real home.
        let (_t, m) = machine();
        let layout = Layout::new(&m);
        #[cfg(unix)]
        crate::import::transcripts::run(
            &layout,
            &w.ctx(),
            crate::import::transcripts::Options::default(),
        )
        .unwrap();
        let core = open_all(&layout);
        let facets: Value = serde_json::from_slice(
            &std::fs::read(golden_dir().join("facets").join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        let norm = |s: &str| mask_uuids(&s.replace(&home, "<HOME>"));
        let mut counted = false;
        for (i, line) in case["run"].as_array().unwrap().iter().enumerate() {
            let skip = UNMAPPED
                .iter()
                .any(|(c, s, _)| *c == name && s.is_none_or(|s| s == i));
            if skip {
                continue;
            }
            let line: Vec<String> = line
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect();
            let (code, stdout, stderr) = run_line(&w, core.conn(), &line);
            let want = |k: &str| {
                facets[format!("step{i}.{k}")]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            };
            assert_eq!(format!("{code}\n"), want("exit"), "{name} step {i} exit");
            assert_eq!(norm(&stdout), want("stdout"), "{name} step {i} stdout");
            if case["stderr"] != json!(false) {
                assert_eq!(norm(&stderr), want("stderr"), "{name} step {i} stderr");
            }
            mapped_steps += 1;
            counted = true;
        }
        mapped_cases += usize::from(counted);
    }
    // Exactly the cases and steps that map: every latest/pick/resolve line, less what UNMAPPED lists.
    assert_eq!((mapped_cases, mapped_steps), (MAPPED_CASES, MAPPED_STEPS));
}

#[cfg(unix)]
mod import {
    use super::*;
    use crate::import::transcripts::{Options, run};

    const OTHER: &str = "22222222-2222-4222-8222-222222222222";
    const GONE: &str = "33333333-3333-4333-8333-333333333333";

    fn record(w: &World, sid: &str, extra: Value) {
        let mut v =
            json!({"v": 1, "sessionId": sid, "agent": "coder", "cwd": "/work", "pid": null});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        write(
            &w.root.join(format!("home/.kith/live/terminal/{sid}.json")),
            &v.to_string(),
        );
    }

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(tree(&p));
            } else {
                out.push((p.to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn the_registry_imports_once_dry_runs_write_nothing_and_the_sources_stay_as_they_were() {
        let w = World::new();
        let ctx = w.ctx();
        let p = w.session(&w.main(), "p", SID);
        write(&p.jsonl(), "hello\n");
        write(&p.sidecar().join("x.txt"), "x");
        // A named account whose folder was removed, a record with no transcriptPath and one whose file is gone.
        record(&w, SID, json!({"transcriptPath": p.text()}));
        record(
            &w,
            OTHER,
            json!({"account": "claude:work", "transcriptPath": w.root.join("kith/machine/accounts/claude/work/projects/p/x.jsonl").to_string_lossy()}),
        );
        record(
            &w,
            GONE,
            json!({"transcriptPath": w.main().join("projects/p/gone.jsonl").to_string_lossy()}),
        );
        // Claim files and strays are not imported.
        write(
            &w.root.join(format!("home/.kith/live/terminal/{SID}.claim")),
            "{}",
        );
        write(&w.root.join("home/.kith/live/terminal/proc-1.claim"), "{}");
        write(
            &w.root.join("home/.kith/live/terminal/junk.json"),
            "not json",
        );

        let before = (tree(&w.root.join("home/.kith")), tree(&w.main()));
        let (_t, m) = machine();
        let layout = Layout::new(&m);

        let dry = run(&layout, &ctx, Options { dry_run: true }).unwrap();
        assert!(dry.changed && dry.imported == 3, "{dry:?}");
        assert!(!layout.db.exists(), "a dry run creates nothing");

        let real = run(&layout, &ctx, Options::default()).unwrap();
        assert_eq!((real.imported, real.changed), (3, true), "{real:?}");
        assert_eq!(real.defaulted, 2, "two records name no account");
        let core = open_all(&layout);
        let c = core.conn();
        let count = |t: &str| {
            c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!((count("conversations"), count("engine_sessions")), (3, 3));
        let row = |conv: &str| super::super::rows::list(c, conv).unwrap().remove(0);
        let a = row(SID);
        assert_eq!(
            (a.account.as_str(), a.engine.as_str()),
            ("claude", "claude")
        );
        assert_eq!(a.hash, hash_session(&p).unwrap().map(|d| d.hash));
        assert_eq!(a.pointer, p.text());
        assert_eq!(row(OTHER).account, "claude:work");
        assert_eq!(row(OTHER).hash, None, "its account folder is gone");
        assert_eq!(row(GONE).hash, None, "its transcript is gone");
        assert_ne!(row(GONE).pointer, "");
        drop(core);

        // Again: nothing written, no second log line.
        let again = run(&layout, &ctx, Options::default()).unwrap();
        assert!(
            !again.changed && again.imported == 0 && again.unchanged == 3,
            "{again:?}"
        );
        let again_dry = run(&layout, &ctx, Options { dry_run: true }).unwrap();
        assert!(!again_dry.changed, "{again_dry:?}");
        let core = open_all(&layout);
        let logged: i64 = core
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM import_log WHERE step = 'transcripts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logged, 1);
        assert_eq!(
            before,
            (tree(&w.root.join("home/.kith")), tree(&w.main())),
            "read-only on the sources"
        );
    }
}

#[test]
fn pick_takes_a_number_in_the_list_and_refuses_anything_else() {
    let rows: Vec<_> = ["a", "b"]
        .iter()
        .map(|s| super::read::Recent {
            session: (*s).to_owned(),
            started: None,
            via: None,
            project: None,
            text: String::new(),
        })
        .collect();
    assert_eq!(pick("coder", &rows, "2\n"), Ok("b".to_owned()));
    assert_eq!(pick("coder", &rows, " 1 "), Ok("a".to_owned()));
    assert_eq!(
        pick("coder", &rows, ""),
        Err(String::new()),
        "a cancel has no message"
    );
    for bad in ["0", "3", "x", "-1", "1.0"] {
        assert_eq!(
            pick("coder", &rows, bad),
            Err(format!("no session {bad} in the list"))
        );
    }
    assert!(
        pick("coder", &[], "1")
            .unwrap_err()
            .starts_with("no coder sessions to resume")
    );
}

#[test]
fn a_place_is_refused_when_the_engine_copy_changed_after_the_decision() {
    let w = World::new();
    let ours = w.session(&w.root.join("store"), "p", SID);
    let engine = w.session(&w.main(), "p", SID);
    write(&ours.jsonl(), "ours\nnewer\n");
    write(&engine.jsonl(), "ours\n");
    let seen_d = hash_session(&engine).unwrap().unwrap();
    let seen = seen_d.hash.clone();
    // The rule says place: the engine's copy is what was last read.
    let mine_d = hash_session(&ours).unwrap().unwrap();
    let mine = mine_d.hash.clone();
    assert_eq!(decide(Some(&seen), Some(&seen), Some(&mine)), Choice::Place);
    // A writer appends between the decision and the copy.
    write(&engine.jsonl(), "ours\nlive\n");
    let o = apply(Choice::Place, &ours, &engine, Some(&seen_d), Some(&mine_d)).unwrap();
    assert_eq!(o.choice, Choice::Adopt, "falls back to adopt");
    assert_eq!(
        std::fs::read_to_string(engine.jsonl()).unwrap(),
        "ours\nlive\n"
    );
    assert_eq!(
        std::fs::read_to_string(ours.jsonl()).unwrap(),
        "ours\nlive\n"
    );
    assert_eq!(o.engine, hash_session(&engine).unwrap());
}

#[test]
fn a_writer_appending_after_a_step_makes_the_next_decision_an_adopt() {
    let w = World::new();
    let ours = w.session(&w.root.join("store"), "p", SID);
    let engine = w.session(&w.main(), "p", SID);
    write(&ours.jsonl(), "ours\n");

    // Place: what is recorded is the core's own copy's hash, not a fresh look at the engine's folder.
    let o = step(&ours, &engine, None).unwrap();
    assert_eq!(o.choice, Choice::Place);
    assert_eq!(o.engine, hash_session(&ours).unwrap());
    let recorded = o.engine.unwrap().hash;
    write(&engine.jsonl(), "ours\nlive\n");
    assert_eq!(
        step(&ours, &engine, Some(&recorded)).unwrap().choice,
        Choice::Adopt
    );
    assert_eq!(
        std::fs::read_to_string(ours.jsonl()).unwrap(),
        "ours\nlive\n"
    );

    // InSync: the hash recorded is the one the decision saw, so an append right after is not recorded as read.
    let seen = hash_session(&engine).unwrap().unwrap();
    let o = apply(
        Choice::InSync,
        &ours,
        &engine,
        Some(&seen),
        hash_session(&ours).unwrap().as_ref(),
    )
    .unwrap();
    write(&engine.jsonl(), "ours\nlive\nmore\n");
    assert_eq!(o.engine.as_ref(), Some(&seen));
    let recorded = o.engine.unwrap().hash;
    assert_eq!(
        step(&ours, &engine, Some(&recorded)).unwrap().choice,
        Choice::Adopt
    );

    // Nothing: no copy on either side is recorded as none.
    let a = w.session(&w.root.join("s3"), "p", "none");
    let b = w.session(&w.root.join("e3"), "p", "none");
    let o = apply(Choice::Nothing, &a, &b, None, None).unwrap();
    assert_eq!(o.engine, None);
}

#[test]
fn a_copy_in_progress_is_not_part_of_the_hash() {
    let w = World::new();
    let p = w.session(&w.main(), "p", SID);
    write(&p.jsonl(), "x\n");
    write(&p.sidecar().join("a.txt"), "1");
    let before = hash_session(&p).unwrap().unwrap();
    write(&p.sidecar().join("a.txt.kith-tmp"), "half");
    assert_eq!(hash_session(&p).unwrap().unwrap(), before);
}

#[test]
fn the_main_login_follows_claude_config_dir_unless_it_is_an_accounts_own_folder() {
    let w = World::new();
    let elsewhere = w.root.join("elsewhere");
    let env_main = elsewhere.to_string_lossy().into_owned();
    let work = w
        .root
        .join("kith/machine/accounts/claude/work")
        .to_string_lossy()
        .into_owned();
    let make = |val: String| {
        let env = move |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| val.clone());
        let env: &'static dyn Fn(&str) -> Option<String> = Box::leak(Box::new(env));
        Ctx {
            state: None,
            machine: Some(w.root.join("kith/machine")),
            home: w.root.join("home"),
            env,
        }
    };
    assert_eq!(config_dir(&make(env_main), "claude"), Some(elsewhere));
    // A run on the work account has its folder in the variable; that is not the main login.
    assert_eq!(config_dir(&make(work), "claude"), Some(w.main()));
}
