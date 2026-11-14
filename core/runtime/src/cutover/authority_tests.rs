#![allow(clippy::unwrap_used, clippy::items_after_statements)]
//! The `authority` module of the cutover framework (M2-18a), method by method and through `step` / `rollback`, on a real
//! core.db with fakes for the Keychain, the envelope and the OS store.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use kith_kernel::secrets::{Ctx, Sandbox, ServeDir};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use zeroize::Zeroizing;

use super::authority::{AuthorityModule, Vault};
use super::same_answer::{Guard, may_serve};
use super::secrets_back::OsStore;
use super::*;
use crate::import::items::{Item, ItemSource, SecretSink};
use crate::tests::machine;

struct FakeKeychain(HashMap<(String, String), String>);

impl ItemSource for FakeKeychain {
    fn read(&self, service: &str, account: &str) -> Item {
        self.0
            .get(&(service.to_owned(), account.to_owned()))
            .map_or(Item::Missing, |v| Item::Found(Zeroizing::new(v.clone())))
    }
}

type Shared<T> = Rc<RefCell<T>>;

struct MemSink(Shared<BTreeMap<String, Vec<u8>>>);

impl SecretSink for MemSink {
    fn names(&mut self, _: &Connection) -> Result<BTreeSet<String>> {
        Ok(self.0.borrow().keys().cloned().collect())
    }
    fn put(&mut self, _: &Connection, name: &str, value: &[u8]) -> Result<()> {
        self.0.borrow_mut().insert(name.to_owned(), value.to_vec());
        Ok(())
    }
}

struct MemVault(Shared<BTreeMap<String, String>>);

impl Vault for MemVault {
    fn keychain_records(&mut self, _: &Connection) -> Result<BTreeMap<String, Zeroizing<String>>> {
        Ok(self
            .0
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), Zeroizing::new(v.clone())))
            .collect())
    }
}

struct MemOs {
    items: Shared<BTreeMap<String, String>>,
    puts: Rc<Cell<usize>>,
}

impl OsStore for MemOs {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>> {
        Ok(self.items.borrow().get(name).cloned().map(Zeroizing::new))
    }
    fn put(&mut self, name: &str, value: &str) -> Result<()> {
        self.puts.set(self.puts.get() + 1);
        self.items.borrow_mut().insert(name.into(), value.into());
        Ok(())
    }
}

struct Rig {
    dir: TempDir,
    env: Env,
    /// The envelope the import sealed into.
    sealed: Shared<BTreeMap<String, Vec<u8>>>,
    /// The envelope a rollback reads from.
    vault: Shared<BTreeMap<String, String>>,
    /// The OS store a rollback writes to.
    os: Shared<BTreeMap<String, String>>,
    puts: Rc<Cell<usize>>,
}

fn original() -> Value {
    json!({
        "profiles": {"github/work": {"GITHUB_TOKEN": "keychain:kith/github/work#GITHUB_TOKEN"}},
        "grants": {"house": {"github": "github/work"}},
        "env": {"house": {"svc": {"A": "plain"}}},
        "once": {},
        "people": {
            "kai": {"name": "Kai", "owner": true, "lang": "en", "handles": {"telegram": "1"}},
            "mia": {"name": "Mia", "agents": {"house": 1}, "was": ["old-mia"]},
            "mia-bot": {"name": "Mia's bot", "bot_of": "mia", "provider": "telegram"},
            "sam": {"name": "Sam"}
        },
        "presets": {"member": {"mode": "default", "trusted": false, "deny": ["Bash"]}},
        "members": {"house": {"*": {"preset": "member"}, "mia": {"preset": "member", "model": "sonnet"}}},
        "invites": {"abc": {"person": "mia", "expires": 4_102_444_800.0}},
        "retired": {"old-sam": {"at": 1_700_000_000_000_i64}},
        "owner_bots_budget": 3
    })
}

fn rig_with(limit: Option<usize>) -> (Rig, Vec<Box<dyn Module>>) {
    let (t, m) = machine();
    let root = t.path().to_path_buf();
    std::fs::create_dir_all(&m).unwrap();
    let house = root.join("agents/house/role");
    std::fs::create_dir_all(&house).unwrap();
    std::fs::write(house.join("agent.md"), "---\nname: house\n---\n").unwrap();
    std::fs::create_dir_all(root.join("kernel/agents")).unwrap();
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    let ctx = Ctx {
        auth: root.join("secrets/auth.json"),
        agents: root.join("agents"),
        kernel: root.join("kernel"),
        person: None,
        now_ms: 1_800_000_000_000.0,
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(root.join("serve")),
    };
    std::fs::write(&ctx.auth, original().to_string()).unwrap();
    let (sealed, vault, os) = (Shared::default(), Shared::default(), Shared::default());
    let puts = Rc::new(Cell::new(0));
    let kc = FakeKeychain(
        std::iter::once((
            ("kith/github/work".to_owned(), "GITHUB_TOKEN".to_owned()),
            "tok".to_owned(),
        ))
        .collect(),
    );
    let module = AuthorityModule::with(
        Box::new(kc),
        Box::new(MemSink(sealed.clone())),
        Box::new(MemVault(vault.clone())),
        Box::new(MemOs {
            items: os.clone(),
            puts: puts.clone(),
        }),
        limit,
    )
    .waiting(std::time::Duration::from_millis(300));
    let rig = Rig {
        env: Env::new(&m).with_ctx(ctx),
        dir: t,
        sealed,
        vault,
        os,
        puts,
    };
    (rig, vec![Box::new(module)])
}

fn rig() -> (Rig, Vec<Box<dyn Module>>) {
    rig_with(None)
}

