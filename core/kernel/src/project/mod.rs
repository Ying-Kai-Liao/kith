//! `kith kernel project-id ...`: `kernel/bin/project-id` in Rust, with its command line, output, exit codes and the
//! `projects.json` it writes (`json.dump(indent=2)`, ASCII only, keys in their order). Resolves the kith project for a
//! directory; registers one only when asked. The Python stays the source of truth while the verb is not switched.
//!
//! ```text
//! [DIR]                                   the ID whose remotes/paths match DIR (default: cwd); nothing and exit 1 when none
//! --list | --info ID | --children ID [--all] | --where ID | --root [DIR]
//! --register ID [DIR] [--in PARENT] [--home [DIR]]
//! ```
//!
//! The guest branch of the Python (a run for someone other than the owner asks the host over the run socket, which
//! decides what that run may see: `serve/host/src/runauth.rs` `project_args`) is not ported: this program refuses there,
//! so a guest run can only be answered by `kernel/bin/project-id`. Whoever switches the verb keeps that branch on Python.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::paths::realpath;
use crate::secrets::pyjson;

/// What a run printed and how it ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// What a run needs from its process: the state folder (`KITH_STATE`), `$HOME`, the working directory and whether this
/// is a guest run (`KITH_RUN_TOKEN` and `KITH_RUN_SOCKET` both set and not empty).
pub struct Ctx {
    pub state: PathBuf,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub guest: bool,
}

/// Whether the environment makes this a guest run. Takes the raw values (`var_os`): a token that is not UTF-8 still
/// counts, as it does for the Python.
pub fn is_guest(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    let set = |n: &str| get(n).is_some_and(|v| !v.is_empty());
    set("KITH_RUN_TOKEN") && set("KITH_RUN_SOCKET")
}

/// Where a new project's home goes unless projects.json says "homes".
const ROOT: &str = "~/Documents";

/// The whole file, atomically (a reader never sees half of it): a temp file beside it, flushed to disk, then the swap, and
/// on unix the directory flushed so the swap survives a crash too. `crash_before_rename` stops after the temp file is
/// written, as a crash there would (tests only). A file that is already there keeps its permissions.
fn write_atomic(file: &Path, data: &[u8], crash_before_rename: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = file.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".projects.json.{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Ok(m) = std::fs::metadata(file) {
            f.set_permissions(m.permissions())?;
        }
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if crash_before_rename {
            return Err(std::io::Error::other("crash before the rename"));
        }
        std::fs::rename(&tmp, file)?;
        // a directory cannot be opened on Windows
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The writers' lock (`projects.json.lock` in the state folder), held from reading the registry to writing it back so two
/// registers at once both land. `kernel/bin/project-id` takes no lock, so this excludes other Rust writers only.
fn lock(state: &Path) -> std::io::Result<std::fs::File> {
    use fs2::FileExt as _;
    let l = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .create(true)
        .open(state.join("projects.json.lock"))?;
    l.lock_exclusive()?;
    Ok(l)
}

impl Out {
    fn say(&mut self, line: impl std::fmt::Display) {
        let _ = writeln!(self.stdout, "{line}");
    }

    fn err(&mut self, line: impl std::fmt::Display) {
        let _ = writeln!(self.stderr, "{line}");
    }

    fn with_code(mut self, code: i32) -> Self {
        self.code = code;
        self
    }
}

/// Python's `str.strip()`: whitespace includes the file and record separators U+001C..U+001F.
fn strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn git(args: &[&str], cwd: &Path) -> String {
    match Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => strip(&String::from_utf8_lossy(&o.stdout)).to_owned(),
        _ => String::new(),
    }
}

