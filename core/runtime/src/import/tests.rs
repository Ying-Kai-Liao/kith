//! The authority import (M2-8) against fixture auth.json files: a real-shaped one, revoked people, a bot whose owner is gone,
//! a missing preset, an empty file, malformed ones, and the safety rules (backup, dry run, idempotence, no edits to the
//! source, rows that differ).
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use kith_kernel::secrets::{Ctx, Sandbox, ServeDir};
use rusqlite::Connection;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::items::{Item, ItemSource, SecretSink};
use super::{check, run};
use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::tests::{machine, open_all};
use crate::{AuthorityOptions, AuthorityReport};

const NOW: f64 = 1_800_000_000_000.0;

/// A Keychain with the items a test puts in it.
#[derive(Default)]
struct FakeKeychain {
    items: HashMap<(String, String), Option<String>>,
    down: Option<String>,
}

impl FakeKeychain {
    fn with(mut self, service: &str, account: &str, value: &str) -> Self {
        self.items
            .insert((service.into(), account.into()), Some(value.into()));
        self
    }
}

impl ItemSource for FakeKeychain {
    fn read(&self, service: &str, account: &str) -> Item {
        if let Some(why) = &self.down {
            return Item::Unavailable(why.clone());
        }
        match self.items.get(&(service.to_owned(), account.to_owned())) {
            Some(Some(v)) => Item::Found(Zeroizing::new(v.clone())),
            _ => Item::Missing,
        }
    }
}

/// An envelope stand-in that keeps what it is given in memory (the real envelope has its own tests and needs an OS store).
#[derive(Default)]
struct MemSink {
    records: BTreeMap<String, Vec<u8>>,
    puts: usize,
    refuse: bool,
}

impl SecretSink for MemSink {
    fn names(&mut self, _: &Connection) -> Result<BTreeSet<String>> {
        Ok(self.records.keys().cloned().collect())
    }
    fn put(&mut self, _: &Connection, name: &str, value: &[u8]) -> Result<()> {
        if self.refuse {
            return Err(Error::Io("no key store here".into()));
        }
        self.puts += 1;
        self.records.insert(name.to_owned(), value.to_vec());
        Ok(())
    }
}

struct Fixture {
    _t: tempfile::TempDir,
    root: PathBuf,
    layout: Layout,
    ctx: Ctx,
}

impl Fixture {
    fn new() -> Self {
        let (t, m) = machine();
        let root = t.path().to_path_buf();
        for a in ["house", "garden"] {
            let d = root.join("agents").join(a).join("role");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent.md"), format!("---\nname: {a}\n---\n")).unwrap();
        }
        let scratch = root.join("kernel/agents/scratch/role");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("agent.md"), "---\nname: scratch\n---\n").unwrap();
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        let ctx = Ctx {
            auth: root.join("secrets/auth.json"),
            agents: root.join("agents"),
            kernel: root.join("kernel"),
            person: None,
            now_ms: NOW,
            sandbox: Sandbox::Fixed(false),
            serve: ServeDir::Fixed(root.join("serve")),
        };
        Self {
            _t: t,
            root,
            layout: Layout::new(&m),
            ctx,
        }
    }

    fn auth(&self, text: &str) -> &Self {
        std::fs::write(&self.ctx.auth, text).unwrap();
        self
    }

    fn run(
        &self,
        opts: AuthorityOptions,
        kc: &FakeKeychain,
        sink: &mut MemSink,
    ) -> Result<AuthorityReport> {
        run(&self.layout, &self.ctx, opts, kc, sink)
    }

    fn import(&self, sink: &mut MemSink) -> Result<AuthorityReport> {
        self.run(AuthorityOptions::default(), &FakeKeychain::default(), sink)
    }

    fn snapshots(&self) -> usize {
        std::fs::read_dir(&self.layout.snapshots).map_or(0, Iterator::count)
    }

    fn count(&self, table: &str) -> i64 {
        let core = open_all(&self.layout);
        core.conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn source(&self) -> Vec<u8> {
        std::fs::read(&self.ctx.auth).unwrap()
    }
}

