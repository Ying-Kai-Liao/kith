//! A blocking client of the holder: what the core does at start (hello, list) and what the tests do. Reads happen
//! on their own thread so a caller can wait with a timeout on any platform.

use crate::auth;
use crate::platform::{self, Stream};
use crate::proto as p;
use crate::wire::{self, Frame};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum ConnectError {
    Io(io::Error),
    Refused(p::Refused),
    /// The far end did not prove it holds the secret: not our holder.
    Impostor,
}

impl From<io::Error> for ConnectError {
    fn from(e: io::Error) -> Self {
        ConnectError::Io(e)
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Io(e) => write!(f, "{e}"),
            ConnectError::Impostor => write!(f, "the holder did not prove it knows the secret"),
            ConnectError::Refused(r) => write!(
                f,
                "the holder refused: {:?} (newest generation {})",
                r.why, r.current
            ),
        }
    }
}

fn with(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!(" (it said: {tail})")
    }
}

/// Ends the link when the client goes: shuts a socket down, or kills the child that carries a stdio link.
type Closer = Box<dyn FnMut() + Send>;

/// The doctor's line for a holder that is tied to its starter (Windows only).
pub const TIED_TO_STARTER_NOTE: &str = "The program that keeps your agents running was started inside a Windows job that \
won't let it leave, so stopping or restarting Kith also stops every agent that is running. Nothing is broken now. \
To fix it, quit Kith and Start Kith from the Start menu or a normal terminal (not from a tool that runs it inside a \
job, such as some IDEs, CI runners or service wrappers).";

fn send_on(
    writer: &Mutex<Box<dyn Write + Send>>,
    kind: u8,
    id: u32,
    payload: &[u8],
) -> io::Result<()> {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    wire::write_frame(&mut *w, kind, id, payload)?;
    w.flush()
}

/// Keeps the lease alive from a thread of its own. A renewal is a round: `ping` sends PING(seq); the connection's
/// reader thread answers the holder's PONG(seq) with RENEW(seq, period) at once (it never waits behind a listing),
/// which anchors the deadline at the hand's clock when it answered. A holder that says LEASE_LOST gets a fresh ping.
#[derive(Clone)]
pub struct Renewer {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    seq: Arc<AtomicU64>,
}

impl Renewer {
    pub fn ping(&self) -> io::Result<()> {
        ping_on(&self.writer, &self.seq)
    }
}

fn ping_on(writer: &Mutex<Box<dyn Write + Send>>, seq: &AtomicU64) -> io::Result<()> {
    let seq = seq.fetch_add(1, Ordering::Relaxed) + 1;
    let payload = serde_json::to_vec(&p::Ping { seq }).map_err(io::Error::other)?;
    send_on(writer, p::PING, 0, &payload)
}

/// The ping a LEASE_LOST asks for, at most one outstanding: a loop of refused tagged spawns must not push the
/// holder's ring of answered pings (8) past a ping whose RENEW is still on its way. The next PONG frees it.
fn ping_after_lost(
    outstanding: &AtomicBool,
    writer: &Mutex<Box<dyn Write + Send>>,
    seq: &AtomicU64,
) -> io::Result<()> {
    if outstanding.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    ping_on(writer, seq)
}

fn renew_on(writer: &Mutex<Box<dyn Write + Send>>, period: Duration, seq: u64) -> io::Result<()> {
    let ms = u64::try_from(period.as_millis()).unwrap_or(u64::MAX);
    let payload = serde_json::to_vec(&p::Renew { ms, seq }).map_err(io::Error::other)?;
    send_on(writer, p::RENEW, 0, &payload)
}

/// Sends commands for held processes on a client's connection from other threads: the core's attach relay forwards a
/// viewer's keystrokes and size, and asks for tails, without going through the thread that owns the [`Client`]. A frame
/// is written whole under the connection's lock, so it never interleaves with another thread's.
#[derive(Clone)]
pub struct Sender(Arc<Mutex<Box<dyn Write + Send>>>);