impl Rig {
    fn auth(&self) -> PathBuf {
        self.env.ctx.as_ref().unwrap().auth.clone()
    }
    fn edit(&self, f: impl FnOnce(&mut Value)) {
        let mut v: Value = serde_json::from_slice(&std::fs::read(self.auth()).unwrap()).unwrap();
        f(&mut v);
        std::fs::write(self.auth(), v.to_string()).unwrap();
    }
    fn db(&self) -> Connection {
        Connection::open(&self.env.layout.db).unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn diffs(&self, m: &dyn Module) -> Vec<String> {
        m.same_answer(&self.env, &self.db()).unwrap()
    }
}

fn opts() -> Opts {
    Opts::default()
}

fn imported() -> (Rig, Vec<Box<dyn Module>>) {
    let (r, reg) = rig();
    {
        let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
        reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
    }
    (r, reg)
}

fn said(lines: &[String], word: &str) -> bool {
    lines.iter().any(|l| l.contains(word))
}

#[test]
fn the_module_is_registered_and_names_auth_json_as_its_tombstone() {
    let reg = registry();
    assert_eq!(
        reg.iter().map(|m| m.name()).collect::<Vec<_>>(),
        [
            "accounts",
            "transcripts",
            "claims",
            "schedules",
            "authority"
        ]
    );
    let (r, reg) = rig();
    assert_eq!(reg[0].tombstones(&r.env), [r.auth()]);
}

#[test]
fn import_is_idempotent_and_a_dry_run_writes_nothing() {
    let (r, reg) = rig();
    let dry = reg[0]
        .import(
            &r.env,
            None,
            Opts {
                dry_run: true,
                force: false,
            },
        )
        .unwrap();
    assert!(said(&dry, "would import"), "{dry:?}");
    assert!(!r.env.layout.db.exists(), "a dry run made no core.db");
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
    let (people, log) = (r.count("people"), r.count("import_log"));
    assert_eq!(people, 4);
    assert_eq!(
        r.sealed
            .borrow()
            .get("keychain:kith/github/work#GITHUB_TOKEN")
            .map(Vec::as_slice),
        Some(b"tok".as_slice())
    );
    let again = reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
    assert!(said(&again, "nothing to change"), "{again:?}");
    assert_eq!((r.count("people"), r.count("import_log")), (people, log));
    assert_eq!(r.diffs(reg[0].as_ref()), Vec::<String>::new());
}

#[test]
fn same_answer_catches_a_revoke_done_only_in_auth_json() {
    let (r, reg) = imported();
    r.edit(|v| v["people"]["mia"]["revoked"] = json!(true));
    let d = r.diffs(reg[0].as_ref());
    assert!(said(&d, "person mia"), "{d:?}");
    assert!(said(&d, "cut_off"), "the answers differ too: {d:?}");
}

#[test]
fn same_answer_catches_bot_was_and_retired_differences_and_removals() {
    let (r, reg) = imported();
    let m = reg[0].as_ref();
    // A bot handed to someone else.
    r.edit(|v| v["people"]["mia-bot"]["bot_of"] = json!("sam"));
    assert!(said(&r.diffs(m), "person mia-bot"));
    r.edit(|v| v["people"]["mia-bot"]["bot_of"] = json!("mia"));
    assert_eq!(r.diffs(m), Vec::<String>::new());
    // A rename: `was` changed.
    r.edit(|v| v["people"]["mia"]["was"] = json!(["old-mia", "older"]));
    assert!(said(&r.diffs(m), "person mia"));
    r.edit(|v| v["people"]["mia"]["was"] = json!(["old-mia"]));
    // A person retired in the file only.
    r.edit(|v| v["retired"]["sam"] = json!({"at": 1}));
    assert!(said(&r.diffs(m), "retired sam"));
    r.edit(|v| {
        v["retired"].as_object_mut().unwrap().remove("sam");
    });
    assert_eq!(r.diffs(m), Vec::<String>::new());
    // A bot removed in the file leaves no row there: the core still has it, and that is a difference.
    r.edit(|v| {
        v["people"].as_object_mut().unwrap().remove("mia-bot");
    });
    let d = r.diffs(m);
    assert!(
        said(&d, "person mia-bot: in core.db, not in auth.json"),
        "{d:?}"
    );
}

#[test]
fn an_owner_who_is_also_revoked_is_flagged_by_import_and_same_answer() {
    let (r, reg) = rig();
    r.edit(|v| v["people"]["kai"]["revoked"] = json!(true));
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    let e = reg[0]
        .import(&r.env, Some(&mut core), opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("kai") && e.contains("owner and revoked"), "{e}");
    assert_eq!(r.count("people"), 0, "nothing was imported");
    let d = r.diffs(reg[0].as_ref());
    assert!(said(&d, "kai") && said(&d, "owner and revoked"), "{d:?}");
}

#[test]
fn a_changed_source_makes_the_opt_in_guard_check_again_and_the_kith_auth_side_answer() {
    let (r, reg) = imported();
    let g = Guard::default();
    let db = r.db();
    assert!(g.may_serve(reg[0].as_ref(), &r.env, &db, true).ok);
    r.edit(|v| v["people"]["mia"]["revoked"] = json!({"at": 5}));
    let s = g.may_serve(reg[0].as_ref(), &r.env, &db, true);
    assert!(!s.ok, "the file changed and the core did not");
    assert!(s.warning.unwrap().contains("difference(s)"));
    // Without the opt-in and the switch file nothing is served, quietly.
    assert!(!may_serve(reg[0].as_ref(), &r.env, &db, false).ok);
}

#[test]
fn fingerprint_follows_the_vault_rows_and_the_serve_db() {
    let (r, reg) = rig();
    drop(crate::start_with(&r.env.layout, MIGRATIONS).unwrap());
    let db = r.db();
    let a = reg[0].fingerprint(&r.env, &db).unwrap();
    assert!(a.contains("novault"), "{a}");
    let serve = r.dir.path().join("serve");
    std::fs::create_dir_all(&serve).unwrap();
    let s = Connection::open(serve.join("serve.db")).unwrap();
    s.execute_batch(
        "CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT); INSERT INTO kv VALUES ('slot', 'vault:v1:x');",
    )
    .unwrap();
    drop(s);
    let b = reg[0].fingerprint(&r.env, &db).unwrap();
    assert_ne!(a, b);
    assert!(b.contains("vault:1"), "{b}");
}

#[test]
fn a_step_switches_and_a_rollback_gives_the_same_auth_json_back() {
    let (r, reg) = rig();
    r.vault.borrow_mut().insert(
        "keychain:kith/github/work#GITHUB_TOKEN".into(),
        "tok".into(),
    );
    let before: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(tombstone::is_tombstone(&r.auth()));
    assert!(
        may_serve(reg[0].as_ref(), &r.env, &r.db(), false).ok,
        "the marker, the file and the tombstone agree"
    );
    // A later write in the core is kept by the rollback.
    r.db()
        .execute(
            "UPDATE people SET revoked = 1, revoked_at = 9 WHERE id = 'sam'",
            [],
        )
        .unwrap();
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(!tombstone::is_tombstone(&r.auth()));
    let after: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    // The left-behind tables and the person keys the core never held are back; sam's revoke is kept.
    for k in [
        "profiles",
        "grants",
        "env",
        "once",
        "presets",
        "members",
        "invites",
        "retired",
        "owner_bots_budget",
    ] {
        assert_eq!(after[k], before[k], "{k}");
    }
    assert_eq!(
        after["people"]["kai"], before["people"]["kai"],
        "handles survive"
    );
    assert_eq!(
        after["people"]["mia"], before["people"]["mia"],
        "agents and was survive"
    );
    assert_eq!(after["people"]["sam"]["revoked"]["at"], 9);
    assert_eq!(
        after["people"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        before["people"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        "file order"
    );
    assert_eq!(
        r.os.borrow()
            .get("keychain:kith/github/work#GITHUB_TOKEN")
            .map(String::as_str),
        Some("tok"),
        "the secret went back before the file"
    );
}

#[test]
fn env_is_imported_compared_and_written_back_from_the_core() {
    let (r, reg) = rig();
    r.edit(|v| {
        v["env"] = json!({"house": {"svc": {"B": "2", "A": 1}, "other": {"X": "y"}}, "gone-agent": {"svc": {"Z": "z"}}});
    });
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
    drop(core);
    assert_eq!(
        r.count("secret_env"),
        4,
        "an agent that no longer exists too"
    );
    let d = r.diffs(reg[0].as_ref());
    assert!(d.is_empty(), "same answer after the import: {d:?}");
    // a different value, a missing variable and one only the core has are all differences, named but never shown
    r.db()
        .execute_batch(
            "UPDATE secret_env SET value = '\"3\"' WHERE agent = 'house' AND service = 'svc' AND var = 'B'; \
             DELETE FROM secret_env WHERE agent = 'house' AND service = 'other'; \
             INSERT INTO secret_env VALUES ('house', 'svc', 'C', '\"c\"', 2);",
        )
        .unwrap();
    let d = r.diffs(reg[0].as_ref());
    assert!(said(&d, "env house/svc: differs in variable B"), "{d:?}");
    assert!(said(&d, "variable C (only in core.db)"), "{d:?}");
    assert!(
        said(&d, "env house/other: in auth.json, not in core.db"),
        "{d:?}"
    );
    assert!(
        !d.iter().any(|l| l.contains("\"3\"")),
        "no value is shown: {d:?}"
    );
    // the same variables in another order is a difference too (`mcp-config` prints them in the file's order)
    r.db()
        .execute_batch(
            "DELETE FROM secret_env WHERE var = 'C'; \
             UPDATE secret_env SET value = '\"2\"' WHERE var = 'B'; \
             INSERT INTO secret_env VALUES ('house', 'other', 'X', '\"y\"', 0); \
             UPDATE secret_env SET seq = 1 WHERE var = 'B'; UPDATE secret_env SET seq = 0 WHERE var = 'A';",
        )
        .unwrap();
    let d = r.diffs(reg[0].as_ref());
    assert!(
        said(&d, "env house/svc: differs in the order of the variables"),
        "{d:?}"
    );
}

#[test]
fn a_rollback_writes_the_cores_env_in_its_order_and_not_the_old_copys() {
    let (r, reg) = rig();
    r.edit(|v| {
        v["env"] = json!({"house": {"svc": {"A": "plain"}}, "gone-agent": {"svc": {"Z": "z"}}});
    });
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert_eq!(r.count("secret_env"), 2);
    r.db()
        .execute_batch(
            "DELETE FROM secret_env WHERE agent = 'gone-agent'; \
             UPDATE secret_env SET seq = 1 WHERE var = 'A'; \
             INSERT INTO secret_env VALUES ('house', 'svc', 'B', '\"2\"', 0), ('house', 'svc', 'N', '1', 2), ('house', 'other', 'X', '\"y\"', 0);",
        )
        .unwrap();
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    let after: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(
        after["env"],
        json!({"house": {"svc": {"B": "2", "A": "plain", "N": 1}, "other": {"X": "y"}}})
    );
    let order: Vec<&String> = after["env"]["house"]["svc"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(order, ["B", "A", "N"], "the core's order");
}

#[test]
fn a_file_whose_env_is_not_an_object_per_service_is_refused() {
    let (r, reg) = rig();
    r.edit(|v| v["env"] = json!({"house": {"svc": "plain"}}));
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    let e = reg[0]
        .import(&r.env, Some(&mut core), opts())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("env.house.svc") && e.contains("not an object"),
        "{e}"
    );
    assert_eq!(r.count("people"), 0, "nothing was imported");
}

#[test]
fn a_host_the_owner_stopped_keeping_stays_gone_after_a_rollback() {
    let (r, reg) = rig();
    r.edit(|v| {
        v["agent_access"] = json!({"house": {
            "domains": ["a.example.com", "b.example.com"],
            "since": {"a.example.com": 1, "b.example.com": 2},
            "by": {"a.example.com": null, "b.example.com": null}
        }});
    });
    step(&reg, &r.env, "authority", opts()).unwrap();
    {
        let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
        crate::authority::call::call_with(
            &mut core,
            r.env.ctx.as_ref().unwrap(),
            "people.agentAccess",
            &json!({"agent": "house", "domain": "a.example.com", "remove": true}),
        )
        .unwrap();
    }
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    let after: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(
        after["agent_access"]["house"]["domains"],
        json!(["b.example.com"]),
        "the removed host is not brought back from the old file"
    );
    assert!(after["agent_access"]["house"]["since"]["a.example.com"].is_null());
}

#[test]
fn handles_the_core_does_not_have_do_not_come_back_from_the_old_file() {
    // an id freed and re-added: the old person's handles must not go to the new one on a rollback
    let base =
        json!({"people": {"sam": {"name": "Sam", "handles": {"telegram": "1", "kith": "k"}}}});
    let snap = json!({"people": {"sam": {"name": "Sam"}}});
    let once = authority::merge_back(Some(base), &snap);
    assert!(once["people"]["sam"].get("handles").is_none());
    let twice = authority::merge_back(Some(once.clone()), &snap);
    assert_eq!(once, twice, "running it again gives the same file");
}

#[test]
fn handles_the_core_changed_win_over_the_old_file() {
    let base = json!({"people": {"sam": {"name": "Sam", "handles": {"telegram": "1"}}}});
    let snap = json!({"people": {"sam": {"name": "Sam", "handles": {"slack": "U2"}}}});
    let once = authority::merge_back(Some(base), &snap);
    assert_eq!(once["people"]["sam"]["handles"], json!({"slack": "U2"}));
    let twice = authority::merge_back(Some(once.clone()), &snap);
    assert_eq!(once, twice);
}

#[test]
fn import_will_not_read_a_tombstone_as_an_empty_file() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    let e = reg[0]
        .import(
            &r.env,
            Some(&mut core),
            Opts {
                dry_run: false,
                force: true,
            },
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("tombstone"), "{e}");
    assert_eq!(r.count("people"), 4);
}

#[test]
fn restore_secrets_is_idempotent_and_refuses_an_unfit_item_before_writing_any() {
    let (r, reg) = rig_with(Some(10));
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    r.vault
        .borrow_mut()
        .insert("keychain:svc#a".into(), "small".into());
    reg[0].restore_secrets(&r.env, &mut core).unwrap();
    assert_eq!(r.puts.get(), 1);
    let again = reg[0].restore_secrets(&r.env, &mut core).unwrap();
    assert_eq!(r.puts.get(), 1, "a second run writes nothing");
    assert!(said(&again, "1 already"), "{again:?}");
    assert_eq!(r.vault.borrow().len(), 1, "the envelope is never touched");
    // Too big for the store, even split: the small one is not written either.
    r.vault.borrow_mut().clear();
    r.os.borrow_mut().clear();
    r.puts.set(0);
    r.vault
        .borrow_mut()
        .insert("keychain:svc#a".into(), "small".into());
    r.vault
        .borrow_mut()
        .insert("keychain:svc#big".into(), "x".repeat(700));
    let e = reg[0]
        .restore_secrets(&r.env, &mut core)
        .unwrap_err()
        .to_string();
    assert!(e.contains("chunks"), "{e}");
    assert_eq!(r.puts.get(), 0);
    // A name `security -i` could not parse.
    r.vault.borrow_mut().clear();
    r.vault
        .borrow_mut()
        .insert("keychain:svc#a b".into(), "v".into());
    assert!(reg[0].restore_secrets(&r.env, &mut core).is_err());
    assert_eq!(r.puts.get(), 0);
}

#[test]
fn a_secret_that_cannot_go_back_stops_the_rollback_before_the_file_is_written() {
    let (r, reg) = rig_with(Some(10));
    step(&reg, &r.env, "authority", opts()).unwrap();
    r.vault
        .borrow_mut()
        .insert("keychain:svc#big".into(), "x".repeat(700));
    let e = rollback(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("chunks"), "{e}");
    assert!(tombstone::is_tombstone(&r.auth()), "still a tombstone");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::RollingBack));
    // The way out: fix the item, run it again.
    r.vault.borrow_mut().clear();
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(!tombstone::is_tombstone(&r.auth()));
}

