// Message shapes follow herdr-core b63ecbd crates/herdr-core/src/holder_proto.rs (Hello, Spawn, Exit, Welcome).
// Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE. The generation fencing, the secret,
// the claim and the kill and ack answers are Kith's own.
//
//! What a core and the holder say to each other. The holder never interprets `conv` or `claim`: it stores them so
//! the core can match its records against `list` after a restart.
//!
//! What "gone" means. A held process is the leader of its own process group on Unix (its own job on Windows), and
//! `Killed::Gone` says that group or job is empty. Unix limit: a descendant that starts its own session (`setsid`)
//! or double-forks into another group leaves the group, so "gone" means the group, not everything the program ever
//! started (the hyperframes door showed this: a detached ffmpeg and Chrome outlived it). Linux could later claim
//! such strays with `PR_SET_CHILD_SUBREAPER`; macOS has no equivalent. Windows has no such escape: the job holds
//! every descendant and ends them when it closes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Checked in hello and welcome. Bumped for a change an older peer cannot read.
pub const VERSION: u32 = 1;

// core to holder
pub const HELLO: u8 = 1;
pub const SPAWN: u8 = 2;
/// Raw bytes to the process. For a pipes process an empty payload closes its stdin.
pub const INPUT: u8 = 3;
pub const RESIZE: u8 = 4;
pub const KILL: u8 = 5;
pub const ACK: u8 = 6;
pub const LIST: u8 = 7;
pub const TAIL: u8 = 8;
/// Kill everything and exit. Not an update path: it ends live processes.
pub const SHUTDOWN: u8 = 9;
/// The core's answer to the holder's challenge (see `auth`).
pub const AUTH: u8 = 10;

// holder to core
pub const WELCOME: u8 = 101;
pub const REFUSED: u8 = 102;
pub const SPAWNED: u8 = 103;
pub const SPAWN_FAILED: u8 = 104;
/// A pty's output, or a pipes process's stdout.
pub const OUTPUT: u8 = 105;
/// A pipes process's stderr.
pub const STDERR: u8 = 106;
pub const EXITED: u8 = 107;
pub const KILLED: u8 = 108;
pub const LISTING: u8 = 109;
pub const TAILED: u8 = 110;
pub const ACKED: u8 = 111;
/// The holder's nonce and its proof that it knows the secret, sent after a hello.
pub const CHALLENGE: u8 = 112;

/// The first frame on every connection. The secret is an opaque key the holder was started with; it will be the
/// local hand's pairing key (docs/plans/runtime-design.md section 10), and it never crosses the wire: the holder
/// answers with a challenge and the core with an `Auth` (see `auth`). `generation` is the core's generation from
/// the core's database; `nonce` is random hex, fresh per connection.
#[derive(Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub generation: u64,
    pub nonce: String,
}

/// The holder's half: its own nonce, and its proof over both nonces, so the core knows who it is talking to.
#[derive(Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: String,
    pub proof: String,
}

/// The core's proof over the same exchange. Only after it checks out does the holder say anything about itself.
#[derive(Serialize, Deserialize)]
pub struct Auth {
    pub proof: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// On a pseudo terminal (a ConPTY on Windows): output is one stream, input is typed.
    Pty,
    /// Plain pipes (an ACP process): stdout and stderr apart, stdin closable.
    Pipes,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Spawn {
    /// The conversation, as the core names it. Opaque here.
    pub conv: String,
    /// The core's claim id. A spawn repeated with the same conv and claim answers with the process already running.
    pub claim: u64,
    /// The core generation this spawn was decided under. Must equal the connection's, which must be the newest.
    pub generation: u64,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub mode: Mode,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    80
}

fn default_rows() -> u16 {
    24
}

#[derive(Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Held {
    pub id: u32,
    pub conv: String,
    pub claim: u64,
    pub generation: u64,
    pub pid: u32,
    pub mode: Mode,
    pub cols: u16,
    pub rows: u16,
    /// Set once it has ended; it stays until acked.
    pub exit: Option<Exit>,
}

#[derive(Serialize, Deserialize)]
pub struct Welcome {
    pub version: u32,
    /// Random per holder process: a different one means the old holder is gone.
    pub instance: String,
    pub pid: u32,
    /// Windows only: the starter's job would not let the holder leave it, so it dies when the starter's job closes
    /// (the daemon ending), which is the one thing it exists to survive. The core reports it (`kith doctor`).
    /// Always false on Unix, where the holder has its own session.
    #[serde(default)]
    pub tied_to_starter: bool,
    pub generation: u64,
    pub procs: Vec<Held>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    BadVersion,
    /// The core's proof did not match: it does not hold the secret.
    BadSecret,
    /// A newer core has already said hello. `current` has its generation.
    StaleGeneration,
    NoHello,
    /// A command from a connection that is no longer the newest.
    Superseded,
    /// The holder is exiting; start a new one.
    Closing,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Refused {
    pub why: Why,
    pub current: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Spawned {
    pub conv: String,
    pub claim: u64,
    pub pid: u32,
}

#[derive(Serialize, Deserialize)]
pub struct SpawnFailed {
    pub conv: String,
    pub claim: u64,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum KillState {
    /// The kill was sent; some process of the tree may still be alive. Send it again.
    Killing,
    /// Nothing of the tree is left, or the id was never known (or already acked).
    Gone,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Killed {
    pub state: KillState,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AckState {
    Acked,
    /// It has not ended yet: nothing to ack.
    Running,
    /// Unknown, or acked before.
    Gone,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Acked {
    pub state: AckState,
}

#[derive(Serialize, Deserialize)]
pub struct Listing {
    pub procs: Vec<Held>,
}
