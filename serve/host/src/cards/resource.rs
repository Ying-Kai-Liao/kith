//! What a card is about: the resource key (A3, A6) and the things that are never carded (A5).
//!
//! The never-carded list mirrors the deny rules `kernel/bin/kith` writes into every run's Claude Code settings (the
//! `sensitive` list and the secrets folder, around line 475-497): the secrets folder, the kith serve token, device
//! identity and databases, browser profiles, the old bridge folder, the memory index and `memory.db*`. A request for
//! one of them is denied at once, with no card, whoever asks: a card cannot make a login or a credential fine to hand
//! to an agent. So is a request for a folder that holds one (searching `~/.kith` would read `secrets/`). Keep the two
//! lists in step; `SENSITIVE` is the one place on the host side.
//!
//! Known limit: for Bash the check is a text match on the command (absolute, `~/`, `$HOME/` and `${HOME}/` spellings,
//! case-folded on macOS). A command can still build a path some other way. The real guards are the kernel's permission
//! deny rules and, for anyone but the owner, the OS sandbox; a card for the owner's Bash is the approver's judgement.

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// Patterns under a Kith state or machine folder (`*` matches within one segment, a trailing `/**` a whole folder).
pub const SENSITIVE: &[&str] = &[
    "secrets/**",
    "auth.json",
    "env",
    "serve/token",
    "serve/identity.json",
    "serve/*.db",
    "serve/*.db-*",
    "browsers/**",
    "bridge/**",
    "index/**",
    "memory.db*",
];

/// The folders below a Kith state or machine folder that a guest's sandbox denies whole (`kith_guard.SECRET_DIRS`). `SENSITIVE`
/// names the files among them a card for a *tool call* never touches; a grant of *access* is judged by the whole folders.
pub const SECRET_DIRS: &[&str] = &["secrets", "serve", "browsers", "bridge", "index", "room-ledger"];

/// Whether `path` is, is below, or holds one of the [`SECRET_DIRS`] of the Kith folders.
pub fn touches_secret_dirs(path: &Path, roots: &Roots) -> bool {
    let p = fold(path);
    roots
        .kith
        .iter()
        .flat_map(|r| SECRET_DIRS.iter().map(move |d| fold(&r.join(d))))
        .any(|s| p.starts_with(&s) || s.starts_with(&p))
}

/// Rule W (designs/2026-10-24-cards-plan.md, plan 2): a run that is not the owner's never writes a file that something
/// outside its sandbox later runs, loads as settings, or loads as instructions. These mirror `kernel/lib/kith_guard.py`
/// (`IN_FOLDER_TREES`, `IN_FOLDER_FILES`, `HOME_TREES`, `HOME_FILES`, `ABS_TREES`); `kith_guard_lists_match_the_kernels` fails when
/// they drift. The kernel's per-run extras (the kernel itself, the agent's package, the people files) are added by
/// [`is_rule_w`]'s caller.
pub const RULE_W_IN_FOLDER_TREES: &[&str] = &[".claude", ".vscode", ".idea", ".git/hooks"];
pub const RULE_W_IN_FOLDER_FILES: &[&str] =
    &["CLAUDE.md", "CLAUDE.local.md", ".mcp.json", ".envrc", ".git/config", ".gitmodules"];
pub const RULE_W_HOME_TREES: &[&str] =
    &[".config/git", "Library/LaunchAgents", ".local/bin", ".npm-global", ".cargo/bin"];
pub const RULE_W_HOME_FILES: &[&str] = &[
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".gitconfig",
    ".claude.json",
];
pub const RULE_W_ABS_TREES: &[&str] = &["/opt/homebrew", "/usr/local/bin"];

/// Whether `path` (absolute, real) is, or is below, a place rule W closes to a boxed run: at any depth for the in-folder
/// names, below `home` for the home ones, below the absolute folders, and below each of `extra`.
pub fn is_rule_w(path: &Path, home: &Path, extra: &[PathBuf]) -> bool {
    let path = fold(path);
    let comps = |p: &str| -> Vec<String> { p.split('/').filter(|c| !c.is_empty()).map(str::to_owned).collect() };
    let have: Vec<String> = comps(&path.to_string_lossy());
    // `want` appears as a run of whole components; a file must end the path, a tree may be followed by more.
    let run = |want: &str, file: bool| {
        let w = comps(&fold_text(want.to_owned()));
        !w.is_empty()
            && have
                .windows(w.len())
                .enumerate()
                .any(|(i, win)| win == w.as_slice() && (!file || i + w.len() == have.len()))
    };
    if RULE_W_IN_FOLDER_TREES.iter().any(|t| run(t, false)) || RULE_W_IN_FOLDER_FILES.iter().any(|f| run(f, true)) {
        return true;
    }
    let home = fold(home);
    let below_home = |rel: &str| path.starts_with(home.join(fold_text(rel.to_owned())));
    RULE_W_HOME_TREES.iter().any(|t| below_home(t))
        || RULE_W_HOME_FILES.iter().any(|f| fold(&home.join(f)) == path)
        || RULE_W_ABS_TREES.iter().any(|a| path.starts_with(fold(Path::new(a))))
        || extra.iter().any(|e| path.starts_with(fold(e)))
}

