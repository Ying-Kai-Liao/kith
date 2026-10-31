//! Platform glue: data dir, background service install (launchd / systemd /
//! the Windows Run key, in `unix.rs` and `windows.rs`),
//! keeping the machine awake during turns, pairing addresses, and the Claude
//! Code status line we route through the host.
//!
//! Everything here is blocking (`std::fs`, `std::process`): async callers go
//! through `spawn_blocking`.

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{install, installed, require_executable, start, stop, uninstall};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{install, installed, require_executable, start, stop, uninstall};

static BINARY_IDENTITY: OnceLock<(String, String)> = OnceLock::new();

/// Snapshot before serving: reading the path again after an update would identify
/// the replacement on disk rather than this running process.
pub fn capture_binary_identity() -> Result<()> {
    let exe = current_exe()?;
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(std::fs::read(&exe)?) {
        write!(hash, "{byte:02x}")?;
    }
    let _ = BINARY_IDENTITY.set((exe.to_string_lossy().into_owned(), hash));
    Ok(())
}

/// This binary's resolved path, in the form other programs print it: Windows'
/// `canonicalize` adds a `\\?\` prefix that the apps' paths and shells don't have.
pub fn current_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    Ok(match exe.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) if cfg!(windows) => PathBuf::from(plain),
        _ => exe,
    })
}

pub fn binary_identity() -> Option<&'static (String, String)> {
    BINARY_IDENTITY.get()
}

/// Do not claim a restart succeeded while an old daemon still holds the data.
fn wait_for_host_exit() -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match lock_host() {
            Ok(lock) => {
                drop(lock);
                return Ok(());
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("old host did not exit; stop any manually started host before reinstalling"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

pub const DEFAULT_PORT: u16 = crate::environment::Environment::current().port();

/// The user's home. Kith can't do anything useful without one, so its absence is fatal.
pub(crate) fn home() -> PathBuf {
    dirs::home_dir().expect("HOME must be set: Kith keeps its data and agent settings there")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("KITH_SERVE_HOME").map_or_else(default_data_dir, PathBuf::from)
}

#[cfg(not(test))]
pub(crate) fn default_data_dir() -> PathBuf {
    home().join(crate::environment::Environment::current().data_folder())
}

/// Unit tests never see a real host's data: tests that start a hub run its start-up clean-up, which deletes files.
#[cfg(test)]
pub(crate) fn default_data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kith-unit-{}", std::process::id()))
}

fn create_parent(file: &Path) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    Ok(())
}

/// Held by the daemon for its entire lifetime. OS advisory locks are released
/// after a crash, so a stale lock file never prevents recovery.
pub fn lock_host() -> Result<std::fs::File> {
    let path = data_dir().join("host.lock");
    create_parent(&path)?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).context("another kith-serve is already running for this data directory")?;
    Ok(file)
}

/// How long a starting host waits for an exiting predecessor to release `host.lock` (launchd restarts it at once).
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// A marker older than this is not a host that is leaving now (it crashed or the exit stalled).
const EXITING_FRESH: Duration = Duration::from_secs(30);

fn exiting_marker() -> PathBuf {
    data_dir().join("host.exiting")
}

/// Called by a host right before it exits (watchdog or shutdown): tells a starting host that the lock is about
/// to be freed, so it should wait instead of refusing. Best effort; the OS drops the lock with the process.
pub fn mark_exiting() {
    let _ = std::fs::write(exiting_marker(), std::process::id().to_string());
}

fn predecessor_is_exiting() -> bool {
    let marker = exiting_marker();
    let Ok(pid) = std::fs::read_to_string(&marker) else { return false };
    pid.trim().parse::<u32>().is_ok_and(|pid| pid != std::process::id())
        && std::fs::metadata(&marker)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < EXITING_FRESH))
}