fn phase(env: &Env, n: &str) -> Option<Phase> {
    state::read(&env.layout, n).unwrap().map(|s| s.phase)
}

#[test]
fn a_core_only_change_closes_a_cached_ok() {
    let (r, reg) = imported();
    let g = Guard::default();
    let db = r.db();
    assert!(g.may_serve(reg[0].as_ref(), &r.env, &db, true).ok);
    let before = reg[0].fingerprint(&r.env, &db).unwrap();
    // A core-side revoke: auth.json and serve.db are as they were, only core.db moved.
    r.db()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
        .unwrap();
    assert_ne!(reg[0].fingerprint(&r.env, &db).unwrap(), before);
    let s = g.may_serve(reg[0].as_ref(), &r.env, &db, true);
    assert!(!s.ok, "the cached yes must not outlive a core-only write");
    assert!(s.warning.unwrap().contains("difference(s)"));
    // Written back the way it was, the answer is the file's again.
    r.db()
        .execute("UPDATE people SET revoked = 0 WHERE id = 'sam'", [])
        .unwrap();
    assert!(g.may_serve(reg[0].as_ref(), &r.env, &db, true).ok);
}

/// A keychain that rewrites auth.json when it is read, which happens in the middle of the import.
struct Touching(PathBuf);

impl ItemSource for Touching {
    fn read(&self, _: &str, _: &str) -> Item {
        let mut text = std::fs::read_to_string(&self.0).unwrap();
        text.push('\n');
        std::fs::write(&self.0, text).unwrap();
        Item::Found(Zeroizing::new("tok".into()))
    }
}

#[test]
fn a_failed_force_import_leaves_the_old_rows() {
    let (r, _reg) = imported();
    r.db()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
        .unwrap();
    let log = r.count("import_log");
    let module = AuthorityModule::with(
        Box::new(Touching(r.auth())),
        Box::new(MemSink(Shared::default())),
        Box::new(MemVault(Shared::default())),
        Box::new(MemOs {
            items: Shared::default(),
            puts: Rc::new(Cell::new(0)),
        }),
        None,
    );
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    let forced = Opts {
        force: true,
        ..Opts::default()
    };
    let e = module
        .import(&r.env, Some(&mut core), forced)
        .unwrap_err()
        .to_string();
    assert!(e.contains("changed while the import ran"), "{e}");
    drop(core);
    let revoked: i64 = r
        .db()
        .query_row("SELECT revoked FROM people WHERE id = 'sam'", [], |x| {
            x.get(0)
        })
        .unwrap();
    assert_eq!(
        revoked, 1,
        "the rows the force would have replaced are still there"
    );
    assert_eq!(r.count("people"), 4);
    assert_eq!(
        r.count("import_log"),
        log,
        "no log row for an import that did not happen"
    );
}

#[test]
fn the_import_runs_under_the_lock_the_framework_holds_and_does_not_take_it_again() {
    let (r, reg) = rig();
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    // The lifetime lock is exclusive (flock): a second taker is refused while `core` lives.
    assert!(crate::start_with(&r.env.layout, MIGRATIONS).is_err());
    reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
    assert_eq!(
        r.count("people"),
        4,
        "it wrote through the core it was given"
    );
    assert!(
        crate::start_with(&r.env.layout, MIGRATIONS).is_err(),
        "and the lock is still ours afterwards"
    );
}

#[test]
fn a_rollback_names_an_item_the_keychain_no_longer_holds() {
    let (r, reg) = rig_with(None);
    step(&reg, &r.env, "authority", opts()).unwrap();
    r.vault
        .borrow_mut()
        .insert("keychain:svc#gone".into(), "s3cr3t-val".into());
    let lines = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(
        said(&lines, "secret restored: keychain:svc#gone"),
        "{lines:?}"
    );
    assert_eq!(r.os.borrow()["keychain:svc#gone"], "s3cr3t-val");
    assert!(
        !lines.iter().any(|l| l.contains("s3cr3t-val")),
        "names only"
    );
}

#[test]
fn a_rollback_writes_auth_json_private_and_keeps_a_newer_keychain_value() {
    let (r, reg) = rig_with(None);
    step(&reg, &r.env, "authority", opts()).unwrap();
    // The Keychain moved on after the step; the sealed copy is the older one.
    r.vault
        .borrow_mut()
        .insert("keychain:svc#a".into(), "sealed-old".into());
    r.vault
        .borrow_mut()
        .insert("keychain:svc#b".into(), "same".into());
    r.os.borrow_mut()
        .insert("keychain:svc#a".into(), "rotated-new".into());
    r.os.borrow_mut()
        .insert("keychain:svc#b".into(), "same".into());
    let lines = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert_eq!(r.os.borrow()["keychain:svc#a"], "rotated-new", "kept");
    assert!(said(&lines, "secret kept: keychain:svc#a"), "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("rotated-new") || l.contains("sealed-old")),
        "names only, never a value"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(r.auth()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(!r.auth().with_extension("restore.tmp").exists());
}

// --- M2-18 step guard: the refusal, the lock with its sha check, the vault rows ---------------------------------------

#[test]
fn a_dry_run_and_check_leave_the_file_alone() {
    let (r, reg) = rig();
    let dry = Opts {
        dry_run: true,
        ..opts()
    };
    let lines = step(&reg, &r.env, "authority", dry).expect("dry run");
    assert!(said(&lines, "nothing was changed"), "{lines:?}");
    drop(crate::start_with(&r.env.layout, MIGRATIONS).unwrap());
    let out = check(&reg, &r.env, "authority").expect("check");
    assert!(!out.is_empty(), "{out:?}");
    assert!(!tombstone::is_tombstone(&r.auth()));
}

/// Wraps a module and writes to auth.json between the import and the tombstone (inside `same_answer`).
struct WriteInBetween(Box<dyn Module>, PathBuf);

impl Module for WriteInBetween {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        self.0.tombstones(env)
    }
    fn import(&self, env: &Env, core: Option<&mut Core>, o: Opts) -> Result<Vec<String>> {
        self.0.import(env, core, o)
    }
    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        let out = self.0.same_answer(env, db);
        let mut v: Value = serde_json::from_slice(&std::fs::read(&self.1).unwrap()).unwrap();
        v["people"]["sam"]["revoked"] = json!({"at": 1});
        std::fs::write(&self.1, v.to_string()).unwrap();
        out
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        self.0.export_back(env, core)
    }
    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        self.0.source_lock(env)
    }
    fn before_tombstone(&self, env: &Env) -> Result<()> {
        self.0.before_tombstone(env)
    }
    fn digest(&self, env: &Env, db: &Connection) -> Result<String> {
        self.0.digest(env, db)
    }
}