fn real_shaped() -> String {
    serde_json::to_string_pretty(&json!({
        "profiles": {
            "github/work": {"GITHUB_TOKEN": "keychain:kith/github/work#GITHUB_TOKEN"},
            "other/home": {"KEY": "env:SOME_VAR"}
        },
        "grants": {"house": ["github/work"]},
        "env": {"house": {"svc": {"A": "plain-value"}}},
        "once": {},
        "people": {
            "kai": {"name": "Kai", "owner": true, "lang": "en", "tz": "Australia/Sydney",
                    "handles": {"telegram": "1"}, "root": "/x"},
            "mia": {"name": "Mia", "kith": "k", "agents": {"house": 1}, "was": ["old-mia"]},
            "mia-bot": {"name": "Mia's bot", "bot_of": "mia", "provider": "telegram"}
        },
        "presets": {"member": {"mode": "default", "trusted": false, "deny": ["Bash"]}},
        "members": {
            "house": {
                "*": {"preset": "member"},
                "mia": {"preset": "member", "model": "sonnet", "helpers": ["garden"],
                        "access": {"domains": ["a.example"]}, "access_by": {"domain:a.example": "kai"}}
            }
        },
        "invites": {"abc": {"person": "mia", "expires": 4_102_444_800.0}},
        "retired": {"old-sam": {"at": 1_700_000_000_000_i64}},
        "owner_bots_budget": 3
    }))
    .unwrap()
}

#[test]
fn a_real_shaped_file_goes_in_with_a_backup_a_log_row_and_the_keychain_item() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let before = f.source();
    let kc = FakeKeychain::default().with("kith/github/work", "GITHUB_TOKEN", "tok");
    let mut sink = MemSink::default();
    let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert!(rep.changed && !rep.dry_run);
    assert!(rep.backup.as_ref().is_some_and(|b| b.is_file()));
    assert_eq!(f.count("people"), 3);
    assert_eq!(f.count("memberships"), 2);
    assert_eq!(f.count("presets"), 1);
    assert_eq!(f.count("invites"), 1);
    assert_eq!(f.count("retired"), 1);
    assert_eq!(f.count("import_log"), 1);
    assert_eq!(
        sink.records
            .get("keychain:kith/github/work#GITHUB_TOKEN")
            .map(Vec::as_slice),
        Some(b"tok".as_slice())
    );
    let said = rep.lines.join("\n");
    assert!(said.contains("left behind: profiles"), "{said}");
    assert!(said.contains("left behind: env"), "{said}");
    assert!(said.contains("person keys"), "{said}");
    assert!(
        !said.contains("tok\n") && !said.contains("plain-value"),
        "{said}"
    );
    assert_eq!(f.source(), before, "the source is never edited");
}

#[test]
fn a_second_run_changes_nothing() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let kc = FakeKeychain::default().with("kith/github/work", "GITHUB_TOKEN", "tok");
    let mut sink = MemSink::default();
    f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    let (snaps, puts, log) = (f.snapshots(), sink.puts, f.count("import_log"));
    let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert!(!rep.changed, "{:?}", rep.lines);
    assert!(rep.backup.is_none());
    assert_eq!(
        (f.snapshots(), sink.puts, f.count("import_log")),
        (snaps, puts, log)
    );
}

#[test]
fn a_dry_run_writes_nothing() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let mut sink = MemSink::default();
    // No core.db yet: the dry run does not make one.
    let rep = f
        .run(
            AuthorityOptions {
                dry_run: true,
                force: false,
            },
            &FakeKeychain::default(),
            &mut sink,
        )
        .unwrap();
    assert!(rep.dry_run && rep.changed);
    assert!(!f.layout.db.exists(), "a dry run made core.db");
    assert!(rep.lines.iter().any(|l| l.contains("would import")));
    // With a migrated, empty core.db it changes no row.
    drop(open_all(&f.layout));
    let rep = f
        .run(
            AuthorityOptions {
                dry_run: true,
                force: false,
            },
            &FakeKeychain::default(),
            &mut sink,
        )
        .unwrap();
    assert!(rep.changed);
    assert_eq!((f.count("people"), f.count("import_log")), (0, 0));
    assert_eq!((f.snapshots(), sink.puts), (0, 0));
}

