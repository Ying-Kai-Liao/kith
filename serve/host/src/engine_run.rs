//! `kith kernel engine-run ENGINE [--output json] [--resume S] [--permission-mode M] [-- task]`: a non-Claude agent's
//! one-shot over ACP, the verb `kernel/bin/kith` runs for an engine that is not Claude Code. It does what
//! `adapters/harness.py`'s ACP path did (the prompt, directories and MCP servers `bin/kith` hands over in
//! `KITH_RUN_PROMPT`, `KITH_RUN_DIRS` and `KITH_RUN_MCP`; the result in the shape of `claude -p --output-format json`;
//! one cost ledger row) on the run that `run-once` does. Exit 78 before anything starts: this engine has no ACP one-shot
//! here, and that is the final answer. `--interactive` is the terminal session of Codex and Hermes: their own CLI, run
//! with the run's flags. Design: 2026-11-21-harness-oneshots, sections 3.1 to 3.3.

use crate::runonce;
use anyhow::{Context as _, Result, anyhow, bail};
use kith_acp::{Outcome, RunSpec, Usage};
use kith_kernel::engines;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// The whole run, as harness.py's ACP path had it.
const TIMEOUT: Duration = Duration::from_secs(86400);

pub struct Args {
    pub engine: String,
    pub json: bool,
    pub resume: Option<String>,
    pub permission_mode: Option<String>,
    /// The engine's terminal session instead of a one-shot (no task).
    pub interactive: bool,
    pub task: Vec<String>,
}

/// A spent plan: when it renews, if the engine said.
struct UsageLimit {
    resets_at: Option<i64>,
}

/// What the run came to, before it is printed and recorded.
struct Done {
    /// `<engine>:<id>`, once the engine opened a session.
    session: Option<String>,
    text: String,
    error: Option<String>,
    /// Only the keys with a count (and `input`, `output` always), as harness.py kept them.
    usage: Map<String, Value>,
    cost: Option<f64>,
    /// Set when the plan is spent: `resets_at` is when it renews, if the engine said.
    usage_limit: Option<UsageLimit>,
}

impl Done {
    fn of(engine: &str, result: Result<Outcome>, spent: Option<f64>) -> Self {
        // What the engine reported it cost, including what a run that failed spent first (a delegation's cap counts it).
        // `or None`: a reported zero is no cost.
        let cost = spent.filter(|c| *c != 0.0);
        match result {
            Ok(out) => {
                let usage = out
                    .usage
                    .map(Usage::to_json)
                    .and_then(|u| match u {
                        Value::Object(m) => Some(m),
                        _ => None,
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(k, v)| v.as_u64().is_some_and(|n| n != 0) || k == "input" || k == "output")
                    .collect();
                Done {
                    session: Some(format!("{engine}:{}", out.session_id)),
                    text: out.text,
                    error: None,
                    usage,
                    cost,
                    usage_limit: None,
                }
            }
            Err(e) => {
                let (kind, resets_at) = runonce::classify(&e);
                Done {
                    session: None,
                    text: String::new(),
                    error: Some(format!("{e:#}")),
                    usage: Map::new(),
                    cost,
                    usage_limit: (kind == "depleted").then_some(UsageLimit { resets_at }),
                }
            }
        }
    }

    /// `finish`'s `--output json` object.
    fn json(&self, engine: &str) -> Value {
        let mut v = json!({
            "type": "result",
            "subtype": if self.error.is_some() { "error" } else { "success" },
            "harness": engine,
            "is_error": self.error.is_some(),
            "result": if self.text.is_empty() { self.error.clone().unwrap_or_default() } else { self.text.clone() },
            "session_id": self.session,
            "num_turns": 1,
            "total_cost_usd": self.cost,
            "usage": self.usage,
        });
        if let Some(UsageLimit { resets_at }) = self.usage_limit {
            v["usage_limit"] = json!({"resets_at": resets_at});
        }
        v
    }

    /// The cost ledger row of `record`: one per run that opened a session.
    fn row(&self, engine: &str, model: &str, task: &str, at: chrono::DateTime<chrono::Utc>) -> Option<Value> {
        let session = self.session.as_ref()?;
        let mut row = json!({
            "id": format!("{session}@{}", at.format("%Y%m%dT%H%M%S.%6f")),
            "session": session,
            "model": if model == engine { engine.to_owned() } else { format!("{engine}/{model}") },
            "turn": task.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect::<String>(),
            "cost": self.cost,
        });
        row.as_object_mut()?.extend(self.usage.clone());
        Some(row)
    }
}

/// For harnesses with no system-prompt option: the agent's instructions travel with the first task. Byte for byte what
/// harness.py's `instructions_first` makes.
fn instructions_first(prompt: &str, task: &str) -> String {
    format!(
        "<hq-agent-instructions>\n{prompt}\n</hq-agent-instructions>\n\nThe instructions above are who you are and how \
         you work for this whole session. The task:\n\n{task}"
    )
}

/// [`resume_id`], or the refusal when the run may only continue the session it was given (`KITH_RESUME_ONLY`: a claimed turn on it).
fn resume_checked(engine: &str, resume: Option<&str>, only: bool) -> Result<Option<String>, String> {
    let named = resume.filter(|r| !r.is_empty());
    if only && let Some(r) = named {
        let (h, id) = r.split_once(':').unwrap_or((r, ""));
        if h != engine || id.is_empty() {
            return Err(format!("session {r} is not a {engine} session; the turn on it does not run on a new one"));
        }
    }
    Ok(resume_id(engine, resume))
}

/// The session to continue: the id when `resume` is this engine's (`<engine>:<id>`), else a note and a new session.
fn resume_id(engine: &str, resume: Option<&str>) -> Option<String> {
    let resume = resume.filter(|r| !r.is_empty())?;
    let (h, id) = resume.split_once(':').unwrap_or((resume, ""));
    if h == engine && !id.is_empty() {
        return Some(id.to_owned());
    }
    eprintln!("kith: session {resume} is not a {engine} session; starting a new one");
    None
}

/// The granted servers and the agent's own (bin/kith passes both files), merged; a later file wins.
fn merge_mcp(files: &str) -> Map<String, Value> {
    let mut servers = Map::new();
    for f in files.lines().filter(|f| !f.is_empty()) {
        match std::fs::read_to_string(f)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        {
            Ok(v) => servers.extend(v["mcpServers"].as_object().cloned().unwrap_or_default()),
            Err(e) => eprintln!("kith: MCP config {f} unreadable: {e}"),
        }
    }
    servers
}

/// A real path, once each (codex refuses to start a command when a writable root has a symlink in it).
fn real_dirs(lines: &str) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    for d in lines.lines().filter(|d| !d.is_empty()) {
        let real = std::fs::canonicalize(d).map_or_else(|_| d.to_owned(), |p| plain(&p.to_string_lossy()));
        if !dirs.contains(&real) {
            dirs.push(real);
        }
    }
    dirs
}

