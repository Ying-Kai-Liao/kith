//! Tidying over the agents' files: kith-mem's tidy (entries, quotes from the user's words, an audit) mapped onto
//! the Markdown an hq agent keeps.
//!
//! What the model sees as entries are the bullet lines of the agent's `memory/MEMORY.md` and of the shared
//! `user.md`, numbered 1.. for the run. A line is the tidy-up's own when it is under a `## From conversations`
//! heading and a run logged writing it (the agent also appends to the end of MEMORY.md, which may be that
//! section); only those can be archived, rewritten or moved out. Every other line is shown as the user's. The agent's other
//! top-level notes (`memory/*.md`) are the "long-form files": listed with their descriptions, appended to, never
//! rewritten. Every file goes through [`super::write`] as `Who::Tidy`, so the same checks, forgotten facts and
//! versions apply as to any write, and a refused file is dropped whole.
//!
//! Each run is a row of `memory_dreams` (keyed by the agent) with the versions it wrote in `memory_dream_files`
//! and its line changes in `hq_dream_changes`; [`undo`] puts the files back.

use super::{Layout, Target, Who, now_ms, put, read, record_undo, sync, write};
use anyhow::{Result, anyhow, bail};
use hmac::{KeyInit as _, Mac as _};
use kith_mem::files::{self, File, Version};
use kith_mem::rusqlite::{Connection, params};
use kith_mem::store::{self, Author, Entry, State};
use kith_mem::tidy::{
    self, Audience, Judgement, NoteFacets, Plan, Promote, Retag, Roster, Snapshot, judge, set_frontmatter,
};
use kith_mem::tokens;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The heading a tidy-up's own lines live under, in `MEMORY.md` (read every session) and in `user.md`.
pub const SECTION: &str = "From conversations";
/// The most lines one run shows the model; the tidy-up's own come first.
const MAX_LINES: usize = 200;
/// The most notes listed as long-form files.
const MAX_NOTES: usize = 30;
/// Characters of one line the model sees.
const LINE_CHARS: usize = 400;
/// `MEMORY.md` is "full" at nine-tenths of the lines hqmem warns at, `user.md` of its token budget: the model may
/// then move lines out to notes.
const FULL_LINES: usize = 135;
const FULL_USER_TOKENS: usize = 360;
const MEMORY: &str = "MEMORY.md";
const USER: &str = "user.md";

/// Creates the table of a run's line changes. Idempotent; [`super::open_with`] calls it.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS hq_dream_changes (
            dream INTEGER NOT NULL,
            kind TEXT NOT NULL,
            file TEXT NOT NULL,
            shared INTEGER NOT NULL,
            text TEXT NOT NULL,
            was TEXT
        );
        CREATE INDEX IF NOT EXISTS hq_dream_changes_dream ON hq_dream_changes (dream);
        -- Retags of notes (promotion). `applied`: a narrowing tidying applied; `pending`: a widening waiting for the
        -- person in `person` ('' is the owner); `approved`: that person said yes and it was written; `declined`:
        -- kept private; `stale`: the note or the readers changed before the answer. `kith_facets.py` reads the
        -- applied and approved rows to honour a wider audience in a note's frontmatter, so a row is the proof, and
        -- `sig` (see [`signature`]) shows the host wrote it.
        CREATE TABLE IF NOT EXISTS hq_promotions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            agent TEXT NOT NULL,
            file TEXT NOT NULL,
            from_audience TEXT NOT NULL,
            audience TEXT NOT NULL,
            room TEXT NOT NULL DEFAULT '',
            projects TEXT NOT NULL DEFAULT '[]',
            readers TEXT NOT NULL DEFAULT '',
            reason TEXT NOT NULL DEFAULT '',
            person TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL,
            decided_by TEXT,
            created_at INTEGER NOT NULL,
            decided_at INTEGER,
            dream INTEGER,
            sig TEXT
        );
        CREATE INDEX IF NOT EXISTS hq_promotions_agent ON hq_promotions (agent, status);",
    )?;
    let signed =
        conn.query_row("SELECT 1 FROM pragma_table_info('hq_promotions') WHERE name = 'sig'", [], |_| Ok(())).is_ok();
    if !signed {
        conn.execute_batch("ALTER TABLE hq_promotions ADD COLUMN sig TEXT")?;
        // Rows granted before approvals were signed are trusted once, as they stand at the upgrade. With no key to sign
        // them they stay unsigned and grant nothing: their notes read as the folder default.
        let ids = conn
            .prepare("SELECT id FROM hq_promotions WHERE status IN ('applied', 'approved')")?
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !ids.is_empty() && approval_key(conn, true).is_some() {
            for id in ids {
                sign(conn, id)?;
            }
        }
    }
    Ok(())
}

/// The agent's folder: the first of the layout's agents folders that holds one by its name.
pub fn agent_dir(layout: &Layout, agent: &str) -> Option<PathBuf> {
    if agent.is_empty() || agent.starts_with('.') || agent.contains(['/', '\\']) {
        return None;
    }
    layout.agents.iter().map(|root| root.join(agent)).find(|dir| dir.is_dir())
}

/// A memory file of the agent (`shared`: false) or of the shared user layer, `None` when the layout has no such
/// agent or the name isn't a memory file.
pub fn target_of(layout: &Layout, agent: &str, shared: bool, name: &str) -> Option<Target> {
    let path = if shared { layout.state.join(name) } else { agent_dir(layout, agent)?.join("memory").join(name) };
    layout.locate(&path, Path::new("/")).filter(|t| t.bot.is_none() == shared && t.name == name)
}

