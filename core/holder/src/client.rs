//! A blocking client of the holder: what the core does at start (hello, list) and what the tests do. Reads happen
//! on their own thread so a caller can wait with a timeout on any platform.

use crate::platform::{self, Stream};
use crate::proto as p;
use crate::wire::{self, Frame};
use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum ConnectError {
    Io(io::Error),
    Refused(p::Refused),
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
            ConnectError::Refused(r) => write!(
                f,
                "the holder refused: {:?} (newest generation {})",
                r.why, r.current
            ),
        }
    }
}

pub struct Client {
    writer: Stream,
    frames: Receiver<Frame>,
    /// Frames read but not yet asked for, in order.
    pending: VecDeque<Frame>,
    pub welcome: p::Welcome,
}

impl Drop for Client {
    /// The reader thread holds the other end of the connection; ending it is what hangs up.
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
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
        let mut writer = stream.try_clone()?;
        let mut reader = stream;
        let hello = p::Hello {
            version: p::VERSION,
            secret: secret.to_string(),
            generation,
        };
        wire::write_json(&mut writer, p::HELLO, 0, &hello)?;
        let first = wire::read(&mut reader)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        match first {
            (p::WELCOME, _, payload) => {
                let welcome: p::Welcome = wire::parse(&payload)?;
                let (tx, frames) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    while let Ok(Some(frame)) = wire::read(&mut reader) {
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                });
                Ok(Client {
                    writer,
                    frames,
                    pending: VecDeque::new(),
                    welcome,
                })
            }
            (p::REFUSED, _, payload) => Err(ConnectError::Refused(wire::parse(&payload)?)),
            (kind, _, _) => {
                Err(io::Error::other(format!("unexpected frame {kind} after hello")).into())
            }
        }
    }

    pub fn send(&mut self, kind: u8, id: u32, payload: &[u8]) -> io::Result<()> {
        wire::write_frame(&mut self.writer, kind, id, payload)?;
        self.writer.flush()
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

    pub fn list(&mut self) -> io::Result<Vec<p::Held>> {
        self.send(p::LIST, 0, &[])?;
        let (_, _, payload) = self
            .expect(p::LISTING, 0, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Listing>(&payload)?.procs)
    }

    pub fn kill(&mut self, id: u32) -> io::Result<p::KillState> {
        self.send(p::KILL, id, &[])?;
        let (_, _, payload) = self
            .expect(p::KILLED, id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Killed>(&payload)?.state)
    }

    pub fn ack(&mut self, id: u32) -> io::Result<p::AckState> {
        self.send(p::ACK, id, &[])?;
        let (_, _, payload) = self
            .expect(p::ACKED, id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(wire::parse::<p::Acked>(&payload)?.state)
    }

    /// Everything the process printed so far that is still in the holder's tail.
    pub fn tail(&mut self, id: u32) -> io::Result<Vec<u8>> {
        self.send(p::TAIL, id, &[])?;
        let (_, _, payload) = self
            .expect(p::TAILED, id, Duration::from_secs(10))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
        Ok(payload)
    }
}
