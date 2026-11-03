//! `terminal.open` (M1-4, docs/plans/runtime-design.md section 4, "start protocol"): the core starts an agent's terminal
//! in the holder, so it survives the window that asked for it, and hands back a single-use ticket to attach with.
//!
//! What one call does, in this order (nothing is spawned unless every step before it held):
//! 1. checks the params and finds the start stub (config, then `$KITH_STUB_BIN`, then `kith-stub` beside this program);
//! 2. answers `direct` when there is no stub or the holder link is down: the caller runs the engine itself (`--direct`),
//!    exactly as it did before, and nothing has been taken;
//! 3. takes the session's writer claim, today's claim file (`<state>/live/terminal/<sid>.claim`, a port of
//!    `kith-sessions take_claim`: same file, same lock file, same record), naming this daemon until the stub is known;
//!    a live claim held by anyone else answers `busy` and spawns nothing;
//! 4. asks the holder, through the supervisor's connection, to spawn
//!    `<stub> --lock-dir <machine>/core/locks --key <sid> -- <argv>` on a PTY with the filtered env;
//! 5. gives the claim to the stub's pid (the held process), so it is live exactly as long as the stub runs;
//! 6. answers `{status: "open", ticket, held_id, ...}`.
//!
//! A spawn that fails, a stub that reports busy (exit 75: a `--direct` writer holds the stub's own lock) or an engine
//! that ends at once gives the claim back. Two calls for one session at once: the claim file lets one win; the other
//! answers `busy`.
//!
//! The env filter. The caller sends the assembled launch env; the core drops what must never reach a terminal that
//! lives on after its caller is gone: the run-token and ask-token variables and their sockets (`serve/host/src/runauth.rs`
//! `SCRUB`: `KITH_RUN_TOKEN`, `KITH_RUN_SOCKET`, `KITH_ASK_TOKEN`, `KITH_ASK_SOCKET` and the `HQ_` spellings) and every
//! `KITH_RUN_*` name. The names come from the host's list, not a new one: a guest's token in an owner's long-lived
//! terminal would outlive the run it was issued for. Provider keys the engine logs in with are the launch's business and
//! are kept; adding a name pattern for them would break engines that read a key from the env.
//!
//! Tickets. 32 random bytes as hex; only their SHA-256 is kept, in memory, with the held process id and an expiry
//! ([`TICKET_TTL`]). [`Terminals::redeem`] removes the entry on the first lookup whether or not it has expired, so a
//! ticket works once and a replay finds nothing. M1-5's attach relay calls it.
//!
//! # Attach frames (the spec M1-5, the relay, and M1-6, the client, build to)
//!
//! One attach is one connection from a client to the relay (the owner socket's attach listener, M1-5). Frames use the
//! holder's layout, `kith_holder::wire` (`[kind u8][id u32 LE][len u32 LE][payload]`), with `id` always 0: a connection
//! is one terminal. Control payloads are JSON, terminal bytes are raw. Kinds below 100 go client to relay, 100 and up
//! the other way ([`attach`] has the numbers).
//!
//! ```text
//! client -> relay
//!   ATTACH  1  JSON {ticket, role: "owner" | "observer", cols?, rows?, credit?, takeover?}   first frame, once
//!   INPUT   2  raw bytes for the engine                                  owner only; an observer's are dropped
//!   RESIZE  3  JSON {cols, rows}                                         owner only
//!   DETACH  4  empty: leave; the engine keeps running in the holder
//!   CREDIT  5  u32 LE: the client will take this many more OUTPUT bytes (flow control)
//! relay -> client
//!   ATTACHED 101 JSON {held_id, role, cols, rows, replay}   the ticket was good; `replay` bytes of tail follow as OUTPUT
//!   OUTPUT   102 raw bytes of the terminal (first the holder's tail, then live)
//!   EXIT     103 JSON {code, signal, error?}                the engine ended; the relay closes after it
//!   DETACHED 104 JSON {why: "detach" | "taken_over" | "holder_lost" | "refused"}     the relay is done with this client
//!   ERROR    105 JSON {error}                                a refused frame or ticket; the relay then closes
//! ```
//!
//! - Roles. `owner` types and resizes; there is at most one. A second `owner` ATTACH is refused with ERROR
//!   `owner_present` unless it carries `takeover: true`, which sends the first owner DETACHED `taken_over`. `observer`s
//!   read only, any number, and never change the terminal's size.
//! - Flow control. OUTPUT is sent only against credit: the client opens with `credit` in ATTACH (default
//!   [`attach::DEFAULT_CREDIT`]) and grants more with CREDIT as it consumes. A relay out of credit stops reading for
//!   that client; the holder's own backlog then slows the engine, so nothing is dropped and nothing grows without
//!   bound. An observer that stays out of credit longer than the relay allows is DETACHED, never the owner.
//! - The ticket is redeemed on ATTACH and only there; a refused ticket (used, expired, unknown) is ERROR with no
//!   ATTACHED. One ticket attaches one client; a client that wants another view asks `terminal.open` or the later
//!   `terminal.attach` for a new ticket.
//!
//! Not part of this PR: the relay and the client. Until M1-5 the supervisor discards the engine's output as it reads it
//! (a core that never reads would let it pile up in memory); the holder keeps its own 1 MB tail for the first replay.