struct Loaded {
    target: Target,
    /// What the run saw: `None` when there was no file.
    content: Option<String>,
}

/// A numbered line the model sees.
struct Line {
    /// Its whole text (the model sees it clipped).
    full: String,
    file: usize,
    /// Its index among the file's lines.
    index: usize,
    tidy: bool,
}

/// The memory of one agent as a run sees it.
pub struct Material {
    pub snapshot: Snapshot,
    agent: String,
    layout: Layout,
    /// `[0]` the agent's MEMORY.md, `[1]` user.md, then its notes.
    loaded: Vec<Loaded>,
    /// Line `n` is `lines[n - 1]`.
    lines: Vec<Line>,
}

struct Bullet {
    index: usize,
    text: String,
    tidy: bool,
}

fn heading(trimmed: &str) -> Option<&str> {
    trimmed.starts_with('#').then(|| trimmed.trim_start_matches('#').trim())
}

fn marker(trimmed: &str) -> Option<&str> {
    trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "))
}

/// The bullet lines of a file outside its frontmatter, and whether each is under the tidy-up's heading.
fn bullets(content: &str) -> Vec<Bullet> {
    let (mut front, mut tidy, mut out) = (false, false, Vec::new());
    for (index, raw) in content.lines().enumerate() {
        let t = raw.trim();
        if index == 0 && t == "---" {
            front = true;
            continue;
        }
        if front {
            front = t != "---";
            continue;
        }
        if let Some(h) = heading(t) {
            tidy = h == SECTION;
        } else if let Some(text) = marker(t).map(str::trim).filter(|t| !t.is_empty()) {
            out.push(Bullet { index, text: text.to_owned(), tidy });
        }
    }
    out
}

fn clip(text: &str) -> String {
    if text.chars().count() <= LINE_CHARS { text.to_owned() } else { text.chars().take(LINE_CHARS).collect() }
}

/// The lines (as [`norm`] reads them) that the agent's runs wrote and are still in effect: what the tidy may change.
pub fn written(conn: &Connection, agent: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT c.text FROM hq_dream_changes c JOIN memory_dreams d ON d.id = c.dream
         WHERE d.bot = ?1 AND d.undone = 0 AND c.kind IN ('added', 'indexed', 'reviewed')",
    )?;
    let texts = stmt.query_map([agent], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(texts.iter().map(|t| norm(t)).collect())
}

