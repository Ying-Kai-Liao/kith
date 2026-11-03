//! kith-core: the sealed core (docs/plans/runtime-design.md). It is the only writer of core.db and, later, the only
//! place decisions are made. Product modules call its API and never its tables; this crate's public surface is the
//! few entry points below, everything else is `pub(crate)`.
//! `kernel/tests/kith-core-boundary-check` fails when that stops being true.
//!
//! This step (M0a) is the foundation: core.db with its lifetime lock and generation, owner-only permissions, the
//! marker that refuses an empty start, and embedded mode. The method table comes next (M0b).
//!
//! Three ways in, each of which takes the lifetime lock and bumps the generation under it:
//! - [`start`]: the daemon. Checks the file, migrates an older one.
//! - [`migrate`]: the explicit migrate entry point (`kith migrate`).
//! - [`open_embedded`]: one command in its own process. Creates a fresh core.db but never migrates one that exists,
//!   and runs no claim transactions. If the lock is held it does not wait: it answers [`Embedded::DaemonRunning`]
//!   (the caller then asks the daemon). Two embedded commands at once behave the same way: the second one finds the
//!   lock held.

mod db;
mod error;
mod layout;
mod lock;
mod marker;
mod perms;

#[cfg(test)]
mod tests;

pub use db::Core;
pub use error::Error;

use kith_kernel::paths::Paths;

use db::{MIGRATIONS, Migration, Mode, Opened};
use layout::Layout;

/// What an embedded open found.
pub enum Embedded {
    /// The lock is ours; the core is open for this command. Dropping it releases the lock.
    Open(Box<Core>),
    /// A core (the daemon, or another embedded command) holds the lock. `pid` is its process when core.lock says.
    DaemonRunning { pid: Option<u32> },
}

/// What [`migrate`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Migrated {
    /// The schema core.db had, or `None` when it was just created.
    pub from: Option<i64>,
    pub to: i64,
}

/// How [`through_core`] ran a verb.
#[derive(Debug, PartialEq, Eq)]
pub enum Via {
    /// Inside an embedded core: lock held, core.db checked, generation bumped.
    Embedded,
    /// Without a core: a daemon holds the lock, there is no core.db yet, or the core would not open here.
    Bare,
}

/// Starts the daemon's core: takes the lifetime lock for good, checks the file (`quick_check`), migrates an older
/// schema after `integrity_check` and a `VACUUM INTO` snapshot, and bumps the generation.
pub fn start(paths: &Paths) -> Result<Core, Error> {
    start_with(&Layout::new(&paths.machine), MIGRATIONS)
}

/// `kith migrate`: the same as [`start`], released at once. Needs the lock, so a running daemon refuses it.
pub fn migrate(paths: &Paths) -> Result<Migrated, Error> {
    migrate_with(&Layout::new(&paths.machine), MIGRATIONS)
}

/// Opens the core inside this process for one command.
pub fn open_embedded(paths: &Paths) -> Result<Embedded, Error> {
    embedded_with(&Layout::new(&paths.machine), MIGRATIONS)
}

/// Runs a verb that reads no core state yet (paths, engines, launch) inside an embedded core, so the verbs already
/// switched to Rust go through the core's lock and checks. When no core can open here (a daemon has the lock; no
/// core.db yet; a guest's sandbox hides it) the verb runs bare: these verbs own no state of the core's, and the
/// method table of M0b replaces this with a call to the daemon.
pub fn through_core<T>(paths: &Paths, verb: impl FnOnce() -> T) -> (T, Via) {
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
        _ => (verb(), Via::Bare),
    }
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

fn busy(holder: Option<lock::Holder>) -> Error {
    let who = holder.map_or(String::new(), |h| format!(" (pid {})", h.pid));
    Error::AlreadyRunning(format!(
        "kith-core: another core{who} holds the lock; stop it first (kith serve stop)"
    ))
}
