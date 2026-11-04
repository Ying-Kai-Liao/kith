//! kith-core: the sealed core (docs/plans/runtime-design.md). It is the only writer of core.db and, later, the only
//! place decisions are made. Product modules call its API and never its tables; this crate's public surface is the
//! few entry points below, everything else is `pub(crate)`.
//! `kernel/tests/kith-core-boundary-check` fails when that stops being true.
//!
//! M0a is the foundation: core.db with its lifetime lock and generation, owner-only permissions, the marker that refuses
//! an empty start, and embedded mode. Calls go through the method table of `kith-api` ([`Core::call`]). M0c puts the
//! daemon in front ([`Daemon`]: the owner socket, the version hello, the link to the holder) and gives the CLI its way
//! in ([`reach`]).
//!
//! Three ways in, each of which takes the lifetime lock and bumps the generation under it:
//! - [`start`]: the daemon. Checks the file, migrates an older one.
//! - [`migrate`]: the explicit migrate entry point (`kith migrate`).
//! - [`open_embedded`]: one command in its own process. Creates a fresh core.db but never migrates one that exists,
//!   and runs no claim transactions. If the lock is held it does not wait: it answers [`Embedded::DaemonRunning`]
//!   (the caller then asks the daemon). Two embedded commands at once behave the same way: the second one finds the
//!   lock held.

// Nothing outside the tests calls it until the host and the CLI are switched to core.db (M2-9 onward). Unix only for
// now: the rules it reuses (`kith_kernel::secrets`) are.
#[cfg(unix)]
#[allow(dead_code)]
mod authority;
mod call;
// Nothing calls the store but the daemon's cards rows and the tests: the host's callers switch in lane H, and the raise,
// claim and close verbs get theirs there and in M2-12.
#[allow(dead_code, reason = "first callers are lane H")]
mod cards;
mod daemon;
// Nothing calls the doors but the daemon's `doors.*` rows and the tests: the host's sweep and `kith_door.py` switch later.
#[cfg(unix)]
#[allow(
    dead_code,
    reason = "first callers are the host's door code after the switch"
)]
mod doors;
// Held terminals start on Unix only for now; elsewhere the relay has nothing to relay.
#[cfg_attr(not(unix), allow(dead_code))]
mod attach;
mod db;
mod error;
mod gate;
mod holder_link;
// `kith migrate --authority`: auth.json and the Keychain into core.db and the envelope (M2-8).
#[cfg(unix)]
mod import;
mod layout;
mod lock;
mod log;
mod marker;
mod migrations;
mod owner;
mod perms;
mod runs;
mod runsock;
// Held terminals start on Unix only for now; elsewhere `terminal.open` answers `direct` and most of it is unused.
#[cfg_attr(not(unix), allow(dead_code))]
mod terminal;
// The claim path's read (`lease_held_for_non_owner`, `held_in`) and `restart` have no caller until M3 and the daemon start
// wire them; the rows reach the rest.
#[allow(dead_code)]
mod sync_lease;
// Nothing calls the envelope yet: the host vault and kith-auth move onto it at M2-7 and M2-18.
#[allow(dead_code, reason = "first callers are M2-6 and M2-7")]
mod secrets;

#[cfg(test)]
mod runs_props;
#[cfg(all(test, unix))]
mod runsock_tests;
#[cfg(test)]
mod tests;

pub use daemon::Daemon;
pub use daemon::status;
pub use db::Core;
pub use error::Error;
pub use owner::Reached;

use kith_kernel::paths::Paths;

use db::{MIGRATIONS, Migration, Mode, Opened};
use layout::Layout;

/// What an embedded open found.
pub enum Embedded {
    /// The lock is ours; the core is open for this command. Dropping it releases the lock.
    Open(Box<Core>),
    /// A core (the daemon, or another embedded command) holds the lock. `pid` is its process when core.holder says.
    DaemonRunning { pid: Option<u32> },
}

