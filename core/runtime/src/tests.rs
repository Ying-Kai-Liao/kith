#![allow(clippy::many_single_char_names)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kith_kernel::paths::Paths;
use tempfile::TempDir;

use crate::db::{MIGRATIONS, Migration};
use crate::layout::Layout;
use crate::{
    Embedded, Error, Via, embedded_with, lock, migrate_with, perms, start_with,
    stateless_through_core,
};

const V2: Migration = Migration {
    version: 2,
    name: "extra",
    sql: "CREATE TABLE extra (x INTEGER);",
};
const BOTH: &[Migration] = &[MIGRATIONS[0], V2];

pub(crate) fn machine() -> (TempDir, PathBuf) {
    let t = tempfile::tempdir().expect("tempdir");
    let m = t.path().join("machine");
    (t, m)
}

pub(crate) fn paths(machine: &Path) -> Paths {
    let p = machine.to_path_buf();
    Paths {
        home: p.clone(),
        state: p.clone(),
        agents: p.clone(),
        home_real: p.clone(),
        state_real: p.clone(),
        agents_real: p.clone(),
        machine: p.clone(),
        secrets: p.clone(),
        logs: p.clone(),
        cache: p.clone(),
        store: p,
    }
}

pub(crate) fn open(l: &Layout) -> crate::Core {
    match embedded_with(l, MIGRATIONS).expect("embedded") {
        Embedded::Open(c) => *c,
        Embedded::DaemonRunning { .. } => panic!("lock held"),
    }
}

#[cfg(unix)]
fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).expect("stat").permissions().mode() & 0o777
}

#[test]
fn a_first_setup_creates_the_folder_the_db_and_the_marker_owner_only() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let c = open(&l);
    assert!(l.db.is_file() && l.lock.is_file() && l.marker.is_file());
    assert_eq!(
        c.schema_version(),
        MIGRATIONS.last().expect("steps").version
    );
    assert_eq!(
        l.dir,
        m.join("core"),
        "everything sits in core/ of the machine folder"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&l.dir), 0o700);
        for f in l.files().iter().filter(|f| f.exists()) {
            assert_eq!(mode(f), 0o600, "{}", f.display());
        }
        // WAL mode keeps -wal and -shm beside the database while it is open.
        assert!(
            l.files()
                .iter()
                .any(|f| f.to_string_lossy().ends_with("-wal") && f.exists())
        );
    }
    let wal: String = c
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("pragma");
    assert_eq!(wal, "wal");
}

#[test]
fn whoever_takes_the_lock_bumps_the_generation_embedded_included() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    assert_eq!(open(&l).generation(), 1);
    assert_eq!(open(&l).generation(), 2);
    assert_eq!(start_with(&l, MIGRATIONS).expect("start").generation(), 3);
    assert_eq!(open(&l).generation(), 4);
}

#[test]
fn a_second_core_in_the_same_process_finds_the_lock_held_and_does_not_wait() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let first = open(&l);
    match embedded_with(&l, MIGRATIONS).expect("second") {
        Embedded::DaemonRunning { pid } => assert_eq!(pid, Some(std::process::id())),
        Embedded::Open(_) => panic!("two cores at once"),
    }
    assert!(matches!(
        start_with(&l, MIGRATIONS),
        Err(Error::AlreadyRunning(_))
    ));
    drop(first);
    assert_eq!(
        open(&l).generation(),
        2,
        "free again, and the refused tries bumped nothing"
    );
}