/// Reads the agent's memory and the shared one. `None` when the agent has no folder here. `written` is what
/// [`written`] says; without it every line under the tidy-up's heading counts as the tidy-up's.
pub fn load(layout: &Layout, agent: &str, today: &str, written: Option<&HashSet<String>>) -> Result<Option<Material>> {
    let Some(dir) = agent_dir(layout, agent) else { return Ok(None) };
    let wanted = |shared: bool, name: &str| {
        target_of(layout, agent, shared, name).ok_or_else(|| anyhow!("{name} is not a memory file here"))
    };
    let mut loaded = Vec::new();
    for (shared, name) in [(false, MEMORY), (true, USER)] {
        let target = wanted(shared, name)?;
        loaded.push(Loaded { content: read(&target.path), target });
    }
    // The agent's other top-level notes, for the model to append to.
    let mut notes: Vec<String> = std::fs::read_dir(dir.join("memory"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| super::is_markdown(n) && n != MEMORY && !n.starts_with('.'))
        .collect();
    notes.sort();
    notes.truncate(MAX_NOTES);
    let mut listed = Vec::new();
    for name in notes {
        let Some(target) = target_of(layout, agent, false, &name) else { continue };
        let content = read(&target.path);
        listed.push(File {
            bot: Some(agent.to_owned()),
            hash: content.as_deref().map(files::hash).unwrap_or_default(),
            size: content.as_deref().map_or(0, |c| c.len() as u64),
            mtime: 0,
            description: content.as_deref().and_then(files::description),
            tokens: content.as_deref().map_or(0, tokens::estimate),
            deleted: false,
            version: 0,
            updated_at: 0,
            rev: 0,
            name,
        });
        loaded.push(Loaded { target, content });
    }

    let mut all: Vec<(usize, Bullet)> = Vec::new();
    for (file, l) in loaded.iter().take(2).enumerate() {
        let lines = bullets(l.content.as_deref().unwrap_or_default());
        all.extend(lines.into_iter().map(|mut b| {
            b.tidy &= written.is_none_or(|w| w.contains(&norm(&b.text)));
            (file, b)
        }));
    }
    if all.len() > MAX_LINES {
        let mut room = MAX_LINES.saturating_sub(all.iter().filter(|(_, b)| b.tidy).count());
        all.retain(|(_, b)| {
            b.tidy || {
                let keep = room > 0;
                room = room.saturating_sub(1);
                keep
            }
        });
    }
    let (mut entries, mut lines) = (Vec::new(), Vec::new());
    for (n, (file, b)) in all.into_iter().enumerate() {
        entries.push(Entry {
            id: i64::try_from(n + 1)?,
            bot: (file == 0).then(|| agent.to_owned()),
            state: State::Active,
            text: clip(&b.text),
            author: if b.tidy { Author::Tidy } else { Author::User },
            author_bot: b.tidy.then(|| agent.to_owned()),
            pinned: false,
            superseded_by: None,
            room: None,
            evidence: Vec::new(),
            created_at: 0,
            updated_at: 0,
            rev: 0,
        });
        lines.push(Line { full: b.text, file, index: b.index, tidy: b.tidy });
    }
    let full = loaded[0].content.as_deref().is_some_and(|c| c.lines().count() >= FULL_LINES)
        || loaded[1].content.as_deref().is_some_and(|c| tokens::estimate(c) >= FULL_USER_TOKENS);
    let snapshot = Snapshot::from_parts(agent, true, entries, listed, full, today);
    Ok(Some(Material { snapshot, agent: agent.to_owned(), layout: layout.clone(), loaded, lines }))
}

/// The most notes whose tags a run shows the model.
const MAX_PROMOTABLE: usize = 60;

/// Who reads which room. Nothing resolves a group room's members yet, so every room is unknown and a move into one
/// is a question (docs/memory-context.md, Promotion).
fn roster() -> Roster {
    Roster::default()
}

/// Whether change `id` is a retag of `file` to `audience` that was applied or approved, not undone, and signed by
/// the host: a row written into the database by hand grants nothing.
pub(super) fn granted(conn: &Connection, agent: &str, file: &str, id: i64, audience: &str) -> bool {
    let sig: Option<String> = conn
        .query_row(
            "SELECT p.sig FROM hq_promotions p JOIN memory_dreams d ON d.id = p.dream
             WHERE p.id = ?1 AND p.agent = ?2 AND p.file = ?3 AND p.audience = ?4
               AND p.status IN ('applied', 'approved') AND d.undone = 0
               AND NOT EXISTS (SELECT 1 FROM hq_promotions q JOIN memory_dreams e ON e.id = q.dream
                               WHERE q.agent = p.agent AND q.file = p.file AND q.id > p.id
                                 AND q.status IN ('applied', 'approved') AND e.undone = 0)",
            params![id, agent, file, audience],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let (Some(sig), Some(key)) = (sig, approval_key(conn, false)) else { return false };
    let Some(sig) = unhex(&sig) else { return false };
    mac(&key, id, agent, file, audience).is_some_and(|m| m.verify_slice(&sig).is_ok())
}

/// The key that signs granted retags: `secrets/memory-approvals.key` beside the database, in the state folder, where
/// no guest run reads. Only the host makes it (`create`); the kernel's twin (`kith_facets.py`) only reads it.
fn approval_key(conn: &Connection, create: bool) -> Option<Vec<u8>> {
    let db = Path::new(conn.path().filter(|p| !p.is_empty())?);
    let path = db.parent()?.join("secrets").join("memory-approvals.key");
    if create && !path.exists() {
        make_key(&path).ok()?;
    }
    std::fs::read(&path).ok().filter(|k| k.len() == 32)
}

fn make_key(path: &Path) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let dir = path.parent().ok_or_else(|| anyhow!("no folder for the key"))?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|_| anyhow!("random key unavailable"))?;
    let tmp = dir.join(format!(".memory-approvals.{}.tmp", uuid::Uuid::new_v4()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(&key)?;
    f.sync_all()?;
    // Linked into place whole, so two processes making it at once keep one key and none reads half of one.
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(e.into()),
        _ => Ok(()),
    }
}

/// The HMAC over what a grant is checked against (twin of `signature` in `kernel/lib/kith_facets.py`). Each text is
/// prefixed with its length in bytes, so no two rows sign the same message.
fn mac(key: &[u8], id: i64, agent: &str, file: &str, audience: &str) -> Option<hmac::Hmac<sha2::Sha256>> {
    use std::fmt::Write as _;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).ok()?;
    let mut msg = format!("kith-promotion-v1\n{id}");
    for field in [agent, file, audience] {
        let _ = write!(msg, "\n{}:{field}", field.len());
    }
    mac.update(msg.as_bytes());
    Some(mac)
}

