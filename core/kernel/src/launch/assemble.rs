//! The whole launch: `kith AGENT --launch-json` (the launch branch of `kernel/bin/kith`) in Rust. It runs the steps the bash
//! entry runs around the pieces in this folder, in the same order, with the same words on stderr and the same exit codes:
//! who the run is for (`secrets`), the membership's budget, the agent's monthly limit, the pinned snapshot, the project,
//! the prompt, the login (`accounts`), the settings, the MCP servers, then one JSON object on stdout.
//!
//! Programs it still asks, because their logic stays in Python until their step of the plan: `kith-pkg resolve` (the
//! snapshot), `kith-cost limit|spent`, `project-id` (finding the project of the working folder), `kith-auth mcp-config`
//! (A5), `kith-permit apply` (a guest's grants), `claude` and `xcode-select`.
//!
//! The environment is a map this module edits as the bash entry edits its exports; the launch object's `env` is read from it.

// A port of one long script: its short names and step-by-step shape are kept, so the two read side by side.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::format_collect,
    clippy::single_match_else,
    clippy::redundant_closure
)]

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

use super::{machine, mcp, prompt, settings};
use crate::secrets::pyjson;
use crate::{accounts, engines, paths, secrets};

pub type Env = HashMap<String, String>;

/// How long a launch waits for `kith-auth agent-access` (kernel/bin/kith waits the same).
const AGENT_ACCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What `kith AGENT [flags]` was given.
#[derive(Default, Debug)]
pub struct Args {
    pub print: bool,
    pub launch: bool,
    pub hold: Option<String>,
    pub resume: bool,
    pub backup: String,
    pub project: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub harness: Option<String>,
    pub perm_mode: Option<String>,
    /// A flag the entry would refuse: (exit code, the line).
    pub error: Option<(i32, String)>,
}

/// The flag loop of `kernel/bin/kith`.
pub fn parse_args(args: &[String]) -> Args {
    let mut a = Args::default();
    let mut i = 0;
    let need = |i: usize, name: &str| -> Result<String, (i32, String)> {
        args.get(i + 1)
            .cloned()
            .ok_or((1, format!("kith: {name} needs a value")))
    };
    while i < args.len() {
        let x = args[i].as_str();
        let step = match x {
            "--print" => {
                a.print = true;
                1
            }
            "--launch-json" => {
                a.launch = true;
                1
            }
            "--hold" | "--resume" | "--account" | "--backup" | "-p" | "--project" | "-m"
            | "--model" | "-e" | "--effort" | "-H" | "--harness" | "--permission-mode" => {
                let v = match need(i, x) {
                    Ok(v) => v,
                    Err(e) => {
                        a.error = Some(e);
                        return a;
                    }
                };
                match x {
                    "--hold" => a.hold = Some(v),
                    "--resume" => a.resume = true,
                    "--account" | "--backup" => a.backup = v, // --backup: the old name
                    "-p" | "--project" => a.project = v,
                    "-m" | "--model" => a.model = Some(v),
                    "-e" | "--effort" => a.effort = Some(v),
                    "-H" | "--harness" => a.harness = Some(v),
                    _ => a.perm_mode = Some(v),
                }
                2
            }
            "-r" => {
                a.resume = true;
                if args.get(i + 1).is_some_and(|n| !n.starts_with('-')) {
                    2
                } else {
                    1
                }
            }
            "-c" | "--continue" => {
                a.resume = true;
                1
            }
            "--bypass" => {
                a.perm_mode = Some("bypassPermissions".to_owned());
                1
            }
            "--" => break,
            _ if x.chars().any(char::is_whitespace) && x.starts_with('-') => break,
            _ if x.starts_with('-') => {
                a.error = Some((
                    1,
                    format!("kith: unknown flag '{x}' (kith --help lists them)"),
                ));
                return a;
            }
            _ => break,
        };
        i += step;
    }
    a
}

// ---- the environment

/// `kith_env_sync` (lib/kith-env.sh): KITH_<X> and HQ_<X> agree.
pub fn sync_env(env: &mut Env) {
    let names: Vec<String> = env.keys().cloned().collect();
    for n in &names {
        if let Some(b) = n.strip_prefix("HQ_") {
            let b = if b == "BIN" { "KERNEL_BIN" } else { b };
            let k = format!("KITH_{b}");
            if env.get(&k).is_none_or(String::is_empty) {
                let v = env[n].clone();
                env.insert(k, v);
            }
        }
    }
    let names: Vec<String> = env.keys().cloned().collect();
    for n in &names {
        if let Some(b) = n.strip_prefix("KITH_") {
            let v = env[n].clone();
            if b == "BIN" || v.is_empty() {
                continue;
            }
            let b = if b == "KERNEL_BIN" { "BIN" } else { b };
            env.insert(format!("HQ_{b}"), v);
        }
    }
}

fn get<'a>(env: &'a Env, k: &str) -> &'a str {
    env.get(k).map_or("", String::as_str)
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
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

