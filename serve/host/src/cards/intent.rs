//! What a permission card says about a call, in three voices that never mix (designer's permission-intent spec):
//!
//! - `intent`: the agent's own words (a Bash call's `description`), cleaned to one plain line. A label, never a grant.
//! - `facts`: what the host itself reads from the call: the programs, the sites, the places. Read from the input only; a
//!   fact names what the call names and never claims what a script does.
//! - `check`: the auto-mode classifier's tag as a plain sentence.
//!
//! The rule that matters is that a fact never makes a call look safer than it is. "reads only" is stated only when every
//! stage is a known read-only program and nothing in the command can write, run or substitute; anything the reader
//! can't follow (an unknown flag, `eval`, `xargs`, `find -exec`, a command past 4 KB, more than four programs) becomes
//! the one "Kith can't read in full" row and the card opens Details.
//!
//! Pure: no I/O but the folder labels, which look at the real paths of the Mac.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_INTENT: usize = 200;
const MAX_COMMAND: usize = 4096;
const MAX_ROWS: usize = 4;
const MAX_PROGRAMS: usize = 4;
const MAX_DEPTH: u8 = 3;
const UNREADABLE: &str = "A shell command Kith can't read in full";

// ---- intent

/// The agent's description of a Bash call as one plain line, or none. No markdown, no links, no line breaks, at most
/// 200 characters. Only Bash has one; nothing else the agent wrote fills it.
pub fn intent(tool: &str, input: &Value) -> Option<String> {
    if tool != "Bash" {
        return None;
    }
    let text = plain(input["description"].as_str()?);
    (!text.is_empty()).then_some(text)
}

fn plain(raw: &str) -> String {
    // [label](url) keeps the label; a bare link keeps its host.
    let mut s = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close) = chars[i..].iter().position(|c| *c == ']').map(|p| i + p)
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = chars[close..].iter().position(|c| *c == ')').map(|p| close + p)
        {
            s.extend(&chars[i + 1..close]);
            i = end + 1;
            continue;
        }
        s.push(chars[i]);
        i += 1;
    }
    let words: Vec<String> = s
        .split_whitespace()
        .map(|w| match url_host(w.trim_matches(['(', ')', '<', '>', '"', '\'', ',', '.'])) {
            Some(Host::Name(h)) => h,
            _ => w.to_owned(),
        })
        .collect();
    let joined: String = words.join(" ").chars().filter(|c| !c.is_control() && !matches!(c, '`' | '*')).collect();
    let trimmed = joined.trim_start_matches(['#', '>', '-', ' ']).trim();
    let cut: String = trimmed.chars().take(MAX_INTENT).collect();
    cut.trim().to_owned()
}

// ---- check

/// The classifier's tag as a sentence for the card, or none when the tag explains nothing (or is not one we know).
pub fn check(reason: &str) -> Option<String> {
    let tag = reason.split_once('[')?.1.split_once(']')?.0.trim().to_lowercase();
    let line = match tag.as_str() {
        "code from external" => "runs code it downloaded",
        "bot check circumvention" => "it presents itself as a web browser to get past the site's bot check",
        "auto-mode bypass" => "it hides what it does inside a script",
        "credential materialization" => "it reads login or key details",
        "interfere with workloads" => "it stops programs that are running",
        "unauthorized access" => "it reads a place it wasn't given",
        "destructive or unwanted actions" => "it could delete or overwrite things",
        "create unsafe agents" => "it starts an agent with fewer checks",
        _ => return None,
    };
    Some(format!("Safety check stopped it: {line}"))
}

// ---- places

/// What a folder is to this agent, for the muted words after a fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    /// How the place is written when the folder is only a part of it (`~/.kith/cli` for the installed program).
    pub root: Option<String>,
}

/// The folders the labels are judged by. Only what this Mac's paths say: no agent declares places yet.
#[derive(Debug, Clone)]
pub struct Places {
    home: PathBuf,
    agents: PathBuf,
    kith: PathBuf,
    agent: String,
}

impl Places {
    pub fn here(agent: &str) -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let agents = ["KITH_AGENTS", "HQ_AGENTS"]
            .iter()
            .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
            .map_or_else(|| home.join("kith/agents"), PathBuf::from);
        Self::new(home.clone(), agents, home.join("kith"), agent)
    }

    pub fn new(home: PathBuf, agents: PathBuf, kith: PathBuf, agent: &str) -> Self {
        Self { home, agents, kith, agent: agent.to_owned() }
    }

    fn under(path: &Path, base: &Path) -> bool {
        let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        path.starts_with(base) || real(path).starts_with(real(base))
    }

    fn label(&self, abs: &Path) -> Option<Label> {
        let plain = |t: &str| Some(Label { text: t.to_owned(), root: None });
        let installed = self.home.join(".kith/cli");
        if Self::under(abs, &installed) || Self::under(abs, &self.home.join(".kith/store")) {
            return Some(Label { text: "Kith's installed program".into(), root: Some("~/.kith/cli".into()) });
        }
        let s = abs.to_string_lossy();
        for scratch in ["/private/tmp/", "/tmp/", "/private/var/folders/", "/var/folders/"] {
            if s.starts_with(scratch) {
                return if s.contains(&format!("-agents-{}", self.agent)) {
                    plain("its scratch")
                } else {
                    plain("a scratch folder")
                };
            }
        }
        if let Ok(rest) = abs.strip_prefix(&self.agents).or_else(|_| abs.strip_prefix(&self.kith.join("agents"))) {
            let who = rest.components().next()?.as_os_str().to_string_lossy().into_owned();
            return if who == self.agent {
                plain("its folder")
            } else {
                Some(Label { text: format!("{who}'s folder"), root: None })
            };
        }
        if Self::under(abs, &self.kith) {
            return plain("the kith repo");
        }
        None
    }

    /// `path` as the command spelled it, made absolute when it can be: `~` and the folder a `cd` put it in.
    fn resolve(&self, path: &str, cwd: Option<&Path>) -> Option<PathBuf> {
        if path.contains('$') || path.contains('`') {
            return None;
        }
        let expanded = match path.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None if path == "~" => self.home.clone(),
            None => PathBuf::from(path),
        };
        if expanded.is_absolute() { Some(expanded) } else { cwd.map(|c| c.join(expanded)) }
    }
}

