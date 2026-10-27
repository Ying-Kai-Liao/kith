//! The agent process: which commands start it, the ACP handshake, the bot's model, and how its instructions are
//! attached to a session request.

use super::{Adapter, Conn, SessionCaps};
use crate::agent::acp::{self, Acp};
use crate::store::BotConfig;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// ACP's `systemPrompt`: the bot's instructions added to the agent's own.
pub(crate) fn system_prompt(system: &str) -> Value {
    json!({"mode": "append", "content": [{"type": "text", "text": system}]})
}

/// Claude's `_meta`: the bot's instructions appended to Claude Code's own system prompt, and its options. The
/// per-session parts of Claude Code's prompt (working directory, git status) move to the first message, so a
/// bot's chat, threads and routines share one cached system prompt.
pub(super) fn claude_meta(cfg: &BotConfig, system: &str) -> Value {
    json!({"systemPrompt": {"append": system, "excludeDynamicSections": true}, "claudeCode": claude_options(cfg)})
}

/// Claude Code's own memory and its auto-tidy stay off, so Kith's is the bot's one memory, unless the user kept
/// them on; then Claude Code sets both itself ([design](../../../../docs/design/bot-memory.md#claude-codes-own-memory)).
fn claude_options(cfg: &BotConfig) -> Value {
    // The bot's replies and memory are Kith's own tools, which follow the bot's permission inside Kith: the
    // user's Claude Code auto-mode classifier mustn't refuse them (it blocked a reply naming the user's daughter).
    let mut settings = json!({"permissions": {"allow": ["mcp__chat", "mcp__memory"]}});
    if !crate::agent::own_memory::kept(cfg) {
        settings["autoMemoryEnabled"] = false.into();
        settings["autoDreamEnabled"] = false.into();
    }
    json!({"options": {"settings": settings}})
}

/// Shell commands that may start the bot's agent over ACP, best first.
/// `progress` receives short status lines ("Downloading Cursor…").
pub(crate) async fn launch_commands(cfg: &BotConfig, progress: impl Fn(&str)) -> Result<Vec<String>> {
    Ok(match cfg.command.as_deref().map(str::trim) {
        Some(c) if !c.is_empty() => vec![c.to_owned()],
        _ => crate::agent::backends::launch_candidates(&cfg.backend, progress)
            .await?
            .iter()
            .map(crate::agent::registry::Cmd::acp)
            .collect(),
    })
}

/// Switches a session (`session/new` or `session/load` answered `res`) to the bot's
/// chosen model, then sets that model's options (effort, fast…). Returns the answer holding
/// the session's config options now: a model can change them.
pub(crate) async fn select_model(acp: &Acp, res: &Value, sid: &str, cfg: &BotConfig) -> Result<Value> {
    let Some(model) = cfg.model.as_deref().filter(|m| !m.is_empty()) else { return Ok(res.clone()) };
    let Some(config_id) = crate::agent::auth::model_config(res).and_then(|c| c["id"].as_str()) else {
        let r =
            timed_request(acp, "session/set_model", json!({"sessionId": sid, "modelId": model}), REQUEST_TIMEOUT).await;
        return r.map(|_| res.clone()).map_err(|e| model_error(model, e));
    };
    let (model, mut options) = split_variant(res, model);
    options.extend(cfg.model_options.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let set = |id: &str, value: &str| {
        let params = json!({"sessionId": sid, "configId": id, "value": value});
        timed_request(acp, "session/set_config_option", params, REQUEST_TIMEOUT)
    };
    let mut current = set(config_id, model).await.map_err(|e| model_error(model, e))?;
    for (id, value) in options {
        let answer = set(id, value).await.map_err(|e| model_error(&format!("{model} with {id}={value}"), e))?;
        if answer["configOptions"].is_array() {
            current = answer;
        }
    }
    Ok(if current["configOptions"].is_array() { current } else { res.clone() })
}

/// How long the agent may take to answer a request that switches the model. As generous as `session/new`: it only
/// recovers an adapter that went silent, it never limits one that is slow but working.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// A request the agent must answer within `limit`. A silent agent is stopped (a hung one would hold the bot, and its
/// Stop, forever) and the failure is a [`acp::FailureKind::Timeout`], which callers use to drop the dead process.
pub(crate) async fn timed_request(acp: &Acp, method: &str, params: Value, limit: Duration) -> Result<Value> {
    if let Ok(result) = tokio::time::timeout(limit, acp.request(method, params)).await {
        return result;
    }
    acp.kill().await;
    let message = format!("the agent didn't answer {method} within {}s", limit.as_secs());
    Err(acp::Failure::error(acp::FailureKind::Timeout, message))
}

/// Whether `e` is an agent that stopped answering (see [`timed_request`]): its process is gone.
pub(crate) fn timed_out(e: &anyhow::Error) -> bool {
    e.downcast_ref::<acp::Failure>().is_some_and(|f| f.kind == acp::FailureKind::Timeout)
}

fn model_error(model: &str, e: anyhow::Error) -> anyhow::Error {
    tracing::warn!(model, error = format!("{e:#}"), "couldn't set model");
    if timed_out(&e) {
        // Kept as it is, so the caller can tell the agent is gone.
        return e.context(format!("Couldn't select model {model}"));
    }
    anyhow!("Couldn't select model {model}: {e:#}")
}

/// Cursor names a model variant `grok-4.7[reasoning_effort=high,fast=true]`. When the agent
/// lists plain models instead, the bracket becomes options of the plain model.
fn split_variant<'a>(res: &Value, model: &'a str) -> (&'a str, Vec<(&'a str, &'a str)>) {
    let listed = |id: &str| crate::agent::auth::model_ids(res).iter().any(|m| m == id);
    if let Some((name, params)) = model.strip_suffix(']').and_then(|m| m.split_once('['))
        && let Some(params) =
            params.split(',').filter(|p| !p.is_empty()).map(|p| p.split_once('=')).collect::<Option<Vec<_>>>()
        && !listed(model)
        && listed(name)
    {
        return (name, params);
    }
    (model, Vec::new())
}

