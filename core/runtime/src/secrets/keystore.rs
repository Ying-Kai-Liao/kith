//! Where the one master key per machine lives: the operating system's credential store, behind [`KeyStore`].
//!
//! The backends here drive the OS's own command line tool, and the key goes to it on its stdin, never on an argv or in
//! an environment (a process list shows both). The signed daemon reads the key through Security.framework instead
//! (M2-6); this macOS backend is the development one. It shares the item name with the signed daemon for now (M2-6 gives
//! the dev backend its own name or re-creates the item, since a key item is bound to the app that made it).
//! There is no file fallback and no in-memory backend a running binary can select: [`MemoryKeyStore`] exists for tests.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Bytes in the master key (XChaCha20-Poly1305).
pub(crate) const KEY_LEN: usize = 32;

/// Item name in the OS store. One per machine, so the service names the product and the account names the use.
const SERVICE: &str = "dev.kith.core.secrets";
const ACCOUNT: &str = "master-key";

/// A locked Keychain can sit on an access prompt nobody sees; a call that long is a refusal, not a hang.
/// Absolute program paths only: a PATH lookup would let a planted binary read or forge the master key.
const SECURITY: &str = "/usr/bin/security";
const SECRET_TOOL: &str = "/usr/bin/secret-tool";

const WAIT: Duration = Duration::from_secs(30);

/// The OS store holding the master key. Both calls are all-or-nothing: `get` answers `Ok(None)` only when the store
/// says plainly that there is no such item, and every other trouble (locked, denied, tool missing) is an `Err`, so a
/// caller can never mistake "could not read" for "no key".
pub(crate) trait KeyStore: Send {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    /// Writes the key. macOS adds only (an existing item is kept); Linux's `secret-tool store` always replaces, so two
    /// cores minting at once can still race there. The caller reads back and adopts whatever key the store holds.
    fn add(&self, key: &[u8]) -> Result<()>;
}

/// The store of this operating system, or a refusal where there is none. Never an in-memory or file fallback.
#[allow(
    clippy::unnecessary_wraps,
    reason = "only the unsupported platforms refuse"
)]
pub(crate) fn os_key_store() -> Result<Box<dyn KeyStore>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(SecurityCli::new(SERVICE, ACCOUNT)))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(SecretTool::new(SERVICE, ACCOUNT)))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Windows Credential Manager needs raw Win32 calls, which this crate forbids (unsafe_code); it lands with the
        // signed daemon's backend. Until then a Windows core refuses secrets rather than keep a key somewhere weaker.
        Err(Error::Io(
            "kith-core: this operating system has no supported key store for the secrets envelope yet; secrets are unavailable"
                .into(),
        ))
    }
}

/// macOS: `/usr/bin/security`, a generic password item. The value is written with `security -i` (the command line,
/// value included, arrives on stdin) as hex so no quoting rules apply, exactly as kith-auth does.
pub(crate) struct SecurityCli {
    service: String,
    account: String,
}

impl SecurityCli {
    pub(crate) fn new(service: &str, account: &str) -> Self {
        // Both go into a `security -i` command line, so keep them to characters that need no quoting.
        debug_assert!(is_plain(service) && is_plain(account));
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn remove(&self) {
        let _ = run(
            SECURITY,
            &[
                "delete-generic-password",
                "-s",
                &self.service,
                "-a",
                &self.account,
            ],
            None,
        );
    }
}

impl KeyStore for SecurityCli {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let out = run(
            SECURITY,
            &[
                "find-generic-password",
                "-s",
                &self.service,
                "-a",
                &self.account,
                "-w",
            ],
            None,
        )?;
        match out.code {
            Some(0) => decode_stored(&out.stdout).map(Some),
            // errSecItemNotFound: the one answer that means "no key here".
            Some(44) => Ok(None),
            code => Err(Error::Io(format!(
                "kith-core: the Keychain would not give up the secrets master key (security exited {})",
                code.map_or_else(|| "by signal".into(), |c| c.to_string())
            ))),
        }
    }

    fn add(&self, key: &[u8]) -> Result<()> {
        // The stored text is the key as hex; `-X` takes the bytes of that text as hex again.
        let text = hex_encode(key);
        let line = Zeroizing::new(format!(
            "add-generic-password -s {} -a {} -X {}\n",
            self.service,
            self.account,
            hex_encode(text.as_bytes()).as_str()
        ));
        // No `-U`: an item that already exists is kept, so the loser of a mint race does not overwrite the winner's key.
        // `-i` exits 0 whatever happened; the caller's read-back is the proof.
        run(SECURITY, &["-i"], Some(line.as_bytes()))?;
        Ok(())
    }
}

/// Linux: the Secret Service through `secret-tool` (libsecret-tools). It reads the password from stdin.
pub(crate) struct SecretTool {
    service: String,
    account: String,
}