/// Run by `the_lock_excludes_another_process` as a child: takes the daemon's lock, says so, and holds it until stdin closes.
#[test]
fn child_holds_the_lock() {
    let Ok(machine) = std::env::var("KITH_CORE_TEST_MACHINE") else {
        return;
    };
    let core = start_with(&Layout::new(Path::new(&machine)), MIGRATIONS).expect("child start");
    println!("locked gen={}", core.generation());
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[test]
fn the_lock_excludes_another_process_and_a_crash_frees_it() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let mut child = Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--exact",
            "tests::child_holds_the_lock",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KITH_CORE_TEST_MACHINE", &m)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut out = BufReader::new(child.stdout.take().expect("stdout"));
    let mut seen = String::new();
    loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).expect("read") > 0,
            "child ended before locking: {seen}"
        );
        seen += &line;
        if line.contains("locked gen=") {
            break;
        }
    }
    match embedded_with(&l, MIGRATIONS).expect("embedded") {
        Embedded::DaemonRunning { pid } => {
            assert_eq!(pid, Some(child.id()), "core.holder names its holder");
        }
        Embedded::Open(_) => panic!("the lock was not exclusive across processes"),
    }
    assert!(matches!(
        start_with(&l, MIGRATIONS),
        Err(Error::AlreadyRunning(_))
    ));
    // A crash (no clean unlock): the OS frees the lock, the file stays.
    child.kill().expect("kill");
    child.wait().expect("wait");
    drop(child.stdin.take());
    assert!(l.lock.is_file(), "core.lock is never deleted");
    let c = open(&l);
    assert_eq!(
        c.generation(),
        2,
        "the dead core's generation was 1; this one is newer"
    );
}

#[test]
fn core_holder_records_the_holder_pid_and_start_time_while_the_lock_is_held() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let c = open(&l);
    // Read while the lock is held: on Windows that is why the line is not in core.lock.
    let h = lock::recorded_holder(&l).expect("holder line");
    assert_eq!(h.pid, std::process::id());
    assert_eq!(
        Some(h.started),
        lock::process_start(std::process::id()).or(Some(String::new()))
    );
    drop(c);
    assert!(l.lock.is_file(), "released, not deleted");
    assert!(l.holder.is_file(), "the last holder's line stays");
}

#[test]
fn a_stale_generation_is_refused_and_claim_ids_survive_a_restart() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    let mut a = open(&l);
    {
        let tx = a.claim_tx().expect("current generation");
        assert_eq!(crate::Core::next_claim_id(&tx).expect("id"), 1);
        assert_eq!(crate::Core::next_claim_id(&tx).expect("id"), 2);
        tx.commit().expect("commit");
    }
    // A newer core takes over (here: another connection stands in for it), bumping the generation.
    let other = rusqlite::Connection::open(&l.db).expect("second connection");
    other
        .execute(
            "UPDATE counters SET value = value + 1 WHERE name = 'generation'",
            [],
        )
        .expect("bump");
    let err = a.claim_tx().expect_err("stale core must be refused");
    assert!(matches!(err, Error::StaleGeneration(_)), "{err}");
    drop(a);
    drop(other);
    let mut b = open(&l);
    let tx = b.claim_tx().expect("new core");
    assert_eq!(
        crate::Core::next_claim_id(&tx).expect("id"),
        3,
        "the counter is durable: no id is reused"
    );
}

#[test]
#[cfg(unix)]
fn looser_permissions_are_refused_with_the_fix() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(open(&l));
    chmod(&l.dir, 0o755);
    let e = start_with(&l, MIGRATIONS).err().expect("open folder");
    assert!(
        matches!(&e, Error::Permissions(t) if t.contains("chmod 700")),
        "{e}"
    );
    assert!(matches!(
        embedded_with(&l, MIGRATIONS),
        Err(Error::Permissions(_))
    ));
    chmod(&l.dir, 0o700);
    for loose in [l.db.clone(), l.lock.clone(), l.marker.clone()] {
        chmod(&loose, 0o644);
        let e = embedded_with(&l, MIGRATIONS).err().expect("open file");
        assert!(
            matches!(&e, Error::Permissions(t) if t.contains("chmod 600") && t.contains(&*loose.to_string_lossy())),
            "{e}"
        );
        chmod(&loose, 0o600);
    }
    assert!(embedded_with(&l, MIGRATIONS).is_ok(), "private again");
}

