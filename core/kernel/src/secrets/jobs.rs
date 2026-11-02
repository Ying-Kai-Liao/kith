//! An agent's jobs, read from its `capabilities.yaml`: a port of the reading half of `kernel/lib/kith_jobs.py` (`read`,
//! `doors`, `door_digest`, `effective`). The file is a small YAML subset read line by line, never a YAML parser, so the
//! quirks of the Python reader are kept: a job or door that breaks a rule is left out whole, never read in part.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The pieces a job grants, in the order `kith_jobs.PIECES` lists them.
pub const PIECES: [&str; 3] = ["domains", "helpers", "doors"];

const DOOR_KEYS: [&str; 10] = ["name", "label", "npm", "lockfile", "chrome", "tools", "bin", "args", "writes", "env"];
const TOOLS: [&str; 3] = ["node", "ffmpeg", "ffprobe"];

/// A job as `kith_jobs.read` returns it: its name and its pieces (a door as `name@digest`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub domains: Vec<String>,
    pub helpers: Vec<String>,
    pub doors: Vec<String>,
}

impl Job {
    pub fn piece(&self, kind: &str) -> &[String] {
        match kind {
            "domains" => &self.domains,
            "helpers" => &self.helpers,
            _ => &self.doors,
        }
    }
}

/// A value of the YAML subset: text, a `[a, b]` list, or a list that was never closed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum V {
    Str(String),
    List(Vec<String>),
    Open,
}

impl V {
    fn json(&self) -> Value {
        match self {
            V::Str(s) => Value::String(s.clone()),
            V::List(l) => Value::Array(l.iter().cloned().map(Value::String).collect()),
            V::Open => Value::Null,
        }
    }
}

type Item = Vec<(String, V)>;

fn field<'a>(item: &'a Item, key: &str) -> Option<&'a V> {
    item.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Python's `str.splitlines`: `\n`, `\r\n`, `\r` and the other line boundaries.
fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r'
                && let Some(&(j, '\n')) = chars.peek()
            {
                chars.next();
                start = j + 1;
            }
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn value(raw: &str) -> V {
    // `re.sub(r'\s+#.*$', '', v)`: the first whitespace run that is followed by a `#` starts a comment
    let mut cut = raw.len();
    let mut run_start: Option<usize> = None;
    for (i, c) in raw.char_indices() {
        if c.is_whitespace() {
            run_start.get_or_insert(i);
        } else {
            if c == '#'
                && let Some(s) = run_start
            {
                cut = s;
                break;
            }
            run_start = None;
        }
    }
    let v = raw[..cut].trim();
    if let Some(inner) = v.strip_prefix('[') {
        let Some(inner) = inner.strip_suffix(']') else {
            return V::Open;
        };
        return V::List(
            inner
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_matches(['"', '\'']).to_owned())
                .collect(),
        );
    }
    V::Str(v.trim_matches(['"', '\'']).to_owned())
}

/// The `- key: value` items of a top-level block, each a list of its keys (`None` for a line it can't read).
fn block(text: &str, key: &str) -> Vec<Option<Item>> {
    // `^key:[ \t]*(?:#.*)?\n` then the lines that are indented, a comment, or blank
    let lines: Vec<&str> = text.split('\n').collect();
    let blank = |l: &str| l.chars().all(|c| c == ' ' || c == '\t');
    let Some(head) = lines.iter().enumerate().position(|(i, l)| {
        i + 1 < lines.len()
            && l.strip_prefix(key)
                .and_then(|r| r.strip_prefix(':'))
                .is_some_and(|r| {
                    let r = r.trim_start_matches([' ', '\t']);
                    r.is_empty() || r.starts_with('#')
                })
    }) else {
        return Vec::new();
    };
    let mut body = String::new();
    for l in &lines[head + 1..] {
        let indented = l.starts_with([' ', '\t']);
        let comment = l.trim_start_matches([' ', '\t']).starts_with('#');
        if !(indented || comment || blank(l)) {
            break;
        }
        body.push_str(l);
        body.push('\n');
    }
    let mut items: Vec<Option<Item>> = Vec::new();
    for line in splitlines(&body) {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        // `\s*-\s+(.*)`: a new item
        let t = line.trim_start();
        let dash = t
            .strip_prefix('-')
            .filter(|r| r.chars().next().is_some_and(char::is_whitespace))
            .map(str::trim_start);
        let body_text = dash.unwrap_or(stripped);
        if dash.is_some() || items.is_empty() {
            items.push(Some(Vec::new()));
        }
        // `([a-z_]+):\s*(.*)`
        let kv = body_text
            .split_once(':')
            .filter(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        let last = items.last_mut().expect("an item was just pushed");
        let bad = last.as_ref().is_some_and(|item| match kv {
            Some((k, _)) => item.iter().any(|(have, _)| have == k),
            None => true,
        });
        if bad {
            *last = None;
        }
        if let (Some(item), Some((k, v))) = (last.as_mut(), kv) {
            item.push((k.to_owned(), value(v.trim_start())));
        }
    }
    items
}

/// `^key:\s*(\[[^\]\n]*\])` (multi-line): the inline list after `key:`, or nothing.
fn inline(text: &str, key: &str) -> Vec<String> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if !line.strip_prefix(key).is_some_and(|r| r.starts_with(':')) {
            continue;
        }
        let after = text[start + key.len() + 1..].trim_start();
        if let Some(open) = after.strip_prefix('[')
            && let Some(e) = open.find([']', '\n'])
            && open.as_bytes()[e] == b']'
        {
            return match value(&after[..e + 2]) {
                V::List(l) => l,
                _ => Vec::new(),
            };
        }
    }
    Vec::new()
}

