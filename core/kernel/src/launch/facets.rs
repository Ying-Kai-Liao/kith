//! The facets of a memory note and the load filter at a run's start: a port of `kernel/lib/kith_facets.py`, which stays
//! the source of truth (docs/memory-context.md is its design). Pure: every path, the reader, the room and the
//! environment are arguments, so the privacy rules can be tested in a tempdir.
//!
//! One deliberate gap: a note whose frontmatter names an `approved_change` is widened in Python only when the host's
//! signed promotion row (`<state>/memory.db`, `hq_promotions`) says so. This crate has no SQLite and no HMAC, so it
//! never finds a grant: such a note keeps its folder's audience. That is the narrow reading, so it can only hide a note
//! a guest could have seen, never show one.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

pub const LOAD_CAP: usize = 8000;
const INDEX_NAMES: [&str; 3] = ["README.md", "MEMORY.md", "index.md"];
const WITHHELD: &str = "Some of what you know is kept out of this room because not everyone here may see it. When it would help the person asking, you may say \"ask me in our 1:1\"; never say what it is.";
pub const DECISIONS_TAIL: usize = 30;
const OWN_OPEN_MAX: usize = 10;
const START_CAP: usize = 16000;
const KINDS: [&str; 5] = ["fact", "preference", "lesson", "decision", "project-detail"];
const CHANNEL_VIAS: [&str; 3] = ["telegram", "slack", "discord"];

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
pub fn is_id(s: &str) -> bool {
    let mut it = s.chars();
    s.chars().count() <= 64
        && it.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && it.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Python's `str.strip()` with no argument.
fn strip(s: &str) -> &str {
    s.trim()
}

/// The `key: value` lines of a leading `---` block (keys lowercased, values unquoted), and the text after it.
pub fn split_frontmatter(text: &str) -> (HashMap<String, String>, String) {
    let lines: Vec<&str> = text.split('\n').collect();
    if strip(lines[0]) != "---" {
        return (HashMap::new(), text.to_owned());
    }
    for end in 1..lines.len().min(60) {
        if strip(lines[end]) == "---" {
            let mut fm = HashMap::new();
            for line in &lines[1..end] {
                if let Some((k, v)) = line.split_once(':') {
                    let k = strip(k);
                    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
                        fm.insert(
                            k.to_lowercase(),
                            strip(v).trim_matches(|c| c == '"' || c == '\'').to_owned(),
                        );
                    }
                }
            }
            return (fm, lines[end + 1..].join("\n"));
        }
    }
    (HashMap::new(), text.to_owned())
}

fn valid_audience(a: &str) -> bool {
    let (kind, rest) = a.split_once(':').unwrap_or((a, ""));
    a == "owner" || a == "shared" || ((kind == "person" || kind == "room") && is_id(rest))
}

fn projects_of(v: &str) -> Vec<String> {
    v.trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|p| strip(p).trim_matches(|c| c == '"' || c == '\'').to_owned())
        .filter(|p| !p.is_empty() && is_id(p))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facets {
    pub audience: String,
    pub room: String,
    pub projects: Vec<String>,
    pub kind: String,
    pub thread: String,
}

impl Facets {
    /// A note with no facets, or facets read wrongly: owner-only and project-less, so old data never widens.
    pub fn owner_only() -> Self {
        Facets {
            audience: "owner".into(),
            room: String::new(),
            projects: vec![],
            kind: "fact".into(),
            thread: String::new(),
        }
    }
}

/// Python's `Path(x).stem`.
fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// Default facets for `rel`, the path under an agent's `memory/`.
pub fn folder_defaults(rel: &str, agent: &str) -> Facets {
    let mut f = Facets::owner_only();
    let parts: Vec<&str> = rel.split('/').collect();
    let id1 = parts.len() > 2 && is_id(parts[1]);
    match parts[0] {
        "shared" => f.audience = "shared".into(),
        "people" if id1 => {
            let pid = parts[1].to_lowercase();
            f.audience = format!("person:{pid}");
            f.room = format!("1:1:{agent}:{pid}");
            if parts[2] == "sessions" {
                f.kind = "project-detail".into();
                f.thread = stem(parts[parts.len() - 1]).to_owned();
            }
        }
        "rooms" if id1 => {
            f.audience = format!("room:{}", parts[1]);
            f.room = parts[1].to_owned();
        }
        "projects" if id1 => f.projects = vec![parts[1].to_owned()],
        "projects"
            if parts.len() == 2
                && parts[1].ends_with(".md")
                && is_id(&parts[1][..parts[1].len() - 3]) =>
        {
            f.projects = vec![parts[1][..parts[1].len() - 3].to_owned()];
        }
        "sessions" if parts.len() == 2 => {
            f.kind = "project-detail".into();
            f.thread = stem(parts[1]).to_owned();
        }
        _ => {}
    }
    f
}

