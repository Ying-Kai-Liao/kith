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
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.items.borrow().get(name).cloned())
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
        "grants": {"house": ["github/work"]},
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
    );
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
        ["authority"]
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
    let db = Connection::open_in_memory().unwrap();
    let a = reg[0].fingerprint(&r.env, &db).unwrap();
    assert!(a.contains("novault"), "{a}");
    let serve = r.dir.path().join("serve");
    std::fs::create_dir_all(&serve).unwrap();
    let s = Connection::open(serve.join("serve.db")).unwrap();
    s.execute_batch(
        "CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT); INSERT INTO kv VALUES ('vault:v1:a', 'x');",
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