/// Tools whose input names one file by an absolute path, and the input fields that hold it.
const FILE_FIELDS: &[&str] = &["file_path", "path", "notebook_path"];
const FILE_TOOLS: &[&str] =
    &["Read", "Write", "Edit", "MultiEdit", "NotebookEdit", "NotebookRead", "Glob", "Grep", "LS"];
/// What the approver can be shown in full (the card's detail is cut there); a longer ask is refused, not trimmed.
pub const MAX_SHOWN: usize = 6000;
const MAX_LINKS: usize = 40;

#[derive(Debug, PartialEq, Eq)]
pub enum Classified {
    /// Ask the approver about this.
    Card { key: String, shown: String },
    /// A secret or a login: no card, no.
    Never,
    /// Not a request the host can make a card of (say why).
    Malformed(&'static str),
}

/// The places the never-carded list hangs off.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// Folders the `SENSITIVE` patterns are relative to: this Mac's Kith folder and the kernel's machine folder, as
    /// spelled and as resolved (either may be a link).
    pub kith: Vec<PathBuf>,
    /// Folders that are secret all the way down: the secrets folder (when it is a folder of its own) and the host's
    /// own data folder (serve token, identity, databases).
    pub whole: Vec<PathBuf>,
    /// The secrets folder even when it is the machine folder itself (the shared layout, no `~/.kith`): a grant of access
    /// never reaches it, though tool calls inside it are judged by the patterns. See [`holds_secrets_folder`].
    pub secrets_folder: Vec<PathBuf>,
}

impl Roots {
    #[cfg(test)]
    pub fn new(kith: Vec<PathBuf>) -> Self {
        Self { kith, whole: Vec::new(), secrets_folder: Vec::new() }
    }

    fn add(list: &mut Vec<PathBuf>, p: PathBuf) {
        if p.is_absolute() {
            for q in [p.clone(), std::fs::canonicalize(&p).unwrap_or(p)] {
                if !list.contains(&q) {
                    list.push(q);
                }
            }
        }
    }
}

/// The folders for this Mac, asked of the kernel (`kith_paths.py --json`). A lookup that failed is not remembered:
/// the next ask tries again, and meanwhile the host's own reading of the rules stands.
pub fn roots() -> Roots {
    static CACHE: Mutex<Option<Roots>> = Mutex::new(None);
    if let Some(r) = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return r;
    }
    let mut r = Roots::default();
    Roots::add(&mut r.kith, crate::kernel::machine_dir(&dirs::home_dir().unwrap_or_default()));
    Roots::add(&mut r.whole, crate::service::data_dir());
    let mut settled = true;
    if let Some(k) = crate::kernel::current_kernel() {
        let asked = std::process::Command::new("python3")
            .arg(k.join("lib/kith_paths.py"))
            .arg("--json")
            .output()
            .ok()
            .and_then(|o| o.status.success().then_some(o.stdout))
            .and_then(|out| serde_json::from_slice::<Value>(&out).ok());
        match asked {
            Some(v) => {
                for key in ["KITH_STATE", "KITH_MACHINE"] {
                    if let Some(p) = v[key].as_str() {
                        Roots::add(&mut r.kith, PathBuf::from(p));
                    }
                }
                if let Some(sec) = v["KITH_SECRETS"].as_str() {
                    Roots::add(&mut r.secrets_folder, PathBuf::from(sec));
                }
                // The older layout keeps the secrets among the machine things: the patterns cover `env` and
                // `auth.json` there, and the folder is only secret whole when it is a folder of its own.
                if let (Some(sec), Some(machine)) = (v["KITH_SECRETS"].as_str(), v["KITH_MACHINE"].as_str())
                    && sec != machine
                {
                    Roots::add(&mut r.whole, PathBuf::from(sec));
                }
            }
            None => settled = false,
        }
    }
    if settled {
        *CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r.clone());
    }
    r
}

