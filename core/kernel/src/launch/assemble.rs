//! The whole launch: `kith AGENT --launch-json` (the launch branch of `kernel/bin/kith`) in Rust. It runs the steps the bash
//! entry runs around the pieces in this folder, in the same order, with the same words on stderr and the same exit codes:
//! who the run is for (`secrets`), the membership's budget, the agent's monthly limit, the pinned snapshot, the project,
//! the prompt, the login (`accounts`), the settings, the MCP servers, then one JSON object on stdout.
//!
//! Programs it still asks, because their logic stays in Python until their step of the plan: `kith-pkg resolve` (the
//! snapshot), `kith-cost limit|spent`, `kith-auth mcp-config`
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
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use super::{machine, mcp, prompt};
use crate::secrets::pyjson;
use crate::{paths, secrets};

pub type Env = HashMap<String, String>;

/// How long a launch waits for `kith-auth agent-access` (kernel/bin/kith waits the same).
pub(super) const AGENT_ACCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What `kith AGENT [flags]` was given.
#[derive(Default, Debug)]
#[allow(clippy::struct_excessive_bools)]
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
    /// Words are left after the flags (a task: the run is a one-shot, not a terminal).
    pub task: bool,
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
    // `--` ends the flags and is not itself a task.
    a.task = args.len() > i + usize::from(args.get(i).is_some_and(|x| x == "--"));
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

pub(super) fn get<'a>(env: &'a Env, k: &str) -> &'a str {
    env.get(k).map_or("", String::as_str)
}

