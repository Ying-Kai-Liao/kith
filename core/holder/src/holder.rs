// Adapted from herdr-core b63ecbd (crates/herdr-core/src/holder.rs: the shape of the holder, its per-core writer
// thread and backlog, the pump and reap threads, the 1 MB output tail). Copyright herdrdev and the herdr-core
// contributors, Apache-2.0. See NOTICE. Everything about who may do what (the secret, generation fencing, the
// claim, kill and ack answers) is Kith's own; herdr's core restarting, scopes and sweeping are not taken.
//
//! The holder: owns every agent process, is its parent, always reads its output, keeps a tail and the unacked
//! exits. No parsing, no policy: it checks a secret and a generation and does what the newest core says.

use crate::auth;
use crate::clock::{self, Clock};
use crate::platform::{self, Proc, Resident, Spec};
use crate::proto::{self as p, Held, KillState, Refused, Why};
use crate::wire;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The longest lease one renewal can grant (ten minutes).
const MAX_LEASE_MS: u64 = 10 * 60 * 1000;
const TAIL_BYTES: usize = 1 << 20;
/// How long an exit waits for the last output of the process before it is reported anyway.
const EXIT_DRAIN: Duration = Duration::from_millis(500);
/// Output queued for a core that reads slowly before programs have to wait for it.
const BACKLOG_BYTES: usize = 8 << 20;
/// Temporary accept failures in a row before the holder gives up, and its first and longest pause between tries
/// (about a minute in all).
const ACCEPT_RETRIES: u32 = 70;
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);
const ACCEPT_PAUSE_CAP: Duration = Duration::from_secs(1);
/// What a failed start of the stub says to do about it.
const STUB_HINT: &str =
    "Install kith-stub next to kith (reinstall Kith), or set KITH_STUB_BIN to it.";

#[derive(Clone)]
pub struct Config {
    /// The holder's folder (private): its socket and log.
    pub dir: PathBuf,
    /// The opaque secret every hello must carry (the local hand's pairing key).
    pub secret: String,
    /// Exit after this long with no core and nothing held. Zero: never.
    pub idle: Duration,
}

struct Entry {
    held: Held,
    proc: Arc<Proc>,
    /// Input goes through its own thread: a program that stops reading blocks only itself. None once closed.
    input: Option<Sender<Vec<u8>>>,
    tail: VecDeque<u8>,
    /// Output streams still being read.
    readers: u8,
    /// A kill has seen the tree empty. Sticks: the group id may be handed to an unrelated group afterwards (the blind
    /// spot of `Proc::tree_empty`), and a later kill must neither say "killing" nor signal that group.
    gone: AtomicBool,
    /// Killed by the holder itself when the lease lapses.
    kill_on_lease_loss: bool,
    /// The lease killed it (the exit then says `lapsed`).
    lapsed: bool,
    /// Lines to the resident stub's control channel (written by a thread of its own, so a stub busy on a disk never
    /// holds the state lock). None for a terminal, and once the stub or the channel is gone.
    ctl: Option<Sender<String>>,
    /// The turn this process serves now (see `Turn`).
    turn: Turn,
    /// Requests sent to the stub and not answered yet, in order: the stub answers one line at a time, in order.
    pending: VecDeque<(p::LockOp, u64, String)>,
    /// Answers for the core, kept until it acks the claim (`LOCK_ACK`), one per claim and kind: a busy for one claim
    /// never replaces a report of another, and a later take of a claim never hides its release.
    reports: BTreeMap<(u64, p::LockOp), p::LockAnswer>,
    /// The highest claim a take was relayed for, whatever the answer: a take below it is stale even when that one came
    /// back busy (a mistaken high claim can still be refused by the stub, but it must not let an older take through).
    requested: u64,
}

/// The claim a warm process serves now: the spawn's until a take is answered `locked`, then that take's. The holder
/// relays a take only for the current claim or a newer one, and never for a claim it has already released (the stub
/// cannot know a claim is finished: that is the core's redeem). A take that came back busy or failed moves nothing,
/// so the lock the stub may still hold for the current claim can always be released.
///
/// `released` is the highest claim a release was accepted for, set when the release is relayed (not when it is
/// answered) and never lowered: a take of that claim, or any older one, is not relayed again. A take still in flight
/// when its release arrives is covered the same way, and a late `locked` cannot undo it.
#[derive(Clone, Copy)]
struct Turn {
    claim: u64,
    released: Option<u64>,
}

/// How many unacked lock reports (and requests in flight) one process may have. Past it a new take is refused with
/// `too-many-unacked`: a kept answer is never evicted, and a core that never acks cannot grow the holder without bound.
const MAX_REPORTS: usize = 256;

/// How strong an answer is when two are kept for one (claim, kind): what happened beats a refusal, a refusal beats an
/// error. A later weaker answer (a retried take that came back as an error after the first one locked) must not hide
/// that the turn ran.
fn strength(word: p::LockWord) -> u8 {
    match word {
        p::LockWord::Locked | p::LockWord::Released => 2,
        p::LockWord::Busy | p::LockWord::NotHeld => 1,
        p::LockWord::Error => 0,
    }
}

/// The stub's own rules for a lock key, checked here too because the key is written into a line: a space or a line
/// break in it would be a second command.
fn lock_key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 200
        && !key.contains("..")
        && key
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

/// The lease the newest core keeps alive. See `proto` for the rules.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Lease {
    /// No renewal has ever arrived: nothing expires.
    Unarmed,
    /// On the lease clock (`clock`, which counts sleep), not on `Instant`.
    Until(Duration),
    Lost,
}

/// How many PONG readings the hand keeps for a RENEW to name. The core pings every third of the lease and echoes
/// the seq at once, so a few is plenty; an older seq arms nothing.
const PONG_RING: usize = 8;

/// The connected core. Frames go out through its own writer thread: a core that stops reading must never block
/// anyone holding the state lock.
struct Core {
    conn: u64,
    frames: Sender<Vec<u8>>,
    backlog: Arc<Backlog>,
    /// Shut down to free a writer stuck on a core that is gone.
    stream: platform::Stream,
}

/// Bytes queued for one core; None once its writer stopped.
struct Backlog {
    bytes: Mutex<Option<usize>>,
    changed: Condvar,
}

impl Backlog {
    fn update(&self, f: impl FnOnce(&mut Option<usize>)) {
        f(&mut self.bytes.lock().unwrap_or_else(|p| p.into_inner()));
        self.changed.notify_all();
    }

    fn over(&self, limit: usize) -> bool {
        self.bytes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some_and(|n| n > limit)
    }

    /// Waits (never under the state lock) until at most `limit` bytes are queued or the writer stopped.
    fn wait(&self, limit: usize) {
        let bytes = self.bytes.lock().unwrap_or_else(|p| p.into_inner());
        drop(
            self.changed
                .wait_while(bytes, |b| b.is_some_and(|n| n > limit)),
        );
    }
}

struct State {
    procs: HashMap<u32, Entry>,
    /// The highest claim seen per conversation, kept after the process is acked: a spawn at or below it is stale.
    claims: HashMap<String, u64>,
    next_id: u32,
    /// The newest generation any core has said hello with. Never goes down.
    newest: u64,
    conns: u64,
    core: Option<Core>,
    /// Set under the lock by whoever decides the holder exits; hello and spawn refuse from then on, so nothing
    /// can start between that decision and the exit.
    closing: bool,
    idle_since: Option<Instant>,
    lease: Lease,
    /// The lease clock at each PONG sent to the current core, newest last. Forgotten at every hello: a seq from an
    /// older connection must not anchor a newer one (the skill's Later item L1).
    pongs: VecDeque<(u64, Duration)>,
}

struct Holder {
    state: Mutex<State>,
    /// Signals: an output stream ended, or an exit was recorded.
    changed: Condvar,
    instance: String,
    secret: Vec<u8>,
    endpoint: PathBuf,
    idle: Duration,
    /// The lease clock; the tests move their own.
    clock: Clock,
}

