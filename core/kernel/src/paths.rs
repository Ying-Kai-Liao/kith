//! Where Kith's folders are: a port of `state_dir()` and `paths()` in `kernel/lib/kith_paths.py`, which stays the
//! source of truth for its own layout rules (read its docstring for the layouts). Pure: the home folder, the
//! environment and the kernel's folder are arguments, so every layout can be tested in a tempdir.
//!
//! Windows layout (decision D1 of `docs/plans/windows-owner.md`): per user, no admin. Programs live in
//! `%LOCALAPPDATA%\Kith\cli\<version>\` (D2: there is no `cli\current` symlink there, `current` is a one-line text file
//! naming the version); state is `%USERPROFILE%\.kith` and agents `%USERPROFILE%\kith\agents`, the Mac layout under the
//! user's profile, so every rule and package placeholder reads the same. Nothing goes in `%APPDATA%`: that would be a second
//! layout. The home is `$HOME` when set (Git Bash, tests), else `%USERPROFILE%` ([`process_home`]).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

/// The marker `kith migrate` and every fresh install write in `~/.kith`.
pub const MARKER: &str = ".migrated";

/// The value of an environment variable by its full name (`KITH_STATE`), or `None` when unset.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Every folder `kith_paths.py --json` names, in its order, with the old `HQ_` spellings after the `KITH_` ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    pub state: PathBuf,
    pub agents: PathBuf,
    pub home_real: PathBuf,
    pub state_real: PathBuf,
    pub agents_real: PathBuf,
    pub machine: PathBuf,
    pub secrets: PathBuf,
    pub logs: PathBuf,
    pub cache: PathBuf,
    pub store: PathBuf,
}

impl Paths {
    /// `(KITH_<X>, folder)` in the order of `kith_paths.py`, then the same under `HQ_<X>`.
    pub fn entries(&self) -> Vec<(String, &Path)> {
        let own: [(&str, &Path); 11] = [
            ("HOME", &self.home),
            ("STATE", &self.state),
            ("AGENTS", &self.agents),
            ("HOME_REAL", &self.home_real),
            ("STATE_REAL", &self.state_real),
            ("AGENTS_REAL", &self.agents_real),
            ("MACHINE", &self.machine),
            ("SECRETS", &self.secrets),
            ("LOGS", &self.logs),
            ("CACHE", &self.cache),
            ("STORE", &self.store),
        ];
        ["KITH_", "HQ_"]
            .iter()
            .flat_map(|p| own.iter().map(move |(k, v)| (format!("{p}{k}"), *v)))
            .collect()
    }

    /// The same text as `kith_paths.py --json`.
    pub fn json(&self) -> String {
        let fields: Vec<String> = self
            .entries()
            .iter()
            .map(|(k, v)| format!("{}: {}", quote(k), quote(&v.to_string_lossy())))
            .collect();
        format!("{{{}}}", fields.join(", "))
    }

    /// The same text as `kith_paths.py` with no argument.
    pub fn text(&self) -> String {
        let lines: Vec<String> = self
            .entries()
            .iter()
            .map(|(k, v)| format!("{k:<15} {}\n", v.display()))
            .collect();
        lines.concat()
    }

    /// The same text as `kith_paths.py --sh`.
    pub fn sh(&self) -> String {
        let lines: Vec<String> = self
            .entries()
            .iter()
            .map(|(k, v)| format!("export {k}={}\n", shell_quote(&v.to_string_lossy())))
            .collect();
        lines.concat()
    }
}

/// A JSON string the way Python's `json.dumps` writes it (non-ASCII escaped).
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
        }
    }
    out.push('"');
    out
}

/// `shlex.quote`.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// `KITH_<name>`, else the old `HQ_<name>`; an empty value counts as unset (Python's `or`).
fn getenv(env: Env<'_>, name: &str) -> String {
    [format!("KITH_{name}"), format!("HQ_{name}")]
        .iter()
        .find_map(|k| env(k).filter(|v| !v.is_empty()))
        .unwrap_or_default()
}

/// A path the way Python's `Path()` spells it: no `.` parts, no doubled or trailing slash.
fn norm(p: &Path) -> PathBuf {
    let out: PathBuf = p.components().filter(|c| *c != Component::CurDir).collect();
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// `os.path.expanduser` for `~` and `~/...` (another user's `~name` stays as written).
fn expanduser(home: &Path, s: &str) -> PathBuf {
    match s.strip_prefix('~') {
        Some("") => norm(home),
        Some(rest) if tilde_rest(rest, cfg!(windows)) => {
            norm(&home.join(rest.trim_start_matches(seps(cfg!(windows)))))
        }
        _ => norm(Path::new(s)),
    }
}

/// The separators that follow a `~`: `/`, and on Windows `\` too (`~\agents`).
fn seps(windows: bool) -> &'static [char] {
    if windows {
        &['/', '\\']
    } else {
        &['/']
    }
}