/// Frontmatter over the folder default. The audience may only stay or narrow to owner: a note never widens by what its
/// own text says, because agents write these files. (No promotion grants here: see the module comment.)
pub fn apply_frontmatter(defaults: &Facets, fm: &HashMap<String, String>) -> Facets {
    let mut f = defaults.clone();
    let a = fm.get("audience").map_or("", String::as_str);
    if !a.is_empty() && valid_audience(a) && (a == defaults.audience || a == "owner") {
        f.audience = a.to_owned();
    }
    if let Some(r) = fm
        .get("room")
        .filter(|r| !r.is_empty() && is_id(&r.replace(':', "-")))
    {
        f.room.clone_from(r);
    }
    if let Some(p) = fm.get("projects") {
        f.projects = projects_of(p);
    }
    if let Some(k) = fm.get("kind").filter(|k| KINDS.contains(&k.as_str())) {
        f.kind.clone_from(k);
    }
    if let Some(t) = fm.get("thread").filter(|t| !t.is_empty() && is_id(t)) {
        f.thread.clone_from(t);
    }
    f
}

pub fn facets_for(rel: &str, agent: &str, text: &str) -> Facets {
    apply_frontmatter(&folder_defaults(rel, agent), &split_frontmatter(text).0)
}

/// user.md (owner) and people/<id>.md: not an agent's.
fn state_file_facets(owner: bool, ident: &str, text: &str) -> Facets {
    let mut f = Facets::owner_only();
    if !owner && !ident.is_empty() && is_id(ident) {
        f.audience = format!("person:{ident}");
    }
    apply_frontmatter(&f, &split_frontmatter(text).0)
}

// ---- who reads, and in which room

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Owner,
    Person,
    Guest,
}

#[derive(Clone, Debug)]
pub struct Reader {
    pub kind: Kind,
    pub id: String,
    pub rooms: Vec<String>,
}

impl Reader {
    fn who(&self) -> &str {
        if self.kind == Kind::Owner {
            "owner"
        } else {
            &self.id
        }
    }
}

pub fn own_room(agent: &str, reader: &Reader) -> String {
    format!("1:1:{agent}:{}", reader.who())
}

/// `1:1:<agent>:<person>` whose person is the reader (the agent part is not compared: a helper inherits its caller's room).
pub fn is_one_to_one(room: &str, reader: &Reader) -> bool {
    room.starts_with("1:1:") && room.rsplit(':').next() == Some(reader.who())
}

/// Anything but the reader's own 1:1 is read by more than one person, or is a room we cannot vouch for: fail closed.
pub fn is_group_room(room: &str, reader: &Reader) -> bool {
    !room.is_empty() && !is_one_to_one(room, reader)
}

/// The audience values one reader may see.
pub fn audience_set(reader: &Reader, room: &str) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = match reader.kind {
        Kind::Owner => ["owner", "shared"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        Kind::Person => ["shared".to_owned(), format!("person:{}", reader.id)].into(),
        Kind::Guest => ["shared".to_owned()].into(),
    };
    let mut rooms: BTreeSet<&str> = reader.rooms.iter().map(String::as_str).collect();
    if !room.is_empty() && reader.kind == Kind::Owner && !room.starts_with("1:1:") {
        rooms.insert(room);
    }
    set.extend(rooms.into_iter().map(|r| format!("room:{r}")));
    set
}

/// Audience values loaded for `reader` in `room`: a 1:1 loads the reader's whole set; a group room is read by everyone
/// there, so the intersection of the members' sets, or `shared` while the members are not known.
pub fn load_audiences(reader: &Reader, room: &str, members: Option<&[Reader]>) -> BTreeSet<String> {
    let mine = audience_set(reader, room);
    if reader.kind == Kind::Guest || !is_group_room(room, reader) {
        return mine;
    }
    let Some(members) = members else {
        return ["shared".to_owned()].into();
    };
    members.iter().fold(mine, |acc, m| {
        let theirs = audience_set(m, room);
        acc.intersection(&theirs).cloned().collect()
    })
}

