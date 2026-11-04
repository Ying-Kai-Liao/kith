//! The frame kinds of an attach connection: a client (`kith attach`, the app) to the core's attach relay. The layout is
//! [`crate::wire`]'s, with the frame `id` always 0 (a connection is one terminal). The rules that go with the numbers
//! (roles, flow control, the order of replay and live output) are in `core/runtime/src/terminal.rs`'s and
//! `attach.rs`'s module docs; this file is only what both ends must agree on, kept here so a client does not link the
//! core. Kinds below 100 go client to relay, 100 and up the other way.

pub const ATTACH: u8 = 1;
pub const INPUT: u8 = 2;
pub const RESIZE: u8 = 3;
pub const DETACH: u8 = 4;
pub const ATTACHED: u8 = 101;
pub const OUTPUT: u8 = 102;
pub const EXIT: u8 = 103;
pub const DETACHED: u8 = 104;
pub const ERROR: u8 = 105;

/// The version of the ATTACH and ATTACHED payloads (`v`).
pub const VERSION: u32 = 1;
/// The largest payload the relay sends in one frame; OUTPUT and the replay are cut to fit, and a client accepts a
/// payload that size.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// What the relay buffers for one attached client before it cuts the client with DETACHED `slow`.
pub const CLIENT_BUFFER: usize = 1024 * 1024;
