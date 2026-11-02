//! Write checks and version history for the hq agents' memory files.
//!
//! The Markdown files stay the truth (agents/<name>/memory/** and the shared user layer in the state folder:
//! user.md, people/, projects/). This module puts kith-mem's checks in front of an agent's or tidy-up's write and
//! records every change as a version in one SQLite file, `<state>/memory.db`. Deleting that file loses history only.
//!
//! The functions here are what the `kith memory` commands and the Claude Code hook call; the host's tidying
//! calls them too, so a tidy-up's write passes the same checks and lands in the same history.

mod browse;
mod cli;
mod hook;
pub mod tidy;

pub use browse::{diff, history, list_files, read_file, restore, save};
pub use cli::{Cmd, run};

use anyhow::{Context, Result};
use kith_mem::check::{self, Refusal};
use kith_mem::files::{self, Author, Cause, Change, Version};
use kith_mem::rusqlite::{Connection, Transaction, TransactionBehavior};
use kith_mem::{schema, store, tokens};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// MEMORY.md is a map, not a copy of the code: past this many lines the agent is told to trim it.
const INDEX_LINES: usize = 150;
/// The shared user.md is read by every agent in every run.
const USER_TOKENS: usize = 400;
const BUSY: Duration = Duration::from_secs(5);
/// A hook has 10 seconds in all: a locked history may cost it this much, not more.
pub const HOOK_BUSY: Duration = Duration::from_secs(2);

/// Where the memory is on this Mac.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The state folder (`KITH_STATE`): user.md, people/, projects/ and memory.db.
    pub state: PathBuf,
    /// The folders holding one folder per agent: the Kith folder's agents/, then the older <state>/agents.
    pub agents: Vec<PathBuf>,
}

impl Layout {
    /// From the environment, the way `lib/kith_paths.py` reads it; the commands the kernel starts pass the answer
    /// explicitly, so this is the fallback for a bare `kith memory log` in a terminal.
    pub fn from_env() -> Self {
        Self::for_home(&kith_kernel::paths::process_home())
    }

    /// `from_env` for a given home folder: the folders `kith paths` says, `KITH_AGENTS` aside.
    pub(crate) fn for_home(home: &Path) -> Self {
        let p = crate::kernel::folders(home);
        let agents = crate::kernel::var("AGENTS").map_or(p.agents, PathBuf::from);
        Self::new(p.state, agents)
    }

    pub fn new(state: PathBuf, agents: PathBuf) -> Self {
        let old = state.join("agents");
        Self { agents: vec![agents, old], state }
    }

    pub fn db(&self) -> PathBuf {
        self.state.join("memory.db")
    }

    /// The memory file a path is, if it is one. `path` may be relative to `cwd`, through a link, or not exist yet.
    pub fn locate(&self, path: &Path, cwd: &Path) -> Option<Target> {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let path = path.strip_prefix("~").map_or_else(|_| path.to_path_buf(), |rest| home.join(rest));
        let absolute = lexical(&cwd.join(path));
        let resolved = resolve(&absolute);
        for candidate in [&absolute, &resolved] {
            for root in &self.agents {
                for root in [root.clone(), resolve(root)] {
                    if let Some(target) = agent_file(&root, candidate, &absolute) {
                        return Some(target);
                    }
                }
            }
            for root in [self.state.clone(), resolve(&self.state)] {
                if let Some(target) = shared_file(&root, candidate, &absolute) {
                    return Some(target);
                }
            }
        }
        None
    }
}

/// A memory file: the layer it belongs to and its path inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The agent whose memory/ holds it; `None` for the shared user layer.
    pub bot: Option<String>,
    /// Relative to the layer's folder, with `/`.
    pub name: String,
    /// Where it is on disk, as the caller spelled it.
    pub path: PathBuf,
}

