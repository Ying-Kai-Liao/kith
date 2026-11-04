//! The same-answer check as a question anyone may ask on its own, and the gate built on it.
//!
//! A module's `same_answer` returns the differences it finds (empty = the core answers what the old store answered). It is
//! a PRE-SWITCH hook: the import invariant, core.db against the module's sources. After the switch the core is the only
//! writer, so comparing against anything recorded at the switch would fail on the first legitimate grant or revoke, with
//! nothing safe to fall back to. `check` on a switched module says so and does not compare.
//!
//! It takes a read-only connection, not the core: the CLI opens its own read-only handle (no lifetime lock, so it works
//! while the daemon is up) and the daemon passes the connection it already holds.
//!
//! [`may_serve`] is the gate (the host-start guard and every answer ask it). One source of truth, the switch file:
//! - `switched` serves only when core.db holds the marker `step` wrote for this file's `at` (else: deny everything, warn),
//!   and every tombstone is still a tombstone naming this module (a missing one still serves, the core is the truth, but
//!   warns loudly: something wrote the old file after the switch). The env opt-in is ignored.
//! - `stepping`, `rolling-back` and a damaged file are never served from the core.
//! - No file: the legacy side answers, unless the env opt-in (`KITH_CORE_<MODULE>=1`, read by the caller ONCE and passed as
//!   `opt_in`; M3 lanes must not read their own; it goes at M2-24). With it, the core serves only while the import
//!   invariant holds *right now*: [`Guard`] fingerprints the sources and re-runs `same_answer` whenever they changed, because
//!   in this mode the old store is still written.
//!
//! A switch file can therefore only ever deny, never grant. Caveat: an owner-run agent can write anything in the core
//! folder (no deny is added, by the rule against restricting owner runs); the marker is why that can only deny.

use std::collections::HashMap;
use std::sync::Mutex;

use rusqlite::Connection;

use super::{Env, Module, marker, state, tombstone};
use crate::error::Result;

/// The count of differences, and the first few of them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub count: usize,
    pub first: Vec<String>,
}

pub(crate) fn check(m: &dyn Module, env: &Env, db: &Connection) -> Result<Check> {
    let diffs = m.same_answer(env, db)?;
    Ok(Check {
        count: diffs.len(),
        first: diffs.into_iter().take(5).collect(),
    })
}

/// May the core serve this module now. `warning` is `None` for a plain "not switched" (nothing is wrong); an `ok` answer may
/// still carry a loud warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serve {
    pub ok: bool,
    pub warning: Option<String>,
}

fn deny(why: impl Into<String>) -> Serve {
    Serve {
        ok: false,
        warning: Some(why.into()),
    }
}

fn names_module(m: &dyn Module, p: &std::path::Path) -> bool {
    tombstone::module_of(p).as_deref() == Some(m.name())
}

/// The gate without the opt-in cache; see the module doc for the rules.
pub(crate) fn may_serve(m: &dyn Module, env: &Env, db: &Connection, opt_in: bool) -> Serve {
    let name = m.name();
    let tombstones = m.tombstones(env);
    match state::read(&env.layout, name) {
        Ok(Some(s)) if s.phase == state::Phase::Switched => match marker::read(db, name) {
            Some((at, _)) if at == s.at => match tombstones.iter().find(|p| !names_module(m, p)) {
                None => Serve {
                    ok: true,
                    warning: None,
                },
                Some(p) => Serve {
                    ok: true,
                    warning: Some(format!(
                        "WARNING {name}: {} is no longer a tombstone: something wrote the old file after the switch. The core still answers; look at it (kith migrate status)",
                        p.display()
                    )),
                },
            },
            _ => deny(format!(
                "{name}: the switch file has no matching marker in core.db (forged, or core.db was restored from before the step); nothing is served from the core. `kith migrate step {name}` or `kith rollback {name}`"
            )),
        },
        Ok(Some(s)) => deny(format!("{name} is {}, not switched", s.phase.word())),
        Err(e) => deny(e.to_string()),
        Ok(None) if !opt_in => Serve {
            ok: false,
            warning: None,
        },
        Ok(None) => {
            if let Some(p) = tombstones.iter().find(|p| names_module(m, p)) {
                return deny(format!(
                    "{name}: {} is tombstoned but there is no switch record; not served from the core (kith migrate step {name} repairs it)",
                    p.display()
                ));
            }
            match check(m, env, db) {
                Ok(c) if c.count == 0 => Serve {
                    ok: true,
                    warning: None,
                },
                Ok(c) => deny(format!(
                    "{name}: {} difference(s) between the core and the old store ({}); not served from the core",
                    c.count,
                    c.first.join(" | ")
                )),
                Err(e) => deny(format!("{name}: the same-answer check could not run: {e}")),
            }
        }
    }
}

/// What the old store looks like right now: each source's inode, size and mtime, plus the module's own fingerprint
/// (a database's `data_version`, the vault). A change means the check must run again.
fn fingerprint(m: &dyn Module, env: &Env, db: &Connection) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for p in m.tombstones(env) {
        match std::fs::metadata(&p) {
            Ok(md) => {
                #[cfg(unix)]
                let ino = std::os::unix::fs::MetadataExt::ino(&md);
                #[cfg(not(unix))]
                let ino = 0u64;
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                let _ = write!(out, "{}:{ino}:{}:{mtime};", p.display(), md.len());
            }
            Err(_) => {
                let _ = write!(out, "{}:none;", p.display());
            }
        }
    }
    out.push_str(
        &m.fingerprint(env, db)
            .unwrap_or_else(|e| format!("err:{e}")),
    );
    out
}

/// The opt-in cache: remembers the last answer per module together with the fingerprint it was given for, so every answer
/// costs one `stat` per source and the data compare runs only after the old store changed.
#[derive(Default)]
pub(crate) struct Guard {
    seen: Mutex<HashMap<String, (String, Serve)>>,
}

impl Guard {
    pub(crate) fn may_serve(
        &self,
        m: &dyn Module,
        env: &Env,
        db: &Connection,
        opt_in: bool,
    ) -> Serve {
        // The file is the truth and cheap to ask: only the file-less opt-in path is cached.
        let filed = !matches!(state::read(&env.layout, m.name()), Ok(None));
        if filed || !opt_in {
            return may_serve(m, env, db, opt_in);
        }
        let fp = fingerprint(m, env, db);
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((was, s)) = seen.get(m.name())
            && *was == fp
        {
            return s.clone();
        }
        let s = may_serve(m, env, db, opt_in);
        seen.insert(m.name().to_owned(), (fp, s.clone()));
        s
    }
}