pub(super) fn signature(key: &[u8], id: i64, agent: &str, file: &str, audience: &str) -> Option<String> {
    use std::fmt::Write as _;
    let bytes = mac(key, id, agent, file, audience)?.finalize().into_bytes();
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    Some(s)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Signs row `id` once it is applied or approved: what [`granted`] checks.
fn sign(conn: &Connection, id: i64) -> Result<()> {
    let key = approval_key(conn, true).ok_or_else(|| anyhow!("no key to sign the approval with"))?;
    let (agent, file, audience): (String, String, String) =
        conn.query_row("SELECT agent, file, audience FROM hq_promotions WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let sig = signature(&key, id, &agent, &file, &audience).ok_or_else(|| anyhow!("bad key"))?;
    conn.execute("UPDATE hq_promotions SET sig = ?1 WHERE id = ?2", params![sig, id])?;
    Ok(())
}

/// The markdown files under the agent's `memory/`, relative, sorted, without hidden ones or links.
fn memory_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, rel: &Path, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(base.join(rel)) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ty) = e.file_type() else { continue };
            if name.starts_with('.') || ty.is_symlink() {
                continue;
            }
            let path = rel.join(&name);
            if ty.is_dir() {
                if depth < 4 {
                    walk(base, &path, depth + 1, out);
                }
            } else if super::is_markdown(&name) {
                out.push(path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, Path::new(""), 0, &mut out);
    out.sort();
    out
}

impl Material {
    /// This run may also propose retags: the model is shown which notes who reads. Rows in `hq_promotions` decide
    /// which wider frontmatter audiences count, as they do for the kernel.
    #[must_use]
    pub fn with_promotions(mut self, conn: &Connection) -> Self {
        let Some(dir) = agent_dir(&self.layout, &self.agent) else { return self };
        let facets: Vec<NoteFacets> = memory_files(&dir.join("memory"))
            .into_iter()
            .filter_map(|rel| {
                let target = target_of(&self.layout, &self.agent, false, &rel)?;
                let text = read(&target.path).unwrap_or_default();
                Some(NoteFacets::read(&self.agent, &rel, &text, &|id, a| granted(conn, &self.agent, &rel, id, a)))
            })
            .take(MAX_PROMOTABLE)
            .collect();
        self.snapshot = self.snapshot.clone().with_facets(facets, roster());
        self
    }

    /// The texts of the lines the tidy wrote itself (what the time review may rewrite).
    pub fn tidy_lines(&self) -> impl Iterator<Item = &str> {
        self.snapshot.entries.iter().zip(&self.lines).filter(|(_, l)| l.tidy).map(|(e, _)| e.text.as_str())
    }
}

/// A file's lines while a run edits it.
struct Doc {
    lines: Vec<String>,
    remove: BTreeSet<usize>,
    replace: HashMap<usize, String>,
}

impl Doc {
    fn new(content: Option<&str>) -> Self {
        Self {
            lines: content.unwrap_or_default().lines().map(str::to_owned).collect(),
            remove: BTreeSet::new(),
            replace: HashMap::new(),
        }
    }

    /// Applies the marked removals and rewrites. The indices of those marks are the file's own, so this comes
    /// before any append.
    fn settle(&mut self) {
        for (index, text) in std::mem::take(&mut self.replace) {
            let line = &self.lines[index];
            let lead: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let bullet = if line.trim_start().starts_with('*') { "*" } else { "-" };
            self.lines[index] = format!("{lead}{bullet} {text}");
        }
        for index in std::mem::take(&mut self.remove).into_iter().rev() {
            self.lines.remove(index);
        }
    }

    fn has(&self, text: &str) -> bool {
        let wanted = norm(text);
        self.lines.iter().any(|l| marker(l.trim()).is_some_and(|t| norm(t) == wanted))
    }

    fn mentions(&self, name: &str) -> bool {
        self.lines.iter().any(|l| l.contains(name))
    }

    /// Adds a bullet at the end of the tidy-up's section, which a file without one gets.
    fn add(&mut self, text: &str, title: &str) {
        let line = format!("- {}", one_line(text));
        if self.lines.iter().all(|l| l.trim().is_empty()) {
            self.lines = vec![format!("# {title}"), String::new()];
        }
        if let Some(h) = self.lines.iter().position(|l| heading(l.trim()) == Some(SECTION)) {
            let end = (h + 1..self.lines.len()).find(|&i| heading(self.lines[i].trim()).is_some());
            let end = end.unwrap_or(self.lines.len());
            let at = (h + 1..end).rev().find(|&i| !self.lines[i].trim().is_empty()).map_or(h + 1, |i| i + 1);
            self.lines.insert(at, line);
        } else {
            if self.lines.last().is_some_and(|l| !l.trim().is_empty()) {
                self.lines.push(String::new());
            }
            self.lines.push(format!("## {SECTION}"));
            self.lines.push(line);
        }
    }

    /// A bullet at the end of the file (a note's line).
    fn push(&mut self, text: &str) {
        self.lines.push(format!("- {}", one_line(text)));
    }

    fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A line compared with another: case, spacing and end punctuation aside.
fn norm(text: &str) -> String {
    one_line(text).to_lowercase().trim_end_matches(['.', '。', '!', '！']).to_owned()
}

/// A line for a note: its text, where it goes, and the line of MEMORY.md or user.md it was moved from.
struct Note {
    file: String,
    text: String,
    description: String,
    from: Option<usize>,
}

/// What a run wrote.
#[derive(Debug, Default)]
pub struct Applied {
    /// The logged run; `None` when nothing was written.
    pub tidy: Option<i64>,
    /// Memory changed during the run: nothing was written.
    pub stale: bool,
    pub versions: Vec<Version>,
    /// Changes dropped, with why: a refused file, a shared note.
    pub dropped: Vec<String>,
    /// The widening retags that now wait for a person (ids in `hq_promotions`).
    pub asked: Vec<i64>,
}

/// Writes an audited plan as one logged run of the agent. `Applied::stale` when a file the run read has changed.
pub fn apply(conn: &Connection, m: &Material, plan: &Plan) -> Result<Applied> {
    let mut out = Applied::default();
    if plan.is_empty() {
        return Ok(out);
    }
    if m.loaded.iter().any(|l| read(&l.target.path) != l.content) {
        out.stale = true;
        return Ok(out);
    }
    let line = |id: i64| usize::try_from(id).ok().and_then(|n| n.checked_sub(1)).and_then(|i| m.lines.get(i));
    let entry = |id: i64| m.snapshot.entries.iter().find(|e| e.id == id);

    // The notes first: a line only leaves MEMORY.md once the note that now holds it is written.
    let mut notes: Vec<Note> = Vec::new();
    for a in &plan.append {
        notes.push(Note { file: a.file.clone(), text: a.text.clone(), description: a.description.clone(), from: None });
    }
    for mv in &plan.move_out {
        match (line(mv.id), entry(mv.id)) {
            (Some(l), Some(_)) if l.file == 0 => notes.push(Note {
                file: mv.file.clone(),
                text: l.full.clone(),
                description: mv.description.clone(),
                from: Some(usize::try_from(mv.id - 1)?),
            }),
            _ => out.dropped.push(format!("move [{}]: only the agent's own lines move to notes", mv.id)),
        }
    }
    // The run is logged once a file is written, so a run that wrote nothing leaves no undoable row.
    let run = std::cell::Cell::new(None::<i64>);
    let ensure_run = || -> Result<i64> {
        if let Some(id) = run.get() {
            return Ok(id);
        }
        conn.execute("INSERT INTO memory_dreams (bot, at) VALUES (?1, ?2)", params![m.agent, now_ms()])?;
        run.set(Some(conn.last_insert_rowid()));
        Ok(conn.last_insert_rowid())
    };
    let log = |kind: &str, target: &Target, text: &str, was: Option<&str>| -> Result<()> {
        conn.execute(
            "INSERT INTO hq_dream_changes (dream, kind, file, shared, text, was) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ensure_run()?, kind, target.name, target.bot.is_none(), text, was],
        )?;
        Ok(())
    };
    let put_file = |out: &mut Applied, target: &Target, before: Option<&str>, after: &str| -> Result<bool> {
        if before == Some(after) {
            return Ok(false);
        }
        match write(conn, target, Who::Tidy(&m.agent), after)? {
            Ok(written) => {
                if let Some(v) = written.version {
                    conn.execute(
                        "INSERT INTO memory_dream_files (dream, version) VALUES (?1, ?2)",
                        params![ensure_run()?, v.id],
                    )?;
                    out.versions.push(v);
                }
                Ok(true)
            }
            Err(refusal) => {
                out.dropped.push(format!("{}: {}", target.label(), refusal.message()));
                Ok(false)
            }
        }
    };

    let mut by_file: BTreeMap<&str, Vec<&Note>> = BTreeMap::new();
    for note in &notes {
        by_file.entry(note.file.as_str()).or_default().push(note);
    }
    let mut written_notes: HashSet<String> = HashSet::new();
    for (file, group) in by_file {
        let Some(target) = target_of(&m.layout, &m.agent, false, file) else {
            out.dropped.push(format!("{file}: not a memory file of {}", m.agent));
            continue;
        };
        let before = m
            .loaded
            .iter()
            .find(|l| l.target.bot.is_some() && l.target.name == file)
            .map_or_else(|| read(&target.path), |l| l.content.clone());
        let mut doc = Doc::new(before.as_deref());
        if before.as_deref().is_none_or(|c| c.trim().is_empty()) {
            let description = group.iter().map(|n| n.description.trim()).find(|d| !d.is_empty()).unwrap_or(file);
            doc.lines = vec!["---".into(), format!("description: {}", one_line(description)), "---".into()];
        }
        let mut added = Vec::new();
        for note in group {
            if !doc.has(&note.text) {
                doc.push(&note.text);
                added.push(note.text.trim().to_owned());
            }
        }
        let wrote = put_file(&mut out, &target, before.as_deref(), &doc.text())?;
        // The note holds the lines now: this run wrote them, or the file had them already.
        if wrote || added.is_empty() {
            written_notes.insert(file.to_owned());
        }
        if wrote {
            for text in &added {
                log("noted", &target, text, None)?;
            }
        }
    }

    // The agent's MEMORY.md and the shared user.md.
    let (mut memory, mut user) = (Doc::new(m.loaded[0].content.as_deref()), Doc::new(m.loaded[1].content.as_deref()));
    let mut changes: [Vec<(&str, String, Option<String>)>; 2] = [Vec::new(), Vec::new()];
    let mut marks = |file: usize,
                     index: usize,
                     new: Option<&str>,
                     changes: &mut [Vec<(&'static str, String, Option<String>)>; 2],
                     kind: &'static str,
                     text: String,
                     was: Option<String>| {
        let doc = if file == 0 { &mut memory } else { &mut user };
        match new {
            Some(new) => {
                doc.replace.insert(index, one_line(new));
            }
            None => {
                doc.remove.insert(index);
            }
        }
        changes[file].push((kind, text, was));
    };
    for a in &plan.archive {
        if let (Some(l), Some(e)) = (line(a.id), entry(a.id))
            && l.tidy
        {
            marks(l.file, l.index, None, &mut changes, "archived", e.text.clone(), None);
        }
    }
    for r in &plan.review {
        if let (Some(l), Some(e)) = (line(r.id), entry(r.id))
            && l.tidy
        {
            marks(l.file, l.index, Some(&r.text), &mut changes, "reviewed", one_line(&r.text), Some(e.text.clone()));
        }
    }
    for note in notes.iter().filter(|n| written_notes.contains(&n.file)) {
        if let Some(from) = note.from {
            marks(0, m.lines[from].index, None, &mut changes, "archived", note.text.clone(), None);
        }
    }
    memory.settle();
    user.settle();
    for add in &plan.add {
        let (doc, title, file) = if add.shared { (&mut user, "About the user", 1) } else { (&mut memory, "Memory", 0) };
        if doc.has(&add.text) {
            out.dropped.push(format!("add \"{}\": already in the file", add.text));
        } else {
            doc.add(&add.text, title);
            changes[file].push(("added", add.text.trim().to_owned(), None));
        }
    }
    let mut indexed: HashSet<&str> = HashSet::new();
    for note in notes.iter().filter(|n| written_notes.contains(&n.file)) {
        if !memory.mentions(&note.file) && indexed.insert(&note.file) {
            let description = m
                .snapshot
                .files
                .iter()
                .find(|f| f.name == note.file)
                .and_then(|f| f.description.clone())
                .or_else(|| Some(note.description.trim().to_owned()).filter(|d| !d.is_empty()))
                .unwrap_or_else(|| note.file.clone());
            let line = format!("[{0}]({0}) — {description}", note.file);
            memory.add(&line, "Memory");
            changes[0].push(("indexed", line, None));
        }
    }
    for (file, doc, changes) in [(0, &memory, &changes[0]), (1, &user, &changes[1])] {
        let l = &m.loaded[file];
        if !changes.is_empty() && put_file(&mut out, &l.target, l.content.as_deref(), &doc.text())? {
            for (kind, text, was) in changes {
                log(kind, &l.target, text, was.as_deref())?;
            }
        }
    }
    // Retags. Each is judged again against the note as it is now, because the notes or the rooms' members may have
    // changed since the audit: one that keeps the readers is applied, one that adds readers waits for the person.
    for p in &plan.promote {
        let Some(target) = target_of(&m.layout, &m.agent, false, &p.file) else {
            out.dropped.push(format!("promote {}: not a memory file of {}", p.file, m.agent));
            continue;
        };
        let Some(text) = read(&target.path) else {
            out.dropped.push(format!("promote {}: no such file", p.file));
            continue;
        };
        let current = facets_now(conn, &m.agent, &p.file, &text);
        match judge(&current, p, &roster()) {
            Err(why) => out.dropped.push(format!("promote {}: {why}", p.file)),
            Ok(Judgement::Nothing) => out.dropped.push(format!("promote {}: nothing would change", p.file)),
            Ok(Judgement::Widens(retag)) => {
                if let Some(id) = ask(conn, &m.agent, p, &current, &retag)? {
                    out.asked.push(id);
                }
            }
            Ok(Judgement::Allowed(retag)) => {
                let change = insert_row(conn, &m.agent, p, &current, &retag, "applied", "")?;
                sign(conn, change)?;
                let after = retagged(&text, &retag, change, "tidy");
                if put_file(&mut out, &target, Some(&text), &after)? {
                    conn.execute("UPDATE hq_promotions SET dream = ?1 WHERE id = ?2", params![ensure_run()?, change])?;
                    log(
                        "retagged",
                        &target,
                        &describe(&current, &retag, &p.file),
                        Some(&current.audience.to_string()),
                    )?;
                } else {
                    conn.execute("DELETE FROM hq_promotions WHERE id = ?1", [change])?;
                }
            }
        }
    }
    out.tidy = run.get();
    Ok(out)
}

fn facets_now(conn: &Connection, agent: &str, file: &str, text: &str) -> NoteFacets {
    NoteFacets::read(agent, file, text, &|id, a| granted(conn, agent, file, id, a))
}

fn describe(current: &NoteFacets, retag: &Retag, file: &str) -> String {
    let mut parts = Vec::new();
    if current.audience != retag.audience {
        parts.push(format!("{} → {}", current.audience, retag.audience));
    }
    if current.room != retag.room {
        parts.push(format!("room {}", retag.room));
    }
    if current.projects != retag.projects {
        parts.push(format!("projects {}", retag.projects.join(", ")));
    }
    format!("{file}: {}", parts.join("; "))
}

/// The note's text with the tags of `retag` in its frontmatter, and the change that vouches for them.
fn retagged(text: &str, retag: &Retag, change: i64, by: &str) -> String {
    set_frontmatter(
        text,
        &[
            ("audience", Some(retag.audience.to_string())),
            ("room", Some(retag.room.clone()).filter(|r| !r.is_empty())),
            ("projects", Some(format!("[{}]", retag.projects.join(", ")))),
            ("approved_by", Some(by.to_owned())),
            ("approved_change", Some(change.to_string())),
        ],
    )
}

fn insert_row(
    conn: &Connection,
    agent: &str,
    p: &Promote,
    current: &NoteFacets,
    retag: &Retag,
    status: &str,
    person: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO hq_promotions
             (agent, file, from_audience, audience, room, projects, readers, reason, person, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            agent,
            p.file,
            current.audience.to_string(),
            retag.audience.to_string(),
            retag.room,
            serde_json::to_string(&retag.projects)?,
            roster().fingerprint(&retag.audience),
            p.reason.chars().take(200).collect::<String>(),
            person,
            status,
            now_ms()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Puts a widening retag to the note's person. `None` when the same question was asked already (pending, or
/// answered "keep private"): tidying doesn't ask twice.
fn ask(conn: &Connection, agent: &str, p: &Promote, current: &NoteFacets, retag: &Retag) -> Result<Option<i64>> {
    let again = conn
        .query_row(
            "SELECT 1 FROM hq_promotions WHERE agent = ?1 AND file = ?2 AND audience = ?3 AND room = ?4
               AND projects = ?5 AND status IN ('pending', 'declined')",
            params![agent, p.file, retag.audience.to_string(), retag.room, serde_json::to_string(&retag.projects)?],
            |_| Ok(()),
        )
        .is_ok();
    // Nobody can say who would read a room with no roster, so there is nothing to put to the person yet.
    if again || roster().fingerprint(&retag.audience) == "?" {
        return Ok(None);
    }
    let person = current.audience.asks().unwrap_or_default();
    insert_row(conn, agent, p, current, retag, "pending", person).map(Some)
}

/// The waiting questions (`status` "pending") or the answered ones, newest first, as the apps show them. `asks` is
/// the person who has to answer: empty is the owner.
pub fn promotions(conn: &Connection, agent: &str, status: &str, limit: usize) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, file, from_audience, audience, room, projects, reason, person, status, created_at, decided_at
         FROM hq_promotions WHERE agent = ?1 AND status = ?2 ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![agent, status, i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
        let projects: String = row.get(5)?;
        Ok(json!({
            "id": row.get::<_, i64>(0)?, "file": row.get::<_, String>(1)?, "from": row.get::<_, String>(2)?,
            "audience": row.get::<_, String>(3)?, "room": row.get::<_, String>(4)?,
            "projects": serde_json::from_str::<Value>(&projects).unwrap_or_else(|_| json!([])),
            "reason": row.get::<_, String>(6)?, "asks": row.get::<_, String>(7)?, "status": row.get::<_, String>(8)?,
            "at": row.get::<_, i64>(9)?, "decidedAt": row.get::<_, Option<i64>>(10)?,
        }))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// How a question ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Answered {
    /// `approved` (written), `declined` (kept private) or `stale` (the note or its readers changed: nothing written,
    /// tidying will ask again if it still wants to).
    pub status: &'static str,
    /// The logged run that wrote it, which the tidy-up undo covers.
    pub run: Option<i64>,
}

/// The person's answer to a waiting question. `by` is who answers: empty for the owner, else a person's id; only
/// the person asked can approve (anyone who can reach the owner's apps can keep a note private).
pub fn answer(conn: &Connection, layout: &Layout, agent: &str, id: i64, approve: bool, by: &str) -> Result<Answered> {
    struct Row {
        file: String,
        from: String,
        audience: String,
        room: String,
        projects: String,
        readers: String,
        reason: String,
        person: String,
    }
    let row = conn
        .query_row(
            "SELECT file, from_audience, audience, room, projects, readers, reason, person FROM hq_promotions
             WHERE id = ?1 AND agent = ?2 AND status = 'pending'",
            params![id, agent],
            |r| {
                Ok(Row {
                    file: r.get(0)?,
                    from: r.get(1)?,
                    audience: r.get(2)?,
                    room: r.get(3)?,
                    projects: r.get(4)?,
                    readers: r.get(5)?,
                    reason: r.get(6)?,
                    person: r.get(7)?,
                })
            },
        )
        .map_err(|_| anyhow!("no such question waiting"))?;
    let decide = |status: &str| -> Result<()> {
        let who = if by.is_empty() { "owner" } else { by };
        // Only a question still waiting can be answered, and only once.
        let n = conn.execute(
            "UPDATE hq_promotions SET status = ?1, decided_by = ?2, decided_at = ?3 WHERE id = ?4 AND status = 'pending'",
            params![status, who, now_ms(), id],
        )?;
        if n == 1 { Ok(()) } else { bail!("no such question waiting") }
    };
    if !approve {
        decide("declined")?;
        return Ok(Answered { status: "declined", run: None });
    }
    if by != row.person {
        let who = if row.person.is_empty() { "the owner" } else { &row.person };
        bail!("{who} has to approve this, not {}", if by.is_empty() { "the owner" } else { by });
    }
    let target = target_of(layout, agent, false, &row.file).ok_or_else(|| anyhow!("the note is gone"))?;
    sync(conn, &target)?;
    let text = read(&target.path).ok_or_else(|| anyhow!("the note is gone"))?;
    let current = facets_now(conn, agent, &row.file, &text);
    let retag = Retag {
        audience: Audience::parse(&row.audience).ok_or_else(|| anyhow!("bad audience"))?,
        room: row.room.clone(),
        projects: serde_json::from_str(&row.projects).unwrap_or_default(),
    };
    // The approval was for these readers of this note. If either moved, it counts for nothing.
    let same = current.audience.to_string() == row.from
        && roster().fingerprint(&retag.audience) == row.readers
        && row.readers != "?";
    if !same {
        decide("stale")?;
        return Ok(Answered { status: "stale", run: None });
    }
    let p = Promote {
        file: row.file.clone(),
        audience: Some(row.audience.clone()),
        room: Some(row.room.clone()),
        projects: Some(retag.projects.clone()),
        reason: row.reason.clone(),
    };
    if judge(&current, &p, &roster()).is_ok_and(|j| j == Judgement::Nothing) {
        decide("stale")?;
        return Ok(Answered { status: "stale", run: None });
    }
    let who = if by.is_empty() { "owner" } else { by };
    // The row is `approved` before the file says so: the kernel trusts the row, and a write that fails leaves it
    // pending again. With no key to sign it, the approval would grant nothing: it stays waiting.
    approval_key(conn, true).ok_or_else(|| anyhow!("no key to sign the approval with"))?;
    decide("approved")?;
    sign(conn, id)?;
    // The note must still be what the approval was judged on.
    if read(&target.path).as_deref() != Some(text.as_str()) {
        conn.execute("UPDATE hq_promotions SET status = 'stale', sig = NULL WHERE id = ?1", [id])?;
        return Ok(Answered { status: "stale", run: None });
    }
    let written = match write(conn, &target, Who::Tidy(agent), &retagged(&text, &retag, id, who))? {
        Ok(w) => w,
        Err(refusal) => {
            conn.execute(
                "UPDATE hq_promotions SET status = 'pending', decided_by = NULL, decided_at = NULL, sig = NULL
                 WHERE id = ?1",
                [id],
            )?;
            bail!("{}: {}", target.label(), refusal.message());
        }
    };
    conn.execute("INSERT INTO memory_dreams (bot, at) VALUES (?1, ?2)", params![agent, now_ms()])?;
    let run = conn.last_insert_rowid();
    if let Some(v) = written.version {
        conn.execute("INSERT INTO memory_dream_files (dream, version) VALUES (?1, ?2)", params![run, v.id])?;
    }
    conn.execute(
        "INSERT INTO hq_dream_changes (dream, kind, file, shared, text, was) VALUES (?1, 'retagged', ?2, 0, ?3, ?4)",
        params![run, target.name, describe(&current, &retag, &row.file), row.from],
    )?;
    conn.execute("UPDATE hq_promotions SET dream = ?1 WHERE id = ?2", params![run, id])?;
    Ok(Answered { status: "approved", run: Some(run) })
}

/// Brings the local recall index up to date after notes were retagged (best effort: recall also updates it on use).
pub fn reindex() {
    let Some(kernel) = crate::kernel::current_kernel() else { return };
    let _ = std::process::Command::new(kernel.join("bin/kith-recall"))
        .args(["index", "update"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The agent's latest runs, newest first, as the apps show them.
pub fn runs(conn: &Connection, agent: &str, limit: usize) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for run in tidy::recent(conn, agent, limit)? {
        let mut stmt =
            conn.prepare("SELECT kind, file, shared, text, was FROM hq_dream_changes WHERE dream = ?1 ORDER BY rowid")?;
        let changes: Vec<Value> = stmt
            .query_map([run.id], |row| {
                Ok(json!({
                    "kind": row.get::<_, String>(0)?, "file": row.get::<_, String>(1)?,
                    "shared": row.get::<_, bool>(2)?, "text": row.get::<_, String>(3)?, "was": row.get::<_, Option<String>>(4)?,
                }))
            })?
            .collect::<std::result::Result<_, _>>()?;
        out.push(json!({"id": run.id, "at": run.at, "undone": run.undone, "files": run.files, "changes": changes}));
    }
    Ok(out)
}

/// What an undo did.
#[derive(Debug, Default)]
pub struct Report {
    pub restored: Vec<String>,
    /// Files left as they are, with why.
    pub refused: Vec<(String, String)>,
    /// Every file is back (the run is marked undone).
    pub undone: bool,
}

/// Puts back the files a run wrote. A file that changed after the run (the agent's turn, the user, a later run) is
/// left as it is and named with the reason; the run counts as undone once all of its files are back.
pub fn undo(conn: &Connection, layout: &Layout, agent: &str, id: i64) -> Result<Report> {
    let run = tidy::get(conn, id)?.filter(|d| d.bot == agent).ok_or_else(|| anyhow!("unknown tidy"))?;
    let mut out = Report { undone: run.undone, ..Report::default() };
    let mut back: Vec<(String, bool)> = Vec::new();
    if run.undone {
        return Ok(out);
    }
    for file in &run.files {
        let label = if file.shared { file.name.clone() } else { format!("{agent}/{}", file.name) };
        let Some(target) = target_of(layout, agent, file.shared, &file.name) else {
            out.refused.push((label, "the file is no longer here".into()));
            continue;
        };
        // An edit made since the run, outside the hook, becomes a version before we compare.
        sync(conn, &target)?;
        let versions = files::versions(conn, target.bot.as_deref(), &target.name, usize::MAX)?;
        let Some(at) = versions.iter().position(|v| v.id == file.version) else {
            out.refused.push((label, "its history no longer holds this version".into()));
            continue;
        };
        let before = versions.get(at + 1).and_then(|v| v.content.clone());
        let now = read(&target.path);
        if now == before {
            // Already back, by hand or by an earlier undo.
            back.push((file.name.clone(), file.shared));
            out.restored.push(label);
            continue;
        }
        if at != 0 {
            let newest = &versions[0];
            let who = match newest.author {
                files::Author::User => "you".to_owned(),
                files::Author::Bot => newest.author_bot.clone().unwrap_or_else(|| "the agent".into()),
                files::Author::Tidy => "a later tidy".into(),
                files::Author::Computer => "something outside kith".into(),
            };
            out.refused.push((label, format!("changed after the tidy (by {who}); restore a version by hand")));
            continue;
        }
        put(&target.path, before.as_deref())?;
        record_undo(conn, &target, Who::Kai)?;
        back.push((file.name.clone(), file.shared));
        out.restored.push(label);
    }
    // What the run added is not learned again.
    let added: Vec<(String, String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT text, file, shared FROM hq_dream_changes WHERE dream = ?1 AND kind IN ('added', 'noted')",
        )?;
        stmt.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    for (text, file, shared) in added {
        if back.contains(&(file, shared)) {
            store::forbid(conn, (!shared).then_some(agent), &text, None)?;
        }
    }
    out.undone = out.refused.is_empty();
    if out.undone {
        conn.execute("UPDATE memory_dreams SET undone = 1 WHERE id = ?1", [id])?;
        // A retag's approval stops counting with the run, even if the note's text is as it was: touch the notes so
        // the recall index reads them again.
        let mut stmt = conn.prepare("SELECT file FROM hq_dream_changes WHERE dream = ?1 AND kind = 'retagged'")?;
        let names: Vec<String> = stmt.query_map([id], |row| row.get(0))?.collect::<std::result::Result<_, _>>()?;
        for name in names {
            if let Some(target) = target_of(layout, agent, false, &name)
                && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target.path)
            {
                let _ = f.set_modified(std::time::SystemTime::now());
            }
        }
    }
    if out.restored.is_empty() && !out.refused.is_empty() {
        let why: Vec<String> = out.refused.iter().map(|(f, r)| format!("{f}: {r}")).collect();
        bail!("can't undo this tidy-up. {}", why.join("; "));
    }
    Ok(out)
}
