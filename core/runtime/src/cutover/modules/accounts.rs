//! The `accounts` module (S1): `<machine>/accounts/state.json`, each account's out mark, usage reading and last start, moves
//! to the `account_state` table (m0105).
//!
//! - `import`: the M3-9a import (`import::accounts`), run under the framework's lock. Read-only on the sources; rows that
//!   differ are replaced (the file is the truth until the switch).
//! - `same_answer`: the file's accounts (the old kernel marks laid over them in memory, as the readers see them) against the
//!   table's, entry by entry and key by key. `pick`'s order and active account are rules over this state plus the settings
//!   (which do not move), run by the same code over either source (`accounts/tests.rs` holds `pick` over core.db to the
//!   file's `pick` on the golden corpora), so equal state is the same order and the same active account. A file that says
//!   nothing (missing, or not ours) has nothing to compare, as the import keeps the rows then.
//! - `tombstones`: `state.json`. The writers that find it fail closed: `kith_accounts.py` (`kith account out|clear|usage`, a
//!   launch's `last_started`) raises `Tombstoned` and `kith-account` prints it; the Rust kernel's `update` and its legacy
//!   merge leave the note alone and log (`kith_kernel::accounts::tombstoned`).
//! - `export_back`: the table's rows written as `state.json` again, over the tombstone (the original stays beside it as
//!   `state.json.pre-cutover-<time>`).
//! - `step_refusal`: `kith migrate step accounts` refuses today. After the tombstone every reader of the file fails closed, and
//!   the core has no reader (`kith_accounts.py` pick/rows/list, `kernel.accounts.*`) or writer (`kith account out|clear|usage`)
//!   for the account state yet, so a launch would stop. The plan gates the step on that lane (docs/plans/one-bot-model.md).
//! - `source_lock`: an exclusive flock on `state.json.lock` from the import through the tombstone, the lock the Python and
//!   Rust writers take, so no mark is lost in between and no writer's rename lands over the note.
//! - Once switched the core is the writer: `accounts::core_writes` is true exactly when `may_serve` says yes, so the
//!   failover's `mark_out` writes the row; a rolled-back or never-stepped core writes none.

use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::cutover::state::{self, Phase};
use crate::cutover::{Env, Module, Opts, SourceLock};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::accounts::{AccountsOptions, import_accounts};

pub(crate) struct Accounts;

/// How long a step waits for a writer that holds `state.json.lock` (a launch's mark takes milliseconds).
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

impl Module for Accounts {
    fn name(&self) -> &'static str {
        "accounts"
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        vec![kith_kernel::accounts::state_file(&env.machine)]
    }

    fn step_refusal(&self, _env: &Env) -> Option<String> {
        // Past the tombstone nothing but the core may answer for the file, and the readers still read the file:
        // `kith_accounts.py`'s read verbs (pick, rows, list), `kernel.accounts.*` and the out, clear and usage commands have
        // no core reader or writer yet, so a step would stop every agent launch (`--record` hits the tombstone).
        Some(
            "not stepped: the core has no reader or writer for kith_accounts.py's read verbs, kernel.accounts.* or out/clear/usage yet; step accounts only after that lane exists"
                .into(),
        )
    }

    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        // `state.json.lock`: the lock `kith_accounts.py` and the Rust `update` take. A writer mid-update finishes first.
        let held = kith_kernel::accounts::hold(&env.machine, LOCK_WAIT).map_err(|e| {
            Error::Refused(format!(
                "cutover: accounts: cannot take the writers' lock: {e}"
            ))
        })?;
        Ok(Some(Box::new(held)))
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let dry_run = opts.dry_run || core.is_none();
        Ok(import_accounts(&env.machine, AccountsOptions { dry_run }, core)?.lines)
    }

    fn same_answer(&self, env: &Env, db: &rusqlite::Connection) -> Result<Vec<String>> {
        if kith_kernel::accounts::tombstoned(&env.machine) {
            // Moved already: the core's rows are the only copy (the check is a pre-switch hook).
            let moved = state::read(&env.layout, self.name())?
                .is_some_and(|s| matches!(s.phase, Phase::Switched | Phase::RollingBack));
            if moved {
                return Ok(Vec::new());
            }
            // Not switched, yet the note is there: a core.db restored from before the step, or a lost record. Whatever
            // the core holds is not the answer the readers got, so a step must not switch on it.
            return Ok(vec![
                "state.json is a tombstone but accounts is not switched here (kith rollback accounts brings the file back)"
                    .into(),
            ]);
        }
        let snap = kith_kernel::accounts::snapshot(&env.machine);
        if (snap.bytes.is_none() && !snap.legacy_present)
            || (snap.not_ours && snap.accounts.is_empty())
        {
            return Ok(Vec::new());
        }
        let have = crate::accounts::read_all(db)?;
        let mut diffs = Vec::new();
        for (id, want) in &snap.accounts {
            match have.get(id) {
                None => diffs.push(format!("{id}: in state.json, not in the core")),
                Some(got) if got != want => {
                    diffs.push(format!("{id}: state.json says {want}, the core says {got}"));
                }
                Some(_) => {}
            }
        }
        for id in have.keys().filter(|id| !snap.accounts.contains_key(*id)) {
            diffs.push(format!("{id}: in the core, not in state.json"));
        }
        Ok(diffs)
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let rows = crate::accounts::read_all(core.conn())?;
        kith_kernel::accounts::replace(&env.machine, &rows).map_err(|e| {
            Error::Io(format!(
                "cutover: accounts: state.json cannot be written again: {e}"
            ))
        })?;
        Ok(vec![format!(
            "accounts: wrote {} account(s) back to state.json",
            rows.len()
        )])
    }

    fn digest(&self, _env: &Env, db: &rusqlite::Connection) -> Result<String> {
        let rows = Value::Object(crate::accounts::read_all(db)?).to_string();
        Ok(Sha256::digest(rows.as_bytes())
            .iter()
            .fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            }))
    }
}