/// The filter: audience visible to everyone present, room empty or this room, projects empty or one of the current
/// ones, and never project-detail (that is recall's).
pub fn loads(f: &Facets, audiences: &BTreeSet<String>, room: &str, projects: &[String]) -> bool {
    f.kind != "project-detail"
        && audiences.contains(&f.audience)
        && (f.room.is_empty() || f.room == room)
        && (f.projects.is_empty() || f.projects.iter().any(|p| projects.contains(p)))
}

// ---- reading notes

/// The text of a regular file (links are not followed), lossy like Python's `errors='replace'`.
fn read(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn mtime(path: &Path) -> f64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64())
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            walk_md(&p, out);
        }
        if p.file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".md"))
        {
            out.push(p);
        }
    }
}

struct Note {
    rel: String,
    text: String,
    mtime: f64,
    facets: Facets,
}

fn rel_of(mem: &Path, p: &Path) -> String {
    p.strip_prefix(mem)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// The `.md` files under `mem/folder`; links are not followed.
fn notes(mem: &Path, folder: &str, agent: &str) -> Vec<Note> {
    let d = mem.join(folder);
    if fs::symlink_metadata(&d).is_ok_and(|m| m.file_type().is_symlink()) || !d.is_dir() {
        return vec![];
    }
    let mut files = vec![];
    walk_md(&d, &mut files);
    files.sort();
    files
        .iter()
        .filter_map(|p| {
            let text = read(p)?;
            (!strip(&text).is_empty()).then(|| {
                let rel = rel_of(mem, p);
                let facets = facets_for(&rel, agent, &text);
                Note {
                    rel,
                    text,
                    mtime: mtime(p),
                    facets,
                }
            })
        })
        .collect()
}

/// Notes of project `p` in either layout (projects/<p>/ folder, projects/<p>.md file), index first, then newest.
fn project_notes(mem: &Path, p: &str, agent: &str) -> Vec<Note> {
    let mut v = notes(mem, &format!("projects/{p}"), agent);
    let rel = format!("projects/{p}.md");
    if let Some(text) = read(&mem.join(&rel)).filter(|t| !strip(t).is_empty()) {
        let facets = facets_for(&rel, agent, &text);
        v.push(Note {
            mtime: mtime(&mem.join(&rel)),
            rel,
            text,
            facets,
        });
    }
    v.sort_by(|a, b| {
        let key = |n: &Note| !INDEX_NAMES.contains(&n.rel.rsplit('/').next().unwrap_or(""));
        key(a).cmp(&key(b)).then(b.mtime.total_cmp(&a.mtime))
    });
    v
}

fn tag(path: &str, text: &str, scope: &str) -> String {
    let text = text.replace("</long-term-memory>", "<\u{200b}/long-term-memory>");
    let scope = if scope.is_empty() {
        String::new()
    } else {
        format!(" scope=\"{scope}\"")
    };
    format!(
        "<long-term-memory path=\"{path}\"{scope}>\n{}\n</long-term-memory>",
        text.trim_end_matches('\n')
    )
}

// ---- the room-note ledger

/// The members of this run's group room from the environment: `None` = unknown (shared only), `Some([])` = nobody but
/// the owner, else the person ids. The owner-only marker counts for an owner run only, never beside a member list, and
/// never in a channel run.
pub fn env_members(kind: Kind, env: &dyn Fn(&str) -> Option<String>) -> Option<Vec<String>> {
    let raw = env("KITH_ROOM_MEMBERS").unwrap_or_default();
    let ids: Vec<String> = raw
        .split(',')
        .map(|x| strip(x).to_lowercase())
        .filter(|m| is_id(m))
        .collect();
    if !ids.is_empty() {
        return Some(ids);
    }
    let via = env("KITH_VIA").unwrap_or_default();
    (kind == Kind::Owner
        && strip(&raw).is_empty()
        && env("KITH_ROOM_OWNER_ONLY").as_deref() == Some("1")
        && !CHANNEL_VIAS.contains(&via.as_str()))
    .then(Vec::new)
}

/// `{note path under memory/: [person ids]}`; an unreadable ledger is an empty one, so every note is owner-only.
pub fn read_ledger(state: &Path, agent: &str, room: &str) -> HashMap<String, Vec<String>> {
    if !(is_id(agent) && is_id(room)) {
        return HashMap::new();
    }
    let f = state
        .join("room-ledger")
        .join(agent)
        .join(format!("{room}.json"));
    let Some(serde_json::Value::Object(d)) = fs::read_to_string(f)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    else {
        return HashMap::new();
    };
    d.into_iter()
        .filter_map(|(k, v)| {
            let a = v.as_array()?;
            Some((
                k,
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
            ))
        })
        .collect()
}

fn room_note_ok(ledger: &HashMap<String, Vec<String>>, rel: &str, present: &[String]) -> bool {
    let was = ledger.get(rel);
    present.iter().all(|p| was.is_some_and(|w| w.contains(p)))
}

// ---- the load

pub struct Loaded {
    pub block: String,
    pub loaded: usize,
    pub more: usize,
    pub group: bool,
}

struct Cand {
    path: String,
    text: String,
    mtime: f64,
    facets: Facets,
    scope: String,
    protected: bool,
}

pub struct LoadArgs<'a> {
    pub agent: &'a str,
    pub home: &'a Path,
    pub person_file: &'a Path,
    pub reader: &'a Reader,
    pub room: &'a str,
    pub projects: &'a [String],
    pub cap: usize,
    pub members: Option<&'a [Reader]>,
    pub ledger: Option<&'a HashMap<String, Vec<String>>>,
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The tier-1 block for a run: about you, the agent's memory with the person, room notes (newest first), project notes
/// (index first, then newest). Over `cap` characters the oldest room and project notes go first; about you and the
/// person's memory never do.
pub fn load_memory(a: &LoadArgs<'_>) -> Loaded {
    let mem = a.home.join("memory");
    let (agent, reader) = (a.agent, a.reader);
    let who = reader.who().to_owned();
    let mut room = a.room.to_owned();
    if is_one_to_one(&room, reader) {
        room = own_room(agent, reader);
    }
    let group = reader.kind != Kind::Guest && is_group_room(&room, reader);
    let aud = load_audiences(reader, &room, a.members);
    let solo = audience_set(reader, "");
    let solo_room = own_room(agent, reader);
    let owner = reader.kind == Kind::Owner;
    let mut cands: Vec<Cand> = vec![];
    if let Some(t) = read(a.person_file).filter(|t| !strip(t).is_empty()) {
        let f = state_file_facets(owner, if owner { "" } else { &who }, &t);
        cands.push(Cand {
            path: path_str(a.person_file),
            text: t,
            mtime: 0.0,
            facets: f,
            scope: "about".into(),
            protected: true,
        });
    }
    let own = if owner {
        ("MEMORY.md".to_owned(), String::new())
    } else {
        (format!("people/{who}/MEMORY.md"), who.clone())
    };
    let shared = ("shared/MEMORY.md".to_owned(), "shared".to_owned());
    for (rel, scope) in if owner { [own, shared] } else { [shared, own] } {
        let t = read(&mem.join(&rel)).unwrap_or_default();
        if owner && scope == "shared" && strip(&t).is_empty() {
            continue; // the owner sees the shared part only when there is one
        }
        let facets = facets_for(&rel, agent, &t);
        cands.push(Cand {
            path: path_str(&mem.join(&rel)),
            text: t,
            mtime: 0.0,
            facets,
            scope,
            protected: true,
        });
    }
    if !room.is_empty() && is_id(&room) {
        let ids: Option<Vec<String>> = a.members.map(|m| m.iter().map(|r| r.id.clone()).collect());
        let mut rn = notes(&mem, &format!("rooms/{room}"), agent);
        rn.sort_by(|x, y| y.mtime.total_cmp(&x.mtime));
        for n in rn {
            if group
                && let Some(ledger) = a.ledger
                && n.facets.audience == format!("room:{room}")
                && !ids
                    .as_ref()
                    .is_some_and(|ids| room_note_ok(ledger, &n.rel, ids))
            {
                continue; // not everyone here was there when it was written
            }
            cands.push(Cand {
                path: path_str(&mem.join(&n.rel)),
                text: n.text,
                mtime: n.mtime,
                facets: n.facets,
                scope: format!("room:{room}"),
                protected: false,
            });
        }
    }
    for p in a.projects.iter().filter(|p| is_id(p)) {
        for n in project_notes(&mem, p, agent) {
            cands.push(Cand {
                path: path_str(&mem.join(&n.rel)),
                text: n.text,
                mtime: n.mtime,
                facets: n.facets,
                scope: format!("project:{p}"),
                protected: false,
            });
        }
    }
    let (mut shown, mut withheld) = (vec![], false);
    for c in cands {
        if loads(&c.facets, &aud, &room, a.projects) {
            shown.push(c);
        } else if group
            && !strip(&c.text).is_empty()
            && loads(
                &c.facets,
                &solo,
                if c.facets.room.is_empty() {
                    &room
                } else {
                    &solo_room
                },
                a.projects,
            )
        {
            withheld = true;
        }
    }
    let mut more = 0;
    let mut oldest: Vec<usize> = (0..shown.len()).filter(|&i| !shown[i].protected).collect();
    oldest.sort_by(|&x, &y| shown[x].mtime.total_cmp(&shown[y].mtime));
    let mut gone = vec![false; shown.len()];
    for i in oldest {
        let total: usize = shown
            .iter()
            .zip(&gone)
            .filter(|(_, g)| !**g)
            .map(|(c, _)| chars(&c.text))
            .sum();
        if total <= a.cap {
            break;
        }
        gone[i] = true;
        more += 1;
    }
    let kept: Vec<&Cand> = shown
        .iter()
        .zip(&gone)
        .filter(|(_, g)| !**g)
        .map(|(c, _)| c)
        .collect();
    let mut block = kept
        .iter()
        .map(|c| tag(&c.path, &c.text, &c.scope))
        .collect::<Vec<_>>()
        .join("\n");
    if withheld {
        block.push('\n');
        block.push_str(WITHHELD);
    }
    Loaded {
        block,
        loaded: kept.len(),
        more,
        group,
    }
}

fn cut(text: &str, cap: usize) -> String {
    if chars(text) <= cap {
        return text.to_owned();
    }
    let head: String = text.chars().take(cap).collect();
    format!("{head}\n[cut at {cap} characters: read the file for the rest]")
}

/// `\d{4}-\d\d-\d\d · AGENT · open:` at the start of a line.
fn is_open_line(line: &str, agent: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 10
        && b[..10].iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
        && line[10..].starts_with(&format!(" \u{b7} {agent} \u{b7} open:"))
}

/// The start files of `project` after the notes: where the reader's last call on it left off (sessions/<ID>.md, by the
/// same audience and room rule as a note), its codebase map, and its decisions: the last `DECISIONS_TAIL` lines plus
/// this agent's own open lines from before them.
pub fn start_files(
    agent: &str,
    home: &Path,
    reader: &Reader,
    room: &str,
    project: &str,
    members: Option<&[Reader]>,
    state: &Path,
) -> String {
    if project.is_empty() || !is_id(project) {
        return String::new();
    }
    let room = if is_one_to_one(room, reader) {
        own_room(agent, reader)
    } else {
        room.to_owned()
    };
    let aud = load_audiences(reader, &room, members);
    let mem = home.join("memory");
    let mut out = vec![];
    let rel = if reader.kind == Kind::Owner {
        format!("sessions/{project}.md")
    } else {
        format!("people/{}/sessions/{project}.md", reader.id)
    };
    let t = read(&mem.join(&rel));
    let f = facets_for(&rel, agent, t.as_deref().unwrap_or(""));
    if let Some(t) = t.filter(|t| !strip(t).is_empty())
        && aud.contains(&f.audience)
        && (f.room.is_empty() || f.room == room)
    {
        out.push(tag(
            &path_str(&mem.join(&rel)),
            &cut(&t, START_CAP),
            &format!("session:{project}"),
        ));
    }
    let d = state.join("projects").join(project);
    if let Some(t) = read(&d.join("codebase.md")).filter(|t| !strip(t).is_empty()) {
        out.push(tag(
            &path_str(&d.join("codebase.md")),
            &cut(&t, START_CAP),
            &format!("project:{project}"),
        ));
    }
    if let Some(t) = read(&d.join("decisions.md")).filter(|t| !strip(t).is_empty()) {
        let lines: Vec<&str> = t.split('\n').filter(|x| !strip(x).is_empty()).collect();
        let (before, tail) = lines.split_at(lines.len().saturating_sub(DECISIONS_TAIL));
        let opens: Vec<&str> = before
            .iter()
            .copied()
            .filter(|x| is_open_line(x, agent))
            .collect();
        let mut text = String::new();
        if !opens.is_empty() {
            let shown = &opens[opens.len().saturating_sub(OWN_OPEN_MAX)..];
            let newest = if opens.len() > OWN_OPEN_MAX {
                format!(", the newest {OWN_OPEN_MAX} here")
            } else {
                String::new()
            };
            text = format!(
                "Your own open lines older than the last {DECISIONS_TAIL} ({}{newest}; all: grep '\u{b7} {agent} \u{b7} open:'):\n{}\n\n",
                opens.len(),
                shown.join("\n")
            );
        }
        text.push_str(&format!(
            "The last {} of {} lines:\n{}",
            tail.len(),
            lines.len(),
            tail.join("\n")
        ));
        out.push(tag(
            &path_str(&d.join("decisions.md")),
            &cut(&text, 2 * START_CAP),
            &format!("project:{project}"),
        ));
    }
    out.join("\n")
}

/// What `kith_facets.py load` hands the launch: the block and the ` loaded="N"...` attributes of the `<session/>` tag.
pub struct Load {
    pub block: String,
    pub attrs: String,
}

pub struct Request<'a> {
    pub agent: &'a str,
    pub home: &'a Path,
    pub person_file: &'a Path,
    pub kind: Kind,
    pub person: &'a str,
    pub rooms: &'a str,
    pub room: &'a str,
    pub project: &'a str,
    pub agent_md: &'a Path,
    pub state: &'a Path,
}

