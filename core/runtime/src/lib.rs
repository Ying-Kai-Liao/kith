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

mod call;
mod daemon;
mod db;
mod error;
mod gate;
mod holder_link;
mod layout;
mod lock;
mod log;
mod marker;
mod migrations;
mod owner;
mod perms;
mod runs;
mod runsock;

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