#[test]
fn a_write_between_the_import_and_the_tombstone_refuses_and_leaves_auth_json() {
    let (r, mut reg) = rig();
    let inner = reg.remove(0);
    let reg: Vec<Box<dyn Module>> = vec![Box::new(WriteInBetween(inner, r.auth()))];
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("auth.json changed during the step"), "{e}");
    assert!(
        !tombstone::is_tombstone(&r.auth()),
        "auth.json is still the file"
    );
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(
        v["people"]["sam"]["revoked"]["at"], 1,
        "the late write is kept"
    );
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Stepping));
}

#[test]
fn the_step_holds_the_writers_lock_from_the_import_through_the_tombstone() {
    use fs2::FileExt as _;
    let (r, reg) = rig();
    let lock = r.auth().with_file_name("auth.json.lock");
    {
        let held = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&lock)
            .unwrap();
        held.lock_exclusive().unwrap();
        // A writer holds it: the step waits a bounded time, then refuses and changes nothing.
        let e = step(&reg, &r.env, "authority", opts())
            .unwrap_err()
            .to_string();
        assert!(e.contains("auth.json.lock is held"), "{e}");
        assert!(!tombstone::is_tombstone(&r.auth()));
        assert!(phase(&r.env, "authority").is_none());
    }
    step(&reg, &r.env, "authority", opts()).expect("step");
    assert!(tombstone::is_tombstone(&r.auth()));
    let f = std::fs::File::open(&lock).unwrap();
    f.try_lock_exclusive().expect("the step let the lock go");
}

/// Wraps a module and, at each point between the import and the tombstone, tries the writers' lock from a second open
/// file: it must be contended all along.
struct ProbeLock(Box<dyn Module>, PathBuf);

impl ProbeLock {
    fn held(&self) {
        use fs2::FileExt as _;
        let f = std::fs::File::open(self.1.with_file_name("auth.json.lock")).unwrap();
        assert!(f.try_lock_exclusive().is_err(), "the lock was let go");
    }
}

impl Module for ProbeLock {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        self.0.tombstones(env)
    }
    fn import(&self, env: &Env, core: Option<&mut Core>, o: Opts) -> Result<Vec<String>> {
        let out = self.0.import(env, core, o);
        self.held();
        out
    }
    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        self.held();
        self.0.same_answer(env, db)
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        self.held();
        self.0.export_back(env, core)
    }
    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        self.0.source_lock(env)
    }
    fn rollback_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        self.0.rollback_lock(env)
    }
    fn before_tombstone(&self, env: &Env) -> Result<()> {
        self.held();
        self.0.before_tombstone(env)
    }
    fn digest(&self, env: &Env, db: &Connection) -> Result<String> {
        self.0.digest(env, db)
    }
}

#[test]
fn the_lock_is_held_throughout_the_step_and_the_rollback() {
    let (r, mut reg) = rig();
    let inner = reg.remove(0);
    let reg: Vec<Box<dyn Module>> = vec![Box::new(ProbeLock(inner, r.auth()))];
    step(&reg, &r.env, "authority", opts()).expect("step");
    rollback(&reg, &r.env, "authority", opts()).expect("rollback");
}

#[test]
fn a_step_that_recorded_no_sha_does_not_tombstone() {
    let (r, reg) = rig();
    let e = reg[0].before_tombstone(&r.env).unwrap_err().to_string();
    assert!(e.contains("did not record auth.json's sha"), "{e}");
}

#[test]
fn export_back_waits_for_the_lock_and_refuses_when_it_is_not_let_go() {
    use fs2::FileExt as _;
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    let lock = r.auth().with_file_name("auth.json.lock");
    let held = std::fs::OpenOptions::new()
        .append(true)
        .open(&lock)
        .unwrap();
    held.lock_exclusive().unwrap();
    let e = rollback(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("auth.json.lock is held"), "{e}");
    assert!(
        tombstone::is_tombstone(&r.auth()),
        "the file was not written"
    );
    // The lock is taken before the rolling-back record, so the refusal wrote nothing.
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    // A holder that lets go within the wait: the rollback goes through.
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held);
    });
    rollback(&reg, &r.env, "authority", opts()).expect("rollback after the lock was let go");
    t.join().unwrap();
    assert!(!tombstone::is_tombstone(&r.auth()));
}

fn serve_with(r: &Rig, rows: &[(&str, &str)]) {
    let serve = r.dir.path().join("serve");
    std::fs::create_dir_all(&serve).unwrap();
    let s = Connection::open(serve.join("serve.db")).unwrap();
    s.execute_batch("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT)")
        .unwrap();
    for (k, v) in rows {
        s.execute("INSERT OR REPLACE INTO kv VALUES (?1, ?2)", [k, v])
            .unwrap();
    }
}

#[test]
fn unmoved_vault_rows_make_step_and_check_refuse_until_the_slots_are_sealed() {
    let (r, reg) = rig();
    drop(crate::start_with(&r.env.layout, MIGRATIONS).unwrap());
    // A plaintext row is not an old record; the two `vault:v1:` ones are.
    serve_with(
        &r,
        &[
            ("router.key", "vault:v1:AAAA"),
            ("slack.token", "vault:v1:BBBB"),
            ("setting", "plain"),
        ],
    );
    let before = std::fs::read(r.auth()).unwrap();
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("2 credential record(s)")
            && e.contains("router.key, slack.token")
            && e.contains("unlock the vault in the app first")
            && e.contains("remove that credential in the app"),
        "{e}"
    );
    let e = check(&reg, &r.env, "authority").unwrap_err().to_string();
    assert!(e.contains("unlock the vault in the app first"), "{e}");
    assert_eq!(std::fs::read(r.auth()).unwrap(), before);
    assert!(
        phase(&r.env, "authority").is_none(),
        "refused before any record"
    );
    // The host moves the slots at its unlock and leaves the old rows: the envelope has them, so they no longer count.
    for name in ["router.key", "slack.token"] {
        r.db()
            .execute(
                "INSERT INTO secret_records (name, key_id, nonce, ciphertext, created_at, updated_at) VALUES (?1, 'k', zeroblob(24), x'00', 0, 0)",
                [name],
            )
            .unwrap();
    }
    step(&reg, &r.env, "authority", opts()).expect("step once the rows have moved");
    assert!(tombstone::is_tombstone(&r.auth()));
}

// --- profiles and grants come back from the core (m0021) ---------------------------------------------------------------------

/// Profiles and grants of every shape the import keeps: lent, a browser, a profile with no variables, `*`, the host-store
/// mark, a list of profiles, an `agent@person` grant and one for a person who has been removed.
fn lane_b() -> (Value, Value) {
    (
        json!({
            "github/work": {"GITHUB_TOKEN": "keychain:kith/github/work#GITHUB_TOKEN", "person": "mia", "with": ["sam"]},
            "github/bare": {},
            "chrome-agent/main": {"browser": "chrome", "person": "*"},
            "other/env": {"KEY": "env:SOME_VAR", "person": "kai"},
            "host/svc": {"store": "host"}
        }),
        json!({
            "house": {"github": "github/work", "multi": ["other/env", "github/work"]},
            "house@mia": {"github": "github/bare"},
            "house@gone": {"github": "github/work"}
        }),
    )
}

fn rolled_back(r: &Rig, reg: &[Box<dyn Module>]) -> Value {
    rollback(reg, &r.env, "authority", opts()).unwrap();
    serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap()
}

#[test]
fn profiles_and_grants_survive_a_step_and_a_rollback_exactly() {
    let (r, reg) = rig();
    let (profiles, grants) = lane_b();
    r.edit(|v| {
        v["profiles"] = profiles.clone();
        v["grants"] = grants.clone();
    });
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert_eq!(
        (r.count("secret_profiles"), r.count("secret_grants")),
        (5, 5)
    );
    let after = rolled_back(&r, &reg);
    assert_eq!(after["profiles"], profiles);
    assert_eq!(after["grants"], grants);
}

