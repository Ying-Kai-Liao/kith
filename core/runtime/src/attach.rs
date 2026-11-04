//! The attach relay (M1-5): clients attach to a held terminal through the core, and the engine runs on whether they
//! do or not. The frames are the spec in `terminal.rs`'s module docs (the numbers are `kith_holder::attach`).
//!
//! Where it listens. `attach.sock` beside the owner socket (a Unix socket, 0600, in the 0700 core folder; on Windows a
//! named pipe whose access list names this account and SYSTEM), from `kith-holder`'s platform layer like the owner and
//! run sockets. Anyone who can open it is this user; what an attach may do is the ticket's (`terminal.open` issues it
//! to the owner). A daemon that cannot open it logs one line and still serves the owner socket.
//!
//! Who does what. One thread per connection reads the client's frames, and one writes to it, from that client's
//! bounded buffer (an [`Outbox`]). The supervisor (`holder_link.rs`) is the only reader of the holder link and feeds
//! this relay through [`Relay::output`], [`Relay::exited`] and [`Relay::tailed`]; those only copy bytes into memory,
//! so a viewer that stops reading can never slow the supervisor, the other viewers or the engine. The connection
//! threads write to the holder (keystrokes, size, a tail request) through a [`Sender`] on the supervisor's connection,
//! one whole frame at a time.
//!
//! The replay and the live output, with no gap and no duplicate. The holder sends everything on one ordered
//! connection, and it answers a TAIL request under the same lock that appends output to the tail and queues the
//! OUTPUT frame. So every OUTPUT for a terminal that arrives before the TAILED answer is already inside that tail,
//! and every one after it is not. An ATTACH registers its client as *pending* and asks for the tail, both under the
//! relay's lock; the supervisor drops the terminal's OUTPUT for a pending client, and when the answer arrives it queues
//! ATTACHED, the tail as the replay, and makes the client live, so what follows is live output only. The relay's
//! lock orders the requests, and answers come back in the same order, so each TAILED belongs to the oldest pending
//! client (the queue of [`Waiter`]s). The replay does not count against the client's buffer; the live output after
//! it does.
//!
//! A slow client. Each client's buffer holds [`attach::CLIENT_BUFFER`] bytes of live output. The output that would
//! overflow it cuts the client instead: what was queued and not yet sent is dropped, DETACHED `slow` is queued, and
//! the connection closes after it. A viewer that is merely behind (not reading for a while) gets what the socket
//! had taken, then DETACHED `slow`; one that never reads again is closed by the write timeout (Unix; on Windows
//! its writer thread waits until the client closes, and the memory it holds is the bounded buffer). The owner is
//! cut like any other client.
//!
//! Size. The owner's ATTACH `cols`/`rows` and its RESIZE frames go to the holder; the terminal has one size, the last
//! active owner's, because there is one owner at a time. An observer's RESIZE is ignored, and so is its INPUT.
//!
//! One owner. A second owner ATTACH is ERROR `owner_present`; with `takeover: true` (and an owner ticket) it
//! DETACHES the first owner `taken_over`. The check and the first owner's INPUT and RESIZE share one lock, so once the
//! takeover is done the old owner's frames are dropped, never forwarded after the new owner's.
//!
//! When the engine ends. The holder's EXITED reaches every live client as EXIT, and they are closed after it. The
//! relay then asks for the tail one last time, keeps it, and acks the EXITED so the holder forgets the process. An
//! ended terminal is kept in the core's memory for [`ENDED_KEEP`] (ten minutes), at most [`MAX_ENDED`] of them, each with its exit and the
//! last [`ENDED_TAIL`] bytes of output. A client that attaches to one (its ticket came first, or it was late) gets
//! ATTACHED, the kept output as the replay, then EXIT, and is closed. After that the terminal is gone: ERROR
//! `terminal_gone`. Nothing is written to disk. A clients that attached while the end was being taken is answered from
//! the same kept tail.
//!
//! The holder link. When it drops, every client is DETACHED `holder_lost` (the terminals live on in the holder). When
//! the supervisor connects again it learns what the holder holds from the welcome: terminals are listed again, and an
//! exit nobody acked is taken as above.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kith_holder::client::Sender;
use kith_holder::platform::{self, Stream};
use kith_holder::proto::{self, HeldId};
use kith_holder::{attach, wire};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::terminal::{Role, TicketRefused, Tickets};

