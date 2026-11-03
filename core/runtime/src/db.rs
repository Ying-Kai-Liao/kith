//! core.db: SQLite in WAL mode, `BEGIN IMMEDIATE` for every write, a migrations table, a generation counter and a durable
//! claim-id counter. This file and `layout.rs` are the only places that touch the database; it is where the rules of
//! runtime-design sections 2 and 6 meet:
//!
//! - The lifetime lock (`lock.rs`) is taken first. Whoever holds it bumps the generation, embedded mode included.
//! - [`Core::claim_tx`] is the one door to a transaction that touches claims, and it refuses a stale generation.
//! - Every daemon start runs `quick_check`; embedded commands skip it (hooks call them, so they stay cheap).
//! - Migrations run only for a daemon start or `migrate`, after `integrity_check` and a `VACUUM INTO` snapshot. An
//!   embedded core creates a fresh core.db but never migrates an existing one.
//! - A binary that knows an older schema than the file it finds refuses to open it.
//! - The marker (`marker.rs`) says a core.db existed; a start that finds none, or another one, refuses to begin empty.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::lock::{self, Holder, Lifetime, Taken};
use crate::{marker, perms};

#[derive(Clone, Copy)]
pub(crate) struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every schema this binary knows, oldest first. Add to the end; never edit a step that has shipped.
pub(crate) const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "core",
    sql: "
        CREATE TABLE migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at INTEGER NOT NULL
        );
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
        INSERT INTO counters (name, value) VALUES ('generation', 0), ('claim_id', 0);
    ",
}];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// `kith serve`: checks the file, migrates when it is older.
    Daemon,
    /// One command in its own process: creates a fresh core.db, never migrates one that exists.
    Embedded,
}

/// An open core: the lock, the connection, and the generation this core took the lock at.
pub struct Core {
    conn: Connection,
    _lock: Lifetime,
    generation: i64,
    schema: i64,
    migrated_from: Option<i64>,
}

pub(crate) enum Opened {
    Core(Box<Core>),
    Busy(Option<Holder>),
}

pub(crate) fn open(layout: &Layout, migrations: &[Migration], mode: Mode) -> Result<Opened> {
    perms::ensure_dir(&layout.dir)?;
    perms::check_files(layout)?;
    let lifetime = match lock::take(layout)? {
        Taken::Held(l) => l,
        Taken::Busy(h) => return Ok(Opened::Busy(h)),
    };
    let latest = migrations.last().map_or(0, |m| m.version);
    let marked = marker::read(layout)?;
    let empty = std::fs::metadata(&layout.db).map_or(true, |m| m.len() == 0);

    let fresh = match (&marked, empty) {
        (None, true) => true,
        (Some(id), true) => {
            return Err(Error::DatabaseMissing(format!(
                "kith-core: core.db is missing, but {} says one existed (id {id}); not starting with an empty database, \
                 which would read as \"no grants\". Restore it (kith restore) or, if you really mean to start over, \
                 delete {}",
                layout.marker.display(),
                layout.marker.display()
            )));
        }
        (_, false) => false,
    };

    perms::create_private_file(&layout.db)?;
    let mut conn = Connection::open(&layout.db)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    perms::check_files(layout)?;

    let mut migrated_from = None;
    let schema;
    if fresh {
        apply(&mut conn, migrations, 0)?;
        schema = latest;
        let id = new_database_id()?;
        set_meta(&mut conn, "database_id", &id)?;
        marker::write(layout, &id)?;
    } else {
        if mode == Mode::Daemon {
            quick_check(&conn)?;
        }
        let have = schema_version(&conn)?;
        let id = get_meta(&conn, "database_id")?;
        match (&marked, id) {
            (Some(m), Some(d)) if *m != d => {
                return Err(Error::DatabaseSwapped(format!(
                    "kith-core: core.db is database {d}, but {} recorded {m}; another file was put in its place. \
                     Not starting; restore the right one or delete the marker to adopt this one.",
                    layout.marker.display()
                )));
            }
            (None, Some(d)) => marker::write(layout, &d)?,
            (_, None) => {
                return Err(Error::Corrupt(
                    "kith-core: core.db has no database id".into(),
                ));
            }
            _ => {}
        }
        if have > latest {
            return Err(Error::BinaryOlder(format!(
                "kith-core: core.db is schema {have}, this kith knows up to {latest}; it won't write. Update kith."
            )));
        }
        if have < latest {
            if mode == Mode::Embedded {
                return Err(Error::NeedsMigration(format!(
                    "kith-core: core.db is schema {have}, this kith is {latest}; a single command doesn't migrate. \
                     Run `kith migrate`, or start the daemon (kith serve start)."
                )));
            }
            integrity_check(&conn)?;
            snapshot(layout, &conn, have)?;
            apply(&mut conn, migrations, have)?;
            migrated_from = Some(have);
        }
        schema = latest;
    }

    let generation = bump_generation(&mut conn)?;
    perms::check_files(layout)?;
    Ok(Opened::Core(Box::new(Core {
        conn,
        _lock: lifetime,
        generation,
        schema,
        migrated_from,
    })))
}

