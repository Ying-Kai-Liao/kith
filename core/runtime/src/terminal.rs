//! `terminal.open` (M1-4, docs/plans/runtime-design.md section 4, "start protocol"): the core starts an agent's terminal
//! in the holder, so it survives the window that asked for it, and hands back a single-use ticket to attach with.
//!
//! What one call does, in this order (nothing is spawned unless every step before it held):
//! 1. checks the params and finds the start stub (config, then `$KITH_STUB_BIN`, then `kith-stub` beside this program);
//! 2. answers `direct` when there is no stub, the holder link is down, or the link is a command link (`ssh host
//!    kith-holder --stdio`): the stub path, the lock dir and the claim file are all on the core's machine, and a remote
//!    holder runs on another one. The caller runs the engine itself (`--direct`), as before, and nothing has been taken;
//! 3. takes the session's writer claim, today's claim file (`<state>/live/terminal/<sid>.claim`) through
//!    `kith_kernel::threads`, the kernel's port of `kith-sessions take_claim`: same file, lock file, record, liveness
//!    rule and bookkeeping (the session's pending intent and 54-minute decision end). Until the stub's pid is known the record is `kind: "starting"` and
//!    names this daemon's pid. That pid is `kith serve`, so nothing may signal it: the host's `stop_holder` answers a
//!    `starting` claim as busy, and the sweeps only touch claims that name a person. A live claim held by anyone else
//!    answers `busy` and spawns nothing;
//! 4. asks the holder, through the supervisor's connection, to spawn
//!    `<stub> --lock-dir <machine>/core/locks --key <sid> -- <argv>` on a PTY with the filtered env;
//! 5. gives the claim to the stub's pid (the held process, kind `terminal`), so it is live exactly as long as the stub
//!    runs. If that fails the held process is killed and the claim given back: the daemon's pid never stays in a claim;
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
//! ticket works once and a replay finds nothing. M1-5's attach relay calls it. A ticket carries the role it allows
//! ([`Role`]); `terminal.open` issues `owner`.
//!
//! # Before M1-5 (who owns what, so it is not found out later)
//! - Nobody acks a terminal's EXITED yet. The relay acks it after it has sent EXIT to the attached clients, and the
//!   holder's output tail is gone with the ack: a client that attaches later to an ended terminal gets nothing, so the
//!   relay (or the core's exit handling) must decide what an ended terminal keeps before it acks.
//! - `kill_on_lease_loss` is false for a terminal: it outlives its core on purpose. It MUST be true for any future guest
//!   terminal (a person's or a bot guest's), whose process must not outlive the core that vouches for it.
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
//!   ATTACH  1  JSON {v: 1, ticket, role: "owner" | "observer", cols?, rows?, takeover?}   first frame, once
//!   INPUT   2  raw bytes for the engine                                  owner only; an observer's are dropped
//!   RESIZE  3  JSON {cols, rows}                                         owner only
//!   DETACH  4  empty: leave; the engine keeps running in the holder
//! relay -> client
//!   ATTACHED 101 JSON {v: 1, held_id, role, cols, rows, replay}   the ticket was good; `replay` bytes of tail follow as OUTPUT
//!   OUTPUT   102 raw bytes of the terminal (first the holder's tail, then live)
//!   EXIT     103 JSON {code, signal, error?}                the engine ended; the relay closes after it
//!   DETACHED 104 JSON {why: "detach" | "taken_over" | "slow" | "holder_lost" | "refused"}   the relay is done with this client
//!   ERROR    105 JSON {error}                                a refused frame or ticket; the relay then closes
//! ```
//!
//! - Versions. `v` is 1 in ATTACH and ATTACHED; a relay that does not speak the client's `v` answers ERROR
//!   `unsupported_version`. The largest payload the relay sends is [`attach::MAX_PAYLOAD`] bytes: OUTPUT (and the replay)
//!   are cut into chunks that fit, and a client must accept a payload that size. Unknown kinds: the client ignores
//!   kinds of 100 and up it does not know (they are later additions); the relay answers an unknown client kind (below
//!   100) with ERROR `unknown_kind` and closes.
//! - Roles. The ticket fixes the role: ATTACH may ask for the ticket's role or a lower one (`observer` is lower than
//!   `owner`), never a higher; an observer ticket asking for `owner` is ERROR `role_not_allowed`. `owner` types and
//!   resizes; there is at most one. A second `owner` ATTACH is refused with ERROR `owner_present` unless it carries
//!   `takeover: true` and its ticket is an owner ticket, which sends the first owner DETACHED `taken_over`.
//!   `observer`s read only, any number, and never change the terminal's size.
//! - Flow control. There is one connection between the core and the holder and one backlog on it (8 MB): a relay that
//!   stopped reading would pause every engine, not just one. So the relay always drains the holder, whatever its
//!   clients do. Each attach gets a bounded buffer ([`attach::CLIENT_BUFFER`]); a client that lets it overflow (the
//!   owner included) gets DETACHED `slow` and may attach again with a new ticket, which replays the holder's tail. An
//!   engine never waits on a client. (There is no credit frame: a client that cannot keep up is cut, not waited for.)
//! - The ticket is redeemed on ATTACH and only there; a refused ticket (used, expired, unknown) is ERROR with no
//!   ATTACHED. One ticket attaches one client; a client that wants another view asks `terminal.open` or the later
//!   `terminal.attach` for a new ticket.
//! - Busy. An EXIT with code 75 right after ATTACHED means busy: another writer has this session (the start stub's own
//!   lock, held by a `--direct` run). The client says so in those words and does not retry.
//!
//! Not part of this PR: the relay and the client. Until M1-5 the supervisor discards the engine's output as it reads it
//! (a core that never reads would let it pile up in memory); the holder keeps its own 1 MB tail for the first replay.
//!
//! Later (listed in the PR): a durable claim-id counter from core.db instead of the epoch; person and agent on the claim
//! before guests; a timeout on the `ps` fallback of `process_start`; `prune_stale` of dead claims; an e2e run against
//! the real kith-stub.

