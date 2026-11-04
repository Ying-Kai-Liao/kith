//! The host's vault: named secrets (connector env, logins, the router and voice keys, the 1Password token, peer seeds).
//!
//! Values are sealed by the core's secrets envelope (`core/runtime/src/secrets`, runtime-design section 9b): one master
//! key per machine in the OS store, XChaCha20-Poly1305 with the slot name as associated data, records in the core's database. The
//! host may not link the core's internals, so it reaches the envelope as a client of the owner socket's `secrets.*`
//! rows ([`CoreLink`], through `core_front`). There is no plaintext fallback: with no daemon, no key store (Windows
//! until M2-6) or a locked one, reads and writes refuse and nothing is stored.
//!
//! Before M2-7 the values lived in `serve.db`'s `kv` table under a key of their own (service
//! `dev.kith.serve.credentials`). They move once, never destructively: `unlock` reads that old key one time, seals every
//! old record into the envelope, reads each back, and leaves the old rows exactly as they were (so a rollback to the
//! previous host still finds its data; a value written after the move is not on the old rows). A record that cannot be
//! read stays where it is and is reported. Once a slot is in the envelope, the envelope wins.
use crate::{LockExt, store::Store};
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zeroize::Zeroizing;

/// What the old vault wrote in front of its base64 records.
const PREFIX: &str = "vault:v1:";
/// The old vault's master key item in the OS store (read once, for the move).
const SERVICE: &str = "dev.kith.serve.credentials";

/// How long a caller waits for the OS credential store. A locked Keychain can sit on an access prompt nobody sees
/// (a freshly signed host binary asks again); the host must keep answering other requests meanwhile. Longer than the
/// core's own wait for the store, so its answer ("keychain refused") arrives before this gives up.
const KEYCHAIN_WAIT: Duration = Duration::from_secs(45);
/// How long `unlock` waits for the daemon to be up (it starts beside the host and may still be migrating).
const DAEMON_WAIT: Duration = Duration::from_secs(15);

/// One unlock at a time, and none while an earlier one is still stuck in the keychain: each would park another
/// thread there for good.
static UNLOCKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static KEYCHAIN_BUSY: AtomicBool = AtomicBool::new(false);

/// Where sealed values are kept: the envelope in the core. A trait so the vault's rules (moving old records, refusing,
/// ordering) are tested here without a daemon; the envelope's own rules are tested in the core.
pub trait Link: Send + Sync {
    /// Loads the machine's key (making it on a first use) so later calls do not wait on the key store.
    fn open(&self) -> Result<()>;
    /// Whether the envelope holds `name`. Needs no key, so it never prompts.
    fn has(&self, name: &str) -> Result<bool>;
    /// Removes `name`; answers whether it was there. Needs no key.
    fn delete(&self, name: &str) -> Result<bool>;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn put(&self, name: &str, value: &str) -> Result<()>;
    /// The old vault's master key, or `None` when this computer never made one. Never makes one.
    fn legacy_key(&self, store: &Store) -> Result<Option<Zeroizing<Vec<u8>>>>;
}

/// A store's vault: the way to the envelope, whether `unlock` has run, and the order of writers.
pub struct Vault {
    link: Arc<dyn Link>,
    open: AtomicBool,
    /// Held across a write and across moving one old record, so a move can never overwrite a newer write.
    writing: Mutex<()>,
}

impl Vault {
    /// The vault over the core's envelope. Locked until `unlock`.
    pub fn new() -> Self {
        Self::with_link(Arc::new(CoreLink), false)
    }

    pub fn with_link(link: Arc<dyn Link>, open: bool) -> Self {
        Self { link, open: AtomicBool::new(open), writing: Mutex::new(()) }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn mark_open(&self) {
        self.open.store(true, Ordering::Release);
    }
}

/// The real link: the owner socket's `secrets.*` rows (`core_front` is the host's one door to the core).
struct CoreLink;

impl CoreLink {
    fn call(method: &str, params: &Value, wait: Duration) -> Result<Value> {
        crate::core_front::secrets_call(method, params, wait).map_err(|e| {
            anyhow!(
                "Secure credential storage refused: {e:#}. Credentials are never saved as plain text, and nothing was changed."
            )
        })
    }
}

impl Link for CoreLink {
    fn open(&self) -> Result<()> {
        Self::call("secrets.open", &json!({}), DAEMON_WAIT).map(drop)
    }

