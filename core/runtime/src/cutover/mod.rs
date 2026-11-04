//! The cutover framework (M3-10a): ONE module-keyed way to move a part of Kith from its Python store to core.db, check it
//! answers the same, make the move one-way for the old readers, and go back. M3's lanes and M2's `authority` plug into it;
//! the framework registers no real module yet ([`registry`] is empty), the tests use a fake one.
//!
//! # A module
//! A [`Module`] is a name plus six hooks: `import` (idempotent; `--dry-run` gets no core; `--force`), `same_answer`
//! (differences, empty = same; callable on its own, before and after the tombstone), `tombstones` (the source files old
//! readers must stop trusting), `export_back` (write the source files again from core.db, keeping every revoke and key),
//! `restore_secrets` (write secrets back to the OS store: Keychain, DPAPI ...; run before the files so a file never
//! appears without its keys) and `live` (module-specific reasons not to roll back now). `depends_on` orders modules:
//! a step refuses until the dependencies are switched, a rollback refuses while a dependent is switched.
//! The cutover module name is the `module` column of `kernel/lib/core-switched.txt` for its verbs (that is how the router
//! knows which switch to look at).
//!
//! # Operations (all idempotent, all under core.db's lifetime lock, so two shells or a daemon refuse instead of racing)
//! - `step`: refuse unless dependencies are switched; `VACUUM INTO` snapshot (recorded, reused on resume); record
//!   `stepping`; `import`; `same_answer` must be empty or the step fails and stays `stepping` (Python is still the writer;
//!   re-run to resume, or roll back); record `switched`; put the tombstones. The tombstone comes AFTER the record so a
//!   crash between them is repaired by running `step` again (the import cannot be re-run once its sources are gone, and
//!   need not be).
//! - `rollback`: refuse while a dependent is switched, or while a claim/take is live (any claim not ended, or the module's
//!   `live`) unless `--force`; record `rolling-back` (still closed to Python); `restore_secrets`; `export_back`; every
//!   tombstone must be gone; clear the record. A crash leaves `rolling-back`: re-run to finish. A `stepping` module has
//!   nothing to export (nothing was tombstoned): the record is just cleared.
//! - `status`, `check` (the same-answer on its own), [`may_serve`] (switched AND the check passes, with a warning text).
//!
//! # State: one source of truth
//! [`state`]: a file per module in the core folder, readable by bash, Python and Rust without core.db (see there for why not
//! a table). The bash router refuses `KITH_CORE=python` for a switched module's verbs; the Python side reads tombstones
//! through `kernel/lib/kith_tombstone.py`.
//!
//! Unix only for now, like the imports it wraps (`Core::backup`).

use std::path::PathBuf;

use crate::db::{Core, MIGRATIONS};
use crate::error::{Error, Result};
use crate::layout::Layout;

pub(crate) mod same_answer;
pub(crate) mod state;
pub(crate) mod tombstone;
#[cfg(test)]
mod tests;

pub(crate) use same_answer::{Serve, may_serve};
use state::{Phase, Switch};

/// Where a module's files are.
pub(crate) struct Env {
    pub machine: PathBuf,
    pub layout: Layout,
}

