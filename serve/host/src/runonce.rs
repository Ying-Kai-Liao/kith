//! `kith kernel run-once <engine> -- <task>`: one task on one engine over ACP, the answer on stdout. The kernel's
//! one-shot path (`kernel/adapters/harness.py`) calls it for an engine whose table row says `oneshot: acp`
//! (docs/plans/core-design.md section 7). The run itself is `kith_acp::run_once`.

use anyhow::{Result, anyhow, bail};
use kith_acp::{Failure, FailureKind, RunSpec, depleted, needs_sign_in};
use kith_kernel::engines::{self, Oneshot};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

/// Exit code for "this engine has no ACP one-shot here" (a registry adapter only, or a native one-shot): the caller
/// runs the engine its own way. Nothing was started. (BSD `EX_CONFIG`.)
pub const NOT_AVAILABLE: i32 = 78;

pub struct Args {
    pub engine: String,
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub mcp_config: Option<String>,
    pub json: bool,
    pub approve: bool,
    pub timeout: u64,
    pub acp_command: Option<String>,
    pub task: Vec<String>,
}

/// Runs it and returns the exit code.
pub async fn run(a: Args) -> Result<i32> {
    let engine = engines::find(&a.engine).ok_or_else(|| anyhow!("no engine called {} (kith engines lists them)", a.engine))?;
    let command = match a.acp_command.clone().or_else(|| std::env::var("KITH_RUN_ONCE_ACP_COMMAND").ok()) {
        Some(c) if !c.is_empty() => c,
        _ => {
            let Some(local) = engine.local.filter(|_| matches!(engine.oneshot, Oneshot::Acp)) else {
                eprintln!("kith: {} has no ACP one-shot on this computer; run it its own way", engine.name);
                return Ok(NOT_AVAILABLE);
            };
            let Some((_, path)) = engines::detect(engine, &|b| crate::agent::backends::which(b)) else {
                bail!("{} is not installed ({})", engine.bins[0], engine.setup);
            };
            local.replace("{bin}", &crate::shell::quote(&path.to_string_lossy()))
        }
    };
    let task = if a.task.is_empty() {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        a.task.join(" ")
    };
    if task.trim().is_empty() {
        bail!("no task: give it after `--`, or on stdin");
    }
    let cwd = match a.cwd {
        Some(c) => c,
        None => std::env::current_dir()?,
    };
    let mcp_servers = match a.mcp_config.as_deref() {
        None | Some("") => json!([]),
        Some(c) => {
            let text = if c.trim_start().starts_with('{') { c.to_owned() } else { std::fs::read_to_string(c)? };
            kith_acp::mcp_servers(&serde_json::from_str::<Value>(&text).map_err(|e| anyhow!("--mcp-config: {e}"))?)
        }
    };
    let spec = RunSpec {
        command,
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
        model: a.model,
        mcp_servers,
        task,
        timeout: Duration::from_secs(a.timeout),
        approve: a.approve,
    };
    match kith_acp::run_once(spec).await {
        Ok(out) if a.json => {
            println!("{}", out.to_json());
            Ok(0)
        }
        Ok(out) => {
            if !out.text.is_empty() {
                println!("{}", out.text.trim_end());
            }
            Ok(0)
        }
        Err(e) => {
            let (kind, resets_at) = classify(&e);
            if a.json {
                println!("{}", json!({"error": format!("{e:#}"), "kind": kind, "resets_at": resets_at}));
            } else {
                eprintln!("kith: {} ({kind}): {e:#}", engine.id);
            }
            Ok(1)
        }
    }
}

/// What kind of failure `e` is, for the caller: `depleted` (with when the plan resets), `needs_sign_in`, `timeout`,
/// `exited` or plain `error`.
fn classify(e: &anyhow::Error) -> (&'static str, Option<i64>) {
    if let Some(d) = depleted(e) {
        return ("depleted", d.resets_at);
    }
    if needs_sign_in(e) {
        return ("needs_sign_in", None);
    }
    match e.downcast_ref::<Failure>().map(|f| f.kind) {
        Some(FailureKind::Timeout) => ("timeout", None),
        Some(FailureKind::Exited) => ("exited", None),
        _ => ("error", None),
    }
}