pub(super) fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub(super) fn truthy(v: &Value) -> bool {
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
pub(super) fn pystr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `repr` of a string.
pub(super) fn pyrepr(x: &str) -> String {
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
/// and `pkg` has none, so the rollback is always allowed for this verb. Add the refusal when it gets a cutover.
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

/// `kith-cost ARGS` answered by the core in process: its stdout without the last newline when it
/// exits 0, else `None`, which the callers read as `kith-cost` failing. Only the verbs the launch asks (`spent`, `limit --sh`).
pub(super) fn cost_core(
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
        ..crate::cost::Env::default()
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let out = crate::cost::run(&paths, &cenv, &args);
    (out.code == 0).then(|| out.stdout.trim_end_matches('\n').to_owned())
}

// ---- programs

pub(super) struct Sys<'a> {
    pub(super) env: &'a Env,
}

impl Sys<'_> {
    fn cmd(&self, prog: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Command {
        let mut c = Command::new(prog);
        c.args(args).env_clear().envs(self.env).stdin(Stdio::null());
        c
    }

    /// stdout of a program that succeeded; `stderr` shows on ours when `loud`. Trailing newlines dropped as `$(...)` does.
    pub(super) fn out(
        &self,
        prog: impl AsRef<std::ffi::OsStr>,
        args: &[&str],
        loud: bool,
    ) -> Option<String> {
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
    pub(super) fn out_within(
        &self,
        prog: impl AsRef<std::ffi::OsStr>,
        args: &[&str],
        limit: std::time::Duration,
    ) -> Option<String> {
        secrets::run_command_with_timeout(self.cmd(prog, args), limit)
            .map(|o| o.trim_end_matches('\n').to_owned())
    }

    /// A program whose stdout, in the launch, goes to stderr.
    pub(super) fn run_loud(&self, prog: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> bool {
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
pub(super) fn assignments(text: &str) -> HashMap<String, String> {
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

pub(super) fn which_in(env: &Env, program: &str) -> Option<PathBuf> {
    std::env::split_paths(get(env, "PATH"))
        .map(|d| d.join(program))
        .find(|p| is_exec(p))
}

/// Whether this process is inside the guest sandbox: the account's home folder (by the OS, not `$HOME`, and by absolute
/// programs, not the caller's `PATH`) can't be listed. No answer is a guest, as in `kith_paths.guest_sandbox`.
pub(super) fn in_guest_sandbox() -> bool {
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

/// Whether `bin`'s `kernel version` lists `module`: the running program, or one that says so.
pub(crate) fn lists_module(bin: &Path, module: &str) -> bool {
    let real = fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    if std::env::current_exe()
        .and_then(fs::canonicalize)
        .is_ok_and(|me| me == real)
    {
        return true;
    }
    secrets::run_with_timeout(
        bin,
        &["kernel", "version"],
        std::time::Duration::from_secs(10),
    )
    .is_some_and(|o| o.contains(&format!("\"{module}\"")))
}

/// Whether `bin` has the `kernel readfence` verb: the running program, or one that answers `kernel readfence --help`. An older
/// kith would read `kernel` as an agent name, so the launch uses the running program for the fence instead.
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

/// Whether `bin` lists `module` in `kernel version`: the running program, or one whose `kernel version` names it. An older kith
/// has `kernel cost` (the read verbs) but not the `cost-ledger` module, which `tag` and `record` are part of.
pub(crate) fn knows_module(bin: &Path, module: &str) -> bool {
    let real = fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf());
    if std::env::current_exe()
        .and_then(fs::canonicalize)
        .is_ok_and(|me| me == real)
    {
        return crate::MODULES.contains(&module);
    }
    secrets::run_with_timeout(
        bin,
        &["kernel", "version"],
        std::time::Duration::from_secs(10),
    )
    .is_some_and(|o| o.contains(&format!("\"{module}\"")))
}

/// The program the `SessionStart` hook runs for the cost ledger (`kith kernel cost tag`): `kith_bin` when it has the `cost-ledger`
/// module, else this program (it always does). `kith_cost_bin` in lib/kith-core.sh decides the same way for the bash launch.
pub fn cost_hook_bin(kith_bin: Option<&str>) -> Option<String> {
    kith_bin
        .filter(|b| !b.is_empty() && knows_module(Path::new(b), "cost-ledger"))
        .map(str::to_owned)
        .or_else(|| std::env::current_exe().ok().map(|e| s(&e)))
}

/// The `--mcp-config` text of anyone but the owner: the bytes bin/kith's `permit_mcp` prints (command before args, no spaces).
pub(super) fn guest_mcp_arg(permit_bin: Option<&str>, kernel: &Path) -> String {
    let (command, args) = mcp::permit_command(permit_bin, kernel);
    let q = |x: &str| serde_json::to_string(x).unwrap_or_default();
    format!(
        "{{\"mcpServers\":{{\"kithpermit\":{{\"command\":{},\"args\":[{}]}}}}}}",
        q(&command),
        args.iter().map(|a| q(a)).collect::<Vec<_>>().join(",")
    )
}

/// The program the permit hooks, the `kithpermit` MCP server and `apply` run (`kith kernel permit`): `kith_bin` when `permit` is
/// switched (a line in `lib/core-switched.txt` and no `KITH_CORE=python`) and the program lists the module. `None` keeps the
/// script (`bin/kith-permit`, the whole Python and the rollback). `kith_core_bin permit` in lib/kith-core.sh decides the same way
/// for the bash launch. `bin/kith-permit` stays for the runs that hold the old path and for `kith bug`, which loads it.
pub fn permit_hook_bin(kernel: &Path, env: &Env, kith_bin: Option<&str>) -> Option<String> {
    switched_bin(kernel, env, kith_bin, "permit", "permit")
}

/// The program the `SessionStart` hook runs for the session registry (`kith kernel threads record`): `kith_bin` when `threads`
/// is switched and the program has the `threads-writer` module (every `kith-sessions` verb in both modes of the claims, not the
/// Files-mode `threads` an older program lists). `None` keeps `bin/kith-sessions record`. `kith_core_bin threads` in
/// lib/kith-core.sh decides the same way for the bash launch.
pub fn threads_hook_bin(kernel: &Path, env: &Env, kith_bin: Option<&str>) -> Option<String> {
    switched_bin(kernel, env, kith_bin, "threads", "threads-writer")
}

fn switched_bin(
    kernel: &Path,
    env: &Env,
    kith_bin: Option<&str>,
    verb: &str,
    module: &str,
) -> Option<String> {
    kith_bin
        .filter(|b| {
            !b.is_empty() && switched(kernel, env, verb) && knows_module(Path::new(b), module)
        })
        .map(str::to_owned)
}

/// The program the Pre/PostToolUse room-ledger hooks run (`kith kernel room-ledger`): `kith_bin` when `room-ledger` is
/// switched (a line in `lib/core-switched.txt` and no `KITH_CORE=python`) and the program lists the module. `None` keeps the
/// hooks on `bin/kith-room-ledger`, a stub that forwards, for runs that hold the old path. `kith_core_bin room-ledger` in
/// lib/kith-core.sh decides the same way for the bash launch.
pub fn ledger_hook_bin(kernel: &Path, env: &Env, kith_bin: Option<&str>) -> Option<String> {
    kith_bin
        .filter(|b| {
            !b.is_empty()
                && switched(kernel, env, "room-ledger")
                && knows_module(Path::new(b), "room-ledger")
        })
        .map(str::to_owned)
}

/// `kith_paths.guest_tmp_ready`: the guest's Claude Code temp folder, made.
pub(super) fn guest_tmp_ready(p: &paths::Paths, id: &str, uid: u32) -> Option<String> {
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
pub(super) fn field(agent_md: &Path, key: &str) -> String {
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

pub(super) fn claude_version(sys: &Sys<'_>) -> Option<String> {
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
pub(super) fn plain_number(t: &str) -> bool {
    !t.is_empty()
        && t.split('.').count() <= 2
        && t.split('.')
            .all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()))
}

/// `uuid4().hex`. An unreadable random source is an error: an all-zero id would be shared by every run, and a run's
/// id and temp file names must not collide.
pub(super) fn random_hex() -> Result<String, String> {
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

pub(super) fn pj_indent1(v: &Value) -> String {
    pyjson::dumps_indent_n(v, 1)
}

// ---- the run

/// `kith AGENT [args]` with `--launch-json`: the JSON text on success, or the exit code (what was said is already on stderr).
pub fn launch(agent: &str, argv: &[String], kernel: &Path, env0: Env) -> Result<String, i32> {
    let plan = super::plan::plan(agent, argv, kernel, env0, &super::plan::Mode::LaunchJson)?;
    Ok(super::json::emit_json(&plan))
}

/// `kith_settings.call_model(agent)`: the model a voice-call turn runs on ("" keeps the agent's own).
pub(super) fn call_model(machine: &Path, agent: &str) -> String {
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
    fn a_guests_mcp_arg_names_the_program_or_the_script() {
        let k = Path::new("/k");
        assert_eq!(
            guest_mcp_arg(Some("/b/kith"), k),
            r#"{"mcpServers":{"kithpermit":{"command":"/b/kith","args":["kernel","permit","--kernel","/k"]}}}"#
        );
        assert_eq!(
            guest_mcp_arg(None, k),
            r#"{"mcpServers":{"kithpermit":{"command":"/k/bin/kith-permit","args":[]}}}"#
        );
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
        assert!(a.launch && a.error.is_none() && !a.resume && a.task);
        assert!(!parse_args(&v(&["-r"])).task);
        assert!(!parse_args(&v(&["--", ])).task);
        assert!(parse_args(&v(&["--", "do it"])).task);
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