/// Python's `str(v)` for the JSON values `kith-auth resolve` gives.
fn pystr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `repr` of a string.
fn pyrepr(x: &str) -> String {
    if x.contains('\'') && !x.contains('"') {
        format!("\"{x}\"")
    } else {
        format!("'{}'", x.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// Whether `verb` is the core's: a line in `lib/core-switched.txt`, or `KITH_CORE=rust`; `KITH_CORE=python` is the rollback
/// (`kith_core_exec` in lib/kith-core.sh decides the same way for the shell).
///
/// Bash also runs `kith_cutover_refuse` under `KITH_CORE=python`, refusing the rollback of a module whose files the core
/// owns after its cutover. It is left out here on purpose: that refusal only matters for a module that has a cutover step,
/// and `pkg`, `project-id` and `cost` have none (the core does not own the cost ledger), so the rollback is always
/// allowed for these verbs. Add the refusal when one of them gets a cutover.
fn switched(kernel: &Path, env: &Env, verb: &str) -> bool {
    match get(env, "KITH_CORE") {
        "python" => return false,
        "rust" => return true,
        _ => {}
    }
    fs::read_to_string(kernel.join("lib/core-switched.txt")).is_ok_and(|t| {
        t.lines()
            .any(|l| l.split_once('\t').is_some_and(|(v, _)| v == verb))
    })
}

/// `kith-cost ARGS` answered by the core in process (`cost` in lib/core-switched.txt): its stdout without the last newline when it
/// exits 0, else `None`, which the callers read as `kith-cost` failing. Only the verbs the launch asks (`spent`, `limit --sh`).
fn cost_core(
    kernel: &Path,
    p: &paths::Paths,
    env: &Env,
    home: &Path,
    person_was: &str,
    args: &[&str],
) -> Option<String> {
    let paths = crate::cost::Paths {
        machine: p.machine.clone(),
        home: home.to_path_buf(),
        kernel: kernel.to_path_buf(),
    };
    let cenv = crate::cost::Env {
        now: env.get("KITH_COST_NOW").cloned(),
        person_was: person_was.to_owned(),
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let out = crate::cost::run(&paths, &cenv, &args);
    (out.code == 0).then(|| out.stdout.trim_end_matches('\n').to_owned())
}

// ---- programs

struct Sys<'a> {
    env: &'a Env,
}

impl Sys<'_> {
    fn cmd(&self, prog: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Command {
        let mut c = Command::new(prog);
        c.args(args).env_clear().envs(self.env).stdin(Stdio::null());
        c
    }

    /// stdout of a program that succeeded; `stderr` shows on ours when `loud`. Trailing newlines dropped as `$(...)` does.
    fn out(&self, prog: impl AsRef<std::ffi::OsStr>, args: &[&str], loud: bool) -> Option<String> {
        let mut c = self.cmd(prog, args);
        c.stderr(if loud {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
        let o = c.output().ok().filter(|o| o.status.success())?;
        Some(
            String::from_utf8_lossy(&o.stdout)
                .trim_end_matches('\n')
                .to_owned(),
        )
    }

    /// [`Self::out`] for a program that may hang: killed after `limit`, which reads as no answer. Quiet.
    fn out_within(
        &self,
        prog: impl AsRef<std::ffi::OsStr>,
        args: &[&str],
        limit: std::time::Duration,
    ) -> Option<String> {
        secrets::run_command_with_timeout(self.cmd(prog, args), limit)
            .map(|o| o.trim_end_matches('\n').to_owned())
    }

    /// A program whose stdout, in the launch, goes to stderr.
    fn run_loud(&self, prog: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> bool {
        let mut c = self.cmd(prog, args);
        let err = std::io::stderr().as_fd_clone();
        c.stdout(err).stderr(Stdio::inherit());
        c.status().is_ok_and(|s| s.success())
    }
}

trait CloneFd {
    fn as_fd_clone(&self) -> Stdio;
}

impl CloneFd for std::io::Stderr {
    fn as_fd_clone(&self) -> Stdio {
        use std::os::fd::AsFd as _;
        self.as_fd()
            .try_clone_to_owned()
            .map_or_else(|_| Stdio::null(), Stdio::from)
    }
}

/// A shell word list as `eval` reads it (the output of `launch-env`, `kith-cost limit --sh`): `name=value` words.
fn assignments(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        while i < cs.len() && cs[i].is_whitespace() {
            i += 1;
        }
        let mut word = String::new();
        while i < cs.len() && !cs[i].is_whitespace() {
            match cs[i] {
                '\'' => {
                    i += 1;
                    while i < cs.len() && cs[i] != '\'' {
                        word.push(cs[i]);
                        i += 1;
                    }
                    i += 1;
                }
                '"' => {
                    i += 1;
                    while i < cs.len() && cs[i] != '"' {
                        if cs[i] == '\\' && i + 1 < cs.len() {
                            i += 1;
                        }
                        word.push(cs[i]);
                        i += 1;
                    }
                    i += 1;
                }
                '\\' if i + 1 < cs.len() => {
                    word.push(cs[i + 1]);
                    i += 2;
                }
                c => {
                    word.push(c);
                    i += 1;
                }
            }
        }
        if let Some((k, v)) = word.split_once('=') {
            out.insert(k.to_owned(), v.to_owned());
        }
    }
    out
}

fn is_exec(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn which_in(env: &Env, program: &str) -> Option<PathBuf> {
    std::env::split_paths(get(env, "PATH"))
        .map(|d| d.join(program))
        .find(|p| is_exec(p))
}

/// Whether this process is inside the guest sandbox: the account's home folder (by the OS, not `$HOME`, and by absolute
/// programs, not the caller's `PATH`) can't be listed. No answer is a guest, as in `kith_paths.guest_sandbox`.
fn in_guest_sandbox() -> bool {
    home_closed(secrets::os_home())
}

fn home_closed(home: Option<PathBuf>) -> bool {
    home.is_none_or(|h| fs::read_dir(h).is_err())
}

/// `kith_paths._has_hook` and the checks `kith_bin` makes of `KITH_BIN`: an executable file, not the kernel's bash entry,
/// that answers `memory hook --help` with "phase". Anything else (an older kith, `/bin/true`) is not the program the hooks
/// need, so the caller's own executable stands in.
pub(crate) fn usable_kith_bin(bin: &Path) -> bool {
    if !is_exec(bin) {
        return false;
    }
    let real = fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    // The running program is the usual answer (the router execs with KITH_BIN set to it): it is a kith by being us.
    if std::env::current_exe()
        .and_then(fs::canonicalize)
        .is_ok_and(|me| me == real)
    {
        return true;
    }
    let tail: Vec<_> = real.iter().rev().take(3).collect();
    if tail.len() == 3 && tail[0] == "kith" && tail[1] == "bin" && tail[2] == "kernel" {
        return false;
    }
    secrets::run_with_timeout(
        bin,
        &["memory", "hook", "--help"],
        std::time::Duration::from_secs(10),
    )
    .is_some_and(|o| o.to_lowercase().contains("phase"))
}

/// Whether `bin` has the `kernel readfence` verb: the running program, or one that answers `kernel readfence --help`. An older
/// kith would read `kernel` as an agent name, so the launch keeps the Python fence for it.
pub(crate) fn knows_readfence(bin: &Path) -> bool {
    let real = fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    if std::env::current_exe()
        .and_then(fs::canonicalize)
        .is_ok_and(|me| me == real)
    {
        return true;
    }
    secrets::run_with_timeout(
        bin,
        &["kernel", "readfence", "--help"],
        std::time::Duration::from_secs(10),
    )
    .is_some_and(|o| o.contains("kith kernel readfence"))
}

/// `kith_paths.guest_tmp_ready`: the guest's Claude Code temp folder, made.
fn guest_tmp_ready(p: &paths::Paths, id: &str, uid: u32) -> Option<String> {
    use sha2::{Digest as _, Sha256};
    let hash = |x: &str| {
        use std::fmt::Write as _;
        Sha256::digest(x.as_bytes())
            .iter()
            .fold(String::new(), |mut a, b| {
                let _ = write!(a, "{b:02x}");
                a
            })
    };
    let d = prompt::guest_tmp(&paths::realpath(&p.machine), id, uid, &hash)?;
    let rec = PathBuf::from(format!("{}-who", d.parent()?.display())).join(d.file_name()?);
    for dir in [d.parent()?.to_path_buf(), rec.parent()?.to_path_buf()] {
        fs::create_dir_all(&dir).ok()?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).ok()?;
    }
    let theirs = fs::read_to_string(&rec).is_ok_and(|t| t == id);
    if !theirs || fs::symlink_metadata(&d).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = Command::new("/bin/rm").args(["-rf", "--"]).arg(&d).output();
        fs::write(&rec, id).ok()?;
    }
    fs::create_dir_all(&d).ok()?;
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).ok()?;
    Some(s(&d))
}

/// A frontmatter field the way the entry's `field` awk reads it: the first `key:` line, the rest after the colon.
fn field(agent_md: &Path, key: &str) -> String {
    let Ok(text) = fs::read(agent_md).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        return String::new();
    };
    let mut lines = text.split('\n');
    if lines.next() != Some("---") {
        return String::new();
    }
    for l in lines {
        if l == "---" {
            break;
        }
        let mut w = l.split_whitespace();
        if w.next() == Some(&format!("{key}:")) {
            let rest = &l[l.find(':').map_or(0, |i| i + 1)..];
            return rest.trim_start_matches([' ', '\t']).to_owned();
        }
    }
    String::new()
}

fn claude_version(sys: &Sys<'_>) -> Option<String> {
    let out = sys.out("claude", &["--version"], false)?;
    for line in out.lines() {
        let digits: String = line
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let parts: Vec<&str> = digits.split('.').collect();
        if parts.len() >= 3 && parts.iter().take(3).all(|x| !x.is_empty()) {
            return Some(parts[..3].join("."));
        }
    }
    None
}

/// Digits with at most one inner dot: a finite, non-negative amount (no NaN, inf, sign or exponent).
fn plain_number(t: &str) -> bool {
    !t.is_empty()
        && t.split('.').count() <= 2
        && t.split('.')
            .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
}

/// `uuid4().hex`. An unreadable random source is an error: an all-zero id would be shared by every run, and a run's
/// id and temp file names must not collide.
fn random_hex() -> Result<String, String> {
    let mut b = [0u8; 16];
    #[cfg(unix)]
    fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))
        .map_err(|e| format!("kith: can't read random bytes from /dev/urandom: {e}"))?;
    #[cfg(not(unix))]
    {
        // No /dev/urandom: std's hasher keys come from the OS random source, one fresh key per `RandomState`.
        use std::hash::{BuildHasher, Hasher};
        for chunk in b.chunks_mut(8) {
            let r = std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish();
            chunk.copy_from_slice(&r.to_le_bytes()[..chunk.len()]);
        }
    }
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn pj_indent1(v: &Value) -> String {
    pyjson::dumps_indent_n(v, 1)
}

// ---- the run

/// `kith AGENT [args]` with `--launch-json`: the JSON text on success, or the exit code (what was said is already on stderr).
pub fn launch(agent: &str, argv: &[String], kernel: &Path, env0: Env) -> Result<String, i32> {
    let a = parse_args(argv);
    let mut env = env0;
    sync_env(&mut env);
    let kernel_s = s(kernel);
    env.insert("KITH_ROOT".to_owned(), kernel_s.clone());
    let home_dir = PathBuf::from(get(&env, "HOME"));
    let p = {
        let e = env.clone();
        paths::paths(&home_dir, kernel, &move |k| e.get(k).cloned())
    };
    for (k, v) in p.entries() {
        env.insert(k, s(v));
    }
    let say = |m: &str| eprintln!("{m}");
    let Some(src_pkg) = prompt::agent_dir(&p, kernel, agent) else {
        say(&format!("kith: no agent named \"{agent}\"."));
        return Err(2);
    };
    let home = if src_pkg.file_name().is_some_and(|n| n == "role") {
        src_pkg.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        p.state.join("agents").join(agent)
    };
    let nested = !get(&env, "KITH_AGENT").is_empty();
    let caller = {
        let c = get(&env, "KITH_AGENT");
        if c == agent { "" } else { c }.to_owned()
    };

    // ---- who the run is for
    let mut p_owner = true;
    let mut p_model = String::new();
    let (mut p_line, mut p_id, mut p_was, mut p_rooms) =
        (String::new(), String::new(), String::new(), String::new());
    let mut p_env_names = String::new();
    let mut member = settings::Member::owner();
    let person = get(&env, "KITH_PERSON").to_owned();
    if !person.is_empty() {
        let mut sctx = secrets::Ctx::from_process(kernel);
        sctx.person = Some(person.clone());
        let hold: u64 = get(&env, "KITH_SYNC_HOLD")
            .parse()
            .ok()
            .filter(|_| {
                get(&env, "KITH_SYNC_HOLD")
                    .chars()
                    .all(|c| c.is_ascii_digit())
            })
            .unwrap_or(120);
        let started = std::time::Instant::now();
        let r: Value = loop {
            let mut args = vec![agent.to_owned()];
            if !caller.is_empty() {
                args.extend(["--via".to_owned(), caller.clone(), "--take-once".to_owned()]);
            }
            let o = secrets::run(&sctx, "resolve", &args);
            eprint!("{}", o.stderr);
            let parsed = (o.code == 0)
                .then(|| serde_json::from_str::<Value>(&o.stdout).ok())
                .flatten();
            let Some(r) = parsed else {
                say(&format!(
                    "kith: can't check whether {person} may use {agent} (kith-auth resolve failed)"
                ));
                return Err(1);
            };
            if !r.get("held").is_some_and(truthy) {
                break r;
            }
            if started.elapsed().as_secs() >= hold {
                say(&format!(
                    "kith: out of sync with the Kith cloud, so {person} can't use {agent} on this computer yet; try again in a minute"
                ));
                return Err(3);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        };
        let txt = |k: &str| r.get(k).map(pystr).unwrap_or_default();
        p_owner = r.get("owner").is_some_and(truthy);
        p_id = txt("person");
        p_model = txt("model");
        let clean = |v: &str| -> String {
            if p_owner {
                v.replace('"', "")
            } else {
                v.chars()
                    .filter(|c| !matches!(c, '<' | '>' | '"' | '\u{0}'..='\u{1f}' | '\u{7f}'))
                    .collect()
            }
        };
        let attrs: Vec<String> = ["person", "name", "lang", "tz"]
            .iter()
            .filter_map(|k| {
                r.get(*k)
                    .filter(|v| truthy(v))
                    .map(|v| format!("{k}=\"{}\"", clean(&pystr(v))))
            })
            .collect();
        p_line = format!(" {}", attrs.join(" "));
        p_was = r
            .get("was")
            .and_then(Value::as_array)
            .map(|w| w.iter().map(pystr).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        p_rooms = r
            .get("rooms")
            .and_then(Value::as_array)
            .map(|w| {
                w.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let mut names = vec![];
        if let Some(Value::Object(e)) = r.get("env") {
            for (k, v) in e {
                let plain = !k.is_empty()
                    && k.chars().enumerate().all(|(i, c)| {
                        c == '_' || c.is_ascii_uppercase() || (i > 0 && c.is_ascii_digit())
                    });
                if !p_owner
                    && (!plain
                        || ["PATH", "HOME", "SHELL"].contains(&k.as_str())
                        || ["DYLD_", "LD_", "KITH_", "HQ_"]
                            .iter()
                            .any(|x| k.starts_with(x)))
                {
                    say(&format!(
                        "kith: skipping membership env {} (not allowed)",
                        pyrepr(k)
                    ));
                    continue;
                }
                env.insert(k.clone(), pystr(v));
                names.push(k.clone());
            }
        }
        p_env_names = names.join(" ");
        if !r.get("allowed").is_some_and(truthy) {
            say(&format!(
                "kith: {} (kith-auth member {agent} {person})",
                txt("why")
            ));
            return Err(3);
        }
        member = settings::Member::from_resolve(&r);
        let p_budget = txt("budget");
        let p_budget_agent = r
            .get("budget_agent")
            .filter(|v| truthy(v))
            .map(pystr)
            .unwrap_or_default();
        if !p_budget.is_empty() {
            // A limit that can't be read is not "no limit": for anyone but the owner the run stops here.
            if !p_owner && !plain_number(&p_budget) {
                let on = if p_budget_agent.is_empty() {
                    agent
                } else {
                    &p_budget_agent
                };
                say(&format!(
                    "kith: your spending limit on {on} can't be read; ask the owner to set it again"
                ));
                return Err(3);
            }
            // `cost` in lib/core-switched.txt: the core's own `spent` (kith-cost spent is the rollback, KITH_CORE=python).
            let spent = if switched(kernel, &env, "cost") {
                cost_core(
                    kernel,
                    &p,
                    &env,
                    &home_dir,
                    &p_was,
                    &["spent", &person, &p_budget_agent],
                )
            } else {
                let sys = Sys { env: &env };
                let mut c = sys.cmd(
                    kernel.join("bin/kith-cost"),
                    &["spent", &person, &p_budget_agent],
                );
                c.env("KITH_PERSON_WAS", &p_was).stderr(Stdio::null());
                c.output().ok().filter(|o| o.status.success()).map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim_end_matches('\n')
                        .to_owned()
                })
            };
            let spent = match spent {
                None if p_owner => "0".to_owned(),
                None => {
                    say(&format!(
                        "kith: can't read what {person} has spent this month (kith-cost failed); not running without the budget check"
                    ));
                    return Err(3);
                }
                Some(t) => {
                    if !p_owner && !plain_number(&t) {
                        say(&format!(
                            "kith: can't read what {person} has spent this month (kith-cost gave '{t}'); not running without the budget check"
                        ));
                        return Err(3);
                    }
                    t
                }
            };
            let f = |x: &str| x.trim().parse::<f64>().ok();
            if let (Some(sp), Some(bu)) = (f(&spent), f(&p_budget))
                && sp >= bu
            {
                if p_budget_agent.is_empty() {
                    say(&format!(
                        "kith: your bots have used ${sp:.2} of their ${p_budget} this month together ({person} was stopped); the owner can raise it (kith auth bots-cap ..)"
                    ));
                } else {
                    say(&format!(
                        "kith: {person} has used ${sp:.2} of their ${p_budget} this month on {p_budget_agent}; the owner can raise it (kith auth member {p_budget_agent} {person} budget=..)"
                    ));
                }
                if !p_owner {
                    return Err(3);
                }
            }
        }
    }
    if p_owner && in_guest_sandbox() {
        say("kith: this is inside a guest's run, which can't start a run as the owner");
        return Err(3);
    }
    for d in ["projects", "sessions", "shared"] {
        let _ = fs::create_dir_all(home.join("memory").join(d));
    }
    env.remove("KITH_WORK");
    let (mut files, mut mem) = (home.join("files"), home.join("memory"));
    let mut ctmp = String::new();
    let mut ctmp_note = String::new();
    if !p_owner {
        let plain = p_id.starts_with(|c: char| c.is_ascii_lowercase())
            && p_id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !plain {
            say(&format!("kith: bad person id '{p_id}' from kith-auth"));
            return Err(3);
        }
        mem = home.join("memory/people").join(&p_id);
        files = home.join("files/people").join(&p_id);
        for d in [
            mem.join("projects"),
            mem.join("sessions"),
            files.clone(),
            p.state.join("people"),
        ] {
            let _ = fs::create_dir_all(d);
        }
        let w = files.join(".work");
        let _ = fs::create_dir_all(w.join("tmp"));
        let _ = fs::create_dir_all(w.join("cache/uv"));
        env.insert("KITH_WORK".to_owned(), s(&w));
        env.insert(
            "CLAUDE_ENV_FILE".to_owned(),
            format!("{kernel_s}/lib/guest-env.sh"),
        );
        let uid: u32 = Sys { env: &env }
            .out("id", &["-u"], false)
            .and_then(|u| u.parse().ok())
            .unwrap_or(0);
        if let Some(d) = guest_tmp_ready(&p, &p_id, uid) {
            env.insert("CLAUDE_CODE_TMPDIR".to_owned(), d.clone());
            ctmp = d;
        } else {
            ctmp_note = prompt::ctmp_note(&p.machine, &p_id);
            say(&format!("kith: {ctmp_note}"));
        }
    }
    let _ = (&mem, &files);

    // ---- the pinned package
    let sys = Sys { env: &env };
    let mut pkg = src_pkg.clone();
    // `pkg` in lib/core-switched.txt: the core's own `resolve` (kith-pkg resolve is the rollback, KITH_CORE=python).
    let resolved = if switched(kernel, &env, "pkg") {
        let ctx = crate::pkg::Ctx {
            kernel: kernel.to_path_buf(),
            agents: p.agents.clone(),
            state: p.state.clone(),
            store: p.store.clone(),
            github: env
                .get("KITH_GITHUB_GIT")
                .map_or("https://github.com", String::as_str)
                .trim_end_matches('/')
                .to_owned(),
            // The must-ask python runs in this process's environment, not in the launch's env map that the old kith-pkg
            // subprocess had; the two differ only by what the launch edits (KITH_RUN_TOKEN, KITH_AGENT*).
            python: "python3".to_owned(),
        };
        let out = crate::pkg::cli::run(&ctx, &["resolve".to_owned(), agent.to_owned()]);
        eprint!("{}", out.stderr);
        (out.code == 0).then(|| out.stdout.trim_end_matches('\n').to_owned())
    } else {
        sys.out(kernel.join("bin/kith-pkg"), &["resolve", agent], true)
    };
    match resolved
        .map(PathBuf::from)
        .filter(|d| d.join("prompt.md").is_file())
    {
        Some(d) => pkg = d,
        None => say(&format!(
            "kith: {agent}: package snapshot failed; running the live folder"
        )),
    }
    let pkg_s = s(&pkg);
    let version = if pkg == src_pkg {
        "live".to_owned()
    } else {
        pkg_s.rsplit('@').next().unwrap_or_default().to_owned()
    };
    for (k, v) in [
        ("KITH_AGENT", agent.to_owned()),
        ("KITH_AGENT_DIR", pkg_s.clone()),
        ("KITH_AGENT_HOME", s(&home)),
        ("KITH_AGENT_VERSION", version.clone()),
    ] {
        env.insert(k.to_owned(), v);
    }

    // ---- the flags
    if let Some((code, msg)) = &a.error {
        say(msg);
        return Err(*code);
    }
    if a.resume {
        say("kith: --resume is for a run in this terminal, not --launch-json or --print");
        return Err(1);
    }
    let mut kith_model = a
        .model
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_MODEL").to_owned());
    let mut kith_effort = a
        .effort
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_EFFORT").to_owned());
    let kith_harness = a
        .harness
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_HARNESS").to_owned());
    let mut perm_mode = a
        .perm_mode
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_PERMISSION_MODE").to_owned());

    // ---- the agent's monthly limit
    let sys = Sys { env: &env };
    let limit_line = if !p_owner {
        None
    } else if switched(kernel, &env, "cost") {
        cost_core(kernel, &p, &env, &home_dir, "", &["limit", agent, "--sh"])
    } else {
        sys.out(
            kernel.join("bin/kith-cost"),
            &["limit", agent, "--sh"],
            false,
        )
    };
    if let Some(lim) = limit_line.filter(|l| !l.is_empty()) {
        let v = assignments(&lim);
        if v.get("l_over").map(String::as_str) == Some("1") {
            let at = v.get("l_at").map_or("ask", String::as_str);
            let text = v.get("l_text").cloned().unwrap_or_default();
            match at {
                "hold" => {
                    say(&format!(
                        "kith: {text}. Its runs are held until you raise the limit (kith {agent} set limit N) or the month turns; kith {agent} set at-limit ask lets them ask instead."
                    ));
                    return Err(3);
                }
                "ask" => say(&format!(
                    "kith: {text}; running anyway (your runs never stop at a limit; kith {agent} set limit N raises it)."
                )),
                _ => say(&format!("kith: {text}; running anyway.")),
            }
        }
    }

    // ---- the project
    let reg = prompt::Registry::load(&p.state).map_err(|e| {
        say(&e);
        1
    })?;
    let mut project = a.project.clone();
    let mut no_token = env.clone();
    no_token.remove("KITH_RUN_TOKEN");
    if project.is_empty() {
        // `project-id` in lib/core-switched.txt: the core answers. Never as a guest: the token is dropped above, and a guest's
        // own calls stay with bin/project-id, which asks the host.
        project = if switched(kernel, &env, "project-id") {
            let ctx = crate::project::Ctx {
                state: p.state.clone(),
                home: home_dir.clone(),
                cwd: std::env::current_dir().unwrap_or_default(),
                guest: false,
            };
            let out = crate::project::run(&ctx, &[]);
            if out.code == 0 {
                out.stdout.trim_end_matches('\n').to_owned()
            } else {
                String::new()
            }
        } else {
            Sys { env: &no_token }
                .out(kernel.join("bin/project-id"), &[], false)
                .unwrap_or_default()
        };
    } else if !reg.has(&project) {
        let known: String = reg.known().iter().map(|k| format!("{k} ")).collect();
        say(&format!(
            "kith: unknown project '{project}'. Known: {known}"
        ));
        say(&format!(
            "    register one with: {kernel_s}/bin/project-id --register {project} [dir]"
        ));
        return Err(1);
    }

    // ---- the prompt
    let cwd_logical = {
        let phys = std::env::current_dir().unwrap_or_default();
        match env.get("PWD").filter(|d| !d.is_empty()) {
            Some(d) if fs::canonicalize(d).ok() == fs::canonicalize(&phys).ok() => d.clone(),
            _ => s(&phys),
        }
    };
    let date = Sys { env: &env }
        .out("date", &["+%F"], false)
        .unwrap_or_default();
    let who = prompt::Who {
        owner: p_owner,
        id: p_id.clone(),
        line: p_line.clone(),
        rooms: p_rooms,
    };
    let envf = {
        let e = env.clone();
        move |k: &str| e.get(k).cloned()
    };
    let text = prompt::build(&prompt::Input {
        agent,
        paths: &p,
        kernel,
        src_pkg: &src_pkg,
        pkg: &pkg,
        who: &who,
        project: &project,
        registry: &reg,
        cwd: &cwd_logical,
        date: &date,
        ctmp_note: &ctmp_note,
        user_home: &home_dir,
        env: &envf,
    })
    .map_err(|e| {
        say(&e);
        1
    })?;

    // ---- run files: the snapshot is held by --hold's pid
    if pkg != src_pkg {
        let hold = a
            .hold
            .clone()
            .unwrap_or_else(|| std::os::unix::process::parent_id().to_string());
        if hold.is_empty() || !hold.chars().all(|c| c.is_ascii_digit()) {
            say("kith: --hold needs a process id");
            return Err(1);
        }
        let _ = fs::create_dir_all(pkg.join(".runs"));
        let _ = fs::write(pkg.join(".runs").join(format!("{hold}.launch")), "");
    }

    // ---- harness and login
    let agent_md = pkg.join("agent.md");
    let norm = |h: &str| {
        if h == "claude-code" {
            "claude".to_owned()
        } else {
            h.to_owned()
        }
    };
    let own = {
        let o = field(&agent_md, "harness");
        norm(if o.is_empty() { "claude" } else { &o })
    };
    let mut harness = norm(if kith_harness.is_empty() {
        &own
    } else {
        &kith_harness
    });
    if matches!(
        get(&env, "KITH_ACCOUNT_VAR"),
        "CLAUDE_CONFIG_DIR" | "CODEX_HOME"
    ) {
        let v = get(&env, "KITH_ACCOUNT_VAR").to_owned();
        env.remove(&v);
    }
    env.remove("KITH_ACCOUNT_VAR");
    env.remove("HQ_ACCOUNT_VAR");
    let mut account_id = harness.clone();
    let mut acct = String::new();
    let system = accounts::System {
        kernel: kernel.to_path_buf(),
    };
    let actx = accounts::Ctx {
        machine: p.machine.clone(),
        agents: p.agents.clone(),
        kernel: kernel.to_path_buf(),
        home: PathBuf::from(get(&env, "HOME")),
        claude_config_dir: env.get("CLAUDE_CONFIG_DIR").cloned(),
        out: &system,
    };
    // Every launch notes last_started for its account (not --print, which starts nothing).
    let record = !a.print;
    let mut be: HashMap<String, String> = HashMap::new();
    let use_account = |id: &str, be: &mut HashMap<String, String>| -> Result<(), i32> {
        match accounts::launch_env(&actx, agent, id, false, record) {
            Ok(t) => {
                *be = assignments(&t);
                Ok(())
            }
            Err(e) => {
                say(&format!("kith: {e}"));
                Err(4)
            }
        }
    };
    let bget = |be: &HashMap<String, String>, k: &str| be.get(k).cloned().unwrap_or_default();
    if !a.backup.is_empty() {
        use_account(&a.backup, &mut be)?;
        harness = bget(&be, "backup_harness");
        acct = bget(&be, "backup_account");
        account_id = a.backup.split('@').next().unwrap_or_default().to_owned();
    } else if harness == own {
        let opts = accounts::MainOpts {
            record,
            ..accounts::MainOpts::default()
        };
        match accounts::main_env(&actx, agent, &agent_md, opts) {
            Ok(t) => be = assignments(&t),
            Err(e) => {
                say(&format!("kith: {e}"));
                return Err(4);
            }
        }
        acct = bget(&be, "backup_account");
        account_id = bget(&be, "main_id");
        // The pick can be an account of another harness: it runs there, as --account would.
        let on = bget(&be, "backup_harness");
        if !on.is_empty() && on != harness {
            harness = on;
            kith_model.clear();
            env.remove("HQ_MODEL");
        }
    }
    let mut resume = get(&env, "KITH_RESUME").to_owned();
    if !resume.is_empty() {
        let made = accounts::session_account(&p.machine, &resume).unwrap_or_default();
        if !made.is_empty() && made != account_id {
            say(&format!(
                "kith: session {resume} was made under {made}, not {account_id}; starting a new one"
            ));
            resume.clear();
            env.remove("KITH_RESUME");
            env.remove("HQ_RESUME");
        }
    }
    if !a.backup.is_empty() && harness != own {
        kith_model.clear();
        env.remove("HQ_MODEL");
    }
    if !a.backup.is_empty() && !bget(&be, "backup_model").is_empty() {
        kith_model = bget(&be, "backup_model");
        env.remove("HQ_MODEL");
    }
    let cfg_var = bget(&be, "backup_cfg_var");
    if !cfg_var.is_empty() {
        env.insert(cfg_var.clone(), bget(&be, "backup_cfg_dir"));
        env.insert("KITH_ACCOUNT_VAR".to_owned(), cfg_var.clone());
    }
    env.insert("KITH_PROVIDER".to_owned(), harness.clone());
    env.insert("KITH_ACCOUNT".to_owned(), acct);
    env.insert("KITH_ACCOUNT_ID".to_owned(), account_id);
    if harness == own && kith_model.is_empty() {
        kith_model = field(&agent_md, "model");
    }
    if !p_model.is_empty() {
        kith_model.clone_from(&p_model);
    }
    if harness != "claude" {
        let m = kith_model.as_str();
        if ["opus", "sonnet", "haiku", "fable", "claude-"]
            .iter()
            .any(|x| m.starts_with(x))
            || m == "best"
            || m == "opusplan"
        {
            say(&format!(
                "kith: {m} is a Claude model; {agent} runs on {harness}'s default model (kith {agent} set harness {harness} --model M picks one)"
            ));
            kith_model.clear();
            env.remove("HQ_MODEL");
        }
    }
    if kith_effort.is_empty() {
        kith_effort = field(&agent_md, "effort");
    }
    env.insert("KITH_MODEL".to_owned(), kith_model.clone());
    env.insert("KITH_EFFORT".to_owned(), kith_effort.clone());
    env.insert("KITH_PROJECT".to_owned(), project.clone());
    for k in [
        "KITH_HARNESS",
        "KITH_OUTPUT",
        "KITH_RESUME",
        "KITH_PERMISSION_MODE",
        "HQ_HARNESS",
        "HQ_OUTPUT",
        "HQ_RESUME",
        "HQ_PERMISSION_MODE",
    ] {
        env.remove(k);
    }
    // a guest continues only a session the cost ledger has as theirs
    if !p_owner && !resume.is_empty() && !resume.contains(':') {
        let ok = (|| -> Option<bool> {
            let db = p.machine.join("cost.db");
            if !db.exists() {
                return None;
            }
            let c = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            let mut ids = vec![p_id.clone()];
            ids.extend(p_was.split(',').map(str::to_owned));
            ids.retain(|x| !x.is_empty());
            let sql = format!(
                "select 1 from runs where session = ? and agent = ? and person in ({})",
                vec!["?"; ids.len()].join(",")
            );
            let mut params: Vec<String> = vec![resume.clone(), agent.to_owned()];
            params.extend(ids);
            let mut st = c.prepare(&sql).ok()?;
            let found = st.exists(rusqlite::params_from_iter(params)).ok()?;
            Some(found)
        })();
        if ok != Some(true) {
            say(&format!(
                "kith: session {resume} is not one of {person}'s with {agent}; starting a new one"
            ));
        }
    }
    for k in [
        "KITH_TERMINAL_SESSION",
        "KITH_RESUMED_FROM",
        "KITH_LAUNCH_KIND",
        "KITH_LAUNCH_LABEL",
        "KITH_RUN_KIND",
        "KITH_RUN_LABEL",
        "KITH_LAUNCH_ASK_ID",
        "KITH_ASK_ID",
        "KITH_LAUNCH_SID_FILE",
        "KITH_CLAUDE_ARGS",
        "HQ_CLAUDE_ARGS",
    ] {
        env.remove(k);
    }
    if nested || !p_owner {
        for k in [
            "KITH_ASK_TOKEN",
            "KITH_ASK_SOCKET",
            "HQ_ASK_TOKEN",
            "HQ_ASK_SOCKET",
        ] {
            env.remove(k);
        }
    }

    // ---- the engine
    let backup_exit = if a.backup.is_empty() { 1 } else { 4 };
    let via = if get(&env, "KITH_VIA").is_empty() {
        "terminal".to_owned()
    } else {
        get(&env, "KITH_VIA").to_owned()
    };
    env.insert("KITH_VIA".to_owned(), via);
    let which = |x: &str| which_in(&env, x);
    let engine_spec = |h: &str| engines::find(h).map(|e| engines::launch_spec(e, &which));
    let mut spec: Option<Value> = None;
    if harness != "claude" && harness != "codex" {
        spec = engine_spec(&harness)
            .filter(|d| d.get("acp").is_some_and(truthy) || d.get("registry").is_some_and(truthy));
        if spec.is_none() {
            say(&format!(
                "kith: {agent} runs on {harness}; --launch-json covers claude and codex only for now"
            ));
            return Err(backup_exit);
        }
    }
    if !p_owner
        && harness != "claude"
        && engine_spec(&harness).and_then(|d| d["serves_guests"].as_bool()) != Some(true)
    {
        say(&format!(
            "kith: {agent} for {person}: runs for anyone but the owner need Claude Code (path rules), not {harness}"
        ));
        return Err(if a.backup.is_empty() { 3 } else { 4 });
    }

    // ---- the settings
    let claude = harness == "claude";
    let sys = Sys { env: &env };
    let project_dirs = settings::project_dirs(&reg, &project, &home_dir);
    let plugins_json = if claude {
        sys.out("claude", &["plugin", "list", "--json"], false)
            .unwrap_or_else(|| "[]".to_owned())
    } else {
        "[]".to_owned()
    };
    let developer_dir = if p_owner {
        String::new()
    } else {
        sys.out("xcode-select", &["-p"], false).unwrap_or_default()
    };
    let cver = if p_owner { None } else { claude_version(&sys) };
    // The hosts every guest of this agent reaches without a card, asked of kith-auth here (not of the host), so a launch with
    // no run socket has them too. No answer is none.
    let guest_domains: Result<Vec<String>, String> = if p_owner || !claude {
        Ok(vec![])
    } else {
        // A hung kith-auth must not hang the launch: on the timeout the guest gets no extra domains and the run goes
        // on with a warning (the same as a failure; kernel/bin/kith waits 20 s too).
        let answer = sys.out_within(
            kernel.join("bin/kith-auth"),
            &["agent-access", agent, "--json"],
            AGENT_ACCESS_TIMEOUT,
        );
        settings::guest_domains(answer.as_deref())
    };
    let kith_bin = Some(get(&env, "KITH_BIN"))
        .filter(|b| usable_kith_bin(Path::new(b)))
        .map(str::to_owned)
        .or_else(|| std::env::current_exe().ok().map(|e| s(&e)));
    let fence_bin = kith_bin.clone().filter(|b| knows_readfence(Path::new(b)));
    if perm_mode.is_empty() {
        perm_mode.clear();
    }
    let once_id = match random_hex() {
        Ok(id) => id,
        Err(m) => {
            say(&m);
            return Err(1);
        }
    };
    let out = {
        let envf = {
            let e = env.clone();
            move |k: &str| e.get(k).cloned()
        };
        settings::build(&settings::Input {
            agent,
            paths: &p,
            kernel,
            src_pkg: &src_pkg,
            pkg: &pkg,
            member: &member,
            claude,
            caller_mode: &perm_mode,
            project: &project,
            project_dirs: &project_dirs,
            cwd: &cwd_logical,
            user_home: &home_dir,
            kith_bin: kith_bin.as_deref(),
            fence_bin: fence_bin.as_deref(),
            plugins_json: &plugins_json,
            owner_terminal: false,
            api_helper: &bget(&be, "backup_api_helper"),
            once_id: &once_id,
            developer_dir: &developer_dir,
            ctmp: &ctmp,
            claude_version: cver.as_deref(),
            guest_domains: guest_domains.as_deref().map_err(String::as_str),
            env: &envf,
        })
    };
    let out = match out {
        Ok(o) => o,
        Err(r) => {
            say(&r.message);
            return Err(r.code);
        }
    };
    for w in &out.warnings {
        say(w);
    }
    let physical_cwd = if out.cwd == cwd_logical {
        std::env::current_dir()
            .map(|d| s(&d))
            .unwrap_or(out.cwd.clone())
    } else {
        let _ = std::env::set_current_dir(&out.cwd);
        std::env::current_dir()
            .map(|d| s(&d))
            .unwrap_or(out.cwd.clone())
    };
    let mut settings_val = out.settings.clone();
    if claude {
        env.insert("CLAUDE_CODE_ARTIFACT".to_owned(), "1".to_owned());
    }
    env.insert(
        "KITH_OWNER".to_owned(),
        if p_owner { "1" } else { "0" }.to_owned(),
    );
    if claude
        && !p_owner
        && !get(&env, "KITH_RUN_TOKEN").is_empty()
        && !get(&env, "KITH_RUN_SOCKET").is_empty()
        && let Some(sv) = &settings_val
    {
        // F11 2b: what the session was given beyond the box goes into the settings (kith-permit apply rewrites the file).
        let Ok(tag) = random_hex().inspect_err(|m| say(m)) else {
            return Err(1);
        };
        let tmp = std::env::temp_dir().join(format!("kith-launch-{tag}.settings.json"));
        if fs::write(&tmp, pj_indent1(sv)).is_ok() {
            let sys = Sys { env: &env };
            let _ = sys.run_loud(
                "python3",
                &[&s(&kernel.join("bin/kith-permit")), "apply", &s(&tmp)],
            );
            if let Some(v) = fs::read_to_string(&tmp)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(t.trim_end_matches('\n')).ok())
            {
                settings_val = Some(v);
            }
        }
        let _ = fs::remove_file(&tmp);
    }

    // ---- the MCP servers
    let sys = Sys { env: &env };
    let granted: Option<Value> =
        match sys.out(kernel.join("bin/kith-auth"), &["mcp-config", agent], true) {
            Some(t) => serde_json::from_str(&t).ok(),
            None => {
                say(&format!(
                    "kith: {agent}: no shared services this run (kith-auth failed)"
                ));
                None
            }
        };
    let merged = mcp::merged(p_owner, kernel, granted.as_ref(), &pkg).map_err(|e| {
        say(&e);
        1
    })?;
    let mcp_servers = merged["mcpServers"].clone();
    let mcp_arg = if p_owner {
        pyjson::dumps(&json!({"mcpServers": mcp_servers}))
    } else {
        format!(
            "{{\"mcpServers\":{{\"kithpermit\":{{\"command\":\"{kernel_s}/bin/kith-permit\",\"args\":[]}}}}}}"
        )
    };
    let call_model = if claude && p_model.is_empty() {
        call_model(&p.machine, agent)
    } else {
        String::new()
    };

    // ---- the object
    let claude_args: Value = if claude {
        let mut v: Vec<String> = vec!["--append-system-prompt".into(), text.clone()];
        for d in &out.add_dirs {
            v.extend(["--add-dir".into(), d.clone()]);
        }
        v.extend([
            "--settings".into(),
            settings_val.as_ref().map(pj_indent1).unwrap_or_default(),
        ]);
        v.push("--allowedTools".into());
        v.extend(out.allowed_tools.iter().cloned());
        v.extend(["--strict-mcp-config".into(), "--mcp-config".into(), mcp_arg]);
        if out.disable_slash_commands {
            v.push("--disable-slash-commands".into());
        }
        if !kith_model.is_empty() {
            v.extend(["--model".into(), kith_model.clone()]);
        }
        if !kith_effort.is_empty() {
            v.extend(["--effort".into(), kith_effort.clone()]);
        }
        if !out.mode_arg.is_empty() {
            v.extend(["--permission-mode".into(), out.mode_arg.clone()]);
        }
        json!(v)
    } else {
        Value::Null
    };
    sync_env(&mut env);
    let mut names: Vec<String> = p.entries().into_iter().map(|(k, _)| k).collect();
    names.extend(
        "KITH_ROOT KITH_AGENT KITH_AGENT_DIR KITH_AGENT_HOME KITH_AGENT_VERSION KITH_MODEL KITH_EFFORT KITH_PROJECT KITH_PERSON KITH_RUN_TOKEN KITH_RUN_SOCKET KITH_ROOM KITH_ROOM_MEMBERS KITH_ROOM_OWNER_ONLY KITH_VIA KITH_PROVIDER KITH_ACCOUNT KITH_ACCOUNT_ID KITH_ACCOUNT_VAR KITH_OWNER CLAUDE_CODE_ARTIFACT"
            .split(' ')
            .map(str::to_owned),
    );
    names.push(cfg_var);
    names.extend(p_env_names.split_whitespace().map(str::to_owned));
    if !p_owner {
        names.extend(["CLAUDE_CODE_TMPDIR", "KITH_WORK", "CLAUDE_ENV_FILE"].map(str::to_owned));
    }
    names.extend(
        "HQ_ROOT HQ_AGENT HQ_AGENT_DIR HQ_AGENT_HOME HQ_AGENT_VERSION HQ_MODEL HQ_EFFORT HQ_PROJECT HQ_PERSON HQ_VIA"
            .split(' ')
            .map(str::to_owned),
    );
    let mut env_obj = Map::new();
    for n in names {
        if let Some(v) = env.get(&n) {
            env_obj.entry(n).or_insert(Value::String(v.clone()));
        }
    }
    let nz = |k: &str| -> Value {
        let v = get(&env, k);
        if v.is_empty() {
            Value::Null
        } else {
            Value::String(v.to_owned())
        }
    };
    let nzs = |v: &str| -> Value {
        if v.is_empty() {
            Value::Null
        } else {
            Value::String(v.to_owned())
        }
    };
    let mut obj = Map::new();
    let mut put = |k: &str, v: Value| {
        obj.insert(k.to_owned(), v);
    };
    put("version", json!(1));
    put("agent", nz("KITH_AGENT"));
    put("agent_version", nz("KITH_AGENT_VERSION"));
    put("harness", json!(harness));
    put("cwd", json!(physical_cwd));
    put("env", Value::Object(env_obj));
    put("system_prompt", json!(text));
    put("settings", settings_val.unwrap_or(Value::Null));
    put("mcp_servers", mcp_servers);
    put("add_dirs", json!(out.add_dirs));
    put(
        "allowed_tools",
        json!(if claude {
            out.allowed_tools.clone()
        } else {
            vec![]
        }),
    );
    put("disable_slash_commands", json!(out.disable_slash_commands));
    put("model", nz("KITH_MODEL"));
    put("effort", nz("KITH_EFFORT"));
    put("project", nz("KITH_PROJECT"));
    put("call_model", nzs(&call_model));
    put(
        "permission_mode",
        out.permission_mode
            .as_deref()
            .map_or(Value::Null, |m| nzs(m)),
    );
    put(
        "cost_tag",
        json!({"agent": get(&env, "KITH_AGENT"), "version": get(&env, "KITH_AGENT_VERSION"), "model": nz("KITH_MODEL"),
               "effort": nz("KITH_EFFORT"), "via": get(&env, "KITH_VIA"), "project": nz("KITH_PROJECT"), "person": nz("KITH_PERSON")}),
    );
    put("claude_args", claude_args);
    put("backup", nzs(&a.backup));
    put("provider", nz("KITH_PROVIDER"));
    put("account", nz("KITH_ACCOUNT"));
    put("account_id", nz("KITH_ACCOUNT_ID"));
    if let Some(sp) = spec {
        for k in ["engine", "acp", "role_prompt", "registry"] {
            put(k, sp.get(k).cloned().unwrap_or(Value::Null));
        }
    }
    let _ = std::io::stdout().flush();
    Ok(pyjson::dumps(&Value::Object(obj)))
}

/// `kith_settings.call_model(agent)`: the model a voice-call turn runs on ("" keeps the agent's own).
fn call_model(machine: &Path, agent: &str) -> String {
    let st = machine::read(machine);
    let chosen = st
        .get("agents")
        .and_then(|a| a.get(agent))
        .and_then(|a| a.get("call_model"))
        .filter(|v| truthy(v))
        .or_else(|| st.get("call_model"))
        .cloned()
        .unwrap_or_else(|| Value::String("sonnet".to_owned()));
    // a stored value that isn't a string reads as none (tests below)
    let chosen = chosen.as_str().unwrap_or_default().to_owned();
    if chosen == "own" || chosen == "default" {
        String::new()
    } else {
        chosen.trim().to_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn a_limit_is_read_only_when_it_is_a_plain_amount() {
        for ok in ["0", "10", "12.50", "0.01"] {
            assert!(plain_number(ok), "{ok}");
        }
        for bad in [
            "", "abc", "NaN", "nan", "inf", "-inf", "Infinity", "-5", "+5", "1e3", "1.", ".5",
            "1.2.3", " 5",
        ] {
            assert!(!plain_number(bad), "{bad:?}");
        }
        // finite budgets compare as before: under runs, over stops
        let f = |x: &str| x.trim().parse::<f64>().ok();
        assert!(f("1.0").unwrap() < f("5").unwrap());
        assert!(f("5").unwrap() >= f("5").unwrap());
    }

    #[test]
    fn random_hex_is_32_digits_and_never_the_same_twice() {
        let (a, b) = (random_hex().unwrap(), random_hex().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_ne!(
            a,
            "0".repeat(32),
            "zeros are what a failed read used to give"
        );
    }

    #[test]
    fn flags_are_read_as_the_entry_reads_them() {
        let a = parse_args(&v(&[
            "--launch-json",
            "--hold",
            "7",
            "-H",
            "codex",
            "-m",
            "m",
            "-e",
            "high",
            "--backup",
            "claude:w",
            "-p",
            "pr",
            "--bypass",
            "task",
        ]));
        assert!(a.launch && a.error.is_none() && !a.resume);
        assert_eq!(a.hold.as_deref(), Some("7"));
        assert_eq!(a.harness.as_deref(), Some("codex"));
        assert_eq!(a.perm_mode.as_deref(), Some("bypassPermissions"));
        assert_eq!((a.backup.as_str(), a.project.as_str()), ("claude:w", "pr"));
        assert!(parse_args(&v(&["--launch-json", "-r"])).resume);
        assert!(
            parse_args(&v(&["--launch-json", "--nosuch"]))
                .error
                .is_some()
        );
        // a task that starts with a dash stops the flags
        assert!(parse_args(&v(&["- one thing", "--nosuch"])).error.is_none());
    }

    #[test]
    fn closed_or_unknown_home_is_a_guest() {
        assert!(home_closed(None));
        assert!(home_closed(Some(PathBuf::from("/nonexistent/kith-home"))));
        assert!(!home_closed(Some(std::env::temp_dir())));
    }

    #[test]
    fn kith_bin_must_answer_memory_hook() {
        let dir = std::env::temp_dir().join(format!("kith-bin-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str, mode: u32| {
            let p = dir.join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
            p
        };
        let good = script("good", "#!/bin/sh\necho 'usage: hook --Phase x'\n", 0o755);
        let wrong = script("wrong", "#!/bin/sh\necho nothing\n", 0o755);
        let failing = script("failing", "#!/bin/sh\necho phase\nexit 1\n", 0o755);
        let plain = script("plain", "#!/bin/sh\necho phase\n", 0o644);
        assert!(usable_kith_bin(&good));
        assert!(!usable_kith_bin(&wrong));
        assert!(!usable_kith_bin(&failing));
        assert!(!usable_kith_bin(&plain));
        assert!(!usable_kith_bin(&dir.join("missing")));
        assert!(!usable_kith_bin(Path::new("")));
        // The running program is taken without asking it anything (the test binary has no `memory hook`).
        assert!(usable_kith_bin(&std::env::current_exe().unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn readfence_verb_is_asked_for() {
        let dir = std::env::temp_dir().join(format!("kith-fence-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let new = script(
            "new",
            "#!/bin/sh\necho 'usage: kith kernel readfence --root DIR'\n",
        );
        let old = script("old", "#!/bin/sh\necho 'no agent named kernel'\nexit 1\n");
        // A script just written can still be "text file busy" while another test forks: ask again.
        let mut knows = false;
        for _ in 0..20 {
            knows = knows_readfence(&new);
            if knows {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        assert!(knows);
        assert!(!knows_readfence(&old));
        assert!(!knows_readfence(&dir.join("missing")));
        assert!(knows_readfence(&std::env::current_exe().unwrap()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_names_agree_both_ways() {
        let mut e: Env = [
            ("HQ_MODEL", "m"),
            ("KITH_BIN", "b"),
            ("HQ_BIN", "k"),
            ("KITH_ROOM", ""),
        ]
        .map(|(k, x)| (k.to_owned(), x.to_owned()))
        .into();
        sync_env(&mut e);
        assert_eq!(e["KITH_MODEL"], "m");
        assert_eq!(e["KITH_KERNEL_BIN"], "k");
        assert_eq!(e["HQ_MODEL"], "m");
        assert!(!e.contains_key("HQ_ROOM"));
    }

    #[test]
    fn shell_assignments_unquote() {
        let m = assignments("a='x y' b='' c=1.50 d='it'\"'\"'s'");
        assert_eq!(m["a"], "x y");
        assert_eq!(m["b"], "");
        assert_eq!(m["c"], "1.50");
        assert_eq!(m["d"], "it's");
    }
}