/// `git@github.com:Owner/Repo.git`, `https://github.com/Owner/Repo` -> `github.com/owner/repo`.
pub fn normalize(url: &str) -> String {
    let mut url = strip(url);
    // re.sub(r"^(\w+://)?([^@/]+@)?", "", url)
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let scheme = url.find(|c: char| !word(c)).unwrap_or(url.len());
    if scheme > 0 && url[scheme..].starts_with("://") {
        url = &url[scheme + 3..];
    }
    if let Some(at) = url.find(['@', '/'])
        && at > 0
        && url[at..].starts_with('@')
    {
        url = &url[at + 1..];
    }
    let mut url = url.to_owned();
    // re.match(r"^[^/]+:[^/]", url): a colon after the first character, before any slash, with a non-slash behind it.
    let head = url.find('/').unwrap_or(url.len());
    let scp = url.char_indices().any(|(i, c)| {
        i > 0 && i < head && c == ':' && url[i + 1..].chars().next().is_some_and(|n| n != '/')
    });
    if scp {
        url = url.replacen(':', "/", 1);
    }
    let url = url.strip_suffix(".git").unwrap_or(&url);
    url.trim_end_matches('/').to_lowercase()
}

/// `(remote, root)` for a directory; worktrees map to their main checkout.
fn locate(d: &str) -> (String, String) {
    let dir = Path::new(d);
    let remote = git(&["remote", "get-url", "origin"], dir);
    let remote = if remote.is_empty() {
        remote
    } else {
        normalize(&remote)
    };
    let common = git(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        dir,
    );
    let root = if let Some(head) = common.strip_suffix("/.git") {
        let head = head.trim_end_matches('/');
        if head.is_empty() {
            "/".to_owned()
        } else {
            head.to_owned()
        }
    } else {
        let top = git(&["rev-parse", "--show-toplevel"], dir);
        if top.is_empty() { d.to_owned() } else { top }
    };
    (remote, root)
}

