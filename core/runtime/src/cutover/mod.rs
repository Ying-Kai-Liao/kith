//! The cutover framework (M3-10a): ONE module-keyed way to move a part of Kith from its Python store to core.db, check it
//! answers the same, make the move one-way for the old readers, and go back. M3's lanes and M2's `authority` plug into it;
//! the framework's own tests use a fake module; the real ones are in [`modules`] (S1: `accounts`, `transcripts`) and
//! `authority` (M2-18a, `authority.rs`; the switch stays OFF).
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
//! - `step`: refuse when the module says it is not ready (`step_refusal`, before anything is written) or unless dependencies
//!   are switched; take the old store's writer lock (`source_lock`, held to the end); `VACUUM INTO` snapshot (recorded, reused on resume); record
//!   `stepping`; `import`; `same_answer` must be empty or the step fails and stays `stepping` (Python is still the writer;
//!   re-run to resume, or roll back); write the marker (`marker.rs`); record `switched`; put the tombstones. The tombstone comes AFTER the record so a
//!   crash between them is repaired by running `step` again (the import cannot be re-run once its sources are gone, and
//!   need not be).
//! - `rollback`: refuse while a dependent is switched, or while a claim/take is live (any claim not ended, or the module's
//!   `live`) unless `--force`; record `rolling-back` (still closed to Python); `restore_secrets`; `export_back`; every
//!   tombstone must be gone; clear the record. A crash leaves `rolling-back`: re-run to finish. A `stepping` module has
//!   nothing to export (nothing was tombstoned): the record is just cleared.
//! - `status`; `check` (the pre-switch same-answer on its own, read-only, no lock, so it runs beside a daemon; on a switched
//!   module it says the core is the only store); [`may_serve`] / the `cutover.mayServe` row (read-only): the gate, see
//!   `same_answer.rs` (switch file + the marker `step` wrote in core.db + tombstones; a file can only deny). A `step` on a
//!   module with no file whose every tombstone names it and whose marker is in core.db records `switched` again without
//!   importing (a lost record). `restore_secrets` helpers (idempotent, never delete, split big values): `secrets_back.rs`.
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

pub(crate) mod authority;
#[cfg(test)]
mod authority_tests;
pub(crate) mod marker;
pub(crate) mod modules;
pub(crate) mod same_answer;
pub(crate) mod secrets_back;
pub(crate) mod state;
#[cfg(test)]
mod tests;
pub(crate) mod tombstone;

pub(crate) use same_answer::Guard;
use state::{Phase, Switch};

/// Where a module's files are.
pub(crate) struct Env {
    pub machine: PathBuf,
    pub layout: Layout,
    /// Where the caller's auth.json and agent folders are, for the modules whose sources live there (`authority`). The
    /// daemon takes it from the request (the `home`, `kernel` and `env` the caller sends, as `people.*` does).
    pub ctx: Option<kith_kernel::secrets::Ctx>,
    /// `$KITH_STATE` (the session registry lives below it) and the home folder, for the modules that read more than the
    /// machine folder (transcripts). `new` leaves them empty: a module that needs them says so.
    pub state: Option<PathBuf>,
    pub home: PathBuf,
    /// The folders (agents, state ...), for the modules that read agents' files (schedules). `new` leaves them empty.
    pub paths: Option<kith_kernel::paths::Paths>,
    /// The host's `serve.db`, when it is not where the host keeps it (`$KITH_SERVE_DB`, `$KITH_SERVE_HOME`, else
    /// `<home>/.kith/serve/serve.db`): the schedules module reads the routines from it.
    pub serve_db: Option<PathBuf>,
}

impl Env {
    pub(crate) fn new(machine: &std::path::Path) -> Self {
        Self {
            machine: machine.to_path_buf(),
            layout: Layout::new(machine),
            ctx: None,
            state: None,
            home: machine.to_path_buf(),
            paths: None,
            serve_db: None,
        }
    }

    pub(crate) fn with_paths(paths: &kith_kernel::paths::Paths) -> Self {
        Self {
            state: Some(paths.state.clone()),
            home: paths.home.clone(),
            paths: Some(paths.clone()),
            ..Self::new(&paths.machine)
        }
    }

