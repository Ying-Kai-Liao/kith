//! Local text to speech: Gradium's Phonon 31M English model, run by the `kith-tts` helper.
//!
//! The helper is a separate process (never inside this one, so a crash in the model code can't take the
//! channels down). It starts with the first request, answers JSON lines on stdio and is stopped after five
//! idle minutes. The model files are downloaded on first use from pinned Hugging Face revisions, checked
//! against SHA-256 constants and only then moved into place; see `docs/local-voice.md`.

use crate::LockExt;
use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub const MODEL: &str = "phonon-31m-en";
const SAMPLE_RATE: u64 = 24_000;
/// The model's own voice, always present.
pub const DEFAULT_VOICE: &str = "default";
const IDLE: Duration = Duration::from_secs(5 * 60);
/// A long reply (4000 bytes) takes seconds at the model's speed; this only catches a wedged helper.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(120);
/// No bytes for this long fails a download.
const STALL: Duration = Duration::from_secs(30);

// Revisions are the ones the `ptts` crate pins for its default model and voice catalog, so the files are the
// ones that crate was tested against. Never `main`.
const MODEL_REPO: &str =
    "https://huggingface.co/gradium/phonon-31m-en/resolve/8c64b55ec301728e2d1695bfea87a08a267d0c2d";
const VOICE_REPO: &str =
    "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3";