/// Python's `repr()` of a string, for the "no project" lines.
fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                let n = c as u32;
                if n < 0x100 {
                    let _ = write!(out, "\\x{n:02x}");
                } else {
                    let _ = write!(out, "\\u{n:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

struct Run<'a> {
    ctx: &'a Ctx,
    reg: Map<String, Value>,
}

/// A non-empty string field (Python's truthiness for what these files hold).
fn text<'v>(p: &'v Value, key: &str) -> Option<&'v str> {
    p.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn list(p: &Value, key: &str) -> Vec<String> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl Run<'_> {
    /// `projects`; `run` has made sure it is there.
    fn projects(&self) -> &Map<String, Value> {
        match self.reg.get("projects") {
            Some(Value::Object(m)) => m,
            _ => unreachable!("run() made the projects object"),
        }
    }

    fn projects_mut(&mut self) -> &mut Map<String, Value> {
        match self
            .reg
            .entry("projects")
            .or_insert_with(|| Value::Object(Map::new()))
        {
            Value::Object(m) => m,
            _ => unreachable!("run() checked the projects object"),
        }
    }

    fn expand(&self, path: &str) -> PathBuf {
        match path.strip_prefix('~') {
            Some("") => self.ctx.home.clone(),
            Some(rest) if rest.starts_with('/') => self.ctx.home.join(rest.trim_start_matches('/')),
            _ => PathBuf::from(path),
        }
    }

    fn real(&self, path: &str) -> String {
        realpath(&self.expand(path)).to_string_lossy().into_owned()
    }

    fn is_dir(&self, path: &str) -> bool {
        Path::new(&self.real(path)).is_dir()
    }

    /// Store paths under the home directory as `~/...`, so the registry still reads right on another Mac.
    fn tilde(&self, path: &str) -> String {
        let home = self.ctx.home.to_string_lossy();
        match path.strip_prefix(home.as_ref()) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_owned(),
        }
    }

    fn save(&self) -> Result<(), String> {
        let text = pyjson::dumps_indent(&Value::Object(self.reg.clone())) + "\n";
        write_atomic(&self.ctx.state.join("projects.json"), text.as_bytes(), false)
            .map_err(|e| format!("project-id: cannot write projects.json: {e}"))
    }

    /// `pid` and its parents, closest first; stops at a missing parent or a loop.
    fn chain(&self, pid: Option<&str>) -> Vec<String> {
        let projects = self.projects();
        let mut out: Vec<String> = Vec::new();
        let mut at = pid.map(str::to_owned);
        while let Some(id) = at {
            if !projects.contains_key(&id) || out.contains(&id) {
                break;
            }
            at = text(&projects[&id], "parent").map(str::to_owned);
            out.push(id);
        }
        out
    }

    /// The projects `pid` holds, sorted; with `deep`, theirs too, each after its parent.
    fn children(&self, pid: &str, deep: bool, seen: &mut Vec<String>) -> Vec<String> {
        let mut kids: Vec<String> = self
            .projects()
            .iter()
            .filter(|(q, p)| text(p, "parent") == Some(pid) && !seen.contains(q))
            .map(|(q, _)| q.clone())
            .collect();
        kids.sort();
        let mut out = Vec::new();
        for c in kids {
            seen.push(c.clone());
            out.push(c.clone());
            if deep {
                out.extend(self.children(&c, true, seen));
            }
        }
        out
    }

    /// Where a project is on this Mac: its home, else the first of its folders that exists.
    fn here(&self, p: &Value) -> Option<String> {
        let home = text(p, "home").map(str::to_owned);
        home.into_iter()
            .chain(list(p, "paths"))
            .find(|d| !d.is_empty() && self.is_dir(d))
    }

    /// The project for directory `d`: its git remote, else the longest path alias that holds it.
    fn matching(&self, d: &str) -> Option<String> {
        let (remote, root) = locate(d);
        let projects = self.projects();
        for (pid, p) in projects {
            if !remote.is_empty() && list(p, "remotes").contains(&remote) {
                return Some(pid.clone());
            }
        }
        let mut best: Option<&String> = None;
        let mut size = -1_i64;
        for (pid, p) in projects {
            for a in list(p, "paths") {
                let a = self.real(&a);
                let prefix = format!("{}/", a.trim_end_matches('/'));
                let len = i64::try_from(a.chars().count()).unwrap_or(i64::MAX);
                if (root == a || root.starts_with(&prefix)) && len > size {
                    best = Some(pid);
                    size = len;
                }
            }
        }
        best.cloned()
    }

    fn no_project(out: &mut Out, argv: &[String]) {
        let name = argv.get(1).map_or("", String::as_str);
        out.err(format!("project-id: no project {}", repr(name)));
    }

    fn known(&self, argv: &[String]) -> bool {
        argv.get(1)
            .is_some_and(|id| self.projects().contains_key(id))
    }

    fn register(&mut self, argv: &[String]) -> Out {
        let mut out = Out::default();
        let mut ids: Vec<&str> = Vec::new();
        let mut parent: Option<String> = None;
        // None: no --home given; Some(None): a bare --home; Some(Some(dir)).
        let mut home_opt: Option<Option<String>> = None;
        let mut rest = &argv[1..];
        while let Some((a, tail)) = rest.split_first() {
            rest = tail;
            match a.as_str() {
                "--in" => {
                    parent = Some(rest.first().cloned().unwrap_or_default());
                    rest = rest.get(1..).unwrap_or_default();
                }
                // A bare --home: the default folder for this project.
                "--home" => match rest.first() {
                    Some(d) if !d.starts_with("--") => {
                        home_opt = Some(Some(d.clone()));
                        rest = &rest[1..];
                    }
                    _ => home_opt = Some(None),
                },
                _ => ids.push(a),
            }
        }
        let valid = ids.first().is_some_and(|id| {
            let mut cs = id.chars();
            cs.next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && cs.all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
                })
        });
        if !valid {
            out.err("project-id: --register needs an ID of lowercase letters, digits, . _ -");
            return out.with_code(2);
        }
        let pid = ids[0].to_owned();
        // opts.get("--home", ""): absent is "", a bare --home is None.
        let mut home: Option<String> = home_opt.unwrap_or_else(|| Some(String::new()));
        let mut made = false;
        if home.is_none() {
            // Inside the closest parent's home, else under the root.
            let from = match parent.as_deref() {
                Some(p) if !p.is_empty() => Some(p.to_owned()),
                _ => self
                    .projects()
                    .get(&pid)
                    .and_then(|p| text(p, "parent"))
                    .map(str::to_owned),
            };
            let near = self
                .chain(from.as_deref())
                .iter()
                .find_map(|q| text(&self.projects()[q], "home").map(str::to_owned));
            let base = near
                .or_else(|| {
                    self.reg
                        .get("homes")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| ROOT.to_owned());
            let dir = format!("{}/{pid}", self.real(&base).trim_end_matches('/'));
            made = !Path::new(&dir).is_dir();
            home = Some(dir);
        } else if home.as_deref() == Some("") {
            home = None;
        }
        if let Some(par) = &parent {
            let known = self.projects().contains_key(par);
            if !known || self.chain(Some(par)).contains(&pid) {
                let why = if known {
                    format!("is {pid} or inside it")
                } else {
                    "is not a project (register it first)".to_owned()
                };
                out.err(format!("project-id: {} {why}", repr(par)));
                return out.with_code(2);
            }
        }
        if let Some(h) = &home
            && !made
            && !self.is_dir(h)
        {
            out.err(format!("project-id: no folder {h}"));
            return out.with_code(2);
        }
        if let Some(d) = ids.get(1)
            && !self.is_dir(d)
        {
            out.err(format!("project-id: no folder {d}"));
            return out.with_code(2);
        }
        if made {
            let h = home.as_deref().unwrap_or_default();
            if let Err(e) = std::fs::create_dir_all(h) {
                out.err(format!("project-id: cannot make {h}: {e}"));
                return out.with_code(1);
            }
            out.err(format!("made {}", self.tilde(h)));
        }
        let (remote, root) = if ids.len() > 1 || home.is_none() {
            let d = ids.get(1).map_or_else(
                || self.ctx.cwd.to_string_lossy().into_owned(),
                |d| (*d).to_owned(),
            );
            locate(&self.real(&d))
        } else {
            // A home is matched by its path, even when it sits inside a repo.
            (
                String::new(),
                self.real(home.as_deref().unwrap_or_default()),
            )
        };
        let home_stored = home.as_deref().map(|h| self.tilde(&self.real(h)));
        let root_stored = self.tilde(&root);
        let entry = self.projects_mut().entry(pid.clone()).or_insert_with(|| {
            let mut m = Map::new();
            m.insert("remotes".into(), Value::Array(Vec::new()));
            m.insert("paths".into(), Value::Array(Vec::new()));
            Value::Object(m)
        });
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Value::Object(p) = entry {
            if let Some(par) = parent {
                p.insert("parent".into(), Value::String(par));
            }
            if let Some(h) = home_stored {
                p.insert("home".into(), Value::String(h));
            }
            // setdefault(key, []) then append when missing; the remotes one only runs for a remote that is not empty.
            let add = |p: &mut Map<String, Value>, key: &str, v: &str| {
                if let Value::Array(a) = p.entry(key).or_insert_with(|| Value::Array(Vec::new()))
                    && !a.iter().any(|x| x.as_str() == Some(v))
                {
                    a.push(Value::String(v.to_owned()));
                }
            };
            if !remote.is_empty() {
                add(p, "remotes", &remote);
            }
            add(p, "paths", &root_stored);
        }
        if let Err(e) = self.save() {
            out.err(e);
            return out.with_code(1);
        }
        out.say(&pid);
        out
    }
}

