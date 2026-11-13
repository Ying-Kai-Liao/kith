// Adapted from herdr-core b63ecbd (crates/herdr-core/src/platform/mod.rs: the idea of one small surface for what
// differs between Unix and Windows). Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE.
//
//! What differs between Unix (Linux, macOS) and Windows, behind one small surface: connections that only this user
//! can open, terminals and pipes, process trees, detaching. Everything above it is shared.

use crate::proto::Mode;
use std::collections::BTreeMap;
use std::io::{Read, Write};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What to start.
pub struct Spec<'a> {
    pub argv: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    pub cwd: &'a str,
    pub mode: Mode,
    pub cols: u16,
    pub rows: u16,
    /// Pipes only: run `argv` under `kith-stub --resident` with a control channel (see `Ctl`). `None` runs it bare;
    /// only the platform tests do that, the holder itself refuses a pipes spawn without it.
    pub resident: Option<Resident<'a>>,
}

/// The resident stub a warm engine runs under.
pub struct Resident<'a> {
    pub stub: &'a str,
    pub lock_dir: &'a str,
}

/// The holder's end of a resident stub's control channel: lines out, lines back (core/stub/src/resident.rs). The
/// stub's end is not in the holder any more once the process is started.
pub struct Ctl {
    pub write: Box<dyn Write + Send>,
    pub read: Box<dyn Read + Send>,
}

/// Waits for a stub to open its control channel (Windows: a named pipe the holder serves, which can take seconds).
pub type AcceptCtl = Box<dyn FnOnce(&Proc) -> std::io::Result<Ctl> + Send>;

/// How a started process's control channel comes to be: already there, or still to be accepted. The accept runs
/// outside the holder's state lock.
pub enum CtlStart {
    Ready(Ctl),
    Accept(AcceptCtl),
}

/// A started process and its pipes. `err` is only there for `Mode::Pipes`.
pub struct Started {
    pub proc: Proc,
    pub input: Box<dyn Write + Send>,
    pub out: Box<dyn Read + Send>,
    pub err: Option<Box<dyn Read + Send>>,
    /// Set for a pipes process started under a resident stub.
    pub ctl: Option<CtlStart>,
}

/// 32 random hex digits (instance ids, socket and pipe names).
pub fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