/// Windows' canonical paths come as `\\?\C:\...`, which an engine does not take for a folder.
fn plain(p: &str) -> String {
    match p.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => p.to_owned(),
    }
}

/// The variables bin/kith sets for the run itself: the engine does not get them.
fn run_vars(names: impl Iterator<Item = String>) -> Vec<String> {
    names.filter(|n| n.starts_with("KITH_RUN_")).collect()
}

/// Completes when the run is told to stop (TERM or INT; Ctrl-C elsewhere), leaving the code to exit with in `code`.
async fn stop_signal(code: Arc<AtomicI32>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => code.store(143, Ordering::SeqCst),
            _ = int.recv() => code.store(130, Ordering::SeqCst),
        }
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_ok() {
            code.store(130, Ordering::SeqCst);
        } else {
            std::future::pending::<()>().await;
        }
    }
}

/// The ledger row, in process (no `kith-cost`); a failed write is said and never fails the run.
fn record(row: &Value) {
    #[cfg(unix)]
    {
        let home = kith_kernel::paths::process_home();
        let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
        let paths = kith_kernel::cost::Paths::from_process(&kernel);
        let env = kith_kernel::cost::Env { stdin: Some(row.to_string()), ..kith_kernel::cost::Env::from_process() };
        let out = kith_kernel::cost::run(&paths, &env, &["record".to_owned()]);
        if !out.stderr.is_empty() {
            eprint!("{}", out.stderr);
        }
    }
    // The cost ledger is the Unix core's for now (kith_kernel::cost): no row on Windows.
    #[cfg(not(unix))]
    let _ = row;
}

/// The engine's own terminal session: its CLI with the run's flags, in place of this process where the system allows.
fn interactive(engine: &str, bypass: bool) -> Result<i32> {
    let prompt_file = std::env::var("KITH_RUN_PROMPT").context("KITH_RUN_PROMPT is not set (bin/kith sets it)")?;
    let inputs = engines::RunInputs {
        prompt: std::fs::read_to_string(&prompt_file).with_context(|| format!("reading {prompt_file}"))?,
        dirs: real_dirs(&std::env::var("KITH_RUN_DIRS").unwrap_or_default()),
        mcp: merge_mcp(&std::env::var("KITH_RUN_MCP").unwrap_or_default()),
        model: std::env::var("KITH_MODEL").unwrap_or_default(),
        effort: std::env::var("KITH_EFFORT").unwrap_or_default(),
        bypass,
        owner: std::env::var("KITH_RUN_OWNER").is_ok_and(|v| v == "1"),
        global_skills: std::env::var("KITH_RUN_GLOBAL_SKILLS").is_ok_and(|v| v == "1"),
    };
    let Some(tui) = engines::tui_argv(engine, &inputs) else {
        eprintln!("kith: no terminal session for {engine}; give a task");
        return Ok(1);
    };
    for note in &tui.notes {
        eprintln!("kith: {note}");
    }
    let mut cmd = std::process::Command::new(&tui.argv[0]);
    cmd.args(&tui.argv[1..]);
    for name in run_vars(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())) {
        cmd.env_remove(name);
    }
    cmd.envs(tui.env.iter().map(|(k, v)| (k, v)));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let e = cmd.exec();
        Err(anyhow!("{} is not installed or would not start ({e})", tui.argv[0]))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().with_context(|| format!("{} is not installed or would not start", tui.argv[0]))?;
        Ok(status.code().unwrap_or(1))
    }
}