#[test]
fn revoked_people_and_their_bots_import_as_cut_off() {
    let f = Fixture::new();
    f.auth(
        &json!({
            "people": {
                "kai": {"owner": true},
                "rae": {"name": "Rae", "revoked": {"at": 1_700_000_000_000_i64}},
                "rae-bot": {"bot_of": "rae"},
                "lou": {"revoked": true}
            },
            "members": {"house": {"rae": {"mode": "default"}, "rae-bot": {"mode": "default"}, "lou": {"mode": "default"}}}
        })
        .to_string(),
    );
    let mut sink = MemSink::default();
    f.import(&mut sink).unwrap();
    let core = open_all(&f.layout);
    let revoked: i64 = core
        .conn()
        .query_row("SELECT COUNT(*) FROM people WHERE revoked = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(revoked, 2);
    let at: Option<i64> = core
        .conn()
        .query_row("SELECT revoked_at FROM people WHERE id = 'rae'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(at, Some(1_700_000_000_000));
}

#[test]
fn a_bot_whose_owner_is_gone_imports_as_it_was() {
    let f = Fixture::new();
    f.auth(
        &json!({
            "people": {"kai": {"owner": true}, "orphan-bot": {"bot_of": "nobody"}},
            "members": {"house": {"orphan-bot": {"mode": "default"}}}
        })
        .to_string(),
    );
    let mut sink = MemSink::default();
    let rep = f.import(&mut sink).unwrap();
    assert!(rep.changed);
    assert_eq!(f.count("people"), 2);
}

#[test]
fn a_membership_naming_a_missing_preset_imports_and_refuses_the_same_way() {
    let f = Fixture::new();
    f.auth(
        &json!({
            "people": {"kai": {"owner": true}, "mia": {"name": "Mia"}},
            "members": {"house": {"mia": {"preset": "gone"}}}
        })
        .to_string(),
    );
    let mut sink = MemSink::default();
    let rep = f.import(&mut sink).unwrap();
    assert!(rep.changed, "{:?}", rep.lines);
    assert_eq!((f.count("memberships"), f.count("presets")), (1, 0));
}

#[test]
fn an_empty_file_and_a_missing_file_import_nothing() {
    let f = Fixture::new();
    let mut sink = MemSink::default();
    let rep = f.import(&mut sink).unwrap();
    assert!(!rep.changed);
    assert!(rep.lines[0].contains("no auth.json"), "{:?}", rep.lines);
    assert!(
        !f.layout.db.exists(),
        "nothing to import must not make core.db"
    );
    f.auth("{}");
    let rep = f.import(&mut sink).unwrap();
    assert!(!rep.changed, "{:?}", rep.lines);
    assert_eq!(f.count("people"), 0);
    assert_eq!(f.count("import_log"), 0);
}

#[test]
fn a_malformed_file_is_refused_never_corrupt() {
    let f = Fixture::new();
    let bad = [
        "{not json",
        "[]",
        "\"text\"",
        r#"{"people": []}"#,
        r#"{"people": {"kai": {"name": 5}}}"#,
        r#"{"people": {"kai": {"owner": "yes"}}}"#,
        r#"{"people": {"kai": {"revoked": {"at": "soon"}}}}"#,
        r#"{"members": {"house": "x"}}"#,
        r#"{"members": {"house": {"kai": {"trusted": "maybe"}}}}"#,
        r#"{"invites": {"abc": {"expires": 1.0}}}"#,
        r#"{"agent_access": {"house": {"domains": "a.example"}}}"#,
        r#"{"agent_access": {"house": 5}}"#,
        r#"{"agent_access": {"house": {"since": []}}}"#,
        r#"{"members": {"house": {"*": {"preset": "x"}, "mia": null}}}"#,
    ];
    for text in bad {
        f.auth(text);
        let mut sink = MemSink::default();
        let e = f.import(&mut sink).unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{text}: {e:?}");
        assert!(!matches!(e, Error::Corrupt(_)));
        assert_eq!(sink.puts, 0);
        assert_eq!(f.source(), text.as_bytes(), "{text}: the source changed");
    }
    // Refused before anything was opened for writing.
    assert!(!f.layout.db.exists());
}

#[test]
fn an_agent_access_with_no_domains_is_not_a_reason_to_refuse() {
    let f = Fixture::new();
    f.auth(r#"{"people": {"kai": {"owner": true}}, "agent_access": {"house": {"domains": []}}}"#);
    let mut sink = MemSink::default();
    assert!(f.import(&mut sink).unwrap().changed);
}

#[test]
fn rows_that_differ_are_refused_unless_forced_and_a_backup_comes_first() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let mut sink = MemSink::default();
    f.import(&mut sink).unwrap();
    // The owner writes something after the import (as M2-2's writes will).
    {
        let core = open_all(&f.layout);
        core.conn()
            .execute(
                "UPDATE people SET name = 'Mia changed' WHERE id = 'mia'",
                [],
            )
            .unwrap();
    }
    let snaps = f.snapshots();
    let e = f.import(&mut sink).unwrap_err();
    assert!(
        matches!(&e, Error::Refused(m) if m.contains("--force")),
        "{e:?}"
    );
    assert_eq!(f.snapshots(), snaps, "a refusal takes no backup");
    let name: String = open_all(&f.layout)
        .conn()
        .query_row("SELECT name FROM people WHERE id = 'mia'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Mia changed", "a refusal changes nothing");

    let rep = f
        .run(
            AuthorityOptions {
                dry_run: false,
                force: true,
            },
            &FakeKeychain::default(),
            &mut sink,
        )
        .unwrap();
    assert!(rep.changed && rep.backup.is_some());
    assert_eq!(f.snapshots(), snaps + 1);
    let name: String = open_all(&f.layout)
        .conn()
        .query_row("SELECT name FROM people WHERE id = 'mia'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Mia");
}

#[test]
fn a_keychain_that_cannot_be_read_skips_the_secret_and_still_imports() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let kc = FakeKeychain {
        down: Some("the Keychain did not answer".into()),
        ..FakeKeychain::default()
    };
    let mut sink = MemSink::default();
    let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert!(rep.changed);
    assert_eq!(sink.puts, 0);
    assert!(
        rep.lines.iter().any(|l| l.starts_with("secret skipped")),
        "{:?}",
        rep.lines
    );
    assert_eq!(f.count("people"), 3);
}

#[test]
fn a_store_that_refuses_leaves_the_authority_import_standing() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let kc = FakeKeychain::default().with("kith/github/work", "GITHUB_TOKEN", "tok");
    let mut sink = MemSink {
        refuse: true,
        ..MemSink::default()
    };
    let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert!(rep.changed);
    assert!(sink.records.is_empty());
    assert!(
        rep.lines
            .iter()
            .any(|l| l.contains("secrets store unavailable")),
        "{:?}",
        rep.lines
    );
    // A later run, with the store back, adds the secret and nothing else.
    sink.refuse = false;
    let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert!(rep.changed);
    assert_eq!(sink.puts, 1);
    assert_eq!(f.count("people"), 3);
}

