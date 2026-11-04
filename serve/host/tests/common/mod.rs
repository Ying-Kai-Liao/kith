//! Shared by the end-to-end tests.

pub mod host;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The host's machine state for `home`. The core's owner socket lives under it, and a Unix socket path holds about a
/// hundred bytes, which a macOS temporary home alone exceeds: the vault reaches the secrets envelope through that
/// socket, so the folder is a short one in /tmp, the same for the same home (a restarted host finds its state).
fn machine_state(home: &Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::hash::{Hash as _, Hasher as _};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        home.hash(&mut hash);
        std::path::PathBuf::from("/tmp").join(format!("ks-{:x}", hash.finish()))
    }
    #[cfg(not(unix))]
    {
        home.join("machine-state")
    }
}

/// A test that picks its own `KITH_STATE` gets it through a short path in /tmp (a link to it), for the reason
/// `machine_state` is short: the core binds its owner socket under it, and a socket path of a macOS temporary folder is
/// too long to bind. Without the core the vault refuses (secrets need it), and a bot that reads a saved key cannot start.
fn short_link(state: &str) -> String {
    #[cfg(unix)]
    {
        use std::hash::{Hash as _, Hasher as _};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        state.hash(&mut hash);
        let link = std::path::PathBuf::from("/tmp").join(format!("ks-{:x}", hash.finish()));
        let _ = std::fs::create_dir_all(state);
        let _ = std::fs::remove_file(&link);
        if std::os::unix::fs::symlink(state, &link).is_ok() {
            return link.to_string_lossy().into_owned();
        }
    }
    state.to_owned()
}

/// Runs `kith-serve serve` for `home` until it answers: the process and its base URL. A port
/// picked here is only free for a moment; when something else takes it before the host binds, the
/// host exits and this starts it again on another one.
#[allow(dead_code)]
pub async fn serve(home: &Path) -> (Child, String) {
    serve_env(home, &[]).await
}

