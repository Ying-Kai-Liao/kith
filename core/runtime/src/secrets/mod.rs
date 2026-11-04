//! The secrets envelope (docs/plans/runtime-design.md section 9b): named secrets as XChaCha20-Poly1305 records in
//! core.db, under one master key per machine that lives in the OS credential store ([`keystore`]) and never in core.db.
//!
//! Rules this module keeps:
//! - No plaintext anywhere: a record is a random 24-byte nonce plus ciphertext, with the record's name as associated
//!   data, so a record copied or renamed in the table no longer opens.
//! - No plaintext fallback and no silent new key. A missing or unreadable key refuses. A fresh key is minted only when
//!   the table holds no records at all; with records and no key the records are left as they are and the error says
//!   to restore this computer's key store.
//! - Records sealed under a different key than the one in the store (a database moved from another machine) refuse
//!   with that said, and a put never mixes two keys in one table.
//!
//! Record names are stored in plaintext (they are the associated data and `list` needs them); only values are secret.
//! Zeroizing is best effort: stack copies and formatted buffers are not tracked.
//!
//! Secret cards write through it (`cards/secret.rs`, M2-12). The host vault calls it through the owner socket's
//! `secrets.open|get|put` rows ([`call`], M2-7); kith-auth's Keychain writes move onto it at M2-18, and the signed
//! daemon reads the key through Security.framework at M2-6.

pub(crate) mod keystore;
#[cfg(test)]
mod tests;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::sync::Mutex;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use keystore::{KEY_LEN, KeyStore};

/// The `secrets.*` rows of the method table, on the core's connection. The envelope is made on the first call and kept
/// (it holds the master key once read); where the OS has no key store the call refuses, every time, and nothing is
/// stored. Values cross as text: the host's vault only keeps text.
pub(crate) fn call(
    conn: &Connection,
    slot: &Mutex<Option<Envelope>>,
    method: &str,
    params: &Value,
) -> Result<Value> {
    let text = |field: &str| {
        params
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::BadParams(format!("kith-core: {method} needs {field}")))
    };
    // Whether a record exists needs no key and no key store, so a caller that has not opened the vault can still tell
    // "nothing there" from "locked" without a prompt.
    if method == "secrets.has" {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM secret_records WHERE name = ?1",
                [text("name")?],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(json!({"has": found.is_some()}));
    }
    // Removing a record needs no key either.
    if method == "secrets.delete" {
        return Ok(json!({"deleted": Envelope::delete(conn, text("name")?)?}));
    }
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(Envelope::os()?);
    }
    let envelope = guard.as_mut().ok_or_else(unreachable_key)?;
    match method {
        "secrets.open" => {
            envelope.key(conn, true)?;
            Ok(json!({"open": true}))
        }
        "secrets.get" => Ok(match envelope.get(conn, text("name")?)? {
            None => json!({"found": false}),
            Some(bytes) => {
                let value = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::Corrupt("kith-core: the secret is not text".into()))?;
                json!({"found": true, "value": value})
            }
        }),
        "secrets.serviceKeys" => {
            let vars: Vec<&str> = params
                .get("vars")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let opt = |f: &str| params.get(f).and_then(Value::as_str);
            envelope.service_keys(
                conn,
                &ServiceKeys {
                    agent: text("agent")?,
                    service: text("service")?,
                    vars: &vars,
                    person: text("person")?,
                    profile: opt("profile"),
                },
            )
        }
        "secrets.put" => {
            envelope.put(conn, text("name")?, text("value")?.as_bytes())?;
            Ok(json!({}))
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

/// The params of `secrets.serviceKeys`.
pub(crate) struct ServiceKeys<'a> {
    pub agent: &'a str,
    pub service: &'a str,
    pub vars: &'a [&'a str],
    /// Who the run acts for (`acting()`'s answer); only "" means no person check.
    pub person: &'a str,
    pub profile: Option<&'a str>,
}

/// Longest record name, in bytes. Names are the associated data, so they stay short and printable-ish.
const MAX_NAME: usize = 256;

pub(crate) struct Envelope {
    store: Box<dyn KeyStore>,
    key: Option<Zeroizing<[u8; KEY_LEN]>>,
}