/// The last two parts of a path.
fn tail2(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

// ---- facts

/// One line of "Kith sees".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// terminal, down, globe, pencil, trash, stop, folder, eye or doc.
    pub icon: &'static str,
    pub text: String,
    /// Pieces of `text` to draw in 600, in order.
    pub strong: Vec<String>,
    /// Muted words after the text: whose folder it is.
    pub muted: Option<String>,
    /// The apps add the Mac's name to this line (it replaces "Runs on <Mac> · folder").
    pub place: bool,
}

impl Fact {
    fn json(&self) -> Value {
        let mut v = json!({"icon": self.icon, "text": self.text, "strong": self.strong});
        if let Some(m) = &self.muted {
            v["muted"] = m.clone().into();
        }
        if self.place {
            v["place"] = true.into();
        }
        v
    }
}

/// Text with bold parts: `["Reaches ", B("x")]`.
enum Part<'a> {
    T(&'a str),
    B(String),
}

fn fact(icon: &'static str, parts: &[Part]) -> Fact {
    let mut text = String::new();
    let mut strong = Vec::new();
    for p in parts {
        match p {
            Part::T(t) => text.push_str(t),
            Part::B(b) => {
                text.push_str(b);
                strong.push(b.clone());
            }
        }
    }
    Fact { icon, text, strong, muted: None, place: false }
}

/// What the host reads from a call: the lines, and whether Details should start open (the reader fell short).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub facts: Vec<Fact>,
    pub open_details: bool,
}

fn unreadable() -> Reading {
    Reading {
        facts: vec![Fact { icon: "terminal", text: UNREADABLE.into(), strong: Vec::new(), muted: None, place: false }],
        open_details: true,
    }
}

/// The facts of a call, or none for a call there is nothing to say about.
pub fn reading(tool: &str, input: &Value, places: &Places) -> Option<Reading> {
    let file = |f: &str| input[f].as_str().filter(|s| !s.is_empty());
    let located = |mut f: Fact, path: &str| {
        if let Some(l) = places.resolve(path, None).and_then(|p| places.label(&p)) {
            f.muted = Some(l.text);
        }
        f
    };
    let one = |f: Fact| Some(Reading { facts: vec![f], open_details: false });
    match tool {
        "Bash" => bash(input["command"].as_str()?, places),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = file("file_path").or_else(|| file("notebook_path"))?;
            one(located(fact("pencil", &[Part::T("Changes "), Part::B(tail2(path))]), path))
        }
        "Read" | "NotebookRead" | "LS" => {
            let path = file("file_path").or_else(|| file("notebook_path")).or_else(|| file("path"))?;
            one(located(fact("eye", &[Part::T("Reads "), Part::B(tail2(path))]), path))
        }
        "Glob" | "Grep" => {
            let path = file("path")?;
            one(located(fact("eye", &[Part::T("Reads "), Part::B(tail2(path))]), path))
        }
        "WebFetch" => match url_host(input["url"].as_str()?)? {
            Host::Name(h) => one(fact("globe", &[Part::T("Opens "), Part::B(h)])),
            Host::Dynamic => None,
        },
        t if t.starts_with("mcp__") => {
            let mut it = t.trim_start_matches("mcp__").splitn(2, "__");
            let (server, name) = (it.next()?, it.next().unwrap_or("tool"));
            one(fact("terminal", &[Part::T("Uses "), Part::B(format!("{server} · {name}"))]))
        }
        _ => None,
    }
}

enum Host {
    Name(String),
    Dynamic,
}

/// The host a URL names; none when `s` is not a URL.
fn url_host(s: &str) -> Option<Host> {
    let rest = s.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = auth.rsplit('@').next().unwrap_or_default();
    let host = host.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(host, |(h, _)| h);
    let host = host.trim_matches(['"', '\'']);
    if host.is_empty() || host.contains(['$', '`', '{', '*']) {
        return Some(Host::Dynamic);
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '[' | ']' | ':'))
        .then(|| Host::Name(host.to_lowercase()))
}

// ---- the shell reader

/// One command of a line: its words and what is wired to it.
#[derive(Debug, Default)]
struct Stage {
    words: Vec<String>,
    /// A heredoc or here-string feeds it.
    text_in: bool,
    piped_in: bool,
    /// Targets of `>` and `>>` (a dup like `2>&1` writes nothing and is not here).
    writes: Vec<String>,
    /// The bodies of `$( )` and backticks in its words.
    subs: Vec<String>,
}

struct Scanner {
    c: Vec<char>,
    i: usize,
}

type Scan<T> = Result<T, ()>;