/// Like `lock_host`, but while the holder is marked as exiting (see `mark_exiting`) it retries every 100 ms for
/// up to `wait`. A live holder (no fresh marker, or a marker from an older build) fails at once, and no host
/// ever shares the lock. A stale marker is harmless: its process is gone so the lock is free, and taking the
/// lock clears it.
pub fn lock_host_within(wait: Duration) -> Result<std::fs::File> {
    let deadline = Instant::now() + wait;
    loop {
        match lock_host() {
            Ok(lock) => {
                let _ = std::fs::remove_file(exiting_marker());
                return Ok(lock);
            }
            Err(error) if Instant::now() >= deadline || !predecessor_is_exiting() => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let ok = Command::new(cmd).args(args).status().with_context(|| format!("running {cmd}"))?.success();
    if !ok {
        bail!("{cmd} {} failed", args.join(" "));
    }
    Ok(())
}

/// Holds a sleep inhibitor while any bot is working.
#[derive(Default)]
pub struct KeepAwake(Option<Child>);

impl KeepAwake {
    pub fn set(&mut self, on: bool) {
        match (on, self.0.is_some()) {
            (true, false) => {
                let pid = std::process::id().to_string();
                let child = if cfg!(target_os = "macos") {
                    Command::new("caffeinate").args(["-i", "-w", &pid]).stdout(Stdio::null()).spawn()
                } else if cfg!(windows) {
                    // The request lasts as long as the PowerShell holding it.
                    let script = format!(
                        "$k = Add-Type -Name P -Namespace W -PassThru -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern uint SetThreadExecutionState(uint f);'; $null = $k::SetThreadExecutionState(0x80000001); Wait-Process -Id {pid}"
                    );
                    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
                    Command::new("powershell")
                        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &B64.encode(utf16)])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                } else {
                    Command::new("systemd-inhibit")
                        .args([
                            "--what=sleep:idle",
                            "--who=Kith",
                            "--why=A bot is working or a routine is scheduled",
                            "--mode=block",
                            "sleep",
                            "infinity",
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                };
                self.0 = child.map_err(|error| tracing::warn!(%error, "can't keep the machine awake")).ok();
            }
            (false, true) => {
                if let Some(mut c) = self.0.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            _ => {}
        }
    }
}

/// Addresses the phone can try, best first: Tailscale, then LAN.
pub fn addresses(port: u16) -> Vec<String> {
    let mut tailscale = vec![];
    let mut lan = vec![];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if iface.is_loopback() {
            continue;
        }
        if let std::net::IpAddr::V4(ip) = iface.ip() {
            let o = ip.octets();
            // 100.64.0.0/10 is Tailscale's CGNAT range.
            if o[0] == 100 && (64..128).contains(&o[1]) {
                tailscale.push(format!("http://{ip}:{port}"));
            } else if ip.is_private() {
                lan.push(format!("http://{ip}:{port}"));
            }
        }
    }
    if let Ok(out) = Command::new("tailscale").args(["status", "--json"]).output()
        && let Ok(v) = serde_json::from_slice::<Value>(&out.stdout)
        && let Some(dns) = v["Self"]["DNSName"].as_str()
    {
        let dns = dns.trim_end_matches('.');
        if !dns.is_empty() {
            tailscale.insert(0, format!("http://{dns}:{port}"));
        }
    }
    tailscale.sort();
    tailscale.dedup();
    lan.sort();
    lan.dedup();
    tailscale.into_iter().chain(lan).collect()
}

pub fn host_name() -> String {
    let n = gethostname::gethostname().to_string_lossy().into_owned();
    n.trim_end_matches(".local").to_owned()
}

/// What the computer is, so phones can draw the right icon (Linux gets its penguin;
/// Windows counts as a desktop).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Laptop,
    MacMini,
    MacStudio,
    IMac,
    MacPro,
    Desktop,
    Linux,
}

/// Detected once; on macOS from `system_profiler`'s model name.
pub fn device() -> Device {
    static DEVICE: OnceLock<Device> = OnceLock::new();
    *DEVICE.get_or_init(|| {
        if cfg!(windows) {
            return Device::Desktop;
        }
        if !cfg!(target_os = "macos") {
            return Device::Linux;
        }
        let out = Command::new("system_profiler").args(["SPHardwareDataType", "-json"]).output();
        let json: Value = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
        mac_device(json["SPHardwareDataType"][0]["machine_name"].as_str().unwrap_or_default())
    })
}

fn mac_device(machine_name: &str) -> Device {
    match machine_name {
        n if n.starts_with("MacBook") => Device::Laptop,
        "Mac mini" => Device::MacMini,
        "Mac Studio" => Device::MacStudio,
        "iMac" | "iMac Pro" => Device::IMac,
        "Mac Pro" => Device::MacPro,
        _ => Device::Desktop,
    }
}

/// RFC 3986 percent-encoding of everything but unreserved characters.
fn pct(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// What the pairing QR carries (spec §4.1): public keys and a one-time code, no secrets
/// that outlive the code.
pub struct PairingQr<'a> {
    pub name: &'a str,
    pub computer_id: &'a str,
    pub sign_key: &'a str,
    pub box_key: &'a str,
    pub code: &'a str,
    pub urls: &'a [String],
    pub cloud: Option<&'a str>,
}

pub fn pairing_url(q: &PairingQr) -> String {
    let mut url = format!(
        "{}://pair?v=3&name={}&id={}&sk={}&bk={}&code={}&urls={}",
        crate::environment::Environment::current().url_scheme(),
        pct(q.name),
        q.computer_id,
        q.sign_key,
        q.box_key,
        q.code,
        pct(&q.urls.join(","))
    );
    if let Some(cloud) = q.cloud {
        url.push_str("&cloud=");
        url.push_str(&pct(cloud));
    }
    url
}

fn read_settings(settings: &Path) -> Result<Option<Value>> {
    match std::fs::read_to_string(settings) {
        Ok(text) => Ok(Some(serde_json::from_str(&text).context("parsing Claude settings")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", settings.display())),
    }
}

fn write_settings(settings: &Path, v: &Value) -> Result<()> {
    create_parent(settings)?;
    std::fs::write(settings, serde_json::to_string_pretty(v)? + "\n")
        .with_context(|| format!("writing {}", settings.display()))
}

/// Routes Claude Code's status line through `kith-serve statusline` so usage
/// limits reach the host locally. An existing status line keeps working: it is
/// wrapped (`kith-serve statusline -- <original>`) and restored on uninstall.
///
/// `host` is the stable installed binary to wrap with; `None` (dev builds, app
/// bundle, Homebrew Cellar, cargo target) never wraps, it only repairs a
/// wrapper whose binary no longer exists, so the user's own line is never lost.
pub fn ensure_statusline(settings: &Path, host: Option<&Path>) -> Result<bool> {
    let mut v = read_settings(settings)?.unwrap_or_else(|| json!({}));
    let current = v["statusLine"]["command"].as_str().map(str::to_owned);
    if v.get("statusLine").is_some() && current.is_none() {
        return Ok(false); // not a command status line; leave it alone
    }
    let wrapper = current.as_deref().and_then(wrapper_parts);
    let new_command = match (host, wrapper) {
        // A wrapper whose binary is gone: back to the user's own line.
        // With a stable host the repaired line is wrapped again in the same step.
        (None, Some((bin, original))) if !Path::new(bin).exists() => original.to_owned(),
        (Some(host), Some((bin, original))) if !Path::new(bin).exists() => wrap(host, original),
        (None, _) => return Ok(false),
        (Some(host), Some((bin, original))) => {
            if Path::new(bin) == host {
                return Ok(false);
            }
            wrap(host, original)
        }
        (Some(host), None) => wrap(host, current.as_deref().unwrap_or("")),
    };
    if new_command.is_empty() {
        if let Some(root) = v.as_object_mut() {
            root.remove("statusLine");
        }
    } else {
        v["statusLine"] = json!({"type": "command", "command": new_command});
    }
    write_settings(settings, &v)?;
    Ok(true)
}

fn wrap(host: &Path, original: &str) -> String {
    let ours = format!("'{}' statusline", host.to_string_lossy());
    if original.is_empty() { ours } else { format!("{ours} -- {original}") }
}

/// Undoes [`ensure_statusline`].
pub fn restore_statusline(settings: &Path) -> Result<()> {
    let Some(mut v) = read_settings(settings)? else { return Ok(()) };
    let Some(cmd) = v["statusLine"]["command"].as_str().map(str::to_owned) else { return Ok(()) };
    match unwrap_statusline(&cmd) {
        None => return Ok(()),
        Some("") => {
            if let Some(root) = v.as_object_mut() {
                root.remove("statusLine");
            }
        }
        Some(original) => v["statusLine"]["command"] = original.into(),
    }
    write_settings(settings, &v)
}

/// The host binary path and the command a wrapper wraps (empty when it wraps
/// nothing), or `None` when `cmd` isn't a `'<host>' statusline [-- <cmd>]` wrapper.
fn wrapper_parts(cmd: &str) -> Option<(&str, &str)> {
    // Ours always starts with the quoted host path followed by ` statusline`.
    let rest = cmd.strip_prefix('\'')?;
    let (bin, after) = rest.split_once("' statusline")?;
    let original = if after.is_empty() { "" } else { after.strip_prefix(" --")?.trim() };
    Some((bin, original))
}

/// The command our wrapper wraps (empty when it wraps nothing), or `None`
/// when `cmd` isn't ours.
fn unwrap_statusline(cmd: &str) -> Option<&str> {
    wrapper_parts(cmd).map(|(_, original)| original)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: both cases use the process-wide host.lock, so they cannot run in parallel.
    #[test]
    fn a_live_holder_fails_a_start_at_once_but_an_exiting_one_is_waited_for() {
        let held = lock_host().unwrap();
        let started = Instant::now();
        assert!(lock_host_within(Duration::from_secs(5)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        // The marker names another process, as a real predecessor's would.
        std::fs::write(exiting_marker(), (std::process::id() + 1).to_string()).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(held);
        });
        let started = Instant::now();
        let lock = lock_host_within(Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(!exiting_marker().exists());
        releaser.join().unwrap();
        drop(lock);
    }

    fn temp_settings(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.json");
        std::fs::write(&f, contents).unwrap();
        f
    }

    fn read(f: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap()
    }

    #[test]
    fn unit_tests_keep_out_of_the_real_data_folder() {
        assert!(
            std::env::var_os("KITH_SERVE_HOME").is_some() || !data_dir().starts_with(home()),
            "{}",
            data_dir().display()
        );
    }

    #[test]
    fn mac_models_map_to_devices() {
        assert_eq!(mac_device("MacBook Pro"), Device::Laptop);
        assert_eq!(mac_device("MacBook Air"), Device::Laptop);
        assert_eq!(mac_device("Mac mini"), Device::MacMini);
        assert_eq!(mac_device("Mac Studio"), Device::MacStudio);
        assert_eq!(mac_device("iMac"), Device::IMac);
        assert_eq!(mac_device(""), Device::Desktop);
        assert_eq!(serde_json::to_value(Device::MacMini).unwrap(), "macmini");
    }

    #[test]
    fn pairing_url_is_escaped() {
        let urls = ["http://100.1.2.3:19222".to_owned(), "http://a:1".to_owned()];
        let mut q = PairingQr {
            name: "Kevin's Mac",
            computer_id: "cid",
            sign_key: "sk",
            box_key: "bk",
            code: "c0de",
            urls: &urls,
            cloud: None,
        };
        assert_eq!(
            pairing_url(&q),
            format!(
                "{}://pair?v=3&name=Kevin%27s%20Mac&id=cid&sk=sk&bk=bk&code=c0de&urls=http%3A%2F%2F100.1.2.3%3A19222%2Chttp%3A%2F%2Fa%3A1",
                crate::environment::Environment::current().url_scheme()
            )
        );
        q.cloud = Some("https://cloud.example");
        assert!(pairing_url(&q).ends_with("&cloud=https%3A%2F%2Fcloud.example"));
    }

    /// A real file standing in for an installed host binary.
    fn fake_bin(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-bin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(name);
        std::fs::write(&f, "").unwrap();
        f
    }

    fn command(f: &Path) -> String {
        read(f)["statusLine"]["command"].as_str().unwrap().to_owned()
    }

    #[test]
    fn statusline_wraps_and_restores() {
        let host = fake_bin("kith");
        let f = temp_settings(r#"{"statusLine":{"type":"command","command":"sh ~/mine.sh"}}"#);
        assert!(ensure_statusline(&f, Some(&host)).unwrap());
        assert!(!ensure_statusline(&f, Some(&host)).unwrap(), "idempotent: no double wrap");
        assert_eq!(command(&f), format!("'{}' statusline -- sh ~/mine.sh", host.display()));
        restore_statusline(&f).unwrap();
        assert_eq!(command(&f), "sh ~/mine.sh");
    }

    #[test]
    fn statusline_without_original_is_removed_on_restore() {
        let host = fake_bin("kith");
        let f = temp_settings("{}");
        assert!(ensure_statusline(&f, Some(&host)).unwrap());
        assert_eq!(command(&f), format!("'{}' statusline", host.display()));
        restore_statusline(&f).unwrap();
        assert!(read(&f).get("statusLine").is_none());
        // No settings file at all: wrapping creates it, restoring a missing one is a no-op.
        let missing = temp_settings("{}").with_file_name("none.json");
        assert!(ensure_statusline(&missing, Some(&host)).unwrap());
        restore_statusline(&temp_settings("{}").with_file_name("absent.json")).unwrap();
    }

    #[test]
    fn statusline_leaves_non_command_lines_alone() {
        let host = fake_bin("kith");
        let text = r#"{"statusLine":{"type":"static","text":"hi"}}"#;
        let f = temp_settings(text);
        assert!(!ensure_statusline(&f, Some(&host)).unwrap());
        restore_statusline(&f).unwrap();
        assert_eq!(read(&f), serde_json::from_str::<Value>(text).unwrap());
    }

    #[test]
    fn statusline_repoints_a_moved_host() {
        let host = fake_bin("kith");
        let old = fake_bin("kith-serve"); // an older Kith build, still on disk
        let f = temp_settings(&format!(
            r#"{{"statusLine":{{"type":"command","command":"'{}' statusline -- sh ~/mine.sh"}}}}"#,
            old.display()
        ));
        assert!(ensure_statusline(&f, Some(&host)).unwrap());
        assert_eq!(command(&f), format!("'{}' statusline -- sh ~/mine.sh", host.display()));
    }

    #[test]
    fn statusline_unwraps_dangling_wrappers() {
        let host = fake_bin("kith");
        let cmd = "'/gone/kith-serve' statusline -- sh ~/mine.sh";
        let f = temp_settings(&format!(r#"{{"statusLine":{{"type":"command","command":"{cmd}"}}}}"#));
        assert!(ensure_statusline(&f, None).unwrap(), "{cmd}");
        assert_eq!(command(&f), "sh ~/mine.sh", "repair only, no wrap");
        let f = temp_settings(&format!(r#"{{"statusLine":{{"type":"command","command":"{cmd}"}}}}"#));
        assert!(ensure_statusline(&f, Some(&host)).unwrap(), "{cmd}");
        assert_eq!(command(&f), format!("'{}' statusline -- sh ~/mine.sh", host.display()));
        // Dangling with nothing wrapped: the line is dropped, not left pointing nowhere.
        let f = temp_settings(r#"{"statusLine":{"type":"command","command":"'/gone/kith-serve' statusline"}}"#);
        assert!(ensure_statusline(&f, None).unwrap());
        assert!(read(&f).get("statusLine").is_none());
    }

    #[test]
    fn statusline_without_a_stable_host_never_wraps() {
        let f = temp_settings(r#"{"statusLine":{"type":"command","command":"sh ~/mine.sh"}}"#);
        assert!(!ensure_statusline(&f, None).unwrap());
        assert_eq!(command(&f), "sh ~/mine.sh");
        let live = fake_bin("kith-serve");
        let wrapped = format!("'{}' statusline -- sh ~/mine.sh", live.display());
        let f = temp_settings(&format!(r#"{{"statusLine":{{"type":"command","command":"{wrapped}"}}}}"#));
        assert!(!ensure_statusline(&f, None).unwrap(), "a live wrapper is left as is");
        assert_eq!(command(&f), wrapped);
    }
}