/// Runs a holder in `cfg.dir` until it is idle or told to shut down.
pub fn run(cfg: Config) -> io::Result<()> {
    run_with(cfg, || {})
}

/// Like `run`, and calls `serving` once, when this holder owns the folder and its socket is listening (so a hello
/// would be answered). A supervisor starts its child from there.
pub fn run_with(cfg: Config, serving: impl FnOnce()) -> io::Result<()> {
    if cfg.secret.len() < 16 {
        return Err(io::Error::other(
            "the holder secret must be at least 16 characters",
        ));
    }
    platform::private_dir(&cfg.dir)?;
    // Before the socket is touched: with the lock held, a socket file nobody answers on is a crash's leftover.
    let Some(_instance) = platform::lock_instance(&cfg.dir)? else {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "a holder already runs here",
        ));
    };
    let endpoint = platform::endpoint(&cfg.dir);
    let listener = match platform::listen(&endpoint) {
        Ok(l) => l,
        Err(e)
            if e.kind() == io::ErrorKind::AddrInUse
                || e.kind() == io::ErrorKind::PermissionDenied =>
        {
            // A live holder answers; a socket file left by a crash does not.
            if platform::connect(&endpoint).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "a holder already runs here",
                ));
            }
            platform::remove_endpoint(&endpoint);
            platform::listen(&endpoint)?
        }
        Err(e) => return Err(e),
    };
    let holder = Arc::new(Holder::new(cfg.secret.into_bytes(), endpoint, cfg.idle));
    serving();
    let watcher = holder.clone();
    std::thread::spawn(move || watcher.watch_idle());
    let watcher = holder.clone();
    std::thread::spawn(move || watcher.watch_lease());
    let gave_up = holder.accept_loop(
        || platform::accept(&listener),
        ACCEPT_RETRIES,
        ACCEPT_BACKOFF,
    );
    // Giving up must not orphan what is held: close down like a SHUTDOWN does, then leave.
    let st = holder.close_down(holder.lock());
    drop(st);
    eprintln!("holder: no longer accepting connections: {gave_up}");
    holder.exit()
}

impl Holder {
    /// Serves each accepted connection on its own thread. A temporary accept failure (out of file descriptors, a
    /// connection that vanished) is retried with a doubling, capped pause; only `retries` failures in a row, or one
    /// that is not temporary, end the loop, returning the last error.
    fn accept_loop(
        self: &Arc<Self>,
        mut accept: impl FnMut() -> io::Result<platform::Stream>,
        retries: u32,
        first_pause: Duration,
    ) -> io::Error {
        let mut failures = 0u32;
        let mut pause = first_pause;
        loop {
            let stream = match accept() {
                Ok(stream) => {
                    failures = 0;
                    pause = first_pause;
                    stream
                }
                Err(e) if platform::accept_error_is_temporary(&e) && failures < retries => {
                    failures += 1;
                    eprintln!("holder: accept: {e} (retrying)");
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(ACCEPT_PAUSE_CAP);
                    continue;
                }
                Err(e) => return e,
            };
            let holder = self.clone();
            std::thread::spawn(move || {
                // A caller that hangs up (a liveness probe, a core that died) is not worth a log line.
                let quiet = [
                    io::ErrorKind::BrokenPipe,
                    io::ErrorKind::UnexpectedEof,
                    io::ErrorKind::ConnectionReset,
                ];
                if let Err(e) = holder.serve(stream)
                    && !quiet.contains(&e.kind())
                {
                    eprintln!("holder: connection: {e}");
                }
            });
        }
    }

    /// Refuses new work, kills every held tree and waits (briefly) for them to empty. The caller leaves while
    /// still holding the returned guard, so nothing starts in between.
    fn close_down<'a>(&'a self, mut st: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        st.closing = true;
        for e in st.procs.values() {
            e.proc.kill_tree();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while st.procs.values().any(|e| !e.proc.tree_empty()) && Instant::now() < deadline {
            drop(st);
            std::thread::sleep(Duration::from_millis(20));
            st = self.lock();
        }
        st
    }