use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kith_holder::proto::{self, HeldId};
use serde_json::{Value, json};

use crate::daemon::Shared;
use crate::error::{Error, Result};
use crate::holder_link::HolderConfig;
#[cfg(unix)]
use crate::layout::Layout;

/// How long a ticket from `terminal.open` can be redeemed.
pub(crate) const TICKET_TTL: Duration = Duration::from_secs(60);
/// The most tickets waiting at once; an older unredeemed one is dropped to make room (a client that asks and never
/// attaches must not grow the table).
const MAX_TICKETS: usize = 256;
/// How long the supervisor may take to answer a spawn before the call gives up and says `direct`.
const SPAWN_WAIT: Duration = Duration::from_secs(20);
/// How long the core watches a new process for the stub's "busy" exit (code 75) before it calls the terminal open.
const BUSY_WAIT: Duration = Duration::from_millis(600);
/// The stub's exit code for "a writer already holds this key" (core/stub).
pub(crate) const STUB_BUSY: i32 = 75;

/// The frame kinds of an attach connection (see the module docs).
#[allow(dead_code)] // the relay (M1-5) and the client (M1-6) use them
pub(crate) mod attach {
    pub const ATTACH: u8 = 1;
    pub const INPUT: u8 = 2;
    pub const RESIZE: u8 = 3;
    pub const DETACH: u8 = 4;
    pub const CREDIT: u8 = 5;
    pub const ATTACHED: u8 = 101;
    pub const OUTPUT: u8 = 102;
    pub const EXIT: u8 = 103;
    pub const DETACHED: u8 = 104;
    pub const ERROR: u8 = 105;
    /// Output bytes a client may be sent before it grants more, when its ATTACH names none.
    pub const DEFAULT_CREDIT: u32 = 64 * 1024;
}

// --- the filter ----------------------------------------------------------------------------------------------------

/// The names `serve/host/src/runauth.rs` scrubs for tokens and their sockets.
const SCRUBBED: [&str; 6] = [
    "KITH_RUN_TOKEN",
    "KITH_RUN_SOCKET",
    "KITH_ASK_TOKEN",
    "KITH_ASK_SOCKET",
    "HQ_ASK_TOKEN",
    "HQ_ASK_SOCKET",
];

/// The env a terminal is started with: the caller's, less what the module docs name.
pub(crate) fn filter_env(env: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, _)| !SCRUBBED.contains(&k.as_str()) && !k.starts_with("KITH_RUN_"))
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect()
}

// --- tickets -------------------------------------------------------------------------------------------------------

#[allow(dead_code)] // read by redeem, which the attach relay (M1-5) calls
struct Entry {
    held: HeldId,
    session: String,
    agent: String,
    expires: Instant,
}