/// `*` inside one path segment.
fn wild(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((head, tail)) => {
            s.starts_with(head)
                && (0..=s.len() - head.len())
                    .filter(|i| s.is_char_boundary(head.len() + i))
                    .any(|i| !s[head.len()..head.len() + i].contains('/') && wild(tail, &s[head.len() + i..]))
        }
    }
}

/// Whether `rel` (a path below a root, `/`-separated) matches a sensitive pattern.
fn sensitive_rel(rel: &str) -> bool {
    SENSITIVE.iter().any(|pat| match pat.strip_suffix("/**") {
        Some(dir) => rel == dir || rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/')),
        None => wild(pat, rel),
    })
}

/// macOS volumes ignore case, so `Secrets/ENV` is the same file: compare folded there.
pub fn fold(p: &Path) -> PathBuf {
    if cfg!(target_os = "macos") { PathBuf::from(p.to_string_lossy().to_lowercase()) } else { p.to_path_buf() }
}

/// Whether `path` (absolute) is one of the never-carded files.
pub fn is_sensitive(path: &Path, roots: &Roots) -> bool {
    let path = fold(path);
    roots.whole.iter().any(|w| path.starts_with(fold(w)))
        || roots.kith.iter().any(|root| {
            path.strip_prefix(fold(root))
                .ok()
                .and_then(|rel| rel.to_str())
                .is_some_and(|rel| !rel.is_empty() && sensitive_rel(rel))
        })
}

/// Whether `path` is, is inside, or holds the secrets folder, whichever layout the kernel reports: a grant of access is
/// refused for all of it (`kith-auth access` does the same with `kith_guard.whole_secret_paths`).
pub fn holds_secrets_folder(path: &Path, roots: &Roots) -> bool {
    let p = fold(path);
    roots.secrets_folder.iter().map(|s| fold(s)).any(|s| p.starts_with(&s) || s.starts_with(&p))
}

/// Whether a folder (or file) `path` is, or holds, something never carded: a request to search it would read it.
pub fn holds_sensitive(path: &Path, roots: &Roots) -> bool {
    let p = fold(path);
    if is_sensitive(path, roots) {
        return true;
    }
    let inside = |q: &Path| fold(q).starts_with(&p);
    roots.whole.iter().any(|w| inside(w))
        || roots.kith.iter().any(|root| {
            SENSITIVE.iter().any(|pat| {
                // The literal start of the pattern: `secrets/**` -> `secrets`, `serve/*.db` -> `serve`.
                let lit = pat.split('*').next().unwrap_or_default().trim_end_matches('/');
                inside(&root.join(lit))
            })
        })
}

/// `path` made absolute and real: links followed one component at a time, with `..` applied to what a link led to
/// (`/tmp/l/../x` with `l -> ~/.kith/serve` is `~/.kith/x`, not `/tmp/x`), whether or not the file exists yet. `None`
/// for a relative path or a loop of links.
pub fn resolve(path: &str) -> Option<PathBuf> {
    let home = dirs::home_dir();
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None if path == "~" => home?,
        None => PathBuf::from(path),
    };
    if !expanded.is_absolute() {
        return None;
    }
    let mut todo: Vec<OsString> = expanded
        .components()
        .filter(|c| matches!(c, Component::Normal(_) | Component::ParentDir))
        .map(|c| c.as_os_str().to_owned())
        .collect();
    todo.reverse();
    let mut cur = PathBuf::from("/");
    let mut links = 0;
    while let Some(part) = todo.pop() {
        if part == ".." {
            cur.pop();
            continue;
        }
        cur.push(&part);
        if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
            links += 1;
            if links > MAX_LINKS {
                return None;
            }
            let target = std::fs::read_link(&cur).ok()?;
            cur.pop();
            if target.is_absolute() {
                cur = PathBuf::from("/");
            }
            for c in target.components().rev() {
                if matches!(c, Component::Normal(_) | Component::ParentDir) {
                    todo.push(c.as_os_str().to_owned());
                }
            }
        }
    }
    // `/tmp` and friends are links themselves; what is left is a real address, except below the first missing part.
    Some(cur)
}

