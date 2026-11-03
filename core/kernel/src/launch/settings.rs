//! The settings a launch hands Claude Code inline (`--settings` JSON, the launch object's `settings`), with the permission
//! mode, the directories and the tool allowlist that go with them: a port of the launch path of `kernel/bin/kith`
//! (permission mode, the guest rules, the settings fill, the guest sandbox). Pure: the person, the folders and what the
//! caller learned from programs (the kith binary, the plugin list, the developer folder, the Claude Code version) are
//! arguments; `prepare_folders` is the one function that writes (the folders a run is made in).
//!
//! Left to the caller, because they need a program or the host: `kith-auth resolve` (who the run is for), the
//! version of Claude Code, and for a guest launched with a run token `kith-permit apply` (the grants of an escape card,
//! applied to what this returns).

// A port of one long script: its short names are kept, so the two read line by line.
#![allow(clippy::many_single_char_names)]

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::guard::{
    self, SECRET_RULES, SOFT_DENY, both, dedup, edit_rules, rule_w, secret_paths, valid_domain,
};
use super::machine;
use super::prompt::Registry;
use crate::paths::{Paths, realpath};

/// A run that does not start: the exit code `kith` would give and what it says.
#[derive(Debug)]
pub struct Refusal {
    pub code: i32,
    pub message: String,
}

fn refuse<T>(code: i32, message: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        code,
        message: message.into(),
    })
}

type R<T> = Result<T, Refusal>;

/// The Claude Code version that holds a guest to its read limits (the sandbox's, and the read hook's `onFailure: block`).
pub const MIN_GUEST_CLAUDE: &str = "2.1.295";

/// The hosts `kith-auth agent-access AGENT --json` names (`None`: the program failed): the rows that carry a `domain`. The
/// error says why there is no list, and the run goes on without one.
pub fn guest_domains(answer: Option<&str>) -> Result<Vec<String>, String> {
    let text = answer.ok_or_else(|| "kith-auth agent-access failed".to_owned())?;
    match serde_json::from_str::<Value>(text).map_err(|e| e.to_string())? {
        Value::Array(rows) => Ok(rows
            .iter()
            .filter_map(|r| r.get("domain").and_then(Value::as_str).map(str::to_owned))
            .collect()),
        Value::Object(_) => Ok(vec![]),
        other => Err(format!("agent-access gave {other}")),
    }
}

/// Who the run is for, as `kith-auth resolve` answers (the fields the settings use).
#[derive(Clone, Debug, Default)]
pub struct Member {
    pub owner: bool,
    /// `r["person"]`: `""` for the owner when no person is named.
    pub id: String,
    pub trusted: bool,
    /// The membership's permission mode (anyone but the owner).
    pub mode: String,
    /// The membership's deny rules, as `kith-auth resolve` gives them.
    pub deny: Vec<String>,
}

impl Member {
    pub fn owner() -> Self {
        Member {
            owner: true,
            ..Member::default()
        }
    }