/// The agent-name rule (`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`), for job, door and helper names.
pub fn agent_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 64
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `kith_guard.valid_domain`: a plain host name (no wildcard, address or port).
pub fn valid_domain(d: &str) -> bool {
    let d = d.strip_suffix('\n').unwrap_or(d);
    if d.is_empty() || d.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = d.split('.').collect();
    let Some((last, init)) = labels.split_last() else {
        return false;
    };
    if init.is_empty() {
        return false;
    }
    let label = |l: &str, first: fn(char) -> bool| {
        let b: Vec<char> = l.chars().collect();
        !b.is_empty()
            && b.len() <= 63
            && first(b[0])
            && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
            && (b.len() == 1 || b[b.len() - 1] != '-')
    };
    init.iter().all(|l| label(l, |c| c.is_ascii_lowercase() || c.is_ascii_digit())) && label(last, |c| c.is_ascii_lowercase())
}

fn relative(p: Option<&V>) -> bool {
    match p {
        Some(V::Str(p)) => !p.is_empty() && !p.starts_with('/') && p.split('/').all(|x| !matches!(x, "" | "." | "..")),
        _ => false,
    }
}

fn list<'a>(item: &'a Item, key: &str) -> Option<&'a [String]> {
    match field(item, key) {
        None => Some(&[]),
        Some(V::List(l)) => Some(l),
        Some(_) => None,
    }
}

fn npm_spec(s: &str) -> bool {
    let lower = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let namechar = |c: char| lower(c) || matches!(c, '.' | '_' | '-');
    let name = |n: &str| n.chars().next().is_some_and(lower) && n.chars().all(namechar);
    let Some((pkg, ver)) = s.rsplit_once('@').filter(|(p, _)| !p.is_empty()) else {
        return false;
    };
    let ok_pkg = match pkg.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').is_some_and(|(scope, n)| name(scope) && name(n)),
        None => name(pkg),
    };
    let (core, pre) = match ver.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (ver, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    ok_pkg
        && nums.len() == 3
        && nums.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        && pre.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
}

fn env_value(v: &str) -> bool {
    matches!(v, "$chrome" | "$node" | "$ffmpeg" | "$ffprobe")
        || (v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
}

fn env_name(k: &str) -> bool {
    let mut cs = k.chars();
    cs.next().is_some_and(|c| c.is_ascii_uppercase())
        && k.len() <= 64
        && cs.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn env_kept(k: &str) -> bool {
    matches!(k, "PATH" | "HOME" | "TMPDIR" | "LANG" | "SHELL" | "USER" | "NODE_OPTIONS" | "NODE_PATH" | "NODE_EXTRA_CA_CERTS")
        || ["KITH_", "DYLD_", "LD_", "npm_"].iter().any(|p| k.starts_with(p))
}

fn door_ok(d: &Item, base: &Path) -> bool {
    if d.iter().any(|(k, _)| !DOOR_KEYS.contains(&k.as_str())) {
        return false;
    }
    if !matches!(field(d, "name"), Some(V::Str(n)) if agent_name(n)) {
        return false;
    }
    let (Some(npm), Some(tools), Some(_), Some(writes), Some(env)) =
        (list(d, "npm"), list(d, "tools"), list(d, "args"), list(d, "writes"), list(d, "env"))
    else {
        return false;
    };
    if npm.is_empty() || !npm.iter().all(|s| npm_spec(s)) {
        return false;
    }
    if !lockfile_ok(d, base) {
        return false;
    }
    if let Some(chrome) = field(d, "chrome") {
        let ok = matches!(chrome, V::Str(c) if {
            let parts: Vec<&str> = c.split('.').collect();
            parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        });
        if !ok {
            return false;
        }
    }
    if !tools.iter().any(|t| t == "node") || tools.iter().any(|t| !TOOLS.contains(&t.as_str())) {
        return false;
    }
    if !relative(field(d, "bin")) {
        return false;
    }
    if writes.is_empty() || !writes.iter().all(|w| relative(Some(&V::Str(w.clone())))) {
        return false;
    }
    for e in env {
        let Some((k, v)) = e.split_once('=') else {
            return false;
        };
        if !env_name(k) || env_kept(k) || !env_value(v) {
            return false;
        }
        let rest: String = v.chars().skip(1).collect();
        if (v == "$chrome" && field(d, "chrome").is_none()) || (TOOLS.contains(&rest.as_str()) && !tools.contains(&rest)) {
            return false;
        }
    }
    true
}

fn lockfile_ok(d: &Item, base: &Path) -> bool {
    if !relative(field(d, "lockfile")) {
        return false;
    }
    let Some(V::Str(l)) = field(d, "lockfile") else {
        return false;
    };
    let path = base.join(l);
    let is_file = path.metadata().is_ok_and(|m| m.is_file());
    let is_link = path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink());
    let inside = std::fs::canonicalize(&path)
        .ok()
        .zip(std::fs::canonicalize(base).ok())
        .is_some_and(|(p, b)| p.to_string_lossy().starts_with(&format!("{}/", b.to_string_lossy())));
    is_file && !is_link && inside
}

/// `json.dumps({k: v for k in DOOR_KEYS}, sort_keys=True)`, then the lockfile's bytes: the digest a tick names.
fn door_digest(d: &Item, lock: &[u8]) -> String {
    let mut kept: Vec<(&str, Value)> = d
        .iter()
        .filter(|(k, _)| DOOR_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.as_str(), v.json()))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(b.0));
    let obj: serde_json::Map<String, Value> = kept.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    let mut hasher = Sha256::new();
    hasher.update(super::pyjson::dumps(&Value::Object(obj)).as_bytes());
    hasher.update([0u8]);
    hasher.update(lock);
    let hex: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    hex[..12].to_owned()
}