    fn has(&self, name: &str) -> Result<bool> {
        Ok(Self::call("secrets.has", &json!({"name": name}), Duration::from_secs(2))?["has"] == true)
    }

    fn delete(&self, name: &str) -> Result<bool> {
        Ok(Self::call("secrets.delete", &json!({"name": name}), Duration::from_secs(2))?["deleted"] == true)
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        let answer = Self::call("secrets.get", &json!({"name": name}), Duration::from_secs(2))?;
        if answer["found"] != true {
            return Ok(None);
        }
        answer["value"]
            .as_str()
            .map(|s| Some(s.to_owned()))
            .ok_or_else(|| anyhow!("The credential store answered without a value"))
    }

    fn put(&self, name: &str, value: &str) -> Result<()> {
        Self::call("secrets.put", &json!({"name": name, "value": value}), Duration::from_secs(2)).map(drop)
    }

    fn legacy_key(&self, store: &Store) -> Result<Option<Zeroizing<Vec<u8>>>> {
        // No id means the old vault never ran here: there is nothing to read and nothing to make.
        let Some(id) = store.kv_read("vault.id")? else {
            return Ok(None);
        };
        let entry = keyring::Entry::new(SERVICE, &id).map_err(|_| unavailable())?;
        let key = match entry.get_secret() {
            Ok(key) => key,
            Err(keyring::Error::NoEntry) => {
                bail!(
                    "The old credential key is missing from this computer's keychain. Restore it; the old records were left as they are."
                )
            }
            Err(_) => return Err(unavailable()),
        };
        if key.len() != 32 {
            bail!(
                "The old credential key is not usable; restore this computer's keychain. The old records were left as they are."
            );
        }
        Ok(Some(Zeroizing::new(key)))
    }
}

pub async fn unlock(hub: Arc<crate::hub::Hub>) -> Result<()> {
    unlock_with(
        hub,
        |store| {
            store.vault.link.open()?;
            let stuck = move_old_records(store);
            if !stuck.is_empty() {
                tracing::warn!(slots = ?stuck, "credential records could not be moved to the secrets envelope; they were left in place");
            }
            Ok(())
        },
        KEYCHAIN_WAIT,
    )
    .await
}

/// `open` opens the envelope and moves the old records. It runs on a blocking thread and never under a lock the rest of
/// the host takes: the keychain can stall, and a stalled lock froze every request.
async fn unlock_with(
    hub: Arc<crate::hub::Hub>,
    open: impl FnOnce(&Store) -> Result<()> + Send + 'static,
    wait: Duration,
) -> Result<()> {
    if hub.store.vault.is_open() {
        return Ok(());
    }
    let _one = UNLOCKING.lock().await;
    if hub.store.vault.is_open() {
        return Ok(());
    }
    if KEYCHAIN_BUSY.swap(true, Ordering::AcqRel) {
        tracing::warn!("{KEYCHAIN_HINT}: an earlier request is still waiting on it");
        bail!("{}", not_answering());
    }
    let reading = hub.clone();
    let task = tokio::task::spawn_blocking(move || {
        let opened = open(&reading.store);
        KEYCHAIN_BUSY.store(false, Ordering::Release);
        opened
    });
    let Ok(joined) = tokio::time::timeout(wait, task).await else {
        tracing::error!("{KEYCHAIN_HINT}");
        bail!("{}", not_answering());
    };
    joined??;
    hub.store.vault.open.store(true, Ordering::Release);
    Ok(())
}

/// Seals every old-format record into the envelope, reading the old key once. The slots that could not be moved (no
/// key, a record that does not open, a value that did not read back) are returned; their old rows are untouched.
fn move_old_records(store: &Store) -> Vec<String> {
    let slots = match store.kv_with_value_prefix(PREFIX) {
        Ok(slots) => slots,
        Err(error) => {
            tracing::warn!(%error, "could not list the old credential records");
            return vec!["(listing failed)".into()];
        }
    };
    if slots.is_empty() {
        return Vec::new();
    }
    let key = match store.vault.link.legacy_key(store) {
        Ok(Some(key)) => key,
        Ok(None) => return slots,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "could not read the old credential key");
            return slots;
        }
    };
    slots
        .into_iter()
        .filter(|slot| match move_slot(store, slot, Some(&key)) {
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(slot, error = format!("{error:#}"), "could not move a credential record");
                true
            }
        })
        .collect()
}

