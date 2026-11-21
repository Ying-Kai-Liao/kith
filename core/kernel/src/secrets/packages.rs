//! Package facts: what the access rules need to know about an agent's package folder (where it is, whether it has an
//! `agent.md`, whether it is one of the kernel's own, the jobs and `guest_domains` its capabilities.yaml declares, the
//! service specs it brings, and the names of all agents). Every such read goes through [`Packages`], nothing else:
//!
//! - [`Packages::Disk`] reads the folders as `kith-auth` does, call by call. The CLI and the embedded runs use it.
//! - [`Packages::Source`] asks a [`PackageSource`] instead. The core daemon's source answers from memory that one thread
//!   fills outside the core lock, so a folder on a disk that does not answer (an external drive waiting for a prompt)
//!   cannot stop every call that waits for the lock. A fact the source cannot give is "unavailable", and the rules
//!   take the narrower answer for it: never a grant.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::Ctx;
use super::jobs::{self, Job};

/// Where package facts come from.
#[derive(Clone, Debug, Default)]
pub enum Packages {
    /// The disk, read when asked.
    #[default]
    Disk,
    /// A source that never touches the disk on the caller's thread.
    Source(Arc<dyn PackageSource>),
}

/// What a source answers from. `None` is "unavailable": the fact could not be had in time, which reads as the narrower
/// answer. A source records what it could not give so the caller can ask again.
pub trait PackageSource: Send + Sync + std::fmt::Debug {
    /// The facts of one agent, for the agents folder and kernel folder of the caller.
    fn facts(&self, agents: &Path, kernel: &Path, agent: &str) -> Option<Arc<AgentFacts>>;
    /// The names of the agents that have a package.
    fn names(&self, agents: &Path, kernel: &Path) -> Option<Arc<Vec<String>>>;
    /// What kith keeps for a person outside the tables, as (label, path): the folders `memory/people/<pid>` and
    /// `files/people/<pid>` of every agent, and `<state>/people/<pid>.md`. A fact the call needs: a source that cannot give
    /// it says so (and the caller asks again), `None` is "unavailable".
    fn life(
        &self,
        _agents: &Path,
        _kernel: &Path,
        _state: &Path,
        _pid: &str,
    ) -> Option<Arc<Vec<(String, PathBuf)>>> {
        None
    }
    /// [`Self::life`] for a call that must never wait on it (a removal that only words a line with it): an answer the
    /// source has, else `None`, and nothing is recorded as missing.
    fn life_soft(
        &self,
        _agents: &Path,
        _kernel: &Path,
        _state: &Path,
        _pid: &str,
    ) -> Option<Arc<Vec<(String, PathBuf)>>> {
        None
    }
    /// `<kernel>/services/<svc>.json`.
    fn kernel_service(&self, _agents: &Path, _kernel: &Path, _svc: &str) -> Option<Arc<Found>> {
        None
    }
}

/// A service spec file of a package: not there, its bytes, or there but not readable.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    Absent,
    Bytes(Vec<u8>),
    Unreadable,
}

/// Everything the rules ask of one agent's package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFacts {
    /// `package_dir`.
    pub dir: PathBuf,
    /// `agent_exists`.
    pub exists: bool,
    /// `is_builtin`.
    pub builtin: bool,
    /// The jobs capabilities.yaml declares.
    pub jobs: Vec<Job>,
    /// The `guest_domains:` capabilities.yaml declares, before the rules drop the invalid ones.
    pub guest_domains: Vec<String>,
    /// `<dir>/services/*.json`: file name to its bytes, `None` when it is there and cannot be read.
    pub services: BTreeMap<String, Option<Vec<u8>>>,
}

/// Service specs are small; a bigger file is read as unreadable rather than held in memory.
const SERVICE_MAX: u64 = 1 << 20;

