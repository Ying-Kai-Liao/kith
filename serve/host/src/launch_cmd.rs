//! `kith kernel launch settings|mcp`: the run settings and the MCP servers of a launch, from the `kith-kernel` crate.
//! This is the caller of the pure modules: it asks the programs that need asking (`kith-auth`, `claude`, `xcode-select`)
//! and passes what they said in. Not routed from `kith AGENT` yet.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, anyhow};
use kith_kernel::launch::{mcp, prompt, settings};
use serde_json::{Value, json};

use crate::kernel;

/// What `kith kernel launch settings|mcp` was asked.
#[derive(Default)]
pub struct Args {
    pub agent: String,
    pub person: Option<String>,
    pub mode: Option<String>,
    pub pkg: Option<PathBuf>,
    /// Pin the package first, as the launch does (`kith-pkg resolve`), and use the snapshot.
    pub snapshot: bool,
    pub kernel: Option<PathBuf>,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub harness: Option<String>,
    pub kith_bin: Option<String>,
    pub plugins_json: Option<PathBuf>,
    pub terminal: bool,
    pub api_helper: Option<String>,
    pub once_id: Option<String>,
    /// The output of `kith-auth resolve` / `mcp-config` (a file), instead of running the command.
    pub resolve: Option<PathBuf>,
    pub granted: Option<PathBuf>,
}

fn run(prog: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(prog).args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// What both verbs need: the folders, the agent's package and who the run is for.
struct Ctx {
    p: kith_kernel::paths::Paths,
    kernel: PathBuf,
    src_pkg: PathBuf,
    pkg: PathBuf,
    home: PathBuf,
    member: settings::Member,
    person: String,
}

fn context(a: &Args) -> Result<Ctx> {
    let home = kith_kernel::paths::process_home();
    let (p, kernel_dir) = match &a.kernel {
        Some(k) => (kith_kernel::paths::paths(&home, k, &kith_kernel::paths::process_env), k.clone()),
        None => (
            kernel::folders(&home),
            kernel::current_kernel().ok_or_else(|| anyhow!("kith: can't find the kernel folder (--kernel)"))?,
        ),
    };
    let src_pkg =
        prompt::agent_dir(&p, &kernel_dir, &a.agent).ok_or_else(|| anyhow!("kith: no agent named \"{}\".", a.agent))?;
    let mut pkg = a.pkg.clone().unwrap_or_else(|| src_pkg.clone());
    if a.snapshot {
        // If versioning itself breaks, the launch runs the live folder rather than not at all.
        let pinned = Command::new(kernel_dir.join("bin/kith-pkg")).args(["resolve", &a.agent]).output().ok();
        match pinned.filter(|o| o.status.success()).map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())) {
            Some(d) if d.join("prompt.md").is_file() => pkg = d,
            _ => eprintln!("kith: {}: package snapshot failed; running the live folder", a.agent),
        }
    }
    let person = a.person.clone().or_else(|| kith_kernel::paths::process_env("KITH_PERSON")).unwrap_or_default();
    let member = if person.is_empty() {
        settings::Member::owner()
    } else {
        // Who the run is for is `kith-auth resolve`'s to say (it moves into the core with the secrets).
        let text = if let Some(f) = &a.resolve {
            std::fs::read_to_string(f)?
        } else {
            {
                let o = Command::new(kernel_dir.join("bin/kith-auth"))
                    .args(["resolve", &a.agent])
                    .env("KITH_PERSON", &person)
                    .output()?;
                if !o.status.success() {
                    return Err(anyhow!(
                        "kith: can't check whether {person} may use {} (kith-auth resolve failed)",
                        a.agent
                    ));
                }
                String::from_utf8_lossy(&o.stdout).into_owned()
            }
        };
        let r: Value = serde_json::from_str(&text)?;
        let allowed =
            r.get("allowed").is_some_and(|v| v.as_i64().map_or_else(|| v.as_bool().unwrap_or(false), |n| n != 0));
        if !allowed {
            let why = r.get("why").and_then(Value::as_str).unwrap_or_default();
            return Err(anyhow!("kith: {why} (kith-auth member {} {person})", a.agent));
        }
        settings::Member::from_resolve(&r)
    };
    Ok(Ctx { p, kernel: kernel_dir, src_pkg, pkg, home, member, person })
}

/// The `harness:` of agent.md (`claude-code` is `claude`).
fn harness_of(a: &Args, pkg: &Path) -> String {
    if let Some(h) = a.harness.clone().filter(|h| !h.is_empty()) {
        return if h == "claude-code" { "claude".to_owned() } else { h };
    }
    let text = std::fs::read_to_string(pkg.join("agent.md")).unwrap_or_default();
    let mut lines = text.lines();
    let mut own = String::new();
    if lines.next() == Some("---") {
        for l in lines {
            if l == "---" {
                break;
            }
            if let Some(v) = l.strip_prefix("harness:") {
                v.trim().clone_into(&mut own);
                break;
            }
        }
    }
    match own.as_str() {
        "" | "claude-code" => "claude".to_owned(),
        _ => own,
    }
}

/// `claude --version`'s `X.Y.Z`.
fn claude_version() -> Option<String> {
    let out = run("claude", &["--version"])?;
    let first = out.lines().next()?;
    let v: String = first.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let parts: Vec<&str> = v.split('.').collect();
    (parts.len() >= 3 && parts.iter().take(3).all(|x| !x.is_empty())).then(|| parts[..3].join("."))
}