impl Scanner {
    fn peek(&self, n: usize) -> Option<char> {
        self.c.get(self.i + n).copied()
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek(0), Some(' ' | '\t' | '\r')) {
            self.i += 1;
        }
    }

    /// The body of a `( )` that starts after the `(` at `self.i`, up to its mate; leaves `i` after it.
    fn paren_body(&mut self) -> Scan<String> {
        let mut depth = 1;
        let start = self.i;
        while let Some(ch) = self.peek(0) {
            match ch {
                '\\' => self.i += 1,
                '\'' => {
                    self.i += 1;
                    while self.peek(0).is_some_and(|c| c != '\'') {
                        self.i += 1;
                    }
                }
                '"' => {
                    self.i += 1;
                    while self.peek(0).is_some_and(|c| c != '"') {
                        if self.peek(0) == Some('\\') {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let body = self.c[start..self.i].iter().collect();
                        self.i += 1;
                        return Ok(body);
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        Err(())
    }

    /// One shell word, quotes removed; the bodies of any substitutions go to `subs`.
    fn word(&mut self, subs: &mut Vec<String>) -> Scan<String> {
        let mut w = String::new();
        while let Some(ch) = self.peek(0) {
            match ch {
                ' ' | '\t' | '\r' | '\n' | '|' | '&' | ';' | '<' | '>' | '(' | ')' => break,
                '\\' => {
                    match self.peek(1) {
                        Some('\n') | None => {}
                        Some(n) => w.push(n),
                    }
                    self.i += 2;
                }
                '\'' => {
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None => return Err(()),
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(n) => {
                                w.push(n);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None => return Err(()),
                            Some('"') => {
                                self.i += 1;
                                break;
                            }
                            Some('\\') => {
                                if let Some(n) = self.peek(1) {
                                    w.push(n);
                                }
                                self.i += 2;
                            }
                            Some('$') if self.peek(1) == Some('(') => self.dollar(&mut w, subs)?,
                            Some('`') => self.backtick(&mut w, subs)?,
                            Some(n) => {
                                w.push(n);
                                self.i += 1;
                            }
                        }
                    }
                }
                '$' if self.peek(1) == Some('(') => self.dollar(&mut w, subs)?,
                '`' => self.backtick(&mut w, subs)?,
                '$' if self.peek(1) == Some('{') => {
                    while let Some(n) = self.peek(0) {
                        w.push(n);
                        self.i += 1;
                        if n == '}' {
                            break;
                        }
                    }
                }
                n => {
                    w.push(n);
                    self.i += 1;
                }
            }
        }
        Ok(w)
    }

    fn dollar(&mut self, w: &mut String, subs: &mut Vec<String>) -> Scan<()> {
        self.i += 2;
        if self.peek(0) == Some('(') {
            // $(( arithmetic ))
            self.i += 1;
            let body = self.paren_body()?;
            let _ = body;
            if self.peek(0) == Some(')') {
                self.i += 1;
            }
            w.push_str("$((..))");
            return Ok(());
        }
        subs.push(self.paren_body()?);
        w.push_str("$()");
        Ok(())
    }

    fn backtick(&mut self, w: &mut String, subs: &mut Vec<String>) -> Scan<()> {
        self.i += 1;
        let start = self.i;
        while self.peek(0).is_some_and(|c| c != '`') {
            self.i += 1;
        }
        if self.peek(0).is_none() {
            return Err(());
        }
        subs.push(self.c[start..self.i].iter().collect());
        self.i += 1;
        w.push('`');
        Ok(())
    }

    /// Skips the bodies of heredocs started on the line that just ended.
    fn heredoc_bodies(&mut self, pending: &mut Vec<(String, bool)>) {
        for (delim, strip) in pending.drain(..) {
            loop {
                if self.i >= self.c.len() {
                    break;
                }
                let start = self.i;
                while self.peek(0).is_some_and(|c| c != '\n') {
                    self.i += 1;
                }
                let line: String = self.c[start..self.i].iter().collect();
                self.i += 1;
                let line = line.trim_end_matches('\r');
                if (if strip { line.trim_start_matches('\t') } else { line }) == delim {
                    break;
                }
            }
        }
    }
}

fn scan(cmd: &str) -> Scan<Vec<Stage>> {
    let mut s = Scanner { c: cmd.chars().collect(), i: 0 };
    let mut stages = Vec::new();
    let mut cur = Stage::default();
    let mut next_piped = false;
    let mut pending: Vec<(String, bool)> = Vec::new();
    let end = |cur: &mut Stage, stages: &mut Vec<Stage>, piped: bool, next_piped: &mut bool| {
        let done = std::mem::take(cur);
        if !done.words.is_empty() || !done.writes.is_empty() || !done.subs.is_empty() {
            stages.push(done);
        }
        cur.piped_in = piped;
        *next_piped = piped;
    };
    cur.piped_in = false;
    while s.i < s.c.len() {
        s.skip_blanks();
        let Some(ch) = s.peek(0) else { break };
        if ch == '\\' && s.peek(1) == Some('\n') {
            s.i += 2;
            continue;
        }
        if ch == '#' {
            while s.peek(0).is_some_and(|c| c != '\n') {
                s.i += 1;
            }
            continue;
        }
        // An fd before a redirect (`2>&1`, `2>/dev/null`).
        if ch.is_ascii_digit() {
            let mut j = 0;
            while s.peek(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            if matches!(s.peek(j), Some('<' | '>')) {
                s.i += j;
                continue;
            }
        }
        match ch {
            '\n' => {
                s.i += 1;
                end(&mut cur, &mut stages, false, &mut next_piped);
                s.heredoc_bodies(&mut pending);
            }
            '|' => {
                let or = s.peek(1) == Some('|');
                s.i += if or || s.peek(1) == Some('&') { 2 } else { 1 };
                end(&mut cur, &mut stages, !or, &mut next_piped);
            }
            '&' if s.peek(1) == Some('>') => {
                s.i += 1;
            }
            '&' => {
                s.i += if s.peek(1) == Some('&') { 2 } else { 1 };
                end(&mut cur, &mut stages, false, &mut next_piped);
            }
            ';' | '(' | ')' => {
                s.i += 1;
                end(&mut cur, &mut stages, false, &mut next_piped);
            }
            '>' => {
                s.i += 1;
                let mut dup = false;
                match s.peek(0) {
                    Some('>' | '|') => s.i += 1,
                    Some('&') => {
                        s.i += 1;
                        dup = true;
                    }
                    Some('(') => return Err(()),
                    _ => {}
                }
                s.skip_blanks();
                let target = s.word(&mut cur.subs)?;
                if !(dup && (target == "-" || target.bytes().all(|b| b.is_ascii_digit()))) {
                    cur.writes.push(target);
                }
            }
            '<' => {
                s.i += 1;
                match s.peek(0) {
                    Some('(') => return Err(()),
                    Some('<') => {
                        s.i += 1;
                        cur.text_in = true;
                        if s.peek(0) == Some('<') {
                            s.i += 1;
                            s.skip_blanks();
                            s.word(&mut cur.subs)?;
                        } else {
                            let strip = s.peek(0) == Some('-');
                            if strip {
                                s.i += 1;
                            }
                            s.skip_blanks();
                            let delim = s.word(&mut cur.subs)?;
                            pending.push((delim, strip));
                        }
                    }
                    _ => {
                        s.skip_blanks();
                        s.word(&mut cur.subs)?;
                    }
                }
            }
            _ => {
                // (an empty word is a lone quote pair: BSD `sed -i ''`)
                let w = s.word(&mut cur.subs)?;
                cur.words.push(w);
            }
        }
    }
    end(&mut cur, &mut stages, false, &mut next_piped);
    Ok(stages)
}

/// Programs that only read and print. A program is here only with the flags that keep it so (see `read_only`).
const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg", "file", "cut", "tr", "stat", "jq", "ffprobe",
    "diff", "cmp", "basename", "dirname", "which", "du", "realpath", "readlink", "find", "git",
];
/// Shell furniture that does nothing the card needs to name.
const NOISE: &[&str] = &[
    "cd", "echo", "printf", "true", "false", ":", "test", "[", "[[", "export", "unset", "read", "pwd", "sleep", "exit",
];
/// Programs whose arguments are text to match or print, not places to reach.
const TEXT_ARGS: &[&str] = &[
    "echo", "printf", "grep", "egrep", "fgrep", "rg", "sed", "awk", "cat", "head", "tail", "wc", "jq", "tr", "cut",
    "sort", "diff", "test", "[",
];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// One program the command runs, as the card names it.
#[derive(Debug, Clone)]
struct Run {
    /// "a Python script written into the command", "ffmpeg"
    text: String,
    strong: String,
    /// A script or interpreter: the card says "Runs …" rather than "A shell command: …".
    interp: bool,
    read_only: bool,
}

#[derive(Debug, Default)]
struct Folder {
    runs: Vec<Run>,
    noise: Vec<String>,
    packages: Vec<(String, bool)>,
    hosts: Vec<String>,
    dynamic_host: bool,
    saves: Vec<String>,
    /// Places a read-only program was given by address (`grep x /home/kai/.kith/cli/...`).
    reads: Vec<String>,
    deletes: Vec<(String, bool)>,
    stops: Vec<String>,
    /// Programs that did something besides run, in stages counted for the four-program limit.
    stages: usize,
    all_read_only: bool,
    /// Largest repeat of a program: Some(n) a loop of n, None unknown count, and `looped` says one was seen.
    times: u32,
    unknown_loop: bool,
    cwd: Option<PathBuf>,
    /// The `cd` as the command spelled it, when it came before any program.
    first_cd: Option<String>,
    started: bool,
    unreadable: bool,
    loops: Vec<Option<u32>>,
    pending_loop: Option<u32>,
    pending_unknown: bool,
}

impl Folder {
    fn mult(&self) -> Option<u32> {
        self.loops.iter().try_fold(1u32, |acc, m| m.map(|m| acc.saturating_mul(m)))
    }

    fn fold(&mut self, cmd: &str, depth: u8, places: &Places) {
        if depth > MAX_DEPTH {
            self.unreadable = true;
            return;
        }
        let Ok(stages) = scan(cmd) else {
            self.unreadable = true;
            return;
        };
        for st in stages {
            self.stage(st, depth, places);
            if self.unreadable {
                return;
            }
        }
    }

    fn stage(&mut self, st: Stage, depth: u8, places: &Places) {
        if st.writes.iter().any(|w| !w.is_empty()) {
            self.all_read_only = false;
            for w in &st.writes {
                if w != "/dev/null" && !w.is_empty() {
                    self.saves.push(w.clone());
                }
            }
        }
        if !st.subs.is_empty() {
            self.all_read_only = false;
        }
        let mut words: &[String] = &st.words;
        // Control words: loops are counted, the rest is furniture.
        loop {
            let Some(first) = words.first().map(String::as_str) else { break };
            match first {
                "for" => {
                    let n = match words.iter().position(|w| w == "in") {
                        Some(p) if words[p + 1..].iter().all(|w| !w.contains(['$', '*', '?', '`', '~', '{'])) => {
                            Some(u32::try_from(words.len() - p - 1).unwrap_or(u32::MAX))
                        }
                        _ => None,
                    };
                    self.pending_loop = n;
                    self.pending_unknown = n.is_none();
                    words = &[];
                }
                "while" | "until" => {
                    self.pending_loop = None;
                    self.pending_unknown = true;
                    words = &words[1..];
                }
                "do" => {
                    self.loops.push(if self.pending_unknown { None } else { self.pending_loop });
                    self.pending_unknown = false;
                    words = &words[1..];
                }
                "done" => {
                    self.loops.pop();
                    words = &words[1..];
                }
                "if" | "then" | "else" | "elif" | "fi" | "{" | "}" | "!" | "time" => words = &words[1..],
                "case" | "esac" | "select" | "function" => {
                    self.unreadable = true;
                    return;
                }
                _ => break,
            }
        }
        for sub in st.subs.clone() {
            self.fold(&sub, depth + 1, places);
            if self.unreadable {
                return;
            }
        }
        // Assignments and wrappers in front of the program.
        let mut words = words;
        loop {
            let Some(first) = words.first() else { break };
            let is_assign = first.split_once('=').is_some_and(|(n, _)| {
                !n.is_empty()
                    && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !n.starts_with(|c: char| c.is_ascii_digit())
            });
            if is_assign {
                words = &words[1..];
                continue;
            }
            match first.as_str() {
                "env" | "command" | "nohup" | "exec" | "builtin" => {
                    words = &words[1..];
                    while words.first().is_some_and(|w| w.contains('=') && !w.starts_with('-')) {
                        words = &words[1..];
                    }
                    if words.first().is_some_and(|w| w.starts_with('-')) {
                        self.unreadable = true;
                        return;
                    }
                }
                _ => break,
            }
        }
        let Some(prog_word) = words.first() else { return };
        let prog = prog_word.rsplit('/').next().unwrap_or(prog_word).to_owned();
        let args = &words[1..];
        if prog == "cd" {
            let target = args.iter().find(|a| !a.starts_with('-'));
            match target {
                Some(t) => {
                    self.cwd = places.resolve(t, self.cwd.as_deref());
                    if !self.started && self.first_cd.is_none() {
                        self.first_cd = Some(t.clone());
                    }
                }
                None => self.cwd = None,
            }
            return;
        }
        if NOISE.contains(&prog.as_str()) {
            if !self.noise.contains(&prog) {
                self.noise.push(prog);
            }
            return;
        }
        self.started = true;
        self.stages += 1;
        match self.mult() {
            Some(n) => self.times = self.times.max(n),
            None => self.unknown_loop = true,
        }
        let ro = self.program(&prog, args, &st, depth, places);
        if !ro {
            self.all_read_only = false;
        }
        if !TEXT_ARGS.contains(&prog.as_str()) && !self.unreadable {
            for a in args {
                match url_host(a) {
                    Some(Host::Name(h)) => {
                        if !self.hosts.contains(&h) {
                            self.hosts.push(h);
                        }
                    }
                    Some(Host::Dynamic) => self.dynamic_host = true,
                    None => {}
                }
            }
        }
    }

    fn run(&mut self, text: &str, strong: &str, interp: bool, read_only: bool) {
        if !self.runs.iter().any(|r| r.text == text) {
            self.runs.push(Run { text: text.to_owned(), strong: strong.to_owned(), interp, read_only });
        }
    }

    fn tool(&mut self, name: &str, read_only: bool) {
        self.run(name, name, false, read_only);
    }

    /// Reads one program's arguments. True when the stage can only read.
    fn program(&mut self, prog: &str, args: &[String], st: &Stage, depth: u8, places: &Places) -> bool {
        let flag = |a: &String| a.starts_with('-') && a.len() > 1;
        let has = |f: &str| args.iter().any(|a| a == f || a.starts_with(&format!("{f}=")));
        match prog {
            "eval" | "xargs" | "source" | "." => {
                self.unreadable = true;
                false
            }
            p if p.starts_with("python") || p == "pypy" || p == "pypy3" => {
                self.python(args, st);
                false
            }
            "node" | "nodejs" | "bun" | "deno" => {
                let mut inline = st.text_in || st.piped_in;
                let mut it = args.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-e" | "--eval" | "-p" | "--print" => {
                            inline = true;
                            it.next();
                        }
                        "-" => inline = true,
                        "run" | "test" if prog != "node" => {}
                        a if a.starts_with("--") && a.contains('=') => {}
                        "--no-warnings" | "--trace-warnings" | "-v" | "--version" | "--harmony" => {}
                        a if a.starts_with('-') => {
                            self.unreadable = true;
                            return false;
                        }
                        _ => break,
                    }
                }
                let kind = if prog == "node" || prog == "nodejs" { "Node script" } else { "script" };
                self.run(
                    &format!("a {kind}{}", if inline { " written into the command" } else { "" }),
                    kind,
                    true,
                    false,
                );
                false
            }
            "uv" => {
                self.uv(args, st);
                false
            }
            "uvx" => {
                self.launcher(args);
                false
            }
            "npx" | "pnpx" | "bunx" => {
                self.launcher(args);
                false
            }
            "npm" | "pnpm" | "yarn" => {
                if matches!(args.first().map(String::as_str), Some("i" | "install" | "add")) {
                    for a in &args[1..] {
                        if flag(a) {
                            if !matches!(
                                a.as_str(),
                                "-g" | "-D" | "--save" | "--save-dev" | "--global" | "-S" | "--no-save"
                            ) {
                                self.unreadable = true;
                                return false;
                            }
                        } else {
                            self.packages.push((a.clone(), false));
                        }
                    }
                }
                self.tool(prog, false);
                false
            }
            "pip" | "pip3" => {
                self.pip(args);
                self.tool(prog, false);
                false
            }
            "rm" | "rmdir" | "unlink" | "trash" => {
                let mut recursive = false;
                for a in args {
                    if flag(a) {
                        if a.starts_with("--") {
                            match a.as_str() {
                                "--recursive" => recursive = true,
                                "--force" | "--verbose" | "--dir" | "--interactive" | "--one-file-system" => {}
                                _ => {
                                    self.unreadable = true;
                                    return false;
                                }
                            }
                        } else if a[1..].chars().all(|c| "rRfivdI".contains(c)) {
                            recursive |= a.contains(['r', 'R']);
                        } else {
                            self.unreadable = true;
                            return false;
                        }
                    } else {
                        self.deletes.push((a.clone(), recursive || a.contains(['*', '?'])));
                    }
                }
                self.tool(prog, false);
                false
            }
            "find" => {
                let mut ro = true;
                let mut start = Vec::new();
                let mut in_start = true;
                for a in args {
                    if matches!(
                        a.as_str(),
                        "-exec" | "-execdir" | "-ok" | "-okdir" | "-fprint" | "-fprintf" | "-fls" | "-fprint0"
                    ) {
                        self.unreadable = true;
                        return false;
                    }
                    if a == "-delete" {
                        ro = false;
                        self.deletes.push((start.first().cloned().unwrap_or_else(|| ".".into()), true));
                    }
                    if a.starts_with(['-', '(', '!']) {
                        in_start = false;
                    }
                    if in_start {
                        start.push(a.clone());
                    }
                }
                self.tool("find", ro);
                ro
            }
            "git" => {
                let sub = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
                let output_flag = args.iter().any(|a| {
                    a.starts_with("--output")
                        || a == "-c"
                        || a.starts_with("--exec-path")
                        || a.starts_with("--ext-diff")
                });
                let ro = matches!(
                    sub,
                    Some(
                        "status"
                            | "log"
                            | "diff"
                            | "show"
                            | "ls-files"
                            | "rev-parse"
                            | "blame"
                            | "rev-list"
                            | "describe"
                    )
                ) && !output_flag;
                if sub == Some("clean") {
                    self.deletes.push((".".into(), true));
                }
                self.tool("git", ro);
                ro
            }
            "kill" | "pkill" | "killall" => {
                let mut value_next = false;
                let mut targets = Vec::new();
                for a in args {
                    if value_next {
                        value_next = false;
                        continue;
                    }
                    if flag(a) {
                        let f = &a[1..];
                        if matches!(f, "u" | "U" | "t" | "P" | "g" | "G" | "s") {
                            value_next = true;
                        } else if f.bytes().all(|b| b.is_ascii_digit())
                            || f.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                            || f.chars().all(|c| "fxiovnl".contains(c))
                        {
                        } else {
                            self.unreadable = true;
                            return false;
                        }
                    } else {
                        targets.push(a.clone());
                    }
                }
                self.stops.push(if prog == "kill" {
                    targets.join(" ")
                } else {
                    targets.first().cloned().unwrap_or_default()
                });
                self.tool(prog, false);
                false
            }
            "sed" => {
                let mut in_place = false;
                let mut script_given = false;
                let mut positional = Vec::new();
                let mut it = args.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-n" | "-E" | "-r" | "-s" | "--quiet" | "--silent" | "--regexp-extended" | "-u" | "-z" => {}
                        "-e" | "-f" | "--expression" | "--file" => {
                            script_given = true;
                            it.next();
                        }
                        a if a.starts_with("-i") || a == "--in-place" || a.starts_with("--in-place=") => {
                            in_place = true;
                            // BSD: `-i ''` takes the empty extension as its own word.
                            if a == "-i" && it.clone().next().is_some_and(String::is_empty) {
                                it.next();
                            }
                        }
                        a if a.starts_with('-') && a.len() > 1 => {
                            self.unreadable = true;
                            return false;
                        }
                        _ => positional.push(a.clone()),
                    }
                }
                if in_place {
                    let files = if script_given { &positional[..] } else { positional.get(1..).unwrap_or(&[]) };
                    for f in files {
                        self.saves.push(f.clone());
                    }
                }
                self.tool("sed", false);
                false
            }
            "tee" => {
                for a in args.iter().filter(|a| !flag(a)) {
                    self.saves.push(a.clone());
                }
                self.tool("tee", false);
                false
            }
            "curl" | "wget" => {
                let mut it = args.iter();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "-o" | "--output" => {
                            if let Some(v) = it.next() {
                                self.saves.push(v.clone());
                            }
                        }
                        a if a.starts_with("--output=") => self.saves.push(a["--output=".len()..].to_owned()),
                        "-O" if prog == "wget" => {
                            if let Some(v) = it.next() {
                                self.saves.push(v.clone());
                            }
                        }
                        "-O" | "--remote-name" => self.saves.push(".".into()),
                        _ => {}
                    }
                }
                self.tool(prog, false);
                false
            }
            "sort" => {
                if has("-o") || has("--output") {
                    self.tool("sort", false);
                    return false;
                }
                self.tool("sort", true);
                true
            }
            s if SHELLS.contains(&s) => {
                if let Some(p) = args.iter().position(|a| a == "-c" || a == "-lc" || a == "-ic") {
                    match args.get(p + 1) {
                        Some(body) => self.fold(body, depth + 1, places),
                        None => self.unreadable = true,
                    }
                    self.tool(prog, false);
                } else if args.iter().all(|a| flag(a)) && (st.piped_in || st.text_in) {
                    self.run(&format!("whatever is piped into {prog}"), prog, true, false);
                } else {
                    self.tool(prog, false);
                }
                false
            }
            other => {
                // Strict on `-o` for every program (it writes for some), so `grep -o` is not "reads only" either.
                let ro = READ_ONLY.contains(&other) && !(other == "rg" && has("--pre")) && !has("-o");
                if READ_ONLY.contains(&other) {
                    self.reads.extend(args.iter().filter(|a| a.starts_with('/') || a.starts_with("~/")).cloned());
                }
                self.tool(other, ro);
                ro
            }
        }
    }

    fn python(&mut self, args: &[String], st: &Stage) {
        let mut inline = false;
        let mut module: Option<String> = None;
        let mut it = args.iter();
        let mut script = false;
        while let Some(a) = it.next() {
            match a.as_str() {
                "-c" => {
                    inline = true;
                    it.next();
                    break;
                }
                "-m" => {
                    module = it.next().cloned();
                    break;
                }
                "-" => {
                    inline = true;
                    break;
                }
                "-W" | "-X" => {
                    it.next();
                }
                "-I" | "-u" | "-B" | "-E" | "-s" | "-S" | "-q" | "-O" | "-OO" | "-b" | "-d" | "-v" | "-x" => {}
                a if a.starts_with('-') => {
                    self.unreadable = true;
                    return;
                }
                _ => {
                    script = true;
                    break;
                }
            }
        }
        if let Some(m) = module {
            if m == "pip" {
                self.pip(&it.cloned().collect::<Vec<_>>());
            }
            self.run(&format!("the Python module {m}"), &m, true, false);
            return;
        }
        if !script && !inline && (st.text_in || st.piped_in) {
            inline = true;
        }
        let text = if inline { "a Python script written into the command" } else { "a Python script" };
        self.run(text, "Python script", true, false);
    }

    fn pip(&mut self, args: &[String]) {
        if args.first().map(String::as_str) != Some("install") {
            return;
        }
        for a in &args[1..] {
            if a.starts_with('-') {
                if !matches!(
                    a.as_str(),
                    "-U" | "--upgrade"
                        | "-q"
                        | "--quiet"
                        | "--user"
                        | "--break-system-packages"
                        | "--no-cache-dir"
                        | "--no-deps"
                        | "--pre"
                ) {
                    self.unreadable = true;
                    return;
                }
            } else {
                self.packages.push((a.clone(), false));
            }
        }
    }

    fn uv(&mut self, args: &[String], st: &Stage) {
        match args.first().map(String::as_str) {
            Some("run") => {
                let mut it = args[1..].iter().peekable();
                while let Some(a) = it.next() {
                    match a.as_str() {
                        "--with" => match it.next() {
                            Some(p) => self.packages.push((p.clone(), true)),
                            None => self.unreadable = true,
                        },
                        a if a.starts_with("--with=") => self.packages.push((a["--with=".len()..].to_owned(), true)),
                        "-p" | "--python" | "--directory" | "--project" => {
                            it.next();
                        }
                        "-q" | "--quiet" | "--isolated" | "--no-project" | "--no-sync" | "--frozen" | "--locked"
                        | "--offline" | "-s" | "--script" | "--no-dev" | "-v" => {}
                        a if a.starts_with('-') => {
                            self.unreadable = true;
                            return;
                        }
                        first => {
                            let rest: Vec<String> = it.cloned().collect();
                            let name = first.rsplit('/').next().unwrap_or(first);
                            if name.starts_with("python") {
                                self.python(&rest, st);
                            } else if name.ends_with(".py") {
                                self.run("a Python script", "Python script", true, false);
                            } else {
                                self.tool(name, false);
                            }
                            return;
                        }
                    }
                }
                self.tool("uv", false);
            }
            Some("tool") if args.get(1).map(String::as_str) == Some("run") => self.launcher(&args[2..]),
            Some("pip") => {
                self.pip(&args[1..]);
                self.tool("uv", false);
            }
            _ => self.tool("uv", false),
        }
    }

    /// `uvx X` and `npx X`: the first word that is not a flag is a package fetched for this run, and what runs.
    fn launcher(&mut self, args: &[String]) {
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-y" | "--yes" | "-q" | "--quiet" | "--no" | "--no-install" => {}
                "-p" | "--package" | "--from" => {
                    if let Some(p) = it.next() {
                        self.packages.push((p.clone(), true));
                    }
                }
                a if a.starts_with('-') => {
                    self.unreadable = true;
                    return;
                }
                pkg => {
                    self.packages.push((pkg.to_owned(), true));
                    self.tool(pkg.split('@').next().filter(|s| !s.is_empty()).unwrap_or(pkg), false);
                    return;
                }
            }
        }
    }
}

