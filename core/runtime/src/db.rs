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

use kith_kernel::paths::Paths;
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::lock::{self, Holder, Lifetime, Role, Taken};
use crate::{marker, perms};

pub(crate) use crate::migrations::{MIGRATIONS, Migration};

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
    /// The folders of the process that opened this core (`paths` answers with them when the caller gives no view).
    folders: Option<Paths>,
}

/// A process the holder reported, as the core noted it: the group leader's pid and the start time the system gave that
/// pid then, so a later kill can tell the process from a stranger that was handed the same pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedProc {
    pub pid: u32,
    pub started: String,
    pub conv: String,
}

/// What the core last knew of the holder: its `instance` (random per holder start) and the processes it listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HolderRecord {
    /// `None` until a hello has met a holder (a core.db from M0a has none: nothing is killed on that account).
    pub instance: Option<String>,
    pub procs: Vec<RecordedProc>,
}

pub(crate) enum Opened {
    Core(Box<Core>),
    Busy(Option<Holder>),
}

pub(crate) fn open(layout: &Layout, migrations: &[Migration], mode: Mode) -> Result<Opened> {
    perms::ensure_dir(&layout.dir)?;
    perms::check_files(layout)?;
    let role = match mode {
        Mode::Daemon => Role::Daemon,
        Mode::Embedded => Role::Embedded,
    };
    let lifetime = match lock::take(layout, role)? {
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
    let mut conn = open_connection(&layout.db)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    perms::check_files(layout)?;

    let mut migrated_from = None;
    let schema;
    if fresh {
        // The database id goes in with the first migration, one transaction: a crash leaves no database, or a whole
        // one, never a schema without an id (which every later open would refuse as corrupt).
        let id = new_database_id()?;
        apply(&mut conn, migrations, Some(&id))?;
        schema = latest;
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
        // What decides is the set of steps, never the highest number: version order is not merge order (reserved
        // lanes), so a step can be missing below a version the database already has.
        let applied = applied_steps(&conn)?;
        let missing = missing_steps(&applied, migrations)?;
        if !missing.is_empty() {
            if mode == Mode::Embedded {
                let list: Vec<String> = missing.iter().map(|m| m.version.to_string()).collect();
                return Err(Error::NeedsMigration(format!(
                    "kith-core: core.db is missing schema step {}, this kith is at {latest}; a single command doesn't migrate. \
                     Run `kith migrate`, or start the daemon (kith serve start).",
                    list.join(", ")
                )));
            }
            integrity_check(&conn)?;
            snapshot(layout, &conn, have)?;
            apply(&mut conn, &missing, None)?;
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
        folders: None,
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

    /// Remembers the folders of the process that opened this core.
    pub(crate) fn with_folders(mut self, folders: &Paths) -> Self {
        self.folders = Some(folders.clone());
        self
    }

    pub(crate) fn folders(&self) -> Option<&Paths> {
        self.folders.as_ref()
    }

    /// `VACUUM INTO` a private copy in the snapshots folder, named for `label` (`kith migrate`'s imports take one before
    /// their first write). Returns the copy's path.
    pub(crate) fn backup(&self, layout: &Layout, label: &str) -> Result<std::path::PathBuf> {
        perms::ensure_dir(&layout.snapshots)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = layout.snapshots.join(format!("core-{label}-{nanos}.db"));
        vacuum_into(&self.conn, &path)?;
        perms::make_private(&path)?;
        Ok(path)
    }

    /// The holder instance and processes the last hello recorded.
    pub(crate) fn holder_record(&self) -> Result<HolderRecord> {
        let instance = get_meta(&self.conn, "holder_instance")?;
        let procs = match get_meta(&self.conn, "holder_procs")? {
            None => Vec::new(),
            Some(text) => parse_procs(&text),
        };
        Ok(HolderRecord { instance, procs })
    }

    /// Records the holder instance and its processes in one transaction that refuses a stale generation: an old core
    /// that lost the lock cannot overwrite what the newer one noted.
    pub(crate) fn save_holder_record(&mut self, rec: &HolderRecord) -> Result<()> {
        let procs = serde_json::Value::Array(
            rec.procs
                .iter()
                .map(|p| serde_json::json!({"pid": p.pid, "started": p.started, "conv": p.conv}))
                .collect(),
        )
        .to_string();
        let tx = self.claim_tx()?;
        match &rec.instance {
            Some(i) => tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('holder_instance', ?1)",
                [i],
            )?,
            None => tx.execute("DELETE FROM meta WHERE key = 'holder_instance'", [])?,
        };
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('holder_procs', ?1)",
            [&procs],
        )?;
        tx.commit()?;
        Ok(())
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

/// Opens core.db. When SQLite cannot, the message carries its extended result code and what the operating system says
/// to a plain open of the same file, since "unable to open database file" alone does not say which of permissions,
/// sharing or the path it was.
fn open_connection(path: &Path) -> Result<Connection> {
    Connection::open(path).map_err(|e| {
        let code = match &e {
            rusqlite::Error::SqliteFailure(f, _) => format!(" (sqlite code {})", f.extended_code),
            _ => String::new(),
        };
        let os = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .err()
            .map_or_else(
                || "a plain open works".to_owned(),
                |o| format!("a plain open says: {o}"),
            );
        Error::Io(format!("core.db: {e}{code}; {os}"))
    })
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

fn parse_procs(text: &str) -> Vec<RecordedProc> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            Some(RecordedProc {
                pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
                started: v.get("started")?.as_str()?.to_owned(),
                conv: v
                    .get("conv")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect()
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

/// The `(version, name)` pairs the migrations table holds.
fn applied_steps(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let corrupt = |e: rusqlite::Error| {
        Error::Corrupt(format!(
            "kith-core: core.db is not a kith core database ({e})"
        ))
    };
    let mut q = conn
        .prepare("SELECT version, name FROM migrations ORDER BY version")
        .map_err(corrupt)?;
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(corrupt)?
        .collect::<std::result::Result<_, _>>()
        .map_err(corrupt)
}

/// The known steps the database lacks, in version order. A version this binary does not know, or a known version
/// stored under another name, refuses: the database was written by a different history than this binary's.
fn missing_steps(applied: &[(i64, String)], migrations: &[Migration]) -> Result<Vec<Migration>> {
    for (version, name) in applied {
        match migrations.iter().find(|m| m.version == *version) {
            None => {
                return Err(Error::BinaryOlder(format!(
                    "kith-core: core.db has schema step {version} ({name}), which this kith does not know; it won't write. Update kith."
                )));
            }
            Some(m) if m.name != name => {
                return Err(Error::Corrupt(format!(
                    "kith-core: core.db has schema step {version} as {name:?}, this kith has it as {:?}; not starting.",
                    m.name
                )));
            }
            Some(_) => {}
        }
    }
    let mut missing: Vec<Migration> = migrations
        .iter()
        .filter(|m| !applied.iter().any(|(v, _)| *v == m.version))
        .copied()
        .collect();
    missing.sort_by_key(|m| m.version);
    Ok(missing)
}

/// Runs `steps` in order, each in its own immediate transaction with its row in the migrations table. A new
/// database's `id` is written inside the first step's transaction.
pub(crate) fn apply(conn: &mut Connection, steps: &[Migration], id: Option<&str>) -> Result<()> {
    for (i, m) in steps.iter().enumerate() {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql)?;
        if let (0, Some(id)) = (i, id) {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('database_id', ?1)",
                [id],
            )?;
        }
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