/// What a good ticket was for.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // M1-5's relay reads it
pub(crate) struct Redeemed {
    pub held: HeldId,
    pub session: String,
    pub agent: String,
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // M1-5's relay reads it
pub(crate) enum TicketRefused {
    /// Never issued, or already used (the two look the same, on purpose).
    Unknown,
    Expired,
}

#[derive(Default)]
pub(crate) struct Tickets {
    by_hash: Mutex<HashMap<String, Entry>>,
}

fn hash(ticket: &str) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(ticket.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl Tickets {
    pub(crate) fn issue(&self, held: HeldId, session: &str, agent: &str) -> Result<String> {
        self.issue_at(Instant::now(), TICKET_TTL, held, session, agent)
    }

    fn issue_at(
        &self,
        now: Instant,
        ttl: Duration,
        held: HeldId,
        session: &str,
        agent: &str,
    ) -> Result<String> {
        let ticket = crate::owner::random_secret()?;
        let mut map = self
            .by_hash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, e| e.expires > now);
        while map.len() >= MAX_TICKETS {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, e)| e.expires)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
        map.insert(
            hash(&ticket),
            Entry {
                held,
                session: session.to_owned(),
                agent: agent.to_owned(),
                expires: now + ttl,
            },
        );
        Ok(ticket)
    }

    /// Uses the ticket up. A second call, an unknown string and an expired ticket are all refused; the first call
    /// removes the entry whatever it finds.
    #[allow(dead_code)] // M1-5's relay calls it
    pub(crate) fn redeem(&self, ticket: &str) -> std::result::Result<Redeemed, TicketRefused> {
        self.redeem_at(Instant::now(), ticket)
    }

    #[allow(dead_code)]
    fn redeem_at(
        &self,
        now: Instant,
        ticket: &str,
    ) -> std::result::Result<Redeemed, TicketRefused> {
        let entry = self
            .by_hash
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&hash(ticket))
            .ok_or(TicketRefused::Unknown)?;
        if entry.expires <= now {
            return Err(TicketRefused::Expired);
        }
        Ok(Redeemed {
            held: entry.held,
            session: entry.session,
            agent: entry.agent,
        })
    }
}

// --- the call to the holder ----------------------------------------------------------------------------------------

/// What the supervisor found when it put a spawn to the holder.
pub(crate) enum Placed {
    Started {
        held: HeldId,
        pid: u32,
    },
    /// The stub ran and exited 75: a writer already holds the stub's lock for this key.
    Busy,
    /// The holder would not start it (`SPAWN_FAILED`), or it ended at once; the text says why.
    Failed(String),
    /// The holder has seen a higher claim for this conversation.
    Stale,
}

/// A spawn waiting for the supervisor, which owns the holder connection. `deadline` is when the caller stops waiting:
/// a request that sat in the queue past it (the link was down) is dropped, never run after its caller gave up.
pub(crate) struct SpawnReq {
    pub spec: proto::Spawn,
    pub deadline: Instant,
    pub busy_wait: Duration,
    pub reply: Sender<Placed>,
}

/// Terminals as the daemon keeps them: the queue to the supervisor, whether the link is up, the stub, the tickets.
pub(crate) struct Terminals {
    link_up: AtomicBool,
    queue: Sender<SpawnReq>,
    inbox: Mutex<Receiver<SpawnReq>>,
    stub: Mutex<Option<PathBuf>>,
    last_claim: AtomicU64,
    pub(crate) tickets: Tickets,
}

impl Terminals {
    pub(crate) fn new() -> Self {
        let (queue, inbox) = mpsc::channel();
        Self {
            link_up: AtomicBool::new(false),
            queue,
            inbox: Mutex::new(inbox),
            stub: Mutex::new(None),
            last_claim: AtomicU64::new(0),
            tickets: Tickets::default(),
        }
    }

    /// The holder link comes up or goes down (the supervisor's side). Going down answers what is still queued.
    pub(crate) fn set_link(&self, up: bool) {
        self.link_up.store(up, Ordering::SeqCst);
        if !up {
            while let Some(req) = self.next(Duration::ZERO) {
                let _ = req
                    .reply
                    .send(Placed::Failed("the holder link went down".into()));
            }
        }
    }

    pub(crate) fn set_stub(&self, stub: Option<PathBuf>) {
        *self
            .stub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = stub;
    }