/// Prints the run's result: `--output json`'s object, or the text and the error.
fn show(done: &Done, engine: &str, json: bool) {
    if json {
        println!("{}", done.json(engine));
    } else {
        if !done.text.is_empty() {
            println!("{}", done.text);
        }
        if let Some(e) = &done.error {
            eprintln!("kith: {engine}: {e}");
        }
    }
}

/// Runs it and returns the exit code.
pub async fn run(a: Args) -> Result<i32> {
    let engine =
        engines::find(&a.engine).ok_or_else(|| anyhow!("no engine called {} (kith engines lists them)", a.engine))?;
    if a.interactive {
        return interactive(engine.id, a.permission_mode.as_deref() == Some("bypassPermissions"));
    }
    let approve = a.permission_mode.as_deref() == Some("bypassPermissions");
    // Before anything starts: an engine that can't hold the run to its folders must not get the task.
    if let Some(why) = runonce::cannot_hold(engine, approve) {
        show(&Done::of(&a.engine, Err(anyhow!(why)), None), &a.engine, a.json);
        return Ok(runonce::CANNOT_HOLD);
    }
    let Some(command) = runonce::acp_command(engine, None).await? else {
        return Ok(runonce::NOT_AVAILABLE);
    };
    let task = a.task.join(" ");
    if task.trim().is_empty() {
        bail!("no task: give it after `--`");
    }
    let prompt_file = std::env::var("KITH_RUN_PROMPT").context("KITH_RUN_PROMPT is not set (bin/kith sets it)")?;
    let prompt = std::fs::read_to_string(&prompt_file).with_context(|| format!("reading {prompt_file}"))?;
    let dirs = real_dirs(&std::env::var("KITH_RUN_DIRS").unwrap_or_default());
    let mcp = merge_mcp(&std::env::var("KITH_RUN_MCP").unwrap_or_default());
    let model = std::env::var("KITH_MODEL").unwrap_or_default();
    let effort = std::env::var("KITH_EFFORT").unwrap_or_default();
    let only = std::env::var("KITH_RESUME_ONLY").is_ok_and(|v| v == "1");
    let resume = match resume_checked(&a.engine, a.resume.as_deref(), only) {
        Ok(r) => r,
        Err(why) => {
            show(&Done::of(&a.engine, Err(anyhow!(why)), None), &a.engine, a.json);
            return Ok(1);
        }
    };
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let spec = RunSpec {
        command,
        cwd,
        // The first folder is the one the run starts in.
        dirs: dirs.into_iter().skip(1).collect(),
        env: Vec::new(),
        model: Some(model.clone()).filter(|m| !m.is_empty()),
        modes: runonce::modes(engine, approve),
        mcp_servers: kith_acp::mcp_servers(&json!({ "mcpServers": mcp })),
        // A resumed session already holds the instructions: the note goes alone.
        task: task.clone(),
        fresh_task: Some(instructions_first(&prompt, &task)),
        resume,
        effort: Some(if effort == "max" { "xhigh".to_owned() } else { effort }).filter(|e| !e.is_empty()),
        env_remove: run_vars(std::env::vars_os().filter_map(|(k, _)| k.into_string().ok())),
        timeout: TIMEOUT,
        approve,
        allow_granted_mcp: engines::allows_granted_mcp_asks(engine.id) && !runonce::command_overridden(None),
    };
    let code = Arc::new(AtomicI32::new(0));
    let (result, spent) = kith_acp::run_once_until(spec, stop_signal(code.clone())).await;
    let stopped = code.load(Ordering::SeqCst);
    if stopped != 0 {
        return Ok(stopped);
    }
    let done = Done::of(&a.engine, result, spent);
    let model = if model.is_empty() { a.engine.as_str() } else { model.as_str() };
    if let Some(row) = done.row(&a.engine, model, &task, chrono::Utc::now()) {
        record(&row);
    }
    show(&done, &a.engine, a.json);
    Ok(i32::from(done.error.is_some()))
}

#[cfg(test)]
mod tests;