impl Env {
    pub(crate) fn new(machine: &std::path::Path) -> Self {
        Self {
            machine: machine.to_path_buf(),
            layout: Layout::new(machine),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Opts {
    pub dry_run: bool,
    pub force: bool,
}

pub(crate) trait Module {
    fn name(&self) -> &'static str;
    /// Modules that must be switched first.
    fn depends_on(&self) -> &'static [&'static str] {
        &[]
    }
    /// The source files old readers must fail closed on after the switch.
    fn tombstones(&self, env: &Env) -> Vec<PathBuf>;
    /// Copies the sources into core.db; idempotent. `core` is `None` in a dry run (open nothing for writing).
    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>>;
    /// The differences between the core and the old store; empty = the same answer.
    fn same_answer(&self, env: &Env, core: &Core) -> Result<Vec<String>>;
    /// Writes the source files again from core.db (over the tombstones), keeping every revoke and key.
    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>>;
    /// Writes secrets back to the OS secret store. Runs before `export_back`.
    fn restore_secrets(&self, _env: &Env, _core: &mut Core) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Reasons this module must not be rolled back right now (besides live claims), e.g. a running host.
    fn live(&self, _env: &Env, _core: &Core) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Whether a live conversation claim or take blocks this module's rollback (default yes).
    fn guards_claims(&self) -> bool {
        true
    }
}

/// The registered modules. Empty until a lane switches (M3-10 b..g, M2-23).
pub(crate) fn registry() -> Vec<Box<dyn Module>> {
    Vec::new()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn find<'a>(reg: &'a [Box<dyn Module>], name: &str) -> Result<&'a dyn Module> {
    reg.iter()
        .find(|m| m.name() == name)
        .map(AsRef::as_ref)
        .ok_or_else(|| {
            let known: Vec<&str> = reg.iter().map(|m| m.name()).collect();
            Error::BadParams(if known.is_empty() {
                format!("cutover: no module \"{name}\" (no module has a cutover yet)")
            } else {
                format!("cutover: no module \"{name}\" (known: {})", known.join(", "))
            })
        })
}

fn phase_of(env: &Env, name: &str) -> Result<Option<Phase>> {
    Ok(state::read(&env.layout, name)?.map(|s| s.phase))
}

/// Takes the lifetime lock (a daemon or another shell refuses: stop it first).
fn lock(env: &Env) -> Result<Core> {
    crate::start_with(&env.layout, MIGRATIONS)
}

pub(crate) fn step(reg: &[Box<dyn Module>], env: &Env, name: &str, opts: Opts) -> Result<Vec<String>> {
    let m = find(reg, name)?;
    let cur = state::read(&env.layout, name)?;
    let mut lines = Vec::new();
    if cur.as_ref().is_some_and(|s| s.phase == Phase::RollingBack) {
        return Err(Error::Refused(format!(
            "cutover: {name} is part-way through a rollback; finish it first (kith rollback {name})"
        )));
    }
    for dep in m.depends_on() {
        if phase_of(env, dep)? != Some(Phase::Switched) {
            return Err(Error::Refused(format!(
                "cutover: {name} needs {dep} switched first (kith migrate step {dep})"
            )));
        }
    }
    if opts.dry_run {
        lines.extend(m.import(env, None, opts)?);
        lines.push(format!(
            "dry run: {name} is {}; nothing was changed",
            cur.map_or("not switched", |s| s.phase.word())
        ));
        return Ok(lines);
    }
    let mut core = lock(env)?;
    let mut snapshot = cur.as_ref().and_then(|s| s.snapshot.clone());
    if cur.as_ref().is_none_or(|s| s.phase == Phase::Stepping) {
        if snapshot.is_none() {
            let p = core.backup(&env.layout, &format!("cutover-{name}"))?;
            snapshot = Some(p.display().to_string());
            lines.push(format!("snapshot of core.db: {}", p.display()));
        }
        state::write(
            &env.layout,
            name,
            &Switch {
                phase: Phase::Stepping,
                snapshot: snapshot.clone(),
                at: now_secs(),
            },
        )?;
        lines.extend(m.import(env, Some(&mut core), opts)?);
        let c = same_answer::check(m, env, &core)?;
        if c.count > 0 {
            return Err(Error::Refused(format!(
                "cutover: {name}: the core's answers differ from the old store's ({} difference(s): {}); NOT switched, Python still writes. Fix and re-run `kith migrate step {name}`, or `kith rollback {name}`",
                c.count,
                c.first.join(" | ")
            )));
        }
        lines.push(format!("{name}: same answer as the old store"));
        state::write(
            &env.layout,
            name,
            &Switch {
                phase: Phase::Switched,
                snapshot,
                at: now_secs(),
            },
        )?;
    } else {
        lines.push(format!("{name} is already switched"));
    }
    for p in m.tombstones(env) {
        lines.push(tombstone::ensure(&p, name, now_secs())?);
    }
    lines.push(format!("{name}: switched"));
    Ok(lines)
}

pub(crate) fn rollback(
    reg: &[Box<dyn Module>],
    env: &Env,
    name: &str,
    opts: Opts,
) -> Result<Vec<String>> {
    let m = find(reg, name)?;
    let Some(cur) = state::read(&env.layout, name)? else {
        return Ok(vec![format!("{name} is not switched: nothing to roll back")]);
    };
    for other in reg.iter().filter(|o| o.depends_on().contains(&name)) {
        if phase_of(env, other.name())?.is_some() {
            return Err(Error::Refused(format!(
                "cutover: {} depends on {name} and is switched; roll it back first (kith rollback {})",
                other.name(),
                other.name()
            )));
        }
    }
    let mut lines = Vec::new();
    let mut core = lock(env)?;
    if cur.phase == Phase::Stepping {
        // Nothing was tombstoned and Python never stopped writing; the imported rows stay (a re-step is idempotent).
        state::clear(&env.layout, name)?;
        lines.push(format!("{name}: an unfinished step was cancelled"));
        return Ok(lines);
    }
    let mut live = m.live(env, &core)?;
    if m.guards_claims() {
        let n: i64 = core.conn().query_row(
            "SELECT COUNT(*) FROM claims WHERE state != 'ended'",
            [],
            |r| r.get(0),
        )?;
        if n > 0 {
            live.push(format!("{n} conversation claim(s) or take(s) are live"));
        }
    }
    if !live.is_empty() {
        if !opts.force {
            return Err(Error::Refused(format!(
                "cutover: not rolling {name} back while: {}; end them, or use --force",
                live.join("; ")
            )));
        }
        lines.push(format!("--force: rolling back although {}", live.join("; ")));
    }
    state::write(
        &env.layout,
        name,
        &Switch {
            phase: Phase::RollingBack,
            ..cur
        },
    )?;
    lines.extend(m.restore_secrets(env, &mut core)?);
    lines.extend(m.export_back(env, &mut core)?);
    for p in m.tombstones(env) {
        if tombstone::is_tombstone(&p) {
            return Err(Error::Refused(format!(
                "cutover: {name}: export-back left a tombstone at {}; still rolling back (re-run `kith rollback {name}`)",
                p.display()
            )));
        }
    }
    state::clear(&env.layout, name)?;
    lines.push(format!("{name}: rolled back; Python reads its own files again"));
    Ok(lines)
}

pub(crate) fn status(reg: &[Box<dyn Module>], env: &Env) -> Result<Vec<String>> {
    if reg.is_empty() {
        return Ok(vec!["no module has a cutover yet".into()]);
    }
    reg.iter()
        .map(|m| {
            let s = state::read(&env.layout, m.name())?;
            Ok(match s {
                None => format!("{}: not switched", m.name()),
                Some(s) => format!("{}: {} (since {})", m.name(), s.phase.word(), s.at),
            })
        })
        .collect()
}

/// The same-answer check on its own: one line per difference, or "same answer".
pub(crate) fn check(reg: &[Box<dyn Module>], env: &Env, name: &str) -> Result<Vec<String>> {
    let m = find(reg, name)?;
    let core = lock(env)?;
    let c = same_answer::check(m, env, &core)?;
    if c.count == 0 {
        return Ok(vec![format!("{name}: same answer")]);
    }
    let mut out = vec![format!("{name}: {} difference(s)", c.count)];
    out.extend(c.first);
    Ok(out)
}

/// `kith kernel cutover ...`: `step MODULE [--dry-run] [--force]`, `rollback MODULE [--force]`, `status`, `check MODULE`,
/// `modules`.
pub(crate) fn run(machine: &std::path::Path, args: &[String]) -> Result<Vec<String>> {
    run_with(&registry(), &Env::new(machine), args)
}

pub(crate) fn run_with(reg: &[Box<dyn Module>], env: &Env, args: &[String]) -> Result<Vec<String>> {
    let usage = || {
        Error::BadParams(
            "usage: cutover step MODULE [--dry-run] [--force] | rollback MODULE [--force] | status | check MODULE | modules"
                .into(),
        )
    };
    let (verb, rest) = args.split_first().ok_or_else(usage)?;
    let mut opts = Opts::default();
    let mut words = Vec::new();
    for a in rest {
        match a.as_str() {
            "--dry-run" if verb == "step" => opts.dry_run = true,
            "--force" if verb == "step" || verb == "rollback" => opts.force = true,
            w if w.starts_with("--") => return Err(usage()),
            w => words.push(w),
        }
    }
    match (verb.as_str(), words.as_slice()) {
        ("step", [m]) => step(reg, env, m, opts),
        ("rollback", [m]) => rollback(reg, env, m, opts),
        ("check", [m]) => check(reg, env, m),
        ("status", []) => status(reg, env),
        ("modules", []) => Ok(reg.iter().map(|m| m.name().to_owned()).collect()),
        _ => Err(usage()),
    }
}