/// Moves one old record (or a plaintext configuration value from before the vault) into the envelope unless the
/// envelope already has the slot, and returns the value. Reads it back before answering; deletes nothing.
fn move_slot(store: &Store, slot: &str, key: Option<&Zeroizing<Vec<u8>>>) -> Result<String> {
    let _writing = store.vault.writing.locked();
    if let Some(sealed) = store.vault.link.get(slot)? {
        return Ok(sealed);
    }
    let Some(raw) = store.kv_read(slot)? else {
        bail!("There is no record to move");
    };
    let value = if raw.starts_with(PREFIX) {
        let key =
            key.ok_or_else(|| anyhow!("The old credential key is not available; the stored record was preserved"))?;
        open_old(key, slot, &raw)?
    } else {
        raw
    };
    store.vault.link.put(slot, &value)?;
    match store.vault.link.get(slot)? {
        Some(back) if back == value => Ok(value),
        _ => bail!("The moved credential did not read back; the old record was left in place"),
    }
}

/// An old-format record: `vault:v1:` and base64 of a 24-byte nonce plus XChaCha20-Poly1305 ciphertext, slot as AD.
fn open_old(key: &[u8], slot: &str, raw: &str) -> Result<String> {
    let bytes = STANDARD.decode(&raw[PREFIX.len()..]).map_err(|_| anyhow!("Invalid encrypted credential record"))?;
    if bytes.len() < 24 {
        bail!("Invalid encrypted credential record");
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| unavailable())?;
    let plain = cipher
        .decrypt(
            &XNonce::try_from(&bytes[..24]).map_err(|_| anyhow!("Invalid credential nonce"))?,
            Payload { msg: &bytes[24..], aad: slot.as_bytes() },
        )
        .map_err(|_| anyhow!("Could not decrypt credentials; the stored record was preserved"))?;
    Ok(String::from_utf8(plain)?)
}

const KEYCHAIN_HINT: &str =
    "keychain not answering (likely an access prompt after a rebuild: open Keychain Access or click Always Allow)";

fn not_answering() -> &'static str {
    "keychain not answering: the OS credential store isn't responding. A prompt may be waiting on this computer (click Always Allow, or unlock Keychain), then try again."
}

fn unavailable() -> anyhow::Error {
    anyhow!(
        "Secure credential storage is locked or unavailable. Unlock Keychain on macOS, or start and unlock a Secret Service keyring in this Linux user's D-Bus session (Windows has no supported store yet). Credentials are never saved as plain text."
    )
}

pub fn read(store: &Store, slot: &str) -> Result<Option<String>> {
    if !store.vault.is_open() {
        // Before `unlock` a slot with nothing behind it is simply empty (callers ask without unlocking to find out
        // whether a feature is set up); one that has a value needs the key store, which only `unlock` may wake.
        if exists(store, slot)? {
            return Err(unavailable());
        }
        return Ok(None);
    }
    if let Some(value) = store.vault.link.get(slot)? {
        return Ok(Some(value));
    }
    let Some(raw) = store.kv_read(slot)? else {
        return Ok(None);
    };
    // Not moved yet (a plaintext value from before the vault, or a record `unlock` could not move): try it here.
    let key = if raw.starts_with(PREFIX) { store.vault.link.legacy_key(store)? } else { None };
    move_slot(store, slot, key.as_ref()).map(Some)
}

/// Whether the slot has a value, sealed or still on an old row. Needs no key, so it never wakes the key store.
pub fn exists(store: &Store, slot: &str) -> Result<bool> {
    Ok(store.vault.link.has(slot)? || store.kv_read(slot)?.is_some())
}