/// How long a connection may take to send its ATTACH.
const FIRST_FRAME: Duration = Duration::from_secs(10);
/// How long a write to a client may make no progress before the client is cut (Unix).
const WRITE_STALL: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 64;
/// Clients attached to one terminal.
const MAX_CLIENTS: usize = 32;
/// How long an ended terminal's exit and tail are kept for a late attach.
pub(crate) const ENDED_KEEP: Duration = Duration::from_secs(600);
pub(crate) const MAX_ENDED: usize = 16;
/// The most of an ended terminal's output that is kept (the end of it).
pub(crate) const ENDED_TAIL: usize = 256 * 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// --- a client's buffer -----------------------------------------------------------------------------------------------

struct Item {
    bytes: Vec<u8>,
    /// Live output counts against the buffer; the replay and the control frames do not.
    live: bool,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    live: usize,
    /// Nothing more is accepted; the writer ends the connection after what is queued.
    closing: bool,
    /// The connection is gone: drop everything.
    dead: bool,
}

/// What is waiting to be written to one client. Pushing never blocks.
#[derive(Default)]
struct Outbox {
    q: Mutex<Queue>,
    cv: Condvar,
}

fn control(kind: u8, msg: &Value) -> Vec<u8> {
    wire::json_frame(kind, 0, msg)
}

impl Outbox {
    fn push(&self, bytes: Vec<u8>) {
        let mut q = lock(&self.q);
        if !q.closing && !q.dead {
            q.items.push_back(Item { bytes, live: false });
            self.cv.notify_one();
        }
    }

    /// Terminal bytes, cut into frames that fit. `live` bytes count against [`attach::CLIENT_BUFFER`].
    fn push_output(&self, data: &[u8], live: bool) {
        let mut q = lock(&self.q);
        for chunk in data.chunks(attach::MAX_PAYLOAD) {
            if q.closing || q.dead {
                return;
            }
            if live && q.live + chunk.len() > attach::CLIENT_BUFFER {
                // Too far behind: what it has not been sent is dropped, and it is told why.
                q.items.clear();
                q.live = 0;
                q.items.push_back(Item {
                    bytes: control(attach::DETACHED, &json!({"why": "slow"})),
                    live: false,
                });
                q.closing = true;
                self.cv.notify_one();
                return;
            }
            if live {
                q.live += chunk.len();
            }
            q.items.push_back(Item {
                bytes: wire::frame(attach::OUTPUT, 0, chunk),
                live,
            });
        }
        self.cv.notify_one();
    }

    /// The last frame: written after what is queued, then the connection is closed.
    fn finish(&self, bytes: Vec<u8>) {
        let mut q = lock(&self.q);
        if !q.closing && !q.dead {
            q.items.push_back(Item { bytes, live: false });
            q.closing = true;
        }
        self.cv.notify_one();
    }

    /// The connection is gone, or must go now.
    fn kill(&self) {
        let mut q = lock(&self.q);
        q.dead = true;
        q.items.clear();
        q.live = 0;
        self.cv.notify_one();
    }

    /// The next frame to write, waiting for one; `None` once the connection is to be closed.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut q = lock(&self.q);
        loop {
            if q.dead {
                return None;
            }
            if let Some(item) = q.items.pop_front() {
                if item.live {
                    q.live -= item.bytes.len() - 9;
                }
                return Some(item.bytes);
            }
            if q.closing {
                return None;
            }
            q = self
                .cv
                .wait(q)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

// --- the registry ----------------------------------------------------------------------------------------------------

/// One attached client.
struct Handle {
    id: u64,
    role: Role,
    held: HeldId,
    out: Outbox,
    /// Its replay is queued: from here on the terminal's output goes to it.
    live: AtomicBool,
    /// It was detached, taken over, cut, or its connection ended: nothing it sends counts any more.
    gone: AtomicBool,
}

impl Handle {
    fn detach(&self, why: &str) {
        self.gone.store(true, Ordering::SeqCst);
        self.out
            .finish(control(attach::DETACHED, &json!({"why": why})));
    }
}

/// Whose answer the next TAILED is.
enum Waiter {
    Client(Arc<Handle>),
    /// The relay's own request after the engine ended: keep the tail, then ack.
    Ender,
}

struct Term {
    held: HeldId,
    session: String,
    agent: Option<String>,
    pid: u32,
    /// Unix seconds when the core first knew of it.
    started: u64,
    cols: u16,
    rows: u16,
    /// EXITED was seen; the end is being taken (the holder keeps the process until it is acked).
    exit: Option<proto::Exit>,
    clients: Vec<Arc<Handle>>,
    waiters: VecDeque<Waiter>,
}

struct Ended {
    exit: proto::Exit,
    tail: Vec<u8>,
    cols: u16,
    rows: u16,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    /// The holder link; `None` while it is down.
    tx: Option<Sender>,
    instance: String,
    /// By process id; all of the current holder instance.
    terms: HashMap<u32, Term>,
    ended: HashMap<HeldId, Ended>,
}

/// Why an ATTACH was refused after its ticket was good.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// The holder link is down.
    HolderLost,
    /// The terminal has ended and been forgotten, or the core never knew it.
    Gone,
    OwnerPresent,
    TooMany,
}

impl Refusal {
    fn code(&self) -> &'static str {
        match self {
            Refusal::HolderLost => "holder_unreachable",
            Refusal::Gone => "terminal_gone",
            Refusal::OwnerPresent => "owner_present",
            Refusal::TooMany => "too_many_clients",
        }
    }
}

