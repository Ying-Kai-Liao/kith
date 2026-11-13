// Message shapes follow herdr-core b63ecbd crates/herdr-core/src/holder_proto.rs (Hello, Spawn, Exit, Welcome).
// Copyright herdrdev and the herdr-core contributors, Apache-2.0. See NOTICE. The generation fencing, the secret,
// the claim and the kill and ack answers are Kith's own.
//
//! What a core and the holder say to each other. The holder never interprets `conv` or `claim`: it stores them so
//! the core can match its records against `list` after a restart.
//!
//! The claims map is only a filter. The holder remembers the highest claim per conversation to refuse an old
//! decision cheaply, but the map dies with the holder; the real "is this still the current claim" check is the start
//! stub redeeming at the core. Once a claim's job has started (not when it failed to start, so a retry after
//! `SPAWN_FAILED` works) any spawn at or below the highest claim is refused, even after the process was acked.
//!
//! An error exit (`Exit::error`) says the holder could not read how the process ended. It does not say the group is
//! empty: the core still waits for KILL `Gone` before it releases the claim.
//!
//! A warm engine (`Mode::Pipes`) always runs under `kith-stub --resident` (docs/plans/runtime-design.md section 4): the
//! core names the stub and the lock folder in the spawn, and without them the spawn is `SPAWN_FAILED` with an install
//! hint (no fallback to a bare engine). The holder keeps the control channel to that stub; `TAKE` and `RELEASE` relay a
//! turn's writer lock through it and are answered with `LOCK_ANSWER`, always naming the claim. The holder acts only for
//! the process's current claim (a take below it, or for a claim it already released, is an `Error`; a release of
//! another claim is `NotHeld` and relays nothing), and keeps its answers per claim and kind until the core says
//! `LOCK_ACK`, so a busy for one claim never replaces an unacked answer of another. The lock itself belongs to the stub
//! and the OS: a holder crash frees nothing, a stub crash frees it.
//!
//! The lease (docs/plans/runtime-design.md section 10, modelled in verification/lease). A hand that cannot reach its
//! core for a lease period kills its guest and bot runs itself, so a revoke is never stranded on a cut-off hand. The
//! core says which processes those are when it spawns them (`Spawn::kill_on_lease_loss`; the default, false, is an
//! owner run and is never touched). The deadline is anchored on the hand's own clock, at a moment before the core
//! sent the renewal: the core sends `PING(seq)`, the hand answers `PONG(seq)` and remembers its clock reading for that
//! seq (the newest few, forgotten at every hello), and the core's `RENEW(seq, ms)` makes the deadline that reading
//! plus `ms`. So however long a RENEW is delayed or sits unread, the deadline is never later than the core's send plus
//! `ms`. A RENEW for a seq the hand never answered, or whose deadline is already past, arms nothing. The clock is
//! one that counts sleep (`clock`), so a hand that slept through its deadline finds the lease lapsed the moment it
//! wakes, and its first step is the expiry. The holder tracks one deadline, renewed only by the newest generation's
//! connection (an older one is refused as `Superseded` before its frame counts). A period is capped at ten minutes.
//! No renewal has ever armed a lease: nothing expires, but a tagged spawn is refused (`LEASE_LOST`, `Unarmed`), so a
//! tagged run never starts without a lease that will end it, and a local holder no core ever leased never kills
//! anything. The deadline outlives a dropped connection: a core that drops and does not come back is exactly the
//! cut-off case. When the deadline passes, in one step under the holder's state lock, the lease is marked lost and
//! every tagged process still running has its whole tree killed; their exits are kept until acked like any exit, and
//! say `lapsed`. While lost or unarmed, a tagged spawn is answered `LEASE_LOST` (`Lapsed` or `Unarmed`). A valid renewal
//! from the newest core restores a lost lease (the model's `fixed`; safe only because of the ping anchor); the welcome
//! of a later connection says `lease_lost`. A holder and core on different `VERSION`s refuse each other at hello.
//!
//! Process ids. A frame id names a process only together with the holder instance (`Welcome::instance`): the
//! payload of KILL, ACK, TAIL, RESIZE and INPUT begins with the 32 character instance, then the body (INPUT: the raw
//! bytes, RESIZE: the JSON, the others: nothing). An id from another instance (a holder that was replaced and
//! whose ids restart at 1) never touches a local process: KILL answers `NotHere`, ACK `NotHere`, TAIL is empty,
//! INPUT and RESIZE are ignored. `NotHere` is not `Gone`: this holder cannot see the groups of an older instance
//! (they outlive a holder crash), so the core kills those itself by pid and start time. The core names a process
//! with a `HeldId` (instance and id), made from a spawn's answer or a listing entry, never from the holder it
//! happens to be connected to.
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
pub const VERSION: u32 = 5;

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
/// The core's lease renewal (payload `Renew`); the holder answers nothing. See `Spawn::kill_on_lease_loss`.
pub const RENEW: u8 = 11;
/// The core's ping (payload `Ping`): the holder answers `PONG` with the same seq and remembers its own clock for it.
pub const PING: u8 = 12;
/// Take a conversation's writer lock for a warm process's turn (payload: the instance, then `LockReq`). The holder
/// relays it to that process's resident stub and answers `LOCK_ANSWER`. The frame id is the process.
pub const TAKE: u8 = 13;
/// Release it again at turn end (payload: the instance, then `LockReq`).
pub const RELEASE: u8 = 14;
/// The core has the answer for a claim (payload: the instance, then `LockAcked`): the holder forgets that report.
pub const LOCK_ACK: u8 = 15;

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
/// A spawn whose claim is below the highest claim the holder has seen for that conversation: the core that sent it
/// is working from an old decision. Payload `StaleClaim`; nothing was started.
pub const STALE_CLAIM: u8 = 113;
/// A spawn tagged `kill_on_lease_loss` while the lease is lost. Payload `LeaseLost`; nothing was started, and the
/// claim is not used up, so the same spawn works once a renewal restores the lease.
pub const LEASE_LOST: u8 = 114;
/// The answer to `PING`. Payload `Pong`.
pub const PONG: u8 = 115;
/// The answer to `TAKE` or `RELEASE`. Payload `LockAnswer`; the frame id is the process.
pub const LOCK_ANSWER: u8 = 116;

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
    /// Kill this process's tree if the lease lapses (a guest's or a bot's run). Default false: an owner run.
    #[serde(default)]
    pub kill_on_lease_loss: bool,
    /// Pipes only: the `kith-stub` program every warm engine runs under (`kith-stub --resident`). A pipes spawn
    /// without it is `SPAWN_FAILED`: the holder never starts an engine that nothing guards (docs/plans/runtime-design.md
    /// section 4). Ignored for a terminal, whose argv already begins with its own start stub.
    #[serde(default)]
    pub stub: Option<String>,
    /// Pipes only: the folder of the conversation lock files (the stub's `--lock-dir`).
    #[serde(default)]
    pub lock_dir: Option<String>,
}