    pub(crate) fn with_ctx(mut self, ctx: kith_kernel::secrets::Ctx) -> Self {
        self.ctx = Some(ctx);
        self
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
    /// The import invariant: the differences between core.db and the sources; empty = the same answer. Meaningful before the
    /// tombstone only (see `same_answer.rs`). Read-only, so it also runs while the daemon holds the core.
    fn same_answer(&self, env: &Env, db: &rusqlite::Connection) -> Result<Vec<String>>;
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
    /// The import's digest, stored in the marker (informational: which import this switch stands for). Default none.
    fn digest(&self, _env: &Env, _db: &rusqlite::Connection) -> Result<String> {
        Ok(String::new())
    }
    /// The old store's own fingerprint beyond its files' inode/size/mtime (a database's `data_version`, the vault), for
    /// the opt-in guard: a changed fingerprint makes it re-run `same_answer`. Default none.
    fn fingerprint(&self, _env: &Env, _db: &rusqlite::Connection) -> Result<String> {
        Ok(String::new())
    }
    /// Whether a live conversation claim or take blocks this module's rollback (default yes).
    fn guards_claims(&self) -> bool {
        true
    }
    /// Why `step` must refuse right now (one line), checked before anything is read or written, so a refused step leaves no
    /// snapshot, record, marker or tombstone. Default: never. A module whose readers or writers are not all in the core yet
    /// says so here instead of switching.
    fn step_refusal(&self, _env: &Env) -> Option<String> {
        None
    }
    /// Whether `step --dry-run` still runs while `step_refusal` refuses (it reads and writes nothing, so a module that
    /// wants its import looked at before its lane exists says yes). A real step stays refused. Default: no.
    fn dry_run_despite_refusal(&self) -> bool {
        false
    }
    /// The lock the old store's own writers take, held by `step` from the import through the tombstone: a write between
    /// the two would be lost, and a writer that checked the file before the tombstone and renames after it would put the
    /// old file back over the note. Waits for a writer, or fails with a clear error; the lock is let go when the step ends.
    fn source_lock(&self, _env: &Env) -> Result<Option<SourceLock>> {
        Ok(None)
    }
}

/// What [`Module::source_lock`] holds; dropping it lets the lock go.
pub(crate) type SourceLock = Box<dyn std::any::Any>;

/// The registered modules, in switch order (S1 here, then M4-5a's schedules; S2 claims+turns, S4 wake, S3 asks and S5 failover plug in after them,
/// and M2's `authority`, M2-18a: registered, switch off). Registered is not switched: nothing steps a module but `kith migrate step`.
pub(crate) fn registry() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(modules::accounts::Accounts),
        Box::new(modules::transcripts::Transcripts),
        Box::new(modules::schedules::Schedules::new()),
        Box::new(authority::AuthorityModule::real()),
    ]
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
                format!(
                    "cutover: no module \"{name}\" (known: {})",
                    known.join(", ")
                )
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