pub(crate) struct Relay {
    inner: Mutex<Inner>,
    next: AtomicU64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Relay {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            next: AtomicU64::new(1),
        }
    }

    // --- the supervisor's side ---------------------------------------------------------------------------------------

    /// The holder link is up: this is what it holds. Terminals the holder no longer has are dropped, new ones are
    /// listed (their agent is unknown), and an exit nobody acked is taken now.
    pub(crate) fn link_up(&self, tx: Sender, instance: &str, procs: &[proto::Held]) {
        let mut inner = lock(&self.inner);
        inner.tx = Some(tx);
        instance.clone_into(&mut inner.instance);
        let alive: Vec<u32> = procs.iter().map(|p| p.id).collect();
        inner.terms.retain(|id, _| alive.contains(id));
        for p in procs {
            let held = p.held_id();
            let term = inner.terms.entry(p.id).or_insert_with(|| Term {
                held: held.clone(),
                session: p.conv.clone(),
                agent: None,
                pid: p.pid,
                started: unix_now(),
                cols: p.cols,
                rows: p.rows,
                exit: None,
                clients: Vec::new(),
                waiters: VecDeque::new(),
            });
            term.cols = p.cols;
            term.rows = p.rows;
            if let Some(exit) = &p.exit {
                term.exit = Some(exit.clone());
            }
        }
        for p in procs.iter().filter(|p| p.exit.is_some()) {
            inner.begin_end(p.id);
        }
    }

    /// The holder link dropped: every client is told, and the terminals stay listed (they live on in the holder).
    pub(crate) fn link_lost(&self) {
        let mut inner = lock(&self.inner);
        inner.tx = None;
        for term in inner.terms.values_mut() {
            for h in term.clients.drain(..) {
                h.detach("holder_lost");
            }
            term.waiters.clear();
        }
    }

    /// A terminal the supervisor has just started.
    pub(crate) fn register(
        &self,
        held: &HeldId,
        session: &str,
        agent: &str,
        pid: u32,
        size: (u16, u16),
    ) {
        let mut inner = lock(&self.inner);
        inner.terms.insert(
            held.id,
            Term {
                held: held.clone(),
                session: session.to_owned(),
                agent: Some(agent.to_owned()),
                pid,
                started: unix_now(),
                cols: size.0,
                rows: size.1,
                exit: None,
                clients: Vec::new(),
                waiters: VecDeque::new(),
            },
        );
    }

    /// A terminal the core ended itself (a claim that could not be handed over).
    pub(crate) fn forget(&self, held: &HeldId) {
        let mut inner = lock(&self.inner);
        if let Some(term) = inner.terms.remove(&held.id) {
            for h in term.clients {
                h.detach("holder_lost");
            }
        }
    }

    /// Whether the supervisor has something to read soon (so it waits for frames in short turns).
    pub(crate) fn busy(&self) -> bool {
        lock(&self.inner)
            .terms
            .values()
            .any(|t| !t.clients.is_empty() || !t.waiters.is_empty())
    }

    /// The engine's output for terminal `id`: to every live client, and dropped for a pending one (it is in the tail
    /// that client will be sent).
    pub(crate) fn output(&self, id: u32, data: &[u8]) {
        let inner = lock(&self.inner);
        if let Some(term) = inner.terms.get(&id) {
            for h in &term.clients {
                if h.live.load(Ordering::SeqCst) {
                    h.out.push_output(data, true);
                }
            }
        }
    }

    /// The holder's EXITED: EXIT to the live clients, which are closed after it; then the end is taken.
    pub(crate) fn exited(&self, id: u32, exit: &proto::Exit) {
        let mut inner = lock(&self.inner);
        let Some(term) = inner.terms.get_mut(&id) else {
            return;
        };
        if term.exit.is_some() {
            return; // already being taken (the welcome listed it and the frame came too)
        }
        term.exit = Some(exit.clone());
        let frame = exit_frame(exit);
        term.clients.retain(|h| {
            if h.live.load(Ordering::SeqCst) {
                h.gone.store(true, Ordering::SeqCst);
                h.out.finish(frame.clone());
                false
            } else {
                true
            }
        });
        inner.begin_end(id);
    }

    /// The holder's answer to a TAIL request. Returns the process to ack when it was the end being taken.
    pub(crate) fn tailed(&self, id: u32, tail: &[u8]) -> Option<HeldId> {
        let mut inner = lock(&self.inner);
        let term = inner.terms.get_mut(&id)?;
        match term.waiters.pop_front()? {
            Waiter::Client(h) => {
                if !h.gone.load(Ordering::SeqCst) {
                    let (cols, rows) = (term.cols, term.rows);
                    let ended = term.exit.clone();
                    activate(&h, tail, (cols, rows));
                    if let Some(exit) = ended {
                        h.gone.store(true, Ordering::SeqCst);
                        h.out.finish(exit_frame(&exit));
                        term.clients.retain(|c| c.id != h.id);
                    }
                }
                None
            }
            Waiter::Ender => {
                let term = inner.terms.remove(&id)?;
                let held = term.held.clone();
                let ended = Ended {
                    exit: term.exit.clone().unwrap_or(proto::Exit {
                        code: None,
                        signal: None,
                        error: None,
                    }),
                    tail: tail[tail.len().saturating_sub(ENDED_TAIL)..].to_vec(),
                    cols: term.cols,
                    rows: term.rows,
                    at: Instant::now(),
                };
                // Clients that attached while the end was taken sit behind this request: answered from the same tail.
                for waiter in term.waiters {
                    if let Waiter::Client(h) = waiter
                        && !h.gone.load(Ordering::SeqCst)
                    {
                        serve_ended(&h, &ended);
                    }
                }
                for h in term.clients {
                    if !h.gone.load(Ordering::SeqCst) {
                        serve_ended(&h, &ended);
                    }
                }
                inner.keep_ended(held.clone(), ended);
                Some(held)
            }
        }
    }

    // --- a connection's side -----------------------------------------------------------------------------------------

    /// Attaches a client for a redeemed ticket. `size` is the owner's cols and rows from its ATTACH.
    fn attach(
        &self,
        held: &HeldId,
        role: Role,
        takeover: bool,
        size: Option<(u16, u16)>,
    ) -> std::result::Result<Arc<Handle>, Refusal> {
        let mut inner = lock(&self.inner);
        inner.purge_ended();
        let h = Arc::new(Handle {
            id: self.next.fetch_add(1, Ordering::SeqCst),
            role,
            held: held.clone(),
            out: Outbox::default(),
            live: AtomicBool::new(false),
            gone: AtomicBool::new(false),
        });
        if let Some(ended) = inner.ended.get(held) {
            serve_ended(&h, ended);
            return Ok(h);
        }
        let tx = inner.tx.clone().ok_or(Refusal::HolderLost)?;
        let term = inner.terms.get_mut(&held.id).ok_or(Refusal::Gone)?;
        if term.clients.len() >= MAX_CLIENTS {
            return Err(Refusal::TooMany);
        }
        if role == Role::Owner {
            let owner = term
                .clients
                .iter()
                .position(|c| c.role == Role::Owner && !c.gone.load(Ordering::SeqCst));
            if let Some(i) = owner {
                if !takeover {
                    return Err(Refusal::OwnerPresent);
                }
                term.clients.remove(i).detach("taken_over");
            }
            if let Some((cols, rows)) = size.filter(|(c, r)| *c > 0 && *r > 0) {
                if tx.resize(held, cols, rows).is_err() {
                    return Err(Refusal::HolderLost);
                }
                term.cols = cols;
                term.rows = rows;
            }
        }
        if term.exit.is_none() {
            // The tail is asked for after the resize, so the redraw it causes comes after the replay, as live output.
            if tx.request_tail(held).is_err() {
                return Err(Refusal::HolderLost);
            }
        }
        term.waiters.push_back(Waiter::Client(h.clone()));
        term.clients.push(h.clone());
        Ok(h)
    }

    /// The client's keystrokes. Only the owner's reach the engine.
    fn input(&self, h: &Handle, bytes: &[u8]) {
        let inner = lock(&self.inner);
        if h.role == Role::Owner
            && !h.gone.load(Ordering::SeqCst)
            && let Some(tx) = &inner.tx
        {
            let _ = tx.input(&h.held, bytes);
        }
    }

    /// The owner's new size. An observer never changes it.
    fn resize(&self, h: &Handle, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let mut inner = lock(&self.inner);
        if h.role != Role::Owner || h.gone.load(Ordering::SeqCst) {
            return;
        }
        let Some(tx) = inner.tx.clone() else { return };
        if tx.resize(&h.held, cols, rows).is_ok()
            && let Some(term) = inner.terms.get_mut(&h.held.id)
        {
            term.cols = cols;
            term.rows = rows;
        }
    }

    /// The client leaves (DETACH, or its connection ended).
    fn leave(&self, h: &Handle) {
        let mut inner = lock(&self.inner);
        h.gone.store(true, Ordering::SeqCst);
        if let Some(term) = inner.terms.get_mut(&h.held.id) {
            term.clients.retain(|c| c.id != h.id);
        }
    }

    /// The held terminals, for `terminal.list`.
    pub(crate) fn list(&self) -> Vec<Value> {
        let inner = lock(&self.inner);
        let mut terms: Vec<&Term> = inner.terms.values().filter(|t| t.exit.is_none()).collect();
        terms.sort_by_key(|t| (t.started, t.held.id));
        terms
            .into_iter()
            .map(|t| {
                let count = |role| {
                    t.clients
                        .iter()
                        .filter(|c| c.role == role && !c.gone.load(Ordering::SeqCst))
                        .count()
                };
                json!({
                    "held_id": {"instance": t.held.instance, "id": t.held.id},
                    "session": t.session,
                    "agent": t.agent,
                    "pid": t.pid,
                    "started": t.started,
                    "owners": count(Role::Owner),
                    "observers": count(Role::Observer),
                    "cols": t.cols,
                    "rows": t.rows,
                })
            })
            .collect()
    }
}