/// `{name: digest}` of the valid doors of a capabilities.yaml, in file order.
fn doors(caps: &Path, text: &str) -> Vec<(String, String)> {
    let base: PathBuf = std::fs::canonicalize(caps)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| caps.parent().map(Path::to_path_buf).unwrap_or_default());
    let mut out: Vec<(String, String)> = Vec::new();
    for item in block(text, "doors") {
        let Some(d) = item.filter(|d| !d.is_empty()) else {
            continue;
        };
        let Some(V::Str(name)) = field(&d, "name").cloned() else {
            continue;
        };
        if out.iter().any(|(n, _)| *n == name) || !door_ok(&d, &base) {
            continue;
        }
        let Some(V::Str(l)) = field(&d, "lockfile") else {
            continue;
        };
        let Ok(lock) = std::fs::canonicalize(base.join(l)).and_then(std::fs::read) else {
            continue;
        };
        out.push((name, door_digest(&d, &lock)));
    }
    out
}

/// The valid jobs of a capabilities.yaml in package order (`kith_jobs.read(caps)[0]`).
pub fn read(caps: &Path) -> Vec<Job> {
    let text = if caps.is_file() { String::from_utf8_lossy(&std::fs::read(caps).unwrap_or_default()).into_owned() } else { String::new() };
    let helpers = inline(&text, "helpers");
    let doors = doors(caps, &text);
    let mut jobs: Vec<Job> = Vec::new();
    for j in block(&text, "jobs") {
        let Some(j) = j.filter(|j| !j.is_empty()) else {
            continue;
        };
        if j.iter().any(|(k, _)| !matches!(k.as_str(), "name" | "label" | "domains" | "helpers" | "doors")) {
            continue;
        }
        let Some(V::Str(name)) = field(&j, "name").cloned() else {
            continue;
        };
        if !agent_name(&name) || jobs.iter().any(|x| x.name == name) {
            continue;
        }
        if matches!(field(&j, "label"), Some(V::List(_) | V::Open)) {
            continue;
        }
        let (Some(domains), Some(hs), Some(ds)) = (list(&j, "domains"), list(&j, "helpers"), list(&j, "doors")) else {
            continue;
        };
        if !domains.iter().all(|d| valid_domain(d))
            || !hs.iter().all(|h| agent_name(h) && helpers.contains(h))
            || !ds.iter().all(|d| doors.iter().any(|(n, _)| n == d))
        {
            continue;
        }
        let dedupe = |v: &[String]| {
            let mut out: Vec<String> = Vec::new();
            for x in v {
                if !out.contains(x) {
                    out.push(x.clone());
                }
            }
            out
        };
        jobs.push(Job {
            name,
            domains: dedupe(domains),
            helpers: dedupe(hs),
            doors: dedupe(ds)
                .into_iter()
                .map(|d| {
                    let digest = doors.iter().find(|(n, _)| *n == d).map(|(_, g)| g.clone()).unwrap_or_default();
                    format!("{d}@{digest}")
                })
                .collect(),
        });
    }
    jobs
}

/// What a tick grants: its snapshot, cut to what the package still declares (nothing when the job is gone).
pub fn effective(tick: &Value, job: Option<&Job>) -> [Vec<Value>; 3] {
    let have = tick.get("snapshot").filter(|s| s.is_object());
    PIECES.map(|k| {
        let Some(job) = job else {
            return Vec::new();
        };
        have.and_then(|h| h.get(k))
            .and_then(Value::as_array)
            .map(|vs| {
                vs.iter()
                    .filter(|v| v.as_str().is_some_and(|s| job.piece(k).iter().any(|p| p == s)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    })
}