#[test]
fn a_marker_with_no_database_refuses_to_start_empty() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(open(&l));
    for f in l.files() {
        if f != l.marker && f != l.lock {
            let _ = std::fs::remove_file(f);
        }
    }
    assert!(matches!(
        start_with(&l, MIGRATIONS),
        Err(Error::DatabaseMissing(_))
    ));
    assert!(matches!(
        embedded_with(&l, MIGRATIONS),
        Err(Error::DatabaseMissing(_))
    ));
}

#[test]
fn another_database_where_the_marker_expects_one_is_refused() {
    let (_t, m) = machine();
    let (_t2, m2) = machine();
    let (l, l2) = (Layout::new(&m), Layout::new(&m2));
    drop(open(&l));
    drop(open(&l2));
    // Put the other machine's database in this one's place (a restore from the wrong backup).
    for f in l.files() {
        if f.to_string_lossy().contains("core.db") {
            let _ = std::fs::remove_file(f);
        }
    }
    std::fs::copy(&l2.db, &l.db).expect("copy");
    let e = start_with(&l, MIGRATIONS).err().expect("swapped");
    assert!(matches!(e, Error::DatabaseSwapped(_)), "{e}");
    assert!(matches!(
        embedded_with(&l, MIGRATIONS),
        Err(Error::DatabaseSwapped(_))
    ));
}

#[test]
fn a_database_without_a_marker_adopts_it() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(open(&l));
    std::fs::remove_file(&l.marker).expect("rm marker");
    drop(open(&l));
    assert!(
        l.marker.is_file(),
        "a crash between the database and the marker is repaired on the next start"
    );
}

#[test]
fn embedded_creates_but_never_migrates_and_the_daemon_snapshots_then_migrates() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(start_with(&l, MIGRATIONS).expect("v1"));
    // This binary now knows schema 2; one command must not migrate.
    let e = embedded_with(&l, BOTH).err().expect("needs migration");
    assert!(
        matches!(&e, Error::NeedsMigration(t) if t.contains("kith migrate")),
        "{e}"
    );
    assert!(!l.snapshots.exists(), "nothing was snapshotted or changed");
    // The daemon (or `kith migrate`) does, under the lock, after a snapshot.
    let done = migrate_with(&l, BOTH).expect("migrate");
    assert_eq!((done.from, done.to), (Some(1), 2));
    let snaps: Vec<_> = std::fs::read_dir(&l.snapshots)
        .expect("snapshots")
        .collect();
    assert_eq!(snaps.len(), 1);
    let snap = snaps[0].as_ref().expect("entry").path();
    #[cfg(unix)]
    assert_eq!(mode(&snap), 0o600);
    let old = rusqlite::Connection::open(&snap).expect("open snapshot");
    let v: i64 = old
        .query_row("SELECT MAX(version) FROM migrations", [], |r| r.get(0))
        .expect("version");
    assert_eq!(v, 1, "the snapshot holds the schema it was taken at");
    // After that, embedded opens it.
    match embedded_with(&l, BOTH).expect("embedded") {
        Embedded::Open(c) => assert_eq!(c.schema_version(), 2),
        Embedded::DaemonRunning { .. } => panic!("lock held"),
    }
}

#[test]
fn a_fresh_embedded_core_is_created_at_the_latest_schema() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    match embedded_with(&l, BOTH).expect("fresh") {
        Embedded::Open(c) => assert_eq!(c.schema_version(), 2),
        Embedded::DaemonRunning { .. } => panic!("lock held"),
    }
}

#[test]
fn a_binary_older_than_the_database_refuses_to_write() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(start_with(&l, BOTH).expect("v2"));
    for e in [
        start_with(&l, MIGRATIONS).err().expect("older"),
        match embedded_with(&l, MIGRATIONS) {
            Err(e) => e,
            Ok(_) => panic!("an older binary opened a newer database"),
        },
    ] {
        assert!(
            matches!(&e, Error::BinaryOlder(t) if t.contains("Update kith")),
            "{e}"
        );
    }
    let c = rusqlite::Connection::open(&l.db).expect("open");
    let g: i64 = c
        .query_row(
            "SELECT value FROM counters WHERE name = 'generation'",
            [],
            |r| r.get(0),
        )
        .expect("gen");
    assert_eq!(
        g, 1,
        "the refused opens wrote nothing, not even a generation"
    );
}