/// `kith kernel project-id ARGS...`.
pub fn run(ctx: &Ctx, argv: &[String]) -> Out {
    let mut out = Out::default();
    if ctx.guest {
        out.err(
            "project-id: a run for someone other than the owner is answered by the host through kernel/bin/project-id \
             (KITH_CORE=python); the core does not take that branch",
        );
        return out.with_code(2);
    }
    let file = ctx.state.join("projects.json");
    // The verbs that write; the lock is released when `_hold` drops (the file closes).
    let writes = match argv.first().map(String::as_str) {
        Some("--register") => true,
        Some("--root") => argv.len() > 1,
        _ => false,
    };
    let _hold = if writes {
        match lock(&ctx.state) {
            Ok(l) => Some(l),
            Err(e) => {
                out.err(format!("project-id: cannot lock projects.json: {e}"));
                return out.with_code(1);
            }
        }
    } else {
        None
    };
    let reg = match std::fs::read_to_string(&file) {
        Ok(t) => match serde_json::from_str::<Value>(&t) {
            Ok(Value::Object(m)) if m.get("projects").is_none_or(Value::is_object) => m,
            _ => {
                out.err(format!("project-id: {} is not a registry", file.display()));
                return out.with_code(1);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => {
            out.err(format!("project-id: cannot read {}: {e}", file.display()));
            return out.with_code(1);
        }
    };
    let mut run = Run { ctx, reg };
    // reg.setdefault("projects", {}): a registry saved after this has the key.
    run.projects_mut();
    let first = argv.first().map(String::as_str);

    match first {
        Some("--list") => {
            let mut all: Vec<(&String, &Value)> = run.projects().iter().collect();
            all.sort_by(|a, b| a.0.cmp(b.0));
            for (pid, p) in all {
                let was = text(p, "formerly")
                    .map(|f| format!(" (formerly {f})"))
                    .unwrap_or_default();
                let more = text(p, "parent")
                    .map(|x| format!(" in:{x}"))
                    .unwrap_or_default()
                    + &text(p, "home")
                        .map(|x| format!(" home:{x}"))
                        .unwrap_or_default();
                let home = text(p, "home");
                let mut aliases = list(p, "remotes");
                aliases.extend(
                    list(p, "paths")
                        .into_iter()
                        .filter(|a| Some(a.as_str()) != home),
                );
                out.say(format!("{pid}{was}{more}\t{}", aliases.join(" ")));
            }
            out
        }
        Some("--info") => {
            if !run.known(argv) {
                Run::no_project(&mut out, argv);
                return out.with_code(1);
            }
            for pid in run.chain(Some(&argv[1])) {
                let home = text(&run.projects()[&pid], "home")
                    .unwrap_or("-")
                    .to_owned();
                out.say(format!("{pid}\t{home}"));
            }
            out
        }
        Some("--children") => {
            if !run.known(argv) {
                Run::no_project(&mut out, argv);
                return out.with_code(1);
            }
            let deep = argv[2..].iter().any(|a| a == "--all");
            let mut seen = vec![argv[1].clone()];
            for pid in run.children(&argv[1], deep, &mut seen) {
                let at = run
                    .here(&run.projects()[&pid])
                    .unwrap_or_else(|| "-".into());
                out.say(format!("{pid}\t{at}"));
            }
            out
        }
        Some("--where") => {
            if !run.known(argv) {
                Run::no_project(&mut out, argv);
                return out.with_code(1);
            }
            let p = &run.projects()[&argv[1]];
            let mut dirs = list(p, "paths");
            dirs.extend(text(p, "home").map(str::to_owned));
            match dirs.iter().find(|d| !d.is_empty() && run.is_dir(d)) {
                Some(d) => {
                    out.say(run.real(d));
                    out
                }
                None => out.with_code(1),
            }
        }
        Some("--root") => {
            if let Some(d) = argv.get(1) {
                if !run.is_dir(d) {
                    out.err(format!("project-id: no folder {d}"));
                    return out.with_code(2);
                }
                let homes = run.tilde(&run.real(d));
                run.reg.insert("homes".into(), Value::String(homes));
                if let Err(e) = run.save() {
                    out.err(e);
                    return out.with_code(1);
                }
            }
            let homes = run
                .reg
                .get("homes")
                .and_then(Value::as_str)
                .unwrap_or(ROOT)
                .to_owned();
            out.say(homes);
            out
        }
        Some("--register") => run.register(argv),
        _ => {
            let d = first.map_or_else(|| ctx.cwd.to_string_lossy().into_owned(), str::to_owned);
            match run.matching(&run.real(&d)) {
                Some(pid) => {
                    out.say(pid);
                    out
                }
                None => out.with_code(1),
            }
        }
    }
}

#[cfg(test)]
mod tests;
