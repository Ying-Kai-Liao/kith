//! Engine sessions held by the core (runtime-design section 4, "Transcripts"): where an engine keeps a session, a hash
//! over its files, the place-or-adopt rule, and the reading `kith-sessions` and `kith-wake` do over transcripts.
//!
//! M3-6 builds and tests this; no start calls it yet (the switch is later) and nothing here writes a transcript in
//! production. Python (`kith-sessions`) stays the writer of `live/terminal/<sid>.json`.
//!
//! - [`locate`]: the session per engine and account. Claude: `<config dir>/projects/<cwd-slug>/<sid>.jsonl` plus the
//!   sidecar folder `<sid>/`. Any other engine is [`Located::NotHeld`], never an error.
//! - [`hash_session`]: SHA-256 over the session's files in a documented stable order ([`hash`]).
//! - [`decide`] / [`step`]: place the core's copy only when the engine's is missing or unchanged since the last read,
//!   else adopt the engine's ([`place`]).
//! - [`TranscriptReader`]: whether a conversation's transcript holds `[kith wake RUN]` ([`read`]), plus the ports of
//!   `recent`, `pick`, `latest` and the reading half of `resolve`.
//! - [`rows`]: the `engine_sessions` table.

pub(crate) mod hash;
pub(crate) mod place;
pub(crate) mod read;
pub(crate) mod rows;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use kith_kernel::accounts::{self, Outside};
use kith_kernel::threads::{self, Ctx, safe_id};

pub(crate) use hash::hash_session;

/// The account id of the engine's default login (what a registry record without `account` means).
pub(crate) const DEFAULT_ACCOUNT: &str = "claude";

/// Where a session's files are: the engine's config dir, the project folder under `projects/`, and the session id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionPtr {
    pub config_dir: PathBuf,
    pub project: String,
    pub sid: String,
}

impl SessionPtr {
    /// The session's main file.
    pub fn jsonl(&self) -> PathBuf {
        self.config_dir
            .join("projects")
            .join(&self.project)
            .join(format!("{}.jsonl", self.sid))
    }

    /// The folder beside it (subagent logs, tool results); often missing.
    pub fn sidecar(&self) -> PathBuf {
        self.config_dir
            .join("projects")
            .join(&self.project)
            .join(&self.sid)
    }

    /// The pointer as the table stores it: the main file's path.
    pub fn text(&self) -> String {
        self.jsonl().to_string_lossy().into_owned()
    }

    /// Reads a pointer back (`<config dir>/projects/<project>/<sid>.jsonl`); `None` for any other shape.
    pub fn from_text(text: &str) -> Option<Self> {
        let p = Path::new(text);
        let sid = p.file_stem()?.to_str()?.to_owned();
        if p.extension()? != "jsonl" || !safe_id(&sid) {
            return None;
        }
        let project = p.parent()?;
        let projects = project.parent()?;
        if projects.file_name()? != "projects" {
            return None;
        }
        Some(Self {
            config_dir: projects.parent()?.to_path_buf(),
            project: project.file_name()?.to_str()?.to_owned(),
            sid,
        })
    }
}

/// What a lookup found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Located {
    Found(SessionPtr),
    /// The engine is held, but there is no such session in the account's folder.
    Missing,
    /// The core holds no transcripts of this engine (only Claude's are).
    NotHeld,
}

struct Silent;

impl Outside for Silent {
    fn status(&self, _: &[&str], _: &[(&str, &str)]) -> (Option<i32>, String) {
        (None, String::new())
    }
    fn granted(&self, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn on_path(&self, _: &str) -> bool {
        false
    }
}

/// `(harness, name)` of an account id; `None` name is the harness's main login.
fn split_account(account: &str) -> (&str, Option<&str>) {
    match account.split_once(':') {
        Some((h, n)) => (h, Some(n)),
        None => (account, None),
    }
}

/// The engine an account id belongs to (`claude:work` -> `claude`).
pub(crate) fn engine_of(account: &str) -> &str {
    split_account(account).0
}

/// An account's Claude config dir. A named one is the accounts lane's `account_dir`. The main login's is kith-kernel's own
/// definition (`main_claude_paths`): `CLAUDE_CONFIG_DIR` when it is set and is not one of the accounts' own folders, else
/// `~/.claude`. So the import must run with the owner's normal environment, and a run's `CLAUDE_CONFIG_DIR` that points
/// into a named account is never taken for the main login. `None` for another engine, or a named account when the
/// machine folder is unknown.
pub(crate) fn config_dir(ctx: &Ctx<'_>, account: &str) -> Option<PathBuf> {
    let (harness, name) = split_account(account);
    if harness != "claude" {
        return None;
    }
    let machine = match (name, ctx.machine.clone()) {
        (_, Some(m)) => m,
        (None, None) => return Some(ctx.claude_dir()),
        (Some(_), None) => return None,
    };
    let actx = accounts::Ctx {
        machine,
        agents: PathBuf::new(),
        kernel: PathBuf::new(),
        home: ctx.home.clone(),
        claude_config_dir: (ctx.env)("CLAUDE_CONFIG_DIR"),
        out: &Silent,
    };
    Some(match name {
        Some(n) => accounts::account_dir(&actx, "claude", n),
        None => accounts::main_claude_paths(&actx).0,
    })
}

/// How Claude names a project folder: every character that is not an ASCII letter or digit becomes `-`
/// (`/Users/k/kith` -> `-Users-k-kith`, `C:\Users\k` -> `C--Users-k`).
pub(crate) fn slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The session `sid` of `account` in its config dir, wherever its project folder is (the glob of `kith-sessions`).
pub(crate) fn locate(ctx: &Ctx<'_>, account: &str, sid: &str) -> Located {
    if engine_of(account) != "claude" {
        return Located::NotHeld;
    }
    let Some(dir) = config_dir(ctx, account) else {
        return Located::Missing;
    };
    find_in(&dir, sid).map_or(Located::Missing, Located::Found)
}

/// The session in one config dir. Projects are looked at in name order, so two copies resolve the same way each time.
pub(crate) fn find_in(config_dir: &Path, sid: &str) -> Option<SessionPtr> {
    if !safe_id(sid) {
        return None;
    }
    let mut projects: Vec<PathBuf> = std::fs::read_dir(config_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    projects.sort();
    projects
        .into_iter()
        .find(|p| p.join(format!("{sid}.jsonl")).is_file())
        .and_then(|p| {
            Some(SessionPtr {
                config_dir: config_dir.to_path_buf(),
                project: p.file_name()?.to_str()?.to_owned(),
                sid: sid.to_owned(),
            })
        })
}

/// Every copy of the session under any Claude login on this Mac (this run's, the default one, each extra account's):
/// the `transcripts()` glob of `kith-sessions`, in the order of its roots.
pub(crate) fn find_everywhere(ctx: &Ctx<'_>, sid: &str) -> Vec<SessionPtr> {
    threads::registry::transcript_roots(ctx)
        .iter()
        .filter_map(|r| find_in(r, sid))
        .collect()
}