pub(crate) fn step(
    reg: &[Box<dyn Module>],
    env: &Env,
    name: &str,
    opts: Opts,
) -> Result<Vec<String>> {
    let m = find(reg, name)?;
    if let Some(why) = m.step_refusal(env)
        && !(opts.dry_run && m.dry_run_despite_refusal())
    {
        return Err(Error::Refused(format!("cutover: {name}: {why}")));
    }
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
    // Held to the end of the step: the import, the check, the marker, the record and the tombstones.
    let _source = m.source_lock(env)?;
    let mut snapshot = cur.as_ref().and_then(|s| s.snapshot.clone());
    // A finished switch whose record was lost: every tombstone names this module and the core holds the marker. The
    // sources are gone, so there is nothing to import; the record is written again and that is all.
    if cur.is_none() {
        let tombs = m.tombstones(env);
        if let Some((at, _)) = marker::read(core.conn(), name).filter(|_| {
            !tombs.is_empty()
                && tombs
                    .iter()
                    .all(|p| tombstone::module_of(p).as_deref() == Some(name))
        }) {
            state::write(
                &env.layout,
                name,
                &Switch {
                    phase: Phase::Switched,
                    snapshot: None,
                    at,
                },
            )?;
            return Ok(vec![format!(
                "{name}: the switch had finished (tombstones in place, the core holds its marker); recorded as switched again, nothing imported"
            )]);
        }
    }
    if cur.as_ref().is_none_or(|s| s.phase == Phase::Stepping) {
        // One `at` for the whole step: the file's, and then the marker's.
        let at = cur.as_ref().map_or_else(now_secs, |s| s.at);
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
                at,
            },
        )?;
        lines.extend(m.import(env, Some(&mut core), opts)?);
        let c = same_answer::check(m, env, core.conn())?;
        if c.count > 0 {
            return Err(Error::Refused(format!(
                "cutover: {name}: the core's answers differ from the old store's ({} difference(s): {}); NOT switched, Python still writes. Fix and re-run `kith migrate step {name}`, or `kith rollback {name}`",
                c.count,
                c.first.join(" | ")
            )));
        }
        lines.push(format!("{name}: same answer as the old store"));
        let digest = m.digest(env, core.conn())?;
        marker::write(&mut core, name, at, &digest)?;
        state::write(
            &env.layout,
            name,
            &Switch {
                phase: Phase::Switched,
                snapshot,
                at,
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
        return Ok(vec![format!(
            "{name} is not switched: nothing to roll back"
        )]);
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
        lines.push(format!(
            "--force: rolling back although {}",
            live.join("; ")
        ));
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
    marker::clear(&mut core, name)?;
    state::clear(&env.layout, name)?;
    lines.push(format!(
        "{name}: rolled back; Python reads its own files again"
    ));
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
    if phase_of(env, name)? == Some(Phase::Switched) {
        return Ok(vec![format!(
            "{name}: switched: the core is the only store"
        )]);
    }
    // Read-only and without the lifetime lock, so it also answers while the daemon runs.
    let db = rusqlite::Connection::open_with_flags(
        &env.layout.db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| Error::Io(format!("cutover: core.db cannot be read here: {e}")))?;
    let c = same_answer::check(m, env, &db)?;
    if c.count == 0 {
        return Ok(vec![format!("{name}: same answer")]);
    }
    let mut out = vec![format!("{name}: {} difference(s)", c.count)];
    out.extend(c.first);
    Ok(out)
}

/// The env opt-in for a module: `KITH_CORE_<MODULE>=1` (dashes become underscores). Read here and nowhere else; it goes at
/// M2-24.
///
/// It is read in the daemon's own environment, not the caller's: a standalone daemon that was not started with the
/// variable fails closed (no opt-in), whatever the host that asks has set.
fn opt_in_env(name: &str) -> bool {
    let var = format!("KITH_CORE_{}", name.to_ascii_uppercase().replace('-', "_"));
    std::env::var(var).is_ok_and(|v| v == "1")
}

/// `cutover.mayServe`: the gate as a read-only row (see `same_answer::may_serve`).
pub(crate) fn serve_json(
    reg: &[Box<dyn Module>],
    env: &Env,
    db: &rusqlite::Connection,
    name: &str,
    opt_in: bool,
    guard: &Guard,
) -> Result<serde_json::Value> {
    let m = find(reg, name)?;
    let s = guard.may_serve(m, env, db, opt_in);
    Ok(serde_json::json!({"ok": s.ok, "warning": s.warning}))
}

pub(crate) fn call_may_serve(core: &Core, params: &serde_json::Value) -> Result<serde_json::Value> {
    static GUARD: std::sync::OnceLock<Guard> = std::sync::OnceLock::new();
    let name = params
        .get("module")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| Error::BadParams("cutover.mayServe: module is required".into()))?;
    let folders = core.folders().ok_or_else(|| {
        Error::NotServed("kith-core: this core was opened without its folders".into())
    })?;
    // The caller's view of the folders (`home`, `kernel`, `env` of the params, as `people.*` reads them) goes in the ctx:
    // a module whose sources are the caller's own files (auth.json) is asked about those; the marker stays the daemon's.
    serve_json(
        &registry(),
        &Env::with_paths(folders).with_ctx(crate::authority::call::ctx_of(core, params)?),
        core.conn(),
        name,
        opt_in_env(name),
        GUARD.get_or_init(Guard::default),
    )
}

/// `kith kernel cutover ...`: `step MODULE [--dry-run] [--force]`, `rollback MODULE [--force]`, `status`, `check MODULE`,
/// `modules`.
pub(crate) fn run(
    paths: &kith_kernel::paths::Paths,
    kernel: &std::path::Path,
    args: &[String],
) -> Result<Vec<String>> {
    let env = Env::with_paths(paths).with_ctx(crate::authority::call::ctx_from(
        paths,
        kernel.to_path_buf(),
    ));
    run_with(&registry(), &env, args)
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