impl Target {
    /// Names the layer for a reader: `builder/MEMORY.md`, `user.md`.
    pub fn label(&self) -> String {
        match &self.bot {
            Some(bot) => format!("{bot}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

fn agent_file(root: &Path, candidate: &Path, logical: &Path) -> Option<Target> {
    let mut parts = candidate.strip_prefix(root).ok()?.components();
    let name = parts.next()?.as_os_str().to_str()?.to_owned();
    if name.starts_with('.') || parts.next()?.as_os_str() != "memory" {
        return None;
    }
    let rel = rel_name(parts)?;
    if !is_markdown(&rel) {
        return None;
    }
    Some(Target { bot: Some(name), name: rel, path: logical.to_path_buf() })
}

fn shared_file(root: &Path, candidate: &Path, logical: &Path) -> Option<Target> {
    let rest = candidate.strip_prefix(root).ok()?;
    let mut parts = rest.components();
    let first = parts.next()?.as_os_str().to_str()?.to_owned();
    match first.as_str() {
        "user.md" if parts.next().is_none() => Some(Target { bot: None, name: first, path: logical.to_path_buf() }),
        "people" | "projects" => {
            let rel = rel_name(rest.components())?;
            if !is_markdown(&rel) {
                return None;
            }
            Some(Target { bot: None, name: rel, path: logical.to_path_buf() })
        }
        _ => None,
    }
}

fn is_markdown(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// `a/b.md` from the components, or `None` for none or any that isn't a plain name.
fn rel_name<'a>(parts: impl Iterator<Item = Component<'a>>) -> Option<String> {
    let names: Option<Vec<&str>> = parts
        .map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    let names = names?;
    (!names.is_empty()).then(|| names.join("/"))
}

/// Joins `.` and `..` away without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The path with links resolved as far as it exists; the rest is appended as written.
fn resolve(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        if let Ok(real) = head.canonicalize() {
            return tail.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (head.file_name().map(ToOwned::to_owned), head.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                head = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Who is changing a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who<'a> {
    /// An agent in a turn.
    Agent(&'a str),
    /// The agent's tidying.
    Tidy(&'a str),
    /// The user, by hand or from their own terminal: never checked.
    Kai,
}

impl Who<'_> {
    fn author(self) -> (Author, Option<String>, Cause) {
        match self {
            Who::Agent(name) => (Author::Bot, Some(name.to_owned()), Cause::Edit),
            Who::Tidy(name) => (Author::Tidy, Some(name.to_owned()), Cause::Tidy),
            Who::Kai => (Author::User, None, Cause::Edit),
        }
    }
}

/// Opens (and on first use creates) the history database. Two sessions at once wait for each other.
pub fn open(layout: &Layout) -> Result<Connection> {
    open_with(layout, BUSY)
}

pub fn open_with(layout: &Layout, busy: Duration) -> Result<Connection> {
    std::fs::create_dir_all(&layout.state).with_context(|| format!("create {}", layout.state.display()))?;
    let conn = Connection::open(layout.db()).context("open memory.db")?;
    conn.busy_timeout(busy)?;
    // WAL lets a reader and a writer overlap; a failure here (a read-only copy) only costs that.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    // Two first runs can both start the migration; the loser finds the tables made and tries once more.
    if schema::migrate(&conn).is_err() {
        schema::migrate(&conn)?;
    }
    tidy::migrate(&conn)?;
    Ok(conn)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// What is on disk: content, modification time (Unix ms) and size. `None` when there is no file.
fn read_disk(path: &Path) -> Option<(String, i64, u64)> {
    // Never block on a pipe or follow a link out of the folder; trust only the open handle.
    let handle = open_regular(path, true)?;
    let meta = handle.metadata().ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(handle, READ_MAX), &mut bytes).ok()?;
    let mtime = Some(meta)
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    let size = bytes.len() as u64;
    Some((String::from_utf8_lossy(&bytes).into_owned(), mtime, size))
}

/// The most of a file a read takes.
const READ_MAX: u64 = 8 * 1024 * 1024;

/// Opens a regular file without blocking; with `no_follow`, not through a link either.
pub(crate) fn open_regular(path: &Path, no_follow: bool) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = libc::O_NONBLOCK | if no_follow { libc::O_NOFOLLOW } else { 0 };
    let file = std::fs::OpenOptions::new().read(true).custom_flags(flags).open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

pub fn read(path: &Path) -> Option<String> {
    read_disk(path).map(|(content, ..)| content)
}

fn immediate(conn: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?)
}

fn next_rev(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT coalesce(max(rev), 0) + 1 FROM memory_file_versions", [], |row| row.get(0))?)
}

/// Records what is on disk as the file's newest version when it differs from the last one.
fn record_disk(
    conn: &Connection,
    target: &Target,
    (author, author_bot, cause): (Author, Option<String>, Cause),
    baseline: bool,
) -> Result<Option<Version>> {
    let disk = read_disk(&target.path);
    let row = files::get(conn, target.bot.as_deref(), &target.name)?;
    let last = files::current(conn, target.bot.as_deref(), &target.name)?;
    let (content, cause_now, mtime, size) = match (&disk, &last) {
        (None, None) => return Ok(None),
        (Some((content, mtime, size)), None) => {
            // Never seen before a change: it is the baseline the change can be undone back to. A file the writer
            // just made, or one seen, deleted and now back, is a change.
            let cause = if row.is_none() && baseline { Cause::Baseline } else { cause };
            (Some(content.as_str()), cause, *mtime, *size)
        }
        (Some((content, mtime, size)), Some(last)) if content != last => (Some(content.as_str()), cause, *mtime, *size),
        (None, Some(_)) => (None, if cause == Cause::Edit { Cause::Delete } else { cause }, 0, 0),
        (Some((_, mtime, size)), Some(_)) => {
            files::observed(conn, target.bot.as_deref(), &target.name, *mtime, *size)?;
            return Ok(None);
        }
    };
    let author = if cause_now == Cause::Baseline { Author::Computer } else { author };
    let change = Change {
        bot: target.bot.as_deref(),
        name: &target.name,
        content,
        author,
        author_bot: author_bot.as_deref(),
        cause: cause_now,
        mtime,
        size,
    };
    let rev = next_rev(conn)?;
    Ok(Some(files::record(conn, &change, now_ms(), rev)?))
}

/// Brings the history up to the file on disk: a file never seen gets a baseline version, and a change made outside
/// the hook (a Bash command, an editor) is recorded as `unknown`, because nothing says who made it. Returns the
/// version it added.
pub fn sync(conn: &Connection, target: &Target) -> Result<Option<Version>> {
    let tx = immediate(conn)?;
    let version = record_disk(&tx, target, (Author::Computer, None, Cause::Edit), true)?;
    tx.commit()?;
    Ok(version)
}

/// Records the file as `who` just changed it. Does nothing when the content is what the last version holds.
pub fn record(conn: &Connection, target: &Target, who: Who<'_>) -> Result<Option<Version>> {
    let tx = immediate(conn)?;
    let version = record_disk(&tx, target, who.author(), false)?;
    tx.commit()?;
    Ok(version)
}

/// Records the file as `who` just put an earlier version back.
pub fn record_undo(conn: &Connection, target: &Target, who: Who<'_>) -> Result<Option<Version>> {
    let (author, author_bot, _) = who.author();
    let tx = immediate(conn)?;
    let version = record_disk(&tx, target, (author, author_bot, Cause::Undo), false)?;
    tx.commit()?;
    Ok(version)
}

/// Records the file as `who` just put an earlier version back by hand (the apps' restore).
pub fn record_restore(conn: &Connection, target: &Target, who: Who<'_>) -> Result<Option<Version>> {
    let (author, author_bot, _) = who.author();
    let tx = immediate(conn)?;
    let version = record_disk(&tx, target, (author, author_bot, Cause::Restore), false)?;
    tx.commit()?;
    Ok(version)
}

/// What a write passes. `old` is the file now (`None`: it doesn't exist), `new` what it would hold. Returns the
/// warnings (a size cap, a big drop) or the reason it is refused. The user is never refused.
pub fn guard(
    conn: Option<&Connection>,
    target: Option<&Target>,
    who: Who<'_>,
    old: Option<&str>,
    new: &str,
) -> std::result::Result<Vec<String>, Refusal> {
    if who != Who::Kai {
        let added = added_lines(old.unwrap_or(""), new);
        check::content(&added)?;
        // Text the file already held is not new: a note that mentions "password" doesn't lock every later edit.
        if let Err(refusal) = check::content(new)
            && old.is_none_or(|old| check::content(old).is_ok())
        {
            return Err(refusal);
        }
        if let (Some(conn), Some(target)) = (conn, target)
            && pieces_forgotten(conn, target, &added)
        {
            return Err(Refusal::Forgotten);
        }
    }
    Ok(warnings(target, old, new))
}

fn warnings(target: Option<&Target>, old: Option<&str>, new: &str) -> Vec<String> {
    let mut out = Vec::new();
    let name = target.map_or("", |t| t.name.as_str());
    if name == "MEMORY.md" && new.lines().count() > INDEX_LINES {
        out.push(format!("MEMORY.md is over {INDEX_LINES} lines: it is a map, so move detail into topic files."));
    } else if name == "user.md" && tokens::estimate(new) > USER_TOKENS {
        out.push(format!("user.md is over {USER_TOKENS} tokens and every agent reads it: keep it to who the user is."));
    } else if tokens::estimate(new) > files::FILE_CAP {
        out.push(format!("This file is over {} tokens: shorten it or move a topic to its own file.", files::FILE_CAP));
    }
    if let Some(old) = old
        && files::drops_much(old, new)
    {
        out.push(
            "This removes more than a quarter of the file. `kith memory undo` brings the last version back.".into(),
        );
    }
    out
}

/// The lines of `new` that `old` doesn't have.
fn added_lines(old: &str, new: &str) -> String {
    let seen: HashSet<&str> = old.lines().map(str::trim).collect();
    new.lines().filter(|line| !seen.contains(line.trim())).collect::<Vec<_>>().join("\n")
}

/// Whether any line or sentence of `text` is one the user told Kith to forget: for the file's layer and for all.
pub fn pieces_forgotten(conn: &Connection, target: &Target, text: &str) -> bool {
    let scopes = [None, target.bot.as_deref()];
    pieces(text).iter().any(|piece| scopes.iter().any(|scope| store::forgotten(conn, *scope, piece).unwrap_or(false)))
}

/// The text as a whole, then each line without its list marker, then each sentence.
fn pieces(text: &str) -> Vec<String> {
    let mut out = vec![text.to_owned()];
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '+', '>', '#', ' ']).trim();
        // A numbered list item: "3. text".
        let line =
            line.split_once(". ").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map_or(line, |(_, rest)| rest);
        if line.is_empty() {
            continue;
        }
        out.push(line.to_owned());
        let sentences = line.split(['。', '；', ';']).chain(line.split(". "));
        out.extend(sentences.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()));
    }
    out
}

/// What a checked write did.
#[derive(Debug)]
pub struct Written {
    pub version: Option<Version>,
    pub warnings: Vec<String>,
}

/// Writes a memory file as `who`: records the file as it is (so nothing is lost), checks the new content, writes it
/// and records it. The call for code that writes memory itself, such as the host's tidying.
pub fn write(
    conn: &Connection,
    target: &Target,
    who: Who<'_>,
    content: &str,
) -> Result<std::result::Result<Written, Refusal>> {
    sync(conn, target)?;
    let old = read(&target.path);
    let warnings = match guard(Some(conn), Some(target), who, old.as_deref(), content) {
        Ok(warnings) => warnings,
        Err(refusal) => return Ok(Err(refusal)),
    };
    put(&target.path, Some(content))?;
    Ok(Ok(Written { version: record(conn, target, who)?, warnings }))
}

/// Writes (or, with `None`, removes) a file through a temporary one beside it, so a reader never sees half a file.
pub fn put(path: &Path, content: Option<&str>) -> Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(content) = content else {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("remove the file"),
            _ => Ok(()),
        };
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("kith-{}-{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path).context("replace the file")
}

/// Tells Kith to keep a fact out of memory: no agent or tidy writes it again.
pub fn forget(conn: &Connection, text: &str) -> Result<()> {
    store::forbid(conn, None, text, None)?;
    Ok(())
}

#[cfg(test)]
mod promote_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tidy_tests;
