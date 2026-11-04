//! The secrets side of the import: the Keychain items `kith-auth set` writes (`keychain:SERVICE#ACCOUNT` references in
//! auth.json's `profiles`) sealed into the envelope, one record each, named by the reference itself so the switch to
//! kith-auth (M2-18) finds them by the name it already holds.
//!
//! Rules:
//! - Only through the envelope's own API (`secrets::Envelope`): no plaintext fallback, no second copy of the crypto.
//! - Only where the store is available. An item that cannot be read (no Keychain on this OS, locked, a prompt nobody
//!   answered) is skipped with a line; the source is never touched. Reading the Keychain is read-only.
//! - A record that already exists is kept as it is (a second run adds nothing, a rotated value is not overwritten).
//! - A value is held in a `Zeroizing` buffer and never printed; the report carries names and counts only.
//! - The host's vault (serve.db `kv` rows `vault:v1:...`) is NOT read here: the host moves those itself, once, at its
//!   first unlock after M2-7 (`move_old_records` in the host's vault), so a second mover would only race it. Later.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::secrets::Envelope;

const SECURITY: &str = "/usr/bin/security";
const WAIT: Duration = Duration::from_secs(10);
/// `security`'s exit code for "item not found".
const NOT_FOUND: i32 = 44;
/// A record name's longest length (the envelope's own limit).
const MAX_NAME: usize = 256;

/// What reading one item came to.
pub(crate) enum Item {
    Found(Zeroizing<String>),
    Missing,
    /// Could not be read at all; the text says why, without any value.
    Unavailable(String),
}

pub(crate) trait ItemSource {
    fn read(&self, service: &str, account: &str) -> Item;
}

/// The macOS Keychain, through `/usr/bin/security` (the absolute path: a PATH lookup could be a planted binary).
pub(crate) struct Keychain;

impl ItemSource for Keychain {
    fn read(&self, service: &str, account: &str) -> Item {
        if !cfg!(target_os = "macos") {
            return Item::Unavailable("no Keychain on this operating system".into());
        }
        let child = Command::new(SECURITY)
            .args(["find-generic-password", "-s", service, "-a", account, "-w"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            return Item::Unavailable("could not run security".into());
        };
        let Some(mut out) = child.stdout.take() else {
            return Item::Unavailable("could not read from security".into());
        };
        // Read on a thread so a prompt nobody answers cannot hang `kith migrate`.
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            Zeroizing::new(buf)
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if started.elapsed() < WAIT => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Item::Unavailable(
                        "the Keychain did not answer (an access prompt may be waiting)".into(),
                    );
                }
            }
        };
        let Ok(bytes) = reader.join() else {
            return Item::Unavailable("could not read from security".into());
        };
        match status.code() {
            Some(0) => {}
            Some(NOT_FOUND) => return Item::Missing,
            Some(c) => return Item::Unavailable(format!("security exited {c}")),
            None => return Item::Unavailable("security was stopped".into()),
        }
        // As `kith-auth`'s `resolve_ref` reads it: the text without its final newline; nothing is no secret.
        match std::str::from_utf8(&bytes) {
            Ok(s) => {
                let s = Zeroizing::new(s.trim_end_matches('\n').to_owned());
                if s.is_empty() {
                    Item::Missing
                } else {
                    Item::Found(s)
                }
            }
            Err(_) => Item::Unavailable("the item is not text".into()),
        }
    }
}

/// Where sealed values go.
pub(crate) trait SecretSink {
    fn names(&mut self, conn: &Connection) -> Result<BTreeSet<String>>;
    fn put(&mut self, conn: &Connection, name: &str, value: &[u8]) -> Result<()>;
}

/// The envelope over this OS's key store, made on the first put (it may refuse).
#[derive(Default)]
pub(crate) struct OsEnvelope(Option<Envelope>);

impl SecretSink for OsEnvelope {
    fn names(&mut self, conn: &Connection) -> Result<BTreeSet<String>> {
        Ok(Envelope::list(conn)?.into_iter().collect())
    }

    fn put(&mut self, conn: &Connection, name: &str, value: &[u8]) -> Result<()> {
        if self.0.is_none() {
            self.0 = Some(Envelope::os()?);
        }
        self.0
            .as_mut()
            .ok_or_else(|| Error::Io("kith-core: the secrets envelope is unavailable".into()))?
            .put(conn, name, value)
    }
}

/// A `keychain:SERVICE#ACCOUNT` reference of a profile.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Reference {
    /// The whole reference: the record's name in the envelope.
    pub name: String,
    pub service: String,
    pub account: String,
}

/// The Keychain references in `profiles` (profile -> VAR -> source), sorted, each once. A reference that is not usable as
/// a record name (too long, a control character) comes back in the second list with the reason.
pub(crate) fn references(auth: &Value) -> (Vec<Reference>, Vec<String>) {
    let mut found = BTreeSet::new();
    let mut unusable = Vec::new();
    for (profile, vars) in auth
        .get("profiles")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        for (var, source) in vars.as_object().into_iter().flatten() {
            let Some(rest) = source.as_str().and_then(|s| s.strip_prefix("keychain:")) else {
                continue;
            };
            let name = format!("keychain:{rest}");
            let (service, account) = rest.split_once('#').unwrap_or((rest, ""));
            if service.is_empty()
                || account.is_empty()
                || name.len() > MAX_NAME
                || name.chars().any(char::is_control)
            {
                unusable.push(format!("{profile}/{var}"));
                continue;
            }
            found.insert(Reference {
                name,
                service: service.to_owned(),
                account: account.to_owned(),
            });
        }
    }
    (found.into_iter().collect(), unusable)
}

/// Reads the Keychain items whose records the envelope lacks. Nothing is written. A missing or unreadable item is a
/// line, not a failure: the source is never touched and the authority import does not depend on it.
pub(crate) fn fetch(
    refs: &[Reference],
    have: &BTreeSet<String>,
    source: &dyn ItemSource,
) -> (Vec<(Reference, Zeroizing<String>)>, Vec<String>) {
    let (mut found, mut lines) = (Vec::new(), Vec::new());
    let kept = refs.iter().filter(|r| have.contains(&r.name)).count();
    let mut missing = 0;
    for r in refs.iter().filter(|r| !have.contains(&r.name)) {
        match source.read(&r.service, &r.account) {
            Item::Missing => {
                missing += 1;
                lines.push(format!("secret skipped: {} has no Keychain item", r.name));
            }
            Item::Unavailable(why) => lines.push(format!("secret skipped: {}: {why}", r.name)),
            Item::Found(value) => found.push((r.clone(), value)),
        }
    }
    lines.push(format!(
        "secrets: {} Keychain item(s) to seal, {kept} already in the envelope, {missing} not in the Keychain",
        found.len()
    ));
    (found, lines)
}

/// Seals what `fetch` found. A store that refuses stops the rest with one line. Returns how many went in.
pub(crate) fn seal(
    conn: &Connection,
    fetched: Vec<(Reference, Zeroizing<String>)>,
    sink: &mut dyn SecretSink,
) -> (usize, Vec<String>) {
    let mut done = 0;
    let mut lines = Vec::new();
    for (r, value) in fetched {
        match sink.put(conn, &r.name, value.as_bytes()) {
            Ok(()) => done += 1,
            Err(e) => {
                lines.push(format!(
                    "secrets store unavailable, the remaining Keychain items were left alone: {e}"
                ));
                break;
            }
        }
    }
    lines.push(format!("secrets: {done} sealed into the envelope"));
    (done, lines)
}