#[test]
fn a_grant_or_profile_removed_in_the_core_stays_removed_and_one_added_there_is_written() {
    let (r, reg) = rig();
    let (profiles, grants) = lane_b();
    r.edit(|v| {
        v["profiles"] = profiles;
        v["grants"] = grants;
    });
    step(&reg, &r.env, "authority", opts()).unwrap();
    r.db()
        .execute_batch(
            "DELETE FROM secret_grants WHERE agent = 'house@gone'; \
             DELETE FROM secret_grants WHERE agent = 'house' AND service = 'multi' AND profile = 'other/env'; \
             DELETE FROM secret_profiles WHERE profile = 'github/bare'; \
             DELETE FROM secret_vars WHERE profile = 'other/env'; \
             INSERT INTO secret_profiles (profile, person, lent) VALUES ('slack/new', 'sam', '[\"mia\"]'); \
             INSERT INTO secret_vars (profile, var, source) VALUES ('slack/new', 'TOKEN', 'keychain:kith/slack/new#TOKEN'); \
             INSERT INTO secret_grants (agent, service, profile, seq) VALUES ('house@sam', 'slack', 'slack/new', 0);",
        )
        .unwrap();
    let after = rolled_back(&r, &reg);
    assert!(
        after["grants"].get("house@gone").is_none(),
        "{}",
        after["grants"]
    );
    assert_eq!(
        after["grants"]["house"]["multi"], "github/work",
        "a list of one reads back as it"
    );
    assert_eq!(after["grants"]["house@sam"]["slack"], "slack/new");
    assert!(after["profiles"].get("github/bare").is_none());
    assert_eq!(
        after["profiles"]["other/env"],
        json!({"person": "kai"}),
        "its variable is gone too"
    );
    assert_eq!(
        after["profiles"]["slack/new"],
        json!({"person": "sam", "with": ["mia"], "TOKEN": "keychain:kith/slack/new#TOKEN"})
    );
    // and a second rollback over the file it wrote gives the same file
    let again = {
        let (base, snap) = (
            Some(after.clone()),
            crate::authority::snapshot(&r.db()).unwrap(),
        );
        authority::merge_back(base, &snap)
    };
    assert_eq!(again, after);
}

#[test]
fn the_cores_profiles_win_in_merge_back_and_the_two_stand_ins_are_written_as_the_file_says_them() {
    let base = json!({
        "profiles": {
            "chrome-agent/team": {"person": "*", "browser": "chrome"},
            "chrome-agent/main": {"browser": "chrome"},
            "github/old": {"K": "env:K"}
        },
        "grants": {"house": {"github": "github/old"}}
    });
    // the core: `*` is NULL, an owner not yet named is `the owner`, github/old is gone, nothing granted
    let snap = json!({
        "profiles": {
            "chrome-agent/team": {"browser": "chrome"},
            "chrome-agent/main": {"person": "the owner", "browser": "chrome"},
            "chrome-agent/kai": {"person": "kai"}
        }
    });
    let out = authority::merge_back(Some(base), &snap);
    assert_eq!(
        out["profiles"]["chrome-agent/team"],
        json!({"person": "*", "browser": "chrome"})
    );
    assert_eq!(
        out["profiles"]["chrome-agent/main"],
        json!({"browser": "chrome"})
    );
    assert_eq!(
        out["profiles"]["chrome-agent/kai"],
        json!({"person": "kai"})
    );
    assert!(out["profiles"].get("github/old").is_none());
    assert_eq!(out["grants"], json!({}), "the core holds no grant");
}

#[test]
fn a_core_imported_before_the_tables_were_filled_keeps_the_old_copys_profiles_and_grants() {
    let base = json!({"profiles": {"github/work": {"person": "mia"}}, "grants": {"house": {"github": "github/work"}}});
    let snap = json!({"people": {}, "secrets_stale": true});
    let out = authority::merge_back(Some(base.clone()), &snap);
    assert_eq!(out["profiles"], base["profiles"]);
    assert_eq!(out["grants"], base["grants"]);
    assert!(out.get("secrets_stale").is_none());
    // with no copy at all they are written empty
    let none = authority::merge_back(None, &snap);
    assert_eq!(
        (none["profiles"].clone(), none["grants"].clone()),
        (json!({}), json!({}))
    );
}

#[test]
fn the_fingerprint_moves_when_a_grant_changes_in_the_core() {
    let (r, reg) = imported();
    let db = r.db();
    let before = reg[0].fingerprint(&r.env, &db).unwrap();
    db.execute(
        "INSERT INTO secret_grants (agent, service, profile, seq) VALUES ('house', 'x', 'x/y', 0)",
        [],
    )
    .unwrap();
    assert_ne!(before, reg[0].fingerprint(&r.env, &db).unwrap());
}

#[test]
fn the_same_answer_names_a_profile_that_differs_between_the_file_and_the_core() {
    let (r, reg) = imported();
    assert!(
        r.diffs(reg[0].as_ref()).is_empty(),
        "{:?}",
        r.diffs(reg[0].as_ref())
    );
    r.db()
        .execute("DELETE FROM secret_grants WHERE agent = 'house'", [])
        .unwrap();
    let said = r.diffs(reg[0].as_ref()).join("\n");
    assert!(
        said.contains("grants house: in auth.json, not in core.db"),
        "{said}"
    );
}

#[test]
fn a_list_of_one_stays_a_list_and_a_derived_owner_is_left_unsaid() {
    let base = json!({
        "profiles": {"chrome-agent/x": {}, "chrome-agent/y": {"person": "kai"}},
        "grants": {"house": {"github": ["github/a"], "slack": "slack/a"}}
    });
    let snap = json!({
        "people": {"kai": {"owner": true}},
        "profiles": {"chrome-agent/x": {"person": "kai"}, "chrome-agent/y": {"person": "kai"}},
        "grants": {"house": {"github": "github/a", "slack": "slack/a", "other": "o/a"}}
    });
    let out = authority::merge_back(Some(base), &snap);
    assert_eq!(out["grants"]["house"]["github"], json!(["github/a"]));
    assert_eq!(out["grants"]["house"]["slack"], "slack/a");
    assert_eq!(out["grants"]["house"]["other"], "o/a");
    assert_eq!(
        out["profiles"]["chrome-agent/x"],
        json!({}),
        "the old copy named no one"
    );
    assert_eq!(
        out["profiles"]["chrome-agent/y"],
        json!({"person": "kai"}),
        "it did name the owner"
    );
}

/// A step that died after the switched record and before the tombstone: the tombstone's temporary file cannot be written.
/// The directory is the crash; removing it is the restart.
fn crash_before_tombstone(r: &Rig, reg: &[Box<dyn Module>]) -> PathBuf {
    let blocker = r.auth().with_extension("tombstone.tmp");
    std::fs::create_dir(&blocker).unwrap();
    step(reg, &r.env, "authority", opts()).unwrap_err();
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(
        !tombstone::is_tombstone(&r.auth()),
        "the file is still real"
    );
    blocker
}

fn revoked_in_file(r: &Rig, id: &str) {
    r.edit(|v| v["people"][id]["revoked"] = json!({"at": 7}));
}

fn revoked_in_core(r: &Rig, id: &str) -> bool {
    r.db()
        .query_row("SELECT revoked FROM people WHERE id = ?1", [id], |x| {
            x.get::<_, i64>(0)
        })
        .unwrap()
        == 1
}

#[test]
fn a_crash_after_the_switched_record_does_not_lose_a_kith_auth_revoke() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // kith-auth still writes the real file (the host is unswitched); its mirror to the core failed.
    revoked_in_file(&r, "sam");
    assert!(!revoked_in_core(&r, "sam"));
    std::fs::remove_dir(blocker).unwrap();
    // The import refuses to overwrite the core's older rows without --force: nothing is tombstoned, the revoke is safe
    // in the file, and the module is stepping again.
    assert!(step(&reg, &r.env, "authority", opts()).is_err());
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Stepping));
    assert!(!tombstone::is_tombstone(&r.auth()));
    let force = Opts {
        force: true,
        ..opts()
    };
    let out = step(&reg, &r.env, "authority", force).unwrap();
    assert!(!said(&out, "already switched"), "{out:?}");
    assert!(revoked_in_core(&r, "sam"), "the revoke was imported");
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    // Run again: now it is finished, and says so.
    let out = step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "already switched"), "{out:?}");
}

#[test]
fn a_rollback_of_a_half_step_leaves_the_real_file_alone() {
    let (r, reg) = rig();
    let _blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    let out = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "left as they are"), "{out:?}");
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(v["people"]["sam"]["revoked"]["at"], 7, "the revoke stays");
    assert_eq!(phase(&r.env, "authority"), None);
    assert!(marker::read(&r.db(), "authority").is_none());
}