/// `kith kernel launch settings`.
pub fn print_settings(a: &Args) -> Result<()> {
    let c = context(a)?;
    let harness = harness_of(a, &c.pkg);
    if harness != "claude" && harness != "codex" {
        return Err(anyhow!("kith: {} runs on {harness}; --launch-json covers claude and codex only for now", a.agent));
    }
    let claude = harness == "claude";
    let owner = c.member.owner;
    if !owner && !claude {
        eprintln!(
            "kith: {} for {}: runs for anyone but the owner need Claude Code (path rules), not {harness}",
            a.agent, c.person
        );
        std::process::exit(3);
    }
    let reg = prompt::Registry::load(&c.p.state).map_err(|e| anyhow!(e))?;
    let project = a.project.clone().unwrap_or_default();
    if !project.is_empty() && !reg.has(&project) {
        return Err(anyhow!("kith: unknown project '{project}'. Known: {}", reg.known().join(" ")));
    }
    let folders = settings::Folders::new(&c.p, &c.src_pkg, &a.agent, &c.member);
    settings::prepare_folders(&c.p, &folders, &c.member)?;
    let project_dirs = settings::project_dirs(&reg, &project, &c.home);
    let cwd = a
        .cwd
        .clone()
        .or_else(|| std::env::var("PWD").ok().filter(|d| !d.is_empty()))
        .or_else(|| std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let kith_bin = a
        .kith_bin
        .clone()
        .or_else(|| kith_kernel::paths::process_env("KITH_BIN").filter(|b| !b.is_empty()))
        .or_else(|| std::env::current_exe().ok().map(|e| e.to_string_lossy().into_owned()));
    let plugins_json = match &a.plugins_json {
        Some(f) => std::fs::read_to_string(f)?,
        None if claude => run("claude", &["plugin", "list", "--json"]).unwrap_or_else(|| "[]".to_owned()),
        None => "[]".to_owned(),
    };
    let developer_dir = if owner { String::new() } else { run("xcode-select", &["-p"]).unwrap_or_default() };
    let claude_version = if owner { None } else { claude_version() };
    let ctmp = if owner {
        String::new()
    } else {
        use sha2::{Digest as _, Sha256};
        let hash = |s: &str| {
            use std::fmt::Write as _;
            Sha256::digest(s.as_bytes()).iter().fold(String::new(), |mut a, b| {
                let _ = write!(a, "{b:02x}");
                a
            })
        };
        let uid = run("id", &["-u"]).and_then(|u| u.parse().ok()).unwrap_or(0);
        prompt::guest_tmp(&kith_kernel::paths::realpath(&c.p.machine), &c.member.id, uid, &hash)
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let once_id = a.once_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let mode = a.mode.clone().or_else(|| kith_kernel::paths::process_env("KITH_PERMISSION_MODE")).unwrap_or_default();
    let input = settings::Input {
        agent: &a.agent,
        paths: &c.p,
        kernel: &c.kernel,
        src_pkg: &c.src_pkg,
        pkg: &c.pkg,
        member: &c.member,
        claude,
        caller_mode: &mode,
        project: &project,
        project_dirs: &project_dirs,
        cwd: &cwd,
        user_home: &c.home,
        kith_bin: kith_bin.as_deref(),
        plugins_json: &plugins_json,
        owner_terminal: a.terminal,
        api_helper: a.api_helper.as_deref().unwrap_or_default(),
        once_id: &once_id,
        developer_dir: &developer_dir,
        ctmp: &ctmp,
        claude_version: claude_version.as_deref(),
        env: &kith_kernel::paths::process_env,
    };
    match settings::build(&input) {
        Ok(o) => {
            for w in &o.warnings {
                eprintln!("{w}");
            }
            println!(
                "{}",
                json!({
                    "settings": o.settings, "permission_mode": o.permission_mode, "allowed_tools": o.allowed_tools,
                    "add_dirs": o.add_dirs, "disable_slash_commands": o.disable_slash_commands, "cwd": o.cwd,
                })
            );
            Ok(())
        }
        Err(r) => {
            eprintln!("{}", r.message);
            std::process::exit(r.code);
        }
    }
}

/// `kith kernel launch mcp`.
pub fn print_mcp(a: &Args) -> Result<()> {
    let c = context(a)?;
    let granted: Option<Value> = if c.member.owner {
        if let Some(f) = &a.granted {
            Some(serde_json::from_str(&std::fs::read_to_string(f)?)?)
        } else {
            {
                // The shared services this agent is granted (secrets resolve in kith-auth, not here).
                let o = Command::new(c.kernel.join("bin/kith-auth")).args(["mcp-config", &a.agent]).output()?;
                if o.status.success() {
                    serde_json::from_slice(&o.stdout).ok()
                } else {
                    eprintln!("kith: {}: no shared services this run (kith-auth failed)", a.agent);
                    None
                }
            }
        }
    } else {
        None
    };
    let merged = mcp::merged(c.member.owner, &c.kernel, granted.as_ref(), &c.pkg).map_err(|e| anyhow!(e))?;
    println!("{}", json!({ "mcp_servers": merged["mcpServers"] }));
    Ok(())
}