/// One file of the model folder: where it goes, where it comes from, what it must hash to.
#[derive(Clone, Debug)]
pub struct Spec {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

fn spec(name: &str, url: String, sha256: &str, size: u64) -> Spec {
    Spec { name: name.to_owned(), url, sha256: sha256.to_owned(), size }
}

/// Phonon 31M (Apache-2.0) and four English voices from `gradium/phonon-voices` (Apache-2.0). The 40M models
/// are CC BY-NC and must not be added here.
fn files() -> Vec<Spec> {
    let model = |f: &str| format!("{MODEL_REPO}/{f}");
    let voice = |id: &str| format!("{VOICE_REPO}/voices/{id}.safetensors");
    vec![
        spec(
            "config.json",
            model("config.json"),
            "7062ea376e6b158543c8abec8e5af67df171c49ad70eb7e2a0fb25c16f265e06",
            2851,
        ),
        spec(
            "tokenizer.json",
            model("tokenizer.json"),
            "691a00068fa7b989f2f3e5ae5daa2d1a5bb319f2d5360deafcfb19a8bb6e7936",
            277_967,
        ),
        spec(
            "model.q8.gguf",
            model("model.q8.gguf"),
            "a7d9aad2cd6dc66473f24a08bc6d2939030ef4d751ef82349361fbef3272dbae",
            64_512_352,
        ),
        spec(
            "default-voice.safetensors",
            model("default-voice.safetensors"),
            "f739e8c4c76ee7f92e7c94a82002748c8cd22e99582ac49f7b9b94cd63786e18",
            256_136,
        ),
        spec(
            "voices/zoey.safetensors",
            voice("NbpkqMVS3CJeq2j8"),
            "97c7f8c253413574406ab80dd23f49c3ebb2517163b00a9bf86b9c4cf43e7e23",
            256_136,
        ),
        spec(
            "voices/harper.safetensors",
            voice("4SZHfMpw-p46Ywgs"),
            "6b3da6e2d796a20c51af892c482bd6509fb88267a221aff83c870843dee2a123",
            256_136,
        ),
        spec(
            "voices/easton.safetensors",
            voice("LMDIYE0Oa377Qh6Z"),
            "45c582673cb20edb04ddcca2b5a8865ddfb0a82599efadb269021e87d20fbdd4",
            256_136,
        ),
        spec(
            "voices/miles.safetensors",
            voice("d9XCyhjo4JhkHF1d"),
            "eab20e4f159b48cfbc01b1885804e24f681f80e7a64f909182763a4e3361bee8",
            256_136,
        ),
    ]
}

/// Voice names the apps may ask for: the model's own, then one file per voice under `voices/`.
fn voices_of(specs: &[Spec]) -> Vec<String> {
    let mut v = vec![DEFAULT_VOICE.to_owned()];
    v.extend(
        specs.iter().filter_map(|s| s.name.strip_prefix("voices/")?.strip_suffix(".safetensors").map(str::to_owned)),
    );
    v
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// ---- the helper process ------------------------------------------------------------------------------

/// How to start the helper.
#[derive(Clone, Debug)]
pub struct HelperCmd {
    pub program: PathBuf,
    pub args: Vec<String>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    last_used: Instant,
    /// Tells a watcher from an earlier run that it has been replaced.
    generation: usize,
}

/// The helper process, started on demand. One request at a time: the lock is held across a request, so two
/// at once (a call prefetching the next sentence while one plays) are answered in turn, not interleaved.
pub struct Helper {
    cmd: HelperCmd,
    idle: Duration,
    state: tokio::sync::Mutex<Option<Running>>,
    next_id: AtomicU64,
    /// How many times the process was started (tests, and the log line).
    spawns: AtomicUsize,
}

impl Helper {
    pub fn new(cmd: HelperCmd, idle: Duration) -> Arc<Self> {
        Arc::new(Self {
            cmd,
            idle,
            state: tokio::sync::Mutex::new(None),
            next_id: AtomicU64::new(1),
            spawns: AtomicUsize::new(0),
        })
    }

    #[cfg(test)]
    pub async fn running(&self) -> bool {
        self.state.lock().await.is_some()
    }

    fn start(&self) -> Result<Running> {
        let mut child = Command::new(&self.cmd.program)
            .args(&self.cmd.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // A host that exits or is killed takes the helper with it.
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("can't start {}", self.cmd.program.display()))?;
        let stdin = child.stdin.take().context("helper has no stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("helper has no stdout")?).lines();
        let generation = self.spawns.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::info!(generation, "started the local voice helper");
        Ok(Running { child, stdin, stdout, last_used: Instant::now(), generation })
    }

    /// Sends one request and returns its `result`. If the helper died, starts it again and tries once more.
    pub async fn call(self: &Arc<Self>, method: &str, params: &Value, timeout: Duration) -> Result<Value> {
        let mut guard = self.state.lock().await;
        let mut last = anyhow!("the voice helper isn't running");
        for attempt in 0..2 {
            if guard.as_mut().is_some_and(|r| !matches!(r.child.try_wait(), Ok(None))) {
                *guard = None;
            }
            if guard.is_none() {
                let running = self.start()?;
                let generation = running.generation;
                *guard = Some(running);
                tokio::spawn(self.clone().stop_when_idle(generation));
            }
            let running = guard.as_mut().context("helper vanished")?;
            match self.exchange(running, method, params, timeout).await {
                Ok(v) => {
                    running.last_used = Instant::now();
                    return Ok(v);
                }
                // The helper answered with an error: that request failed, the process is fine.
                Err((e, false)) => return Err(e),
                Err((e, true)) => {
                    tracing::warn!(error = %e, attempt, "the local voice helper failed; restarting it");
                    *guard = None; // kill_on_drop
                    last = e;
                }
            }
        }
        Err(last.context("the voice helper keeps failing"))
    }

    /// `Err((error, helper_is_broken))`.
    async fn exchange(
        &self,
        r: &mut Running,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, (anyhow::Error, bool)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = format!("{}\n", json!({"id": id, "method": method, "params": params}));
        let go = async {
            r.stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| (anyhow!("can't write to the voice helper: {e}"), true))?;
            r.stdin.flush().await.map_err(|e| (anyhow!("can't write to the voice helper: {e}"), true))?;
            loop {
                let answer =
                    r.stdout.next_line().await.map_err(|e| (anyhow!("can't read the voice helper: {e}"), true))?;
                let answer = answer.ok_or_else(|| (anyhow!("the voice helper stopped"), true))?;
                let Ok(v) = serde_json::from_str::<Value>(&answer) else { continue };
                if v["id"].as_u64() != Some(id) {
                    continue;
                }
                return match v["error"].as_str() {
                    Some(e) => Err((anyhow!("{e}"), false)),
                    None => Ok(v["result"].clone()),
                };
            }
        };
        match tokio::time::timeout(timeout, go).await {
            Ok(r) => r,
            Err(_) => Err((anyhow!("the voice helper didn't answer in {} s", timeout.as_secs()), true)),
        }
    }

    /// Ends this run of the helper once it has been unused for `idle`.
    async fn stop_when_idle(self: Arc<Self>, generation: usize) {
        let mut wait = self.idle;
        loop {
            tokio::time::sleep(wait).await;
            let mut guard = self.state.lock().await;
            match guard.as_ref() {
                Some(r) if r.generation == generation => {
                    let idle = r.last_used.elapsed();
                    if idle >= self.idle {
                        tracing::info!("stopping the idle local voice helper");
                        *guard = None;
                        return;
                    }
                    wait = self.idle.saturating_sub(idle);
                }
                _ => return,
            }
        }
    }

    /// Resident memory of the running helper, if it is running and free to answer right now. Asking neither
    /// starts the helper nor counts as use, so a status poll can't keep it alive.
    pub async fn rss_bytes(&self) -> Option<u64> {
        // A request in progress holds the lock; status must not wait for it.
        let mut guard = self.state.try_lock().ok()?;
        let running = guard.as_mut()?;
        let v = self.exchange(running, "status", &json!({}), Duration::from_secs(2)).await.ok()?;
        v["rssBytes"].as_u64()
    }
}

// ---- the model files ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Download {
    Idle,
    Running,
    Failed(String),
}

pub struct Local {
    dir: PathBuf,
    files: Vec<Spec>,
    /// Why this computer can't run it (wrong platform, no helper binary).
    unavailable: Option<String>,
    download: Mutex<Download>,
    done: AtomicU64,
    changed: tokio::sync::watch::Sender<u64>,
    helper: Option<Arc<Helper>>,
    /// Counts download runs started (tests).
    runs: AtomicUsize,
}

impl Local {
    pub fn new(dir: PathBuf, files: Vec<Spec>, helper: Result<HelperCmd, String>, idle: Duration) -> Arc<Self> {
        let (helper, unavailable) = match helper {
            Ok(cmd) => (Some(Helper::new(cmd, idle)), None),
            Err(why) => (None, Some(why)),
        };
        Arc::new(Self {
            dir,
            files,
            unavailable,
            download: Mutex::new(Download::Idle),
            done: AtomicU64::new(0),
            changed: tokio::sync::watch::channel(0).0,
            helper,
            runs: AtomicUsize::new(0),
        })
    }