use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
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
/// How long a spawn may wait in the queue: the supervisor drops a request that is older than this when it comes to it.
const SPAWN_WAIT: Duration = Duration::from_secs(20);
/// How long the caller waits for the answer: past the supervisor's worst case once it has started on the request (10 s
/// for the holder's answer, 0.6 s watching the stub), so a caller that gives up is a supervisor that is gone. If the
/// supervisor does answer late anyway, it ends the terminal it started (`serve_spawn`).
const ANSWER_WAIT: Duration = Duration::from_secs(SPAWN_WAIT.as_secs() + 15);
/// How long the core watches a new process for the stub's "busy" exit (code 75) before it calls the terminal open.
const BUSY_WAIT: Duration = Duration::from_millis(600);
/// The stub's exit code for "a writer already holds this key" (core/stub).
pub(crate) const STUB_BUSY: i32 = 75;

/// The frame kinds of an attach connection (see the module docs).
#[allow(dead_code)] // the relay (M1-5) and the client (M1-6) use them
pub(crate) mod attach {
    pub(crate) const ATTACH: u8 = 1;
    pub(crate) const INPUT: u8 = 2;
    pub(crate) const RESIZE: u8 = 3;
    pub(crate) const DETACH: u8 = 4;
    pub(crate) const ATTACHED: u8 = 101;
    pub(crate) const OUTPUT: u8 = 102;
    pub(crate) const EXIT: u8 = 103;
    pub(crate) const DETACHED: u8 = 104;
    pub(crate) const ERROR: u8 = 105;
    /// The largest payload the relay sends in one frame; OUTPUT and the replay are cut to fit.
    pub(crate) const MAX_PAYLOAD: usize = 64 * 1024;
    /// What the relay buffers for one attached client before it cuts the client with DETACHED `slow`.
    pub(crate) const CLIENT_BUFFER: usize = 1024 * 1024;
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

/// What an attach may do. Ordered: an observer is lower than an owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // M1-5's relay reads it
pub(crate) enum Role {
    Observer,
    Owner,
}

#[allow(dead_code)] // read by redeem, which the attach relay (M1-5) calls
struct Entry {
    role: Role,
    held: HeldId,
    session: String,
    agent: String,
    expires: Instant,
}

/// What a good ticket was for.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // M1-5's relay reads it
pub(crate) struct Redeemed {
    /// The most this ticket allows; the ATTACH may ask for this or lower.
    pub role: Role,
    pub held: HeldId,
    pub session: String,
    pub agent: String,
}

#[allow(dead_code)] // M1-5's relay calls it
impl Redeemed {
    /// The role an ATTACH gets for the role it asked for and whether it asked to take over: the ticket's role or a
    /// lower one, and only an owner ticket may take over. `None` is ERROR `role_not_allowed`.
    pub(crate) fn grant(&self, asked: Role, takeover: bool) -> Option<Role> {
        (asked <= self.role && (!takeover || self.role == Role::Owner)).then_some(asked)
    }
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
    pub(crate) fn issue(
        &self,
        role: Role,
        held: HeldId,
        session: &str,
        agent: &str,
    ) -> Result<String> {
        self.issue_at(Instant::now(), TICKET_TTL, role, held, session, agent)
    }

