//! The real [`WarmStub`] (design 1.1, 1.3): a warm engine under `kith-stub --resident`, spawned with
//! [`kith_acp::spawn_resident`], whose writer lock the host drives through the stub's control channel ([`kith_acp::Ctl`]).
//! The stub's protocol is `core/stub/src/resident.rs`: `take <key> <claim>` and `release <key> <claim>`, one answer each.
//!
//! The ACP wire is the engine's, over the stub's stdio, so [`kith_acp::Acp::pid`] is the stub's pid and the stub leads the
//! process group: [`ResidentStub::kill_group`] signals the group, engine and children with it, and the lock goes with the
//! stub. The stub outlives the host (it keeps a held lock until its engine exits), which is the point of holding the lock
//! there rather than in the host.
//!
//! One `ResidentStub` is one warm process and has no opinion on sessions: the actor loads its main session and its threads
//! through [`ResidentStub::acp`] as it does today, and registers each with the core (`CoreWriter::register_proc`). A restart
//! (a new account, a stub that failed) makes a new process with a new pid; the caller then loads its sessions again.
//!
//! Unix only. On Windows the stub takes a named pipe the host does not create yet, so this module is not built there and
//! Writer mode is refused there (S2). Blocking calls (`take`, `release`, `restart`) are for a blocking thread, as
//! `core_writer` says; `restart` and `spawn` need a tokio runtime (the child is a tokio process).
//!
//! Nothing calls this until the wiring package (P7b).

#![allow(dead_code, reason = "machinery with no caller yet")]

use super::core_writer::WarmStub;
use super::registry::pid_runs;
use kith_acp::{Acp, Ctl, Incoming, Release, Take, spawn_resident};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

/// How long a restart waits for the old process group to leave before it starts the next one.
const OLD_GONE_WITHIN: Duration = Duration::from_secs(3);

/// What to run for a warm process: the engine's shell line (`kith-stub` execs it), its folder and its environment.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub command: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
}

/// Makes the [`Launch`] for an account (`None`: the one the process had, or the default). The wiring package builds it from the
/// agent's config, so a restart on another account gets that account's `CLAUDE_CONFIG_DIR`.
pub type Launcher = Box<dyn Fn(Option<&str>) -> Launch + Send + Sync>;

/// A warm process with a resident stub.
pub struct ResidentStub {
    stub: PathBuf,
    lock_dir: PathBuf,
    launcher: Launcher,
    account: Option<String>,
    acp: Arc<Acp>,
    incoming: Option<UnboundedReceiver<Incoming>>,
    ctl: Ctl,
    handle: tokio::runtime::Handle,
}

impl ResidentStub {
    /// Starts the stub and the engine under it, as `account` (`None`: the launcher's default). Needs a tokio runtime.
    pub fn spawn(stub: &Path, lock_dir: &Path, launcher: Launcher, account: Option<String>) -> anyhow::Result<Self> {
        let handle = tokio::runtime::Handle::try_current()?;
        std::fs::create_dir_all(lock_dir)?;
        let (acp, incoming, ctl) = start(stub, lock_dir, &launcher, account.as_deref())?;
        Ok(Self {
            stub: stub.to_path_buf(),
            lock_dir: lock_dir.to_path_buf(),
            launcher,
            account,
            acp,
            incoming: Some(incoming),
            ctl,
            handle,
        })
    }

    /// The engine's ACP connection. A new one after every [`WarmStub::restart`]: fetch it again, never keep the old.
    pub fn acp(&self) -> Arc<Acp> {
        self.acp.clone()
    }

    /// The engine's incoming requests and notifications. Once per process: `None` after it was taken (a restart makes a new
    /// channel).
    pub fn take_incoming(&mut self) -> Option<UnboundedReceiver<Incoming>> {
        self.incoming.take()
    }

    /// Whether the control channel still answers (it breaks on the first call that got no answer).
    pub fn ctl_open(&self) -> bool {
        self.ctl.is_open()
    }

    fn stub_pid(&self) -> u32 {
        self.acp.pid().unwrap_or(0)
    }

    /// Signals the stub's group and has the runtime reap the leader (a zombie still "runs" to `kill(pid, 0)`).
    fn signal_group(&self) {
        let pid = self.stub_pid();
        if let Ok(group) = libc::pid_t::try_from(pid)
            && group > 1
        {
            // SAFETY: a signal to the process group this struct started (the stub is its leader).
            unsafe { libc::killpg(group, libc::SIGKILL) };
        }
        let acp = self.acp.clone();
        self.handle.spawn(async move { acp.kill().await });
    }
}

fn start(
    stub: &Path,
    lock_dir: &Path,
    launcher: &Launcher,
    account: Option<&str>,
) -> anyhow::Result<(Arc<Acp>, UnboundedReceiver<Incoming>, Ctl)> {
    let l = launcher(account);
    spawn_resident(stub, lock_dir, &l.command, &l.cwd, &l.env)
}

impl WarmStub for ResidentStub {
    fn pid(&self) -> u32 {
        self.stub_pid()
    }

    fn account(&self) -> Option<String> {
        self.account.clone()
    }

    fn take(&self, key: &str, claim: i64) -> std::io::Result<Take> {
        self.ctl.take(key, claim)
    }

    fn release(&self, key: &str, claim: i64) -> std::io::Result<Release> {
        self.ctl.release(key, claim)
    }

    fn restart(&mut self, account: Option<&str>) -> std::io::Result<()> {
        // The old group goes first: its stub may hold a lock, and two stubs on one conversation would answer `busy` to us.
        let old = self.stub_pid();
        self.signal_group();
        let until = Instant::now() + OLD_GONE_WITHIN;
        while pid_runs(old) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let account = account.map(str::to_owned).or_else(|| self.account.clone());
        // A runtime context for the spawn: this runs on a blocking thread, where the handle's runtime is current already,
        // and from a plain thread, where it is not.
        let _in_runtime = self.handle.enter();
        let (acp, incoming, ctl) = start(&self.stub, &self.lock_dir, &self.launcher, account.as_deref())
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        self.acp = acp;
        self.incoming = Some(incoming);
        self.ctl = ctl;
        self.account = account;
        Ok(())
    }

    fn kill_group(&self) {
        self.signal_group();
    }
}