/// A question to the hand: "what is your clock now?". The answer is `Pong` with the same seq.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Ping {
    pub seq: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Pong {
    pub seq: u64,
}

/// How long the lease lasts, counted from the moment the holder answered PONG `seq` (not from when it reads this).
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Renew {
    pub ms: u64,
    pub seq: u64,
}

/// Why a tagged spawn was refused.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeaseWhy {
    /// No renewal has armed a lease on this holder.
    Unarmed,
    /// The lease ran out (a guest run was or would have been killed).
    Lapsed,
}

#[derive(Serialize, Deserialize)]
pub struct LeaseLost {
    pub conv: String,
    pub claim: u64,
    pub why: LeaseWhy,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// Set when the holder could not read how the process ended (code and signal are then both None): an error
    /// exit, kept until acked like any other so the core still learns the process is over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The holder killed this run because the lease lapsed (a guest or bot run), not the program or a KILL.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lapsed: bool,
}

/// A process as the core must remember it: the holder instance that owns it, and its id there.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct HeldId {
    pub instance: String,
    pub id: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Held {
    /// The holder instance this entry came from.
    #[serde(default)]
    pub instance: String,
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
    /// The writer-lock answers not yet acked (`LOCK_ACK`), one entry per claim and kind, so a core that missed an
    /// answer finds it here after a reconnect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locks: Vec<LockAnswer>,
}

