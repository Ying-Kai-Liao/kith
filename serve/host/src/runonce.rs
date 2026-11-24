//! `kith kernel run-once <engine> -- <task>`: one task on one engine over ACP, the answer on stdout. The kernel's
//! one-shot path (`kernel/adapters/harness.py`) calls it for an engine whose table row says `oneshot: acp`
//! (docs/plans/core-design.md section 7). The run itself is `kith_acp::run_once`.

use anyhow::{Result, anyhow, bail};
use kith_acp::{Failure, FailureKind, RunSpec, depleted, needs_sign_in};
use kith_kernel::engines::{self, Oneshot, Reach};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

/// Exit code for "this engine has no ACP one-shot here" (a native one-shot, or neither an ACP mode nor a registry
/// adapter Kith may start): the caller runs the engine its own way. Nothing was started. (BSD `EX_CONFIG`.)
pub const NOT_AVAILABLE: i32 = 78;

pub struct Args {
    pub engine: String,
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub dirs: Vec<PathBuf>,
    pub mcp_config: Option<String>,
    pub json: bool,
    pub approve: bool,
    pub timeout: u64,
    pub acp_command: Option<String>,
    pub task: Vec<String>,
}

/// Runs it and returns the exit code.
pub async fn run(a: Args) -> Result<i32> {
    let engine =
        engines::find(&a.engine).ok_or_else(|| anyhow!("no engine called {} (kith engines lists them)", a.engine))?;
    let command = match a.acp_command.clone().or_else(|| std::env::var("KITH_RUN_ONCE_ACP_COMMAND").ok()) {
        Some(c) if !c.is_empty() => c,
        // A registry adapter only for an engine whose session modes Kith sets (codex): any other would run in the
        // adapter's own default mode.
        _ if !matches!(engine.oneshot, Oneshot::Acp)
            || (engine.local.is_none() && engines::session_modes(engine.id, Reach::Workspace).is_empty()) =>
        {
            eprintln!("kith: {} has no ACP one-shot on this computer; run it its own way", engine.name);
            return Ok(NOT_AVAILABLE);
        }
        _ => {
            let Some((_, path)) = engines::detect(engine, &|b| crate::agent::backends::which(b)) else {
                bail!("{} is not installed ({})", engine.bins[0], engine.setup);
            };
            match engine.local {
                Some(local) => local.replace("{bin}", &crate::shell::quote(&path.to_string_lossy())),
                // No ACP mode in the CLI itself (codex): its registry adapter, as the app's chats start it.
                None => match crate::agent::backends::launch_candidates(engine.id, |_| {}).await {
                    Ok(cmds) if !cmds.is_empty() => cmds[0].acp(),
                    other => {
                        let why = other.err().map(|e| format!(" ({e:#})")).unwrap_or_default();
                        eprintln!("kith: {} has no ACP adapter to start here{why}; run it its own way", engine.name);
                        return Ok(NOT_AVAILABLE);
                    }
                },
            }
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
        dirs: a.dirs.iter().map(|d| d.to_string_lossy().into_owned()).collect(),
        env: Vec::new(),
        model: a.model,
        // Native codex one-shots were workspace-write, or bypass with `--approve`; asks reach `handle`, which refuses
        // them without it.
        modes: engines::session_modes(engine.id, if a.approve { Reach::Full } else { Reach::Workspace })
            .iter()
            .map(|m| (*m).to_owned())
            .collect(),
        mcp_servers,
        task,
        timeout: Duration::from_secs(a.timeout),
        approve: a.approve,
    };
    let (result, spent) = kith_acp::run_once_spent(spec).await;
    match result {
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
                // `cost_usd`: what the engine had reported before it failed, which a delegation's cap still counts.
                println!(
                    "{}",
                    json!({"error": format!("{e:#}"), "kind": kind, "resets_at": resets_at, "cost_usd": spent})
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(engine: &str) -> Args {
        Args {
            engine: engine.into(),
            model: None,
            cwd: None,
            dirs: Vec::new(),
            mcp_config: None,
            json: true,
            approve: false,
            timeout: 5,
            // Empty: not given, and KITH_RUN_ONCE_ACP_COMMAND is not read either.
            acp_command: Some(String::new()),
            task: vec!["hello".into()],
        }
    }

    #[tokio::test]
    async fn registry_only_engines_but_codex_stay_native() {
        for engine in ["pi", "amp", "vibe", "antigravity"] {
            let e = engines::find(engine).unwrap();
            assert!(e.local.is_none() && matches!(e.oneshot, Oneshot::Acp), "{engine} changed in the table");
            assert_eq!(run(args(engine)).await.unwrap(), NOT_AVAILABLE, "{engine}");
        }
    }
}