/// Spawns an ACP agent and completes the `initialize` handshake.
pub(crate) async fn start_agent(command: &str, cwd: &str, env: &[(String, String)], budget: Duration) -> Result<Conn> {
    let (acp, rx) = Acp::spawn(command, cwd, env)?;
    let init = tokio::time::timeout(
        budget,
        acp.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                // `session.compaction`: tell us when the agent summarizes its context.
                // `parameterizedModelPicker`: Cursor lists plain models with their options
                // (effort, fast…) instead of one fixed variant each; model discovery asks the same.
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false,
                    "session": {"compaction": {}},
                    "_meta": {"parameterizedModelPicker": true},
                },
                "clientInfo": {"name": "kith", "title": "Kith", "version": env!("CARGO_PKG_VERSION")},
            }),
        ),
    )
    .await;
    let init = match init {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            acp.kill().await;
            return Err(e);
        }
        Err(_) => {
            acp.kill().await;
            let message = format!("the agent didn't answer the ACP handshake within {}s", budget.as_secs());
            return Err(acp::Failure::error(acp::FailureKind::Timeout, message));
        }
    };
    let sessions = SessionCaps {
        load: init["agentCapabilities"]["loadSession"].as_bool().unwrap_or(false),
        fork: init["agentCapabilities"]["sessionCapabilities"]["fork"].is_object(),
        close: init["agentCapabilities"]["sessionCapabilities"]["close"].is_object(),
        system_prompt: init["agentCapabilities"]["sessionCapabilities"]["systemPrompt"].is_object(),
        additional_directories: init["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"].is_object(),
    };
    Ok(Conn {
        acp,
        rx,
        sessions,
        adapter: adapter(&init),
        loaded: HashMap::new(),
        started: Instant::now(),
        fast_options: HashMap::new(),
        reports_compaction: false,
        usage: HashMap::new(),
        call_sessions: HashMap::new(),
        model_options: HashMap::new(),
        call_refused: HashSet::new(),
        profiles: HashMap::new(),
        run_grant: None,
    })
}

fn adapter(init: &Value) -> Adapter {
    if init["agentCapabilities"]["_meta"]["claudeCode"].is_object() {
        Adapter::Claude
    } else if init["agentInfo"]["name"].as_str().is_some_and(|name| name.ends_with("codex-acp")) {
        Adapter::Codex
    } else {
        Adapter::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_ids_become_options_only_when_the_agent_lists_plain_models() {
        let plain = json!({"configOptions": [{"id": "model", "category": "model", "type": "select",
            "options": [{"value": "grok-4.7"}]}]});
        let variant = "grok-4.7[context=256k,reasoning_effort=medium,fast=false]";
        assert_eq!(
            split_variant(&plain, variant),
            ("grok-4.7", vec![("context", "256k"), ("reasoning_effort", "medium"), ("fast", "false")])
        );
        assert_eq!(split_variant(&plain, "grok-4.7"), ("grok-4.7", vec![]));
        let variants = json!({"configOptions": [{"id": "model", "category": "model", "type": "select",
            "options": [{"value": variant}]}]});
        assert_eq!(split_variant(&variants, variant), (variant, vec![]));
        assert_eq!(split_variant(&plain, "other[x=1]"), ("other[x=1]", vec![]));
        // Claude's `opus[1m]` is a model id of its own, not a variant.
        assert_eq!(split_variant(&json!({"models": {"availableModels": [{"modelId": "opus"}]}}), "opus[1m]").1, vec![]);
        // Cursor's Auto: `default[]` with variants, `default` without.
        let auto = json!({"models": {"availableModels": [{"modelId": "default"}]}});
        assert_eq!(split_variant(&auto, "default[]"), ("default", vec![]));
    }

    #[test]
    fn claude_codes_memory_is_off_in_any_folder_unless_the_user_kept_it() {
        let bot = |kept: Value| -> BotConfig {
            serde_json::from_value(
                json!({"id": "a", "name": "B", "backend": "claude", "cwd": "/work/app", "harnessMemory": kept}),
            )
            .unwrap()
        };
        let settings = |cfg: &BotConfig| claude_options(cfg)["options"]["settings"].clone();
        // `null`: a client sending back a bot that had no switch.
        for off in [json!(false), Value::Null] {
            let off = settings(&bot(off));
            assert_eq!(off["autoMemoryEnabled"], false, "a project folder too");
            assert_eq!(off["autoDreamEnabled"], false);
        }
        let kept = settings(&bot(json!(true)));
        assert!(
            kept.get("autoMemoryEnabled").is_none() && kept.get("autoDreamEnabled").is_none(),
            "Claude Code decides"
        );
    }

    #[tokio::test]
    async fn an_agent_that_never_answers_a_model_switch_is_stopped_not_waited_for() {
        let (acp, _rx) = Acp::spawn("sleep 30", ".", &[]).unwrap();
        let error = timed_request(&acp, "session/set_model", json!({}), Duration::from_millis(100)).await.unwrap_err();
        assert!(timed_out(&error));
        let wrapped = model_error("opus", error);
        assert!(timed_out(&wrapped), "the caller still sees that the agent is gone");
        assert!(format!("{wrapped:#}").contains("Couldn't select model opus"));
        assert!(!timed_out(&anyhow!("Method not found")));
    }
}
