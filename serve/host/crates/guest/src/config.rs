//! What the bot keeps on its VM (section 9.1): `device.key` (the Ed25519 seed) and `guest.json`, both 0600 in a 0700
//! directory. `KITH_GUEST_CREDENTIALS` carries both for a VM that loses its files between sessions; it is never written back.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ed25519_dalek::SigningKey;
use kith_wire::crypto::{b64, unb64_n};
use kith_wire::rpc::BotGuestState;
use serde::{Deserialize, Serialize};

use crate::exit::CliError;

pub const GUEST_FILE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Joining,
    Pending,
    Active,
    Denied,
    Revoked,
}

impl State {
    /// The host's word for the bot's state (the contract's `BotGuestState`). `None` for one this version doesn't know.
    pub fn of(s: BotGuestState) -> Option<Self> {
        Some(match s {
            BotGuestState::Joining | BotGuestState::Invited => Self::Joining,
            BotGuestState::Pending => Self::Pending,
            BotGuestState::Active => Self::Active,
            BotGuestState::Denied => Self::Denied,
            BotGuestState::Revoked => Self::Revoked,
            BotGuestState::Unknown => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joining => "joining",
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Denied => "denied",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub agent: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guest {
    pub v: u32,
    pub cloud: String,
    pub computer_id: String,
    /// The computer's Ed25519 key, b64url: pinned at join, what every welcome must be signed by.
    pub host_sign_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<String>,
    pub name: String,
    pub state: State,
    pub invite_id: String,
    /// The invite token. Kept only while `state` is `joining`, so a later command can finish the join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Cached from the last `botStatus`, so `status` can say something while the computer is offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentRef>,
}

#[derive(Debug)]
pub enum Store {
    Dir(PathBuf),
    Env { device: String, guest: Box<Guest> },
}

pub type Loaded = (SigningKey, Guest);

/// `$KITH_GUEST_HOME`, else `$XDG_CONFIG_HOME/kith`, else `~/.config/kith`.
pub fn config_dir(home_override: Option<&Path>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(h) = home_override.filter(|p| !p.as_os_str().is_empty()) {
        return Some(h.to_path_buf());
    }
    if let Some(x) = xdg.filter(|x| !x.is_empty()) {
        return Some(Path::new(x).join("kith"));
    }
    home.filter(|h| !h.is_empty()).map(|h| Path::new(h).join(".config").join("kith"))
}

fn decode_any(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
        .or_else(|| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s).ok())
}

fn key_from_seed(seed: &str) -> Result<SigningKey, CliError> {
    let seed: [u8; 32] = unb64_n(seed.trim()).map_err(|_| CliError::unexpected("device.key is damaged"))?;
    Ok(SigningKey::from_bytes(&seed))
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    device: String,
    guest: Guest,
}

/// The `KITH_GUEST_CREDENTIALS` value: base64 of `{device, guest}`. The token is left out; a bot that is still joining
/// finishes joining on the VM that started it.
pub fn credentials_line(key: &SigningKey, guest: &Guest) -> String {
    let mut guest = guest.clone();
    guest.token = None;
    let json = serde_json::to_vec(&Credentials { device: b64(&key.to_bytes()), guest }).unwrap_or_default();
    b64(&json)
}

impl Store {
    /// `credentials` is the value of `KITH_GUEST_CREDENTIALS`; when set it replaces the files.
    pub fn open(dir: Option<PathBuf>, credentials: Option<&str>) -> Result<Self, CliError> {
        if let Some(c) = credentials.filter(|c| !c.trim().is_empty()) {
            let bytes = decode_any(c).ok_or_else(|| CliError::usage("KITH_GUEST_CREDENTIALS isn't valid base64"))?;
            let parsed: Credentials = serde_json::from_slice(&bytes)
                .map_err(|_| CliError::usage("KITH_GUEST_CREDENTIALS doesn't hold a Kith bot's credentials"))?;
            key_from_seed(&parsed.device).map_err(|_| CliError::usage("KITH_GUEST_CREDENTIALS holds a damaged key"))?;
            return Ok(Self::Env { device: parsed.device, guest: Box::new(parsed.guest) });
        }
        let dir = dir.ok_or_else(|| CliError::unexpected("No home directory: set KITH_GUEST_HOME or HOME"))?;
        Ok(Self::Dir(dir))
    }

    pub fn is_env(&self) -> bool {
        matches!(self, Self::Env { .. })
    }

    fn files(dir: &Path) -> (PathBuf, PathBuf) {
        (dir.join("device.key"), dir.join("guest.json"))
    }

    /// `None` when nothing was joined yet.
    pub fn load(&self) -> Result<Option<Loaded>, CliError> {
        match self {
            Self::Env { device, guest } => Ok(Some((key_from_seed(device)?, (**guest).clone()))),
            Self::Dir(dir) => {
                let (key_path, guest_path) = Self::files(dir);
                let (Ok(seed), Ok(guest)) = (fs::read_to_string(&key_path), fs::read_to_string(&guest_path)) else {
                    return Ok(None);
                };
                let guest: Guest = serde_json::from_str(&guest).map_err(|e| {
                    CliError::unexpected(format!(
                        "{} is damaged ({e}). Run `kith leave`, then `kith join` again.",
                        guest_path.display()
                    ))
                })?;
                Ok(Some((key_from_seed(&seed)?, guest)))
            }
        }
    }

    /// Writes `guest.json` (and the key, when given). With credentials from the environment nothing is written.
    pub fn save(&self, key: Option<&SigningKey>, guest: &Guest) -> Result<(), CliError> {
        let Self::Dir(dir) = self else { return Ok(()) };
        make_private_dir(dir)?;
        let (key_path, guest_path) = Self::files(dir);
        if let Some(key) = key {
            write_private(&key_path, b64(&key.to_bytes()).as_bytes())?;
        }
        let json = serde_json::to_vec_pretty(guest).map_err(|e| CliError::unexpected(e.to_string()))?;
        write_private(&guest_path, &json)
    }

    pub fn clear(&self) -> Result<(), CliError> {
        let Self::Dir(dir) = self else { return Ok(()) };
        let (key_path, guest_path) = Self::files(dir);
        for p in [key_path, guest_path] {
            match fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(CliError::unexpected(format!("Can't remove {}: {e}", p.display()))),
            }
        }
        Ok(())
    }
}

fn io_err(path: &Path, e: &std::io::Error) -> CliError {
    CliError::unexpected(format!("Can't write {}: {e}", path.display()))
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| io_err(dir, &e))?;
    // A directory that already existed keeps its mode from create(); tighten it, since it now holds a key.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| io_err(dir, &e))
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> Result<(), CliError> {
    fs::create_dir_all(dir).map_err(|e| io_err(dir, &e))
}

/// Write to a sibling, mode 0600 from the first byte, then rename: a crash never leaves a half file, and the key is
/// never readable by others even for a moment.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).map_err(|e| io_err(path, &e))?;
    f.write_all(bytes).map_err(|e| io_err(path, &e))?;
    f.sync_all().map_err(|e| io_err(path, &e))?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| io_err(path, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest() -> Guest {
        Guest {
            v: GUEST_FILE_VERSION,
            cloud: "https://cloud.example.com".into(),
            computer_id: "cid".into(),
            host_sign_key: "hk".into(),
            bot_id: None,
            name: "Rex".into(),
            state: State::Joining,
            invite_id: "bi_1".into(),
            token: Some("secret-token".into()),
            created_at: 1,
            provider: Some("grok".into()),
            owner: None,
            agents: vec![],
        }
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    #[test]
    fn dir_precedence() {
        let p = Path::new("/h");
        assert_eq!(config_dir(Some(p), Some("/x"), Some("/u")), Some(PathBuf::from("/h")));
        assert_eq!(config_dir(None, Some("/x"), Some("/u")), Some(PathBuf::from("/x/kith")));
        assert_eq!(config_dir(None, Some(""), Some("/u")), Some(PathBuf::from("/u/.config/kith")));
        assert_eq!(config_dir(None, None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_0600_in_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        // The directory already exists with a loose mode: saving tightens it.
        let dir = tmp.path().join("kith");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let store = Store::open(Some(dir.clone()), None).unwrap();
        store.save(Some(&key()), &guest()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("device.key")), 0o600);
        assert_eq!(mode(&dir.join("guest.json")), 0o600);
        // Saving again (a state change) keeps it private and leaves no temp files behind.
        store.save(None, &guest()).unwrap();
        assert_eq!(mode(&dir.join("guest.json")), 0o600);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    }

    #[test]
    fn round_trip_and_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(tmp.path().join("k")), None).unwrap();
        assert!(store.load().unwrap().is_none());
        store.save(Some(&key()), &guest()).unwrap();
        let (k, g) = store.load().unwrap().unwrap();
        assert_eq!(k.to_bytes(), key().to_bytes());
        assert_eq!(g, guest());
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn guest_json_uses_the_documented_keys() {
        let v = serde_json::to_value(guest()).unwrap();
        for k in ["v", "cloud", "computerId", "hostSignKey", "name", "state", "inviteId", "token", "createdAt"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert_eq!(v["state"], "joining");
    }

    #[test]
    fn credentials_env_replaces_the_files_and_drops_the_token() {
        let line = credentials_line(&key(), &guest());
        let store = Store::open(None, Some(&line)).unwrap();
        assert!(store.is_env());
        let (k, g) = store.load().unwrap().unwrap();
        assert_eq!(k.to_bytes(), key().to_bytes());
        assert_eq!(g.token, None);
        assert_eq!(g.name, "Rex");
        // Never written back.
        store.save(None, &g).unwrap();
        store.clear().unwrap();
        assert!(!line.contains("secret-token"));
    }

    #[test]
    fn credentials_accept_standard_base64_with_padding() {
        let line = credentials_line(&key(), &guest());
        let bytes = decode_any(&line).unwrap();
        let std_padded = base64::engine::general_purpose::STANDARD.encode(bytes);
        assert!(Store::open(None, Some(&std_padded)).unwrap().is_env());
    }

    #[test]
    fn bad_credentials_are_a_usage_error() {
        assert_eq!(Store::open(None, Some("!!!")).unwrap_err().exit, crate::exit::Exit::Usage);
        assert_eq!(Store::open(None, Some(&b64(b"{}"))).unwrap_err().exit, crate::exit::Exit::Usage);
    }
}