    /// From the JSON `kith-auth resolve` prints.
    pub fn from_resolve(r: &Value) -> Self {
        let text = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Member {
            owner: r.get("owner").is_some_and(truthy),
            id: text("person"),
            trusted: r.get("trusted").is_some_and(truthy),
            mode: text("mode"),
            deny: r
                .get("deny")
                .and_then(Value::as_array)
                .map(|d| {
                    d.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Everything the settings read that is not a file under the agent's folders.
pub struct Input<'a> {
    pub agent: &'a str,
    pub paths: &'a Paths,
    /// The kernel's folder (`$KITH`, `KITH_ROOT`).
    pub kernel: &'a Path,
    /// The agent's live package folder; `pkg` the pinned snapshot (or the same when running the live folder).
    pub src_pkg: &'a Path,
    pub pkg: &'a Path,
    pub member: &'a Member,
    /// The engine is Claude Code: the other engines get no settings.
    pub claude: bool,
    /// `KITH_PERMISSION_MODE`: what the caller asked for.
    pub caller_mode: &'a str,
    /// The project the run is in (`""` for none).
    pub project: &'a str,
    /// The folders of the project chain that exist on this Mac, `~` already expanded (see `project_dirs`).
    pub project_dirs: &'a [String],
    /// `$PWD`.
    pub cwd: &'a str,
    /// `$HOME`.
    pub user_home: &'a Path,
    /// The kith program that answers `kith memory hook` (`kith_paths.py --bin`), when there is one.
    pub kith_bin: Option<&'a str>,
    /// `claude plugin list --json` (`[]` when it failed).
    pub plugins_json: &'a str,
    /// The owner at their own terminal: their plugins stay as their settings have them.
    pub owner_terminal: bool,
    /// The `apiKeyHelper` of a claude-api backup (`""` for none).
    pub api_helper: &'a str,
    /// A fresh id for the folder of a once-allowed retry (`uuid4().hex`).
    pub once_id: &'a str,
    /// `xcode-select -p`, trimmed (`""` when there is none).
    pub developer_dir: &'a str,
    /// Claude Code's temp folder for a guest (`""` for none; `prompt::guest_tmp`).
    pub ctmp: &'a str,
    /// The version `claude --version` printed, for a guest's run (`None`: missing or no version).
    pub claude_version: Option<&'a str>,
    /// The hosts every guest of this agent reaches without a card (`kith-auth agent-access AGENT --json`), or why they could
    /// not be read: the run goes on without them.
    pub guest_domains: Result<&'a [String], &'a str>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

/// What a launch takes from here.
pub struct Output {
    /// `settings`: null for an engine that is not Claude Code.
    pub settings: Option<Value>,
    /// The launch object's `permission_mode`.
    pub permission_mode: Option<String>,
    /// The mode the run is told with `--permission-mode` (empty: none; the defaultMode in the settings then rules).
    pub mode_arg: String,
    pub allowed_tools: Vec<String>,
    pub add_dirs: Vec<String>,
    pub disable_slash_commands: bool,
    /// The working folder the run starts in (a guest's is never wider than they may read).
    pub cwd: String,
    /// Lines `kith` would write to stderr on the way.
    pub warnings: Vec<String>,
}

// ---- the folders of a run

/// Where the run's own folders are.
pub struct Folders {
    pub home: String,
    pub home_real: String,
    pub mem: String,
    pub files: String,
    pub shared: String,
    pub person_file: String,
    /// A guest's workspace (`""` for the owner).
    pub work: String,
}

impl Folders {
    pub fn new(p: &Paths, src_pkg: &Path, agent: &str, member: &Member) -> Self {
        let state = p.state.to_string_lossy();
        let sp = src_pkg.to_string_lossy();
        let home = match sp.strip_suffix("/role") {
            Some(h) => h.to_owned(),
            None => format!("{state}/agents/{agent}"),
        };
        let home_real = s(&realpath(Path::new(&home)));
        let (mem, files, person_file, work) = if member.owner {
            (
                format!("{home}/memory"),
                format!("{home}/files"),
                format!("{state}/user.md"),
                String::new(),
            )
        } else {
            let id = &member.id;
            let files = format!("{home}/files/people/{id}");
            (
                format!("{home}/memory/people/{id}"),
                files.clone(),
                format!("{state}/people/{id}.md"),
                format!("{files}/.work"),
            )
        };
        Folders {
            shared: format!("{home}/memory/shared"),
            home,
            home_real,
            mem,
            files,
            person_file,
            work,
        }
    }

    /// A path in the agent's folder with symlinks resolved (`real` of the script): the home prefix swapped.
    fn real(&self, p: &str) -> String {
        match p.strip_prefix(&self.home) {
            Some(rest) => format!("{}{rest}", self.home_real),
            None => p.to_owned(),
        }
    }
}

/// The folders a run is made in (`mkdir -p`s of the launch): the agent's memory, a guest's own and their workspace.
pub fn prepare_folders(p: &Paths, f: &Folders, member: &Member) -> std::io::Result<()> {
    for d in ["projects", "sessions", "shared"] {
        fs::create_dir_all(format!("{}/memory/{d}", f.home))?;
    }
    if !member.owner {
        for d in [
            format!("{}/projects", f.mem),
            format!("{}/sessions", f.mem),
            f.files.clone(),
        ] {
            fs::create_dir_all(d)?;
        }
        fs::create_dir_all(p.state.join("people"))?;
        fs::create_dir_all(format!("{}/tmp", f.work))?;
        fs::create_dir_all(format!("{}/cache/uv", f.work))?;
    }
    Ok(())
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `shlex.quote`.
fn shquote(x: &str) -> String {
    if !x.is_empty()
        && x.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        x.to_owned()
    } else {
        format!("'{}'", x.replace('\'', "'\"'\"'"))
    }
}

/// `printf %q` of bash 3.2 / 5 for the characters a path or a name can hold.
fn bash_q(x: &str) -> String {
    if x.is_empty() {
        return "''".to_owned();
    }
    if x.chars().any(char::is_control) {
        let mut out = String::from("$'");
        for c in x.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\'' => out.push_str("\\'"),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => out.push_str(&format!("\\{:03o}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::new();
    for (i, c) in x.chars().enumerate() {
        if " !\"$&'()*,;<>?[\\]^`{|}".contains(c) || (c == '#' && i == 0) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `os.path.abspath` for a path that is already absolute: `.`, `..` and doubled slashes folded away.
fn abspath(p: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

/// Bash's `"$dir"/*`: the entries not starting with a dot, sorted.
fn glob(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.into_iter().map(|n| format!("{dir}/{n}")).collect()
}

fn base(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or_default()
}

/// `sort -V` order of two `X.Y.Z` versions: whether `a` is at least `b`.
fn version_at_least(a: &str, b: &str) -> bool {
    let n = |v: &str| {
        v.split('.')
            .map(|x| x.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    n(a) >= n(b)
}

// ---- JSON helpers (Python's `setdefault` on the settings)

fn bad(key: &str) -> Refusal {
    Refusal {
        code: 1,
        message: format!("kith: settings.json: {key} has the wrong type"),
    }
}

fn obj<'a>(m: &'a mut Map<String, Value>, key: &str) -> R<&'a mut Map<String, Value>> {
    m.entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| bad(key))
}

fn list<'a>(m: &'a mut Map<String, Value>, key: &str) -> R<&'a mut Vec<Value>> {
    m.entry(key)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| bad(key))
}

fn has(l: &[Value], x: &str) -> bool {
    l.iter().any(|v| v.as_str() == Some(x))
}

/// `lst.extend(r for r in rules if r not in lst)`.
fn extend_new(l: &mut Vec<Value>, rules: impl IntoIterator<Item = String>) {
    for r in rules {
        if !has(l, &r) {
            l.push(Value::String(r));
        }
    }
}

fn strings(v: &[String]) -> Value {
    Value::Array(v.iter().map(|x| Value::String(x.clone())).collect())
}

/// The ids in `claude plugin list --json`; anything unexpected is none.
fn installed_plugins(text: &str) -> Vec<String> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let ids: Option<Vec<String>> = items
        .iter()
        .map(|p| p.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    ids.unwrap_or_default()
}

/// The folders of the project chain on this Mac (`project-id --info` homes and `--children --all` places), `~` expanded.
pub fn project_dirs(reg: &Registry, project: &str, user_home: &Path) -> Vec<String> {
    if project.is_empty() {
        return vec![];
    }
    let uh = s(user_home);
    let mut found: Vec<String> = vec![];
    let mut add = |h: Option<String>| {
        if let Some(h) = h.filter(|h| !h.is_empty() && h != "-") {
            let h = match h.strip_prefix('~') {
                Some(rest) => format!("{uh}{rest}"),
                None => h,
            };
            if is_dir(&h) {
                found.push(h);
            }
        }
    };
    let text = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).map(str::to_owned);
    for id in reg.chain(project) {
        add(reg.entry(&id).and_then(|e| text(e, "home")));
    }
    for id in reg.held(project) {
        let Some(e) = reg.entry(&id) else { continue };
        // where it is on this Mac: its home, else its first folder that exists
        let mut places: Vec<String> = text(e, "home").into_iter().collect();
        if let Some(Value::Array(ps)) = e.get("paths") {
            places.extend(ps.iter().filter_map(|x| x.as_str().map(str::to_owned)));
        }
        let here = places.into_iter().find(|d| {
            let d = match d.strip_prefix('~') {
                Some(rest) => format!("{uh}{rest}"),
                None => d.clone(),
            };
            !d.is_empty() && realpath(Path::new(&d)).is_dir()
        });
        add(here);
    }
    found
}

// ---- the run

/// The settings, permission mode, directories and tool allowlist of a launch.
pub fn build(i: &Input<'_>) -> R<Output> {
    let p = i.paths;
    let m = i.member;
    let owner = m.owner;
    let env = |k: &str| (i.env)(k).filter(|v| !v.is_empty());
    let kernel = s(i.kernel);
    let state = s(&p.state);
    let state_real = s(&p.state_real);
    let machine = s(&p.machine);
    let agents = s(&p.agents);
    let f = Folders::new(p, i.src_pkg, i.agent, m);
    let src_pkg = s(i.src_pkg);
    let pkg = s(i.pkg);
    let mut warnings: Vec<String> = vec![];

    // The agent's defaultMode is its author's suggestion: this Mac's choice wins over it, the caller's over both. An owner's
    // run of an agent that reads other people's text never starts in bypass; anyone else's mode is the membership's.
    let mut perm = i.caller_mode.to_owned();
    if perm.is_empty() {
        perm = machine::agent_permission_mode(&p.machine, i.agent);
    }
    let reads_others = machine::reads_others(&i.pkg.join("capabilities.yaml"));
    if owner && perm == "bypassPermissions" && reads_others {
        perm = "auto".to_owned();
    }
    if !owner {
        perm.clone_from(&m.mode);
        // Their reads are an allowlist (the sandbox's denyRead, and a read hook that blocks when it fails), which Claude Code
        // has from 2.1.295: an older or unreadable version is no run.
        let ok = i
            .claude_version
            .is_some_and(|v| version_at_least(v, MIN_GUEST_CLAUDE));
        if !ok {
            return refuse(
                3,
                format!(
                    "kith: runs for anyone but the owner need Claude Code {MIN_GUEST_CLAUDE} or newer (read limits); Claude Code is {}",
                    i.claude_version.map_or_else(
                        || "missing or gives no version".to_owned(),
                        |v| format!("version {v}")
                    )
                ),
            );
        }
    }
    perm = machine::current(&perm).to_owned();

    // ---- the guest's deny rules (their view of this agent's own life only)
    let mut deny_rules: Vec<String> = m.deny.clone();
    if !owner {
        deny_rules.extend(guest_deny(i, &f, &agents, &kernel, &src_pkg, &pkg));
    }

    let state_projects = if owner {
        vec![
            format!("{state}/projects"),
            format!("{state_real}/projects"),
        ]
    } else {
        vec![]
    };
    let learned: Vec<String> = if owner {
        vec![
            format!("{src_pkg}/skills/learned"),
            format!("{}/skills/learned", s(&realpath(i.src_pkg))),
        ]
    } else {
        vec![]
    };
    let builtin = i.src_pkg.starts_with(i.kernel.join("agents"));

    // Memory checks and the room ledger
    let ledger_pre = format!(
        "{} pre --agent {} --home {} || true",
        bash_q(&format!("{kernel}/bin/kith-room-ledger")),
        bash_q(i.agent),
        bash_q(&f.home)
    );
    let ledger_post = ledger_pre.replacen(" pre --agent ", " post --agent ", 1);
    let (mut mem_pre, mut mem_post) = (String::new(), String::new());
    if let Some(mbin) = i.kith_bin.filter(|b| !b.is_empty()) {
        mem_pre = format!(
            "{} memory hook pre --agent {} --state {} --agents {}",
            bash_q(mbin),
            bash_q(i.agent),
            bash_q(&state),
            bash_q(&agents)
        );
        mem_post = mem_pre.replacen(" hook pre ", " hook post ", 1);
        // Claude Code blocks a tool call on exit 2, which an older kith binary's usage error also exits with.
        mem_pre.push_str(" || true");
        mem_post.push_str(" || true");
        if !owner {
            let inner = mem_pre.strip_suffix(" || true").unwrap_or(&mem_pre);
            mem_pre = format!(
                "out=$({inner}) || out='{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"the memory check could not run, so this write is refused\"}}}}'; printf '%s\\n' \"$out\""
            );
        }
    } else {
        warnings.push(format!(
            "kith: no kith binary found, so memory writes by {} are not checked or versioned (kith serve start builds it)",
            i.agent
        ));
        if !owner {
            return refuse(
                3,
                format!(
                    "kith: runs for anyone but the owner need the memory check; not running {} for {}",
                    i.agent, m.id
                ),
            );
        }
    }

    // What the run may write without a prompt
    let mut writable: Vec<String> = if owner {
        if builtin {
            vec![]
        } else {
            vec![
                format!("{}/memory", f.home),
                format!("{}/memory", f.home_real),
            ]
        }
    } else {
        vec![
            f.mem.clone(),
            f.real(&f.mem),
            f.shared.clone(),
            f.real(&f.shared),
            f.files.clone(),
            f.real(&f.files),
        ]
    };
    // The run's outbox: anyone but the owner writes that one folder of <machine>/outbox and nothing else in it.
    let mut run_outbox = String::new();
    if !owner
        && let Some(ob) = env("KITH_RUN_OUTBOX").filter(|o| is_dir(o))
        && is_dir(&format!("{machine}/outbox"))
    {
        let ob = realpath(Path::new(&ob));
        let outbox = realpath(&p.machine.join("outbox"));
        if ob.parent() == Some(outbox.as_path())
            && !ob
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            run_outbox = s(&ob);
        }
    }
    if !run_outbox.is_empty() {
        writable.push(run_outbox.clone());
    }
    let writable_all: Vec<String> = writable
        .iter()
        .chain(&learned)
        .chain(&state_projects)
        .cloned()
        .collect();

    // ---- directories
    let kith_real = s(&realpath(i.kernel));
    let mut dirs: Vec<String> = vec![kernel.clone()];
    if owner {
        if kith_real != kernel {
            dirs.push(kith_real.clone());
        }
        if state != kernel {
            dirs.push(state.clone());
        }
        let under = |x: &str, root: &str| x.starts_with(&format!("{root}/"));
        if is_dir(&agents) && !under(&agents, &state) {
            dirs.push(agents.clone());
        }
        let store = s(&p.store);
        if is_dir(&store) && !under(&store, &state) && !under(&store, &agents) {
            dirs.push(store);
        }
        dirs.extend(i.project_dirs.iter().cloned());
        let uh = s(i.user_home);
        for d in [
            f.home.clone(),
            f.home_real.clone(),
            format!("{uh}/kith/agents/{}", i.agent),
            format!("{uh}/hq/agents/{}", i.agent),
        ] {
            if is_dir(&d)
                && d != i.cwd
                && s(&realpath(Path::new(&d))) == f.home_real
                && !dirs.contains(&d)
            {
                dirs.push(d);
            }
        }
    } else {
        dirs = vec![
            kernel.clone(),
            f.mem.clone(),
            f.shared.clone(),
            f.files.clone(),
            format!("{state}/people"),
            pkg.clone(),
        ];
        if is_dir(&format!("{}/data", f.home)) {
            dirs.push(format!("{}/data", f.home));
        }
        if kith_real != kernel {
            dirs.push(kith_real.clone());
        }
        if !i.project.is_empty() && is_dir(&format!("{state}/projects/{}", i.project)) {
            dirs.push(format!("{state}/projects/{}", i.project));
        }
    }

    // A guest's working directory: never the home folder or one that holds a folder they may read.
    let mut cwd = i.cwd.to_owned();
    if !owner && i.claude {
        let uh = s(i.user_home);
        let mut wide = cwd == uh || cwd == s(&realpath(i.user_home));
        wide |= dirs
            .iter()
            .any(|d| *d == cwd || d.starts_with(&format!("{cwd}/")));
        if wide && cwd != f.files {
            warnings.push(format!(
                "kith: working directory {cwd} is wider than a guest may read; working in {}",
                f.files
            ));
            cwd.clone_from(&f.files);
        }
    }

    // The read-only kernel commands PROTOCOL tells agents to run, allowed without a prompt.
    let mut allowed: Vec<String> = vec![];
    // ... under every path the kernel has (logical, resolved, the installed CLI's stable one)
    let kernel_stable = match env("KITH_KERNEL").filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => format!("{}/cli/current/share/kith/kernel", s(&p.machine)),
    };
    for b in dedup([kernel.clone(), kith_real.clone(), kernel_stable]) {
        if b.is_empty() || !is_dir(&format!("{b}/bin")) {
            continue;
        }
        for c in [
            "project-id",
            "project-id --list",
            "project-id --info:*",
            "project-id --children:*",
            "kith-memory search:*",
            "kith-who:*",
            "hq-who:*",
        ] {
            let name = c.split([' ', ':']).next().unwrap_or_default();
            let rest = &c[name.len()..];
            allowed.push(format!("Bash({b}/bin/{c})"));
            allowed.push(format!("Bash(\"{b}/bin/{name}\"{rest})"));
        }
    }
    if !owner {
        allowed.extend(
            [
                "mcp__kithpermit__request_access",
                "mcp__kithpermit__report_bug",
                "mcp__kithpermit__ask_helper",
            ]
            .map(str::to_owned),
        );
    }

    // Skills load lazily for the owner's runs between agents; off for a guest, a shared room, or a role that says so.
    let caps = fs::read(i.pkg.join("capabilities.yaml"))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let global_skills = |v: &str| {
        caps.lines().any(|l| {
            l.strip_prefix("global_skills:")
                .is_some_and(|r| r.trim_start_matches(' ').starts_with(v))
        })
    };
    let disable_slash_commands = if owner {
        env("KITH_ROOM_MEMBERS").is_some() || global_skills("false")
    } else {
        !global_skills("true")
    };

    if !i.claude {
        let launch_mode = if perm.is_empty() {
            Some(machine::global_permission_mode(&p.machine))
        } else {
            Some(Value::String(perm.clone()))
        };
        return Ok(Output {
            settings: None,
            permission_mode: launch_mode.and_then(|v| v.as_str().map(str::to_owned)),
            mode_arg: perm.clone(),
            allowed_tools: vec![],
            add_dirs: dirs,
            disable_slash_commands,
            cwd,
            warnings,
        });
    }

    // ---- the settings
    let mut st = fill(i, &f, &src_pkg, &pkg)?;
    let sm = st.as_object_mut().ok_or_else(|| bad("settings"))?;
    {
        let hooks = obj(sm, "hooks")?;
        list(hooks, "SessionStart")?.push(json!({"hooks": [{"type": "command", "command": format!("\"{kernel}/bin/kith-cost\" tag"), "timeout": 10}]}));
        // Lists the session in the apps when it is a listable terminal run; never fails the session.
        list(hooks, "SessionStart")?.push(json!({"hooks": [{"type": "command", "command": format!("\"{kernel}/bin/kith-sessions\" record"), "timeout": 5}]}));
        for (event, cmd) in [
            ("PreToolUse", &mem_pre),
            ("PostToolUse", &mem_post),
            ("PreToolUse", &ledger_pre),
            ("PostToolUse", &ledger_post),
        ] {
            if !cmd.is_empty() {
                list(hooks, event)?.push(json!({"matcher": "Write|Edit|MultiEdit", "hooks": [{"type": "command", "command": cmd, "timeout": 10}]}));
            }
        }
        // Anyone but the owner: Bash runs in the OS sandbox below, which is the boundary, so a command needs no card.
        if !owner {
            let allow_bash = r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "in the OS sandbox"}}"#;
            list(hooks, "PreToolUse")?.push(json!({"matcher": "Bash", "hooks": [{"type": "command", "command": format!("printf %s {}", shquote(allow_bash)), "timeout": 5}]}));
        }
    }
    let sec = s(&p.secrets);
    let secret_files: Vec<String> = if sec == machine {
        vec![format!("{sec}/env"), format!("{sec}/auth.json")]
    } else {
        vec![format!("{sec}/**")]
    };
    let perms = obj(sm, "permissions")?;
    {
        let deny = list(perms, "deny")?;
        for f in &secret_files {
            deny.push(Value::String(format!("Read(/{f})")));
            deny.push(Value::String(format!("Edit(/{f})")));
        }
        // ~/.kith stays readable, but not the credentials in it, for every run, owner or not.
        let roots = dedup(
            [state.clone(), machine.clone()]
                .iter()
                .flat_map(|r| both(r)),
        );
        for r in &roots {
            for f in SECRET_RULES {
                for tool in ["Read", "Edit"] {
                    let rule = format!("{tool}(/{r}/{f})");
                    if !has(deny, &rule) {
                        deny.push(Value::String(rule));
                    }
                }
            }
        }
        deny.extend(
            deny_rules
                .iter()
                .filter(|d| !d.is_empty())
                .cloned()
                .map(Value::String),
        );
        // The allow lists behind "Always allow" are written by `kith allow` alone.
        let ag: Vec<String> = dedup(
            [agents.clone(), format!("{state}/agents")]
                .iter()
                .flat_map(|r| [r.clone(), s(&realpath(Path::new(r)))]),
        );
        for a in ag {
            let rule = format!("Edit(/{a}/*/allowed.json)");
            if !has(deny, &rule) {
                deny.push(Value::String(rule));
            }
        }
    }
    // The must-ask classes
    let places = guard::Places::new(
        &s(i.user_home),
        &state,
        &machine,
        &agents,
        &s(&p.home),
        i.kernel,
        &src_pkg,
    );
    let must = guard::must_ask_rules(&places);
    {
        extend_new(list(perms, "deny")?, must.deny.clone());
        let ask = list(perms, "ask")?;
        extend_new(ask, must.ask_files.clone());
        if owner {
            extend_new(ask, must.ask_bash.clone());
        }
    }
    {
        let auto = obj(sm, "autoMode")?;
        let own: Vec<String> = auto
            .get("soft_deny")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let soft = dedup(
            std::iter::once("$defaults".to_owned())
                .chain(own)
                .chain(SOFT_DENY.iter().map(|x| (*x).to_owned())),
        );
        auto.insert("soft_deny".to_owned(), strings(&soft));
        // A package may declare its routine work as autoMode.allow sentences; owner runs keep them, others have none.
        let declared: Vec<String> = auto
            .get("allow")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if owner && !declared.is_empty() {
            let all = dedup(std::iter::once("$defaults".to_owned()).chain(declared));
            auto.insert("allow".to_owned(), strings(&all));
        } else {
            auto.remove("allow");
        }
    }
    // The permission rules
    let perms = obj(sm, "permissions")?;
    {
        let allow = list(perms, "allow")?;
        if !owner {
            *allow = machine::strip_broad_bash(std::mem::take(allow), m.trusted);
        }
    }
    // A role rule for a kernel tool names the path bare; a working call with a space quotes it, so add that spelling.
    let kbin = format!("Bash({kernel}/bin/");
    for key in ["allow", "deny"] {
        let rules = list(perms, key)?;
        let found: Vec<String> = rules
            .iter()
            .filter_map(|r| r.as_str())
            .filter(|r| r.starts_with(&kbin))
            .map(str::to_owned)
            .collect();
        for r in found {
            let tail = &r[kbin.len()..];
            let name_len = tail.find([' ', ':', ')']).unwrap_or(tail.len());
            let (name, rest) = tail.split_at(name_len);
            let q = format!("Bash(\"{}{name}\"{rest}", &kbin[5..]);
            if !has(rules, &q) {
                let at = rules
                    .iter()
                    .position(|x| x.as_str() == Some(r.as_str()))
                    .unwrap_or(0);
                rules.insert(at + 1, Value::String(q));
            }
        }
    }
    {
        let allow = list(perms, "allow")?;
        let edits = dedup(
            writable_all
                .iter()
                .filter(|d| !d.trim().is_empty())
                .map(|d| format!("Edit(/{d}/**)")),
        );
        extend_new(allow, edits);
    }
    if perm.is_empty() {
        perms
            .entry("defaultMode")
            .or_insert_with(|| machine::global_permission_mode(&p.machine));
    }
    if let Some(dm) = perms.get_mut("defaultMode") {
        if let Some(cur) = dm.as_str().map(|x| machine::current(x).to_owned()) {
            *dm = Value::String(cur);
        }
        let cap = owner && reads_others;
        if cap && dm.as_str() == Some("bypassPermissions") {
            *dm = Value::String("auto".to_owned());
        }
    }
    let run_mode = if perm.is_empty() {
        perms
            .get("defaultMode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    } else {
        perm.clone()
    };
    if owner {
        // What the owner said is OK for this agent (kith allow).
        let (rules, notes, problem) =
            machine::allowed_for_settings(Path::new(&format!("{}/allowed.json", f.home)));
        if let Some(b) = problem {
            warnings.push(format!("kith: {b}"));
        }
        extend_new(list(perms, "allow")?, rules);
        if !notes.is_empty() {
            let listed = list(obj(sm, "autoMode")?, "allow")?;
            if listed.is_empty() {
                listed.push(Value::String("$defaults".to_owned()));
            }
            extend_new(listed, notes);
        }
        // Auto mode has no prompt: a PermissionDenied hook turns a refusal into a card, a PreToolUse hook lets one retry through.
        if run_mode == "auto" {
            let permit = shquote(&format!("{kernel}/bin/kith-permit"));
            let once_dir = shquote(&format!("{machine}/run/once/{}", i.once_id));
            let hooks = obj(sm, "hooks")?;
            list(hooks, "PermissionDenied")?.push(json!({"hooks": [{"type": "command", "command": format!("{permit} denied --dir {once_dir}"), "timeout": 660}]}));
            list(hooks, "PreToolUse")?.push(json!({"hooks": [{"type": "command", "command": format!("[ -d {once_dir} ] || exit 0; exec {permit} once --dir {once_dir}"), "timeout": 10}]}));
            let perms = obj(sm, "permissions")?;
            let deny = list(perms, "deny")?;
            for r in dedup([machine.clone(), s(&realpath(&p.machine))]) {
                deny.push(Value::String(format!("Edit(/{r}/run/once/**)")));
            }
        }
    }
    if !owner {
        // The sandbox key keeps the place the script gives it (before enabledPlugins); guest_sandbox fills it in.
        sm.entry("sandbox").or_insert(Value::Null);
    }
    {
        let installed = installed_plugins(i.plugins_json);
        let every = machine::plugins(&p.machine);
        let on = obj(sm, "enabledPlugins")?;
        for pl in installed {
            let want = pl == "kith@kith" || pl == "hq@hq" || every.contains(&pl);
            if want || !i.owner_terminal {
                on.entry(pl).or_insert(Value::Bool(want));
            }
        }
    }
    if !i.api_helper.is_empty() {
        sm.insert(
            "apiKeyHelper".to_owned(),
            Value::String(i.api_helper.to_owned()),
        );
    }

    if !owner {
        guest_sandbox(
            i,
            sm,
            &f,
            &GuestCtx {
                cwd: &cwd,
                dirs: &dirs,
                run_outbox: &run_outbox,
                deny_rules: &deny_rules,
                writable_all: &writable_all,
                kernel: &kernel,
                src_pkg: &src_pkg,
                pkg: &pkg,
            },
            &mut warnings,
        )?;
    }

    let launch_mode = if perm.is_empty() {
        st.get("permissions")
            .and_then(|x| x.get("defaultMode"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        Some(perm.clone())
    };
    Ok(Output {
        settings: Some(st),
        permission_mode: launch_mode,
        mode_arg: perm.clone(),
        allowed_tools: allowed,
        add_dirs: dirs,
        disable_slash_commands,
        cwd,
        warnings,
    })
}

/// The rules the settings file gets with the placeholders filled (`fill < "$pkg/settings.json"`).
fn fill(i: &Input<'_>, f: &Folders, src_pkg: &str, pkg: &str) -> R<Value> {
    let path = i.pkg.join("settings.json");
    let Ok(raw) = fs::read(&path) else {
        return refuse(1, format!("kith: {}: no such file", path.display()));
    };
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    let p = i.paths;
    let fills: [(&str, String); 15] = [
        ("{{MEMORY_DIR}}", f.mem.clone()),
        ("{{SHARED_MEMORY}}", f.shared.clone()),
        ("{{FILES_DIR}}", f.files.clone()),
        ("{{PERSON_FILE}}", f.person_file.clone()),
        ("{{AGENT_DIR}}", pkg.to_owned()),
        ("{{HOME_DIR}}", f.home.clone()),
        ("{{HOME_REAL}}", f.home_real.clone()),
        ("{{STATE}}", s(&p.state)),
        ("{{MACHINE}}", s(&p.machine)),
        ("{{AGENTS}}", s(&p.agents)),
        ("{{PACKAGE}}", src_pkg.to_owned()),
        ("{{USER_HOME}}", s(i.user_home)),
        ("{{KITH}}", s(i.kernel)),
        ("{{HQ}}", s(i.kernel)),
        ("{{AGENT}}", i.agent.to_owned()),
    ];
    for (k, v) in &fills {
        // The text is JSON, so a path goes in as a JSON string body: a backslash or quote in it stays a character of the
        // path. (Bash's sed turns `&` into the placeholder and drops a backslash; see intended.json.)
        let lit = serde_json::to_string(v).unwrap_or_default();
        text = text.replace(k, lit.trim_matches('"'));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => refuse(1, format!("kith: {}: not a JSON object", path.display())),
        Err(e) => refuse(1, format!("kith: {}: {e}", path.display())),
    }
}

/// The deny rules for a guest: other agents' folders and the owner's user.md, then inside this agent the owner's memory
/// and outputs and everyone else's, then what an owner run executes or follows.
fn guest_deny(
    i: &Input<'_>,
    f: &Folders,
    agents: &str,
    kernel: &str,
    src_pkg: &str,
    pkg: &str,
) -> Vec<String> {
    let p = i.paths;
    let state = s(&p.state);
    let state_real = s(&p.state_real);
    let mut deny: Vec<String> = vec![];
    for d in glob(agents) {
        if !is_dir(&d) || base(&d) == i.agent {
            continue;
        }
        for x in [d.clone(), s(&realpath(Path::new(&d)))] {
            deny.push(format!("Read(/{x}/**)"));
            deny.push(format!("Edit(/{x}/**)"));
        }
    }
    deny.push(format!("Read(/{state}/user.md)"));
    deny.push(format!("Read(/{state_real}/user.md)"));
    let mut hide: Vec<String> = vec![];
    for e in glob(&format!("{}/memory", f.home))
        .into_iter()
        .chain(glob(&format!("{}/files", f.home)))
    {
        if !Path::new(&e).exists() {
            continue;
        }
        match base(&e) {
            "shared" if e == format!("{}/memory/shared", f.home) => continue,
            "people" => continue,
            _ => {}
        }
        hide.push(if is_dir(&e) { format!("{e}/**") } else { e });
    }
    for e in glob(&format!("{}/memory/people", f.home))
        .into_iter()
        .chain(glob(&format!("{}/files/people", f.home)))
    {
        if is_dir(&e) && base(&e) != i.member.id {
            hide.push(format!("{e}/**"));
        }
    }
    for e in glob(&format!("{state}/people")) {
        if e.ends_with(".md") && Path::new(&e).is_file() && e != f.person_file {
            hide.push(e);
        }
    }
    for h in hide {
        let swapped = match h.strip_prefix(&state) {
            Some(rest) => format!("{state_real}{rest}"),
            None => h.clone(),
        };
        for x in [h.clone(), f.real(&h), swapped] {
            deny.push(format!("Read(/{x})"));
            deny.push(format!("Edit(/{x})"));
        }
    }
    // Nothing an owner run executes or follows may be written from here: the kernel, this agent's package and the people
    // files. A folder that holds their own memory or outputs is not listed.
    let mine = [&f.mem, &f.files, &f.shared];
    let mine_real = [f.real(&f.mem), f.real(&f.files), f.real(&f.shared)];
    for d in [
        kernel.to_owned(),
        pkg.to_owned(),
        src_pkg.to_owned(),
        format!("{state}/people"),
    ] {
        if !is_dir(&d) {
            continue;
        }
        for x in [d.clone(), s(&realpath(Path::new(&d)))] {
            let holds = |y: &str| format!("{y}/").starts_with(&format!("{x}/"));
            if mine.iter().any(|y| holds(y)) || mine_real.iter().any(|y| holds(y)) {
                continue;
            }
            deny.push(format!("Edit(/{x}/**)"));
        }
    }
    deny
}

struct GuestCtx<'a> {
    cwd: &'a str,
    dirs: &'a [String],
    run_outbox: &'a str,
    deny_rules: &'a [String],
    writable_all: &'a [String],
    kernel: &'a str,
    src_pkg: &'a str,
    pkg: &'a str,
}

/// Anyone but the owner: the OS sandbox around Bash and every process it starts, read limits, and the write rules (W).
fn guest_sandbox(
    i: &Input<'_>,
    sm: &mut Map<String, Value>,
    f: &Folders,
    g: &GuestCtx<'_>,
    warnings: &mut Vec<String>,
) -> R<()> {
    let p = i.paths;
    let env = |k: &str| (i.env)(k).filter(|v| !v.is_empty());
    let state = s(&p.state);
    let machine = s(&p.machine);
    let secrets_dir = s(&p.secrets);
    let uh = s(i.user_home);

    // -- the first block: what the membership and the writable folders say
    let mut deny_read = secret_paths(&[state.clone(), machine.clone()], &secrets_dir, &machine);
    let machine_both = both(&machine);
    if let Some(serve) = env("KITH_SERVE_HOME") {
        let x = match serve.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{uh}{rest}"),
            _ => serve,
        };
        deny_read.extend(both(&x));
    }
    for d in g.deny_rules {
        if d.starts_with("Read(/") && d.ends_with(')') {
            let path = &d[5..d.len() - 1];
            let path = path
                .strip_prefix("//")
                .map_or_else(|| path.to_owned(), |r| format!("/{r}"));
            let keep: Vec<&str> = path
                .split('/')
                .take_while(|part| !part.contains('*'))
                .collect();
            let path = keep.join("/");
            if path.starts_with('/') && path.len() > 1 {
                deny_read.extend(both(&path));
            }
        }
    }
    let allow_write: Vec<String> = g
        .writable_all
        .iter()
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
        .chain(
            [f.work.clone(), i.ctmp.to_owned()]
                .into_iter()
                .filter(|x| !x.is_empty()),
        )
        .collect();
    // The host socket has a fixed place, and one that differs (KITH_RUN_SOCKET) is allowed as well.
    let mut socks: Vec<String> = machine_both
        .iter()
        .flat_map(|m| both(&format!("{m}/run/host.sock")))
        .collect();
    if let Some(sock) = env("KITH_RUN_SOCKET") {
        socks.extend(both(&sock));
    }
    let socks = dedup(socks);
    let allow_write = dedup(allow_write);

    // -- the second block: read allowlist, write rules
    let read_dirs: Vec<String> = std::iter::once(g.cwd.to_owned())
        .chain(g.dirs.iter().cloned())
        .collect();
    let mut allow: Vec<String> = vec![];
    for d in read_dirs.iter().filter(|d| !d.is_empty()) {
        allow.extend(both(&abspath(d)));
    }
    // what a kernel helper needs to start: uv, the pythons it manages, the kith program itself
    for d in [
        f.work.clone(),
        format!("{uh}/.local/share/uv"),
        format!("{uh}/.local/bin"),
    ] {
        if !d.is_empty() {
            allow.extend(both(&d));
        }
    }
    if !i.ctmp.is_empty() {
        allow.push(i.ctmp.to_owned());
    }
    if !g.run_outbox.is_empty() {
        allow.extend(both(g.run_outbox));
    }
    // the developer shims of macOS look the toolchain up here, which can sit on another volume
    let mut dev = i.developer_dir.to_owned();
    if let Some(at) = dev.find(".app/") {
        dev.truncate(at + 4);
    }
    if !dev.is_empty() {
        allow.extend(both(&dev));
    }
    // A link on the way to an allowed folder is read as a link before it is followed: allow the link itself.
    for a in allow.clone() {
        let mut d = Path::new(&a).parent().map(s).unwrap_or_default();
        while d != "/" && !d.is_empty() {
            if fs::symlink_metadata(&d).is_ok_and(|m| m.file_type().is_symlink()) {
                allow.push(d.clone());
            }
            d = Path::new(&d).parent().map(s).unwrap_or_default();
        }
    }
    // Deny by default: nothing is readable but what is allowed below; the named denies stay listed.
    let mut deny_read: Vec<String> = std::iter::once("/".to_owned())
        .chain(dedup(deny_read).into_iter().filter(|d| d != "/"))
        .collect();
    for sys in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/private/etc",
        "/private/var/db/timezone",
        "/private/var/select",
        "/dev",
        "/opt/homebrew",
    ] {
        allow.extend(both(sys));
    }
    allow.push("/var".to_owned());
    allow.push("/etc".to_owned());
    // The Kith folder and the agents folder, dotfiles included, are denied whole; the allowlist opens back only what was given.
    for r in [s(&p.home), s(&p.agents)] {
        if !r.is_empty() {
            for x in both(&abspath(&r)) {
                if !deny_read.contains(&x) {
                    deny_read.push(x);
                }
            }
        }
    }
    // the host socket sits among the machine things: allowing it to be read costs nothing
    allow.extend(socks.iter().cloned());
    let allow_read = dedup(allow);

    // Writes: rule W and the secrets are denied on top, also below the folders that are allowed (deny beats allow).
    let folders: Vec<String> = allow_write
        .iter()
        .cloned()
        .chain(read_dirs.iter().filter(|d| !d.is_empty()).cloned())
        .collect();
    let writable: Vec<String> = allow_write
        .iter()
        .map(|w| s(&realpath(Path::new(w))))
        .collect();
    let extra: Vec<String> = [
        g.kernel.to_owned(),
        format!("{}/role", g.src_pkg),
        format!("{}/skills", g.src_pkg),
        g.pkg.to_owned(),
        g.src_pkg.to_owned(),
        format!("{state}/people"),
        s(&p.store),
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    // A folder that holds something writable (the agent folder holds the person memory) is not denied whole.
    .filter(|x| {
        let xr = s(&realpath(Path::new(x)));
        !writable
            .iter()
            .any(|w| *w == xr || w.starts_with(&format!("{xr}/")))
    })
    .collect();
    let (w_trees, w_files) = rule_w(&uh, &folders, &extra);
    let secrets = secret_paths(&[state.clone(), machine.clone()], &secrets_dir, &machine);
    let deny_write = dedup(
        secrets
            .into_iter()
            .chain(w_trees.iter().cloned())
            .chain(
                w_trees
                    .iter()
                    .filter(|t| t.contains("**"))
                    .map(|t| format!("{t}/**")),
            )
            .chain(w_files.iter().cloned()),
    );

    let perms = obj(sm, "permissions")?;
    let deny = list(perms, "deny")?;
    extend_new(deny, edit_rules(&w_trees, &w_files));
    // The file tools write only where Bash may: a folder the run can read but not write is closed to Edit/Write.
    for d in read_dirs.iter().filter(|d| !d.is_empty()) {
        for x in both(&abspath(d)) {
            let xr = s(&realpath(Path::new(&x)));
            if !writable.iter().any(|w| {
                *w == xr || w.starts_with(&format!("{xr}/")) || xr.starts_with(&format!("{w}/"))
            }) {
                let r = format!("Edit(/{x}/**)");
                if !has(deny, &r) {
                    deny.push(Value::String(r));
                }
            }
        }
    }
    // The file tools stay inside the same folders through a PreToolUse hook (lib/kith_readfence.py), not through
    // permissions.blockReadsOutsideWorkingDirectories: that setting makes Claude Code ask for every Bash command it cannot
    // trace (a cd followed by an export), whatever a hook says, and each ask was a card to the owner. Bash is fenced by the
    // sandbox below.
    let fence_roots = dedup(
        read_dirs
            .iter()
            .cloned()
            .chain([f.work.clone(), i.ctmp.to_owned()])
            .filter(|d| !d.is_empty())
            .flat_map(|d| both(&abspath(&d))),
    );
    let refused = r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "the read check could not run, so this read is refused"}}"#;
    let fence = format!(
        "out=$(python3 -I {}/lib/kith_readfence.py {}) || out={}; printf %s \"$out\"",
        shquote(g.kernel),
        fence_roots
            .iter()
            .map(|r| format!("--root {}", shquote(r)))
            .collect::<Vec<_>>()
            .join(" "),
        shquote(refused)
    );
    list(obj(sm, "hooks")?, "PreToolUse")?.push(json!({
        "matcher": "Read|Grep|Glob|LS|NotebookRead",
        "hooks": [{"type": "command", "command": fence, "timeout": 30, "onFailure": "block"}],
    }));
    // The hosts every guest of this agent reaches without a card; no answer is none.
    let mut network = json!({"allowUnixSockets": socks});
    match i.guest_domains {
        Ok(hosts) => {
            let hosts = dedup(hosts.iter().filter(|h| valid_domain(h)).cloned());
            if !hosts.is_empty() {
                network["allowedDomains"] = json!(hosts);
            }
        }
        Err(why) => warnings.push(format!(
            "kith: could not read the guest domains of the agent ({why}); the run goes on without them"
        )),
    }
    // The sandbox: fails closed, no escape hatch, no network but the host socket. Inherited by children.
    sm.insert(
        "sandbox".to_owned(),
        json!({
            "enabled": true, "failIfUnavailable": true, "allowUnsandboxedCommands": false, "autoAllowBashIfSandboxed": true,
            "filesystem": {
                "denyRead": deny_read, "allowWrite": allow_write, "allowRead": allow_read, "denyWrite": deny_write,
            },
            "network": network,
        }),
    );
    // The tool reads the owner global config, which sits in the closed home folder: point it at nothing instead.
    let env_obj = obj(sm, "env")?;
    env_obj.insert(
        "GIT_CONFIG_GLOBAL".to_owned(),
        Value::String("/dev/null".to_owned()),
    );
    if !f.work.is_empty() {
        env_obj.insert("KITH_WORK".to_owned(), Value::String(f.work.clone()));
    }
    env_obj.insert(
        "CLAUDE_ENV_FILE".to_owned(),
        Value::String(format!("{}/lib/guest-env.sh", g.kernel)),
    );
    Ok(())
}
