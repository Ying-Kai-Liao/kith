//! Accounts: the managed set (policy, pins, the state file), what an account id means, whether it is ready, which one a
//! run starts on (`pick`), and the environment it launches with. A port of the read side of `kernel/lib/kith_accounts.py`
//! and the account part of `kernel/lib/kith_settings.py`, which stay the source of truth; `kernel/tests/difftest` fails
//! when the two differ. What it writes is what a run writes: `last_started` in the state file (and a legacy file's marks
//! imported into it), a Claude account's folder made to mirror the main login, and `kith account list` migrating an old
//! settings file. `kith account add|use|remove|pin|out|clear|usage ...` and `kith backup ...` stay Python.
//!
//! Everything outside the file system goes through [`Outside`] (a harness's status command, the `kith-auth granted`
//! lookup, a program on PATH), so each rule is tested in a tempdir with fakes. No token, credential file or Keychain
//! item is ever read here: readiness is asked of the harness's own status command.

use rusqlite::{Connection, OpenFlags, types::Value as Sql};
use serde_json::Value;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod clock;
mod listing;
mod mirror;
mod pick;
mod pyjson;
pub mod settings;
mod state;

pub use pick::Pick;
pub use settings::read_settings;

type Map = serde_json::Map<String, Value>;

/// How long a harness status command or the grants lookup may take (`STATUS_TIMEOUT`).
const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
/// What `logged_in` reads a status command that timed out as (the same code Python's `run_harness` answers).
const TIMED_OUT: i32 = 124;

/// What this module asks of the outside world.
pub trait Outside {
    /// Run a status command with `env` added to the process environment: (exit code, or `None` when the program is not
    /// installed; stdout). A command that outlives [`STATUS_TIMEOUT`] answers `Some(124)`.
    fn status(&self, argv: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String);
    /// The profiles `kith-auth granted --for AGENT SERVICE` names (names only; no secret is read).
    fn granted(&self, agent: &str, service: &str) -> Vec<String>;
    /// Whether a program is on PATH.
    fn on_path(&self, program: &str) -> bool;
}

/// Where things are, and the outside world.
pub struct Ctx<'a> {
    /// `KITH_MACHINE`: `settings.json`, `cost.db` and `accounts/` live here.
    pub machine: PathBuf,
    /// `KITH_AGENTS`: the user's agents.
    pub agents: PathBuf,
    /// The kernel folder (`bin/kith-auth` is named in an API-key entry's helper command).
    pub kernel: PathBuf,
    /// The user's home folder (`~`): the main Claude login's folder is `~/.claude`.
    pub home: PathBuf,
    /// `CLAUDE_CONFIG_DIR` as the run sees it: the main login's folder when it is set and is not one of the accounts' own.
    pub claude_config_dir: Option<String>,
    pub out: &'a dyn Outside,
}

/// The real thing: child processes and the process PATH.
pub struct System {
    pub kernel: PathBuf,
}

impl Outside for System {
    fn status(&self, argv: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
        // A program that is not there (or can't start) is "not installed", as Python's FileNotFoundError reads.
        run_with_timeout(argv, env).unwrap_or((None, String::new()))
    }

    fn granted(&self, agent: &str, service: &str) -> Vec<String> {
        if agent.is_empty() {
            return Vec::new();
        }
        let auth = self.kernel.join("bin/kith-auth");
        match run_with_timeout(
            &[&auth.to_string_lossy(), "granted", "--for", agent, service],
            &[],
        ) {
            Ok((Some(0), out)) => out.split_whitespace().map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }

    fn on_path(&self, program: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path).any(|d| is_executable(&d.join(program)))
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `subprocess.run(..., capture_output, timeout=20, stdin=DEVNULL)`: stdout only; stderr is dropped.
fn run_with_timeout(argv: &[&str], env: &[(&str, &str)]) -> std::io::Result<(Option<i32>, String)> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait()? {
            break status.code().unwrap_or(-1);
        }
        if start.elapsed() >= STATUS_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Ok((Some(TIMED_OUT), String::new()));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok((Some(code), reader.join().unwrap_or_default()))
}

// --- Python's output forms ------------------------------------------------------------------------------------------

/// A JSON value that keeps the order its keys were added in, printed the way Python's `json.dumps` does.
#[derive(Clone, Debug)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
    /// A value read from a file or built as JSON, printed as it is.
    Val(Value),
}