    fn new(secret: Vec<u8>, endpoint: PathBuf, idle: Duration) -> Holder {
        Holder {
            state: Mutex::new(State {
                procs: HashMap::new(),
                claims: HashMap::new(),
                next_id: 1,
                newest: 0,
                conns: 0,
                core: None,
                closing: false,
                idle_since: Some(Instant::now()),
                lease: Lease::Unarmed,
                pongs: VecDeque::new(),
            }),
            changed: Condvar::new(),
            instance: platform::random_hex(),
            secret,
            endpoint,
            idle,
            clock: clock::lease_clock(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn exit(&self) -> ! {
        platform::remove_endpoint(&self.endpoint);
        std::process::exit(0);
    }

    fn watch_idle(&self) {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let mut st = self.lock();
            if !self.idle.is_zero()
                && st.core.is_none()
                && st.procs.is_empty()
                && st.idle_since.is_some_and(|t| t.elapsed() >= self.idle)
            {
                // The lock stays held through the exit: a hello or spawn cannot land in between.
                st.closing = true;
                self.exit();
            }
        }
    }

    /// Expiry and the kill are one step under the state lock: a later kill would be a window in which a cut-off
    /// hand still runs a revoked guest. Callers hold the lock and call this before acting on anything, so a
    /// renewal or spawn that arrives as the lease lapses sees the lapse first. While lost it keeps killing, since
    /// a tree that was slow to die must not be left alone.
    fn expire_lease(&self, st: &mut State) {
        if let Lease::Until(t) = st.lease
            && (self.clock)() >= t
        {
            st.lease = Lease::Lost;
        }
        if st.lease == Lease::Lost {
            for e in st.procs.values_mut() {
                if e.kill_on_lease_loss && e.held.exit.is_none() && !e.gone.load(Ordering::Relaxed)
                {
                    e.lapsed = true;
                    e.proc.kill_tree();
                }
            }
        }
    }

    fn watch_lease(&self) {
        loop {
            std::thread::sleep(Duration::from_millis(10));
            let mut st = self.lock();
            if st.closing {
                return;
            }
            self.expire_lease(&mut st);
        }
    }

    /// Queues a frame for the current core.
    fn to_core(&self, st: &State, frame: Vec<u8>) {
        if let Some(core) = &st.core {
            core.backlog.update(|b| *b = b.map(|n| n + frame.len()));
            let _ = core.frames.send(frame);
        }
    }

    fn drop_core(&self, st: &mut State) {
        if let Some(core) = st.core.take() {
            let _ = core.stream.shutdown(std::net::Shutdown::Both);
            core.backlog.update(|b| *b = None);
        }
        st.idle_since = Some(Instant::now());
    }

    /// Writes one core's frames in order; a failed write means that core is gone.
    fn write_to_core(
        &self,
        conn: u64,
        mut stream: platform::Stream,
        frames: Receiver<Vec<u8>>,
        backlog: Arc<Backlog>,
    ) {
        for frame in frames {
            if stream.write_all(&frame).is_err() {
                break;
            }
            backlog.update(|b| *b = b.map(|n| n.saturating_sub(frame.len())));
        }
        backlog.update(|b| *b = None);
        let mut st = self.lock();
        if st.core.as_ref().is_some_and(|c| c.conn == conn) {
            self.drop_core(&mut st);
        }
    }

    fn refuse(stream: &mut platform::Stream, why: Why, current: u64) -> io::Result<()> {
        wire::write_json(stream, p::REFUSED, 0, &Refused { why, current })
    }

    fn held_list(st: &State) -> Vec<Held> {
        let mut procs: Vec<Held> = st
            .procs
            .values()
            .map(|e| Held {
                locks: e.reports.values().cloned().collect(),
                ..e.held.clone()
            })
            .collect();
        procs.sort_by_key(|h| h.id);
        procs
    }

    fn serve(self: &Arc<Self>, mut stream: platform::Stream) -> io::Result<()> {
        let hello: p::Hello = match wire::read(&mut stream)? {
            Some((p::HELLO, _, payload)) => wire::parse(&payload)?,
            // Nothing is known about this peer yet, so nothing about the holder is told to it.
            _ => return Self::refuse(&mut stream, Why::NoHello, 0),
        };
        if hello.version != p::VERSION {
            return Self::refuse(&mut stream, Why::BadVersion, 0);
        }
        if self.lock().closing {
            return Self::refuse(&mut stream, Why::Closing, 0);
        }
        // The holder proves the key first, then the core does; only then does the holder say what it holds.
        let nonce = platform::random_hex();
        let challenge = p::Challenge {
            proof: auth::proof(
                &self.secret,
                auth::HOLDER,
                &hello.nonce,
                &nonce,
                hello.generation,
            ),
            nonce,
        };
        wire::write_json(&mut stream, p::CHALLENGE, 0, &challenge)?;
        let answer: p::Auth = match wire::read(&mut stream)? {
            Some((p::AUTH, _, payload)) => wire::parse(&payload)?,
            _ => return Self::refuse(&mut stream, Why::NoHello, 0),
        };
        let expect = auth::proof(
            &self.secret,
            auth::CORE,
            &hello.nonce,
            &challenge.nonce,
            hello.generation,
        );
        if !auth::same(expect.as_bytes(), answer.proof.as_bytes()) {
            return Self::refuse(&mut stream, Why::BadSecret, 0);
        }
        let conn = {
            let mut st = self.lock();
            if st.closing {
                return Self::refuse(&mut stream, Why::Closing, st.newest);
            }
            // The same generation may come back (a dropped connection); an older one may not.
            if hello.generation < st.newest {
                return Self::refuse(&mut stream, Why::StaleGeneration, st.newest);
            }
            self.drop_core(&mut st);
            st.newest = hello.generation;
            st.conns += 1;
            st.idle_since = None;
            st.pongs.clear();
            let conn = st.conns;
            let (frames, queued) = std::sync::mpsc::channel();
            let backlog = Arc::new(Backlog {
                bytes: Mutex::new(Some(0)),
                changed: Condvar::new(),
            });
            let (writer, kept) = (stream.try_clone()?, stream.try_clone()?);
            let (holder, written) = (self.clone(), backlog.clone());
            std::thread::spawn(move || holder.write_to_core(conn, writer, queued, written));
            st.core = Some(Core {
                conn,
                frames,
                backlog,
                stream: kept,
            });
            let welcome = p::Welcome {
                version: p::VERSION,
                instance: self.instance.clone(),
                pid: std::process::id(),
                tied_to_starter: platform::tied_to_starter(),
                generation: st.newest,
                lease_lost: st.lease == Lease::Lost,
                procs: Self::held_list(&st),
            };
            self.to_core(&st, wire::json_frame(p::WELCOME, 0, &welcome));
            conn
        };
        let result = self.commands(&mut stream, conn, hello.generation);
        let mut st = self.lock();
        if st.core.as_ref().is_some_and(|c| c.conn == conn) {
            self.drop_core(&mut st);
        }
        result
    }

    fn commands(
        self: &Arc<Self>,
        stream: &mut platform::Stream,
        conn: u64,
        generation: u64,
    ) -> io::Result<()> {
        while let Some((kind, id, payload)) = wire::read(stream)? {
            let mut st = self.lock();
            // A newer core said hello: this one is done, and nothing it sends counts any more.
            if st.core.as_ref().is_none_or(|c| c.conn != conn) {
                let current = st.newest;
                drop(st);
                return Self::refuse(stream, Why::Superseded, current);
            }
            self.expire_lease(&mut st);
            match kind {
                p::PING => {
                    let ping: p::Ping = wire::parse(&payload)?;
                    // The anchor is read here, before the PONG leaves, so the deadline a RENEW makes is this clock plus
                    // the period: at or after the core's send plus the period. Safe because the core's belief only
                    // starts at the PONG receipt, later than this read.
                    let now = (self.clock)();
                    if st.pongs.len() == PONG_RING {
                        st.pongs.pop_front();
                    }
                    st.pongs.push_back((ping.seq, now));
                    self.to_core(
                        &st,
                        wire::json_frame(p::PONG, 0, &p::Pong { seq: ping.seq }),
                    );
                }
                p::RENEW => {
                    let renew: p::Renew = wire::parse(&payload)?;
                    // A seq the hand never answered, a zero period, or a deadline already past (a stale RENEW read
                    // after a wake) arms nothing. A huge period must neither panic this thread nor switch the kill
                    // off: it is capped.
                    let anchor = st
                        .pongs
                        .iter()
                        .find(|(s, _)| *s == renew.seq)
                        .map(|(_, t)| *t);
                    let period = Duration::from_millis(renew.ms.min(MAX_LEASE_MS));
                    if let Some(deadline) = anchor
                        .and_then(|t| t.checked_add(period))
                        .filter(|d| renew.ms > 0 && *d > (self.clock)())
                    {
                        st.lease = Lease::Until(deadline);
                    }
                }
                p::SPAWN => {
                    let spec: p::Spawn = wire::parse(&payload)?;
                    if st.closing {
                        let current = st.newest;
                        let refused = Refused {
                            why: Why::Closing,
                            current,
                        };
                        self.to_core(&st, wire::json_frame(p::REFUSED, 0, &refused));
                        continue;
                    }
                    if spec.generation != generation || generation != st.newest {
                        let current = st.newest;
                        self.to_core(
                            &st,
                            wire::json_frame(
                                p::REFUSED,
                                0,
                                &Refused {
                                    why: Why::StaleGeneration,
                                    current,
                                },
                            ),
                        );
                        continue;
                    }
                    self.spawn(st, spec);
                }
                p::INPUT => {
                    let body = self.body_for(&payload);
                    if let Some(e) = body.and_then(|_| st.procs.get_mut(&id)) {
                        let payload = body.unwrap_or_default();
                        if payload.is_empty() {
                            e.input = None;
                        } else if let Some(tx) = &e.input {
                            let _ = tx.send(payload.to_vec());
                        }
                    }
                }
                p::RESIZE => {
                    let Some(body) = self.body_for(&payload) else {
                        continue;
                    };
                    let size: p::Size = wire::parse(body)?;
                    if let Some(e) = st.procs.get_mut(&id).filter(|e| e.held.exit.is_none()) {
                        e.held.cols = size.cols;
                        e.held.rows = size.rows;
                        e.proc.resize(size.cols, size.rows);
                    }
                }
                p::KILL => {
                    let state = if self.body_for(&payload).is_some() {
                        self.kill(&st, id)
                    } else {
                        KillState::NotHere
                    };
                    self.to_core(&st, wire::json_frame(p::KILLED, id, &p::Killed { state }));
                }
                p::ACK => {
                    let here = self.body_for(&payload).is_some();
                    let known = st.procs.get(&id).map(|e| e.held.exit.is_some());
                    let state = match known.filter(|_| here) {
                        None if !here => p::AckState::NotHere,
                        None => p::AckState::Gone,
                        Some(false) => p::AckState::Running,
                        Some(true) => {
                            st.procs.remove(&id);
                            if st.procs.is_empty() && st.core.is_none() {
                                st.idle_since = Some(Instant::now());
                            }
                            p::AckState::Acked
                        }
                    };
                    self.to_core(&st, wire::json_frame(p::ACKED, id, &p::Acked { state }));
                }
                p::LIST => {
                    let listing = p::Listing {
                        procs: Self::held_list(&st),
                    };
                    self.to_core(&st, wire::json_frame(p::LISTING, 0, &listing));
                }
                p::TAIL => {
                    let tail = self
                        .body_for(&payload)
                        .and_then(|_| st.procs.get(&id))
                        .map(|e| e.tail.iter().copied().collect::<Vec<u8>>())
                        .unwrap_or_default();
                    self.to_core(&st, wire::frame(p::TAILED, id, &tail));
                }
                p::TAKE | p::RELEASE => {
                    // The instance is a fixed 32 characters, so the request can be read (and answered, with its
                    // claim) even when it names another holder's process.
                    let here = self.body_for(&payload).is_some();
                    let req: p::LockReq =
                        wire::parse(payload.get(self.instance.len()..).unwrap_or_default())?;
                    let op = if kind == p::TAKE {
                        p::LockOp::Take
                    } else {
                        p::LockOp::Release
                    };
                    if here {
                        self.lock_request(&mut st, op, id, &req);
                    } else {
                        self.refuse_lock(&st, id, op, req.claim, p::LockWord::Error, "not-here");
                    }
                }
                p::LOCK_ACK => {
                    if let Some(body) = self.body_for(&payload) {
                        let acked: p::LockAcked = wire::parse(body)?;
                        if let Some(e) = st.procs.get_mut(&id) {
                            e.reports.remove(&(acked.claim, p::LockOp::Take));
                            e.reports.remove(&(acked.claim, p::LockOp::Release));
                        }
                    }
                }
                p::SHUTDOWN => {
                    let _st = self.close_down(st);
                    // Still holding the lock: nothing can start between the wait and the exit.
                    self.exit();
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// What follows the instance at the start of a payload, or None when the payload names another holder (or none):
    /// then the frame id means nothing here.
    fn body_for<'a>(&self, payload: &'a [u8]) -> Option<&'a [u8]> {
        payload.strip_prefix(self.instance.as_bytes())
    }

    /// Idempotent: kills the whole tree and says "gone" only once nothing of it is left. A kill for an id never
    /// held (or already acked) says "gone" too, so a core can resend until it hears that.
    fn kill(&self, st: &State, id: u32) -> KillState {
        let Some(e) = st.procs.get(&id) else {
            return KillState::Gone;
        };
        // An ended process had its tree swept before its exit was recorded, but "gone" still means the group or job
        // is empty: a member that is slow to die keeps the answer at "killing". (After the leader is reaped, Unix
        // sends no more signals, since its group id could be recycled; the sweep's SIGKILL is already on its way.)
        if e.gone.load(Ordering::Relaxed) {
            return KillState::Gone;
        }
        e.proc.kill_tree();
        if e.proc.tree_empty() {
            e.gone.store(true, Ordering::Relaxed);
            KillState::Gone
        } else {
            KillState::Killing
        }
    }

    /// Why a spawn must not start, as the frame that tells the core: the same start again (answered, not run twice), a
    /// lease that would not end a tagged run, or a claim decided before. Asked again after the Windows stub wait,
    /// which runs outside the state lock.
    fn spawn_refusal(&self, st: &State, spec: &p::Spawn) -> Option<Vec<u8>> {
        // The same spawn again (the core did not hear the first answer) is answered, not run twice.
        if let Some(e) = st
            .procs
            .values()
            .find(|e| e.held.conv == spec.conv && e.held.claim == spec.claim)
        {
            return Some(wire::json_frame(
                p::SPAWNED,
                e.held.id,
                &p::Spawned {
                    conv: spec.conv.clone(),
                    claim: spec.claim,
                    pid: e.held.pid,
                    instance: self.instance.clone(),
                    id: e.held.id,
                },
            ));
        }
        // Unarmed counts as lost here: a tagged run must never start without a lease that will end it.
        if spec.kill_on_lease_loss && !matches!(st.lease, Lease::Until(_)) {
            let lost = p::LeaseLost {
                conv: spec.conv.clone(),
                claim: spec.claim,
                why: if st.lease == Lease::Lost {
                    p::LeaseWhy::Lapsed
                } else {
                    p::LeaseWhy::Unarmed
                },
            };
            return Some(wire::json_frame(p::LEASE_LOST, 0, &lost));
        }
        // A claim at or below the highest started for this conversation was decided before (or is) one whose job
        // already ran: refuse it, whether or not that process is still held or was acked.
        let highest = st
            .claims
            .get(&spec.conv)
            .copied()
            .filter(|h| spec.claim <= *h)?;
        let stale = p::StaleClaim {
            conv: spec.conv.clone(),
            claim: spec.claim,
            highest,
        };
        Some(wire::json_frame(p::STALE_CLAIM, 0, &stale))
    }

    fn spawn<'a>(self: &'a Arc<Self>, mut st: MutexGuard<'a, State>, spec: p::Spawn) {
        if let Some(frame) = self.spawn_refusal(&st, &spec) {
            return self.to_core(&st, frame);
        }
        // A warm engine always runs under the resident stub (runtime-design section 4): without it nothing holds the
        // conversation's lock for a turn. No stub is a failed start, never a bare engine.
        let resident = match (spec.mode, spec.stub.as_deref(), spec.lock_dir.as_deref()) {
            (p::Mode::Pipes, Some(stub), Some(lock_dir)) => Some(Resident { stub, lock_dir }),
            (p::Mode::Pipes, _, _) => {
                let error = format!(
                    "no kith-stub to run {:?} under. {STUB_HINT}",
                    spec.argv.first()
                );
                eprintln!("holder: {}: {error}", spec.conv);
                let failed = p::SpawnFailed {
                    conv: spec.conv,
                    claim: spec.claim,
                    error,
                };
                return self.to_core(&st, wire::json_frame(p::SPAWN_FAILED, 0, &failed));
            }
            _ => None,
        };
        let started = platform::spawn(&Spec {
            argv: &spec.argv,
            env: &spec.env,
            cwd: &spec.cwd,
            mode: spec.mode,
            cols: spec.cols,
            rows: spec.rows,
            resident,
        });
        // Windows: the stub opens its control pipe a moment after it starts, and waiting for it holds up every other
        // process if the state lock is held (a stub can be slow for seconds, and the lease is 30). So the wait runs
        // without the lock, and everything the spawn rules looked at is looked at again once it is back.
        let mut waited = false;
        let started = match started {
            Ok(mut s) => match s.ctl.take() {
                Some(platform::CtlStart::Accept(accept)) => {
                    drop(st);
                    let accepted = accept(&s.proc);
                    st = self.lock();
                    waited = true;
                    accepted.map(|ctl| {
                        s.ctl = Some(platform::CtlStart::Ready(ctl));
                        s
                    })
                }
                other => {
                    s.ctl = other;
                    Ok(s)
                }
            },
            Err(e) => Err(e),
        };
        if waited
            && let Ok(s) = &started
            && let Some(frame) = self.spawn_refusal(&st, &spec)
        {
            // Another start of this conversation got in while the lock was free: this one is not wanted.
            s.proc.kill_tree();
            return self.to_core(&st, frame);
        }
        let started = match started {
            Ok(s) => s,
            Err(e) => {
                // With a stub, what failed to start is the stub (a missing engine is the stub's exit 127).
                let error = match (&spec.stub, spec.mode) {
                    (Some(stub), p::Mode::Pipes) => {
                        format!("cannot start kith-stub {stub:?}: {e}. {STUB_HINT}")
                    }
                    _ => format!(
                        "cannot start {:?}: {e}",
                        spec.argv.first().map_or("", |s| s.as_str())
                    ),
                };
                eprintln!("holder: {}: {error}", spec.conv);
                let failed = p::SpawnFailed {
                    conv: spec.conv,
                    claim: spec.claim,
                    error,
                };
                return self.to_core(&st, wire::json_frame(p::SPAWN_FAILED, 0, &failed));
            }
        };
        // Recorded only now: a start that failed can be tried again with the same claim.
        st.claims.insert(spec.conv.clone(), spec.claim);
        let id = st.next_id;
        st.next_id += 1;
        let pid = started.proc.pid();
        let proc = Arc::new(started.proc);
        let (input, queued) = std::sync::mpsc::channel::<Vec<u8>>();
        let (mut ctl_lines, mut ctl_reader) = (None, None);
        if let Some(platform::CtlStart::Ready(ctl)) = started.ctl {
            let (tx, lines) = std::sync::mpsc::channel::<String>();
            let (holder, mut to_stub) = (self.clone(), ctl.write);
            std::thread::spawn(move || {
                for line in lines {
                    if to_stub
                        .write_all(line.as_bytes())
                        .and_then(|()| to_stub.flush())
                        .is_err()
                    {
                        holder.ctl_down(id);
                        return;
                    }
                }
            });
            ctl_lines = Some(tx);
            ctl_reader = Some(ctl.read);
        }
        let mut writer = started.input;
        std::thread::spawn(move || {
            for bytes in queued {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        st.procs.insert(
            id,
            Entry {
                held: Held {
                    instance: self.instance.clone(),
                    id,
                    conv: spec.conv.clone(),
                    claim: spec.claim,
                    generation: spec.generation,
                    pid,
                    mode: spec.mode,
                    cols: spec.cols,
                    rows: spec.rows,
                    exit: None,
                    locks: Vec::new(),
                },
                proc: proc.clone(),
                input: Some(input),
                tail: VecDeque::new(),
                readers: 1 + u8::from(started.err.is_some()),
                gone: AtomicBool::new(false),
                kill_on_lease_loss: spec.kill_on_lease_loss,
                lapsed: false,
                ctl: ctl_lines,
                turn: Turn {
                    claim: spec.claim,
                    released: None,
                },
                pending: VecDeque::new(),
                reports: BTreeMap::new(),
                requested: 0,
            },
        );
        if let Some(read) = ctl_reader {
            let holder = self.clone();
            std::thread::spawn(move || holder.ctl_reader(id, read));
        }
        let reader = |stream: Box<dyn std::io::Read + Send>, kind: u8| {
            let holder = self.clone();
            std::thread::spawn(move || holder.pump(id, kind, stream));
        };
        reader(started.out, p::OUTPUT);
        if let Some(err) = started.err {
            reader(err, p::STDERR);
        }
        let holder = self.clone();
        std::thread::spawn(move || holder.reap(id, proc));
        self.to_core(
            &st,
            wire::json_frame(
                p::SPAWNED,
                id,
                &p::Spawned {
                    conv: spec.conv,
                    claim: spec.claim,
                    pid,
                    instance: self.instance.clone(),
                    id,
                },
            ),
        );
    }

    /// Sends an answer for the core and keeps it until acked, one per claim and kind. For what the stub said (or its
    /// loss): the core may not have heard it, and LIST after a reconnect tells it. A weaker answer never replaces a
    /// stronger one for the same key (see `strength`).
    fn answer(
        &self,
        st: &mut State,
        id: u32,
        op: p::LockOp,
        claim: u64,
        word: p::LockWord,
        reason: Option<&str>,
    ) {
        let answer = self.lock_answer(id, op, claim, word, reason);
        if let Some(e) = st.procs.get_mut(&id) {
            let keep = e
                .reports
                .get(&(claim, op))
                .is_none_or(|old| strength(old.word) <= strength(word));
            if keep {
                e.reports.insert((claim, op), answer.clone());
            }
        }
        self.to_core(st, wire::json_frame(p::LOCK_ANSWER, id, &answer));
    }

    /// Sends an answer the holder made up itself, on the spot (a refusal): the core has it on the frame, there is
    /// nothing to recover, so it is not kept.
    fn refuse_lock(
        &self,
        st: &State,
        id: u32,
        op: p::LockOp,
        claim: u64,
        word: p::LockWord,
        reason: &str,
    ) {
        let answer = self.lock_answer(id, op, claim, word, Some(reason));
        self.to_core(st, wire::json_frame(p::LOCK_ANSWER, id, &answer));
    }

    fn lock_answer(
        &self,
        id: u32,
        op: p::LockOp,
        claim: u64,
        word: p::LockWord,
        reason: Option<&str>,
    ) -> p::LockAnswer {
        p::LockAnswer {
            instance: self.instance.clone(),
            id,
            op,
            claim,
            word,
            reason: reason.map(str::to_owned),
        }
    }

    /// A core's `TAKE` or `RELEASE`: relayed to the process's stub only for its current claim (or one whose take is
    /// still in flight), refused here when there is nothing to ask. The holder decides nothing about the lock itself:
    /// the stub's answer is the truth.
    fn lock_request(&self, st: &mut State, op: p::LockOp, id: u32, req: &p::LockReq) {
        use p::LockWord::{Error, NotHeld};
        let claim = req.claim;
        let take = op == p::LockOp::Take;
        let miss = if take { Error } else { NotHeld };
        if !lock_key_ok(&req.key) {
            return self.refuse_lock(st, id, op, claim, Error, "bad-key");
        }
        let Some(e) = st.procs.get_mut(&id) else {
            return self.refuse_lock(st, id, op, claim, miss, "gone");
        };
        // The process's lock is its conversation's: another key is a core mixing two conversations up.
        if e.held.conv != req.key {
            return self.refuse_lock(st, id, op, claim, Error, "wrong-key");
        }
        // The engine has ended, and its stub with it: no lock is held, and the core hears of the end as an exit.
        if e.held.exit.is_some() {
            return self.refuse_lock(st, id, op, claim, miss, "exited");
        }
        let Some(tx) = e.ctl.clone() else {
            return self.refuse_lock(st, id, op, claim, miss, "stub-gone");
        };
        // A claim below the highest the holder saw for this conversation is an old decision (the spawn rule too).
        let older_for_conv = st.claims.get(&e.held.conv).is_some_and(|h| claim < *h);
        let take_in_flight = e
            .pending
            .iter()
            .any(|(o, c, _)| *o == p::LockOp::Take && *c == claim);
        if take {
            let refuse = if claim < e.turn.claim || older_for_conv {
                Some("stale-claim")
            } else if e.turn.released.is_some_and(|r| claim <= r) {
                // No `take` after `release` for one claim: the stub cannot know the claim is finished.
                Some("claim-released")
            } else if claim < e.requested {
                Some("stale-claim")
            } else if e.reports.len() + e.pending.len() >= MAX_REPORTS {
                Some("too-many-unacked")
            } else {
                None
            };
            if let Some(why) = refuse {
                return self.refuse_lock(st, id, op, claim, Error, why);
            }
            e.requested = e.requested.max(claim);
        } else if claim != e.turn.claim && !take_in_flight {
            // Only the current claim's lock, or one being taken right now, can be held here; an older or unknown
            // claim holds nothing. (Below a newer take's claim it is an old decision, not just an unknown one.)
            let why = if claim < e.requested {
                "stale-claim"
            } else {
                "not-current"
            };
            return self.refuse_lock(st, id, op, claim, NotHeld, why);
        } else {
            e.turn.released = Some(e.turn.released.map_or(claim, |r| r.max(claim)));
        }
        e.pending.push_back((op, claim, req.key.clone()));
        let verb = if take { "take" } else { "release" };
        if tx.send(format!("{verb} {} {claim}\n", req.key)).is_err() {
            e.pending.pop_back();
            self.refuse_lock(st, id, op, claim, miss, "stub-gone");
        }
    }

    /// Reads the stub's answers for as long as the channel lives.
    fn ctl_reader(&self, id: u32, read: Box<dyn std::io::Read + Send>) {
        use std::io::BufRead;
        for line in std::io::BufReader::new(read).lines().map_while(Result::ok) {
            let mut st = self.lock();
            self.stub_said(&mut st, id, &line);
        }
        self.ctl_down(id);
    }

    /// One line from the stub: `locked|busy|released|not-held <claim>` or `error <claim|-> <reason>`.
    fn stub_said(&self, st: &mut State, id: u32, line: &str) {
        let mut parts = line.trim_end().splitn(3, ' ');
        let (Some(word), Some(claim)) = (parts.next(), parts.next()) else {
            return;
        };
        let reason = parts.next();
        let (op, word) = match word {
            "locked" => (Some(p::LockOp::Take), p::LockWord::Locked),
            "busy" => (Some(p::LockOp::Take), p::LockWord::Busy),
            "released" => (Some(p::LockOp::Release), p::LockWord::Released),
            "not-held" => (Some(p::LockOp::Release), p::LockWord::NotHeld),
            "error" => (None, p::LockWord::Error),
            _ => return,
        };
        let Some(e) = st.procs.get_mut(&id) else {
            return;
        };
        // The oldest request it answers: `-` is an error for a line the stub could not attribute to a claim.
        let at = e.pending.iter().position(|(o, c, _)| {
            op.is_none_or(|op| op == *o) && (claim == "-" || claim.parse() == Ok(*c))
        });
        let Some((op, claim, _)) = at.and_then(|i| e.pending.remove(i)) else {
            return;
        };
        if word == p::LockWord::Locked {
            // `released` stays: a release relayed while this take was in flight is still the later word.
            if claim > e.turn.claim {
                e.turn.claim = claim;
            }
            let highest = st.claims.entry(e.held.conv.clone()).or_insert(claim);
            *highest = (*highest).max(claim);
        }
        self.answer(st, id, op, claim, word, reason);
    }

    /// The channel to the stub ended (the stub died, or a write failed): every request still waiting is an error,
    /// and nothing more is relayed. The lock itself is the OS's: it went with the stub, or it stays with a live one.
    fn ctl_down(&self, id: u32) {
        let mut st = self.lock();
        let Some(e) = st.procs.get_mut(&id) else {
            return;
        };
        e.ctl = None;
        // An engine with nobody to hold its lock must not go on running: end its tree. The exit is reported by the
        // reaper as for any other end.
        if e.held.exit.is_none() && !e.gone.load(Ordering::Relaxed) {
            e.proc.kill_tree();
        }
        let waiting: Vec<_> = e.pending.drain(..).collect();
        for (op, claim, _) in waiting {
            self.answer(
                &mut st,
                id,
                op,
                claim,
                p::LockWord::Error,
                Some("stub-gone"),
            );
        }
    }

    /// Always reads, core or not, so programs never stall on a full pipe.
    fn pump(&self, id: u32, kind: u8, mut reader: impl std::io::Read) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break, // EIO on a pty: every slave side closed
            };
            let mut st = self.lock();
            let Some(e) = st.procs.get_mut(&id) else {
                break;
            };
            if kind == p::OUTPUT {
                e.tail.extend(&buf[..n]);
                let excess = e.tail.len().saturating_sub(TAIL_BYTES);
                e.tail.drain(..excess);
            }
            self.to_core(&st, wire::frame(kind, id, &buf[..n]));
            // A core that falls behind slows its programs, as a pipe nobody reads would.
            let Some(backlog) = st
                .core
                .as_ref()
                .map(|c| c.backlog.clone())
                .filter(|b| b.over(BACKLOG_BYTES))
            else {
                continue;
            };
            drop(st);
            backlog.wait(BACKLOG_BYTES);
        }
        if let Some(e) = self.lock().procs.get_mut(&id) {
            e.readers -= 1;
        }
        self.changed.notify_all();
    }

    /// The exit is recorded after the tree is swept and the last output forwarded; it stays until a core acks it.
    fn reap(&self, id: u32, proc: Arc<Proc>) {
        proc.wait_exit();
        // The program is done; what it left behind goes too, before its exit is reported.
        proc.sweep();
        let exit = match proc.reap() {
            Ok(exit) => exit,
            Err(e) => {
                // Dropping the exit would leave the process "running" for ever: record that it ended unreadably.
                eprintln!("holder: wait for process {id}: {e}");
                p::Exit {
                    code: None,
                    signal: None,
                    lapsed: false,
                    error: Some(e.to_string()),
                }
            }
        };
        let deadline = Instant::now() + EXIT_DRAIN;
        let mut st = self.lock();
        while st.procs.get(&id).is_some_and(|e| e.readers > 0) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            st = self
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        if st.procs.get(&id).is_some_and(|e| e.held.exit.is_none()) {
            let mut exit = exit;
            exit.lapsed = st.procs.get(&id).is_some_and(|e| e.lapsed);
            self.to_core(&st, wire::json_frame(p::EXITED, id, &exit));
            if let Some(e) = st.procs.get_mut(&id) {
                e.held.exit = Some(exit);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::Client;
    use crate::clock::fake::Fake;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_failed_reap_is_recorded_as_an_error_exit_and_kept() {
        let holder = test_holder();
        let argv = ["true".to_string()];
        let env = std::env::vars().collect();
        let started = platform::spawn(&Spec {
            argv: &argv,
            env: &env,
            cwd: "/",
            mode: p::Mode::Pipes,
            cols: 80,
            rows: 24,
            resident: None,
        })
        .unwrap();
        let proc = Arc::new(started.proc);
        // Reaped behind the holder's back: the holder's own wait will then fail.
        proc.wait_exit();
        proc.reap().unwrap();
        holder.lock().procs.insert(
            1,
            Entry {
                held: Held {
                    instance: String::new(),
                    id: 1,
                    conv: "c".into(),
                    claim: 1,
                    generation: 1,
                    pid: proc.pid(),
                    mode: p::Mode::Pipes,
                    cols: 80,
                    rows: 24,
                    exit: None,
                    locks: Vec::new(),
                },
                proc: proc.clone(),
                input: None,
                tail: VecDeque::new(),
                readers: 0,
                gone: AtomicBool::new(false),
                kill_on_lease_loss: false,
                lapsed: false,
                ctl: None,
                turn: Turn {
                    claim: 1,
                    released: None,
                },
                pending: VecDeque::new(),
                reports: BTreeMap::new(),
                requested: 0,
            },
        );
        holder.reap(1, proc);
        let st = holder.lock();
        let exit = st.procs[&1].held.exit.clone().expect("an exit is kept");
        assert_eq!((exit.code, exit.signal), (None, None));
        assert!(exit.error.is_some());
    }

    #[test]
    fn a_weaker_answer_never_replaces_a_stronger_one_for_the_same_claim_and_kind() {
        use p::LockOp::{Release, Take};
        use p::LockWord::{Busy, Error, Locked, NotHeld, Released};
        let holder = test_holder();
        let argv = ["sleep".to_string(), "30".to_string()];
        let env = std::env::vars().collect();
        let started = platform::spawn(&Spec {
            argv: &argv,
            env: &env,
            cwd: "/",
            mode: p::Mode::Pipes,
            cols: 80,
            rows: 24,
            resident: None,
        })
        .unwrap();
        let proc = Arc::new(started.proc);
        holder.lock().procs.insert(
            1,
            Entry {
                held: Held {
                    instance: String::new(),
                    id: 1,
                    conv: "c".into(),
                    claim: 1,
                    generation: 1,
                    pid: proc.pid(),
                    mode: p::Mode::Pipes,
                    cols: 80,
                    rows: 24,
                    exit: None,
                    locks: Vec::new(),
                },
                proc: proc.clone(),
                input: None,
                tail: VecDeque::new(),
                readers: 0,
                gone: AtomicBool::new(false),
                kill_on_lease_loss: false,
                lapsed: false,
                ctl: None,
                turn: Turn {
                    claim: 1,
                    released: None,
                },
                pending: VecDeque::new(),
                reports: BTreeMap::new(),
                requested: 0,
            },
        );
        let kept = |holder: &Holder, claim, op| holder.lock().procs[&1].reports[&(claim, op)].word;
        let mut st = holder.lock();
        holder.answer(&mut st, 1, Take, 2, Locked, None);
        // A refused retry (busy, or an error after the stub went away) is told to the core, and not kept over it.
        holder.answer(&mut st, 1, Take, 2, Busy, None);
        holder.answer(&mut st, 1, Take, 2, Error, Some("stub-gone"));
        drop(st);
        assert_eq!(kept(&holder, 2, Take), Locked);
        // Weaker kinds fill in until something stronger comes.
        let mut st = holder.lock();
        holder.answer(&mut st, 1, Release, 2, Error, Some("stub-gone"));
        holder.answer(&mut st, 1, Release, 2, NotHeld, None);
        drop(st);
        assert_eq!(kept(&holder, 2, Release), NotHeld);
        let mut st = holder.lock();
        holder.answer(&mut st, 1, Release, 2, Released, None);
        holder.answer(&mut st, 1, Release, 2, NotHeld, None);
        drop(st);
        assert_eq!(kept(&holder, 2, Release), Released);
        proc.kill_tree();
        proc.wait_exit();
        let _ = proc.reap();
    }

    #[test]
    fn once_closing_is_set_a_hello_is_refused() {
        let holder = Arc::new(Holder::new(
            b"0123456789abcdef".to_vec(),
            PathBuf::from("unused"),
            Duration::ZERO,
        ));
        holder.lock().closing = true;
        let (mut core, theirs) = UnixStream::pair().unwrap();
        let served = holder.clone();
        let t = std::thread::spawn(move || served.serve(theirs));
        let hello = p::Hello {
            version: p::VERSION,
            generation: 1,
            nonce: "aa".into(),
        };
        wire::write_json(&mut core, p::HELLO, 0, &hello).unwrap();
        let (kind, _, payload) = wire::read(&mut core).unwrap().unwrap();
        assert_eq!(kind, p::REFUSED);
        assert_eq!(wire::parse::<Refused>(&payload).unwrap().why, Why::Closing);
        t.join().unwrap().unwrap();
    }

    #[test]
    fn a_renewal_from_a_connection_that_is_not_the_newest_is_refused_and_does_not_count() {
        let holder = test_holder();
        let (mut theirs, mut mine) = UnixStream::pair().unwrap();
        wire::write_json(&mut theirs, p::RENEW, 0, &p::Renew { ms: 60_000, seq: 1 }).unwrap();
        // No core is registered, so this connection is not the newest one.
        holder.commands(&mut mine, 7, 1).unwrap();
        assert_eq!(holder.lock().lease, Lease::Unarmed);
        let (kind, _, _) = wire::read(&mut theirs).unwrap().unwrap();
        assert_eq!(kind, p::REFUSED);
    }

    /// A holder on a clock the test moves, serving one core over a socket pair.
    fn leased(clock: &Fake) -> (Arc<Holder>, Client) {
        stub_and_locks(); // built before any lease clock runs
        let mut holder = Holder::new(
            b"0123456789abcdef".to_vec(),
            PathBuf::from("unused"),
            Duration::ZERO,
        );
        holder.clock = clock.clock();
        let holder = Arc::new(holder);
        let client = connect_to(&holder, 1);
        (holder, client)
    }

    fn connect_to(holder: &Arc<Holder>, generation: u64) -> Client {
        let (core, theirs) = UnixStream::pair().unwrap();
        let served = holder.clone();
        std::thread::spawn(move || served.serve(theirs));
        Client::over(core, "0123456789abcdef", generation).expect("served")
    }

    /// The built `kith-stub` every pipes spawn runs under, and a lock folder for this test process.
    fn stub_and_locks() -> (String, String) {
        static BUILT: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        BUILT
            .get_or_init(|| {
                let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
                let manifest =
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stub/Cargo.toml");
                let built = std::process::Command::new(cargo)
                    .args(["build", "--manifest-path"])
                    .arg(&manifest)
                    .status()
                    .unwrap();
                assert!(built.success(), "kith-stub did not build");
                // The test binary is in target/<profile>/deps; the stub is in target/<profile>.
                let exe = std::env::current_exe().unwrap();
                let stub = exe.parent().unwrap().parent().unwrap().join("kith-stub");
                assert!(stub.is_file(), "no {}", stub.display());
                let locks = std::env::temp_dir().join(format!("kh-ulocks-{}", std::process::id()));
                (stub.display().to_string(), locks.display().to_string())
            })
            .clone()
    }

    fn under_stub(mut spec: p::Spawn) -> p::Spawn {
        let (stub, locks) = stub_and_locks();
        (spec.stub, spec.lock_dir) = (Some(stub), Some(locks));
        spec
    }

    fn guest(conv: &str, tagged: bool) -> p::Spawn {
        under_stub(p::Spawn {
            conv: conv.into(),
            claim: 1,
            generation: 1,
            argv: vec!["sleep".into(), "30".into()],
            env: std::env::vars().collect(),
            cwd: "/".into(),
            mode: p::Mode::Pipes,
            cols: 80,
            rows: 24,
            kill_on_lease_loss: tagged,
            stub: None,
            lock_dir: None,
        })
    }

    const LEASE_MS: u64 = 30_000;
    const WAIT: Duration = Duration::from_secs(10);

    fn ping(c: &mut Client, seq: u64) {
        c.send_json(p::PING, 0, &p::Ping { seq }).unwrap();
        let (_, _, payload) = c.expect(p::PONG, 0, WAIT).expect("pong");
        assert_eq!(wire::parse::<p::Pong>(&payload).unwrap().seq, seq);
    }

    fn renew(c: &mut Client, seq: u64) {
        c.send_json(p::RENEW, 0, &p::Renew { ms: LEASE_MS, seq })
            .unwrap();
    }

    fn start(c: &mut Client, spec: &p::Spawn) -> p::HeldId {
        c.spawn(spec).unwrap();
        c.expect(p::SPAWNED, 0, WAIT).expect("spawned");
        c.list()
            .unwrap()
            .into_iter()
            .find(|h| h.conv == spec.conv)
            .expect("listed")
            .held_id()
    }

    fn exit_of(c: &mut Client, id: &p::HeldId) -> Option<p::Exit> {
        let until = Instant::now() + WAIT;
        loop {
            let held = c.list().unwrap().into_iter().find(|h| h.id == id.id);
            if let Some(exit) = held.and_then(|h| h.exit) {
                return Some(exit);
            }
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn alive(c: &mut Client, id: &p::HeldId) -> bool {
        c.list()
            .unwrap()
            .into_iter()
            .find(|h| h.id == id.id)
            .is_some_and(|h| h.exit.is_none())
    }

    #[test]
    fn a_renewal_read_late_still_ends_at_the_cores_send_plus_the_period() {
        // Model row `today`/`read-anchor`: the old deadline was "when I read it + period".
        let clock = Fake::default();
        let (holder, mut c) = leased(&clock);
        ping(&mut c, 1);
        // The RENEW sits in the link for 20 s before the hand reads it.
        clock.advance(Duration::from_secs(20));
        renew(&mut c, 1);
        let g = start(&mut c, &guest("g", true));
        assert!(alive(&mut c, &g));
        // 31 s after the ping (not after the read): the lease is over and the guest with it.
        clock.advance(Duration::from_secs(11));
        let exit = exit_of(&mut c, &g).expect("the guest outlived the core's belief");
        assert!(exit.lapsed, "{exit:?}");
        drop(holder.close_down(holder.lock()));
    }

    #[test]
    fn a_hand_that_sleeps_through_the_deadline_kills_guests_first_and_never_owners() {
        let clock = Fake::default();
        let (holder, mut c) = leased(&clock);
        ping(&mut c, 1);
        renew(&mut c, 1);
        let g = start(&mut c, &guest("g", true));
        let o = start(&mut c, &guest("o", false));
        // The machine sleeps ten minutes: the lease clock counts it, and no frame came in between.
        clock.advance(Duration::from_secs(600));
        // The first thing the hand runs after waking is the expiry (the watcher's every 10 ms, or a frame's).
        {
            let mut st = holder.lock();
            holder.expire_lease(&mut st);
            assert_eq!(st.lease, Lease::Lost);
        }
        let exit = exit_of(&mut c, &g).expect("guest killed");
        assert!(exit.lapsed);
        assert!(
            alive(&mut c, &o),
            "lease expiry must never touch an owner run (I18)"
        );
        drop(holder.close_down(holder.lock()));
    }

    #[test]
    fn a_renewal_for_an_unknown_seq_or_with_a_past_deadline_arms_nothing() {
        let clock = Fake::default();
        let (holder, mut c) = leased(&clock);
        // Never pinged: nothing to anchor on.
        renew(&mut c, 7);
        c.list().unwrap();
        assert_eq!(holder.lock().lease, Lease::Unarmed);
        // Pinged, but the RENEW is read after the period already ran out (a stale one, read after a wake).
        ping(&mut c, 1);
        clock.advance(Duration::from_secs(31));
        renew(&mut c, 1);
        c.list().unwrap();
        assert_eq!(holder.lock().lease, Lease::Unarmed);
        // A tagged spawn is still refused.
        c.spawn(&guest("g", true)).unwrap();
        let (_, _, payload) = c.expect(p::LEASE_LOST, 0, WAIT).expect("refused");
        assert_eq!(
            wire::parse::<p::LeaseLost>(&payload).unwrap().why,
            p::LeaseWhy::Unarmed
        );
    }

    #[test]
    fn a_seq_from_an_older_connection_does_not_anchor_a_newer_one() {
        let clock = Fake::default();
        let (holder, mut old) = leased(&clock);
        ping(&mut old, 1);
        let mut new = connect_to(&holder, 1);
        renew(&mut new, 1);
        new.list().unwrap();
        assert_eq!(holder.lock().lease, Lease::Unarmed);
    }

    #[test]
    fn after_a_lapse_a_valid_renewal_restores_the_lease_but_a_stale_one_does_not() {
        // The rule chosen for M3-0c (model `fixed`, RenewRestores = TRUE): restore on a valid renewal.
        let clock = Fake::default();
        let (holder, mut c) = leased(&clock);
        ping(&mut c, 1);
        renew(&mut c, 1);
        let g = start(&mut c, &guest("g", true));
        clock.advance(Duration::from_secs(31));
        assert!(exit_of(&mut c, &g).expect("killed").lapsed);
        // The refused spawn says why, and the claim is not used up.
        let mut again = guest("g2", true);
        again.claim = 2;
        c.spawn(&again).unwrap();
        let (_, _, payload) = c.expect(p::LEASE_LOST, 0, WAIT).expect("refused");
        assert_eq!(
            wire::parse::<p::LeaseLost>(&payload).unwrap().why,
            p::LeaseWhy::Lapsed
        );
        // The RENEW of the old round is stale: its deadline is past.
        renew(&mut c, 1);
        c.list().unwrap();
        assert_eq!(holder.lock().lease, Lease::Lost);
        // A fresh round restores it.
        ping(&mut c, 2);
        renew(&mut c, 2);
        c.list().unwrap();
        assert!(matches!(holder.lock().lease, Lease::Until(_)));
        let g2 = start(&mut c, &again);
        assert!(alive(&mut c, &g2));
        // A core coming back after the lapse is told: the welcome says the lease is lost.
        clock.advance(Duration::from_secs(31));
        c.list().unwrap();
        let back = connect_to(&holder, 1);
        assert!(back.welcome.lease_lost);
        drop(holder.close_down(holder.lock()));
    }

    fn test_holder() -> Arc<Holder> {
        stub_and_locks();
        Arc::new(Holder::new(
            b"0123456789abcdef".to_vec(),
            PathBuf::from("unused"),
            Duration::ZERO,
        ))
    }

    #[test]
    fn a_temporary_accept_failure_does_not_end_the_holder() {
        let holder = test_holder();
        let (core, theirs) = UnixStream::pair().unwrap();
        let mut offered = vec![
            Err(io::Error::from_raw_os_error(libc::EMFILE)),
            Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
            Ok(theirs),
        ];
        let looping = holder.clone();
        std::thread::spawn(move || {
            looping.accept_loop(
                || {
                    if offered.is_empty() {
                        std::thread::sleep(Duration::from_secs(60));
                    }
                    offered.remove(0)
                },
                5,
                Duration::from_millis(1),
            )
        });
        // The connection that came after the failures is served, and the process it spawns stays listed.
        let mut client = Client::over(core, "0123456789abcdef", 1).expect("served");
        client
            .spawn(&under_stub(p::Spawn {
                conv: "c".into(),
                claim: 1,
                generation: 1,
                argv: vec!["sleep".into(), "30".into()],
                env: std::env::vars().collect(),
                cwd: "/".into(),
                mode: p::Mode::Pipes,
                cols: 80,
                rows: 24,
                kill_on_lease_loss: false,
                stub: None,
                lock_dir: None,
            }))
            .unwrap();
        client
            .expect(p::SPAWNED, 0, Duration::from_secs(10))
            .expect("spawned");
        assert_eq!(client.list().unwrap().len(), 1);
        drop(holder.close_down(holder.lock()));
    }

    #[test]
    fn gone_sticks_once_a_kill_has_seen_the_tree_empty() {
        let holder = test_holder();
        let (core, theirs) = UnixStream::pair().unwrap();
        let mut offered = vec![Ok(theirs)];
        let looping = holder.clone();
        std::thread::spawn(move || {
            looping.accept_loop(
                || {
                    if offered.is_empty() {
                        std::thread::sleep(Duration::from_secs(60));
                    }
                    offered.remove(0)
                },
                5,
                Duration::from_millis(1),
            )
        });
        let mut client = Client::over(core, "0123456789abcdef", 1).expect("served");
        client
            .spawn(&under_stub(p::Spawn {
                conv: "c".into(),
                claim: 1,
                generation: 1,
                argv: vec!["sleep".into(), "30".into()],
                env: std::env::vars().collect(),
                cwd: "/".into(),
                mode: p::Mode::Pipes,
                cols: 80,
                rows: 24,
                kill_on_lease_loss: false,
                stub: None,
                lock_dir: None,
            }))
            .unwrap();
        client
            .expect(p::SPAWNED, 0, Duration::from_secs(10))
            .expect("spawned");
        let id = client.list().unwrap()[0].held_id();
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.kill(&id).unwrap() != KillState::Gone {
            assert!(Instant::now() < deadline, "kill never reached gone");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Seen once: the entry remembers it, so a later kill says gone without signalling a group id that may by now
        // belong to someone else (the blind spot of `tree_empty`).
        assert!(holder.lock().procs[&id.id].gone.load(Ordering::Relaxed));
        assert_eq!(client.kill(&id).unwrap(), KillState::Gone);
        drop(holder.close_down(holder.lock()));
    }

    #[test]
    fn giving_up_on_accept_kills_what_is_held() {
        let holder = test_holder();
        let (core, theirs) = UnixStream::pair().unwrap();
        let mut offered = vec![Ok(theirs)];
        let looping = holder.clone();
        let ended = std::thread::spawn(move || {
            looping.accept_loop(
                || {
                    if offered.is_empty() {
                        Err(io::Error::from_raw_os_error(libc::EMFILE))
                    } else {
                        offered.remove(0)
                    }
                },
                3,
                Duration::from_millis(1),
            )
        });
        let mut client = Client::over(core, "0123456789abcdef", 1).expect("served");
        client
            .spawn(&under_stub(p::Spawn {
                conv: "c".into(),
                claim: 1,
                generation: 1,
                argv: vec!["sleep".into(), "30".into()],
                env: std::env::vars().collect(),
                cwd: "/".into(),
                mode: p::Mode::Pipes,
                cols: 80,
                rows: 24,
                kill_on_lease_loss: false,
                stub: None,
                lock_dir: None,
            }))
            .unwrap();
        client
            .expect(p::SPAWNED, 0, Duration::from_secs(10))
            .expect("spawned");
        let err = ended.join().unwrap();
        assert_eq!(err.raw_os_error(), Some(libc::EMFILE));
        let st = holder.close_down(holder.lock());
        assert!(st.closing);
        assert!(st.procs.values().all(|e| e.proc.tree_empty()));
    }
}