#[derive(Serialize, Deserialize)]
pub struct Welcome {
    pub version: u32,
    /// Random per holder process: a different one means the old holder is gone.
    pub instance: String,
    pub pid: u32,
    /// Windows only: the holder is in any job (the starter's job would not let it break away), so the starter's
    /// supervisor can end it by ending the job (the daemon ending), which is the one thing it exists to survive. The core reports it (`kith doctor`).
    /// Always false on Unix, where the holder has its own session.
    #[serde(default)]
    pub tied_to_starter: bool,
    pub generation: u64,
    /// The lease is lost right now: guest runs were killed by it, or a renewal has not restored it. The exits of
    /// those runs (`Exit::lapsed`) are in `procs` until acked.
    #[serde(default)]
    pub lease_lost: bool,
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
    /// The holder instance and the process id there: the only way, with a listing, to get a `HeldId`.
    #[serde(default)]
    pub instance: String,
    #[serde(default)]
    pub id: u32,
}

impl Spawned {
    pub fn held_id(&self) -> HeldId {
        HeldId {
            instance: self.instance.clone(),
            id: self.id,
        }
    }
}

impl Held {
    pub fn held_id(&self) -> HeldId {
        HeldId {
            instance: self.instance.clone(),
            id: self.id,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct SpawnFailed {
    pub conv: String,
    pub claim: u64,
    pub error: String,
}

#[derive(Serialize, Deserialize)]
pub struct StaleClaim {
    pub conv: String,
    pub claim: u64,
    /// The highest claim the holder has seen for this conversation.
    pub highest: u64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum KillState {
    /// The kill was sent; some process of the tree may still be alive. Send it again.
    Killing,
    /// Nothing of the tree is left, or the id was never known (or already acked).
    Gone,
    /// The id belongs to another holder instance. Not "gone": that instance's groups may still be alive.
    NotHere,
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
    /// The id belongs to another holder instance.
    NotHere,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Acked {
    pub state: AckState,
}

#[derive(Serialize, Deserialize)]
pub struct Listing {
    pub procs: Vec<Held>,
}

/// What a core asks of a warm process's resident stub: the lock of one conversation for one turn.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LockReq {
    /// The lock key (`a-z 0-9 . _ -`, as the stub checks it). Opaque to the holder apart from that.
    pub key: String,
    /// The turn's claim id. The holder acts only for the process's current claim: a take for an older one is an
    /// `Error`, and so is a take for a claim that was already released (the stub cannot know a claim is finished).
    pub claim: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LockOp {
    Take,
    Release,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockWord {
    Locked,
    /// Another writer holds the conversation (a terminal's stub, a `--direct` run), or this stub serves another turn.
    Busy,
    Released,
    /// The stub did not hold that claim's lock (never taken, already released, or another claim's).
    NotHeld,
    /// Nothing was done: the stub could not say (`reason`), the process or its stub is gone, or the claim is stale.
    Error,
}

/// The holder's answer to a `TAKE` or `RELEASE`. Always names the claim it is for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LockAnswer {
    #[serde(default)]
    pub instance: String,
    pub id: u32,
    pub op: LockOp,
    pub claim: u64,
    pub word: LockWord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LockAcked {
    pub claim: u64,
}
