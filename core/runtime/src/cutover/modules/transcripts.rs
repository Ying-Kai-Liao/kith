//! The `transcripts` module (S1): the session registry `$KITH_STATE/live/terminal/<sid>.json` is read into `conversations`
//! and `engine_sessions` (M3-6), so the core knows where each engine keeps a session and the hash of its files.
//!
//! It is the minimal correct module, and the reason it has no tombstone is the point of S1:
//! - The registry's only Python writer is `kith-sessions record` (the `SessionStart` hook) beside its writer claims. Those are
//!   the claims-and-turns lane (S2), and a hook that fails closed at session start would stop every terminal session before
//!   the core can run one. So S1 tombstones nothing, `tombstones` is empty, and Python stays the writer of the registry.
//!   What the switch changes is only that the core's rows are the answer for `conversation.transcript`.
//! - Because Python still writes, `same_answer` can only say that every session the registry names is in the core with the
//!   same engine, session id, pointer and working folder. It ignores the hash and the size (the core reads the transcript
//!   itself and moves them on), and it ignores rows the core holds that the registry does not.
//! - Re-running `step` (or `kith migrate --transcripts`) imports sessions registered since: an import leaves a row that is
//!   there as it is. A rollback has nothing to export: the registry was never touched, so it clears the marker and the switch
//!   file, and the imported rows stay (the next step finds them already there).
//!
//! The registry needs `$KITH_STATE` and the home folder, which `Env` carries when the CLI built it from the machine's
//! `Paths` (`Env::with_paths`).

use std::path::PathBuf;

use kith_kernel::threads::Ctx;

use crate::cutover::{Env, Module, Opts};
use crate::db::Core;
use crate::error::Result;
use crate::import::transcripts::{Options, expected, run_in};
use crate::transcripts::rows;

pub(crate) struct Transcripts;

fn ctx<'a>(env: &Env) -> Ctx<'a> {
    Ctx {
        state: env.state.clone(),
        machine: Some(env.machine.clone()),
        home: env.home.clone(),
        env: &kith_kernel::paths::process_env,
    }
}

impl Module for Transcripts {
    fn name(&self) -> &'static str {
        "transcripts"
    }

    fn tombstones(&self, _env: &Env) -> Vec<PathBuf> {
        Vec::new()
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let opts = Options {
            dry_run: opts.dry_run || core.is_none(),
        };
        Ok(run_in(&env.layout, &ctx(env), opts, core)?.lines)
    }

    fn same_answer(&self, env: &Env, db: &rusqlite::Connection) -> Result<Vec<String>> {
        let mut diffs = Vec::new();
        for (_, want) in expected(&ctx(env)) {
            let id = format!("{} ({})", want.conversation, want.account);
            let conversations: i64 = db.query_row(
                "SELECT COUNT(*) FROM conversations WHERE id = ?1",
                [&want.conversation],
                |r| r.get(0),
            )?;
            let Some(got) = rows::get(db, &want.conversation, &want.account)? else {
                diffs.push(format!("{id}: in the registry, not in the core"));
                continue;
            };
            if conversations == 0 {
                diffs.push(format!(
                    "{id}: the core has the session but no conversation"
                ));
            }
            for (what, a, b) in [
                ("engine", &want.engine, &got.engine),
                ("session id", &want.session_id, &got.session_id),
                ("pointer", &want.pointer, &got.pointer),
                ("working folder", &want.cwd, &got.cwd),
            ] {
                if a != b {
                    diffs.push(format!(
                        "{id}: {what} is {a:?} in the registry, {b:?} in the core"
                    ));
                }
            }
        }
        Ok(diffs)
    }

    fn export_back(&self, _env: &Env, _core: &mut Core) -> Result<Vec<String>> {
        Ok(vec![
            "transcripts: nothing to write back (Python never stopped writing the registry)".into(),
        ])
    }

    fn fingerprint(&self, env: &Env, _db: &rusqlite::Connection) -> Result<String> {
        // No tombstone to stat, so the registry folder stands for "the old store changed" in the opt-in guard.
        let Some(dir) = ctx(env).reg_dir() else {
            return Ok(String::new());
        };
        let mut seen: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let md = e.metadata().ok()?;
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
