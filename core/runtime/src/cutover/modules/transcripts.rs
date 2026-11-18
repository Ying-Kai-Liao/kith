//! The `transcripts` module: the session registry `$KITH_STATE/live/terminal/<sid>.json` becomes the core's. After the switch
//! `kith-sessions record` (the `SessionStart` hook) spools its record and asks the core (`session.record`); the core keeps the
//! `session_records` rows and writes each `<sid>.json` from its row (a projection, so every reader of the files is unchanged).
//! Stepped by `kith migrate step transcripts` on a Mac with the host stopped and `claims` already switched. The model is
//! `verification/registry/RegistryStep.tla` (skill `registry-step`); the files here are its `Begin`..`Switch`, `Cancel`,
//! `Rollback` and `ExportBack`.
//!
//! - `preflight` ([`super::transcripts_preflight`]): plain refusals that write nothing: the host is stopped, not Windows, the
//!   installed `kith-sessions` knows Writer mode, core.db (when it exists) has `session_records`. Entries in `inbox/` are not
//!   a refusal: the tombstone the step itself puts makes hooks spool, so a rerun would meet them (architect MUST 1).
//! - `import`: files into rows with `clear_gone`: a file newer than its row replaces it (the one order key, A6/A7), and a row
//!   whose file is gone (pruned since an earlier step or cancel) is dropped, so a second step does not write it back.
//! - `same_answer` ([`super::transcripts_answer`]): every file has a row with the same record, and no row is newer than its file
//!   or has no file. The check the step runs after the import and again, narrower, in the re-read.
//! - The tombstone is `live/terminal/registry.tombstone`, read by existence (`kernel/lib/kith_registry.py`). It is put in
//!   `before_tombstone`, i.e. BEFORE the marker and the switched record: from then on a hook is in Writer mode (spools, calls
//!   the core, which answers not-serving until the switch). `reread` then drains the inbox, takes the files in again and
//!   clears rows whose file is gone, under the lifetime lock. A row newer than its file, or with no file, refuses and takes the
//!   tombstone away again (architect MUST 2): the module stays `stepping`, hooks write files as before, nothing is lost.
//!   A file newer than its row is no refusal; the daemon's reconcile adopts it after the switch.
//! - A crash with the tombstone in place leaves the module `stepping` and hooks in Writer mode: they spool, nothing serves
//!   `session.record`, so a new session is not listed until `kith migrate step transcripts` is run again or the step is
//!   cancelled with `kith rollback transcripts` (see [`Module::check_notes`]).
//! - Rollback ([`super::transcripts_back`]): the host must be stopped; the record says rolling-back (hooks keep spooling); then
//!   `export_back` drains the inbox into the rows, writes each row to its file only where it is newer than the file (A3), drains
//!   once more and removes the tombstone last. An entry spooled after that last drain waits for the next Files-mode drain (the
//!   next session start on this Mac, or kith-sched's hourly pass): a documented gap, A4.
//! - After the switch only the daemon's 60 s pass reconciles files and rows; start the host.

use std::path::{Path, PathBuf};

use kith_kernel::threads::Ctx;

use super::transcripts_answer::{differences, reread};
use super::transcripts_back::{cancel, export_back};
use super::transcripts_preflight::{host_line, preflight};
use crate::cutover::authority_preflight::Preflight;
use crate::cutover::{Env, Module, Opts, tombstone};
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::transcripts::{Options, run_in};

pub(crate) struct Transcripts;

pub(super) fn state(env: &Env) -> Result<&Path> {
    env.state.as_deref().ok_or_else(|| {
        Error::Refused("cutover: transcripts: needs $KITH_STATE (the CLI passes it)".into())
    })
}

/// `live/terminal/registry.tombstone`: the marker `kith_registry.switched` reads by existence.
pub(super) fn marker_of(state: &Path) -> PathBuf {
    kith_kernel::threads::registry_dir(state).join("registry.tombstone")
}

pub(super) fn ctx<'a>(env: &Env) -> Ctx<'a> {
    Ctx {
        state: env.state.clone(),
        machine: Some(env.machine.clone()),
        home: env.home.clone(),
        env: &kith_kernel::paths::process_env,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl Module for Transcripts {
    fn name(&self) -> &'static str {
        "transcripts"
    }

    fn depends_on(&self) -> &'static [&'static str] {
        // `kith-sessions record` writes the registry beside its writer claims; once the claims are the core's the whole hook
        // asks the core.
        &["claims"]
    }

    fn guards_claims(&self) -> bool {
        // A live claim is not a registry writer: nothing here depends on it.
        false
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        env.state.iter().map(|s| marker_of(s)).collect()
    }

    fn preflight(
        &self,
        env: &Env,
        _import: Option<&Result<Vec<String>>>,
    ) -> Result<Option<Preflight>> {
        preflight(env).map(Some)
    }

    fn rollback_refusal(&self, env: &Env) -> Option<String> {
        // Not overridable: a running host starts hooks' sessions while the registry is half put back.
        host_line(
            env,
            "the host is running; stop it, roll back, start it; --force does not override this",
        )
    }

    fn cancel_step(&self, env: &Env) -> Result<Vec<String>> {
        cancel(env)
    }

    fn before_tombstone(&self, env: &Env) -> Result<()> {
        let state = state(env)?;
        if let Some(why) = host_line(env, super::transcripts_preflight::HOST_RUNNING) {
            return Err(Error::Refused(format!(
                "cutover: transcripts: {why}; NOT switched"
            )));
        }
        // First, so a hook that starts from here on is in Writer mode (spools, writes no file).
        tombstone::ensure(&marker_of(state), self.name(), now_secs())?;
        Ok(())
    }

    fn reread(&self, env: &Env, core: &mut Core) -> Result<()> {
        reread(env, core)
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let opts = Options {
            dry_run: opts.dry_run || core.is_none(),
            clear_gone: true,
        };
        Ok(run_in(&env.layout, &ctx(env), opts, core)?.lines)
    }

    fn same_answer(&self, env: &Env, db: &rusqlite::Connection) -> Result<Vec<String>> {
        differences(env, db)
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        export_back(env, core)
    }

    fn check_notes(&self, env: &Env) -> Vec<String> {
        let mut out = vec![
            "transcripts: after the step start the host (`kith serve`): only the daemon's 60 s pass drains the spool and reconciles files and rows. If a step stopped with registry.tombstone in place, new sessions are not listed until `kith migrate step transcripts` is run again or `kith rollback transcripts` cancels it".to_owned(),
        ];
        if let Ok(s) = state(env)
            && marker_of(s).exists()
            && !tombstone::is_tombstone(&marker_of(s))
        {
            out.push(format!(
                "transcripts: {} exists but is not a tombstone note",
                marker_of(s).display()
            ));
        }
        out
    }

    fn fingerprint(&self, env: &Env, _db: &rusqlite::Connection) -> Result<String> {
        // The registry's record files only: the inbox, the locks, temp files and the tombstone are not "the old store changed".
        let Some(dir) = ctx(env).reg_dir() else {
            return Ok(String::new());
        };
        let mut seen: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                Path::new(&n).extension().is_some_and(|x| x == "json") && !n.starts_with('.')
            })
            .filter_map(|e| {
                let md = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
                let t = md
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?;
                Some(format!(
                    "{}:{}:{};",
                    e.file_name().to_string_lossy(),
                    md.len(),
                    t.as_nanos()
                ))
            })
            .collect();
        seen.sort();
        Ok(seen.concat())
    }
}
