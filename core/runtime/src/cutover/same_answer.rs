//! The same-answer check as a question anyone may ask on its own, and the guard built on it.
//!
//! A module's `same_answer` returns the differences it finds (empty = the core answers what the old store answered). It
//! must work both before the tombstone (comparing with the source files) and after it (the sources are gone: compare with
//! what the module recorded at the switch). The framework calls it after an import; the host-start guard and a fresh
//! read of the switch call [`may_serve`].

use super::{Env, Module, state};
use crate::db::Core;
use crate::error::Result;

/// The count of differences, and the first few of them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub count: usize,
    pub first: Vec<String>,
}

pub(crate) fn check(m: &dyn Module, env: &Env, core: &Core) -> Result<Check> {
    let diffs = m.same_answer(env, core)?;
    Ok(Check {
        count: diffs.len(),
        first: diffs.into_iter().take(5).collect(),
    })
}

/// May the core serve this module now: switched and the check passes. While the check fails the module stays effectively
/// unswitched whatever an env opt-in (KITH_CORE_<MODULE>=1) says, and `warning` says why. `warning` is `None` for a plain
/// "not switched" (nothing is wrong).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Serve {
    pub ok: bool,
    pub warning: Option<String>,
}

pub(crate) fn may_serve(m: &dyn Module, env: &Env, core: &Core) -> Serve {
    let name = m.name();
    match state::read(&env.layout, name) {
        Ok(Some(s)) if s.phase == state::Phase::Switched => match check(m, env, core) {
            Ok(c) if c.count == 0 => Serve {
                ok: true,
                warning: None,
            },
            Ok(c) => Serve {
                ok: false,
                warning: Some(format!(
                    "{name}: {} difference(s) between the core and the old store ({}); not served from the core until `kith migrate step {name}` or `kith rollback {name}`",
                    c.count,
                    c.first.join(" | ")
                )),
            },
            Err(e) => Serve {
                ok: false,
                warning: Some(format!("{name}: the same-answer check could not run: {e}")),
            },
        },
        Ok(Some(s)) => Serve {
            ok: false,
            warning: Some(format!("{name} is {}, not switched", s.phase.word())),
        },
        Ok(None) => Serve {
            ok: false,
            warning: None,
        },
        Err(e) => Serve {
            ok: false,
            warning: Some(e.to_string()),
        },
    }
}