impl Sender {
    fn send_for(&self, kind: u8, h: &p::HeldId, body: &[u8]) -> io::Result<()> {
        let mut payload = h.instance.as_bytes().to_vec();
        payload.extend_from_slice(body);
        send_on(&self.0, kind, h.id, &payload)
    }

    /// Raw bytes to a process. Empty bytes are not sent: to a holder an empty INPUT closes a pipes process's stdin.
    pub fn input(&self, h: &p::HeldId, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.send_for(p::INPUT, h, bytes)
    }

    pub fn resize(&self, h: &p::HeldId, cols: u16, rows: u16) -> io::Result<()> {
        let body = serde_json::to_vec(&p::Size { cols, rows }).expect("serializable");
        self.send_for(p::RESIZE, h, &body)
    }

    /// Asks for the process's tail. The answer (TAILED) arrives on the client's frames, in order with its OUTPUT.
    pub fn request_tail(&self, h: &p::HeldId) -> io::Result<()> {
        self.send_for(p::TAIL, h, &[])
    }
}

pub struct Client {
    /// Shared with a [`Renewer`]: a frame is written whole under the lock.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    close: Closer,
    /// Set by `answer_pongs`: the reader thread then answers every PONG with a RENEW for this long.
    lease: Arc<Mutex<Option<Duration>>>,
    seq: Arc<AtomicU64>,
    frames: Receiver<Frame>,
    /// Frames read but not yet asked for, in order.
    pending: VecDeque<Frame>,
    pub welcome: p::Welcome,
}

impl Drop for Client {
    /// The reader thread holds the other end of the connection; ending it is what hangs up.
    fn drop(&mut self) {
        (self.close)();
    }
}

impl Client {
    /// Connects to the holder whose folder is `dir` and says hello.
    pub fn connect(dir: &Path, secret: &str, generation: u64) -> Result<Client, ConnectError> {
        Self::over(
            platform::connect(&platform::endpoint(dir))?,
            secret,
            generation,
        )
    }

    /// Says hello over a connection already made (any stream the platform gives).
    pub fn over(stream: Stream, secret: &str, generation: u64) -> Result<Client, ConnectError> {
        let writer = stream.try_clone()?;
        let closer = stream.try_clone()?;
        Self::over_parts(
            Box::new(stream),
            Box::new(writer),
            Box::new(move || {
                let _ = closer.shutdown(std::net::Shutdown::Both);
            }),
            secret,
            generation,
        )
    }