impl AgentFacts {
    /// Reads them all from the disk: the same code `Packages::Disk` runs one fact at a time.
    pub fn read(agents: &Path, kernel: &Path, agent: &str) -> AgentFacts {
        let dir = package_dir(agents, kernel, agent);
        let caps = dir.join("capabilities.yaml");
        let mut services = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(dir.join("services")) {
            for e in rd.filter_map(Result::ok) {
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                let path = e.path();
                // exactly as `service_file` asks for them: `<name>.json`, in that case
                #[allow(clippy::case_sensitive_file_extension_comparisons)]
                let is_spec = name.ends_with(".json");
                if !is_spec || !path.is_file() {
                    continue;
                }
                let bytes = std::fs::metadata(&path)
                    .ok()
                    .filter(|m| m.len() <= SERVICE_MAX)
                    .and_then(|_| std::fs::read(&path).ok());
                services.insert(name, bytes);
            }
        }
        AgentFacts {
            exists: agent_exists(agents, kernel, agent),
            builtin: is_builtin(agents, kernel, agent),
            jobs: jobs::read(&caps),
            guest_domains: jobs::guest_domains(&caps),
            services,
            dir,
        }
    }

    /// The files and folders these facts came from, to stat: when none of them changed, the facts did not. The
    /// candidates that may not exist yet are in the list too, so a file appearing is a change.
    pub fn watched(&self, agents: &Path, kernel: &Path, agent: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !agent.is_empty() && !agent.contains('/') && !agent.starts_with('.') {
            for root in [agents.to_path_buf(), kernel.join("agents")] {
                out.push(root.join(agent).join("role/agent.md"));
                out.push(root.join(agent).join("agent.md"));
            }
        }
        let caps = self.dir.join("capabilities.yaml");
        out.push(caps.clone());
        out.extend(jobs::door_lockfiles(&caps));
        out.push(self.dir.join("services"));
        out.extend(
            self.services
                .keys()
                .map(|n| self.dir.join("services").join(n)),
        );
        out
    }
}

/// Run on every read of the disk made on behalf of the access rules, when a test set it: a test that the daemon never
/// reads a package folder under its core lock puts its check here.
static DISK_READ: OnceLock<fn()> = OnceLock::new();

/// Sets what [`Packages`] calls before it reads the disk. Once per process; a second call changes nothing.
pub fn on_disk_read(f: fn()) {
    let _ = DISK_READ.set(f);
}

fn touch() {
    if let Some(f) = DISK_READ.get() {
        f();
    }
}

/// A read of a folder the rules do not own (a person's folders, the kernel's service specs) outside [`Packages`]: the
/// same check as the package reads get.
pub fn note_disk_read() {
    touch();
}

fn plain(agent: &str) -> bool {
    !agent.is_empty() && !agent.contains('/') && !agent.starts_with('.')
}

/// `agent_dirs()` in `kith-auth`: see [`Ctx::package_dir`].
pub(super) fn package_dir(agents: &Path, kernel: &Path, agent: &str) -> PathBuf {
    touch();
    if plain(agent) {
        for root in [agents.to_path_buf(), kernel.join("agents")] {
            let d = root.join(agent);
            let pkg = if d.join("role/agent.md").is_file() {
                d.join("role")
            } else {
                d
            };
            if pkg.join("agent.md").is_file() {
                return pkg;
            }
        }
    }
    agents.join(agent)
}

pub(super) fn agent_exists(agents: &Path, kernel: &Path, agent: &str) -> bool {
    plain(agent)
        && package_dir(agents, kernel, agent)
            .join("agent.md")
            .is_file()
}

pub(super) fn is_builtin(agents: &Path, kernel: &Path, agent: &str) -> bool {
    touch();
    let has = |root: &Path| {
        let d = root.join(agent);
        plain(agent) && (d.join("role/agent.md").is_file() || d.join("agent.md").is_file())
    };
    let kernel_agents = kernel.join("agents");
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    !same(agents, &kernel_agents) && !has(agents) && has(&kernel_agents)
}