    /// The next waiting spawn, waiting up to `wait` (the supervisor's idle tick).
    pub(crate) fn next(&self, wait: Duration) -> Option<SpawnReq> {
        let inbox = self
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inbox.recv_timeout(wait).ok()
    }

    #[allow(dead_code)] // M1-5's relay calls it
    pub(crate) fn redeem(&self, ticket: &str) -> std::result::Result<Redeemed, TicketRefused> {
        self.tickets.redeem(ticket)
    }

    /// A claim id that rises with every spawn, across daemon restarts: the holder refuses a claim at or below the
    /// highest it has seen for a conversation. Milliseconds since the epoch, pushed past the last one given.
    fn next_claim(&self) -> u64 {
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(u64::MAX);
        let mut last = self.last_claim.load(Ordering::SeqCst);
        loop {
            let next = now.max(last + 1);
            match self
                .last_claim
                .compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(seen) => last = seen,
            }
        }
    }

    fn stub_path(&self) -> Option<PathBuf> {
        let configured = self
            .stub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        HolderConfig::find_stub(configured)
    }

    /// The `terminal.open` method.
    pub(crate) fn open(&self, shared: &Shared, params: &Value) -> Result<Value> {
        let row = kith_api::method("terminal.open")
            .ok_or_else(|| Error::NoSuchMethod("kith-core: no method terminal.open".into()))?;
        kith_api::params::validate(row, params)
            .map_err(|e| Error::BadParams(format!("kith-core: {e}")))?;
        let req = OpenParams::read(params)?;
        let paths = shared.with_core(|c| {
            c.folders().cloned().ok_or_else(|| {
                Error::NotServed("kith-core: this core was opened without its folders".into())
            })
        })?;
        let direct = |note: String| Ok(json!({"status": "direct", "note": note}));
        let Some(stub) = self.stub_path() else {
            return direct(
                "no kith-stub program to run the engine under (looked for $KITH_STUB_BIN and beside kith): use --direct"
                    .into(),
            );
        };
        if !self.link_up.load(Ordering::SeqCst) {
            return direct("the holder is not reachable: use --direct".into());
        }
        #[cfg(not(unix))]
        {
            let _ = (&req, &stub, &paths);
            direct("held terminals are not on this system yet: use --direct".into())
        }
        #[cfg(unix)]
        {
            self.open_held(
                shared,
                &req,
                &stub,
                &paths.state,
                &Layout::new(&paths.machine),
            )
        }
    }

    #[cfg(unix)]
    fn open_held(
        &self,
        shared: &Shared,
        req: &OpenParams,
        stub: &Path,
        state: &Path,
        layout: &Layout,
    ) -> Result<Value> {
        let me = std::process::id();
        match claim::take(state, &req.session, me)? {
            Ok(()) => {}
            Err(held) => {
                return Ok(json!({
                    "status": "busy",
                    "note": format!("session {} is being written by pid {} ({})", req.session, held.pid, held.kind),
                    "held_by": {"pid": held.pid, "kind": held.kind},
                }));
            }
        }
        // From here every way out but the open one gives the claim back.
        let give_back = |pid: u32| claim::release(state, &req.session, pid);
        let locks = layout.dir.join("locks");
        if let Err(e) = crate::perms::ensure_dir(&locks) {
            give_back(me);
            return Err(e);
        }
        let generation = match shared.with_core(|c| Ok(c.generation())) {
            Ok(g) => u64::try_from(g).unwrap_or(0),
            Err(e) => {
                give_back(me);
                return Err(e);
            }
        };
        let mut argv = vec![
            stub.to_string_lossy().into_owned(),
            "--lock-dir".into(),
            locks.to_string_lossy().into_owned(),
            "--key".into(),
            req.session.clone(),
            "--".into(),
        ];
        argv.extend(req.argv.iter().cloned());
        let spec = proto::Spawn {
            conv: req.session.clone(),
            claim: self.next_claim(),
            generation,
            argv,
            env: filter_env(&req.env),
            cwd: req.cwd.clone(),
            mode: proto::Mode::Pty,
            cols: req.cols,
            rows: req.rows,
            // A terminal outlives its core by design; the lease would end it.
            kill_on_lease_loss: false,
        };
        let (reply, answer) = mpsc::channel();
        let sent = self.queue.send(SpawnReq {
            spec,
            deadline: Instant::now() + SPAWN_WAIT,
            busy_wait: BUSY_WAIT,
            reply,
        });
        let placed = match sent {
            Ok(()) => answer
                .recv_timeout(SPAWN_WAIT + Duration::from_secs(2))
                .ok(),
            Err(_) => None,
        };
        match placed {
            Some(Placed::Started { held, pid }) => {
                if let Err(e) = claim::retarget(state, &req.session, me, pid) {
                    // The claim stays with this daemon's pid: still live, still ours, only not tied to the stub.
                    crate::log::note(
                        &shared.logs,
                        &format!(
                            "terminal: could not hand the claim of {} to pid {pid}: {e}",
                            req.session
                        ),
                    );
                }
                let ticket = self.tickets.issue(held.clone(), &req.session, &req.agent)?;
                Ok(json!({
                    "status": "open",
                    "ticket": ticket,
                    "expires_in": TICKET_TTL.as_secs(),
                    "held_id": {"instance": held.instance, "id": held.id},
                    "pid": pid,
                    "session": req.session,
                    "agent": req.agent,
                    "cols": req.cols,
                    "rows": req.rows,
                }))
            }
            Some(Placed::Busy) => {
                give_back(me);
                Ok(json!({
                    "status": "busy",
                    "note": format!("another writer holds session {}'s lock (a --direct run)", req.session),
                }))
            }
            Some(Placed::Stale) => {
                give_back(me);
                Err(Error::StaleGeneration(
                    "kith-core: the holder has seen a newer claim for this session; try again"
                        .into(),
                ))
            }
            Some(Placed::Failed(why)) => {
                give_back(me);
                Err(Error::Io(format!(
                    "kith-core: the holder could not start the terminal: {why}"
                )))
            }
            None => {
                give_back(me);
                Ok(json!({
                    "status": "direct",
                    "note": "the holder did not answer in time: use --direct",
                }))
            }
        }
    }
}