/// Removes the slot's value. The old row, if any, goes too: this is the owner clearing a credential, not a move.
pub fn remove(store: &Store, slot: &str) -> Result<()> {
    let _writing = store.vault.writing.locked();
    store.vault.link.delete(slot)?;
    store.kv_delete(slot)
}

pub fn write(store: &Store, slot: &str, value: &str) -> Result<()> {
    if !store.vault.is_open() {
        return Err(unavailable());
    }
    let _writing = store.vault.writing.locked();
    store.vault.link.put(slot, value)
}

#[cfg(test)]
pub(crate) mod memory {
    //! A stand-in for the envelope for tests: values in a map, and switches for the refusals the real one makes.
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct MemoryLink {
        pub values: Mutex<HashMap<String, String>>,
        pub puts: std::sync::atomic::AtomicUsize,
        /// The refusal of a missing or locked key store (or no daemon), as the core words it.
        pub refuse: Mutex<Option<String>>,
        pub legacy: Mutex<Option<Vec<u8>>>,
    }

    impl MemoryLink {
        fn check(&self) -> Result<()> {
            match &*self.refuse.locked() {
                Some(why) => bail!(
                    "Secure credential storage refused: {why}. Credentials are never saved as plain text, and nothing was changed."
                ),
                None => Ok(()),
            }
        }
    }

    impl Link for MemoryLink {
        fn open(&self) -> Result<()> {
            self.check()
        }
        fn has(&self, name: &str) -> Result<bool> {
            self.check()?;
            Ok(self.values.locked().contains_key(name))
        }
        fn delete(&self, name: &str) -> Result<bool> {
            self.check()?;
            Ok(self.values.locked().remove(name).is_some())
        }
        fn get(&self, name: &str) -> Result<Option<String>> {
            self.check()?;
            Ok(self.values.locked().get(name).cloned())
        }
        fn put(&self, name: &str, value: &str) -> Result<()> {
            self.check()?;
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.values.locked().insert(name.into(), value.into());
            Ok(())
        }
        fn legacy_key(&self, _: &Store) -> Result<Option<Zeroizing<Vec<u8>>>> {
            Ok(self.legacy.locked().clone().map(Zeroizing::new))
        }
    }

    /// A vault over a fresh memory link.
    pub fn vault(open: bool) -> Vault {
        Vault::with_link(Arc::new(MemoryLink::default()), open)
    }

    /// An open vault over a fresh memory link.
    pub fn open_vault() -> (Vault, Arc<MemoryLink>) {
        let link = Arc::new(MemoryLink::default());
        (Vault::with_link(link.clone(), true), link)
    }
}

#[cfg(test)]
mod tests {
    use super::memory::*;
    use super::*;

    fn store_with(vault: Vault) -> Store {
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store.vault = vault;
        store
    }