#[test]
fn a_damaged_database_is_not_started() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(open(&l));
    let _ = std::fs::remove_file(format!("{}-wal", l.db.display()));
    let _ = std::fs::remove_file(format!("{}-shm", l.db.display()));
    let mut bytes = std::fs::read(&l.db).expect("read");
    for b in bytes.iter_mut().take(100) {
        *b = 0x41;
    }
    std::fs::write(&l.db, bytes).expect("write");
    assert!(start_with(&l, MIGRATIONS).is_err());
}

#[test]
fn stateless_through_core_runs_the_verb_inside_an_embedded_core_or_bare() {
    let (_t, m) = machine();
    let p = paths(&m);
    // No machine folder yet (first setup): bare, and nothing is created.
    let (v, via) = stateless_through_core(&p, || 7);
    assert_eq!((v, via), (7, Via::Bare));
    assert!(!m.exists());
    // A machine folder with no core.db: still bare, and the verb creates nothing.
    std::fs::create_dir_all(&m).expect("machine");
    let (v, via) = stateless_through_core(&p, || 8);
    assert_eq!((v, via), (8, Via::Bare));
    assert_eq!(std::fs::read_dir(&m).expect("ls").count(), 0);
    let l = Layout::new(&m);
    drop(open(&l));
    let ((), via) = stateless_through_core(&p, || {
        // The lock is held while the verb runs.
        assert!(matches!(
            embedded_with(&l, MIGRATIONS),
            Ok(Embedded::DaemonRunning { .. })
        ));
    });
    assert_eq!(via, Via::Embedded);
    // A daemon holds the lock: the verb still runs.
    let daemon = start_with(&l, MIGRATIONS).expect("daemon");
    let (v, via) = stateless_through_core(&p, || "ran");
    assert_eq!((v, via), ("ran", Via::Bare));
    drop(daemon);
}

#[test]
fn only_claim_tx_opens_a_transaction_that_touches_claims() {
    // Every `BEGIN IMMEDIATE` is in db.rs; this crate's other files open no transaction at all, so a claim transaction
    // can only come from `Core::claim_tx`, which checks the generation.
    for (name, src) in [
        ("lib.rs", include_str!("lib.rs")),
        ("lock.rs", include_str!("lock.rs")),
        ("marker.rs", include_str!("marker.rs")),
        ("perms.rs", include_str!("perms.rs")),
        ("layout.rs", include_str!("layout.rs")),
        ("error.rs", include_str!("error.rs")),
    ] {
        for needle in ["Immediate", "transaction(", "BEGIN"] {
            assert!(
                !src.contains(needle),
                "{name} opens a transaction ({needle})"
            );
        }
    }
    let db = include_str!("db.rs");
    let claim_fn = db.find("fn claim_tx").expect("claim_tx");
    let first_claim_read = db.find("StaleGeneration").expect("stale");
    assert!(first_claim_read > claim_fn);
    assert_eq!(db.matches("pub(crate) fn claim_tx").count(), 1);
    assert_eq!(
        db.matches("-> Result<Transaction").count(),
        1,
        "one function returns a transaction"
    );
}

#[test]
fn an_owner_only_access_list_is_told_from_an_open_one() {
    let p = r"C:\Users\kai\.kith\core";
    let mine = format!(
        "{p} Kai-PC\\kai:(OI)(CI)(F)\n          NT AUTHORITY\\SYSTEM:(OI)(CI)(F)\n          BUILTIN\\Administrators:(OI)(CI)(F)\n\nSuccessfully processed 1 files; Failed processing 0 files\n"
    );
    assert!(perms::acl_is_owner_only(&mine, p, "kai"));
    let open = format!("{p} Kai-PC\\kai:(OI)(CI)(F)\n          BUILTIN\\Users:(OI)(CI)(RX)\n");
    assert!(!perms::acl_is_owner_only(&open, p, "kai"));
    let everyone = format!("{p} Everyone:(OI)(CI)(F)\n");
    assert!(!perms::acl_is_owner_only(&everyone, p, "kai"));
    assert!(
        !perms::acl_is_owner_only("", p, "kai"),
        "an unreadable list is not owner-only"
    );
    let _ = std::io::stdout().flush();
}

