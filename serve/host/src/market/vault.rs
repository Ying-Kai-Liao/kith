//! Encrypted records backed by the operating system credential store. The database
//! contains ciphertext only; its random master key lives in Keychain/Secret Service.
use crate::{LockExt, store::Store};
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use zeroize::Zeroizing;

const PREFIX: &str = "vault:v1:";
const SERVICE: &str = "dev.kith.serve.credentials";

/// How long a caller waits for the OS credential store. A locked Keychain can sit on an access prompt nobody sees
/// (a freshly signed host binary asks again); the host must keep answering other requests meanwhile.
const KEYCHAIN_WAIT: Duration = Duration::from_secs(20);

/// One unlock at a time, and none while an earlier one is still stuck in the keychain: each would park another
/// thread there for good.
static UNLOCKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static KEYCHAIN_BUSY: AtomicBool = AtomicBool::new(false);

pub async fn unlock(hub: Arc<crate::hub::Hub>) -> Result<()> {
    unlock_with(hub, load_key, KEYCHAIN_WAIT).await
}

/// `load` reads (or creates) the master key. It runs on a blocking thread and never under `secret_key`: that lock
/// is taken on async workers all over the host, so holding it while the keychain stalled froze them, and with
/// them every request.
async fn unlock_with(
    hub: Arc<crate::hub::Hub>,
    load: impl FnOnce(&Store) -> Result<Vec<u8>> + Send + 'static,
    wait: Duration,
) -> Result<()> {
    if hub.store.secret_key.locked().is_some() {
        return Ok(());
    }
    let _one = UNLOCKING.lock().await;
    if hub.store.secret_key.locked().is_some() {
        return Ok(());
    }
    if KEYCHAIN_BUSY.swap(true, Ordering::AcqRel) {
        tracing::warn!("{KEYCHAIN_HINT}: an earlier request is still waiting on it");
        bail!("{}", not_answering());
    }
    let reading = hub.clone();
    let task = tokio::task::spawn_blocking(move || {
        let key = load(&reading.store);
        KEYCHAIN_BUSY.store(false, Ordering::Release);
        key
    });
    let Ok(joined) = tokio::time::timeout(wait, task).await else {
        tracing::error!("{KEYCHAIN_HINT}");
        bail!("{}", not_answering());
    };
    let key = joined??;
    *hub.store.secret_key.locked() = Some(Zeroizing::new(key));
    Ok(())
}

fn load_key(store: &Store) -> Result<Vec<u8>> {
    let id = if let Some(id) = store.kv_read("vault.id")? {
        id
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        store.kv_set("vault.id", &id)?;
        id
    };
    let entry = keyring::Entry::new(SERVICE, &id).map_err(|_| unavailable())?;
    let key = match entry.get_secret() {
        Ok(key) => key,
        Err(keyring::Error::NoEntry) => {
            if store.kv_read("vault.initialized")?.is_some() {
                bail!(
                    "The credential key is missing. Restore this computer's keychain; encrypted credentials were preserved."
                );
            }
            let mut key = vec![0; 32];
            getrandom::fill(&mut key).map_err(|_| anyhow!("Could not generate credential key"))?;
            entry.set_secret(&key).map_err(|_| unavailable())?;
            key
        }
        Err(_) => return Err(unavailable()),
    };
    if key.len() != 32 {
        bail!("Invalid credential key; restore this computer's keychain.");
    }
    store.kv_set("vault.initialized", "true")?;
    Ok(key)
}

const KEYCHAIN_HINT: &str =
    "keychain not answering (likely an access prompt after a rebuild: open Keychain Access or click Always Allow)";

fn not_answering() -> &'static str {
    "keychain not answering: the OS credential store isn't responding. A prompt may be waiting on this computer (click Always Allow, or unlock Keychain), then try again."
}

fn unavailable() -> anyhow::Error {
    anyhow!(
        "Secure credential storage is locked or unavailable. Unlock Keychain on macOS, or start and unlock a Secret Service keyring in this Linux user's D-Bus session (Windows keeps it in Credential Manager). Credentials are never saved as plain text."
    )
}