#[test]
fn a_record_already_in_the_envelope_is_kept_not_overwritten() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let kc = FakeKeychain::default().with("kith/github/work", "GITHUB_TOKEN", "new");
    let mut sink = MemSink::default();
    sink.records.insert(
        "keychain:kith/github/work#GITHUB_TOKEN".into(),
        b"old".to_vec(),
    );
    f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    assert_eq!(sink.puts, 0);
    assert_eq!(
        sink.records["keychain:kith/github/work#GITHUB_TOKEN"],
        b"old".to_vec()
    );
}

#[test]
fn the_same_answer_check_catches_rows_that_say_something_else() {
    let f = Fixture::new();
    let auth: Value = json!({
        "people": {"kai": {"owner": true}, "mia": {"name": "Mia"}},
        "members": {"house": {"mia": {"mode": "default"}}}
    });
    // The rows of a file where Mia holds no membership, asked against the file where she does.
    let other = json!({"people": {"kai": {"owner": true}, "mia": {"name": "Mia"}}});
    let conn = {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::apply(&mut c, crate::db::MIGRATIONS, Some("t")).unwrap();
        super::authority::insert_all(&c, &other).unwrap();
        c
    };
    let diffs = check::diffs(&auth, &conn, &f.ctx).unwrap();
    assert!(
        !diffs.is_empty(),
        "a membership the rows lack went unnoticed"
    );
    // And the same rows against the same file agree.
    let conn = {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::apply(&mut c, crate::db::MIGRATIONS, Some("t")).unwrap();
        super::authority::insert_all(&c, &auth).unwrap();
        c
    };
    assert_eq!(
        check::diffs(&auth, &conn, &f.ctx).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn the_source_files_are_left_exactly_as_they_were() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let before = f.source();
    let mtime = std::fs::metadata(&f.ctx.auth).unwrap().modified().unwrap();
    let mut sink = MemSink::default();
    f.import(&mut sink).unwrap();
    f.import(&mut sink).unwrap();
    assert_eq!(f.source(), before);
    assert_eq!(
        std::fs::metadata(&f.ctx.auth).unwrap().modified().unwrap(),
        mtime
    );
    let names: Vec<_> = std::fs::read_dir(f.root.join("secrets"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "something else appeared beside auth.json: {names:?}"
    );
}

#[test]
fn rows_that_differ_only_in_the_order_of_people_are_not_the_same() {
    let f = Fixture::new();
    f.auth(
        &json!({
            "people": {"kai": {"owner": true}, "mia": {"name": "Mia"}, "ana": {"name": "Ana"}},
        })
        .to_string(),
    );
    let mut sink = MemSink::default();
    f.import(&mut sink).unwrap();
    // The same people, kai now after the others: `owner()` and `canonical()` take the first match, so this is another answer.
    open_all(&f.layout)
        .conn()
        .execute("UPDATE people SET seq = seq + 100 WHERE id = 'kai'", [])
        .unwrap();
    let e = f.import(&mut sink).unwrap_err();
    assert!(
        matches!(&e, Error::Refused(m) if m.contains("--force")),
        "{e:?}"
    );
    let rep = f
        .run(
            AuthorityOptions {
                dry_run: true,
                force: false,
            },
            &FakeKeychain::default(),
            &mut sink,
        )
        .unwrap_err();
    assert!(matches!(rep, Error::Refused(_)));
    f.run(
        AuthorityOptions {
            dry_run: false,
            force: true,
        },
        &FakeKeychain::default(),
        &mut sink,
    )
    .unwrap();
    let first: String = open_all(&f.layout)
        .conn()
        .query_row("SELECT id FROM people ORDER BY seq LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(first, "kai");
}

#[test]
fn a_file_that_is_not_utf8_is_refused() {
    let f = Fixture::new();
    std::fs::write(&f.ctx.auth, b"{\"people\": {\"k\xff\": {}}}").unwrap();
    let mut sink = MemSink::default();
    let e = f.import(&mut sink).unwrap_err();
    assert!(
        matches!(&e, Error::Refused(m) if m.contains("UTF-8")),
        "{e:?}"
    );
}

#[test]
fn a_rerun_whose_sealing_keeps_failing_leaves_no_backup_and_no_log_row() {
    let f = Fixture::new();
    f.auth(&real_shaped());
    let kc = FakeKeychain::default().with("kith/github/work", "GITHUB_TOKEN", "tok");
    let mut sink = MemSink {
        refuse: true,
        ..MemSink::default()
    };
    f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
    let (snaps, log) = (f.snapshots(), f.count("import_log"));
    for _ in 0..2 {
        let rep = f.run(AuthorityOptions::default(), &kc, &mut sink).unwrap();
        assert!(!rep.changed && rep.backup.is_none(), "{:?}", rep.lines);
    }
    assert_eq!((f.snapshots(), f.count("import_log")), (snaps, log));
}

// --- people.listPeople against `kith-auth people --json` ------------------------------------------------------------------

/// Bots (one of a revoked person, one under an old id, a loop, one whose owner is gone), a revoked person, a person with
/// an empty handles object, a row for a kernel agent, an `*` row, and handles with the `kith` link among them.
fn people_fixture() -> String {
    serde_json::to_string_pretty(&json!({
        "people": {
            "kai": {"name": "Kai", "owner": true, "handles": {"telegram": "1", "kith": "k1"}},
            "sam": {"name": "Sam", "handles": {"slack": "U1"}, "was": ["sammy"], "provider": "telegram"},
            "sam-b": {"bot_of": "sammy", "provider": "telegram"},
            "mia": {"revoked": {"at": 1_700_000_000}, "handles": {}},
            "mia-b": {"bot_of": "mia"},
            "x1": {"bot_of": "x2"},
            "x2": {"bot_of": "x1"},
            "orphan": {"bot_of": "gone"},
            "zed": {"name": "Zed", "lang": "fr", "tz": "Europe/Paris"}
        },
        "members": {
            "house": {"sam": {"preset": "member"}, "*": {"mode": "default"}, "mia": {}},
            "garden": {"*": {}, "sam-b": {"preset": "guest"}},
            "scratch": {"sam": {}}
        }
    }))
    .unwrap()
}

/// `kith-auth people --json` over the fixture's file, or `None` where there is no python3 to run it.
fn python_people(f: &Fixture) -> Option<Value> {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
    let out = std::process::Command::new("python3")
        .arg(&script)
        .args(["people", "--json"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH")?)
        .env("HOME", &f.root)
        .env("KITH_AUTH", &f.ctx.auth)
        .env("KITH_HOME", f.root.join("kith"))
        .env("KITH_AGENTS", &f.ctx.agents)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "kith-auth people --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

fn core_people(f: &Fixture) -> Value {
    let mut core = open_all(&f.layout);
    let said =
        crate::authority::call::call_with(&mut core, &f.ctx, "people.listPeople", &json!({}))
            .unwrap();
    said["rows"].clone()
}

#[test]
fn the_cores_people_are_what_kith_auth_people_json_prints() {
    let f = Fixture::new();
    f.auth(&people_fixture());
    f.import(&mut MemSink::default()).unwrap();
    let ours = core_people(&f);
    let ids: Vec<&str> = ours
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    // the owner first, then by id with each person's bots after them, an orphan, then the loop last
    assert_eq!(
        ids,
        [
            "kai", "mia", "mia-b", "orphan", "sam", "sam-b", "zed", "x1", "x2"
        ]
    );
    let sam = &ours[4];
    assert_eq!(sam["handles"], json!({"slack": "U1"}));
    assert_eq!(
        sam["agents"],
        json!([{"agent": "garden", "role": "guest", "via": "everyone"},
               {"agent": "house", "role": "member", "via": "own"}])
    );
    assert_eq!(ours[0]["kith"], "k1");
    assert_eq!(ours[0]["handles"], json!({"telegram": "1"}));
    assert_eq!(ours[1]["revoked_at"], 1_700_000_000);
    assert_eq!(ours[2]["revoked"], true, "a bot of a revoked person");
    if let Some(theirs) = python_people(&f) {
        assert_eq!(ours, theirs);
    } else {
        // CI must not pass on the expectations alone: the parity with kith-auth is the point of the test.
        assert!(
            std::env::var_os("CI").is_none(),
            "no python3 in CI: the parity with `kith-auth people --json` was not compared"
        );
        eprintln!("no python3: compared with the expectations above only");
    }
}

#[test]
fn no_people_is_an_empty_list_and_a_database_from_before_handles_has_none() {
    let f = Fixture::new();
    f.auth("{}");
    f.import(&mut MemSink::default()).unwrap();
    assert_eq!(core_people(&f), json!([]));
    let g = Fixture::new();
    g.auth(&people_fixture());
    g.import(&mut MemSink::default()).unwrap();
    // what an import before m0015 left: the column exists (the migration ran) and is empty
    open_all(&g.layout)
        .conn()
        .execute("UPDATE people SET handles = NULL", [])
        .unwrap();
    let rows = core_people(&g);
    assert_eq!(rows[0]["kith"], "");
    assert_eq!(rows[0]["handles"], json!({}));
    assert_eq!(rows[0]["id"], "kai", "everything else is still answered");
}
