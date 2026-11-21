//! Which calls and rules a must-ask class covers: the judging half of `kernel/lib/kith_guard.py` (`must_ask`,
//! `must_ask_rule`, `sentence_class` and the `Places` methods they use), ported line for line. `guard.rs` has the other
//! half (the rules a launch writes); `kernel/tests/difftest/golden-lib/guard-must-ask.json` freezes the Python's answers.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::guard::{
    CODE_CMDS, DESTROY_CMDS, HOME_SECRETS, INLINE, INLINE_FISH, OUTWARD_CMDS, Places, SECRET_CMDS,
    SECRET_CMDS_LAST, SECRET_DIRS, SHELLS, TRUST_CMDS, both, dedup, whole_secret_paths,
};
use super::pyport::{fnmatch, py_split, py_strip, redact, shlex_words};
use crate::paths::realpath;

/// The five classes, in the order a call's class is picked.
pub const CLASSES: [&str; 5] = ["secret", "trust", "destroy", "outward", "code"];

#[allow(clippy::unwrap_used)] // fixed patterns
fn rx(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

static SECRET_WORDS: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        rx(
            r"(?i)\.ssh\b|\.aws\b|\.gnupg\b|\.netrc|\.npmrc|\.pypirc|\.docker/config|\bid_(?:rsa|dsa|ed25519|ecdsa)\b|auth\.json|serve/token|\.kith/(?:secrets|serve|browsers|bridge|index)\b|/secrets/|\bkeychain|\bsecurity\s+(?:find|dump|add|delete)-|\.pem\b|\.env\b",
        ),
        // the lookarounds `(?<![A-Za-z])` and `(?![a-z])` of the Python are checked by hand (`word_secret`)
        rx(
            r"(?i)(?:credentials?|passwords?|passwd|api[_-]?keys?|secrets?|tokens?|bearer|private[_-]?key)",
        ),
    ]
});

/// Whether `c` is a letter the Python's case-insensitive `[A-Za-z]` takes (Unicode folding adds the long s and the Kelvin sign).
fn letter(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphabetic() || c == '\u{17f}' || c == '\u{212a}')
}