/// What [`migrate`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Migrated {
    /// The schema core.db had, or `None` when it was just created.
    pub from: Option<i64>,
    pub to: i64,
}

/// What `kith migrate --authority` may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityOptions {
    /// Print what would change and write nothing.
    pub dry_run: bool,
    /// Replace authority rows core.db already holds when they differ from auth.json (the default is to refuse).
    pub force: bool,
}

/// What [`migrate_authority`] did, one line each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthorityReport {
    pub dry_run: bool,
    /// Whether core.db (or the envelope) changed, or in a dry run would.
    pub changed: bool,
    /// The `VACUUM INTO` copy taken before the first write.
    pub backup: Option<std::path::PathBuf>,
    pub lines: Vec<String>,
}

/// How [`stateless_through_core`] ran a verb.
#[derive(Debug, PartialEq, Eq)]
pub enum Via {
    /// Inside an embedded core: lock held, core.db checked, generation bumped.
    Embedded,
    /// Without a core, for a reason that is no fault: a daemon holds the lock, or there is no core.db yet.
    Bare,
    /// Without a core because it would not open here (permissions, missing, swapped, corrupt, needs migration, binary
    /// older, I/O). The reason is one line in the core log; it is never silent.
    BareAfterError(String),
}

/// Starts the daemon's core: takes the lifetime lock for good, checks the file (`quick_check`), migrates an older
/// schema after `integrity_check` and a `VACUUM INTO` snapshot, and bumps the generation.
pub fn start(paths: &Paths) -> Result<Core, Error> {
    start_with(&Layout::new(&paths.machine), MIGRATIONS).map(|c| c.with_folders(paths))
}

/// `kith migrate`: the same as [`start`], released at once. Needs the lock, so a running daemon refuses it.
pub fn migrate(paths: &Paths) -> Result<Migrated, Error> {
    migrate_with(&Layout::new(&paths.machine), MIGRATIONS)
}

/// `kith migrate --authority`: imports auth.json's access tables into core.db and its profiles' Keychain items into the
/// secrets envelope (see `import`). Idempotent, never edits the sources, `VACUUM INTO` a backup before the first write, and
/// refuses rather than store an answer different from the file's. `ctx` says where the auth.json and the agents are.
#[cfg(unix)]
pub fn migrate_authority(
    paths: &Paths,
    ctx: &kith_kernel::secrets::Ctx,
    opts: AuthorityOptions,
) -> Result<AuthorityReport, Error> {
    import::run(
        &Layout::new(&paths.machine),
        ctx,
        opts,
        &import::items::Keychain,
        &mut import::items::OsEnvelope::default(),
    )
}

/// Opens the core inside this process for one command.
pub fn open_embedded(paths: &Paths) -> Result<Embedded, Error> {
    match embedded_with(&Layout::new(&paths.machine), MIGRATIONS)? {
        Embedded::Open(c) => Ok(Embedded::Open(Box::new(c.with_folders(paths)))),
        busy @ Embedded::DaemonRunning { .. } => Ok(busy),
    }
}

/// Points the kernel's notes (a lock or write it could not do and carried on) at the core log for the rest of this
/// process: a kernel verb that does not go through the core (`kernel accounts`) calls this itself.
pub fn route_kernel_notes(paths: &Paths) {
    log::route_kernel_notes(&paths.logs);
}

/// The holder's folder and its secret file for this machine, the secret made if it is missing. The service layer names
/// them in the job that makes the holder the supervised root (`kith-holder supervise`), so the holder that job starts is
/// the one the daemon says hello to.
pub fn holder_files(paths: &Paths) -> Result<(std::path::PathBuf, std::path::PathBuf), Error> {
    let layout = Layout::new(&paths.machine);
    perms::ensure_dir(&layout.dir)?;
    holder_link::secret(&layout)?;
    Ok((layout.holder_dir, layout.holder_secret))
}