#[test]
fn a_rollback_that_crashed_after_the_rename_does_not_undo_a_later_revoke() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    let at = state::read(&r.env.layout, "authority").unwrap().unwrap().at;
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    // The crash: the file is renamed, the record and the marker are still there.
    state::write(
        &r.env.layout,
        "authority",
        &Switch {
            phase: Phase::RollingBack,
            snapshot: None,
            at,
        },
    )
    .unwrap();
    {
        let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
        marker::write(&mut core, "authority", at, "", None).unwrap();
    }
    revoked_in_file(&r, "sam");
    let out = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "left as they are"), "{out:?}");
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(v["people"]["sam"]["revoked"]["at"], 7);
    assert_eq!(phase(&r.env, "authority"), None);
    assert!(marker::read(&r.db(), "authority").is_none());
}

#[test]
fn export_back_does_not_replace_a_real_file_even_when_called_on_one() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    std::fs::write(r.auth(), json!({"people": {"x": {}}}).to_string()).unwrap();
    let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
    let out = reg[0].export_back(&r.env, &mut core).unwrap();
    assert!(said(&out, "left as it is"), "{out:?}");
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert!(v["people"]["x"].is_object() && v["people"]["kai"].is_null());
}

/// A person removed in the core and added again while switched (a new `seq`).
fn readd_mia(r: &Rig) {
    let db = r.db();
    db.execute("DELETE FROM memberships WHERE person = 'mia'", [])
        .unwrap();
    db.execute("DELETE FROM people WHERE id = 'mia'", [])
        .unwrap();
    db.execute(
        "INSERT INTO people (id, name) VALUES ('mia', 'New Mia')",
        [],
    )
    .unwrap();
}

#[test]
fn a_reused_id_gets_none_of_the_old_rows_copy_only_keys_after_a_rollback() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    readd_mia(&r);
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert_eq!(v["people"]["mia"]["name"], "New Mia");
    assert!(
        v["people"]["mia"].get("agents").is_none(),
        "{}",
        v["people"]["mia"]
    );
    // A row that was there at the switch and never left keeps its keys.
    assert_eq!(v["people"]["mia-bot"]["bot_of"], "mia");
    assert_eq!(v["people"]["kai"]["handles"]["telegram"], "1");
}

#[test]
fn a_marker_from_before_seeds_brings_no_copy_only_keys_back() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    r.db()
        .execute("DELETE FROM meta WHERE key = 'cutover:authority:seed'", [])
        .unwrap();
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert!(v["people"]["mia"].get("agents").is_none());
    assert_eq!(
        v["people"]["mia"]["name"], "Mia",
        "the core's keys are there"
    );
}

#[test]
fn a_rollback_of_a_half_step_does_not_bring_back_a_key_the_owner_deleted() {
    let (r, reg) = rig();
    let _blocker = crash_before_tombstone(&r, &reg);
    // The envelope still seals an item the owner has deleted from the Keychain since.
    r.vault
        .borrow_mut()
        .insert("keychain:svc#gone".into(), "s3cr3t-val".into());
    let out = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "left as they are"), "{out:?}");
    assert!(r.os.borrow().is_empty(), "nothing was written back");
    assert_eq!(phase(&r.env, "authority"), None);
}

#[test]
fn a_rollback_re_run_after_the_rename_does_not_bring_back_a_key_the_owner_deleted() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    let at = state::read(&r.env.layout, "authority").unwrap().unwrap().at;
    r.vault
        .borrow_mut()
        .insert("keychain:svc#gone".into(), "s3cr3t-val".into());
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    // The owner deletes the restored key, then the crash: the file is renamed, the record and marker are still there.
    r.os.borrow_mut().clear();
    state::write(
        &r.env.layout,
        "authority",
        &Switch {
            phase: Phase::RollingBack,
            snapshot: None,
            at,
        },
    )
    .unwrap();
    {
        let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
        marker::write(&mut core, "authority", at, "", None).unwrap();
    }
    rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(r.os.borrow().is_empty(), "{:?}", r.os.borrow());
    assert_eq!(phase(&r.env, "authority"), None);
    assert!(marker::read(&r.db(), "authority").is_none());
}

#[test]
fn a_step_over_a_missing_auth_json_is_refused_and_the_record_stays() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    std::fs::remove_file(r.auth()).unwrap();
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("missing"), "{e}");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(marker::read(&r.db(), "authority").is_some());
    assert!(!r.auth().exists(), "nothing was written");
}

#[test]
fn a_step_over_an_old_auth_json_put_back_after_a_finished_switch_is_refused() {
    let (r, reg) = rig();
    step(&reg, &r.env, "authority", opts()).unwrap();
    let kept = std::fs::read_dir(r.auth().parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().contains(".pre-cutover-"))
        .expect("the tombstone kept a copy");
    // An old file that differs from the kept copy (one that is the copy is the half-step, see below).
    let mut old = std::fs::read(&kept).unwrap();
    old.extend_from_slice(b"\n");
    std::fs::write(r.auth(), old).unwrap();
    let before = std::fs::read(r.auth()).unwrap();
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("real file"), "{e}");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(marker::read(&r.db(), "authority").is_some());
    assert_eq!(std::fs::read(r.auth()).unwrap(), before);
}

#[test]
fn the_repair_refusal_names_step_force_and_says_the_file_wins() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(e.contains("kith migrate step authority --force"), "{e}");
    assert!(e.contains("the file is newer and wins"), "{e}");
    assert!(!e.contains("core-migrate"), "{e}");
}

#[test]
fn rolling_back_a_stepping_module_left_by_a_repair_clears_its_marker() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    assert!(step(&reg, &r.env, "authority", opts()).is_err());
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Stepping));
    assert!(marker::read(&r.db(), "authority").is_some(), "S1 leaves it");
    let out = rollback(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "cancelled"), "{out:?}");
    assert_eq!(phase(&r.env, "authority"), None);
    assert!(marker::read(&r.db(), "authority").is_none());
    assert!(marker::seed(&r.db(), "authority").is_none());
}

#[test]
fn a_crash_between_the_hard_link_and_the_rename_is_still_a_half_step_and_steps_again() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // The tombstone got as far as the kept link; the rename never happened. The link is the real file itself.
    let mut name = r.auth().file_name().unwrap().to_os_string();
    name.push(".pre-cutover-9999999999");
    std::fs::hard_link(r.auth(), r.auth().with_file_name(name)).unwrap();
    std::fs::remove_dir(blocker).unwrap();
    step(&reg, &r.env, "authority", opts()).unwrap();
    // The file is exactly what the switch imported: no import, the record stays Switched, the tombstone is finished.
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
}

// ---- the preflight (M2-23 P4c) ----

fn dry() -> Opts {
    Opts {
        dry_run: true,
        ..opts()
    }
}

/// The dry run's error text (not ready) or its lines.
fn dry_text(r: &Rig, reg: &[Box<dyn Module>]) -> std::result::Result<String, String> {
    step(reg, &r.env, "authority", dry())
        .map(|l| l.join("\n"))
        .map_err(|e| e.to_string())
}

fn claim(r: &Rig, id: i64, person: &str, state: &str) {
    let db = r.db();
    db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    db.execute(
            "INSERT INTO claims (claim_id, conversation, generation, kind, state, person, created_at) VALUES (?1, ?2, 1, 'turn', ?3, ?4, 1)",
            rusqlite::params![id, format!("c{id}"), state, person],
        )
        .unwrap();
}

fn ask(r: &Rig, id: &str, person: &str, state: &str) {
    r.db()
        .execute(
            "INSERT INTO asks (id, chain, caller_person, state) VALUES (?1, ?1, ?2, ?3)",
            rusqlite::params![id, person, state],
        )
        .unwrap();
}

#[test]
fn preflight_reads_zero_on_an_imported_core_and_prints_every_item() {
    let (r, reg) = imported();
    let t = dry_text(&r, &reg).expect("ready");
    for n in 1..=6 {
        assert!(t.contains(&format!("preflight {n}.")), "{t}");
    }
    assert!(t.contains("preflight: ready"), "{t}");
    assert!(t.contains("nothing was changed"), "{t}");
    // Repeats change nothing.
    assert_eq!(dry_text(&r, &reg).unwrap(), t);
    assert!(phase(&r.env, "authority").is_none());
}

#[test]
fn preflight_with_no_core_db_counts_the_import_only() {
    let (r, reg) = rig();
    assert!(!r.env.layout.db.exists());
    let t = dry_text(&r, &reg).expect("ready");
    assert!(t.contains("no core.db yet"), "{t}");
    assert!(t.contains("preflight 1. the import refuses: 0"), "{t}");
    assert!(!r.env.layout.db.exists(), "a dry run makes no core.db");
}

#[test]
fn preflight_item_1_counts_a_refusal_of_the_dry_run_import() {
    let (r, reg) = rig();
    r.edit(|v| v["people"]["kai"]["revoked"] = json!(true));
    let e = dry_text(&r, &reg).unwrap_err();
    assert!(e.contains("preflight 1. the import refuses: 1"), "{e}");
    assert!(
        e.contains("owner and revoked") && e.contains("NOT ready"),
        "{e}"
    );
}