    fn present(&self, s: &Spec) -> bool {
        std::fs::metadata(self.dir.join(&s.name)).is_ok_and(|m| m.len() == s.size)
    }

    fn ready(&self) -> bool {
        self.files.iter().all(|s| self.present(s))
    }

    fn total(&self) -> u64 {
        self.files.iter().map(|s| s.size).sum()
    }

    /// `voiceLocalStatus`.
    pub async fn status(self: &Arc<Self>) -> Value {
        let download = self.download.locked().clone();
        let (state, progress, error) = if self.ready() {
            ("ready", None, None)
        } else {
            match download {
                Download::Running => ("downloading", Some(self.progress()), None),
                Download::Failed(e) => ("error", None, Some(e)),
                Download::Idle => ("missing", None, None),
            }
        };
        let mut out = json!({
            "available": self.unavailable.is_none(),
            "model": MODEL,
            "state": state,
            "voices": voices_of(&self.files),
            "defaultVoice": DEFAULT_VOICE,
        });
        if let Some(p) = progress {
            out["progress"] = json!(p);
        }
        // An unavailable computer says why; so does a failed download.
        if let Some(e) = self.unavailable.clone().or(error) {
            out["error"] = json!(e);
        }
        if let Some(rss) = match &self.helper {
            Some(h) => h.rss_bytes().await,
            None => None,
        } {
            out["helperRssBytes"] = json!(rss);
        }
        out
    }