impl J {
    fn s(v: &str) -> J {
        J::Str(v.to_owned())
    }

    fn opt(v: Option<&str>) -> J {
        v.map_or(J::Null, J::s)
    }

    /// `json.dumps(v)`: `", "` and `": "` separators, non-ASCII as `\uXXXX`.
    pub fn dumps(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => {
                let _ = write!(out, "{n}");
            }
            J::Str(s) => pyjson::quote(s, out),
            J::Val(v) => out.push_str(&pyjson::dumps(v)),
            J::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    pyjson::quote(k, out);
                    out.push_str(": ");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// `shlex.quote`.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

// --- entry ids --------------------------------------------------------------------------------------------------------

/// A Claude model name or alias (`CLAUDE_MODEL_RE`); it means nothing to another harness.
pub fn is_claude_model(model: &str) -> bool {
    ["opus", "sonnet", "haiku", "fable", "claude-"]
        .iter()
        .any(|p| model.starts_with(p))
        || model == "best"
        || model == "opusplan"
}

/// `(id without the model, model)`: an entry is `<harness>[:<name>][@<model>]`.
pub fn split(entry: &str) -> (&str, Option<&str>) {
    match entry.split_once('@') {
        Some((base, model)) => (base, Some(model)),
        None => (entry, None),
    }
}

/// What Python's `$` allows before the end: one trailing newline.
fn dollar(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

fn model_ok(model: &str) -> bool {
    let m = dollar(model);
    !m.is_empty()
        && m.chars()
            .all(|c| c.is_alphanumeric() || "_.:[]/-".contains(c))
}

/// `(harness, name)`; the error says what is wrong with the id. A trailing `@model` is checked and left out.
pub fn parse(entry: &str) -> Result<(String, Option<String>), String> {
    let (base, model) = split(entry);
    if let Some(m) = model
        && !model_ok(m)
    {
        return Err(format!(
            "'{base}@{m}': @ needs a model name (claude@sonnet, claude:work@opus)"
        ));
    }
    let id = dollar(base);
    let (harness, name) = match id.split_once(':') {
        Some((h, n)) => (h, Some(n)),
        None => (id, None),
    };
    let mut hc = harness.chars();
    let harness_ok = hc.next().is_some_and(|c| c.is_ascii_lowercase())
        && hc.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let name_ok = name.is_none_or(|n| {
        let mut nc = n.chars();
        nc.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && nc.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    });
    if !harness_ok || !name_ok {
        return Err(format!(
            "'{base}' is not an account id: use <harness> or <harness>:<name>, with @<model> for a model of its own \
             (claude, claude:work, claude@sonnet, claude-api:main, codex)"
        ));
    }
    if harness == "claude-api" && name.is_none() {
        return Err("claude-api needs a profile: claude-api:<profile>".to_owned());
    }
    Ok((harness.to_owned(), name.map(str::to_owned)))
}

/// The entry harnesses that can be an agent's main account: its own, and for Claude an API key too.
pub fn family(harness: &str) -> Vec<&str> {
    match harness {
        "claude" => vec!["claude", "claude-api"],
        h => vec![h],
    }
}

/// An error unless the entry's model can belong to its harness: a Claude model on another one (the B0 rule).
pub fn check_entry_model(entry: &str) -> Result<(), String> {
    let (h, _) = parse(entry)?;
    if let Some(model) = split(entry).1
        && is_claude_model(model)
        && !family("claude").contains(&h.as_str())
    {
        return Err(format!(
            "{model} is a Claude model and {h} is not Claude: use {h}@<a {h} model>, or claude@{model}"
        ));
    }
    Ok(())
}

/// An error unless `entry` can be the pinned account of an agent on `harness`.
pub fn check_main(entry: &str, harness: &str) -> Result<(), String> {
    if split(entry).1.is_some() {
        return Err(format!(
            "{entry}: a pinned account takes no @model (the agent's own model applies); @model is for backups"
        ));
    }
    let (h, _) = parse(entry)?;
    if !family(harness).contains(&h.as_str()) {
        let ok: Vec<&str> = family(harness)
            .into_iter()
            .map(|x| {
                if x == "claude-api" {
                    "claude-api:<profile>"
                } else {
                    x
                }
            })
            .collect();
        return Err(format!(
            "{entry} can't be the account of a {harness} agent: use {} or <harness>:<name> of those",
            ok.join(", ")
        ));
    }
    Ok(())
}

// --- the agent ----------------------------------------------------------------------------------------------------------

/// The agent's `agent.md`, found as `bin/kith`'s `agent_dir` finds it; `None` when there is no such agent.
pub fn agent_md_of(ctx: &Ctx<'_>, agent: &str) -> Option<PathBuf> {
    [
        ctx.agents.join(agent).join("role"),
        ctx.agents.join(agent),
        ctx.kernel.join("agents").join(agent),
    ]
    .into_iter()
    .map(|d| d.join("agent.md"))
    .find(|f| f.is_file())
}

/// The harness `agent.md` names (`claude-code` reads as `claude`); `claude` when it names none or there is no file.
pub fn agent_harness(agent_md: Option<&Path>) -> String {
    let named = agent_md
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| {
            let rest = text.strip_prefix("---\n")?;
            let front = &rest[..rest.find("\n---")?];
            front.lines().find_map(|line| {
                let v = line
                    .strip_prefix("harness:")?
                    .trim_start_matches([' ', '\t']);
                let word: String = v.chars().take_while(|c| !c.is_whitespace()).collect();
                (!word.is_empty()).then_some(word)
            })
        });
    match named.as_deref() {
        None | Some("claude-code") => "claude".to_owned(),
        Some(h) => h.to_owned(),
    }
}

// --- the managed set ------------------------------------------------------------------------------------------------------

/// Whether account id `entry` is of `harness`'s family (the model is not part of it: `claude@sonnet` is a Claude account).
fn in_family(entry: &str, harness: &str) -> bool {
    family(harness).contains(&split(entry).0.split(':').next().unwrap_or_default())
}

/// The agent's pin, then the accounts of its own harness family in set order, then those of other harnesses in set order.
/// Accounts of another harness count only when the agent's own list names them, or cross-harness fallback is on for it.
/// The agent's own list when it has one, else the global set. Without any account of the family: the harness's default
/// login, unless the agent's own list is explicitly empty (then the pin, if any, is all there is).
pub struct Layout {
    pub pin: Option<settings::Pin>,
    pub own: Vec<String>,
    pub foreign: Vec<String>,
}

pub fn layout(s: &Map, agent: &str, harness: &str) -> Layout {
    let (ids, wher) = settings::accounts(s, agent);
    let ids = ids.unwrap_or_default();
    // a hand-edited pin the agent's harness can't use is none
    let pin = settings::pin(s, agent).filter(|p| check_main(&p.id, harness).is_ok());
    let mut own: Vec<String> = ids
        .iter()
        .filter(|i| in_family(i, harness))
        .cloned()
        .collect();
    let mut foreign: Vec<String> = if wher == "agent" || settings::cross_harness(s, agent) {
        ids.iter()
            .filter(|i| !in_family(i, harness))
            .cloned()
            .collect()
    } else {
        vec![]
    };
    if own.is_empty() && !(wher == "agent" && ids.is_empty() && pin.is_some()) {
        own = vec![harness.to_owned()];
    }
    if let Some(p) = &pin {
        own.retain(|i| *i != p.id);
        foreign.retain(|i| *i != p.id);
    }
    Layout { pin, own, foreign }
}

/// The agent's accounts as the settings say, the same on every call: the pin first (alone when it is "only"), then its own
/// harness family, then other harnesses.
pub fn static_order(s: &Map, agent: &str, harness: &str) -> (Vec<String>, Option<settings::Pin>) {
    let l = layout(s, agent, harness);
    match l.pin {
        Some(p) if p.only => (vec![p.id.clone()], Some(p)),
        Some(p) => {
            let order = std::iter::once(p.id.clone())
                .chain(l.own)
                .chain(l.foreign)
                .collect();
            (order, Some(p))
        }
        None => (l.own.into_iter().chain(l.foreign).collect(), None),
    }
}

/// The harness the agent runs on: `harness` when given, else what `agent_md` (or the agent's own `agent.md`) names.
fn harness_for(
    ctx: &Ctx<'_>,
    agent: &str,
    agent_md: Option<&Path>,
    harness: Option<&str>,
) -> String {
    match harness {
        Some(h) if !h.is_empty() => h.to_owned(),
        _ => match agent_md {
            Some(p) if !p.as_os_str().is_empty() => agent_harness(Some(p)),
            _ => agent_harness(agent_md_of(ctx, agent).as_deref()),
        },
    }
}

/// The account the agent's runs start on when nothing is out: the first of its static order.
pub fn main_id(
    ctx: &Ctx<'_>,
    agent: &str,
    agent_md: Option<&Path>,
    harness: Option<&str>,
) -> String {
    let own = harness_for(ctx, agent, agent_md, harness);
    let s = settings::read(ctx);
    static_order(&s, agent, &own).0.swap_remove(0)
}

// --- readiness ----------------------------------------------------------------------------------------------------------

/// `(ready, why)`: the harness's own status command, in the account's dir when it has one.
fn logged_in(ctx: &Ctx<'_>, harness: &str, directory: Option<&Path>) -> (bool, Option<String>) {
    let (argv, var): (&[&str], &str) = if harness == "claude" {
        (&["claude", "auth", "status"], "CLAUDE_CONFIG_DIR")
    } else {
        (&["codex", "login", "status"], "CODEX_HOME")
    };
    let dir = directory.map(|d| d.to_string_lossy().into_owned());
    let env: Vec<(&str, &str)> = dir.as_deref().map(|d| (var, d)).into_iter().collect();
    let (code, out) = ctx.out.status(argv, &env);
    match code {
        None => (false, Some(format!("{harness} is not installed"))),
        Some(TIMED_OUT) => (false, Some(format!("{harness} did not answer"))),
        Some(c) if c != 0 => (false, Some("not logged in".to_owned())),
        Some(_) => {
            if let Ok(Value::Object(d)) = serde_json::from_str::<Value>(&out)
                && d.get("loggedIn") == Some(&Value::Bool(false))
            {
                return (false, Some("not logged in".to_owned()));
            }
            (true, None)
        }
    }
}

pub fn account_dir(ctx: &Ctx<'_>, harness: &str, name: &str) -> PathBuf {
    ctx.machine.join("accounts").join(harness).join(name)
}

/// The one command that makes an entry ready.
pub fn fix(entry: &str, agent: &str) -> Result<Option<String>, String> {
    let (harness, name) = parse(entry)?;
    Ok(match (harness.as_str(), name) {
        ("claude" | "codex", Some(n)) => Some(format!("kith account add {harness} {n}")),
        ("claude", None) => Some("claude auth login".to_owned()),
        ("codex", None) => Some("codex login".to_owned()),
        ("claude-api", n) => {
            let n = n.unwrap_or_default();
            let who = if agent.is_empty() { "<agent>" } else { agent };
            Some(format!(
                "kith auth set anthropic/{n} ANTHROPIC_API_KEY && kith auth grant {who} anthropic anthropic/{n}"
            ))
        }
        _ => None,
    })
}

fn no_key(agent: &str, name: &str) -> String {
    let who = if agent.is_empty() { "the agent" } else { agent };
    format!("no anthropic/{name} key granted to {who}")
}

fn granted(ctx: &Ctx<'_>, agent: &str) -> Vec<String> {
    ctx.out.granted(agent, "anthropic")
}

/// `(ready, why)`. Unknown harnesses are never ready here.
pub fn check(ctx: &Ctx<'_>, entry: &str, agent: &str) -> Result<(bool, Option<String>), String> {
    let (harness, name) = parse(entry)?;
    Ok(match (harness.as_str(), name) {
        ("claude-api", n) => {
            let n = n.unwrap_or_default();
            if !ctx.out.on_path("claude") {
                (false, Some("claude is not installed".to_owned()))
            } else if granted(ctx, agent).contains(&format!("anthropic/{n}")) {
                (true, None)
            } else {
                (false, Some(no_key(agent, &n)))
            }
        }
        ("claude" | "codex", None) => logged_in(ctx, &harness, None),
        ("claude" | "codex", Some(n)) => {
            let d = account_dir(ctx, &harness, &n);
            if d.is_dir() {
                logged_in(ctx, &harness, Some(&d))
            } else {
                (false, Some(format!("no account {n}")))
            }
        }
        (h, _) => (false, Some(format!("{h} is not supported yet"))),
    })
}

/// `(ready, why)` without asking the harness: the account's folder is there, or the API key is granted. For the main
/// account, which every run applies: a logged-out one is the harness's to say, at once and in its words.
fn present(ctx: &Ctx<'_>, entry: &str, agent: &str) -> Result<(bool, Option<String>), String> {
    let (harness, name) = parse(entry)?;
    if let ("claude" | "codex", Some(n)) = (harness.as_str(), name.as_deref())
        && !account_dir(ctx, &harness, n).is_dir()
    {
        return Ok((false, Some(format!("no account {n}"))));
    }
    if harness == "claude-api" {
        let n = name.unwrap_or_default();
        if !granted(ctx, agent).contains(&format!("anthropic/{n}")) {
            return Ok((false, Some(no_key(agent, &n))));
        }
    }
    Ok((true, None))
}

// --- the chain ----------------------------------------------------------------------------------------------------------

fn kind(harness: &str) -> &'static str {
    if harness == "claude-api" {
        "api-key"
    } else {
        "subscription"
    }
}

fn entry_json(ctx: &Ctx<'_>, entry: &str, agent: &str, primary: bool) -> Result<J, String> {
    let (harness, name) = parse(entry)?;
    // The main account on the default login stays ready as it always was: the host may hold that login's keys where
    // `auth status` can't see them. A named or API-key main account is checked like any entry.
    let (ready, why) = if primary && name.is_none() {
        (true, None)
    } else {
        check(ctx, entry, agent)?
    };
    let mut f = vec![
        ("id", J::s(entry)),
        (
            "harness",
            J::s(if harness == "claude-api" {
                "claude"
            } else {
                &harness
            }),
        ),
        ("account", J::opt(name.as_deref())),
        ("model", J::opt(split(entry).1)),
        (
            "kind",
            J::s(if primary { "primary" } else { kind(&harness) }),
        ),
        ("ready", J::Bool(ready)),
    ];
    if !ready {
        f.push(("why", J::opt(why.as_deref())));
    }
    Ok(J::Obj(f))
}

/// `backups-json`: the agent's accounts as a chain for `kith AGENT --backups-json`, the older view the host still reads:
/// its static order (pin first, then its set), entry 0 the primary. No state in it.
pub fn chain(ctx: &Ctx<'_>, agent: &str, agent_md: &Path) -> J {
    let harness = agent_harness(Some(agent_md));
    let s = settings::read(ctx);
    let (ids, _) = static_order(&s, agent, &harness);
    let items = ids
        .iter()
        .enumerate()
        .map(|(n, i)| {
            // a hand-edited bad id: listed as not ready rather than failing the whole chain
            entry_json(ctx, i, agent, n == 0).unwrap_or_else(|e| {
                J::Obj(vec![
                    ("id", J::Str(i.clone())),
                    ("harness", J::Null),
                    ("account", J::Null),
                    ("model", J::Null),
                    ("kind", J::s("unknown")),
                    ("ready", J::Bool(false)),
                    ("why", J::Str(e)),
                ])
            })
        })
        .collect();
    J::Obj(vec![
        ("version", J::Int(1)),
        ("agent", J::s(agent)),
        ("chain", J::Arr(items)),
    ])
}

/// The main account for the apps and `kith account use --json`: with an agent, the first of its static order and where that
/// comes from (`agent`: its pin, `global`: the set, `default`: the harness's login); without, the first of the global set
/// (id null when there is none).
pub fn main_json(ctx: &Ctx<'_>, agent: &str) -> J {
    let s = settings::read(ctx);
    let (entry, harness, source): (Option<String>, Option<String>, &str) = if agent.is_empty() {
        let first = settings::accounts(&s, "")
            .0
            .unwrap_or_default()
            .into_iter()
            .next();
        (first, None, "global")
    } else {
        let md = agent_md_of(ctx, agent);
        let harness = agent_harness(md.as_deref());
        let (order, pin) = static_order(&s, agent, &harness);
        let entry = order.into_iter().next().unwrap_or_default();
        let source = if pin.as_ref().is_some_and(|p| p.id == entry) {
            "agent"
        } else if entry == harness {
            "default"
        } else {
            "global"
        };
        (Some(entry), Some(harness), source)
    };
    let mut out = vec![
        ("id", J::opt(entry.as_deref())),
        ("source", J::s(source)),
        ("harness", J::opt(harness.as_deref())),
    ];
    if let Some(entry) = entry {
        // a hand-edited bad id is reported as not ready, with the parse error as the reason
        let (ready, why) = match entry_json(ctx, &entry, agent, true) {
            Ok(J::Obj(e)) => {
                let ready = e
                    .iter()
                    .any(|(k, v)| *k == "ready" && matches!(v, J::Bool(true)));
                (
                    ready,
                    e.iter().find(|(k, _)| *k == "why").map(|(_, v)| v.clone()),
                )
            }
            Ok(_) => (false, None),
            Err(why) => (false, Some(J::Str(why))),
        };
        out.push(("ready", J::Bool(ready)));
        if !ready {
            out.push(("why", why.unwrap_or(J::Null)));
            if let Ok(f) = fix(&entry, agent) {
                out.push(("fix", J::opt(f.as_deref())));
            }
        }
    }
    J::Obj(out)
}

// --- launch -------------------------------------------------------------------------------------------------------------

/// The apiKeyHelper command: prints the key on demand, run by Claude Code, so the key never passes through us.
fn api_helper(ctx: &Ctx<'_>, agent: &str, name: &str) -> String {
    let auth = ctx.kernel.join("bin/kith-auth");
    let profile = format!("anthropic/{name}");
    [
        auth.to_string_lossy().as_ref(),
        "get",
        "--for",
        agent,
        "--profile",
        &profile,
        "anthropic",
        "ANTHROPIC_API_KEY",
    ]
    .iter()
    .map(|a| sh_quote(a))
    .collect::<Vec<_>>()
    .join(" ")
}

/// `launch-env`: shell assignments for `bin/kith`; the error (exit 4) says why the entry can't run. `light`: the main
/// account's check (`present`), not the harness's status command. `record`: the launch is real, note `last_started`.
pub fn launch_env(
    ctx: &Ctx<'_>,
    agent: &str,
    entry: &str,
    light: bool,
    record: bool,
) -> Result<String, String> {
    let (harness, name) = parse(entry)?;
    if !["claude", "claude-api", "codex"].contains(&harness.as_str()) {
        return Err(format!(
            "{entry}: {harness} is not supported in launch-json yet"
        ));
    }
    let mut model = split(entry).1.map(str::to_owned);
    if model.is_none() && !light {
        // --account claude for a set entry claude@sonnet: the id without its model finds it, unless the set lists the plain
        // id too (the exact entry wins). The set's first entry is the main account (it takes no model), so only the rest
        // are backups.
        let ids: Vec<String> = settings::accounts(&settings::read(ctx), agent)
            .0
            .unwrap_or_default()
            .into_iter()
            .skip(1)
            .collect();
        if !ids.iter().any(|i| i == entry) {
            model = ids.iter().find_map(|i| match split(i) {
                (b, Some(m)) if b == entry && !m.is_empty() => Some(m.to_owned()),
                _ => None,
            });
        }
    }
    let (ready, why) = if light {
        present(ctx, entry, agent)?
    } else {
        check(ctx, entry, agent)?
    };
    if !ready {
        let hint = fix(entry, agent)?;
        return Err(format!("{entry}: {}", why.unwrap_or_default())
            + &hint
                .map(|h| format!(" (make it ready: {h})"))
                .unwrap_or_default());
    }
    let run_harness = if harness == "claude-api" {
        "claude"
    } else {
        harness.as_str()
    };
    let (mut cfg_var, mut cfg_dir) = ("", String::new());
    if let (Some(n), "claude" | "codex") = (name.as_deref(), harness.as_str()) {
        cfg_var = if harness == "claude" {
            "CLAUDE_CONFIG_DIR"
        } else {
            "CODEX_HOME"
        };
        let dir = account_dir(ctx, &harness, n);
        cfg_dir = dir.to_string_lossy().into_owned();
        if harness == "claude" {
            mirror::prepare_claude_dir(ctx, &dir);
        }
    }
    let helper = if harness == "claude-api" {
        api_helper(ctx, agent, name.as_deref().unwrap_or_default())
    } else {
        String::new()
    };
    if record {
        state::record_started(&ctx.machine, entry, now_secs());
    }
    Ok(format!(
        "backup_harness={} backup_account={} backup_cfg_var={} backup_cfg_dir={} backup_api_helper={} backup_model={}",
        sh_quote(run_harness),
        sh_quote(name.as_deref().unwrap_or_default()),
        sh_quote(cfg_var),
        sh_quote(&cfg_dir),
        sh_quote(&helper),
        sh_quote(model.as_deref().unwrap_or_default())
    ))
}

#[allow(clippy::cast_possible_truncation)]
fn now_secs() -> i64 {
    clock::now() as i64
}

/// `bin/kith`'s assignments for a run on `entry` (`main_id` plus `launch-env`'s; empty for a default login). A named account
/// the run can't use is the error (exit 4), with what fixes it.
pub fn env_for(ctx: &Ctx<'_>, agent: &str, entry: &str, record: bool) -> Result<String, String> {
    let env = if parse(entry)?.1.is_none() {
        "backup_harness='' backup_account='' backup_cfg_var='' backup_cfg_dir='' backup_api_helper=''".to_owned()
    } else {
        launch_env(ctx, agent, entry, true, false)
            .map_err(|e| format!("{agent}'s main account {e}; or run on the default login: kith account unpin {agent}"))?
    };
    if record {
        state::record_started(&ctx.machine, entry, now_secs());
    }
    Ok(format!("main_id={} {env}", sh_quote(entry)))
}

/// What `main-env` is asked for besides the agent.
#[derive(Clone, Copy, Debug, Default)]
pub struct MainOpts<'a> {
    /// Start on `pick`'s first candidate, so a run skips accounts that are out, not on the static main.
    pub state: bool,
    /// The launch is real: note `last_started`.
    pub record: bool,
    /// The account a handed-on session was made under: goes first when it is one of the candidates.
    pub prefer: Option<&'a str>,
}