impl Inner {
    /// The engine ended: ask for the last tail; its answer keeps the end and acks it.
    fn begin_end(&mut self, id: u32) {
        let Some(term) = self.terms.get_mut(&id) else {
            return;
        };
        term.waiters.push_back(Waiter::Ender);
        if let Some(tx) = &self.tx {
            let _ = tx.request_tail(&term.held);
        }
    }

    fn purge_ended(&mut self) {
        let now = Instant::now();
        self.ended
            .retain(|_, e| now.duration_since(e.at) < ENDED_KEEP);
    }

    fn keep_ended(&mut self, held: HeldId, ended: Ended) {
        self.purge_ended();
        while self.ended.len() >= MAX_ENDED {
            let Some(oldest) = self
                .ended
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.ended.remove(&oldest);
        }
        self.ended.insert(held, ended);
    }
}

fn exit_frame(exit: &proto::Exit) -> Vec<u8> {
    control(
        attach::EXIT,
        &serde_json::to_value(exit).unwrap_or_else(|_| json!({"code": null, "signal": null})),
    )
}

fn attached_frame(h: &Handle, size: (u16, u16), replay: usize) -> Vec<u8> {
    control(
        attach::ATTACHED,
        &json!({
            "v": attach::VERSION,
            "held_id": {"instance": h.held.instance, "id": h.held.id},
            "role": role_name(h.role),
            "cols": size.0,
            "rows": size.1,
            "replay": replay,
        }),
    )
}