/// The names of the agents that have a package, in the user's agents and the kernel's.
pub fn read_names(agents: &Path, kernel: &Path) -> Vec<String> {
    touch();
    let mut out: Vec<String> = Vec::new();
    for root in [agents.to_path_buf(), kernel.join("agents")] {
        let Ok(dir) = std::fs::read_dir(root) else {
            continue;
        };
        let mut names: Vec<String> = dir
            .filter_map(Result::ok)
            .filter_map(|d| d.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && agent_exists(agents, kernel, n))
            .collect();
        names.sort();
        for n in names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

impl Ctx {
    fn facts(&self, agent: &str) -> Option<Arc<AgentFacts>> {
        match &self.packages {
            Packages::Disk => None,
            Packages::Source(s) => s.facts(&self.agents, &self.kernel, agent),
        }
    }

    /// Where an agent's package is (`agent_dirs()` in `kith-auth`): `<agent>/role` when that holds an `agent.md`, else the
    /// agent's own folder, in the user's agents then the kernel's; else where it would be.
    pub fn package_dir(&self, agent: &str) -> PathBuf {
        match &self.packages {
            Packages::Disk => package_dir(&self.agents, &self.kernel, agent),
            Packages::Source(_) => self
                .facts(agent)
                .map_or_else(|| self.agents.join(agent), |f| f.dir.clone()),
        }
    }

    /// Whether `agent` is in `kith-auth`'s `agent_dirs()`: a package with an `agent.md`, in the user's agents or the
    /// kernel's. The owner's writes (M2-2) refuse an agent that is not.
    pub fn agent_exists(&self, agent: &str) -> bool {
        match &self.packages {
            Packages::Disk => agent_exists(&self.agents, &self.kernel, agent),
            Packages::Source(_) => self.facts(agent).is_some_and(|f| f.exists),
        }
    }

    /// `agent_dirs()` in `kith-auth`: the names of the agents that have a package, in the user's agents and the kernel's.
    pub fn agent_names(&self) -> Vec<String> {
        match &self.packages {
            Packages::Disk => read_names(&self.agents, &self.kernel),
            Packages::Source(s) => s
                .names(&self.agents, &self.kernel)
                .map(|n| n.as_ref().clone())
                .unwrap_or_default(),
        }
    }

    /// `builtin()` in `kith-auth`: one of the kernel's own agents (scratch ...): found under `<kernel>/agents` and not in
    /// the user's agents folder. Its life folder is one for everyone who runs it, so only the owner does. An agent whose
    /// package can't be read counts as one: only the owner (who is answered before this) gets through.
    pub fn is_builtin(&self, agent: &str) -> bool {
        match &self.packages {
            Packages::Disk => is_builtin(&self.agents, &self.kernel, agent),
            Packages::Source(_) => self.facts(agent).is_none_or(|f| f.builtin),
        }
    }

    /// Why a call can't go on for `agent` because its package could not be read: `Some` only for a source that had no
    /// answer in time. The words a refusal gives instead of "no such agent" or "built-in".
    pub fn unreadable(&self, agent: &str) -> Option<String> {
        match &self.packages {
            Packages::Disk => None,
            Packages::Source(_) => self.facts(agent).is_none().then(|| {
                format!("can't read {agent}'s package: the disk did not answer, try again")
            }),
        }
    }

    /// The jobs the agent's package declares (capabilities.yaml `jobs:`); none when it can't be read.
    pub(super) fn declared_jobs(&self, agent: &str) -> Vec<Job> {
        match &self.packages {
            Packages::Disk => jobs::read(&self.package_dir(agent).join("capabilities.yaml")),
            Packages::Source(_) => self
                .facts(agent)
                .map(|f| f.jobs.clone())
                .unwrap_or_default(),
        }
    }

    /// The `guest_domains:` of the agent's package; none when it can't be read.
    pub(super) fn declared_domains(&self, agent: &str) -> Vec<String> {
        match &self.packages {
            Packages::Disk => {
                jobs::guest_domains(&self.package_dir(agent).join("capabilities.yaml"))
            }
            Packages::Source(_) => self
                .facts(agent)
                .map(|f| f.guest_domains.clone())
                .unwrap_or_default(),
        }
    }

    /// `<package>/services/<file>` of the agent.
    pub fn service_file(&self, agent: &str, file: &str) -> Found {
        match &self.packages {
            Packages::Disk => {
                let path = self.package_dir(agent).join("services").join(file);
                if !path.is_file() {
                    return Found::Absent;
                }
                std::fs::read(&path).map_or(Found::Unreadable, Found::Bytes)
            }
            // No facts at all (the cache could not give them) is not a file that is not there.
            Packages::Source(_) => match self.facts(agent) {
                None => Found::Unreadable,
                Some(f) => match f.services.get(file).cloned() {
                    None => Found::Absent,
                    Some(None) => Found::Unreadable,
                    Some(Some(b)) => Found::Bytes(b),
                },
            },
        }
    }
}
