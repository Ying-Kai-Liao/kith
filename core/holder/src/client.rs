//! A blocking client of the holder: what the core does at start (hello, list) and what the tests do. Reads happen
//! on their own thread so a caller can wait with a timeout on any platform.

use crate::auth;
use crate::platform::{self, Stream};
use crate::proto as p;
use crate::wire::{self, Frame};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
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

/// Ends the link when the client goes: shuts a socket down, or kills the child that carries a stdio link.
type Closer = Box<dyn FnMut() + Send>;

pub struct Client {
    writer: Box<dyn Write + Send>,
    close: Closer,
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
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut writer = child.stdin.take().expect("piped");
        let reader = child.stdout.take().expect("piped");
        let mut close: Closer = Box::new(move || {
            let _ = child.kill();
            let _ = child.wait();
        });
        if let Err(e) = writeln!(writer, "{secret}").and_then(|()| writer.flush()) {
            close();
            return Err(e.into());
        }
        Self::over_parts(
            Box::new(reader),
            Box::new(writer),
            close,
            secret,
            generation,
        )
    }

    fn over_parts(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        mut close: Closer,
        secret: &str,
        generation: u64,
    ) -> Result<Client, ConnectError> {
        match Self::handshake(reader, writer, secret, generation) {
            Ok((frames, welcome, writer)) => Ok(Client {
                writer,
                close,
                frames,
                pending: VecDeque::new(),
                welcome,
            }),
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
    ) -> Result<(Receiver<Frame>, p::Welcome, Box<dyn Write + Send>), ConnectError> {
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
                let (tx, frames) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    while let Ok(Some(frame)) = wire::read(&mut reader) {
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                });
                Ok((frames, welcome, writer))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

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
                procs: vec![],
            };
            wire::write_json(&mut fake, p::WELCOME, 0, &welcome).unwrap();
        });
        let result = Client::over(ours, "0123456789abcdef", 1);
        t.join().unwrap();
        assert!(matches!(result, Err(ConnectError::Impostor)));
    }
}