    /// A record as the old vault wrote it.
    fn old_record(key: &[u8], slot: &str, value: &str) -> String {
        let cipher = XChaCha20Poly1305::new_from_slice(key).unwrap();
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).unwrap();
        let sealed = cipher.encrypt(&nonce.into(), Payload { msg: value.as_bytes(), aad: slot.as_bytes() }).unwrap();
        let bytes: Vec<u8> = nonce.into_iter().chain(sealed).collect();
        format!("{PREFIX}{}", STANDARD.encode(bytes))
    }

    #[test]
    #[ignore = "requires an unlocked OS keychain or Secret Service session"]
    fn platform_keyring_roundtrip() {
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let entry = keyring::Entry::new(SERVICE, &id).unwrap();
        let key = [42u8; 32];
        entry.set_secret(&key).unwrap();
        let loaded = entry.get_secret().unwrap();
        entry.delete_credential().unwrap();
        assert_eq!(loaded, key);
        assert!(matches!(entry.get_secret(), Err(keyring::Error::NoEntry)));
    }

    /// A keychain that never answers must not freeze the threads that merely check whether the vault is open.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stuck_keychain_blocks_neither_other_callers_nor_the_unlock_itself() {
        use crate::remote::identity::Identity;
        let dir = std::env::temp_dir().join(format!("kith-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("kith.db")).unwrap();
        store.vault = Vault::with_link(Arc::new(MemoryLink::default()), false);
        let hub =
            crate::hub::Hub::new(store, "vault-test".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let unlocking = tokio::spawn(unlock_with(
            hub.clone(),
            move |_| {
                let _ = stuck.recv();
                Ok(())
            },
            Duration::from_millis(300),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // What `automatic_loop` and every request do on an async worker meanwhile.
        // On a thread of its own, so a regression fails here instead of hanging the run.
        let (done, checked) = std::sync::mpsc::channel();
        let probe = hub.clone();
        std::thread::spawn(move || done.send(probe.store.vault.is_open()));
        let open = checked.recv_timeout(Duration::from_secs(5)).expect("the vault check hung behind the keychain");
        assert!(!open);
        let error = unlocking.await.unwrap().unwrap_err();
        assert!(format!("{error:#}").contains("keychain not answering"), "{error:#}");
        // The stuck read is still out there: a second try says so at once instead of parking another thread.
        let again = unlock_with(hub.clone(), |_| Ok(()), Duration::from_secs(5)).await.unwrap_err();
        assert!(format!("{again:#}").contains("keychain not answering"), "{again:#}");
        // Once it answers, the next unlock works.
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        unlock_with(hub.clone(), |_| Ok(()), Duration::from_secs(5)).await.unwrap();
        assert!(hub.store.vault.is_open());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_value_round_trips_and_the_last_write_wins() {
        let (vault, link) = open_vault();
        let store = store_with(vault);
        assert_eq!(read(&store, "one").unwrap(), None);
        write(&store, "one", "private password").unwrap();
        assert_eq!(read(&store, "one").unwrap().as_deref(), Some("private password"));
        write(&store, "one", "").unwrap();
        assert_eq!(read(&store, "one").unwrap().as_deref(), Some(""));
        // Sealed by the envelope, never in serve.db.
        assert_eq!(store.kv_read("one").unwrap(), None);
        assert_eq!(link.values.locked().len(), 1);
    }

    #[test]
    fn two_threads_writing_one_slot_leave_one_whole_value() {
        let (vault, _) = open_vault();
        let store = Arc::new(store_with(vault));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || write(&store, "shared", &format!("value-{i}")).unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let got = read(&store, "shared").unwrap().unwrap();
        assert!(got.starts_with("value-"), "{got}");
    }

    #[test]
    fn existing_plaintext_is_moved_into_the_envelope_before_use() {
        let (vault, link) = open_vault();
        let store = store_with(vault);
        store.kv_set("record", "private password").unwrap();
        assert_eq!(read(&store, "record").unwrap().as_deref(), Some("private password"));
        assert_eq!(link.values.locked().get("record").map(String::as_str), Some("private password"));
        // The old row stays (a rollback still reads it); the envelope answers from now on.
        assert_eq!(store.kv_read("record").unwrap().as_deref(), Some("private password"));
        write(&store, "record", "newer").unwrap();
        assert_eq!(read(&store, "record").unwrap().as_deref(), Some("newer"));
    }

    #[test]
    fn old_records_move_once_and_a_second_run_changes_nothing() {
        let (vault, link) = open_vault();
        let key = vec![5u8; 32];
        *link.legacy.locked() = Some(key.clone());
        let store = store_with(vault);
        let one = old_record(&key, "connectors", "[1]");
        let two = old_record(&key, "credential.1password", "token");
        store.kv_set("connectors", &one).unwrap();
        store.kv_set("credential.1password", &two).unwrap();
        store.kv_set("unrelated", "not a record").unwrap();
        assert_eq!(move_old_records(&store), Vec::<String>::new());
        assert_eq!(link.puts.load(Ordering::SeqCst), 2);
        assert_eq!(link.values.locked().get("connectors").map(String::as_str), Some("[1]"));
        assert_eq!(link.values.locked().get("credential.1password").map(String::as_str), Some("token"));
        // Nothing deleted or rewritten on the old side.
        assert_eq!(store.kv_read("connectors").unwrap().as_deref(), Some(one.as_str()));
        assert_eq!(store.kv_read("credential.1password").unwrap().as_deref(), Some(two.as_str()));
        // A value written after the move survives a second run.
        write(&store, "connectors", "[2]").unwrap();
        let puts = link.puts.load(Ordering::SeqCst);
        assert_eq!(move_old_records(&store), Vec::<String>::new());
        assert_eq!(link.puts.load(Ordering::SeqCst), puts, "the second run wrote nothing");
        assert_eq!(read(&store, "connectors").unwrap().as_deref(), Some("[2]"));
    }

    #[test]
    fn a_record_that_cannot_be_read_stays_and_is_reported() {
        let (vault, link) = open_vault();
        *link.legacy.locked() = Some(vec![5u8; 32]);
        let store = store_with(vault);
        let good = old_record(&[5u8; 32], "good", "fine");
        let foreign = old_record(&[6u8; 32], "bad", "other key");
        store.kv_set("good", &good).unwrap();
        store.kv_set("bad", &foreign).unwrap();
        assert_eq!(move_old_records(&store), ["bad"]);
        assert_eq!(read(&store, "good").unwrap().as_deref(), Some("fine"));
        let error = read(&store, "bad").unwrap_err();
        assert!(format!("{error:#}").contains("stored record was preserved"), "{error:#}");
        assert_eq!(store.kv_read("bad").unwrap().as_deref(), Some(foreign.as_str()));
        // A record sealed under a slot name it does not belong to does not open either.
        store.kv_set("moved", &good).unwrap();
        assert!(read(&store, "moved").is_err());
        assert_eq!(link.values.locked().get("moved"), None);
    }

    #[test]
    fn without_the_old_key_the_old_records_are_reported_not_lost() {
        let (vault, link) = open_vault();
        let store = store_with(vault);
        let rec = old_record(&[5u8; 32], "slot", "v");
        store.kv_set("slot", &rec).unwrap();
        assert_eq!(move_old_records(&store), ["slot"]);
        assert!(read(&store, "slot").is_err());
        assert_eq!(store.kv_read("slot").unwrap().as_deref(), Some(rec.as_str()));
        assert_eq!(link.values.locked().len(), 0);
    }

    #[test]
    fn a_missing_key_store_refuses_everything_and_stores_nothing() {
        let (vault, link) = open_vault();
        let store = store_with(vault);
        store.kv_set("old", "plain value").unwrap();
        *link.refuse.lock().unwrap() = Some("no key store here".into());
        for result in [read(&store, "old").map(drop), read(&store, "new").map(drop), write(&store, "new", "secret")] {
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains("no key store here") && error.contains("never saved as plain text"), "{error}");
        }
        assert_eq!(link.values.locked().len(), 0);
        assert_eq!(store.kv_read("new").unwrap(), None, "no plaintext fallback");
        assert_eq!(store.kv_read("old").unwrap().as_deref(), Some("plain value"));
        assert!(link.open().is_err());
    }

    #[test]
    fn removing_a_slot_clears_the_envelope_and_the_old_row() {
        let (vault, _) = open_vault();
        let store = store_with(vault);
        store.kv_set("k", "old").unwrap();
        write(&store, "k", "new").unwrap();
        assert!(exists(&store, "k").unwrap());
        remove(&store, "k").unwrap();
        assert!(!exists(&store, "k").unwrap());
        assert_eq!(read(&store, "k").unwrap(), None);
    }

    #[test]
    fn a_locked_vault_refuses_and_preserves_existing_data() {
        let (vault, link) = open_vault();
        let store = store_with(vault);
        store.kv_set("record", "existing secret").unwrap();
        store.vault.open.store(false, Ordering::Release);
        assert!(read(&store, "record").is_err());
        assert_eq!(read(&store, "nothing-here").unwrap(), None);
        assert!(write(&store, "record", "replacement").is_err());
        assert_eq!(store.kv_read("record").unwrap().as_deref(), Some("existing secret"));
        assert_eq!(link.values.locked().len(), 0);
    }
}
