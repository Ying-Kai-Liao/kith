//! Page pictures (phase2-feed section 8): a page post gets a 720x420 picture once per version. `put_post` queues
//! `(post id, version)`; one shooter thread takes them one at a time, runs `kith-serve shoot` on a copy of the page
//! under `taskpolicy -b`, stores the PNG in the chat's files root like any shared file, and sets it on the post only
//! if that version is still the one shown (`put_shot`). One try per version: a failure leaves the post without a
//! picture (the app shows a title card) and is logged. Mac only: there is no shooter elsewhere, so the title card stays.

use crate::chat::files::{self, SharedFile};
use crate::hub::Hub;
use crate::store::Post;
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, LazyLock, Mutex, Once, Weak};
use std::time::{Duration, Instant};

/// What `kith-serve shoot` says where there is no helper to run.
pub const NO_SHOOTER: &str = "no shooter on this platform";

/// The helper takes 10 s itself; this is the host's own limit on it, so a stuck one can't hold the queue.
const RUN_LIMIT: Duration = Duration::from_secs(25);

/// Most (post, version) pairs remembered as tried, so a long-lived host doesn't grow without bound.
const REMEMBERED: usize = 1024;

/// `kith-serve shoot <html> <out.png>`: runs the bundled helper. The exit code of the process.
pub fn cli(html: &Path, out: &Path) -> i32 {
    match run_helper(html, out, RUN_LIMIT, false) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

/// `kith-shoot` next to this executable (the Mac app's Resources folder), in the sibling cargo profile folders (a dev
/// build), then on `PATH`.
fn find_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("kith-shoot"), dir.join("../release/kith-shoot"), dir.join("../debug/kith-shoot")]
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("kith-shoot")).find(|p| p.is_file())
        })
}

/// Runs the helper and waits for it, killing it past `limit`. `background`: under `taskpolicy -b`.
fn run_helper(html: &Path, out: &Path, limit: Duration, background: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(NO_SHOOTER);
    }
    let helper = find_helper().ok_or_else(|| anyhow!("kith-shoot is missing next to the host"))?;
    let mut cmd = if background {
        let mut c = std::process::Command::new("taskpolicy");
        c.arg("-b").arg(&helper);
        c
    } else {
        std::process::Command::new(&helper)
    };
    let mut child = cmd
        .arg(html)
        .arg(out)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .context("starting the shooter")?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "the shooter failed ({status})");
            return Ok(());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the shooter took longer than {}s", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One page version to shoot. The hub is weak: a queued job must not keep a stopping host alive.
struct Job {
    hub: Weak<Hub>,
    post: String,
    version: String,
    chat: String,
    file: SharedFile,
}

/// Jobs waiting, and which (post, version) pairs were ever queued: one try per version.
#[derive(Default)]
struct Queue {
    waiting: VecDeque<Job>,
    tried: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
}

impl Queue {
    /// Queues the job unless that post and version was queued before. Whether it was.
    fn push(&mut self, job: Job) -> bool {
        let key = (job.post.clone(), job.version.clone());
        if !self.tried.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > REMEMBERED
            && let Some(old) = self.order.pop_front()
        {
            self.tried.remove(&old);
        }
        self.waiting.push_back(job);
        true
    }
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

static SHARED: LazyLock<Shared> = LazyLock::new(Shared::default);
static START: Once = Once::new();

/// Called by `put_post` for every post it wrote: a page whose picture is not yet taken is queued.
pub fn enqueue(hub: &Hub, post: &Post) {
    // Unit tests write page posts all the time and must not start a browser.
    if !cfg!(target_os = "macos") || cfg!(test) {
        return;
    }
    let Some(a) = post.attachment.as_ref().filter(|a| a["type"] == "page" && a["shot"].is_null()) else {
        return;
    };
    let (Some(version), Ok(file), Some(hub)) =
        (a["version"].as_str(), serde_json::from_value::<SharedFile>(a["file"].clone()), hub.shared())
    else {
        return;
    };
    let job = Job {
        hub: Arc::downgrade(&hub),
        post: post.id.clone(),
        version: version.to_owned(),
        chat: post.source_chat.clone(),
        file,
    };
    if !SHARED.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(job) {
        return;
    }
    START.call_once(|| {
        if let Err(error) = std::thread::Builder::new().name("feed-shooter".into()).spawn(worker) {
            tracing::warn!(%error, "feed: couldn't start the page shooter");
        }
    });
    SHARED.wake.notify_one();
}

fn worker() {
    loop {
        let job = {
            let mut q = SHARED.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(job) = q.waiting.pop_front() {
                    break job;
                }
                q = SHARED.wake.wait(q).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let Some(hub) = job.hub.upgrade() else { continue };
        let root = files::root(&job.chat);
        if let Err(error) = shoot(&hub, &root, &job) {
            tracing::warn!(error = format!("{error:#}"), post = %job.post, "feed: no picture for a page");
        }
    }
}

/// Copies the page out of the files root into a folder of its own (the helper may read only that folder, and a
/// snapshot has no extension), shoots it and sets the picture.
fn shoot(hub: &Hub, root: &Path, job: &Job) -> Result<()> {
    let work = std::env::temp_dir().join(format!("kith-shot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work)?;
    let result = (|| {
        let html = work.join("page.html");
        copy_page(root, &job.file, &html)?;
        let png = work.join("shot.png");
        run_helper(&html, &png, RUN_LIMIT, true)?;
        set_picture(hub, root, job, &png)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn copy_page(root: &Path, file: &SharedFile, to: &Path) -> Result<()> {
    use std::io::Write as _;
    let mut out = std::fs::File::create(to)?;
    let mut offset = 0;
    while offset < file.size {
        let chunk = files::read(root, file, offset)?;
        ensure!(!chunk.is_empty(), "the shared page is shorter than it says");
        out.write_all(&chunk)?;
        offset += chunk.len() as u64;
    }
    Ok(())
}

/// Stores the PNG as a file of the chat and sets it on the post, if the post still shows this version. A picture
/// of a version the page has moved on from is removed again.
fn set_picture(hub: &Hub, root: &Path, job: &Job, png: &Path) -> Result<()> {
    let stem = Path::new(&job.file.name).file_stem().and_then(|s| s.to_str()).unwrap_or("page");
    let name = files::name(&format!("{stem}.png")).unwrap_or_else(|_| "page.png".into());
    let never = AtomicBool::new(false);
    let mut snap =
        files::snapshot(root, png.parent().unwrap_or(png), &png.to_string_lossy(), Some(&name), None, &never)?;
    let meta = serde_json::to_value(&snap.meta)?;
    if crate::feed::put_shot(hub, &job.post, &job.version, Some(meta))?.is_some() {
        snap.commit();
    } else {
        tracing::debug!(post = %job.post, "feed: the page moved on before its picture was ready; dropped");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