fn word_secret(text: &str) -> bool {
    let mut at = 0;
    while let Some(m) = SECRET_WORDS[1].find_at(text, at) {
        let (before, after) = (
            text[..m.start()].chars().next_back(),
            text[m.end()..].chars().next(),
        );
        if !letter(before) && !letter(after) {
            return true;
        }
        at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// `json.dumps(v, ensure_ascii=False)`: the separators Python puts between items.
fn py_json(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(py_json).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_json(&Value::String(k.clone())), py_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        v => v.to_string(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn fold() -> bool {
    cfg!(target_os = "macos")
}

fn lower(s: &str) -> String {
    if fold() {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

fn basename(p: &str) -> &str {
    p.rfind('/').map_or(p, |i| &p[i + 1..])
}

/// `holds(path, inner)`: `path` is `inner` or a folder above it. Case-folded on macOS.
pub fn holds(path: &str, inner: &str) -> bool {
    let (path, inner) = (lower(path), lower(inner));
    inner == path || inner.starts_with(&format!("{}/", path.trim_end_matches('/')))
}

/// `covers(entry, path)`: a deny entry (a path, `dir/**` for a tree, `dir/**/name` at any depth) holds `path`.
pub fn covers(entry: &str, path: &str) -> bool {
    let (entry, path) = (lower(entry), lower(path));
    let e = entry.strip_suffix("/**").unwrap_or(&entry);
    let pieces: Vec<&str> = e.split("/**/").collect();
    cover_from(&path, &pieces)
}

fn cover_from(rest: &str, pieces: &[&str]) -> bool {
    let Some((first, tail)) = pieces.split_first() else {
        return false;
    };
    let Some(r) = rest.strip_prefix(first) else {
        return false;
    };
    if tail.is_empty() {
        // `(?:/.*)?`, where `.` stops at a newline
        return r.is_empty() || r.strip_prefix('/').is_some_and(|x| !x.contains('\n'));
    }
    let Some(s) = r.strip_prefix('/') else {
        return false;
    };
    // `/(?:.*/)?`: the next piece follows at once or after a run of folders
    cover_from(s, tail)
        || s.match_indices('/')
            .any(|(i, _)| !s[..i].contains('\n') && cover_from(&s[i + 1..], tail))
}

fn expand(p: &str, home: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        return format!("{home}{}", &p[1..]);
    }
    for v in ["$HOME", "${HOME}"] {
        if p == v || p.starts_with(&format!("{v}/")) {
            return format!("{home}{}", &p[v.len()..]);
        }
    }
    p.to_owned()
}

impl Places {
    /// `Places.secret_trees`.
    pub fn secret_trees(&self) -> Vec<String> {
        let roots: Vec<String> = self.state.iter().chain(&self.machine).cloned().collect();
        let mut out = whole_secret_paths(&roots, &self.secrets_dir, &self.machine[0]);
        for h in both(&self.home) {
            out.extend(HOME_SECRETS.iter().map(|s| format!("{h}/{s}")));
        }
        out
    }

    /// `Places.trust`'s globs: `fnmatch` patterns on an absolute path.
    fn trust_globs(&self) -> Vec<String> {
        let mut globs = vec![];
        for a in &self.agents {
            for n in [
                "*/allowed.json",
                "*/allowed.json.*",
                "*/role/settings.json",
                "*/role/capabilities.yaml",
                "*/role/mcp.json",
            ] {
                globs.push(format!("{a}/{n}"));
            }
        }
        globs.extend(
            [
                "*/.claude/settings.json",
                "*/.claude/settings.local.json",
                "*/.git/hooks/*",
            ]
            .map(str::to_owned),
        );
        dedup(globs)
    }

    /// `Places.is_trust`: whether writing `path` changes what later runs may do.
    pub fn is_trust(&self, path: &str) -> bool {
        let mut path = expand(path, &self.home);
        if !path.starts_with('/') {
            path = format!("/{path}"); // a relative path: only the at-any-depth names can be told
        }
        let (trees, files) = self.trust();
        let globs = self.trust_globs();
        let real = realpath(std::path::Path::new(&path))
            .to_string_lossy()
            .into_owned();
        for x in dedup([path, real]) {
            for pk in &self.package {
                // the agent's own learned skills are its to write
                if holds(&format!("{pk}/skills/learned"), &x) {
                    return false;
                }
            }
            if trees.iter().any(|t| holds(t, &x))
                || files.iter().any(|f| lower(f) == lower(&x))
                || globs.iter().any(|g| fnmatch(&lower(&x), &lower(g)))
            {
                return true;
            }
        }
        false
    }

    /// `Places.is_secret`.
    pub fn is_secret(&self, path: &str) -> bool {
        let path = expand(path, &self.home);
        let real = realpath(std::path::Path::new(&path))
            .to_string_lossy()
            .into_owned();
        let trees = self.secret_trees();
        for x in dedup([path, real]) {
            for s in &trees {
                if s.contains('*') {
                    if fnmatch(&x, &s.replace("/**", "/*")) || fnmatch(&x, &s.replace("/**", "")) {
                        return true;
                    }
                } else if covers(s, &x) {
                    return true;
                }
            }
        }
        false
    }

    fn names_secret(&self, text: &str) -> bool {
        if SECRET_WORDS[0].is_match(text) || word_secret(text) {
            return true;
        }
        if let Some(s) = &self.kith_secrets
            && s.chars().count() > 1
            && text.contains(s.as_str())
        {
            return true;
        }
        for r in self.state.iter().chain(&self.machine) {
            if SECRET_DIRS
                .iter()
                .any(|d| text.contains(&format!("{r}/{d}")))
            {
                return true;
            }
        }
        redact(text) != text
    }
}

/// `_segments`: the simple commands of a shell line, as word lists.
fn segments(command: &str) -> Vec<Vec<String>> {
    let line = command.replace('\n', " ; ");
    let toks = shlex_words(&line).unwrap_or_else(|| py_split(&line));
    let (mut segs, mut cur) = (vec![], vec![]);
    for t in toks {
        if !t.is_empty() && t.chars().all(|c| ";&|()".contains(c)) {
            if !cur.is_empty() {
                segs.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(t);
        }
    }
    if !cur.is_empty() {
        segs.push(cur);
    }
    segs
}

const WRAPPERS: [&str; 10] = [
    "env",
    "command",
    "builtin",
    "nohup",
    "time",
    "nice",
    "exec",
    "sudo",
    "doas",
    "caffeinate",
];
const LAUNCHERS: [&str; 19] = [
    "uv", "poetry", "pipenv", "pdm", "hatch", "conda", "rye", "pyenv", "mise", "asdf", "xcrun",
    "nix", "arch", "xargs", "find", "npm", "pnpm", "yarn", "bun",
];
const WRITERS: [&str; 6] = ["tee", "touch", "chmod", "chown", "truncate", "install"];
const LAST_WRITERS: [&str; 5] = ["cp", "mv", "ln", "rsync", "ditto"];
const DELETERS: [&str; 5] = ["rm", "rmdir", "unlink", "trash", "truncate"];
const REDIRECTS: [&str; 5] = [">", ">>", ">|", "&>", "&>>"];

/// `_strip`: the command a segment runs, with leading `VAR=value` words and wrappers taken off.
fn strip(words: &[String]) -> Vec<String> {
    static ASSIGN: LazyLock<Regex> = LazyLock::new(|| rx(r"^[A-Za-z_][A-Za-z0-9_]*="));
    let mut w = words;
    while let Some(first) = w.first() {
        if ASSIGN.is_match(first) {
            w = &w[1..];
        } else if WRAPPERS.contains(&basename(first)) {
            w = &w[1..];
            while w.first().is_some_and(|x| x.starts_with('-')) {
                w = &w[1..];
            }
        } else if basename(first) == "timeout" && w.len() > 1 {
            w = &w[2..];
        } else {
            break;
        }
    }
    w.to_vec()
}

fn base(word: &str) -> &str {
    static PY: LazyLock<Regex> = LazyLock::new(|| rx(r"^(?:python(?:\d+(?:\.\d+)?)?|pypy\d*)$"));
    let b = basename(word);
    if PY.is_match(b) { "python" } else { b }
}

/// `_starts`: whether the command's leading words are one of `cmds`.
fn starts(words: &[String], cmds: &[&str]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    let first = basename(first);
    let first = match first {
        "hq-auth" => "kith-auth",
        "hq-allow" => "kith-allow",
        "hq" => "kith",
        f => f,
    };
    let mut lead: Vec<&str> = std::iter::once(first)
        .chain(words[1..].iter().map(String::as_str))
        .collect();
    if lead[0] == "kith" && lead.len() > 1 && lead[1] == "auth" {
        lead = std::iter::once("kith-auth")
            .chain(lead[2..].iter().copied())
            .collect();
    }
    if lead[0] == "git" {
        // git -C <dir> x: judged as git x
        while lead.len() > 2 && (lead[1] == "-C" || lead[1] == "-c") {
            lead = std::iter::once(lead[0])
                .chain(lead[3..].iter().copied())
                .collect();
        }
    }
    cmds.iter().any(|c| {
        let cw: Vec<&str> = c.split_whitespace().collect();
        lead.get(..cw.len()) == Some(&cw[..]) || (cw[0] == "mkfs" && lead[0].starts_with("mkfs"))
    })
}

fn git_args(words: &[String]) -> &[String] {
    let mut w = &words[1..];
    while w.len() > 1 && (w[0] == "-C" || w[0] == "-c") {
        w = &w[2..];
    }
    w
}

/// `_push_outward`: a force push, a push of tags, or a push to main or master.
fn push_outward(words: &[String]) -> bool {
    let w = git_args(words);
    if w.first().is_none_or(|x| x != "push") {
        return false;
    }
    for a in &w[1..] {
        if [
            "-f",
            "--force",
            "--tags",
            "--follow-tags",
            "--mirror",
            "--delete",
            "-d",
        ]
        .contains(&a.as_str())
            || a.starts_with("--force")
            || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('f'))
        {
            return true;
        }
        if !a.starts_with('-')
            && (a.starts_with('+') || a.starts_with(':') || a.contains("refs/tags/"))
        {
            return true;
        }
        let r = if a.contains(':') {
            a.rsplit(':').next().unwrap_or("")
        } else {
            a
        };
        let r = r.strip_prefix("refs/heads/").unwrap_or(r);
        if !a.starts_with('-') && (r == "main" || r == "master") {
            return true;
        }
    }
    false
}

/// `\w` for the Python's patterns.
fn word_chars(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn gh_api_write(words: &[String]) -> bool {
    let w = &words[1..];
    if w.first().is_none_or(|x| x != "api") {
        return false;
    }
    for (i, a) in w.iter().enumerate() {
        if (a == "-X" || a == "--method") && i + 1 < w.len() && w[i + 1].to_uppercase() != "GET" {
            return true;
        }
        // re.fullmatch(r"(?:-X|--method=)(?!GET$)\w+", a, re.I)
        let lowered = a.to_ascii_lowercase();
        for p in ["-x", "--method="] {
            if lowered.starts_with(p) {
                let rest = &a[p.len()..];
                if word_chars(rest) && !rest.eq_ignore_ascii_case("GET") {
                    return true;
                }
            }
        }
        if ["-f", "-F", "--field", "--raw-field", "--input"].contains(&a.as_str())
            || ["--field=", "--raw-field=", "--input="]
                .iter()
                .any(|p| a.starts_with(p))
        {
            return true;
        }
    }
    false
}

fn curl_write(words: &[String]) -> bool {
    static SHORT: LazyLock<Regex> = LazyLock::new(|| rx(r"^-[A-Za-z]*[dFT]\S*$"));
    let b = basename(&words[0]);
    for (i, a) in words.iter().enumerate().skip(1) {
        if b == "curl" {
            if (a == "-X" || a == "--request")
                && i + 1 < words.len()
                && !["GET", "HEAD"].contains(&words[i + 1].to_uppercase().as_str())
            {
                return true;
            }
            // re.fullmatch(r"-X(?!GET$|HEAD$)\w+", a, re.I)
            if a.to_ascii_lowercase().starts_with("-x") {
                let rest = &a[2..];
                if word_chars(rest)
                    && !rest.eq_ignore_ascii_case("GET")
                    && !rest.eq_ignore_ascii_case("HEAD")
                {
                    return true;
                }
            }
            if ["--data", "--form", "--json", "--upload-file"]
                .iter()
                .any(|p| a.starts_with(p))
            {
                return true;
            }
            if ["-d", "-F", "-T"].contains(&a.as_str())
                || (SHORT.is_match(a) && !a.starts_with("--"))
            {
                return true;
            }
        }
        if b == "wget"
            && [
                "--post-data",
                "--post-file",
                "--method",
                "--body-data",
                "--body-file",
            ]
            .iter()
            .any(|p| a.starts_with(p))
        {
            return true;
        }
    }
    false
}

fn inline_flags(b: &str) -> Option<&'static [&'static str]> {
    INLINE
        .iter()
        .chain([&INLINE_FISH])
        .find(|(n, _)| *n == b)
        .map(|(_, f)| *f)
}

/// `_inline_code`: an interpreter or shell given code inline, alone (reading stdin) or fed a heredoc.
fn inline_code(words: &[String]) -> bool {
    static SHELL_C: LazyLock<Regex> = LazyLock::new(|| rx(r"^-[a-z]*c[a-z]*$"));
    let mut starts_at = vec![0];
    if LAUNCHERS.contains(&basename(&words[0])) {
        starts_at.extend((1..words.len()).filter(|i| inline_flags(base(&words[*i])).is_some()));
    }
    for i in starts_at {
        let b = base(&words[i]);
        let Some(flags) = inline_flags(b) else {
            continue;
        };
        let rest = &words[i + 1..];
        if rest
            .first()
            .is_none_or(|r| ["-", "<", "<<", "<<<"].contains(&r.as_str()))
        {
            return true;
        }
        for r in rest {
            if flags.contains(&r.as_str()) || (SHELLS.contains(&b) && SHELL_C.is_match(r)) {
                return true;
            }
            if !r.starts_with('-') {
                break;
            }
        }
    }
    false
}

/// `_targets`: the files a command writes, by its spelling.
fn targets(words: &[String]) -> Vec<String> {
    let mut out = vec![];
    for (i, w) in words.iter().enumerate() {
        if REDIRECTS.contains(&w.as_str()) && i + 1 < words.len() {
            out.push(words[i + 1].clone());
        }
    }
    let cmd: Vec<&String> = words
        .iter()
        .filter(|w| !REDIRECTS.contains(&w.as_str()))
        .collect();
    if cmd.is_empty() {
        return out;
    }
    let b = basename(cmd[0]);
    let args: Vec<&String> = cmd[1..]
        .iter()
        .filter(|a| !a.starts_with('-'))
        .copied()
        .collect();
    if WRITERS.contains(&b) {
        out.extend(args.iter().map(|a| (*a).clone()));
    } else if LAST_WRITERS.contains(&b) && !args.is_empty() {
        out.push(args[args.len() - 1].clone());
    } else if ["sed", "gsed", "perl"].contains(&b)
        && cmd[1..]
            .iter()
            .any(|a| a.starts_with("-i") || a.starts_with("--in-place"))
    {
        out.extend(args.iter().skip(1).map(|a| (*a).clone()));
    } else if b == "dd" {
        out.extend(
            cmd[1..]
                .iter()
                .filter_map(|a| a.strip_prefix("of=").map(str::to_owned)),
        );
    }
    out
}

fn inside(path: &str, roots: &[String], home: &str) -> bool {
    let path = expand(path, home);
    if !path.starts_with('/') {
        return false;
    }
    let real = realpath(std::path::Path::new(&path))
        .to_string_lossy()
        .into_owned();
    roots.iter().any(|r| holds(r, &path) || holds(r, &real))
        && !roots
            .iter()
            .any(|r| r.trim_end_matches('/') == real.trim_end_matches('/'))
}

fn bash(command: &str, roots: &[String], pl: &Places) -> BTreeSet<&'static str> {
    let mut found = BTreeSet::new();
    let secret_cmds: Vec<&str> = SECRET_CMDS
        .iter()
        .copied()
        .chain([SECRET_CMDS_LAST])
        .collect();
    for seg in segments(command) {
        let words = strip(&seg);
        if words.is_empty() {
            continue;
        }
        let b = basename(&words[0]);
        if starts(&words, &secret_cmds) {
            found.insert("secret");
        }
        if starts(&words, &TRUST_CMDS) {
            found.insert("trust");
        }
        let tg = targets(&words);
        if tg.iter().any(|t| pl.is_secret(t)) {
            found.insert("secret");
        } else if tg.iter().any(|t| pl.is_trust(t)) {
            found.insert("trust");
        }
        if starts(&words, &DESTROY_CMDS)
            || (b == "dd" && words.iter().any(|a| a.starts_with("of=")))
        {
            found.insert("destroy");
        }
        let has = |x: &str| words.iter().any(|w| w == x);
        let deleter = DELETERS.contains(&b);
        if deleter || (b == "find" && (has("-delete") || has("rm"))) || (b == "xargs" && has("rm"))
        {
            let tg: Vec<&String> = words[1..]
                .iter()
                .filter(|a| !a.starts_with('-') && !REDIRECTS.contains(&a.as_str()))
                .collect();
            if b == "find"
                || b == "xargs"
                || tg.is_empty()
                || !tg.iter().all(|t| inside(t, roots, &pl.home))
            {
                found.insert("destroy");
            }
            if deleter && tg.iter().any(|t| pl.is_trust(t)) {
                found.insert("trust");
            }
        }
        if (b == "git" && push_outward(&words))
            || (b == "gh" && gh_api_write(&words))
            || curl_write(&words)
            || starts(&words, &OUTWARD_CMDS)
        {
            found.insert("outward");
        }
        if starts(&words, &CODE_CMDS) || inline_code(&words) {
            found.insert("code");
        }
    }
    found
}

/// `must_ask`: the must-ask class of one call, or `None` for routine. `roots` are the folders the run may change freely.
pub fn must_ask(tool: &str, input: &Value, roots: &[String], pl: &Places) -> Option<&'static str> {
    static OUTWARD_TOOL: LazyLock<Regex> = LazyLock::new(|| {
        rx(
            r"(?:^|_)(?:merge|publish|deploy|send|post|tweet|pay|purchase|buy|transfer|invite|share)(?:_|$)",
        )
    });
    static DESTROY_TOOL: LazyLock<Regex> =
        LazyLock::new(|| rx(r"(?:^|_)(?:delete|destroy|drop|purge|remove|wipe)(?:_|$)"));
    let empty = Value::Object(serde_json::Map::new());
    let input = if input.is_object() { input } else { &empty };
    let mut found: BTreeSet<&str> = BTreeSet::new();
    if pl.names_secret(&py_json(input)) {
        found.insert("secret");
    }
    let roots: Vec<String> = roots.iter().flat_map(|r| both(r)).collect();
    if tool == "Bash" {
        if let Some(cmd) = input.get("command").and_then(Value::as_str) {
            found.extend(bash(cmd, &roots, pl));
        }
    } else if [
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Read",
        "Grep",
        "Glob",
    ]
    .contains(&tool)
    {
        let p = ["file_path", "notebook_path", "path"]
            .iter()
            .filter_map(|k| input.get(k))
            .find(|v| truthy(v));
        if let Some(p) = p.and_then(Value::as_str) {
            if pl.is_secret(p) {
                found.insert("secret");
            } else if !["Read", "Grep", "Glob"].contains(&tool) && pl.is_trust(p) {
                found.insert("trust");
            }
        }
    } else if tool.starts_with("mcp__") && !tool.starts_with("mcp__kithpermit__") {
        let name = tool.rsplit("__").next().unwrap_or("").to_lowercase();
        if OUTWARD_TOOL.is_match(&name) {
            found.insert("outward");
        } else if DESTROY_TOOL.is_match(&name) {
            found.insert("destroy");
        }
    }
    CLASSES.iter().copied().find(|c| found.contains(c))
}

/// The path a Read/Edit rule names, with its `/**` or `/*` tail cut: `re.sub(r"/\*\*.*$|/\*$", "", p)`.
fn cut_glob_tail(p: &str) -> String {
    for (i, _) in p.match_indices('/') {
        let r = &p[i..];
        if let Some(after) = r.strip_prefix("/**") {
            // `.*$`: to the end of the line, which must be the end of the text or sit just before a final newline
            let line = after.find('\n').unwrap_or(after.len());
            if line == after.len() {
                return p[..i].to_owned();
            }
            if after[line..] == *"\n" {
                return format!("{}\n", &p[..i]);
            }
        }
        if r == "/*" || r == "/*\n" {
            return format!("{}{}", &p[..i], &r[2..]);
        }
    }
    p.to_owned()
}

/// `\s` as the Python takes it.
fn space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `re.sub(r"(?:(?<=\s)|^)\*(?=\s|$)", "x", cmd)`: a `*` that is a word of its own.
fn star_words(cmd: &str) -> String {
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let lone = *c == '*'
            && (i == 0 || space(chars[i - 1]))
            && chars.get(i + 1).is_none_or(|n| space(*n));
        out.push(if lone { 'x' } else { *c });
    }
    out
}

/// `must_ask_rule`: the must-ask class of a permission rule a package allows, as the call it lets through.
pub fn must_ask_rule(
    rule: &str,
    fill: &dyn Fn(&str) -> String,
    pl: &Places,
) -> Option<&'static str> {
    static RULE: LazyLock<Regex> = LazyLock::new(|| rx(r"(?s)^([A-Za-z0-9_]+)(?:\((.*)\))?$"));
    let rule = py_strip(rule);
    let empty = Value::Object(serde_json::Map::new());
    let Some(m) = RULE.captures(rule) else {
        return if rule.starts_with("mcp__") {
            must_ask(rule, &empty, &[], pl)
        } else {
            None
        };
    };
    let tool = m.get(1).map_or("", |g| g.as_str());
    let group = m.get(2).map_or("", |g| g.as_str());
    let arg = fill(group);
    if tool.starts_with("mcp__") || (group.is_empty() && tool != "Bash") {
        return must_ask(tool, &empty, &[], pl);
    }
    if tool == "Bash" {
        let prefix = arg.ends_with(":*") || arg.ends_with(" *");
        let cmd = if prefix {
            &arg[..arg.len() - 2]
        } else {
            &arg[..]
        };
        let cmd = star_words(cmd).replace('*', "");
        // a prefix rule leaves the last word open: a copy's source is not where it writes
        let command = format!("{}{}", py_strip(&cmd), if prefix { " x" } else { "" });
        return must_ask("Bash", &serde_json::json!({ "command": command }), &[], pl);
    }
    if ["Edit", "Write", "Read", "MultiEdit", "NotebookEdit"].contains(&tool) {
        let p = arg.strip_prefix("//").map_or(arg.as_str(), |_| &arg[1..]);
        let p = if p == "/" {
            p.to_owned()
        } else {
            cut_glob_tail(p)
        };
        let p = expand(&p, &pl.home);
        if p.is_empty() {
            return None;
        }
        // a rule wider than a must-ask place is fine: that place's own deny or ask rule still wins inside it
        if pl.is_secret(&p)
            || pl
                .secret_trees()
                .iter()
                .any(|s| !s.contains('*') && holds(s, &p))
        {
            return Some("secret");
        }
        if tool != "Read" && pl.is_trust(&p) {
            return Some("trust");
        }
    }
    None
}

static SENTENCE: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        rx(
            r"(?i)\b(passwords?|tokens?|credentials?|keychain|api[ _-]?keys?|secrets?|logins?|sign[ -]?in)\b",
        ),
        rx(
            r"(?i)allowed\.json|settings(?:\.local)?\.json|capabilities\.yaml|run/once|\.zshrc|\.bashrc|\.profile|launchagents|protected[ -]paths|\bkith[ -]allow\b",
        ),
        rx(r"(?i)\brm\s+-\w*r|\bdelet\w*|\bwipe|\berase|reset --hard|git clean|\boverwrit"),
        rx(
            r"(?i)\b(push\w*|publish\w*|merg\w*|deploy\w*|releas\w*|post\w* to|send\w* (to|an? e-?mail)|e-?mail\w*|tweet\w*|pay\w*|purchas\w*|buy\w*|spend\w*)\b",
        ),
        rx(
            r"(?i)\b(npx|uvx|bunx|pnpx|dlx|eval|inline code|python3? -c|node -e|ba?sh -c|curl[^.]*\|\s*\w*sh)\b",
        ),
    ]
});

/// `sentence_class`: the must-ask class an `autoMode.allow` sentence names, or `None`.
pub fn sentence_class(text: &str) -> Option<&'static str> {
    CLASSES
        .iter()
        .copied()
        .zip(SENTENCE.iter())
        .find(|(_, r)| r.is_match(text))
        .map(|(c, _)| c)
}
