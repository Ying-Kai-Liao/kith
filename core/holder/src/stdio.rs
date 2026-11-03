//! `kith-holder --stdio`: the holder reachable over a remote shell's stdio (`ssh host kith-holder --stdio`).
//!
//! The relay is a byte pipe and nothing else: it makes sure the machine's holder runs (starting one detached, as
//! `start` does, if none answers), connects to it, and copies bytes both ways between its stdin/stdout and the
//! holder's socket. The frame protocol, the mutual proof and the generation fence all run end to end between the
//! core and the holder, so the relay can neither forge nor read anything it could use. Ending the relay (the shell
//! session dropped) hangs up on the holder like any closed connection; the holder keeps its processes and a
//! reconnecting core resumes from its backlog.
//!
//! The pairing key is the first line of stdin, like `start`'s secret: it never travels in the command line or the
//! environment. The relay needs it only to start a holder when none runs.

use crate::{launch, platform};
use std::io::{self, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Relays `input` and `output` to the holder of `dir` until either side closes. `exe` is the holder program to
/// start when none answers.
pub fn relay(
    exe: &Path,
    dir: &Path,
    idle: Duration,
    input: impl Read + Send + 'static,
    mut output: impl Write + Send + 'static,
) -> io::Result<()> {
    let mut input = BufReader::new(input);
    let secret = launch::read_secret(&mut input)?;
    launch::ensure(exe, dir, &secret, idle)?;
    let stream = platform::connect(&platform::endpoint(dir))?;
    let (mut to_holder, closer) = (stream.try_clone()?, stream.try_clone()?);
    let mut from_holder = stream;
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut input, &mut to_holder);
        // The far end hung up: let the holder see the connection end.
        let _ = to_holder.shutdown(std::net::Shutdown::Both);
    });
    let mut buf = [0u8; 16 * 1024];
    let result = loop {
        match from_holder.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                if let Err(e) = output.write_all(&buf[..n]).and_then(|()| output.flush()) {
                    break Err(e);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => break Err(e),
        }
    };
    let _ = closer.shutdown(std::net::Shutdown::Both);
    // The input thread may be blocked reading stdin with nobody left to answer; the process exit ends it.
    if up.is_finished() {
        let _ = up.join();
    }
    result
}