/// `main-env`: the account a plain run starts on, for `bin/kith`. By default the static main (the pin, else the set's first):
/// what a host launch and a guest's run use.
pub fn main_env(
    ctx: &Ctx<'_>,
    agent: &str,
    agent_md: &Path,
    opts: MainOpts<'_>,
) -> Result<String, String> {
    let mut entry = main_id(ctx, agent, Some(agent_md), None);
    if opts.state {
        let s = settings::read(ctx);
        let now = clock::now();
        let state = state::read(&ctx.machine);
        let picked = pick::pick(ctx, &s, agent, Some(agent_md), false, now, &state);
        let mut cands = picked.candidates.clone();
        match opts.prefer {
            Some(p) if cands.iter().any(|c| c == p) && !picked.all_out => {
                cands.retain(|c| c != p);
                cands.insert(0, p.to_owned());
            }
            _ => {
                if cands.first() != Some(&entry)
                    && let Some(why) = picked.skipped.iter().find(|x| x.id == entry)
                {
                    let until = why
                        .status
                        .out_until
                        .filter(|u| *u != 0)
                        .map(|u| format!(" until {}", clock::hhmm(u)))
                        .unwrap_or_default();
                    eprintln!(
                        "kith: {entry} is {}{until}; starting on {}",
                        why.status.status.replace('_', " "),
                        cands.first().cloned().unwrap_or_default()
                    );
                }
            }
        }
        entry = cands.into_iter().next().unwrap_or(entry);
    }
    env_for(ctx, agent, &entry, opts.record)
}