#[test]
fn a_fallback_caused_by_an_error_is_one_line_in_the_core_log_and_a_free_run_is_not() {
    let (_t, m) = machine();
    let p = paths(&m);
    let l = Layout::new(&m);
    drop(open(&l));
    let log = m.join(crate::log::FILE);

    // A daemon holds the lock: bare, nothing wrong, nothing logged.
    let daemon = start_with(&l, MIGRATIONS).expect("daemon");
    let ((), via) = stateless_through_core(&p, || ());
    assert_eq!(via, Via::Bare);
    assert!(!log.exists());
    drop(daemon);

    // The marker names another database: the verb still runs, and the reason is on record.
    crate::marker::write(&l, "00000000000000000000000000000000").expect("marker");
    let (v, via) = stateless_through_core(&p, || 5);
    assert_eq!(v, 5);
    assert!(
        matches!(&via, Via::BareAfterError(why) if why.contains("another file")),
        "{via:?}"
    );
    let text = std::fs::read_to_string(&log).expect("core.log");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("verb ran without the core") && text.contains("another file"));
}

#[test]
fn the_database_id_is_written_in_the_first_migrations_transaction() {
    const BROKEN: &[Migration] = &[Migration {
        version: 1,
        name: "broken",
        sql: "CREATE TABLE migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL);
              CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
              INSERT INTO nowhere VALUES (1);",
    }];
    let mut conn = rusqlite::Connection::open_in_memory().expect("memory");
    crate::db::apply(&mut conn, MIGRATIONS, Some("abc")).expect("apply");
    let id: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'database_id'",
            [],
            |r| r.get(0),
        )
        .expect("id");
    assert_eq!(id, "abc");

    // A first step that fails after its schema leaves nothing: no tables, hence no database without an id.
    let mut conn = rusqlite::Connection::open_in_memory().expect("memory");
    assert!(crate::db::apply(&mut conn, BROKEN, Some("abc")).is_err());
    let tables: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .expect("count");
    assert_eq!(tables, 0);
}

#[test]
fn calls_go_through_the_table() {
    use kith_api::Principal;
    use serde_json::json;
    let (_t, m) = machine();
    let p = paths(&m);
    let who = Principal::Owner;
    let core = open(&Layout::new(&m)).with_folders(&p);

    // A method the core answers without state.
    let v = core
        .call(who, "kernel.version", &json!({}))
        .expect("version");
    assert_eq!(v["abi"], 1);
    let v = core.call(who, "paths", &json!({})).expect("paths");
    assert_eq!(v["KITH_MACHINE"], m.to_string_lossy().as_ref());

    // A caller that gives its view gets the paths of that view, not the core's: the daemon's environment is not the
    // environment of the CLI that asked.
    let seen = core
        .call(
            who,
            "paths",
            &json!({"home": "/nowhere/home", "env": {"KITH_HOME": "/caller/kith", "KITH_STATE": "/caller/state"}, "kernel": "/k/kernel"}),
        )
        .expect("caller view");
    // Compared as paths: Windows spells them with backslashes.
    let path = |k: &str| PathBuf::from(seen[k].as_str().expect(k));
    assert_eq!(path("KITH_HOME"), Path::new("/caller/kith"));
    assert_eq!(path("KITH_STATE"), Path::new("/caller/state"));
    assert_ne!(seen["KITH_MACHINE"], m.to_string_lossy().as_ref());
    assert!(
        core.call(who, "paths", &json!({"env": {"KITH_HOME": 3}}))
            .is_err(),
        "env is a map of strings"
    );

    // Refused before anything runs.
    assert!(matches!(
        core.call(who, "no.such", &json!({})),
        Err(Error::NoSuchMethod(_))
    ));
    assert!(matches!(
        core.call(Principal::Member, "kernel.version", &json!({})),
        Err(Error::NotAllowed(_))
    ));
    assert!(matches!(
        core.call(who, "kernel.version", &json!({"x": 1})),
        Err(Error::BadParams(_))
    ));
    assert!(matches!(
        core.call(who, "kernel.delegation.record", &json!({})),
        Err(Error::BadParams(_))
    ));

    // In the table but not answered here: the caller falls back.
    assert!(matches!(
        core.call(who, "agents.list", &json!({})),
        Err(Error::NotServed(_))
    ));
}

