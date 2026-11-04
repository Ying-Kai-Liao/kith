//! Background jobs on the bot's own harness (tidying, automatic naming): one prompt to a throwaway agent.

use crate::agent::acp::{self, Acp, Incoming};
use crate::agent::bot::{Adapter, Instructions};
use crate::hub::Hub;
use crate::store::BotConfig;
use anyhow::{Result, anyhow, bail};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(180);

/// What a background job answered, and what it used when the agent says.
pub struct Reply {
    pub text: String,
    /// The job's session in the agent (the cost ledger's session id).
    pub session: String,
    pub usage: Option<Usage>,
}

/// Tokens a job used, as the agent reported them in its answer to `session/prompt`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// ACP's `usage` (`inputTokens`, `outputTokens`, `cachedReadTokens`, `cachedWriteTokens`), also under `_meta`.
    fn of(result: &serde_json::Value) -> Option<Self> {
        let u = [&result["usage"], &result["_meta"]["usage"]].into_iter().find(|u| u.is_object())?;
        let n = |names: [&str; 2]| names.iter().find_map(|k| u[*k].as_u64()).unwrap_or(0);
        let usage = Self {
            input: n(["inputTokens", "input_tokens"]),
            output: n(["outputTokens", "output_tokens"]),
            cache_read: n(["cachedReadTokens", "cache_read_input_tokens"]),
            cache_write: n(["cachedWriteTokens", "cache_creation_input_tokens"]),
        };
        (usage != Self::default()).then_some(usage)
    }
}

/// Runs one prompt on a throwaway agent of the bot's harness and returns its reply.
/// Claude gets a real system prompt, no tools, no settings and no saved session; an agent with ACP
/// `systemPrompt` gets the instructions there, other harnesses inline. None of them may remember the job.
pub async fn one_shot(hub: &Arc<Hub>, cfg: &BotConfig, system: &str, user: &str) -> Result<String> {
    if cfg.is_hq() {
        bail!("{} is an hq agent: the host runs no background jobs as it", cfg.name);
    }
    Ok(run(hub, cfg, system, user, false).await?.text)
}

/// [`one_shot`] with what the job used, for a job whose cost goes in a ledger (the thread summaries).
pub async fn one_shot_reply(hub: &Arc<Hub>, cfg: &BotConfig, system: &str, user: &str) -> Result<Reply> {
    if cfg.is_hq() {
        bail!("{} is an hq agent: the host runs no background jobs as it", cfg.name);
    }
    run(hub, cfg, system, user, false).await
}

/// [`one_shot`] for an hq agent's tidying. The agent itself is not launched (no `--launch-json`, tools, servers or
/// settings of its own), so the job takes no agent slot: it is plain Claude with model haiku, whatever the agent's
/// own model. A test host names the ACP command in `KITH_SERVE_HQ_ACP_COMMAND`, as it does for the agents.
pub async fn one_shot_plain(hub: &Arc<Hub>, system: &str, user: &str) -> Result<Reply> {
    let cfg: BotConfig =
        serde_json::from_value(json!({"id": "background", "name": "background", "backend": "claude", "cwd": "/"}))?;
    run(hub, &cfg, system, user, true).await
}