/// ATTACHED and the replay; live output follows.
fn activate(h: &Handle, tail: &[u8], size: (u16, u16)) {
    h.out.push(attached_frame(h, size, tail.len()));
    h.out.push_output(tail, false);
    h.live.store(true, Ordering::SeqCst);
}

/// A client of a terminal that has ended: what it printed, how it ended, and the close.
fn serve_ended(h: &Handle, ended: &Ended) {
    h.out.push(attached_frame(
        h,
        (ended.cols, ended.rows),
        ended.tail.len(),
    ));
    h.out.push_output(&ended.tail, false);
    h.gone.store(true, Ordering::SeqCst);
    h.out.finish(exit_frame(&ended.exit));
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Observer => "observer",
    }
}

// --- the listener ----------------------------------------------------------------------------------------------------

/// Where the daemon of this layout listens for attaches: next to the owner socket.
#[cfg(unix)]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    layout.dir.join("attach.sock")
}

#[cfg(not(unix))]
pub(crate) fn endpoint(layout: &Layout) -> PathBuf {
    crate::owner::pipe_name(layout, "attach")
}

pub(crate) struct Server {
    stop: Arc<AtomicBool>,
    endpoint: PathBuf,
    accept: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Ctx {
    tickets: Arc<Tickets>,
    relay: Arc<Relay>,
    stop: Arc<AtomicBool>,
    active: AtomicUsize,
}

impl Server {
    /// Binds the socket (this daemon holds the lock, so a socket file left by a dead one is not in use and is
    /// replaced) and starts accepting.
    pub(crate) fn bind(
        layout: &Layout,
        tickets: Arc<Tickets>,
        relay: Arc<Relay>,
    ) -> Result<Server> {
        let ep = endpoint(layout);
        platform::remove_endpoint(&ep);
        let listener = platform::listen(&ep).map_err(|e| {
            Error::Io(format!(
                "kith-core: can't listen on the attach socket {}: {e}",
                ep.display()
            ))
        })?;
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::new(Ctx {
            tickets,
            relay,
            stop: stop.clone(),
            active: AtomicUsize::new(0),
        });
        let accept = std::thread::Builder::new()
            .name("kith-attach-accept".into())
            .spawn(move || accept_loop(&listener, &ctx))
            .map_err(|e| Error::Io(format!("kith-core: can't start the attach socket: {e}")))?;
        Ok(Server {
            stop,
            endpoint: ep,
            accept: Mutex::new(Some(accept)),
        })
    }