#[test]
fn preflight_item_2_counts_the_differences_once_the_core_holds_people() {
    let (r, reg) = imported();
    r.edit(|v| v["people"]["mia"]["revoked"] = json!({"at": 5}));
    let e = dry_text(&r, &reg).unwrap_err();
    assert!(
        e.contains("preflight 2.")
            && !e.contains("differences between the core and auth.json (check authority): 0"),
        "{e}"
    );
    assert!(e.contains("mia"), "{e}");
}

#[test]
fn preflight_item_2_is_skipped_not_passed_on_an_empty_core() {
    // A core.db that exists but holds no people yet.
    let (r, reg) = rig();
    drop(crate::start_with(&r.env.layout, MIGRATIONS).unwrap());
    let t = dry_text(&r, &reg).expect("ready");
    assert!(
        t.contains("(check authority): skipped: the core holds no people yet"),
        "{t}"
    );
    assert!(!t.contains("(check authority): 0"), "{t}");
    assert!(
        t.contains("the step will compare after its own import"),
        "{t}"
    );
    assert!(!t.contains("items 1 to 5 are zero"), "{t}");
}

#[test]
fn preflight_item_2_is_skipped_not_passed_while_item_4_is_not_zero() {
    let (r, reg) = imported();
    serve_with(&r, &[("router.key", "vault:v1:AAAA")]);
    let e = dry_text(&r, &reg).unwrap_err();
    assert!(
        e.contains("(check authority): skipped: item 4 is not zero"),
        "{e}"
    );
    assert!(!e.contains("(check authority): 0"), "{e}");
}

#[test]
fn preflight_item_3_names_an_open_removal() {
    let (r, reg) = imported();
    r.db()
        .execute(
            "INSERT INTO open_removals (person, bots, at) VALUES ('sam', '[]', 1)",
            [],
        )
        .unwrap();
    let e = dry_text(&r, &reg).unwrap_err();
    assert!(e.contains("preflight 3. open removals: 1 (sam)"), "{e}");
}

#[test]
fn preflight_item_4_names_a_vault_row_that_has_not_moved() {
    let (r, reg) = imported();
    serve_with(&r, &[("router.key", "vault:v1:AAAA")]);
    let e = dry_text(&r, &reg).unwrap_err();
    assert!(
        e.contains("have not moved into the vault: 1 (router.key)"),
        "{e}"
    );
}

#[test]
fn preflight_item_5_names_live_holders_that_are_not_people_and_skips_ended_claims() {
    let (r, reg) = imported();
    claim(&r, 1, "ghost", "running");
    claim(&r, 2, "gone", "ended");
    claim(&r, 3, "mia", "claimed");
    ask(&r, "a1", "ghost-ask", "open");
    ask(&r, "a2", "gone-ask", "answered");
    let e = dry_text(&r, &reg).unwrap_err();
    let line = e.lines().find(|l| l.starts_with("preflight 5.")).unwrap();
    assert!(
        line.contains(": 2 (") && line.contains("ghost:") && line.contains("ghost-ask:"),
        "{line}"
    );
    assert!(!line.contains("gone") && !line.contains("mia"), "{line}");
}

#[test]
fn preflight_item_6_lists_a_bots_live_claim_without_stopping() {
    let (r, reg) = imported();
    claim(&r, 1, "mia-bot", "running");
    ask(&r, "a1", "mia-bot", "open");
    claim(&r, 2, "kai", "running");
    let t = dry_text(&r, &reg).expect("a listing does not stop");
    let line = t.lines().find(|l| l.starts_with("preflight 6.")).unwrap();
    assert!(
        line.contains(": 1 (mia-bot: 1 claim(s), 1 ask(s))"),
        "{line}"
    );
    assert!(!line.contains("kai"), "{line}");
    assert!(t.contains("preflight: ready"), "{t}");
}

#[test]
fn preflight_on_a_switched_module_says_so_and_counts_nothing() {
    let (r, reg) = imported();
    claim(&r, 1, "ghost", "running");
    state::write(
        &r.env.layout,
        "authority",
        &Switch {
            phase: Phase::Switched,
            snapshot: None,
            at: 1,
        },
    )
    .unwrap();
    let t = dry_text(&r, &reg).expect("ready");
    assert!(
        t.contains("already switched") && !t.contains("preflight 5."),
        "{t}"
    );
}

#[test]
fn a_real_step_stops_on_the_preflight_before_it_writes_anything() {
    let (r, reg) = imported();
    claim(&r, 1, "ghost", "running");
    let e = step(&reg, &r.env, "authority", opts())
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("preflight not ready") && e.contains("ghost"),
        "{e}"
    );
    assert!(
        phase(&r.env, "authority").is_none(),
        "no record was written"
    );
    assert!(!tombstone::is_tombstone(&r.auth()));
}

fn kept_copy_name(r: &Rig) -> PathBuf {
    let mut name = r.auth().file_name().unwrap().to_os_string();
    name.push(".pre-cutover-9999999999");
    r.auth().with_file_name(name)
}

fn refusal(res: Result<Vec<String>>) -> String {
    match res {
        Err(Error::Refused(s)) => s,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_half_step_then_a_kith_auth_write_is_told_to_roll_back_first() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // The crash came between the hard link and the rename: the kept copy is the real file's inode.
    std::fs::hard_link(r.auth(), kept_copy_name(&r)).unwrap();
    std::fs::remove_dir(blocker).unwrap();
    // kith-auth's next write replaces the file (temp file, rename): a new inode with new bytes.
    revoked_in_file(&r, "sam");
    let mut v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    v["people"]["sam"]["revoked"] = json!({"at": 8});
    let tmp = r.auth().with_extension("tmp");
    std::fs::write(&tmp, v.to_string()).unwrap();
    std::fs::rename(&tmp, r.auth()).unwrap();
    let bytes = std::fs::read(r.auth()).unwrap();
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    let (roll, back) = (
        msg.find("kith rollback authority").expect(&msg),
        msg.find("Put the tombstone back").expect(&msg),
    );
    assert!(roll < back, "rollback comes first: {msg}");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(!tombstone::is_tombstone(&r.auth()));
    assert_eq!(std::fs::read(r.auth()).unwrap(), bytes, "file unchanged");
}

#[test]
fn a_crash_before_the_link_with_differing_rows_may_use_force() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("the file is newer and wins"), "{msg}");
    assert!(msg.contains("--force"), "{msg}");
}

#[test]
fn a_file_equal_to_a_kept_copy_finishes_the_tombstone_without_importing() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // The kept copy is the file's own bytes (the crash came after the link); the core moved on meanwhile (a revoke).
    std::fs::copy(r.auth(), kept_copy_name(&r)).unwrap();
    std::fs::remove_dir(blocker).unwrap();
    r.db()
        .execute("UPDATE people SET revoked = 1 WHERE id = 'sam'", [])
        .unwrap();
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(revoked_in_core(&r, "sam"), "the core's rows are unchanged");
}

#[test]
fn a_kith_auth_write_after_a_kept_copy_keeps_the_record_switched_and_says_rollback_first() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    std::fs::copy(r.auth(), kept_copy_name(&r)).unwrap();
    std::fs::remove_dir(blocker).unwrap();
    revoked_in_file(&r, "sam");
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("kith rollback authority"), "{msg}");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(!tombstone::is_tombstone(&r.auth()));
}

fn revoke_in_core(r: &Rig, id: &str) {
    r.db()
        .execute("UPDATE people SET revoked = 1 WHERE id = ?1", [id])
        .unwrap();
}

/// A marker from before the sha was kept.
fn drop_auth_sha(r: &Rig) {
    let seed: String = r
        .db()
        .query_row(
            "SELECT value FROM meta WHERE key = 'cutover:authority:seed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut v: Value = serde_json::from_str(&seed).unwrap();
    v.as_object_mut().unwrap().remove("auth_sha");
    r.db()
        .execute(
            "UPDATE meta SET value = ?1 WHERE key = 'cutover:authority:seed'",
            [v.to_string()],
        )
        .unwrap();
}

#[test]
fn a_repair_over_a_never_touched_file_finishes_the_tombstone_and_keeps_the_core_revoke() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // The core moved on (a revoke) while auth.json stayed the file the step imported: the core is the newer side.
    revoke_in_core(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(revoked_in_core(&r, "sam"), "the core's revoke stays");
    // Running it again is the finished switch.
    let out = step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(said(&out, "already switched"), "{out:?}");
    assert!(revoked_in_core(&r, "sam"));
}

#[test]
fn a_changed_file_over_an_unmoved_core_is_told_to_force_and_force_imports_it() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("kith migrate step authority --force"), "{msg}");
    let force = Opts {
        force: true,
        ..opts()
    };
    step(&reg, &r.env, "authority", force).unwrap();
    assert!(revoked_in_core(&r, "sam"), "the file's revoke was imported");
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
}