fn number_word(n: u32) -> String {
    const W: [&str; 11] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    W.get(n as usize).map_or_else(|| n.to_string(), |w| (*w).to_owned())
}

/// `a`, `a, and b`, `a, b, and c`.
fn list_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a}, and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn many(items: &[String], shown: usize) -> (Vec<String>, usize) {
    (items.iter().take(shown).cloned().collect(), items.len().saturating_sub(shown))
}

fn bash(command: &str, places: &Places) -> Option<Reading> {
    if command.trim().is_empty() {
        return None;
    }
    if command.len() > MAX_COMMAND {
        return Some(unreadable());
    }
    let mut f = Folder { all_read_only: true, ..Folder::default() };
    f.fold(command, 0, places);
    if f.unreadable || f.stages > MAX_PROGRAMS {
        return Some(unreadable());
    }
    let cwd = f.first_cd.as_deref().and_then(|c| places.resolve(c, None));
    let mut facts: Vec<Fact> = Vec::new();

    // Deletes and stops come first.
    if !f.deletes.is_empty() {
        let mut shown: Vec<String> = Vec::new();
        for (t, recursive) in &f.deletes {
            let abs = places.resolve(t, cwd.as_deref());
            let base = abs.as_ref().map_or_else(|| t.clone(), |p| p.to_string_lossy().into_owned());
            let spot = if *recursive {
                tail2(&base)
            } else {
                abs.as_ref().and_then(|p| p.parent()).map_or_else(|| tail2(t), |p| tail2(&p.to_string_lossy()))
            };
            let spot = if spot.is_empty() { base } else { spot };
            if !shown.contains(&spot) {
                shown.push(spot);
            }
        }
        let recursive = f.deletes.iter().any(|(_, r)| *r);
        let (head, more) = many(&shown, 2);
        let mut parts = vec![Part::T(if recursive { "Deletes " } else { "Deletes files in " })];
        for (i, h) in head.iter().enumerate() {
            if i > 0 {
                parts.push(Part::T(", "));
            }
            parts.push(Part::B(h.clone()));
        }
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        if recursive {
            parts.push(Part::T(" and what is in it"));
        }
        parts.push(Part::T(&tail));
        facts.push(fact("trash", &parts));
    }
    if !f.stops.is_empty() {
        let names: Vec<String> = f.stops.iter().filter(|s| !s.is_empty()).cloned().collect();
        let (head, more) = many(&names, 2);
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        let mut parts = vec![Part::T("Stops processes named ")];
        if head.is_empty() {
            parts = vec![Part::T("Stops processes")];
        }
        for (i, h) in head.iter().enumerate() {
            if i > 0 {
                parts.push(Part::T(", "));
            }
            parts.push(Part::B(h.clone()));
        }
        parts.push(Part::T(&tail));
        facts.push(fact("stop", &parts));
    }

    // What runs.
    let interp = f.runs.iter().any(|r| r.interp);
    // A program with a row of its own (deletes, stops) is not named again here.
    let own_row =
        |r: &&Run| !["rm", "rmdir", "unlink", "trash", "kill", "pkill", "killall"].contains(&r.strong.as_str());
    let listed: Vec<&Run> = {
        let loud: Vec<&Run> = f.runs.iter().filter(|r| !r.read_only).filter(own_row).collect();
        if loud.is_empty() && f.runs.iter().all(|r| r.read_only) { f.runs.iter().collect() } else { loud }
    };
    let repeat = if f.unknown_loop {
        Some("in a loop".to_owned())
    } else if f.times > 1 {
        Some(format!("{} times", number_word(f.times)))
    } else {
        None
    };
    if interp {
        let items: Vec<String> = listed.iter().map(|r| r.text.clone()).collect();
        let mut text = format!("Runs {}", list_and(&items));
        if let Some(r) = &repeat {
            text.push_str(&format!(", {r}"));
        }
        let strong = listed.iter().map(|r| r.strong.clone()).collect();
        facts.push(Fact { icon: "terminal", text, strong, muted: None, place: false });
    } else if !listed.is_empty() || !f.noise.is_empty() {
        let names: Vec<String> =
            if listed.is_empty() { f.noise.clone() } else { listed.iter().map(|r| r.strong.clone()).collect() };
        let mut text = format!("A shell command: {}", names.join(", "));
        if let Some(r) = &repeat {
            text.push_str(&format!(", {r}"));
        }
        if f.all_read_only && !f.runs.is_empty() {
            text.push_str(", reads only");
        }
        facts.push(Fact { icon: "terminal", text, strong: vec!["A shell command".into()], muted: None, place: false });
    }

    // Packages.
    if !f.packages.is_empty() {
        let for_run = f.packages.iter().all(|(_, r)| *r);
        let names: Vec<String> = f.packages.iter().map(|(p, _)| p.clone()).collect();
        let mut parts = vec![Part::T("Fetches the ")];
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                parts.push(Part::T(if i + 1 == names.len() { " and " } else { ", " }));
            }
            parts.push(Part::B(n.clone()));
        }
        parts.push(Part::T(if names.len() > 1 { " packages" } else { " package" }));
        if for_run {
            parts.push(Part::T(" for this run"));
        }
        facts.push(fact("down", &parts));
    }

    // Sites.
    if !f.hosts.is_empty() || f.dynamic_host {
        let (head, more) = many(&f.hosts, 3);
        let mut parts = vec![Part::T("Reaches ")];
        for (i, h) in head.iter().enumerate() {
            if i > 0 {
                parts.push(Part::T(", "));
            }
            parts.push(Part::B(h.clone()));
        }
        let tail = match (more, f.dynamic_host) {
            (0, false) => String::new(),
            (0, true) if head.is_empty() => "a site the command picks".to_owned(),
            (0, true) => " and a site the command picks".to_owned(),
            (n, _) => format!(" and {n} more"),
        };
        parts.push(Part::T(&tail));
        facts.push(fact("globe", &parts));
    }

    // Places it saves into.
    let mut spots: Vec<(String, Option<Label>)> = Vec::new();
    for t in &f.saves {
        // The folder it lands in: a `$` in the file's own name doesn't hide it.
        let dir_part = t.rsplit_once('/').map(|(d, _)| if d.is_empty() { "/" } else { d });
        let dir = match dir_part {
            Some(d) => places.resolve(d, cwd.as_deref()),
            None => cwd.clone(),
        };
        let (shown, label) = match &dir {
            Some(d) => (tail2(&d.to_string_lossy()), places.label(d)),
            None => (tail2(dir_part.unwrap_or(t)), None),
        };
        if !shown.is_empty() && !spots.iter().any(|(s, _)| *s == shown) {
            spots.push((shown, label));
        }
    }
    if !spots.is_empty() {
        let names: Vec<String> = spots.iter().map(|(s, _)| s.clone()).collect();
        let (head, more) = many(&names, 2);
        let mut parts = vec![Part::T("Saves into ")];
        for (i, h) in head.iter().enumerate() {
            if i > 0 {
                parts.push(Part::T(", "));
            }
            parts.push(Part::B(h.clone()));
        }
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        parts.push(Part::T(&tail));
        let mut s = fact("pencil", &parts);
        if spots.len() == 1 {
            s.muted = spots[0].1.as_ref().map(|l| l.text.clone());
        }
        facts.push(s);
    }

    // Places a read-only program was pointed at: one row for the first, the rest only in Details.
    if let Some(path) = f.reads.first().and_then(|r| places.resolve(r, None)) {
        let label = places.label(&path);
        let dir = path.parent().map_or_else(|| path.clone(), Path::to_path_buf);
        let shown = label.as_ref().and_then(|l| l.root.clone()).unwrap_or_else(|| tail2(&dir.to_string_lossy()));
        let more = f.reads.iter().skip(1).filter(|r| **r != f.reads[0]).count();
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        let mut r = fact("eye", &[Part::T("Reads "), Part::B(shown), Part::T(&tail)]);
        r.muted = label.map(|l| l.text);
        facts.push(r);
    }
    // Where it runs: only a `cd` that came before everything else says so.
    if let Some(dir) = &cwd {
        let label = places.label(dir);
        let shown = label.as_ref().and_then(|l| l.root.clone()).unwrap_or_else(|| tail2(&dir.to_string_lossy()));
        let mut w = fact("folder", &[Part::T("In "), Part::B(shown)]);
        w.muted = label.map(|l| l.text);
        w.place = true;
        facts.push(w);
    }
    if facts.len() > MAX_ROWS {
        facts.retain(|f| !f.place);
    }
    if facts.is_empty() {
        return None;
    }
    if facts.len() > MAX_ROWS {
        return Some(unreadable());
    }
    Some(Reading { facts, open_details: false })
}

/// Puts the three voices into a Permission entry's data. `reason` is the classifier's, for an auto-mode card.
/// Nothing is added for a call that has nothing to say, so an older app's plain card stays as it was.
pub fn annotate(data: &mut Value, tool: &str, input: &Value, agent: &str, reason: Option<&str>) {
    if let Some(i) = intent(tool, input) {
        data["intent"] = i.into();
    }
    if let Some(r) = reading(tool, input, &Places::here(agent)) {
        data["facts"] = Value::Array(r.facts.iter().map(Fact::json).collect());
        if r.open_details {
            data["detailsOpen"] = true.into();
        }
        if let Some(c) = reason.and_then(check) {
            data["check"] = c.into();
        }
    }
}