    fn issue_at(
        &self,
        now: Instant,
        ttl: Duration,
        role: Role,
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
                role,
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
            role: entry.role,
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

/// What the supervisor is asked to do with its holder connection.
pub(crate) enum Req {
    Spawn(SpawnReq),
    /// End a held process and forget it (a terminal whose claim could not be handed over).
    End(HeldId),
}

/// A spawn waiting for the supervisor, which owns the holder connection. `deadline` is when the caller stops waiting:
/// a request that sat in the queue past it (the link was down) is dropped, never run after its caller gave up.
pub(crate) struct SpawnReq {
    pub spec: proto::Spawn,
    pub deadline: Instant,
    pub busy_wait: Duration,
    pub reply: SyncSender<Placed>,
}

/// Terminals as the daemon keeps them: the queue to the supervisor, whether the link is up, the stub, the tickets.
pub(crate) struct Terminals {
    link_up: AtomicBool,
    /// The holder link is a command (`ssh host kith-holder --stdio`): the holder is on another machine.
    remote: AtomicBool,
    queue: Sender<Req>,
    inbox: Mutex<Receiver<Req>>,
    stub: Mutex<Option<PathBuf>>,
    last_claim: AtomicU64,
    pub(crate) tickets: Tickets,
    /// Tests: make handing the claim to the stub's pid fail.
    #[cfg(test)]
    pub(crate) fail_retarget: AtomicBool,
    /// Tests: how long a call waits for the supervisor's answer.
    #[cfg(test)]
    pub(crate) answer_wait: Mutex<Option<Duration>>,
}

impl Terminals {
    pub(crate) fn new() -> Self {
        let (queue, inbox) = mpsc::channel();
        Self {
            link_up: AtomicBool::new(false),
            remote: AtomicBool::new(false),
            queue,
            inbox: Mutex::new(inbox),
            stub: Mutex::new(None),
            last_claim: AtomicU64::new(0),
            tickets: Tickets::default(),
            #[cfg(test)]
            fail_retarget: AtomicBool::new(false),
            #[cfg(test)]
            answer_wait: Mutex::new(None),
        }
    }

    /// The holder link comes up or goes down (the supervisor's side). Going down answers what is still queued.
    pub(crate) fn set_link(&self, up: bool) {
        self.link_up.store(up, Ordering::SeqCst);
        if !up {
            // Spawns are answered (their callers are waiting); an `End` is kept for the next connection: the process it
            // names is still in the holder, and dropping the request would leave it running with no claim.
            let mut keep = Vec::new();
            while let Some(req) = self.next(Duration::ZERO) {
                match req {
                    Req::Spawn(req) => {
                        let _ = req
                            .reply
                            .send(Placed::Failed("the holder link went down".into()));
                    }
                    end @ Req::End(_) => keep.push(end),
                }
            }
            for end in keep {
                let _ = self.queue.send(end);
            }
        }
    }

    /// Whether the holder is reached through a command (the supervisor says so once, when it starts).
    pub(crate) fn set_remote(&self, remote: bool) {
        self.remote.store(remote, Ordering::SeqCst);
    }

    pub(crate) fn set_stub(&self, stub: Option<PathBuf>) {
        *self
            .stub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = stub;
    }

    /// The next waiting spawn, waiting up to `wait` (the supervisor's idle tick).
    pub(crate) fn next(&self, wait: Duration) -> Option<Req> {
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

    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn answer_wait(&self) -> Duration {
        #[cfg(test)]
        if let Some(d) = *self
            .answer_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return d;
        }
        ANSWER_WAIT
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
        if self.remote.load(Ordering::SeqCst) {
            return direct(
                "the holder is on another computer: its stub, lock folder and claim files are not this one's; use --direct"
                    .into(),
            );
        }
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
        match claim::take(state, &req.session, me, "starting")? {
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
        // A rendezvous: a send succeeds only if this call actually received it, so a `Started` that arrives after the caller
        // gave up fails in the supervisor, which then ends the terminal.
        let (reply, answer) = mpsc::sync_channel(0);
        let sent = self.queue.send(Req::Spawn(SpawnReq {
            spec,
            deadline: Instant::now() + SPAWN_WAIT,
            busy_wait: BUSY_WAIT,
            reply,
        }));
        let placed = match sent {
            Ok(()) => answer.recv_timeout(self.answer_wait()).ok(),
            Err(_) => None,
        };
        drop(answer);
        match placed {
            Some(Placed::Started { held, pid }) => {
                #[cfg(test)]
                let injected = self.fail_retarget.load(Ordering::SeqCst);
                #[cfg(not(test))]
                let injected = false;
                let handed = if injected {
                    Err(std::io::Error::other("injected"))
                } else {
                    claim::retarget(state, &req.session, me, pid)
                };
                if !matches!(handed, Ok(true)) {
                    // Fail closed: a terminal whose claim is not the stub's must not run, and the daemon's pid (`kith
                    // serve`) must never be left as the session's holder, where a Stop would signal it.
                    let why = handed.err().map_or_else(
                        || "the claim was taken by someone else".to_owned(),
                        |e| e.to_string(),
                    );
                    let _ = self.queue.send(Req::End(held));
                    give_back(me);
                    crate::log::note(
                        &shared.logs,
                        &format!(
                            "terminal: could not hand the claim of {} to pid {pid} ({why}); the terminal was ended",
                            req.session
                        ),
                    );
                    return Err(Error::Io(format!(
                        "kith-core: could not give the session's claim to the terminal ({why}); it was ended"
                    )));
                }
                let ticket =
                    self.tickets
                        .issue(Role::Owner, held.clone(), &req.session, &req.agent)?;
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

// --- today's claim file --------------------------------------------------------------------------------------------

#[cfg(unix)]
mod claim {
    //! `<state>/live/terminal/<sid>.claim`, through the kernel's port of `kith-sessions` (`kith_kernel::threads`): the same
    //! file, lock file, record, liveness rule (only ESRCH means a claimer is gone) and bookkeeping (a taker ends the
    //! session's pending intent and 54-minute decision), so a `--direct` run, the host and a held terminal all see one
    //! writer. This is only the core's view of it.

    use std::io;
    use std::path::Path;

    use kith_kernel::threads::{self, Ctx, Held as Holding, Take};

    pub(super) struct Held {
        pub pid: u32,
        pub kind: String,
    }

    fn ctx(state: &Path) -> Ctx<'static> {
        Ctx {
            state: Some(state.to_path_buf()),
            machine: None,
            home: std::path::PathBuf::new(),
            env: &|_| None,
        }
    }

    /// Takes the claim for `pid`, recorded as `kind`. `Ok(Err(held))`: a live claim is there.
    pub(super) fn take(
        state: &Path,
        sid: &str,
        pid: u32,
        kind: &str,
    ) -> io::Result<Result<(), Held>> {
        let taken = threads::take_claim(&ctx(state), sid, i64::from(pid), &Take::new(kind))?;
        Ok(taken.map_err(|Holding(c)| Held {
            pid: c.pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0),
            kind: c.kind,
        }))
    }

    /// Hands a claim this daemon took for itself to the stub's pid. `Ok(false)`: it is no longer ours.
    pub(super) fn retarget(state: &Path, sid: &str, from: u32, to: u32) -> io::Result<bool> {
        threads::retarget(&ctx(state), sid, i64::from(from), i64::from(to), "terminal")
    }

    /// Gives the claim back if `pid` still holds it.
    pub(super) fn release(state: &Path, sid: &str, pid: u32) {
        threads::release_one(&ctx(state), sid, i64::from(pid), false);
    }
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