    #[allow(clippy::cast_precision_loss)]
    fn progress(&self) -> f64 {
        let total = self.total().max(1);
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).clamp(0.0, 1.0)
    }

    /// `voiceLocalPrepare`: starts the download if the model is missing (once, however many callers).
    pub async fn prepare(self: &Arc<Self>) -> Value {
        if self.unavailable.is_none() && !self.ready() {
            let mut d = self.download.locked();
            if *d != Download::Running {
                *d = Download::Running;
                self.runs.fetch_add(1, Ordering::Relaxed);
                drop(d);
                tokio::spawn(self.clone().download_all());
            }
        }
        self.status().await
    }

    async fn download_all(self: Arc<Self>) {
        let result = self.fetch_missing().await;
        if let Err(e) = &result {
            tracing::warn!(error = %e, "downloading the local voice model failed");
        }
        *self.download.locked() = match result {
            Ok(()) => Download::Idle,
            Err(e) => Download::Failed(format!("{e:#}")),
        };
        self.changed.send_modify(|n| *n += 1);
    }

    async fn fetch_missing(&self) -> Result<()> {
        self.done.store(0, Ordering::Relaxed);
        for s in &self.files {
            if self.present(s) {
                self.done.fetch_add(s.size, Ordering::Relaxed);
                continue;
            }
            self.fetch(s).await.with_context(|| format!("downloading {}", s.name))?;
        }
        Ok(())
    }

    /// One file: into `<name>.part`, hashed on the way, renamed only when the hash matches. A `.part` left by
    /// a killed host is simply overwritten.
    async fn fetch(&self, s: &Spec) -> Result<()> {
        let dest = self.dir.join(&s.name);
        let part = dest.with_file_name(format!("{}.part", dest.file_name().context("file name")?.to_string_lossy()));
        tokio::fs::create_dir_all(dest.parent().context("folder")?).await?;
        let before = self.done.load(Ordering::Relaxed);
        let res = crate::http().get(&s.url).send().await.map_err(|e| anyhow!("can't reach Hugging Face: {e}"))?;
        if !res.status().is_success() {
            bail!("Hugging Face answered {}", res.status());
        }
        let mut out = tokio::fs::File::create(&part).await?;
        let mut hash = Sha256::new();
        let mut stream = res.bytes_stream();
        let mut size = 0u64;
        let written: Result<()> = async {
            while let Some(chunk) =
                tokio::time::timeout(STALL, stream.next()).await.map_err(|_| anyhow!("the download stalled"))?
            {
                let chunk = chunk.map_err(|e| anyhow!("the download broke: {e}"))?;
                hash.update(&chunk);
                size += chunk.len() as u64;
                out.write_all(&chunk).await?;
                self.done.store(before + size.min(s.size), Ordering::Relaxed);
            }
            out.flush().await?;
            Ok(())
        }
        .await;
        drop(out);
        if let Err(e) = written {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        let got = hex(&hash.finalize());
        if size != s.size || !got.eq_ignore_ascii_case(&s.sha256) {
            let _ = tokio::fs::remove_file(&part).await;
            bail!("the file failed its checksum ({size} bytes, sha256 {got}); deleted");
        }
        tokio::fs::rename(&part, &dest).await?;
        Ok(())
    }

    /// Starts the download if needed and waits for it.
    async fn ensure_ready(self: &Arc<Self>) -> Result<()> {
        if let Some(why) = &self.unavailable {
            bail!("{why}");
        }
        // Subscribe before starting, so a download that finishes in between still wakes us.
        let mut rx = self.changed.subscribe();
        self.prepare().await;
        loop {
            if self.ready() {
                return Ok(());
            }
            match self.download.locked().clone() {
                Download::Running => {}
                Download::Failed(e) => bail!("The voice model download failed: {e}"),
                Download::Idle => bail!("The voice model isn't ready."),
            }
            rx.changed().await.map_err(|_| anyhow!("the voice model download was cancelled"))?;
        }
    }

    /// `voiceLocalSpeak`.
    pub async fn speak(self: &Arc<Self>, text: &str, voice: Option<&str>) -> Result<Value> {
        if text.trim().is_empty() || text.len() > 4000 {
            bail!("text must be 1–4000 bytes");
        }
        if let Some(v) = voice.filter(|v| !v.is_empty())
            && !voices_of(&self.files).iter().any(|known| known == v)
        {
            bail!("unknown voice `{v}`");
        }
        self.ensure_ready().await?;
        let helper = self.helper.as_ref().context("no voice helper")?;
        let params = json!({"text": text, "voice": voice.filter(|v| !v.is_empty())});
        let v = helper.call("speak", &params, SPEAK_TIMEOUT).await?;
        let audio = v["audio"].as_str().context("the voice helper sent no audio")?;
        Ok(json!({"audio": audio, "format": "wav", "sampleRate": v["sampleRate"].as_u64().unwrap_or(SAMPLE_RATE)}))
    }
}

// ---- where things are --------------------------------------------------------------------------------

/// `kith-tts` next to this executable, in the sibling cargo profile folders (a dev build), then on `PATH`.
fn find_helper() -> Option<PathBuf> {
    let name = if cfg!(windows) { "kith-tts.exe" } else { "kith-tts" };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let near = [
        dir.join(name),
        dir.join("../release").join(name),
        dir.join("../debug").join(name),
        dir.join("../dev-app").join(name),
    ];
    near.into_iter()
        .find(|p| p.is_file())
        .or_else(|| std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file()))
}