struct OpenParams {
    agent: String,
    session: String,
    argv: Vec<String>,
    cwd: String,
    env: serde_json::Map<String, Value>,
    cols: u16,
    rows: u16,
}

/// A session id is a file name in the claim folder: letters, digits, `-`, `_`, and no leading dot.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

impl OpenParams {
    fn read(p: &Value) -> Result<Self> {
        let bad = |m: &str| Error::BadParams(format!("kith-core: terminal.open: {m}"));
        let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_owned);
        let session = text("session").ok_or_else(|| bad("missing session"))?;
        if !safe_id(&session) {
            return Err(bad("session is not a session id"));
        }
        let argv: Vec<String> = p
            .get("argv")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if argv.is_empty() || argv.iter().any(|a| a.contains('\0')) {
            return Err(bad("argv needs the engine's command"));
        }
        let size = |k: &str, d: u16| {
            p.get(k)
                .and_then(Value::as_u64)
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| *n > 0)
                .unwrap_or(d)
        };
        Ok(Self {
            agent: text("agent").ok_or_else(|| bad("missing agent"))?,
            session,
            argv,
            cwd: text("cwd").ok_or_else(|| bad("missing cwd"))?,
            env: p
                .get("env")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            cols: size("cols", 80),
            rows: size("rows", 24),
        })
    }
}

// --- today's claim file (a port of kith-sessions take_claim) --------------------------------------------------------

#[cfg(unix)]
mod claim {
    //! `<state>/live/terminal/<sid>.claim`: `{"pid","procStart","at","kind"}`, made whole in a temp file and linked into
    //! place (exclusive) under `<sid>.claim.lock` (flock), a stale one removed under that lock. The same file, lock and
    //! record as `kernel/bin/kith-sessions` and the host's `terminal/claim.rs`, so a `--direct` run, the host and a
    //! held terminal all see one writer. Not ported: ending the session's intent and pending decision (the taker's
    //! bookkeeping in `take_claim`); a held terminal's start does not touch them yet.