impl Envelope {
    /// The envelope over this operating system's key store, or a refusal where there is none.
    pub(crate) fn os() -> Result<Self> {
        Ok(Self::with_store(keystore::os_key_store()?))
    }

    pub(crate) fn with_store(store: Box<dyn KeyStore>) -> Self {
        Self { store, key: None }
    }

    /// Loads the master key from the key store without opening any record (a locked store fails here; with records and
    /// no key it refuses, as every call does). Mints nothing.
    pub(crate) fn warm(&mut self, conn: &Connection) -> Result<()> {
        self.key(conn, false).map(drop)
    }

    /// Stores `bytes` under `name`, replacing an earlier value (one statement, so last write wins atomically).
    pub(crate) fn put(&mut self, conn: &Connection, name: &str, bytes: &[u8]) -> Result<()> {
        check_name(name)?;
        let key = self.key(conn, true)?.ok_or_else(unreachable_key)?;
        let key_id = key_id(&key);
        let other: i64 = conn.query_row(
            "SELECT COUNT(*) FROM secret_records WHERE key_id <> ?1",
            [&key_id],
            |r| r.get(0),
        )?;
        if other > 0 {
            return Err(Error::Io(format!(
                "kith-core: {other} secret record(s) were sealed under a different master key than this computer's; \
                 nothing was written. Restore the key store they came from, or remove those records"
            )));
        }
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
        let sealed = cipher(&key)?
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: bytes,
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| Error::Io("kith-core: could not seal the secret".into()))?;
        let now = now_secs();
        conn.execute(
            "INSERT INTO secret_records (name, key_id, nonce, ciphertext, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(name) DO UPDATE SET key_id = excluded.key_id, nonce = excluded.nonce,
                 ciphertext = excluded.ciphertext, updated_at = excluded.updated_at",
            params![name, key_id, nonce.as_slice(), sealed, now],
        )?;
        Ok(())
    }

    /// The keys `agent` may use for `service` (M2-18b), as `kith-auth exec` resolves them: the values of the `vars` the
    /// service declares, from the profile granted (`agent@person` first, then `agent`; `profile` picks one of a list,
    /// else the lowest `seq`). Refuses, naming what is missing and never a value: no grant (with the `kith-auth grant`
    /// hint), another person's profile that is not lent, a declared variable with no row or an empty value. A source
    /// that is not a `keychain:` record (`env:`, `env-file:`) comes back unresolved: it names the caller's own
    /// environment or files, which the core must not read for it. With no `vars` the grant and the person rule
    /// are still applied and the profile row must exist, but no variable row or record is read; the chosen profile is
    /// returned for the caller's audit. `person` is required (the caller passes `acting()`): the run is checked like
    /// anyone's, the owner's included, and only "" skips the check, as Python's empty `acting()` does. A `profile` pick
    /// with a single-string grant refuses here where Python ignores `KITH_AUTH_PROFILE` for a string grant (the only
    /// setter, kith-send-to, sets it for a granted profile, so no live path differs). "no profile" (no `secret_profiles`
    /// row) and "not set" (a declared var with no row) stay distinct messages. The audit line (agent, service, profile, person) is the caller's to write; the core writes none.
    /// Through the daemon the master key is loaded before the call (see `daemon.rs`), so a locked key store refuses
    /// even a call that would read no record.
    pub(crate) fn service_keys(&mut self, conn: &Connection, p: &ServiceKeys) -> Result<Value> {
        for n in [p.agent, p.service] {
            if n.is_empty()
                || n.len() > MAX_NAME
                || n.chars().any(|c| c.is_control() || c == '/' || c == '@')
            {
                return Err(Error::BadParams(
                    "kith-core: secrets.serviceKeys needs an agent and a service name".into(),
                ));
            }
        }
        let (agent, service) = (p.agent, p.service);
        let granted = |who: &str| -> Result<Vec<String>> {
            let mut stmt = conn.prepare(
                "SELECT profile FROM secret_grants WHERE agent = ?1 AND service = ?2 ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([who, service], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        };
        let person = p.person;
        let mut list = if person.is_empty() {
            Vec::new()
        } else {
            granted(&format!("{agent}@{person}"))?
        };
        if list.is_empty() {
            list = granted(agent)?;
        }
        let profile = match (p.profile, list.first()) {
            (_, None) => {
                return Err(Error::Refused(format!(
                    "kith-core: {agent} has no grant for {service} (kith-auth grant {agent} {service} <profile>)"
                )));
            }
            (Some(pick), _) if list.iter().any(|g| g == pick) => pick.to_string(),
            (Some(pick), _) => {
                return Err(Error::Refused(format!(
                    "kith-core: {agent} has no grant for {service} as {pick} (kith-auth grant {agent} {service} {pick})"
                )));
            }
            (None, Some(first)) => first.clone(),
        };
        let row: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT person, lent FROM secret_profiles WHERE profile = ?1",
                [&profile],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some(row) = row else {
            return Err(Error::Refused(format!(
                "kith-core: no profile '{profile}' (granted to {agent} for {service}; kith-auth set {profile} <var>)"
            )));
        };
        if let (Some(owner), lent, false) = (&row.0, &row.1, person.is_empty()) {
            let lent: Vec<String> = lent
                .as_deref()
                .and_then(|t| serde_json::from_str(t).ok())
                .unwrap_or_default();
            if owner != person && !lent.iter().any(|l| l == person) {
                return Err(Error::Refused(format!(
                    "kith-core: {profile} is {owner}'s and this run is for {person} (kith-auth grant {agent}@{person} {service} <their profile>)"
                )));
            }
        }
        let mut keys = Vec::new();
        let mut unresolved = Vec::new();
        for var in p.vars {
            let source: Option<String> = conn
                .query_row(
                    "SELECT source FROM secret_vars WHERE profile = ?1 AND var = ?2",
                    [&profile, *var],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(source) = source else {
                return Err(Error::Refused(format!(
                    "kith-core: {profile}: {var} does not resolve (not set; kith-auth set {profile} {var})"
                )));
            };
            if !source.starts_with("keychain:") {
                unresolved.push(json!({"var": var, "source": source}));
                continue;
            }
            let value = self.get(conn, &source)?.filter(|v| !v.is_empty());
            let Some(value) = value else {
                return Err(Error::Refused(format!(
                    "kith-core: {profile}: {var} does not resolve ({source} is not stored or empty; kith-auth set {profile} {var})"
                )));
            };
            let value = std::str::from_utf8(&value)
                .map_err(|_| Error::Corrupt("kith-core: the secret is not text".into()))?;
            keys.push(json!({"var": var, "value": value}));
        }
        Ok(
            json!({"agent": agent, "service": service, "profile": profile, "keys": keys, "unresolved": unresolved}),
        )
    }

    /// The secret under `name`, or `None` if there is no such record.
    pub(crate) fn get(
        &mut self,
        conn: &Connection,
        name: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>> {
        check_name(name)?;
        let row: Option<(String, Vec<u8>, Vec<u8>)> = conn
            .query_row(
                "SELECT key_id, nonce, ciphertext FROM secret_records WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((record_key_id, nonce, ciphertext)) = row else {
            // Still check the key state: records with a lost key refuse even for a name that is not among them.
            self.key(conn, false)?;
            return Ok(None);
        };
        let key = self.key(conn, false)?.ok_or_else(unreachable_key)?;
        if record_key_id != key_id(&key) {
            return Err(Error::Io(format!(
                "kith-core: the secret {name:?} was sealed under a different master key than this computer's \
                 (a database from another machine?). Restore the key store it came from"
            )));
        }
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| {
            Error::Corrupt(format!(
                "kith-core: the secret {name:?} has a malformed nonce"
            ))
        })?;
        let plain = cipher(&key)?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| {
                Error::Corrupt(format!(
                    "kith-core: the secret {name:?} did not open (altered, or stored under another name). Run kith restore"
                ))
            })?;
        Ok(Some(Zeroizing::new(plain)))
    }

    /// Removes `name`; answers whether it was there. Needs no key.
    pub(crate) fn delete(conn: &Connection, name: &str) -> Result<bool> {
        check_name(name)?;
        let n = conn.execute("DELETE FROM secret_records WHERE name = ?1", [name])?;
        Ok(n > 0)
    }

    /// The names of all records, sorted. Needs no key; the names are not secret from the core.
    pub(crate) fn list(conn: &Connection) -> Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT name FROM secret_records ORDER BY name")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(names)
    }

    /// The master key, loaded once. `create` allows minting one, and only when the table is empty.
    fn key(&mut self, conn: &Connection, create: bool) -> Result<Option<Zeroizing<[u8; KEY_LEN]>>> {
        if self.key.is_none() {
            if let Some(raw) = self.store.get()? {
                self.key = Some(key_array(&raw)?);
            } else {
                {
                    let records: i64 =
                        conn.query_row("SELECT COUNT(*) FROM secret_records", [], |r| r.get(0))?;
                    if records > 0 {
                        return Err(Error::Io(format!(
                            "kith-core: the secrets master key is missing from this computer's key store, and {records} \
                             secret record(s) exist that only it opens. Nothing was changed. Restore the key store from \
                             a backup (kith restore); a new key is never made over existing records"
                        )));
                    }
                    if !create {
                        return Ok(None);
                    }
                    let mut fresh = Zeroizing::new([0u8; KEY_LEN]);
                    getrandom::fill(fresh.as_mut_slice())
                        .map_err(|_| Error::Io("kith-core: random bytes unavailable".into()))?;
                    self.store.add(fresh.as_slice())?;
                    // Adopt whatever well-formed key the store now holds: if another core won a mint race, its key
                    // is the one (this table is empty, so nothing was sealed under ours). On Linux `secret-tool store`
                    // replaces, so there the race stays open: the last minter wins and the other core's first
                    // records would not open.
                    let adopted = self
                        .store
                        .get()?
                        .and_then(|b| <[u8; KEY_LEN]>::try_from(b.as_slice()).ok());
                    let Some(adopted) = adopted else {
                        return Err(Error::Io(
                            "kith-core: the key store did not keep the new secrets master key; no secret was stored"
                                .into(),
                        ));
                    };
                    self.key = Some(Zeroizing::new(adopted));
                }
            }
        }
        Ok(self.key.clone())
    }
}