impl SecretTool {
    pub(crate) fn new(service: &str, account: &str) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn remove(&self) {
        let _ = run(
            SECRET_TOOL,
            &["clear", "service", &self.service, "account", &self.account],
            None,
        );
    }
}

impl KeyStore for SecretTool {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let out = run(
            SECRET_TOOL,
            &["lookup", "service", &self.service, "account", &self.account],
            None,
        )?;
        match out.code {
            Some(0) => decode_stored(&out.stdout).map(Some),
            // No item: exit 1 and nothing on either stream. Exit 1 with a message is a failure (no daemon, locked).
            Some(1) if out.stdout.is_empty() && out.stderr.trim().is_empty() => Ok(None),
            code => Err(Error::Io(format!(
                "kith-core: the Secret Service would not give up the secrets master key (secret-tool exited {}: {})",
                code.map_or_else(|| "by signal".into(), |c| c.to_string()),
                out.stderr.trim()
            ))),
        }
    }

    fn add(&self, key: &[u8]) -> Result<()> {
        let text = hex_encode(key);
        let out = run(
            SECRET_TOOL,
            &[
                "store",
                "--label=Kith secrets master key",
                "service",
                &self.service,
                "account",
                &self.account,
            ],
            Some(text.as_bytes()),
        )?;
        if out.code == Some(0) {
            Ok(())
        } else {
            Err(Error::Io(format!(
                "kith-core: the Secret Service would not store the secrets master key: {}",
                out.stderr.trim()
            )))
        }
    }
}

/// Tests only: a store in memory. Clones share the one slot, so a test can open the "same machine" twice, or hand a
/// different slot to "another machine". Not compiled into a running binary, so no setting can select it.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct MemoryKeyStore {
    slot: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    unreadable: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl MemoryKeyStore {
    /// Makes `get` fail the way a locked Keychain does.
    pub(crate) fn set_unreadable(&self, yes: bool) {
        self.unreadable
            .store(yes, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn forget(&self) {
        *self.slot.lock().expect("slot") = None;
    }

    pub(crate) fn peek(&self) -> Option<Vec<u8>> {
        self.slot.lock().expect("slot").clone()
    }
}

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        if self.unreadable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Io("kith-core: the key store is locked".into()));
        }
        Ok(self.slot.lock().expect("slot").clone().map(Zeroizing::new))
    }

    fn add(&self, key: &[u8]) -> Result<()> {
        *self.slot.lock().expect("slot") = Some(key.to_vec());
        Ok(())
    }
}

fn is_plain(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> Zeroizing<String> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = Zeroizing::new(String::with_capacity(bytes.len() * 2));
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

pub(crate) fn hex_decode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let raw = text.as_bytes();
    if raw.len() & 1 == 1 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = Zeroizing::new(Vec::with_capacity(raw.len() / 2));
    for pair in raw.chunks(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Some(out)
}

/// The key as a store printed it: hex text, maybe with a trailing newline.
fn decode_stored(stdout: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let text = std::str::from_utf8(stdout).ok().map(str::trim);
    text.and_then(hex_decode).ok_or_else(|| {
        Error::Io(
            "kith-core: the secrets master key in the key store is not in the form this core wrote; it was left untouched"
                .into(),
        )
    })
}

struct Ran {
    code: Option<i32>,
    stdout: Zeroizing<Vec<u8>>,
    stderr: String,
}

/// Runs `program` with `stdin` fed to it, waiting at most [`WAIT`]. Output is captured, never shown or logged.
fn run(program: &str, args: &[&str], stdin: Option<&[u8]>) -> Result<Ran> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::Io(format!(
                "kith-core: cannot run {program} for the key store: {e}"
            ))
        })?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let sent = pipe.write_all(data);
        drop(pipe); // EOF, so the tool stops reading
        if let Err(e) = sent {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Io(format!(
                "kith-core: {program} did not take its input: {e}"
            )));
        }
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    // The readers hand their bytes over a channel, so a pipe a grandchild keeps open (libsecret's D-Bus autolaunch) can
    // not hold this call past the deadline.
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut v = Zeroizing::new(Vec::new());
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut v);
        }
        let _ = out_tx.send(v);
    });
    std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut v);
        }
        let _ = err_tx.send(v);
    });
    let deadline = Instant::now() + WAIT;
    let too_slow = || {
        Error::Io(format!(
            "kith-core: {program} did not answer in {}s (a locked key store waiting for an unlock?)",
            WAIT.as_secs()
        ))
    };
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(too_slow());
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let left = |d: Instant| d.saturating_duration_since(Instant::now());
    let stdout = out_rx
        .recv_timeout(left(deadline))
        .map_err(|_| too_slow())?;
    let stderr = String::from_utf8_lossy(
        &err_rx
            .recv_timeout(left(deadline))
            .map_err(|_| too_slow())?,
    )
    .into_owned();
    Ok(Ran {
        code: status.code(),
        stdout,
        stderr,
    })
}
