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
}

/// A started process and its pipes. `err` is only there for `Mode::Pipes`.
pub struct Started {
    pub proc: Proc,
    pub input: Box<dyn Write + Send>,
    pub out: Box<dyn Read + Send>,
    pub err: Option<Box<dyn Read + Send>>,
}

/// 32 random hex digits (instance ids, socket and pipe names).
pub fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