impl Core {
    /// The generation this core took the lock at. A newer core has a larger one.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The schema version of core.db.
    pub fn schema_version(&self) -> i64 {
        self.schema
    }

    /// The schema this open migrated from, when it migrated.
    pub(crate) fn migrated_from(&self) -> Option<i64> {
        self.migrated_from
    }

    /// The only door to a transaction that touches claims (claim, redeem, ack, reconcile apply): `BEGIN IMMEDIATE`, and
    /// the generation in core.db must still be this core's. A core that lost the lock (a lost lock on a network disk, a
    /// bug) finds a larger number and is refused, so the two layers each cover the other's failure.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the first caller is the claim code of M3")
    )]
    pub(crate) fn claim_tx(&mut self) -> Result<Transaction<'_>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_counter(&tx, "generation")?;
        if current != self.generation {
            return Err(Error::StaleGeneration(format!(
                "kith-core: this core is generation {}, core.db is at {current}; a newer core has taken over, so this one refuses claims",
                self.generation
            )));
        }
        Ok(tx)
    }

    /// The next claim id, advanced inside the caller's claim transaction so a restarted core never reuses one.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the first caller is the claim code of M3")
    )]
    pub(crate) fn next_claim_id(tx: &Transaction<'_>) -> Result<i64> {
        tx.execute(
            "UPDATE counters SET value = value + 1 WHERE name = 'claim_id'",
            [],
        )?;
        read_counter(tx, "claim_id")
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn read_counter(c: &Connection, name: &str) -> Result<i64> {
    Ok(
        c.query_row("SELECT value FROM counters WHERE name = ?1", [name], |r| {
            r.get(0)
        })?,
    )
}

fn bump_generation(conn: &mut Connection) -> Result<i64> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE counters SET value = value + 1 WHERE name = 'generation'",
        [],
    )?;
    let g = read_counter(&tx, "generation")?;
    tx.commit()?;
    Ok(g)
}

fn set_meta(conn: &mut Connection, key: &str, value: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )?;
    tx.commit()?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    match conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::Corrupt(format!(
            "kith-core: core.db is not a kith core database ({e})"
        ))),
    }
}

fn schema_version(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM migrations",
        [],
        |r| r.get(0),
    )
    .map_err(|e| {
        Error::Corrupt(format!(
            "kith-core: core.db is not a kith core database ({e})"
        ))
    })
}

/// Runs the steps above `from`, each in its own immediate transaction with its row in the migrations table.
fn apply(conn: &mut Connection, migrations: &[Migration], from: i64) -> Result<()> {
    for m in migrations.iter().filter(|m| m.version > from) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            "INSERT INTO migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.version, m.name, now_secs()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn quick_check(conn: &Connection) -> Result<()> {
    check_pragma(conn, "quick_check")
}

fn integrity_check(conn: &Connection) -> Result<()> {
    check_pragma(conn, "integrity_check")
}

fn check_pragma(conn: &Connection, which: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA {which}"))
        .map_err(|e| corrupt(which, &e.to_string()))?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| corrupt(which, &e.to_string()))?
        .filter_map(std::result::Result::ok)
        .collect();
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(corrupt(which, &rows.join("; ")))
    }
}

fn corrupt(which: &str, what: &str) -> Error {
    Error::Corrupt(format!(
        "kith-core: core.db failed {which}: {what}. Not starting; restore the latest backup (kith restore)."
    ))
}

/// `VACUUM INTO` a private copy before a migration, named for the schema it holds.
fn snapshot(layout: &Layout, conn: &Connection, version: i64) -> Result<()> {
    perms::ensure_dir(&layout.snapshots)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = layout.snapshots.join(format!("core-v{version}-{nanos}.db"));
    vacuum_into(conn, &path)?;
    perms::make_private(&path)
}

fn vacuum_into(conn: &Connection, path: &Path) -> Result<()> {
    let quoted = path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{quoted}'"))?;
    Ok(())
}

fn new_database_id() -> Result<String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw)
        .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
    Ok(raw.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}
