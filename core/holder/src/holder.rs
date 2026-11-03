// Adapted from herdr-core b63ecbd (crates/herdr-core/src/holder.rs: the shape of the holder, its per-core writer
// thread and backlog, the pump and reap threads, the 1 MB output tail). Copyright herdrdev and the herdr-core
// contributors, Apache-2.0. See NOTICE. Everything about who may do what (the secret, generation fencing, the
// claim, kill and ack answers) is Kith's own; herdr's core restarting, scopes and sweeping are not taken.
//
//! The holder: owns every agent process, is its parent, always reads its output, keeps a tail and the unacked
//! exits. No parsing, no policy: it checks a secret and a generation and does what the newest core says.

use crate::auth;
use crate::platform::{self, Proc, Spec};
use crate::proto::{self as p, Held, KillState, Refused, Why};
use crate::wire;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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
}

/// The lease the newest core keeps alive. See `proto` for the rules.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Lease {
    /// No renewal has ever arrived: nothing expires.
    Unarmed,
    Until(Instant),
    Lost,
}

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
}

struct Holder {
    state: Mutex<State>,
    /// Signals: an output stream ended, or an exit was recorded.
    changed: Condvar,
    instance: String,
    secret: Vec<u8>,
    endpoint: PathBuf,
    idle: Duration,
}

/// Runs a holder in `cfg.dir` until it is idle or told to shut down.
pub fn run(cfg: Config) -> io::Result<()> {
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
            }),
            changed: Condvar::new(),
            instance: platform::random_hex(),
            secret,
            endpoint,
            idle,
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
            && Instant::now() >= t
        {
            st.lease = Lease::Lost;
        }
        if st.lease == Lease::Lost {
            for e in st.procs.values() {
                if e.kill_on_lease_loss && e.held.exit.is_none() && !e.gone.load(Ordering::Relaxed)
                {
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
        let mut procs: Vec<Held> = st.procs.values().map(|e| e.held.clone()).collect();
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
                p::RENEW => {
                    let renew: p::Renew = wire::parse(&payload)?;
                    // A zero period would lapse at once; it is not a way to end a lease.
                    if renew.ms > 0 {
                        st.lease = Lease::Until(Instant::now() + Duration::from_millis(renew.ms));
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

    fn spawn(self: &Arc<Self>, mut st: MutexGuard<'_, State>, spec: p::Spawn) {
        // The same spawn again (the core did not hear the first answer) is answered, not run twice.
        if let Some(e) = st
            .procs
            .values()
            .find(|e| e.held.conv == spec.conv && e.held.claim == spec.claim)
        {
            let frame = wire::json_frame(
                p::SPAWNED,
                e.held.id,
                &p::Spawned {
                    conv: spec.conv,
                    claim: spec.claim,
                    pid: e.held.pid,
                    instance: self.instance.clone(),
                    id: e.held.id,
                },
            );
            return self.to_core(&st, frame);
        }
        if spec.kill_on_lease_loss && st.lease == Lease::Lost {
            let lost = p::LeaseLost {
                conv: spec.conv,
                claim: spec.claim,
            };
            return self.to_core(&st, wire::json_frame(p::LEASE_LOST, 0, &lost));
        }
        // A claim at or below the highest started for this conversation was decided before (or is) one whose job
        // already ran: refuse it, whether or not that process is still held or was acked.
        if let Some(highest) = st
            .claims
            .get(&spec.conv)
            .copied()
            .filter(|h| spec.claim <= *h)
        {
            let stale = p::StaleClaim {
                conv: spec.conv,
                claim: spec.claim,
                highest,
            };
            return self.to_core(&st, wire::json_frame(p::STALE_CLAIM, 0, &stale));
        }
        let started = platform::spawn(&Spec {
            argv: &spec.argv,
            env: &spec.env,
            cwd: &spec.cwd,
            mode: spec.mode,
            cols: spec.cols,
            rows: spec.rows,
        });
        let started = match started {
            Ok(s) => s,
            Err(e) => {
                let error = format!(
                    "cannot start {:?}: {e}",
                    spec.argv.first().map_or("", |s| s.as_str())
                );
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
                },
                proc: proc.clone(),
                input: Some(input),
                tail: VecDeque::new(),
                readers: 1 + u8::from(started.err.is_some()),
                gone: AtomicBool::new(false),
                kill_on_lease_loss: spec.kill_on_lease_loss,
            },
        );
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
                },
                proc: proc.clone(),
                input: None,
                tail: VecDeque::new(),
                readers: 0,
                gone: AtomicBool::new(false),
                kill_on_lease_loss: false,
            },
        );
        holder.reap(1, proc);
        let st = holder.lock();
        let exit = st.procs[&1].held.exit.clone().expect("an exit is kept");
        assert_eq!((exit.code, exit.signal), (None, None));
        assert!(exit.error.is_some());
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

    fn test_holder() -> Arc<Holder> {
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
            .spawn(&p::Spawn {
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
            })
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
            .spawn(&p::Spawn {
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
            })
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
            .spawn(&p::Spawn {
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
            })
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