pub fn read(store: &Store, slot: &str) -> Result<Option<String>> {
    let Some(raw) = store.kv_read(slot)? else {
        return Ok(None);
    };
    if !raw.starts_with(PREFIX) {
        // Move existing host configuration into the vault before returning it.
        write(store, slot, &raw)?;
        return Ok(Some(raw));
    }
    let key = store.secret_key.locked();
    let key = key.as_ref().ok_or_else(unavailable)?;
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
    Ok(Some(String::from_utf8(plain)?))
}

pub fn write(store: &Store, slot: &str, value: &str) -> Result<()> {
    let key = store.secret_key.locked();
    let key = key.as_ref().ok_or_else(unavailable)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| unavailable())?;
    let mut nonce = [0; 24];
    getrandom::fill(&mut nonce).map_err(|_| anyhow!("Could not generate credential nonce"))?;
    let sealed = cipher
        .encrypt(&nonce.into(), Payload { msg: value.as_bytes(), aad: slot.as_bytes() })
        .map_err(|_| anyhow!("Could not encrypt credentials"))?;
    let bytes: Vec<u8> = nonce.into_iter().chain(sealed).collect();
    store.kv_set(slot, &format!("{PREFIX}{}", STANDARD.encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let store = Store::open(&dir.join("kith.db")).unwrap();
        let hub =
            crate::hub::Hub::new(store, "vault-test".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let unlocking = tokio::spawn(unlock_with(
            hub.clone(),
            move |_| {
                let _ = stuck.recv();
                Ok(vec![7; 32])
            },
            Duration::from_millis(300),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // What `automatic_loop` and every request do on an async worker meanwhile.
        // On a thread of its own, so a regression fails here instead of hanging the run.
        let (done, checked) = std::sync::mpsc::channel();
        let probe = hub.clone();
        std::thread::spawn(move || done.send(probe.store.secret_key.locked().is_some()));
        let open = checked.recv_timeout(Duration::from_secs(5)).expect("the vault check hung behind the keychain");
        assert!(!open);
        let error = unlocking.await.unwrap().unwrap_err();
        assert!(format!("{error:#}").contains("keychain not answering"), "{error:#}");
        // The stuck read is still out there: a second try says so at once instead of parking another thread.
        let again = unlock_with(hub.clone(), |_| Ok(vec![1; 32]), Duration::from_secs(5)).await.unwrap_err();
        assert!(format!("{again:#}").contains("keychain not answering"), "{again:#}");
        // Once it answers, the next unlock works.
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        unlock_with(hub.clone(), |_| Ok(vec![9; 32]), Duration::from_secs(5)).await.unwrap();
        assert!(hub.store.secret_key.locked().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_plaintext_is_migrated_before_use() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store.kv_set("record", "private password").unwrap();
        assert_eq!(read(&store, "record").unwrap().as_deref(), Some("private password"));
        assert!(store.kv_read("record").unwrap().unwrap().starts_with(PREFIX));
    }

    #[test]
    fn ciphertext_is_random_and_bound_to_its_record() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        write(&store, "one", "private password").unwrap();
        let first = store.kv_read("one").unwrap().unwrap();
        assert!(!first.contains("private password"));
        assert_eq!(read(&store, "one").unwrap().as_deref(), Some("private password"));
        write(&store, "one", "private password").unwrap();
        assert_ne!(first, store.kv_read("one").unwrap().unwrap());
        store.kv_set("two", &first).unwrap();
        assert!(read(&store, "two").is_err());
    }
    #[test]
    fn locked_storage_preserves_existing_data() {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store.kv_set("record", "existing secret").unwrap();
        *store.secret_key.locked() = None;
        assert!(read(&store, "record").is_err());
        assert!(write(&store, "record", "replacement").is_err());
        assert_eq!(store.kv_read("record").unwrap().as_deref(), Some("existing secret"));
    }
}