/// Kills the host it holds when dropped: a panic or failed assert before the caller owns the child (it never
/// answered `/health`, the channels guard failed) must not leave a host running, connected to real bots.
struct Reaper(Option<Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// [`serve`] with extra environment variables for the host.
pub async fn serve_env(home: &Path, env: &[(&str, &str)]) -> (Child, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0").expect("free port").local_addr().expect("port").port();
        let base = format!("http://127.0.0.1:{port}");
        let mut child = Reaper(Some(
            Command::new(env!("CARGO_BIN_EXE_kith-serve"))
                .args(["serve", "--bind", "127.0.0.1", "--port", &port.to_string()])
                .env("KITH_SERVE_HOME", home)
                .env("KITH_SERVE_CLOUD", "off")
                // Never the hq on this computer: a test that wants one passes its own `KITH_KERNEL_BIN`.
                .env("KITH_KERNEL_BIN", "")
                .env("HQ_BIN", "")
                // The test machine's memory and a two-agent limit must not hold a test back (see `gate_e2e`).
                // Never this computer's state: the fake agents share a session id, and turn claims and terminal
                // registries live under it. An explicit folder inside the test's home.
                .env("KITH_STATE", machine_state(home))
                // Never Kai's real master key item: a debug core reads this account name instead (see core keystore.rs).
                .env(
                    "KITH_SECRETS_KEYSTORE_ACCOUNT",
                    format!("test-{}", machine_state(home).display().to_string().replace('/', "-")),
                )
                .env("CLAUDE_CONFIG_DIR", home.join("claude-config"))
                .env("KITH_SERVE_MAX_RUNNING_AGENTS", "64")
                .env("KITH_SERVE_MIN_FREE_MEMORY_PERCENT", "0")
                .env("KITH_SERVE_MAX_SWAP_USED_GB", "0")
                .envs(env.iter().map(|(k, v)| (*k, if *k == "KITH_STATE" { short_link(v) } else { (*v).to_owned() })))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the host"),
        ));
        while child.0.as_mut().unwrap().try_wait().expect("host state").is_none() {
            if answered_by(home, &base).await {
                // A temporary home never runs the real bots' channels (unless a test opts in).
                if !env.iter().any(|(k, _)| *k == "KITH_SERVE_CHANNELS") {
                    assert_channels_off(&base).await;
                }
                return (child.0.take().unwrap(), base);
            }
            assert!(Instant::now() < deadline, "host didn't start");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(Instant::now() < deadline, "host didn't start");
    }
}

/// Whether the host answering at `base` is the one for `home`. `/health` alone is not proof: another
/// test's host may have taken the port between the pick and our bind, so ours exits while theirs
/// answers. Theirs has another token, so a call with this home's token gets a 401 from it. Ours
/// writes the token file before it listens, so no file yet means no answer from ours either, on
/// first start and on a restart (whose old file holds the same token) alike.
async fn answered_by(home: &Path, base: &str) -> bool {
    let Ok(token) = std::fs::read_to_string(home.join("token")) else { return false };
    let token = token.trim();
    if token.is_empty() {
        return false;
    }
    // An unknown method is rejected after the token check and does nothing else.
    match reqwest::Client::new().post(format!("{base}/api/probe-whose-host")).bearer_auth(token).send().await {
        Ok(res) => res.status() != reqwest::StatusCode::UNAUTHORIZED,
        Err(_) => false,
    }
}

/// The host's token file, once it is there: the host writes it independently of answering
/// `/health`, and `std::fs::write` truncates first, so a read can see no file or an empty one.
#[allow(dead_code)]
pub fn read_token(home: &Path) -> String {
    let path = home.join("token");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(t) = std::fs::read_to_string(&path)
            && !t.trim().is_empty()
        {
            return t;
        }
        assert!(Instant::now() < deadline, "the host never wrote its token file {}", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The host decides about channels in the background after it listens: waits for `/health` to say they are off.
async fn assert_channels_off(base: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let health: serde_json::Value = reqwest::get(format!("{base}/health")).await.unwrap().json().await.unwrap();
        let channels = &health["channels"];
        assert!(
            channels["connections"].as_array().is_none_or(Vec::is_empty),
            "a test host connected to channels: {channels}"
        );
        if channels["off"] == "temporary home" {
            return;
        }
        assert!(Instant::now() < deadline, "a temporary home didn't turn channels off: {channels}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Stops a test host the way launchd does (SIGTERM), so it shuts down its own
/// children: a SIGKILL would orphan whatever it had running, such as a harness
/// sign-in check (`cursor-agent status` then spins forever). Kills it after 5 s.
pub fn stop(child: &mut Child) {
    let _ = Command::new("kill").arg(child.id().to_string()).status();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Python as the test machine names it.
pub fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// The agent command that runs a Python fixture (quoted for `sh -c` / `cmd /c`). UTF-8 mode makes it read the host's
/// UTF-8 the way real agents do: Windows Python reads pipes in the ANSI code page, turning a `”` into a lone surrogate.
pub fn python_agent(script: &Path) -> String {
    let path = script.display().to_string();
    if cfg!(windows) {
        format!("{} -X utf8 -u \"{path}\"", python())
    } else {
        format!("{} -X utf8 -u '{}'", python(), path.replace('\'', "'\\''"))
    }
}

/// What a client holds after the event stream's catch-up: `{"rev", "bots": [...], "entries": [...]}`,
/// the newest row per id. Reads until the stream goes quiet.
pub async fn snapshot(base: &str, token: &str) -> serde_json::Value {
    use futures::StreamExt as _;
    let res = reqwest::Client::new()
        .get(format!("{base}/events?since=0&client=test"))
        .bearer_auth(token)
        .send()
        .await
        .expect("open the event stream");
    let mut body = res.bytes_stream();
    let mut buf = String::new();
    let (mut rev, mut bots, mut entries) = (serde_json::Value::Null, Vec::<serde_json::Value>::new(), Vec::new());
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout(Duration::from_millis(300), body.next()).await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find('\n') {
            let line: String = buf.drain(..=i).collect();
            let Some(event) = line
                .trim_end()
                .strip_prefix("data:")
                .and_then(|data| serde_json::from_str::<serde_json::Value>(data.trim_start()).ok())
            else {
                continue;
            };
            let (rows, row) = match event["type"].as_str() {
                Some("hello") => {
                    rev = event["rev"].clone();
                    continue;
                }
                Some("bot") => (&mut bots, event["bot"].clone()),
                Some("entry") => (&mut entries, event["entry"].clone()),
                _ => continue,
            };
            rows.retain(|r| r["id"] != row["id"]);
            rows.push(row);
        }
    }
    serde_json::json!({"rev": rev, "bots": bots, "entries": entries})
}