#[test]
fn a_front_note_that_stays_the_same_is_one_line_in_the_core_log() {
    let (_t, m) = machine();
    let p = paths(&m);
    for _ in 0..3 {
        crate::note_once(&p, "the daemon speaks protocol 9");
    }
    crate::note_once(&p, "the daemon has another method table");
    crate::note_once(&p, "the daemon has another method table");
    let log = std::fs::read_to_string(p.logs.join("core.log")).expect("core.log");
    assert_eq!(log.lines().count(), 2, "{log}");
    assert!(
        log.lines().all(|l| l.contains(" front: the daemon ")),
        "{log}"
    );
}

/// Schema version 1 as M0 shipped it, written out here so the fixture does not depend on the code under test.
const M0_SCHEMA: &str = "
        CREATE TABLE migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at INTEGER NOT NULL
        );
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
        INSERT INTO counters (name, value) VALUES ('generation', 0), ('claim_id', 0);
    ";

/// A core.db as an M0 binary left it: the old SQL, its migrations row, a database id and the marker.
fn m0_database(l: &Layout) {
    perms::ensure_dir(&l.dir).expect("dir");
    perms::create_private_file(&l.db).expect("db");
    let c = rusqlite::Connection::open(&l.db).expect("open");
    c.pragma_update(None, "journal_mode", "WAL").expect("wal");
    c.execute_batch(M0_SCHEMA).expect("m0 schema");
    c.execute(
        "INSERT INTO migrations (version, name, applied_at) VALUES (1, 'core', 1)",
        [],
    )
    .expect("row");
    c.execute(
        "INSERT INTO meta (key, value) VALUES ('database_id', 'm0-fixture')",
        [],
    )
    .expect("id");
    drop(c);
    crate::marker::write(l, "m0-fixture").expect("marker");
}

fn schema_of(l: &Layout) -> Vec<(String, String, Option<String>)> {
    let c = rusqlite::Connection::open(&l.db).expect("open");
    let mut q = c
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
        .expect("prepare");
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

#[test]
fn an_m0_database_opens_unchanged_and_migrates_to_the_new_head() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    m0_database(&l);
    // Today's head is still 1: the M0 file opens as it is, nothing migrates.
    let c = start_with(&l, MIGRATIONS).expect("m0 opens");
    assert_eq!(c.schema_version(), 1);
    assert_eq!(c.migrated_from(), None);
    drop(c);
    // A later head migrates it, and the daemon says where it came from.
    let c = start_with(&l, BOTH).expect("migrates");
    assert_eq!(c.schema_version(), 2);
    assert_eq!(c.migrated_from(), Some(1));
}

#[test]
fn a_fresh_database_and_a_migrated_one_end_with_the_same_schema() {
    let (_t, fresh) = machine();
    let lf = Layout::new(&fresh);
    drop(start_with(&lf, BOTH).expect("fresh"));

    let (_t2, old) = machine();
    let lo = Layout::new(&old);
    m0_database(&lo);
    drop(start_with(&lo, BOTH).expect("migrated"));

    assert_eq!(schema_of(&lf), schema_of(&lo));
    assert!(schema_of(&lf).iter().any(|(_, name, _)| name == "counters"));
}