fn key_array(raw: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let arr: [u8; KEY_LEN] = raw.try_into().map_err(|_| {
        Error::Io(
            "kith-core: the secrets master key in the key store has the wrong length; it was left untouched".into(),
        )
    })?;
    Ok(Zeroizing::new(arr))
}

/// Reads the master key from the OS key store through `load`, for the daemon to call before it takes the core lock: a
/// key store that stalls (a locked keychain waits up to 30 s) then holds up only this call, never the owner socket's
/// other rows. `None` means no key is stored yet (the call that follows decides whether that is empty or an error).
pub(crate) fn preload(
    load: &dyn Fn() -> Result<Option<Zeroizing<Vec<u8>>>>,
) -> Result<Option<Zeroizing<[u8; KEY_LEN]>>> {
    load()?.map(|raw| key_array(&raw)).transpose()
}

/// Hands a key read by [`preload`] to the envelope in `slot`, so the call under the core lock finds it loaded.
pub(crate) fn adopt(slot: &Mutex<Option<Envelope>>, key: Zeroizing<[u8; KEY_LEN]>) -> Result<()> {
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(Envelope::os()?);
    }
    if let Some(envelope) = guard.as_mut() {
        envelope.key.get_or_insert(key);
    }
    Ok(())
}

/// The OS key store's read, as the daemon uses it unless a test supplies another.
pub(crate) fn os_load() -> Result<Option<Zeroizing<Vec<u8>>>> {
    keystore::os_key_store()?.get()
}

pub(crate) type KeyLoader =
    std::sync::Arc<dyn Fn() -> Result<Option<Zeroizing<Vec<u8>>>> + Send + Sync>;

fn unreachable_key() -> Error {
    Error::Io("kith-core: the secrets master key is unavailable".into())
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(Error::BadParams(format!(
            "kith-core: a secret name is 1 to {MAX_NAME} bytes with no control characters"
        )));
    }
    Ok(())
}

fn cipher(key: &[u8; KEY_LEN]) -> Result<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| Error::Io("kith-core: the secrets master key is unusable".into()))
}

/// A short fingerprint of the key, stored beside each record: it names which key sealed it without revealing it.
fn key_id(key: &[u8; KEY_LEN]) -> String {
    let mut h = Sha256::new();
    h.update(b"kith secrets key id v1");
    h.update(key);
    keystore::hex_encode(&h.finalize()[..8]).to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}