    /// Says hello to a holder reached through a child process's stdio: `ssh <host> kith-holder --stdio`, or in tests
    /// `kith-holder --stdio --dir D` run directly. The child relays to the holder of its machine (starting one
    /// detached if none runs), so ending the child never ends a held process. The pairing key goes down the child's
    /// stdin as its first line (it is what a relay needs to start a holder), never in argv or the environment, and
    /// then the usual hello and mutual proof follow over the same pipes. Dropping the client kills the child.
    pub fn over_command(
        mut cmd: Command,
        secret: &str,
        generation: u64,
        handshake: Duration,
    ) -> Result<Client, ConnectError> {
        if secret.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the pairing key cannot hold a line break",
            )
            .into());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut writer = child.stdin.take().expect("piped");
        let reader = child.stdout.take().expect("piped");
        // The command's last few stderr lines (ssh says why it could not connect there), for the error.
        let said: Arc<Mutex<VecDeque<String>>> = Arc::default();
        if let Some(err) = child.stderr.take() {
            let said = said.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    let mut said = said.lock().unwrap_or_else(|e| e.into_inner());
                    said.push_back(line);
                    if said.len() > 5 {
                        said.pop_front();
                    }
                }
            });
        }
        let child = Arc::new(Mutex::new(child));
        let kill = {
            let child = child.clone();
            move || {
                let mut child = child.lock().unwrap_or_else(|e| e.into_inner());
                let _ = child.kill();
                let _ = child.wait();
            }
        };
        // Pipes have no read timeout: a command that connects and then says nothing would hold the caller for good.
        // The watchdog ends the child, which ends the read; it stands down the moment the handshake is over.
        let (done, waiting) = std::sync::mpsc::channel::<()>();
        let timed_out = Arc::new(AtomicBool::new(false));
        {
            let (kill, timed_out) = (kill.clone(), timed_out.clone());
            std::thread::spawn(move || {
                if waiting.recv_timeout(handshake) == Err(RecvTimeoutError::Timeout) {
                    timed_out.store(true, Ordering::SeqCst);
                    kill();
                }
            });
        }
        let close: Closer = Box::new(kill);
        let result = match writeln!(writer, "{secret}").and_then(|()| writer.flush()) {
            Ok(()) => Self::over_parts(
                Box::new(reader),
                Box::new(writer),
                close,
                secret,
                generation,
            ),
            Err(e) => {
                let mut close = close;
                close();
                Err(e.into())
            }
        };
        drop(done);
        result.map_err(|e| {
            let tail = said.lock().unwrap_or_else(|e| e.into_inner());
            let tail = tail.iter().cloned().collect::<Vec<_>>().join(" / ");
            match e {
                ConnectError::Io(_) if timed_out.load(Ordering::SeqCst) => io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no welcome within {handshake:?}{}", with(&tail)),
                )
                .into(),
                ConnectError::Io(e) => {
                    io::Error::new(e.kind(), format!("{e}{}", with(&tail))).into()
                }
                other => other,
            }
        })
    }

    fn over_parts(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        mut close: Closer,
        secret: &str,
        generation: u64,
    ) -> Result<Client, ConnectError> {
        match Self::handshake(reader, writer, secret, generation) {
            Ok((mut reader, welcome, writer)) => {
                let writer = Arc::new(Mutex::new(writer));
                let lease: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
                let seq = Arc::new(AtomicU64::new(0));
                let (tx, frames) = std::sync::mpsc::channel();
                let (w, l, n) = (writer.clone(), lease.clone(), seq.clone());
                let lost_ping = Arc::new(AtomicBool::new(false));
                std::thread::spawn(move || {
                    while let Ok(Some(frame)) = wire::read(&mut reader) {
                        // The lease round is answered here, not by whoever reads the frames next: a listing or a
                        // reconcile must never delay a RENEW (it is the core's promise of "L from the ping").
                        let period = *l.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(period) = period {
                            match frame.0 {
                                p::PONG => {
                                    lost_ping.store(false, Ordering::SeqCst);
                                    if let Ok(pong) = wire::parse::<p::Pong>(&frame.2) {
                                        let _ = renew_on(&w, period, pong.seq);
                                    }
                                    continue;
                                }
                                // Nothing started and nothing armed: ask again at once instead of waiting for
                                // the next tick. The frame is still passed on.
                                p::LEASE_LOST => {
                                    let _ = ping_after_lost(&lost_ping, &w, &n);
                                }
                                _ => {}
                            }
                        }
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                });
                Ok(Client {
                    writer,
                    close,
                    lease,
                    seq,
                    frames,
                    pending: VecDeque::new(),
                    welcome,
                })
            }
            // A refused or silent link leaves nothing behind (no child, no open socket).
            Err(e) => {
                close();
                Err(e)
            }
        }
    }

    /// The hello and the mutual proof; on success the reader moves to a thread of its own.
    #[allow(clippy::type_complexity)]
    fn handshake(
        mut reader: Box<dyn Read + Send>,
        mut writer: Box<dyn Write + Send>,
        secret: &str,
        generation: u64,
    ) -> Result<(Box<dyn Read + Send>, p::Welcome, Box<dyn Write + Send>), ConnectError> {
        let key = secret.as_bytes();
        let nonce = platform::random_hex();
        let hello = p::Hello {
            version: p::VERSION,
            generation,
            nonce: nonce.clone(),
        };
        wire::write_json(&mut writer, p::HELLO, 0, &hello)?;
        let mut first = wire::read(&mut reader)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let proved = first.0 == p::CHALLENGE;
        if proved {
            // The holder goes first: nothing is answered to a peer that cannot prove the key.
            let challenge: p::Challenge = wire::parse(&first.2)?;
            let expect = auth::proof(key, auth::HOLDER, &nonce, &challenge.nonce, generation);
            if !auth::same(expect.as_bytes(), challenge.proof.as_bytes()) {
                return Err(ConnectError::Impostor);
            }
            let mine = auth::proof(key, auth::CORE, &nonce, &challenge.nonce, generation);
            wire::write_json(&mut writer, p::AUTH, 0, &p::Auth { proof: mine })?;
            first = wire::read(&mut reader)?
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        }
        if !proved && first.0 != p::REFUSED {
            // A welcome that skipped the challenge could carry a made-up listing.
            return Err(ConnectError::Impostor);
        }
        match first {
            (p::WELCOME, _, payload) => {
                let welcome: p::Welcome = wire::parse(&payload)?;
                Ok((reader, welcome, writer))
            }
            (p::REFUSED, _, payload) => Err(ConnectError::Refused(wire::parse(&payload)?)),
            (kind, _, _) => {
                Err(io::Error::other(format!("unexpected frame {kind} after hello")).into())
            }
        }
    }

    /// True when this holder cannot outlive the process that started it (Windows, job without breakaway): what
    /// `kith doctor` shows.
    pub fn tied_to_starter(&self) -> bool {
        self.welcome.tied_to_starter
    }

    /// What `kith doctor` says when [`Self::tied_to_starter`]: what it means and what to do, in plain words. None when
    /// the holder is free of its starter.
    pub fn tied_to_starter_note(&self) -> Option<&'static str> {
        self.tied_to_starter().then_some(TIED_TO_STARTER_NOTE)
    }

    pub fn send(&mut self, kind: u8, id: u32, payload: &[u8]) -> io::Result<()> {
        send_on(&self.writer, kind, id, payload)
    }

    /// A handle that sends commands for held processes from other threads.
    pub fn sender(&self) -> Sender {
        Sender(self.writer.clone())
    }

    /// A handle that renews the lease from another thread, so renewals never wait behind a listing.
    pub fn renewer(&self) -> Renewer {
        Renewer {
            writer: self.writer.clone(),
            seq: self.seq.clone(),
        }
    }

    /// From now on the connection answers every PONG with a RENEW for `period`. Send a ping (a [`Renewer`]) to start
    /// a round.
    pub fn answer_pongs(&self, period: Duration) {
        *self.lease.lock().unwrap_or_else(|p| p.into_inner()) = Some(period);
    }

    /// A command for process `id` of holder `instance`: the instance goes first in the payload (see proto.rs).
    pub fn send_for(&mut self, kind: u8, instance: &str, id: u32, body: &[u8]) -> io::Result<()> {
        let mut payload = instance.as_bytes().to_vec();
        payload.extend_from_slice(body);
        self.send(kind, id, &payload)
    }

    /// Raw bytes to a process; one held by another instance than this holder ignores them.
    pub fn input(&mut self, h: &p::HeldId, bytes: &[u8]) -> io::Result<()> {
        self.send_for(p::INPUT, &h.instance, h.id, bytes)
    }

    pub fn resize(&mut self, h: &p::HeldId, cols: u16, rows: u16) -> io::Result<()> {
        let body = serde_json::to_vec(&p::Size { cols, rows }).expect("serializable");
        self.send_for(p::RESIZE, &h.instance, h.id, &body)
    }

    pub fn send_json(&mut self, kind: u8, id: u32, msg: &impl serde::Serialize) -> io::Result<()> {
        self.send(kind, id, &serde_json::to_vec(msg).expect("serializable"))
    }

    /// The next frame accepted by `want`, waiting up to `timeout`; frames it passes over stay queued in order.
    pub fn take(
        &mut self,
        timeout: Duration,
        mut want: impl FnMut(&Frame) -> bool,
    ) -> Option<Frame> {
        if let Some(i) = self.pending.iter().position(&mut want) {
            return self.pending.remove(i);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.frames.recv_timeout(left) {
                Ok(frame) if want(&frame) => return Some(frame),
                Ok(frame) => self.pending.push_back(frame),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Waits for a frame of `kind` (for `id`, unless it is 0).
    pub fn expect(&mut self, kind: u8, id: u32, timeout: Duration) -> Option<Frame> {
        self.take(timeout, |(k, i, _)| *k == kind && (id == 0 || *i == id))
    }

    /// True once the holder has closed the connection (after any queued frames).
    pub fn closed(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.frames.recv_timeout(left) {
                Ok(frame) => self.pending.push_back(frame),
                Err(RecvTimeoutError::Disconnected) => return true,
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }

    pub fn spawn(&mut self, spec: &p::Spawn) -> io::Result<()> {
        self.send_json(p::SPAWN, 0, spec)
    }

    /// One whole lease round, waiting for the PONG: the lease then lasts `period` from the moment the holder answered
    /// it. Fails if the holder does not answer (a superseded or closed connection).
    pub fn renew(&mut self, period: Duration) -> io::Result<()> {
        self.renewer().ping()?;
        let pong = self
            .expect(p::PONG, 0, Duration::from_secs(5))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        let pong: p::Pong = wire::parse(&pong.2)?;
        renew_on(&self.writer, period, pong.seq)
    }

    pub fn list(&mut self) -> io::Result<Vec<p::Held>> {
        self.send(p::LIST, 0, &[])?;
        let (_, _, payload) = self
            .expect(p::LISTING, 0, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Listing>(&payload)?.procs)
    }

    pub fn kill(&mut self, h: &p::HeldId) -> io::Result<p::KillState> {
        self.send_for(p::KILL, &h.instance, h.id, &[])?;
        let (_, _, payload) = self
            .expect(p::KILLED, h.id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Killed>(&payload)?.state)
    }

    pub fn ack(&mut self, h: &p::HeldId) -> io::Result<p::AckState> {
        self.send_for(p::ACK, &h.instance, h.id, &[])?;
        let (_, _, payload) = self
            .expect(p::ACKED, h.id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Acked>(&payload)?.state)
    }

    /// Everything the process printed so far that is still in the holder's tail.
    pub fn tail(&mut self, h: &p::HeldId) -> io::Result<Vec<u8>> {
        self.send_for(p::TAIL, &h.instance, h.id, &[])?;
        let (_, _, payload) = self
            .expect(p::TAILED, h.id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(payload)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_loop_of_lease_lost_sends_one_ping_until_a_pong_frees_it() {
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Mutex<Box<dyn Write + Send>> = Mutex::new(Box::new(Sink(bytes.clone())));
        let (out, seq) = (AtomicBool::new(false), AtomicU64::new(0));
        for _ in 0..20 {
            ping_after_lost(&out, &writer, &seq).unwrap();
        }
        assert_eq!(seq.load(Ordering::Relaxed), 1, "one ping for the burst");
        out.store(false, Ordering::SeqCst); // what a PONG does
        ping_after_lost(&out, &writer, &seq).unwrap();
        assert_eq!(seq.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn the_tied_note_says_what_it_means_and_what_to_do() {
        assert!(TIED_TO_STARTER_NOTE.contains("also stops"));
        assert!(TIED_TO_STARTER_NOTE.contains("Start Kith from"));
        assert!(
            !TIED_TO_STARTER_NOTE.contains("starter"),
            "no field names in a user's words"
        );
    }

    #[test]
    fn a_welcome_that_skips_the_challenge_is_an_impostor() {
        let (ours, mut fake) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || {
            wire::read(&mut fake).unwrap().expect("hello");
            let welcome = p::Welcome {
                version: p::VERSION,
                instance: "fake".into(),
                pid: 1,
                tied_to_starter: false,
                generation: 1,
                lease_lost: false,
                procs: vec![],
            };
            wire::write_json(&mut fake, p::WELCOME, 0, &welcome).unwrap();
        });
        let result = Client::over(ours, "0123456789abcdef", 1);
        t.join().unwrap();
        assert!(matches!(result, Err(ConnectError::Impostor)));
    }
}