// --- the cost ledger ----------------------------------------------------------------------------------------------------

/// A ledger text column the way Python's truthiness reads it: empty or null is nothing.
fn text(v: &Sql) -> Option<String> {
    match v {
        Sql::Text(s) if !s.is_empty() => Some(s.clone()),
        Sql::Integer(n) if *n != 0 => Some(n.to_string()),
        Sql::Real(f) if *f != 0.0 => Some(f.to_string()),
        _ => None,
    }
}

/// A ledger row's account id; `None` when it can't tell. Rows from before `account_id`: `provider[:account]`; rows from
/// before `provider` don't say.
fn ledger_id(provider: &Sql, account: &Sql, account_id: &Sql) -> Option<String> {
    text(account_id)
        .or_else(|| text(provider).map(|p| text(account).map_or(p.clone(), |a| format!("{p}:{a}"))))
}

/// The ledger, read-only; `None` without a session id or a ledger file.
fn ledger(machine: &Path, sid: &str) -> Option<Connection> {
    let db = machine.join("cost.db");
    if sid.is_empty() || !db.is_file() {
        return None;
    }
    Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

/// The first row of `select <columns> from runs where session = sid`, the columns the `runs` table of this ledger has.
fn run_row(
    c: &Connection,
    sid: &str,
    pick: &dyn Fn(&[String]) -> Option<String>,
) -> rusqlite::Result<Option<Vec<Sql>>> {
    let cols: Vec<String> = c
        .prepare("pragma table_info(runs)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    let Some(select) = pick(&cols) else {
        return Ok(None);
    };
    let mut st = c.prepare(&format!("select {select} from runs where session = ?"))?;
    let n = st.column_count();
    let mut rows = st.query([sid])?;
    match rows.next()? {
        Some(r) => Ok(Some(
            (0..n)
                .map(|i| r.get::<_, Sql>(i))
                .collect::<rusqlite::Result<_>>()?,
        )),
        None => Ok(None),
    }
}

/// The account id the session was made under, from the cost ledger; `None` when the ledger doesn't know it.
pub fn session_account(machine: &Path, sid: &str) -> Option<String> {
    let c = ledger(machine, sid)?;
    let row = run_row(&c, sid, &|cols| {
        let has = |n: &str| cols.iter().any(|c| c == n);
        has("provider").then(|| {
            format!(
                "provider, account, {}",
                if has("account_id") {
                    "account_id"
                } else {
                    "null"
                }
            )
        })
    })
    .ok()??;
    ledger_id(&row[0], &row[1], &row[2])
}

/// What a session started with, from the cost ledger (the run's tag): `harness` is claude for a row from before provider.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionRun {
    pub agent: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub fn session_run(machine: &Path, sid: &str) -> Option<SessionRun> {
    let c = ledger(machine, sid)?;
    let row = run_row(&c, sid, &|cols| {
        Some(format!(
            "agent, {}, model, effort",
            if cols.iter().any(|c| c == "provider") {
                "provider"
            } else {
                "null"
            }
        ))
    })
    .ok()??;
    Some(SessionRun {
        agent: match &row[0] {
            Sql::Text(s) => Some(s.clone()),
            _ => None,
        },
        harness: text(&row[1]).unwrap_or_else(|| "claude".to_owned()),
        model: text(&row[2]),
        effort: text(&row[3]),
    })
}

impl SessionRun {
    /// `session-run SID`'s JSON.
    pub fn json(&self) -> J {
        J::Obj(vec![
            ("agent", J::opt(self.agent.as_deref())),
            ("harness", J::s(&self.harness)),
            ("model", J::opt(self.model.as_deref())),
            ("effort", J::opt(self.effort.as_deref())),
        ])
    }

    /// `session-run SID --sh`: what `bin/kith` evals for a resumed thread.
    pub fn sh(&self) -> String {
        sh_line(&self.harness, self.model.as_deref(), self.effort.as_deref())
    }
}

/// The `--sh` line; also for a session the ledger doesn't know (all empty).
pub fn sh_line(harness: &str, model: Option<&str>, effort: Option<&str>) -> String {
    format!(
        "s_harness={} s_model={} s_effort={}",
        sh_quote(harness),
        sh_quote(model.unwrap_or_default()),
        sh_quote(effort.unwrap_or_default())
    )
}

// --- `kith account list` --------------------------------------------------------------------------------------------------

/// `kith account list [--agent A] [--json]` as `kernel/bin/kith-account` runs it: a settings file still in the older shape
/// is rewritten in the new one first, then every account is listed. The error is a usage error (exit 2).
pub fn list(ctx: &Ctx<'_>, json: bool, agent: &str) -> Result<String, String> {
    let _ = settings::migrate_file(ctx);
    if !agent.is_empty() && agent_md_of(ctx, agent).is_none() {
        return Err(format!("no agent '{agent}' (kith ls)"));
    }
    Ok(listing::list(ctx, json, agent))
}

// --- files ---------------------------------------------------------------------------------------------------------------

/// A name for a temp file in `dir` that no other process or call is using (`mkstemp`'s job).
fn temp_in(dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "{prefix}{}-{nanos:x}-{n}{suffix}",
        std::process::id()
    ))
}

/// A new file for writing, with `mode` (less the umask) on Unix; fails when the name is taken.
fn create_new(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    o.open(path)
}

#[cfg(test)]
mod tests;