fn helper_cmd(dir: &Path) -> Result<HelperCmd, String> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("Local voice needs a Mac with Apple silicon.".to_owned());
    }
    let program =
        find_helper().ok_or_else(|| "Local voice isn't installed: kith-tts is missing next to the host.".to_owned())?;
    Ok(HelperCmd { program, args: vec![dir.to_string_lossy().into_owned()] })
}

fn local() -> &'static Arc<Local> {
    static LOCAL: LazyLock<Arc<Local>> = LazyLock::new(|| {
        let dir = crate::service::data_dir().join("models").join(MODEL);
        let helper = helper_cmd(&dir);
        Local::new(dir, files(), helper, IDLE)
    });
    &LOCAL
}

pub async fn status() -> Value {
    local().status().await
}

pub async fn prepare() -> Value {
    local().prepare().await
}

pub async fn speak(text: &str, voice: Option<&str>) -> Result<Value> {
    local().speak(text, voice).await
}

// Fake `kith` shell scripts and chmod: Unix-only until windows-owner W2b.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::os::unix::fs::PermissionsExt as _;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-voice-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn the_file_list_pins_revisions_and_checksums() {
        let all = files();
        assert!(all.iter().all(|s| s.sha256.len() == 64 && s.url.starts_with("https://huggingface.co/")));
        assert!(all.iter().all(|s| !s.url.contains("/main/")), "pin a revision, not main");
        assert_eq!(voices_of(&all), ["default", "zoey", "harper", "easton", "miles"]);
    }

    /// A stand-in for Hugging Face; counts the requests it gets.
    async fn serve(body: &'static [u8]) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let app = Router::new().route(
            "/f",
            get(move || {
                let h = h.clone();
                async move {
                    h.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    body
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/f", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, hits)
    }

    fn fake_helper(dir: &Path, once: bool) -> HelperCmd {
        // Answers each line with a tiny "audio"; the id is whatever follows `"id":`.
        let answer = r#"id=${line#*\"id\":}; id=${id%%,*}; printf '{"id":%s,"result":{"audio":"QQ==","sampleRate":24000,"rssBytes":42}}\n' "$id""#;
        let script = if once { format!("read line; {answer}") } else { format!("while read line; do {answer}; done") };
        let path = dir.join(if once { "once.sh" } else { "loop.sh" });
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        HelperCmd { program: path, args: vec![] }
    }

    fn one_file(url: String, body: &[u8], name: &str) -> Vec<Spec> {
        vec![Spec { name: name.to_owned(), url, sha256: sha(body), size: body.len() as u64 }]
    }

    async fn wait_for(local: &Arc<Local>, state: &str) -> Value {
        for _ in 0..100 {
            let s = local.status().await;
            if s["state"] == state {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never reached {state}: {}", local.status().await);
    }

    #[tokio::test]
    async fn status_walks_missing_downloading_ready_and_downloads_once() {
        let dir = scratch("states");
        let (url, hits) = serve(b"model bytes").await;
        let local = Local::new(
            dir.join("m"),
            one_file(url, b"model bytes", "voices/a.safetensors"),
            Ok(fake_helper(&dir, false)),
            IDLE,
        );
        let s = local.status().await;
        assert_eq!((s["state"].as_str(), s["available"].as_bool()), (Some("missing"), Some(true)));
        assert_eq!(s["model"], MODEL);
        assert_eq!(s["defaultVoice"], DEFAULT_VOICE);

        let (a, b) = tokio::join!(local.prepare(), local.prepare());
        assert_eq!(a["state"], "downloading");
        assert_eq!(b["state"], "downloading");
        assert!(a["progress"].is_number());
        wait_for(&local, "ready").await;
        assert_eq!(hits.load(Ordering::Relaxed), 1, "a second prepare must not start a second download");
        assert_eq!(local.runs.load(Ordering::Relaxed), 1);
        assert!(dir.join("m/voices/a.safetensors").is_file());
        assert!(!dir.join("m/voices/a.safetensors.part").exists());
    }

    #[tokio::test]
    async fn a_wrong_checksum_is_an_error_and_leaves_nothing_behind_then_retries() {
        let dir = scratch("badsum");
        let (url, hits) = serve(b"tampered").await;
        let mut files = one_file(url, b"genuine!", "x.bin");
        files[0].size = 8;
        let local = Local::new(dir.join("m"), files, Ok(fake_helper(&dir, false)), IDLE);
        local.prepare().await;
        let s = wait_for(&local, "error").await;
        assert!(s["error"].as_str().unwrap().contains("checksum"), "{s}");
        assert!(!dir.join("m/x.bin").exists() && !dir.join("m/x.bin.part").exists());
        local.prepare().await; // error: retry on the next prepare
        wait_for(&local, "error").await;
        assert_eq!(hits.load(Ordering::Relaxed), 2);
        let err = local.speak("hi", None).await.unwrap_err().to_string();
        assert!(err.contains("checksum"), "{err}");
    }

    #[tokio::test]
    async fn a_partial_file_from_a_killed_host_is_replaced() {
        let dir = scratch("partial");
        std::fs::create_dir_all(dir.join("m")).unwrap();
        std::fs::write(dir.join("m/x.bin.part"), b"half").unwrap();
        let (url, _) = serve(b"all of it").await;
        let local = Local::new(dir.join("m"), one_file(url, b"all of it", "x.bin"), Ok(fake_helper(&dir, false)), IDLE);
        assert_eq!(local.status().await["state"], "missing");
        local.prepare().await;
        wait_for(&local, "ready").await;
        assert_eq!(std::fs::read(dir.join("m/x.bin")).unwrap(), b"all of it");
    }

    #[tokio::test]
    async fn unavailable_says_why_and_does_not_download() {
        let dir = scratch("unavailable");
        let local = Local::new(
            dir.join("m"),
            one_file("http://127.0.0.1:9/f".into(), b"x", "x.bin"),
            Err("no helper".into()),
            IDLE,
        );
        let s = local.prepare().await;
        assert_eq!(
            (s["available"].as_bool(), s["error"].as_str(), s["state"].as_str()),
            (Some(false), Some("no helper"), Some("missing"))
        );
        assert!(local.speak("hi", None).await.unwrap_err().to_string().contains("no helper"));
    }

    #[tokio::test]
    async fn speak_has_the_voice_speak_shape_and_checks_its_input() {
        let dir = scratch("speak");
        let (url, _) = serve(b"m").await;
        let mut files = one_file(url, b"m", "voices/zoey.safetensors");
        let extra = Spec { name: "config.json".into(), ..files[0].clone() };
        files.push(extra);
        let local = Local::new(dir.join("m"), files, Ok(fake_helper(&dir, false)), IDLE);
        for bad in ["", "   ", &"a".repeat(4001)] {
            assert!(local.speak(bad, None).await.unwrap_err().to_string().contains("1–4000"));
        }
        assert!(local.speak("hi", Some("nobody")).await.unwrap_err().to_string().contains("unknown voice"));
        // Missing model: speak downloads first, then answers.
        let v = local.speak("Hello", Some("zoey")).await.unwrap();
        assert_eq!(
            (v["audio"].as_str(), v["format"].as_str(), v["sampleRate"].as_u64()),
            (Some("QQ=="), Some("wav"), Some(24_000))
        );
        let s = local.status().await;
        assert_eq!(s["state"], "ready");
        assert_eq!(s["voices"], json!(["default", "zoey"]));
        assert_eq!(s["helperRssBytes"], 42, "the running helper reports its memory");
    }

    #[tokio::test]
    async fn two_speaks_at_once_both_succeed() {
        let dir = scratch("concurrent");
        let h = Helper::new(fake_helper(&dir, false), IDLE);
        let p = json!({"text": "x"});
        let (first, second, third) = tokio::join!(
            h.call("speak", &p, Duration::from_secs(5)),
            h.call("speak", &p, Duration::from_secs(5)),
            h.call("speak", &p, Duration::from_secs(5))
        );
        assert!(first.is_ok() && second.is_ok() && third.is_ok());
        assert_eq!(h.spawns.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn the_helper_stops_when_idle_and_starts_again_on_demand() {
        let dir = scratch("idle");
        let h = Helper::new(fake_helper(&dir, false), Duration::from_millis(300));
        h.call("speak", &json!({"text": "x"}), Duration::from_secs(5)).await.unwrap();
        assert!(h.running().await);
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(!h.running().await, "stopped after the idle time");
        h.call("speak", &json!({"text": "x"}), Duration::from_secs(5)).await.unwrap();
        assert_eq!(h.spawns.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn activity_keeps_the_helper_alive() {
        let dir = scratch("busy");
        let h = Helper::new(fake_helper(&dir, false), Duration::from_millis(400));
        for _ in 0..4 {
            h.call("speak", &json!({"text": "x"}), Duration::from_secs(5)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(h.spawns.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn a_dead_helper_is_restarted_once_and_the_request_still_succeeds() {
        let dir = scratch("crash");
        // Answers one line and exits, so every second request finds a dead process.
        let h = Helper::new(fake_helper(&dir, true), IDLE);
        for _ in 0..3 {
            h.call("speak", &json!({"text": "x"}), Duration::from_secs(5)).await.unwrap();
        }
        assert!(h.spawns.load(Ordering::Relaxed) >= 3);
    }

    #[tokio::test]
    async fn a_helper_that_cannot_start_fails_the_request_only() {
        let h = Helper::new(HelperCmd { program: "/nonexistent/kith-tts".into(), args: vec![] }, IDLE);
        assert!(h.call("speak", &json!({}), Duration::from_secs(1)).await.is_err());
        assert!(!h.running().await);
    }
}