fn room_ok(r: &str) -> bool {
    let mut it = r.chars();
    r.chars().count() <= 128
        && it.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && it.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// `kith_facets.py load`: the whole memory part of a launch prompt.
pub fn load(r: &Request<'_>, env: &dyn Fn(&str) -> Option<String>) -> Load {
    let mut cap = LOAD_CAP;
    if let Some(text) = fs::read(r.agent_md)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        && let Some(n) = split_frontmatter(&text)
            .0
            .get("memory_cap")
            .and_then(|v| v.trim().parse::<i64>().ok())
        && n > 0
    {
        cap = usize::try_from(n).unwrap_or(cap);
    }
    let mut room = if room_ok(r.room) {
        r.room.to_owned()
    } else if r.room.is_empty() {
        String::new()
    } else {
        "?".to_owned()
    };
    let mut reader = Reader {
        kind: r.kind,
        id: r.person.to_owned(),
        rooms: r
            .rooms
            .split_whitespace()
            .filter(|x| is_id(x))
            .map(str::to_owned)
            .collect(),
    };
    if room.is_empty() && r.kind != Kind::Guest {
        // A channel group chat sets no room: a group VIA without one is a group (shared only)
        room = if matches!(
            env("KITH_VIA").as_deref(),
            Some("telegram" | "slack" | "discord" | "group")
        ) {
            "?".to_owned()
        } else {
            own_room(r.agent, &reader)
        };
    }
    let ids = env_members(r.kind, env);
    let members: Option<Vec<Reader>> = ids.as_ref().map(|ids| {
        ids.iter()
            .map(|m| Reader {
                kind: Kind::Person,
                id: m.clone(),
                rooms: vec![room.clone()],
            })
            .collect()
    });
    if r.kind == Kind::Person
        && ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|i| i == r.person))
        && is_id(&room)
        && !reader.rooms.contains(&room)
    {
        reader.rooms.push(room.clone());
    }
    let ledger = if room != "?" && !room.is_empty() && is_id(&room) {
        read_ledger(r.state, r.agent, &room)
    } else {
        HashMap::new()
    };
    let projects: Vec<String> = if r.project.is_empty() {
        vec![]
    } else {
        vec![r.project.to_owned()]
    };
    let out = load_memory(&LoadArgs {
        agent: r.agent,
        home: r.home,
        person_file: r.person_file,
        reader: &reader,
        room: &room,
        projects: &projects,
        cap,
        members: members.as_deref(),
        ledger: Some(&ledger),
    });
    let attrs = format!(
        " loaded=\"{}\"{}{}",
        out.loaded,
        if out.more > 0 {
            format!(" more=\"{}\"", out.more)
        } else {
            String::new()
        },
        if out.group { " room=\"group\"" } else { "" }
    );
    let start = start_files(
        r.agent,
        r.home,
        &reader,
        &room,
        r.project,
        members.as_deref(),
        r.state,
    );
    let block = [out.block, start]
        .into_iter()
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Load { block, attrs }
}