#[test]
fn the_m2_stub_rows_answer_not_served_so_the_caller_falls_back() {
    let (_t, m) = machine();
    let c = open(&Layout::new(&m));
    let owner = kith_api::Principal::Owner;
    for (name, params) in [
        (
            "people.grant",
            serde_json::json!({"person": "ann", "agent": "echo"}),
        ),
        (
            "people.revoke",
            serde_json::json!({"person": "ann", "agent": "echo"}),
        ),
        ("people.list", serde_json::json!({})),
        ("people.requests", serde_json::json!({})),
        ("people.approve", serde_json::json!({"id": "r1"})),
        ("people.deny", serde_json::json!({"id": "r1"})),
        ("cards.list", serde_json::json!({})),
        ("cards.show", serde_json::json!({"id": "c1"})),
        (
            "cards.answer",
            serde_json::json!({"id": "c1", "answer": "yes"}),
        ),
    ] {
        assert!(
            matches!(c.call(owner, name, &params), Err(Error::NotServed(_))),
            "{name}"
        );
        for other in [
            kith_api::Principal::Member,
            kith_api::Principal::Bot,
            kith_api::Principal::RunToken,
        ] {
            assert!(
                matches!(c.call(other, name, &params), Err(Error::NotAllowed(_))),
                "{name} for {}",
                other.name()
            );
        }
    }
}

const MID: Migration = Migration {
    version: 11,
    name: "mid",
    sql: "CREATE TABLE mid (x INTEGER);",
};
const LATE: Migration = Migration {
    version: 20,
    name: "late",
    sql: "CREATE TABLE late (x INTEGER);",
};
const LATE_RENAMED: Migration = Migration {
    version: 20,
    name: "other",
    sql: "CREATE TABLE late (x INTEGER);",
};

#[test]
fn a_step_below_the_highest_applied_one_is_applied_and_ends_like_a_fresh_database() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    // A binary that shipped step 20 before step 11 was merged.
    drop(start_with(&l, &[MIGRATIONS[0], LATE]).expect("{1, 20}"));
    let all = [MIGRATIONS[0], MID, LATE];
    // One command does not migrate, and says which step is missing.
    let e = embedded_with(&l, &all).err().expect("missing 11");
    assert!(
        matches!(&e, Error::NeedsMigration(t) if t.contains("11")),
        "{e}"
    );
    let c = start_with(&l, &all).expect("daemon applies 11");
    assert_eq!(c.schema_version(), 20);
    drop(c);

    let (_t2, fresh) = machine();
    let lf = Layout::new(&fresh);
    drop(start_with(&lf, &all).expect("fresh"));
    assert_eq!(schema_of(&l), schema_of(&lf));
    let c = rusqlite::Connection::open(&l.db).expect("open");
    let rows: i64 = c
        .query_row("SELECT COUNT(*) FROM migrations", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 3);
    // Now nothing is missing, so one command opens it.
    assert!(matches!(
        embedded_with(&l, &all).expect("embedded"),
        Embedded::Open(_)
    ));
}

#[test]
fn an_unknown_applied_step_is_refused_by_the_daemon_and_by_embedded_mode() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(start_with(&l, &[MIGRATIONS[0], LATE]).expect("{1, 20}"));
    // This binary knows 1 and 11 only: 20 is somebody else's.
    let known = [MIGRATIONS[0], MID];
    for e in [
        start_with(&l, &known).err().expect("daemon"),
        embedded_with(&l, &known).err().expect("embedded"),
    ] {
        assert!(
            matches!(&e, Error::BinaryOlder(t) if t.contains("Update kith")),
            "{e}"
        );
    }
    let c = rusqlite::Connection::open(&l.db).expect("open");
    let rows: i64 = c
        .query_row("SELECT COUNT(*) FROM migrations", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rows, 2, "nothing was applied");
}

#[test]
fn a_known_version_under_another_name_is_refused() {
    let (_t, m) = machine();
    let l = Layout::new(&m);
    drop(start_with(&l, &[MIGRATIONS[0], LATE]).expect("{1, 20}"));
    let other = [MIGRATIONS[0], LATE_RENAMED];
    for e in [
        start_with(&l, &other).err().expect("daemon"),
        embedded_with(&l, &other).err().expect("embedded"),
    ] {
        assert!(matches!(&e, Error::Corrupt(t) if t.contains("20")), "{e}");
    }
}