/// Runs a verb that touches NO core state (paths, engines, launch today) inside an embedded core, so the verbs already
/// switched to Rust go through the core's lock and checks. A verb that reads or writes core state must use
/// [`open_embedded`], which refuses instead of falling back. This helper goes when M0b/M0c give those verbs the
/// method table; until then it runs the verb bare when no core can open here:
/// - quietly, when a daemon holds the lock or there is no core.db yet (nothing is wrong);
/// - with one line in `core.log` (in the logs folder, never stdout) when the open failed, so a damaged or swapped
///   core.db does not go unnoticed until the daemon refuses to start.
pub fn stateless_through_core<T>(paths: &Paths, verb: impl FnOnce() -> T) -> (T, Via) {
    // The verb's kernel rules note a lock or write they could not do (the accounts state, the settings migration) here.
    log::route_kernel_notes(&paths.logs);
    // A read-only verb never creates the core as a side effect (a sandbox's state folder stays as it was): the daemon,
    // `kith migrate` or a first embedded open that needs it makes core.db, and only then do verbs pass through it.
    if !Layout::new(&paths.machine).db.is_file() {
        return (verb(), Via::Bare);
    }
    match open_embedded(paths) {
        Ok(Embedded::Open(core)) => {
            let out = verb();
            drop(core);
            (out, Via::Embedded)
        }
        Ok(Embedded::DaemonRunning { .. }) => (verb(), Via::Bare),
        Err(e) => {
            log::fell_back(&paths.logs, &e);
            let line = e.to_string();
            (verb(), Via::BareAfterError(line))
        }
    }
}

/// Calls a method of the table on the daemon of this machine, through the owner socket. `repeatable` says the method
/// may run twice without harm (a read): a broken connection is then retried once, as a restarting daemon needs. A write
/// is never sent a second time, whatever the connection did.
///
/// Where nothing answers (no daemon, a stale socket) the answer is [`Reached::NoDaemon`] and the caller runs the verb
/// embedded as it did before; where a daemon answers but disagrees about the protocol or the method table it is
/// [`Reached::Mismatch`] with a note, and nothing has run.
pub fn reach(paths: &Paths, method: &str, params: &serde_json::Value, repeatable: bool) -> Reached {
    owner::reach(&Layout::new(&paths.machine), method, params, repeatable)
}

/// A line in the core log for the CLI's side of the front (a daemon that disagrees about the version or the method
/// table), written once while it stays the same, so `kith paths` prints nothing about it.
pub fn note_once(paths: &Paths, note: &str) {
    log::note_once(&paths.logs, "front: ", &format!("front: {note}"));
}

pub(crate) fn start_with(layout: &Layout, migrations: &[Migration]) -> Result<Core, Error> {
    match db::open(layout, migrations, Mode::Daemon)? {
        Opened::Core(c) => Ok(*c),
        Opened::Busy(h) => Err(busy(h)),
    }
}

pub(crate) fn migrate_with(layout: &Layout, migrations: &[Migration]) -> Result<Migrated, Error> {
    let core = start_with(layout, migrations)?;
    Ok(Migrated {
        from: core.migrated_from(),
        to: core.schema_version(),
    })
}

pub(crate) fn embedded_with(layout: &Layout, migrations: &[Migration]) -> Result<Embedded, Error> {
    match db::open(layout, migrations, Mode::Embedded)? {
        Opened::Core(c) => Ok(Embedded::Open(c)),
        Opened::Busy(h) => Ok(Embedded::DaemonRunning {
            pid: h.map(|h| h.pid),
        }),
    }
}

pub(crate) fn busy(holder: Option<lock::Holder>) -> Error {
    let who = holder.map_or(String::new(), |h| format!(" (pid {})", h.pid));
    Error::AlreadyRunning(format!(
        "kith-core: another core{who} holds the lock; stop it first (kith serve stop)"
    ))
}