/// Whether what follows a `~` starts a path below the home (so `~name` stays another user's).
fn tilde_rest(rest: &str, windows: bool) -> bool {
    rest.starts_with(seps(windows))
}

/// A path as `std::fs::canonicalize` returns it on Windows, without its `\\?\` verbatim prefix: `\\?\C:\x` is `C:\x`
/// and `\\?\UNC\srv\share` is `\\srv\share`. Anything else is returned as it came.
pub fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            (Some(d), Some(':')) if d.is_ascii_alphabetic() => rest.to_owned(),
            _ => s.to_owned(),
        }
    } else {
        s.to_owned()
    }
}

/// `os.path.realpath` (not strict): symlinks resolved as far as the path exists, the rest kept as written.
pub fn realpath(p: &Path) -> PathBuf {
    resolve(p, false).unwrap_or_default()
}

/// [`realpath`] that gives up (None) when the links run past the cap instead of reading the next link as a plain name, for
/// callers that must not guess (the read fence).
pub fn realpath_strict(p: &Path) -> Option<PathBuf> {
    resolve(p, true)
}

/// One step of a path being resolved: a drive or share prefix, the root, or a name.
enum Piece {
    Prefix(OsString),
    Root,
    Name(OsString),
}

fn pieces(p: &Path) -> impl DoubleEndedIterator<Item = Piece> + '_ {
    p.components().map(|c| match c {
        Component::Prefix(x) => Piece::Prefix(x.as_os_str().to_owned()),
        Component::RootDir => Piece::Root,
        other => Piece::Name(other.as_os_str().to_owned()),
    })
}

/// The root of a path being built: its prefix (a drive, on Windows) and the root slash.
fn root_of(out: &Path) -> PathBuf {
    let mut root: PathBuf = out
        .components()
        .take_while(|c| matches!(c, Component::Prefix(_)))
        .collect();
    root.push(Component::RootDir);
    root
}

fn resolve(p: &Path, strict: bool) -> Option<PathBuf> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut pending: VecDeque<Piece> = pieces(&abs).collect();
    let mut out = PathBuf::new();
    let mut hops = 0;
    while let Some(c) = pending.pop_front() {
        let c = match c {
            Piece::Prefix(x) => {
                out = PathBuf::from(x);
                continue;
            }
            Piece::Root => {
                out = root_of(&out);
                continue;
            }
            Piece::Name(c) => c,
        };
        if c == "." {
            // stays where it is
        } else if c == ".." {
            out.pop();
        } else {
            let next = out.join(&c);
            match std::fs::read_link(&next) {
                Ok(_) if strict && hops >= 40 => return None,
                Ok(target) if hops < 40 => {
                    hops += 1;
                    if target.is_absolute() {
                        out = PathBuf::new();
                    }
                    for part in pieces(&target).rev() {
                        pending.push_front(part);
                    }
                }
                _ => out = next,
            }
        }
    }
    if cfg!(windows) {
        // read_link hands back `\\?\C:\...`; the rest of Kith compares and shows plain `C:\...`.
        return Some(PathBuf::from(strip_verbatim(&out.to_string_lossy())));
    }
    Some(out)
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// `(this Mac's Kith folder, whether it is a real machine folder rather than the old link to the Kith folder)`.
pub fn state_dir(home: &Path, env: Env<'_>) -> (PathBuf, bool) {
    let given = getenv(env, "STATE");
    let given = given.trim();
    if !given.is_empty() {
        let d = expanduser(home, given);
        // <home>/state is what the old link layout exports as KITH_STATE; children must not mistake it for a choice.
        let old_export = d.file_name().is_some_and(|n| n == "state")
            && d.parent().is_some_and(|p| p.join("kernel").is_dir());
        return (d, !old_export);
    }
    let (dot, old) = (home.join(".kith"), home.join(".hq"));
    if dot.join(MARKER).exists() {
        return (dot, true);
    }
    if old.is_dir() && !is_link(&old) {
        return (old, true);
    }
    if old.is_dir() {
        if dot.is_dir() && realpath(&old) == realpath(&dot) {
            return (dot, true);
        }
        return (old, false);
    }
    // Nothing yet: the old link layout (state in <home>/state) until install.sh makes ~/.kith.
    let mine = dot.is_dir();
    (dot, mine)
}