/// A short stable digest of an input, so equal asks have equal keys whatever the key order.
pub fn digest(input: &Value) -> String {
    fn canon(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                out.push('{');
                for k in keys {
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    canon(&m[k], out);
                    out.push(',');
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for x in a {
                    canon(x, out);
                    out.push(',');
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    canon(input, &mut s);
    Sha256::digest(s.as_bytes()).iter().take(12).fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Whether a command line names a sensitive place by its address: absolute, `~/`, `$HOME/` or `${HOME}/`, case-folded
/// on macOS. A best effort (see the module note).
fn command_names_secret(command: &str, roots: &Roots) -> bool {
    let home = dirs::home_dir();
    let text = fold_text(command.to_owned());
    let spellings = |root: &Path| {
        let mut out = vec![root.to_string_lossy().into_owned()];
        if let Some(rest) = home.as_ref().and_then(|h| root.strip_prefix(h).ok()) {
            for h in ["~", "$HOME", "${HOME}"] {
                out.push(format!("{h}/{}", rest.display()));
            }
        }
        out.into_iter().map(fold_text).collect::<Vec<_>>()
    };
    roots.whole.iter().any(|w| spellings(w).iter().any(|s| text.contains(s.as_str())))
        || roots.kith.iter().any(|root| {
            spellings(root).iter().any(|base| {
                SENSITIVE.iter().any(|pat| {
                    let lit = pat.split('*').next().unwrap_or_default().trim_end_matches('/');
                    !lit.is_empty() && text.contains(&fold_text(format!("{base}/{lit}")))
                })
            })
        })
}

fn fold_text(s: String) -> String {
    if cfg!(target_os = "macos") { s.to_lowercase() } else { s }
}

/// The folder an absolute glob starts in: its literal part up to the last `/` before the first wildcard.
fn glob_base(pattern: &str) -> Option<&str> {
    if !(pattern.starts_with('/') || pattern.starts_with("~/")) {
        return None;
    }
    let lit = pattern.find(['*', '?', '[', '{']).map_or(pattern, |i| &pattern[..i]);
    Some(if lit.len() == pattern.len() {
        lit
    } else {
        lit.rsplit_once('/').map_or("/", |(dir, _)| if dir.is_empty() { "/" } else { dir })
    })
}

pub fn is_file_tool(tool: &str) -> bool {
    FILE_TOOLS.contains(&tool)
}

/// The absolute addresses a file tool's input names (and the folder a Glob pattern starts in), as the run spelled them.
pub fn named_paths<'a>(tool: &str, input: &'a Value) -> Vec<&'a str> {
    if !is_file_tool(tool) {
        return Vec::new();
    }
    let base = (tool == "Glob").then(|| input["pattern"].as_str().and_then(glob_base)).flatten();
    FILE_FIELDS.iter().filter_map(|f| input[f].as_str()).chain(base).collect()
}

/// Sorts a tool call into a card, a never, or a malformed ask. `roots` are the Kith folders (see [`roots`]).
pub fn classify(tool: &str, input: &Value, roots: &Roots) -> Classified {
    if tool.is_empty() || tool.len() > 200 || tool.chars().any(char::is_control) {
        return Classified::Malformed("the tool name is missing or malformed");
    }
    let is_file_tool = FILE_TOOLS.contains(&tool);
    let named: Vec<&str> = FILE_FIELDS.iter().filter_map(|f| input[f].as_str()).collect();
    let pattern_base = (tool == "Glob").then(|| input["pattern"].as_str().and_then(glob_base)).flatten();
    if is_file_tool || tool.starts_with("mcp__") {
        // Every address the call names, and every folder it would search, is checked; a link or a `..` can't hide one.
        for raw in named.iter().copied().chain(pattern_base) {
            let Some(real) = resolve(raw) else {
                return Classified::Malformed("use an absolute path for files");
            };
            if holds_sensitive(&real, roots) || is_sensitive(Path::new(raw), roots) {
                return Classified::Never;
            }
        }
    }
    // One file, one key: its real path. A search (Grep, Glob) is bound by its pattern too, below.
    if is_file_tool
        && !matches!(tool, "Grep" | "Glob")
        && let Some(raw) = named.first()
    {
        let Some(real) = resolve(raw) else { return Classified::Malformed("use an absolute path for files") };
        let shown = real.to_string_lossy().into_owned();
        return Classified::Card { key: format!("{tool}:{shown}"), shown };
    }
    if let Some(command) = input["command"].as_str()
        && command_names_secret(command, roots)
    {
        return Classified::Never;
    }
    // Anything else is bound by everything it was asked with (A3): the key is a digest of the whole input and the
    // approver is shown all of it, or the ask is refused.
    // A command is shown as itself unless it carries more than a description (a timeout, a sandbox switch...).
    let plain = input.as_object().is_some_and(|m| m.keys().all(|k| k == "command" || k == "description"));
    let shown = input["command"].as_str().filter(|_| plain).map_or_else(|| input.to_string(), str::to_owned);
    if shown.len() > MAX_SHOWN {
        return Classified::Malformed("it is too long to show in full for approval");
    }
    Classified::Card { key: format!("{tool}:{}", digest(input)), shown }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roots() -> Roots {
        Roots {
            kith: vec![PathBuf::from("/Users/x/.kith"), PathBuf::from("/Volumes/ssd/kith-state")],
            whole: vec![PathBuf::from("/Users/x/.kith/serve-data"), PathBuf::from("/Volumes/ssd/secretsdir")],
            secrets_folder: Vec::new(),
        }
    }

    fn real(dir: &Path, rel: &str) -> String {
        std::fs::canonicalize(dir).unwrap().join(rel).to_string_lossy().into_owned()
    }

    #[test]
    fn secrets_and_logins_are_never_carded() {
        let r = roots();
        for p in [
            "/Users/x/.kith/secrets/env",
            "/Users/x/.kith/secrets",
            "/Users/x/.kith/auth.json",
            "/Users/x/.kith/serve/token",
            "/Users/x/.kith/serve/identity.json",
            "/Users/x/.kith/serve/kith.db",
            "/Users/x/.kith/serve/kith.db-wal",
            "/Users/x/.kith/browsers/profile/Cookies",
            "/Users/x/.kith/bridge/state",
            "/Users/x/.kith/index/mem",
            "/Users/x/.kith/memory.db",
            "/Users/x/.kith/memory.db-shm",
            "/Volumes/ssd/kith-state/secrets/env",
            "/Users/x/.kith/serve-data/anything",
            "/Volumes/ssd/secretsdir/deep/er/file",
        ] {
            assert_eq!(classify("Read", &json!({"file_path": p}), &r), Classified::Never, "{p}");
            assert_eq!(classify("Edit", &json!({"file_path": p}), &r), Classified::Never, "{p}");
        }
        for p in [
            "/Users/x/.kith/projects/a.md",
            "/Users/x/.kith/serve/other.txt",
            "/Users/x/.kith/secretsy",
            "/tmp/auth.json",
        ] {
            assert!(matches!(classify("Read", &json!({"file_path": p}), &r), Classified::Card { .. }), "{p}");
        }
        assert_eq!(classify("Bash", &json!({"command": "cat /Users/x/.kith/secrets/env"}), &r), Classified::Never);
        assert_eq!(
            classify("Bash", &json!({"command": "cat /Volumes/ssd/kith-state/serve/token | pbcopy"}), &r),
            Classified::Never
        );
        assert!(matches!(classify("Bash", &json!({"command": "ls /tmp"}), &r), Classified::Card { .. }));
    }

    #[test]
    fn bash_names_through_home_spellings_and_case_are_still_secrets() {
        let home = dirs::home_dir().unwrap();
        let r = Roots::new(vec![home.join(".kith")]);
        for c in [
            "cat ~/.kith/secrets/env",
            "cat $HOME/.kith/secrets/env",
            "cat ${HOME}/.kith/serve/token",
            &format!("cat {}/.kith/auth.json", home.display()),
        ] {
            assert_eq!(classify("Bash", &json!({"command": c}), &r), Classified::Never, "{c}");
        }
        if cfg!(target_os = "macos") {
            assert_eq!(classify("Bash", &json!({"command": "cat ~/.KITH/Secrets/env"}), &r), Classified::Never);
            assert_eq!(
                classify("Read", &json!({"file_path": format!("{}/.kith/SECRETS/env", home.display())}), &r),
                Classified::Never
            );
        }
    }

    #[test]
    fn a_dotdot_after_a_link_is_applied_to_where_the_link_led() {
        // /tmp/l -> <state>/serve : /tmp/l/../secrets/env is <state>/secrets/env, not /tmp/secrets/env.
        let dir = std::env::temp_dir().join(format!("kc-{}", uuid::Uuid::new_v4().simple()));
        let serve = dir.join("state/serve");
        std::fs::create_dir_all(&serve).unwrap();
        std::fs::create_dir_all(dir.join("state/secrets")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        std::os::unix::fs::symlink(&serve, dir.join("other/l")).unwrap();
        std::os::unix::fs::symlink("../state/secrets", dir.join("other/rel")).unwrap();
        let r = Roots::new(vec![std::fs::canonicalize(dir.join("state")).unwrap()]);
        for p in [
            format!("{}/other/l/../secrets/env", dir.display()),
            format!("{}/other/rel/env", dir.display()),
            format!("{}/other/../state/secrets/env", dir.display()),
        ] {
            assert_eq!(classify("Read", &json!({"file_path": p}), &r), Classified::Never, "{p}");
        }
        assert_eq!(resolve(&format!("{}/other/l/../x", dir.display())).unwrap(), PathBuf::from(real(&dir, "state/x")));
        // A loop of links is no address.
        std::os::unix::fs::symlink(dir.join("a"), dir.join("b")).unwrap();
        std::os::unix::fs::symlink(dir.join("b"), dir.join("a")).unwrap();
        assert_eq!(resolve(&format!("{}/a/x", dir.display())), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_holds_a_secret_is_never_carded_either() {
        let r = roots();
        for (tool, input) in [
            ("Grep", json!({"pattern": "token", "path": "/Users/x/.kith"})),
            ("LS", json!({"path": "/Users/x/.kith/serve"})),
            ("Glob", json!({"pattern": "**/*", "path": "/Users/x"})),
            ("Glob", json!({"pattern": "/Users/x/.kith/secrets/*"})),
            ("Glob", json!({"pattern": "/Users/x/.kith/**/env"})),
            ("Glob", json!({"pattern": "/Users/**"})),
            ("Grep", json!({"pattern": "x", "path": "/"})),
        ] {
            assert_eq!(classify(tool, &input, &r), Classified::Never, "{tool} {input}");
        }
        for (tool, input) in [
            ("Grep", json!({"pattern": "x", "path": "/Users/x/.kith/projects"})),
            ("Glob", json!({"pattern": "/tmp/**/*.rs"})),
            ("Glob", json!({"pattern": "src/**/*.rs", "path": "/Users/x/code"})),
        ] {
            assert!(matches!(classify(tool, &input, &r), Classified::Card { .. }), "{tool} {input}");
        }
    }

    #[test]
    fn the_card_names_the_resolved_real_path() {
        let dir = std::env::temp_dir().join(format!("kc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let Classified::Card { key, shown } =
            classify("Write", &json!({"file_path": format!("{}/link/new/../a.txt", dir.display())}), &Roots::default())
        else {
            panic!("a card")
        };
        assert_eq!(shown, real(&dir, "real/a.txt"), "links and dots resolved, a new file too");
        assert_eq!(key, format!("Write:{shown}"));
        assert!(matches!(
            classify("Read", &json!({"file_path": "relative/x"}), &Roots::default()),
            Classified::Malformed(_)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_card_is_bound_to_everything_it_was_asked_with() {
        let none = Roots::default();
        let key = |input: Value| match classify("mcp__fs__write", &input, &none) {
            Classified::Card { key, shown } => (key, shown),
            other => panic!("{other:?}"),
        };
        let (a, shown) = key(json!({"path": "/tmp/a", "content": "one"}));
        let (b, _) = key(json!({"path": "/tmp/a", "content": "two"}));
        assert_ne!(a, b, "other arguments are part of the resource");
        assert!(shown.contains("one") && shown.contains("/tmp/a"), "and the approver sees them: {shown}");
        // Too much to show in full is refused, not cut.
        let long = json!({"command": "x".repeat(MAX_SHOWN + 1)});
        assert!(matches!(classify("Bash", &long, &none), Classified::Malformed(_)));
        assert!(matches!(classify("Bash", &json!({"command": "x".repeat(MAX_SHOWN)}), &none), Classified::Card { .. }));
    }

    #[test]
    fn equal_asks_have_equal_keys_whatever_the_field_order() {
        let none = Roots::default();
        let a = classify("Bash", &json!({"command": "ls", "description": "list"}), &none);
        let b = classify(
            "Bash",
            &serde_json::from_str::<Value>(r#"{"description":"list","command":"ls"}"#).unwrap(),
            &none,
        );
        assert_eq!(a, b);
        assert_ne!(a, classify("Bash", &json!({"command": "ls -la", "description": "list"}), &none));
        assert!(matches!(classify("", &json!({}), &none), Classified::Malformed(_)));
    }
}
