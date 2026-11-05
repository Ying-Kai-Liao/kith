//! The `claims` module (S2, the claims-and-turns lane): the live writer claim files `$KITH_STATE/live/terminal/<sid>.claim`
//! into the `claims` table, so a core that starts serving finds the writers that are already running (M3-10b3,
//! `import::claims`). Registered so the import has a caller; it switches nothing until S2.
//!
//! - `import`: `import::claims::run_in` under the framework's lock. Read-only on the files, idempotent (a conversation that
//!   already has a live claim row on the file's pid is left alone; a live row on another pid is listed, never replaced).
//! - `same_answer`: every live claim file of a conversation the core knows has a claim row on its pid. The files are only
//!   copied, never touched, so there is nothing to tombstone and nothing to write back.
//! - `step_refusal`: `kith migrate step claims` refuses today. Python's `kith-sessions`, `kith` and `kith-wake` are the writers
//!   of the claim files, and the core runs no turn, so a switch would leave two writers. `step --dry-run` still shows what an
//!   import would add.

use std::path::PathBuf;

use crate::cutover::{Env, Module, Opts};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::claims::{differences, run, run_in};

pub(crate) struct Claims;

fn state(env: &Env) -> Result<&std::path::Path> {
    env.state.as_deref().ok_or_else(|| {
        Error::Refused("cutover: claims: needs $KITH_STATE (the CLI passes it)".into())
    })
}

impl Module for Claims {
    fn name(&self) -> &'static str {
        "claims"
    }

    fn tombstones(&self, _env: &Env) -> Vec<PathBuf> {
        Vec::new()
    }

    fn step_refusal(&self, _env: &Env) -> Option<String> {
        Some(
            "not stepped: kith-sessions, kith and kith-wake still write the claim files and the core runs no turn; step claims only with S2's switch"
                .into(),
        )
    }

    fn dry_run_despite_refusal(&self) -> bool {
        true
    }

    fn guards_claims(&self) -> bool {
        // The module never owned the writers: a live claim must not block its rollback.
        false
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let state = state(env)?;
        let report = match core {
            Some(core) => run_in(core, state, opts.dry_run)?,
            None => run(&env.layout, state, true)?,
        };
        Ok(report.lines)
    }

    fn same_answer(&self, env: &Env, db: &rusqlite::Connection) -> Result<Vec<String>> {
        differences(state(env)?, db)
    }

    fn export_back(&self, _env: &Env, _core: &mut Core) -> Result<Vec<String>> {
        Ok(vec![
            "claims: nothing to write back (the claim files were never touched)".into(),
        ])
    }
}