    /// Stops accepting and removes the socket. Attached clients are cut by the supervisor's end of the link.
    pub(crate) fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // The thread sits in accept: one connection of our own wakes it.
        let _ = platform::connect(&self.endpoint);
        if let Some(handle) = self.accept.lock().ok().and_then(|mut a| a.take()) {
            let _ = handle.join();
        }
        platform::remove_endpoint(&self.endpoint);
    }
}

fn accept_loop(listener: &platform::Listener, ctx: &Arc<Ctx>) {
    loop {
        let stream = match platform::accept(listener) {
            Ok(s) => s,
            Err(e)
                if platform::accept_error_is_temporary(&e) && !ctx.stop.load(Ordering::SeqCst) =>
            {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => break,
        };
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        if ctx.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let spawned = std::thread::Builder::new()
            .name("kith-attach-conn".into())
            .spawn({
                let ctx = ctx.clone();
                move || {
                    serve_one(stream, &ctx);
                    ctx.active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            ctx.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Sends one last frame on a connection nothing else writes to yet, and closes it.
fn refuse(stream: &mut Stream, code: &str) {
    let _ = stream.write_all(&control(attach::ERROR, &json!({"error": code})));
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// What an ATTACH frame says, checked.
struct AttachReq {
    ticket: String,
    role: Role,
    takeover: bool,
    size: Option<(u16, u16)>,
}

fn read_attach(payload: &[u8]) -> std::result::Result<AttachReq, &'static str> {
    let v: Value = serde_json::from_slice(payload).map_err(|_| "bad_attach")?;
    if v["v"].as_u64() != Some(u64::from(attach::VERSION)) {
        return Err("unsupported_version");
    }
    let ticket = v["ticket"].as_str().ok_or("bad_attach")?.to_owned();
    let role = match v["role"].as_str() {
        Some("owner") => Role::Owner,
        Some("observer") => Role::Observer,
        _ => return Err("bad_attach"),
    };
    let dim = |k: &str| {
        v[k].as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| *n > 0)
    };
    Ok(AttachReq {
        ticket,
        role,
        takeover: v["takeover"].as_bool().unwrap_or(false),
        size: dim("cols").zip(dim("rows")),
    })
}

fn serve_one(mut stream: Stream, ctx: &Ctx) {
    // The ATTACH has a time to arrive in: a peer that sends nothing is cut off.
    let (done, waiting) = mpsc::channel::<()>();
    if let Ok(cutter) = stream.try_clone() {
        let _ = std::thread::Builder::new()
            .name("kith-attach-budget".into())
            .spawn(move || {
                if waiting.recv_timeout(FIRST_FRAME) == Err(mpsc::RecvTimeoutError::Timeout) {
                    let _ = cutter.shutdown(std::net::Shutdown::Both);
                }
            });
    }
    let first = wire::read(&mut stream);
    drop(done);
    let Ok(Some((kind, _, payload))) = first else {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    };
    if kind != attach::ATTACH {
        let known = matches!(kind, attach::INPUT | attach::RESIZE | attach::DETACH);
        return refuse(
            &mut stream,
            if known {
                "attach_first"
            } else {
                "unknown_kind"
            },
        );
    }
    let req = match read_attach(&payload) {
        Ok(r) => r,
        Err(code) => return refuse(&mut stream, code),
    };
    let ticket = match ctx.tickets.redeem(&req.ticket) {
        Ok(t) => t,
        Err(TicketRefused::Unknown) => return refuse(&mut stream, "ticket_unknown"),
        Err(TicketRefused::Expired) => return refuse(&mut stream, "ticket_expired"),
    };
    let Some(role) = ticket.grant(req.role, req.takeover) else {
        return refuse(&mut stream, "role_not_allowed");
    };
    let handle = match ctx.relay.attach(&ticket.held, role, req.takeover, req.size) {
        Ok(h) => h,
        Err(why) => return refuse(&mut stream, why.code()),
    };
    let Ok(writer) = stream.try_clone() else {
        ctx.relay.leave(&handle);
        return;
    };
    let started = {
        let handle = handle.clone();
        std::thread::Builder::new()
            .name("kith-attach-write".into())
            .spawn(move || write_loop(&handle, writer))
    };
    if started.is_err() {
        ctx.relay.leave(&handle);
        handle.out.kill();
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    }
    let hung_up = read_loop(&mut stream, ctx, &handle);
    ctx.relay.leave(&handle);
    if hung_up {
        // The client went without a word: nothing queued for it is wanted. (A client that was sent its last frame has
        // it written first: the writer closes the connection, and that is what ended the read.)
        handle.out.kill();
    }
}

/// Writes what the relay queues for one client; ends the connection after the last frame.
fn write_loop(h: &Handle, mut stream: Stream) {
    #[cfg(unix)]
    let _ = stream.set_write_timeout(Some(WRITE_STALL));
    while let Some(frame) = h.out.pop() {
        if stream.write_all(&frame).is_err() {
            h.out.kill();
            break;
        }
    }
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The frames after ATTACH, until the client leaves or its connection ends. True when the connection ended.
fn read_loop(stream: &mut Stream, ctx: &Ctx, h: &Arc<Handle>) -> bool {
    loop {
        let Ok(Some((kind, _, payload))) = wire::read(stream) else {
            return true;
        };
        if payload.len() > attach::MAX_PAYLOAD {
            h.detach_with_error("payload_too_large", &ctx.relay);
            return false;
        }
        match kind {
            attach::INPUT => ctx.relay.input(h, &payload),
            attach::RESIZE => {
                if let Ok(proto::Size { cols, rows }) = serde_json::from_slice(&payload) {
                    ctx.relay.resize(h, cols, rows);
                }
            }
            attach::DETACH => {
                ctx.relay.leave(h);
                h.detach("detach");
                return false;
            }
            attach::ATTACH => {
                h.detach_with_error("already_attached", &ctx.relay);
                return false;
            }
            _ => {
                h.detach_with_error("unknown_kind", &ctx.relay);
                return false;
            }
        }
    }
}

impl Handle {
    /// A refused frame after the attach: ERROR, then the client is out.
    fn detach_with_error(&self, code: &str, relay: &Relay) {
        relay.leave(self);
        self.out
            .finish(control(attach::ERROR, &json!({"error": code})));
    }
}

#[cfg(all(test, unix))]
#[path = "attach_tests.rs"]
mod tests;