fn first(candidates: [PathBuf; 2]) -> PathBuf {
    let [a, b] = candidates;
    if a.exists() {
        a
    } else if b.exists() {
        b
    } else {
        a
    }
}

/// Every folder, for a home folder, an environment and the folder of the running kernel (the one holding `bin/` and
/// `lib/`): the Kith folder falls back to the one around the kernel when `~/kith` and `~/hq` are both missing.
pub fn paths(home: &Path, kernel: &Path, env: Env<'_>) -> Paths {
    let (dot, mine) = state_dir(home, env);
    let given = getenv(env, "HOME");
    let given = given.trim();
    let (linked, old_linked) = (home.join("kith"), home.join("hq"));
    let fallback = if kernel.file_name().is_some_and(|n| n == "kernel") {
        kernel
            .parent()
            .map_or_else(|| kernel.to_path_buf(), Path::to_path_buf)
    } else {
        kernel.to_path_buf()
    };
    let kith = if !given.is_empty() {
        expanduser(home, given)
    } else if linked.is_dir() {
        linked
    } else if old_linked.is_dir() {
        old_linked
    } else if is_link(&dot) && dot.is_dir() {
        dot.clone()
    } else {
        fallback
    };
    let state = if mine {
        dot.clone()
    } else {
        kith.join("state")
    };
    // KITH_AGENTS (HQ_AGENTS) moves the agents folder alone; kith_paths.py reads it too.
    let given_agents = getenv(env, "AGENTS");
    let given_agents = given_agents.trim();
    let agents = if given_agents.is_empty() {
        kith.join("agents")
    } else {
        expanduser(home, given_agents)
    };
    let (home_real, state_real, agents_real) =
        (realpath(&kith), realpath(&state), realpath(&agents));
    let (machine, secrets, logs, cache, store);
    if mine {
        machine = dot.clone();
        secrets = dot.join("secrets");
        logs = dot.join("logs");
        cache = dot.join("cache");
        store = dot.join("store");
    } else {
        let config = first([home.join(".config/kith"), home.join(".config/hq")]);
        secrets = config.clone();
        machine = config;
        logs = first([home.join("Library/Logs/kith"), home.join("Library/Logs/hq")]);
        cache = first([
            home.join("Library/Caches/kith"),
            home.join("Library/Caches/hq"),
        ]);
        store = agents.join(".store");
    }
    Paths {
        home: kith,
        state,
        agents,
        home_real,
        state_real,
        agents_real,
        machine,
        secrets,
        logs,
        cache,
        store,
    }
}

/// The process environment, as `Env`.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The home folder from the two variables, `HOME` first: Git Bash and the tests set it, and it must win there. Windows
/// falls back to `USERPROFILE` (a cmd or service process has no `HOME`); a OneDrive-redirected profile is just another
/// `USERPROFILE` (only an absolute one: never a relative folder). Empty when neither says, which callers that build folders must refuse ([`known_home`]).
fn home_from(home: Option<OsString>, profile: Option<OsString>, windows: bool) -> PathBuf {
    home.filter(|h| !h.is_empty())
        .or_else(|| profile.filter(|h| windows && win_absolute(&h.to_string_lossy())))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// A Windows absolute path by its spelling (`C:\x`, `C:/x`, `\\srv\share`), so it can be told apart on every platform.
fn win_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with(r"\\") || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}

/// `$HOME` (Python's `Path.home()`), on Windows else `%USERPROFILE%`; empty when neither is set.
pub fn process_home() -> PathBuf {
    home_from(
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        cfg!(windows),
    )
}

/// The process's home when it can be known: [`process_home`], else the password database's home (`$HOME` is unset under
/// some launchers). `None` when neither says: a caller that needs a folder under it must refuse, never use a relative path.
pub fn known_home() -> Option<PathBuf> {
    let home = process_home();
    if !home.as_os_str().is_empty() {
        return Some(home);
    }
    #[cfg(unix)]
    return crate::threads::os::passwd_home().filter(|h| h.is_absolute());
    #[cfg(not(unix))]
    None
}

/// `paths` for this process: its `$HOME` and environment, and the given kernel folder.
pub fn from_process(kernel: &Path) -> Paths {
    paths(&process_home(), kernel, &process_env)
}

// The tests build POSIX layouts and symlinks; the Windows job runs the rest of the crate.
#[cfg(all(test, unix))]
mod tests;
#[cfg(test)]
mod win_tests;