    use std::fs::{self, OpenOptions};
    use std::io::{self, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::{Path, PathBuf};

    use fs2::FileExt as _;
    use serde_json::{Value, json};

    use crate::lock::process_start;

    pub(super) struct Held {
        pub pid: u32,
        pub kind: String,
    }

    fn dir(state: &Path) -> PathBuf {
        state.join("live").join("terminal")
    }

    fn squeeze(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn read(path: &Path) -> Option<Value> {
        let v: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
        v.is_object().then_some(v)
    }

    /// The claimer runs and started when the record says (a reused pid started at another time is not it).
    fn live(c: &Value) -> bool {
        let Some(pid) = c
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
        else {
            return false;
        };
        let Some(actual) = process_start(pid) else {
            return false;
        };
        match c
            .get("procStart")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(claimed) => squeeze(claimed) == squeeze(&actual),
            None => true,
        }
    }

    fn held_by(c: &Value) -> Held {
        Held {
            pid: c
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
                .unwrap_or(0),
            kind: c
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("terminal")
                .to_owned(),
        }
    }

    /// `YYYY-MM-DDTHH:MM:SSZ`, as `kith-sessions now()` writes it.
    fn stamp() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (days, rest) = (i64::try_from(secs / 86400).unwrap_or(0), secs % 86400);
        // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            rest % 3600 / 60,
            rest % 60
        )
    }

    fn record(pid: u32) -> String {
        json!({
            "pid": pid,
            "procStart": process_start(pid).unwrap_or_default(),
            "at": stamp(),
            "kind": "terminal",
        })
        .to_string()
    }

    fn unique() -> u64 {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn lock(parent: &Path, sid: &str) -> io::Result<fs::File> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(parent.join(format!("{sid}.claim.lock")))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn write_new(path: &Path, text: &str) -> io::Result<()> {
        let _ = fs::remove_file(path);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(text.as_bytes())
    }

    fn make_dir(parent: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
    }

    /// Takes the claim for `pid`. `Ok(Err(held))`: a live claim is there.
    pub(super) fn take(state: &Path, sid: &str, pid: u32) -> io::Result<Result<(), Held>> {
        let parent = dir(state);
        make_dir(&parent)?;
        let dest = parent.join(format!("{sid}.claim"));
        // Unique per call: two takers in this process at once must not share a temp file.
        let tmp = parent.join(format!(
            ".{sid}.claim.{}.{}.tmp",
            std::process::id(),
            unique()
        ));
        write_new(&tmp, &record(pid))?;
        let result = (|| {
            let _lock = lock(&parent, sid)?;
            for _ in 0..3 {
                match fs::hard_link(&tmp, &dest) {
                    Ok(()) => return Ok(Ok(())),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e),
                }
                if let Some(seen) = read(&dest).filter(live) {
                    return Ok(Err(held_by(&seen)));
                }
                // Stale or garbage: nobody else judges it while we hold the lock.
                let _ = fs::remove_file(&dest);
            }
            Ok(Err(read(&dest).map_or(
                Held {
                    pid,
                    kind: "terminal".into(),
                },
                |c| held_by(&c),
            )))
        })();
        let _ = fs::remove_file(&tmp);
        result
    }

    /// Hands a claim this daemon took for itself to the stub's pid, if it is still ours.
    pub(super) fn retarget(state: &Path, sid: &str, from: u32, to: u32) -> io::Result<()> {
        let parent = dir(state);
        let dest = parent.join(format!("{sid}.claim"));
        let _lock = lock(&parent, sid)?;
        if read(&dest).is_some_and(|c| held_by(&c).pid == from) {
            let tmp = parent.join(format!(
                ".{sid}.claim.{}.{}.retarget",
                std::process::id(),
                unique()
            ));
            write_new(&tmp, &record(to))?;
            fs::rename(&tmp, &dest)?;
        }
        Ok(())
    }

    /// Gives the claim back if `pid` still holds it.
    pub(super) fn release(state: &Path, sid: &str, pid: u32) {
        let parent = dir(state);
        let Ok(_lock) = lock(&parent, sid) else {
            return;
        };
        let dest = parent.join(format!("{sid}.claim"));
        if read(&dest).is_some_and(|c| held_by(&c).pid == pid) {
            let _ = fs::remove_file(&dest);
        }
    }
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