async fn run(hub: &Arc<Hub>, cfg: &BotConfig, system: &str, user: &str, plain: bool) -> Result<Reply> {
    let cwd = crate::service::data_dir().join("background");
    tokio::fs::create_dir_all(&cwd).await?;
    let cwd = cwd.to_string_lossy().into_owned();
    crate::market::vault::unlock_if_sealed(hub, crate::agent::auth::env_slot(&cfg.backend)).await?;
    let mut env = crate::agent::auth::env(&hub.store, &cfg.backend)?;
    let mut commands = match std::env::var(crate::agent::hq::ACP_COMMAND_ENV) {
        Ok(command) if plain && !command.is_empty() => vec![command],
        _ => crate::agent::bot::launch_commands(cfg, |_| {}).await?,
    };
    crate::agent::own_memory::turn_off(&cfg.backend, &cwd, &mut env, &mut commands).await;
    let mut conn = None;
    let mut last_err = None;
    for command in commands {
        match crate::agent::bot::start_agent(&command, &cwd, &env, Duration::from_secs(60)).await {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let mut conn = conn.ok_or_else(|| last_err.unwrap_or_else(|| anyhow!("no agent to run a background job")))?;
    let acp = conn.acp.clone();
    let result = async {
        let mut params = json!({"cwd": cwd, "mcpServers": []});
        let text = match conn.instructions() {
            Instructions::ClaudeMeta => {
                params["_meta"] = json!({
                    "systemPrompt": system,
                    "claudeCode": {"options": {"tools": [], "persistSession": false, "settingSources": [], "model": "haiku"}},
                });
                user.to_owned()
            }
            Instructions::SystemPrompt => {
                params["systemPrompt"] = crate::agent::bot::system_prompt(system);
                user.to_owned()
            }
            Instructions::FirstMessage => format!("{system}\n\n---\n\n{user}"),
        };
        let res = acp.request("session/new", params).await?;
        let sid = res["sessionId"].as_str().ok_or_else(|| anyhow!("agent returned no sessionId"))?.to_owned();
        // Same model as the bot: the harness default may be another (local) model that
        // would load next to the bot's, or a cloud one the user kept these chats away from.
        if conn.adapter != Adapter::Claude {
            crate::agent::bot::select_model(&acp, &res, &sid, cfg).await?;
        }
        let prompt = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]}));
        tokio::pin!(prompt);
        let deadline = tokio::time::sleep(TIMEOUT);
        tokio::pin!(deadline);
        let mut out = String::new();
        let usage;
        loop {
            tokio::select! {
                r = &mut prompt => { usage = Usage::of(&r?); break }
                inc = conn.rx.recv() => {
                    if !collect(&acp, inc, &mut out).await {
                        bail!("the background job's agent exited");
                    }
                }
                () = &mut deadline => bail!("the background job timed out"),
            }
        }
        while let Ok(inc) = conn.rx.try_recv() {
            collect(&acp, Some(inc), &mut out).await;
        }
        Ok(Reply { text: out, session: sid, usage })
    }
    .await;
    acp.kill().await;
    result
}

/// Keeps reply text; refuses anything the agent asks of us. False once the agent is gone.
async fn collect(acp: &Acp, inc: Option<Incoming>, out: &mut String) -> bool {
    match inc {
        None | Some(Incoming::Closed { .. }) => false,
        Some(Incoming::Notification { method, params }) => {
            let u = &params["update"];
            if method == "session/update" && u["sessionUpdate"] == "agent_message_chunk" {
                out.push_str(&acp::content_text(&u["content"]));
            }
            true
        }
        Some(Incoming::Request { id, method, .. }) => {
            let _ = if method == "session/request_permission" {
                acp.respond(id, json!({"outcome": {"outcome": "cancelled"}})).await
            } else {
                acp.respond_error(id, -32601, "not supported").await
            };
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_read_from_the_prompt_result_when_the_agent_reports_it() {
        let u = Usage::of(
            &json!({"stopReason": "end_turn", "usage": {"inputTokens": 7, "outputTokens": 3, "cachedReadTokens": 2}}),
        );
        assert_eq!(u, Some(Usage { input: 7, output: 3, cache_read: 2, cache_write: 0 }));
        let meta = Usage::of(&json!({"_meta": {"usage": {"input_tokens": 5, "output_tokens": 1}}}));
        assert_eq!(meta, Some(Usage { input: 5, output: 1, cache_read: 0, cache_write: 0 }));
        assert_eq!(Usage::of(&json!({"stopReason": "end_turn"})), None);
        assert_eq!(Usage::of(&json!({"usage": {}})), None);
    }
}
