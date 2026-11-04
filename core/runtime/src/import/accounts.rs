//! `<machine>/accounts/state.json` -> the `account_state` table of core.db (m0105), M3-9a. Unix only, like the other
//! imports (the pre-write backup is a `VACUUM INTO`).
//!
//! What it promises:
//! - Read-only on the sources: `state.json` and `serve/kernel-backups-out.json` are read (under a shared hold of
//!   `state.json.lock` when the lock file exists, so a launch writing `last_started` is never seen half done) and never
//!   written, renamed or created. The kernel's own `read` imports the legacy file by renaming it; this step lays the same
//!   marks over the file's accounts IN MEMORY (`kith_kernel::accounts::snapshot`, the same merge code) and leaves the
//!   file where it is, so the file side keeps importing it the way it always did.
//! - `--dry-run` opens nothing for writing and prints what would change.
//! - A real run takes the core's lifetime lock, a `VACUUM INTO` backup before its first write, and writes in one
//!   transaction: the rows, a read-back that must equal what the file says, a row in `import_log`.
//! - Idempotent: when core.db already holds exactly these accounts, nothing is written (no backup, no log row).
//! - Rows that differ are REPLACED, not refused: the core writes no account state while Python owns the file (m0105), so a
//!   difference is an older import, and the file is the truth until the writers switch together.
//! - No file at all (no `state.json`, no legacy file): nothing to import, rows already in core.db stay as they are.
//!
//! Mapping and what the columns hold: see the header of m0105. In short: `out_until`, `reason`, `since`, `cleared_at`,
//! `cleared_host_until`, `last_started` and `usage` have columns; any other key of an entry, and a known key whose value
//! is not the plain type, is kept whole in `extra_json`, so an entry reads back exactly as the file had it.
//!
//! Reported, not kept: a `state.json` that is not ours (corrupt, not an object, no `accounts` object; the file side reads
//! it as empty too, and the next write moves it to `state.json.corrupt`) imports nothing; top-level keys other than
//! `version` and `accounts`; an entry of the file that is not an object (the readers drop those too).

use std::path::Path;

use kith_kernel::paths::Paths;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest as _, Sha256};

use crate::db::MIGRATIONS;
use crate::error::Result;
use crate::layout::Layout;

/// What `kith migrate --accounts` may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountsOptions {
    /// Print what would change and write nothing.
    pub dry_run: bool,
}

/// What [`migrate_accounts`] did, one line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountsReport {
    pub dry_run: bool,
    /// Whether core.db changed, or in a dry run would.
    pub changed: bool,
    /// The `VACUUM INTO` copy taken before the first write.
    pub backup: Option<std::path::PathBuf>,
    pub lines: Vec<String>,
}

fn sha_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `kith migrate --accounts`: copies the accounts' state file into core.db (see the module doc).
pub fn migrate_accounts(paths: &Paths, opts: AccountsOptions) -> Result<AccountsReport> {
    let layout = Layout::new(&paths.machine);
    let snap = kith_kernel::accounts::snapshot(&paths.machine);
    let file = snap.file.clone();
    let bytes = snap.bytes.clone();
    let mut rep = AccountsReport {
        dry_run: opts.dry_run,
        ..AccountsReport::default()
    };
    if snap.not_ours {
        rep.lines.push(format!(
            "{} is not ours (unreadable, not an object, or no `accounts` object): nothing is imported from it",
            file.display()
        ));
    }
    if !snap.extra_keys.is_empty() {
        rep.lines.push(format!(
            "top-level keys of state.json other than version and accounts are not kept: {:?}",
            snap.extra_keys
        ));
    }
    if snap.legacy_present {
        rep.lines.push(if snap.legacy_applied {
            "serve/kernel-backups-out.json: its marks are laid over the file's accounts (the file is left where it is)".into()
        } else {
            "serve/kernel-backups-out.json is unreadable in part: none of its marks are used, as the kernel reads it".into()
        });
    }
    if bytes.is_none() && !snap.legacy_present {
        rep.lines.push(format!(
            "no state.json at {}: nothing to import",
            file.display()
        ));
        return Ok(rep);
    }
    // A file that is not ours says nothing: rows core.db already holds are kept, not deleted for it.
    let keep = snap.not_ours && snap.accounts.is_empty();
    let desired = snap.accounts;
    let n = desired.len();
    let readable = if layout.db.is_file() {
        Connection::open_with_flags(&layout.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
            .filter(has_table)
    } else {
        None
    };
    let have = readable.as_ref().map(crate::accounts::read_all);
    if opts.dry_run {
        match have {
            Some(Ok(h)) if keep && !h.is_empty() => rep.lines.push(format!(
                "accounts: state.json is not ours; core.db's {} account row(s) would be kept",
                h.len()
            )),
            Some(Ok(h)) if h == desired => rep.lines.push(format!(
                "accounts: core.db already holds exactly state.json's {n} account(s); nothing to change"
            )),
            Some(Ok(h)) if !h.is_empty() => {
                rep.changed = true;
                rep.lines.push(format!(
                    "accounts: would REPLACE core.db's {} account row(s) with state.json's {n}",
                    h.len()
                ));
            }
            _ => {
                rep.changed = true;
                rep.lines
                    .push(format!("accounts: would import {n} account(s)"));
            }
        }
        rep.lines.push("dry run: nothing was written".into());
        return Ok(rep);
    }
    drop(readable);
    real(&layout, &file, bytes.as_deref(), &desired, keep, &mut rep)?;
    Ok(rep)
}

fn has_table(c: &Connection) -> bool {
    c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'account_state'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|n| n > 0)
}

fn real(
    layout: &Layout,
    file: &Path,
    bytes: Option<&[u8]>,
    desired: &serde_json::Map<String, serde_json::Value>,
    keep: bool,
    rep: &mut AccountsReport,
) -> Result<()> {
    let n = desired.len();
    // The lifetime lock for the whole step; this also brings the schema up to date, as `kith migrate` does.
    let mut core = crate::start_with(layout, MIGRATIONS)?;
    let held = crate::accounts::read_all(core.conn())?;
    if keep && !held.is_empty() {
        rep.lines.push(format!(
            "state.json is not ours: core.db's {} account row(s) are kept",
            held.len()
        ));
        return Ok(());
    }
    if held == *desired {
        rep.lines.push(format!(
            "nothing to change: core.db already holds state.json's {n} account(s)"
        ));
        return Ok(());
    }
    // Before the first write.
    let backup = core.backup(layout, "pre-import")?;
    rep.lines
        .push(format!("backup of core.db: {}", backup.display()));
    rep.backup = Some(backup.clone());
    let tx = core.claim_tx()?;
    crate::accounts::replace_all(&tx, desired)?;
    if crate::accounts::read_all(&tx)? != *desired {
        return Err(crate::error::Error::Refused(
            "accounts import: the stored rows do not read back as the file's; nothing was imported"
                .into(),
        ));
    }
    tx.execute(
        "INSERT INTO import_log (step, at, source, source_sha256, backup, summary) VALUES ('accounts', ?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            now_secs(),
            file.display().to_string(),
            bytes.map(sha_hex).unwrap_or_default(),
            backup.display().to_string(),
            format!("accounts: {n} account(s)"),
        ],
    )?;
    tx.commit()?;
    rep.changed = true;
    rep.lines.push(format!(
        "accounts: imported {n} account(s). core.db is a copy: every run still reads and writes state.json (the switch is a later step)"
    ));
    Ok(())
}