#[test]
fn a_changed_file_and_a_moved_core_refuse_without_force_advice_and_leave_the_record() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    revoke_in_core(&r, "mia");
    std::fs::remove_dir(blocker).unwrap();
    let bytes = std::fs::read(r.auth()).unwrap();
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("both sides changed"), "{msg}");
    assert!(msg.contains("kith migrate check authority"), "{msg}");
    assert!(!msg.contains("--force"), "{msg}");
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(!tombstone::is_tombstone(&r.auth()));
    assert_eq!(std::fs::read(r.auth()).unwrap(), bytes);
    assert!(revoked_in_core(&r, "mia"));
}

#[test]
fn an_older_marker_with_no_sha_is_told_to_force_only_while_the_core_is_unmoved() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    revoked_in_file(&r, "sam");
    drop_auth_sha(&r);
    std::fs::remove_dir(blocker).unwrap();
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("kith migrate step authority --force"), "{msg}");
    // Once the core has moved too, nothing says which side is newer.
    revoke_in_core(&r, "mia");
    let msg = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(msg.contains("compare by hand"), "{msg}");
    assert!(!msg.contains("--force"), "{msg}");
}

fn force() -> Opts {
    Opts {
        force: true,
        ..opts()
    }
}

#[test]
fn force_on_a_stepping_record_is_refused_once_the_core_has_moved() {
    for old_marker in [false, true] {
        let (r, reg) = rig();
        let blocker = crash_before_tombstone(&r, &reg);
        revoked_in_file(&r, "sam");
        if old_marker {
            drop_auth_sha(&r);
        }
        std::fs::remove_dir(blocker).unwrap();
        // The first repair sees an unmoved core: it advises --force and leaves the record Stepping.
        let msg = refusal(step(&reg, &r.env, "authority", opts()));
        assert!(msg.contains("--force"), "{msg}");
        assert_eq!(phase(&r.env, "authority"), Some(Phase::Stepping));
        // Then the core takes a revoke of its own.
        revoke_in_core(&r, "mia");
        let bytes = std::fs::read(r.auth()).unwrap();
        let msg = refusal(step(&reg, &r.env, "authority", force()));
        assert!(!msg.contains("--force"), "{msg}");
        assert!(revoked_in_core(&r, "mia"), "the core's revoke is kept");
        assert!(!revoked_in_core(&r, "sam"), "the file was not imported");
        assert_eq!(std::fs::read(r.auth()).unwrap(), bytes);
        assert!(!tombstone::is_tombstone(&r.auth()));
    }
}

#[test]
fn a_stepping_record_with_the_same_marker_and_file_finishes_the_tombstone_without_importing() {
    let (r, reg) = rig();
    let blocker = crash_before_tombstone(&r, &reg);
    // The crash came after the marker and before the switched record.
    let s = state::read(&r.env.layout, "authority").unwrap().unwrap();
    state::write(
        &r.env.layout,
        "authority",
        &Switch {
            phase: Phase::Stepping,
            ..s
        },
    )
    .unwrap();
    revoke_in_core(&r, "sam");
    std::fs::remove_dir(blocker).unwrap();
    step(&reg, &r.env, "authority", opts()).unwrap();
    assert!(tombstone::is_tombstone(&r.auth()));
    assert_eq!(phase(&r.env, "authority"), Some(Phase::Switched));
    assert!(
        revoked_in_core(&r, "sam"),
        "no import over the core's revoke"
    );
}

#[test]
fn a_rollback_writes_the_cores_helper_since_back_unless_the_database_is_from_before_it() {
    let base = json!({
        "people": {"kai": {"owner": true}, "sam": {}},
        "members": {"house": {"sam": {"helpers": ["garden"], "helper_since": {"garden": 1}}}},
    });
    let mut snap = json!({
        "people": {"kai": {"owner": true}, "sam": {}},
        "members": {"house": {"sam": {"helpers": ["garden"], "helper_since": {"garden": 2}}}},
        "presets": {}, "invites": {}, "retired": {}, "agent_access": {},
    });
    // the core's value wins
    let out = authority::merge_back(Some(base.clone()), &snap);
    assert_eq!(
        out["members"]["house"]["sam"]["helper_since"],
        json!({"garden": 2})
    );
    // a helper removed in the core does not come back from the old copy
    snap["members"]["house"]["sam"]["helpers"] = json!([]);
    snap["members"]["house"]["sam"]
        .as_object_mut()
        .unwrap()
        .remove("helper_since");
    let out = authority::merge_back(Some(base.clone()), &snap);
    assert!(out["members"]["house"]["sam"].get("helper_since").is_none());
    // a database imported before the column existed has NULLs that say nothing: the old copy's stays
    snap["helper_since_stale"] = json!(true);
    let out = authority::merge_back(Some(base), &snap);
    assert_eq!(
        out["members"]["house"]["sam"]["helper_since"],
        json!({"garden": 1})
    );
}

fn put_once(r: &Rig) {
    r.edit(|v| {
        v["once"] =
            json!({"o1": {"agent": "house", "created": 1, "expires": 4_102_444_800_000_i64}});
    });
}

#[test]
fn a_step_with_one_time_secrets_in_auth_json_is_refused_and_changes_nothing() {
    let (r, reg) = rig();
    put_once(&r);
    let before = std::fs::read(r.auth()).unwrap();
    for _ in 0..2 {
        let e = refusal(step(&reg, &r.env, "authority", opts()));
        assert!(
            e.contains("1 one-time secret(s)")
                && e.contains("wait until they are used or expire (at most an hour"),
            "{e}"
        );
    }
    assert_eq!(std::fs::read(r.auth()).unwrap(), before);
    assert!(!tombstone::is_tombstone(&r.auth()));
    assert!(
        phase(&r.env, "authority").is_none(),
        "no record was written"
    );
    // used or expired: the file has none, and the step goes through
    r.edit(|v| v["once"] = json!({}));
    step(&reg, &r.env, "authority", opts()).expect("step");
    assert!(tombstone::is_tombstone(&r.auth()));
}

#[test]
fn a_dry_run_reports_one_time_secrets_and_is_not_ready() {
    let (r, reg) = rig();
    put_once(&r);
    let dry = Opts {
        dry_run: true,
        ..opts()
    };
    let e = refusal(step(&reg, &r.env, "authority", dry));
    assert!(e.contains("preflight 7.") && e.contains("o1"), "{e}");
    assert!(e.contains("not ready"), "{e}");
}

/// Wraps a module and has a `kith-auth once put` land in auth.json after the once check (inside `same_answer`), as a writer
/// that got in between would.
struct OnceInBetween(Box<dyn Module>, PathBuf);

impl Module for OnceInBetween {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        self.0.tombstones(env)
    }
    fn import(&self, env: &Env, core: Option<&mut Core>, o: Opts) -> Result<Vec<String>> {
        self.0.import(env, core, o)
    }
    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        let out = self.0.same_answer(env, db);
        let mut v: Value = serde_json::from_slice(&std::fs::read(&self.1).unwrap()).unwrap();
        v["once"] =
            json!({"late": {"agent": "house", "created": 1, "expires": 4_102_444_800_000_i64}});
        std::fs::write(&self.1, v.to_string()).unwrap();
        out
    }
    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        self.0.export_back(env, core)
    }
    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        self.0.source_lock(env)
    }
    fn before_tombstone(&self, env: &Env) -> Result<()> {
        self.0.before_tombstone(env)
    }
    fn digest(&self, env: &Env, db: &Connection) -> Result<String> {
        self.0.digest(env, db)
    }
}

#[test]
fn a_one_time_secret_written_after_the_check_is_not_lost_to_the_tombstone() {
    let (r, mut reg) = rig();
    let inner = reg.remove(0);
    let reg: Vec<Box<dyn Module>> = vec![Box::new(OnceInBetween(inner, r.auth()))];
    let e = refusal(step(&reg, &r.env, "authority", opts()));
    assert!(e.contains("auth.json changed during the step"), "{e}");
    assert!(
        !tombstone::is_tombstone(&r.auth()),
        "auth.json is still the file"
    );
    let v: Value = serde_json::from_slice(&std::fs::read(r.auth()).unwrap()).unwrap();
    assert!(v["once"]["late"].is_object(), "the late once is kept");
}

#[test]
fn a_core_imported_before_secrets_or_helper_since_were_kept_is_told_to_import_again() {
    for (sql, word) in [
        (
            "INSERT INTO secret_stale VALUES ('before secret_grants')",
            "profiles, grants and env",
        ),
        (
            "INSERT INTO authority_settings (key, value) VALUES ('helper_since_stale', '1')",
            "a helper's `since`",
        ),
        (
            "INSERT INTO authority_settings (key, value) VALUES ('handles_stale', '1')",
            "people's handles",
        ),
    ] {
        let (r, reg) = rig();
        {
            let mut core = crate::start_with(&r.env.layout, MIGRATIONS).unwrap();
            reg[0].import(&r.env, Some(&mut core), opts()).unwrap();
        }
        r.db().execute_batch(sql).unwrap();
        let before = std::fs::read(r.auth()).unwrap();
        let e = refusal(step(&reg, &r.env, "authority", opts()));
        assert!(e.contains("import again") && e.contains(word), "{e}");
        assert_eq!(std::fs::read(r.auth()).unwrap(), before);
        assert!(!tombstone::is_tombstone(&r.auth()));
    }
}
