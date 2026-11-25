//! One task on one engine over ACP, start to finish: spawn, initialize, open a session, send the task, collect the
//! answer, stop the agent (docs/plans/core-design.md section 7). The host's background jobs do the same by hand in
//! `chat/oneshot.rs`; this is the launcher the `kith` program and the kernel's one-shots use.

use crate::client::{Acp, Failure, FailureKind, Incoming, content_text};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// What to run.
#[derive(Default)]
pub struct RunSpec {
    /// The engine's ACP command line (its row's `local` mode with `{bin}` resolved, or a registry adapter's).
    pub command: String,
    pub cwd: String,
    /// More folders the engine may work in (ACP `additionalDirectories`); sent only to an agent that says it takes them.
    pub dirs: Vec<String>,
    /// Added to the agent's environment; kept out of the command line (it can hold API keys).
    pub env: Vec<(String, String)>,
    pub model: Option<String>,
    /// ACP session modes to run in, best first: the first the agent lists is set. Empty: the agent's own.
    pub modes: Vec<String>,
    /// ACP `mcpServers` for the session (see [`mcp_servers`]).
    pub mcp_servers: Value,
    pub task: String,
    /// Continue this session (`session/load`) when the agent advertises `loadSession`; `task` is then the next prompt.
    pub resume: Option<String>,
    /// The task as a new session needs it (the agent's instructions in front): sent instead of `task` when the run is
    /// not a resumed one, that is when `resume` is unset or the agent cannot load sessions.
    pub fresh_task: Option<String>,
    /// The reasoning effort to ask for: set when `session/new` lists a `thought_level` config option, else said on stderr.
    pub effort: Option<String>,
    /// Variables of this process kept out of the agent's environment.
    pub env_remove: Vec<String>,
    /// For the whole run, handshake included.
    pub timeout: Duration,
    /// Answer the engine's permission requests with its first "allow" option; otherwise they are refused.
    pub approve: bool,
    /// Without `approve`, still allow (once) the ask to run a tool of one of `mcp_servers`, when the agent marks it as an
    /// MCP tool approval and names the server in the call it reported first (see [`Asks::allows`]). Off: every ask is
    /// refused. Only for an agent whose own per-tool approval refuses a server the owner granted (codex-acp).
    pub allow_granted_mcp: bool,
}

/// What the run answers the agent's permission asks with, and what it has seen to decide that.
struct Asks {
    approve: bool,
    /// The servers of this run's `session/new`, when `allow_granted_mcp` is on; empty otherwise.
    granted: Vec<String>,
    /// `toolCallId` -> the MCP server of each tool call the agent reported (`rawInput.server`).
    servers: std::collections::HashMap<String, String>,
}

impl Asks {
    fn of(spec: &RunSpec) -> Self {
        let granted = if spec.allow_granted_mcp {
            spec.mcp_servers
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect()
        } else {
            Vec::new()
        };
        Self {
            approve: spec.approve,
            granted,
            servers: std::collections::HashMap::new(),
        }
    }

    /// The option to answer a `session/request_permission` with, if any. `approve`: the first allow option. Otherwise
    /// only an MCP tool approval (`_meta.is_mcp_tool_approval`) for a call this run saw reported on one of its granted
    /// servers, answered with `allow_once`: never an "always" option, which the agent keeps for later calls. If a later
    /// agent drops the `_meta` flag the run loses its MCP tools; it never gains a permission.
    fn allows<'a>(&self, params: &'a Value) -> Option<&'a str> {
        let options = params["options"].as_array()?;
        let kind = |o: &&Value, want: &dyn Fn(&str) -> bool| o["kind"].as_str().is_some_and(want);
        if self.approve {
            return options
                .iter()
                .find(|o| kind(o, &|k| k.starts_with("allow")))?["optionId"]
                .as_str();
        }
        if params["_meta"]["is_mcp_tool_approval"].as_bool() != Some(true) {
            return None;
        }
        let call = params["toolCall"]["toolCallId"].as_str()?;
        let server = self.servers.get(call)?;
        if !self.granted.contains(server) {
            return None;
        }
        options.iter().find(|o| kind(o, &|k| k == "allow_once"))?["optionId"].as_str()
    }
}

/// Tokens a run used, as the agent reported them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// ACP's `usage` (`inputTokens`, `outputTokens`, `cachedReadTokens`, `cachedWriteTokens`), also under `_meta`.
    pub fn of(result: &Value) -> Option<Self> {
        let u = [&result["usage"], &result["_meta"]["usage"]]
            .into_iter()
            .find(|u| u.is_object())?;
        let n = |names: [&str; 2]| names.iter().find_map(|k| u[*k].as_u64()).unwrap_or(0);
        let usage = Self {
            input: n(["inputTokens", "input_tokens"]),
            output: n(["outputTokens", "output_tokens"]),
            cache_read: n(["cachedReadTokens", "cache_read_input_tokens"]),
            cache_write: n(["cachedWriteTokens", "cache_creation_input_tokens"]),
        };
        (usage != Self::default()).then_some(usage)
    }

    pub fn to_json(self) -> Value {
        json!({"input": self.input, "output": self.output, "cache_read": self.cache_read, "cache_write": self.cache_write})
    }
}

/// What a finished run gives back.
#[derive(Debug)]
pub struct Outcome {
    pub text: String,
    pub stop_reason: Option<String>,
    pub usage: Option<Usage>,
    /// What the engine reported it cost, in US dollars. `None`: it reports none.
    pub cost_usd: Option<f64>,
    pub session_id: String,
}

impl Outcome {
    /// `{text, stop_reason, usage, cost_usd, session_id}`, the shape `kith kernel run-once --json` prints.
    pub fn to_json(&self) -> Value {
        json!({
            "text": self.text,
            "stop_reason": self.stop_reason,
            "usage": self.usage.map(Usage::to_json),
            "cost_usd": self.cost_usd,
            "session_id": self.session_id,
        })
    }
}

/// The cost in dollars an agent reports: in the prompt result (`usage.costUsd`, `_meta.costUsd`, `_meta.cost_usd`) or
/// in a `usage_update` (`cost: {amount, currency}`).
fn cost_of(v: &Value) -> Option<f64> {
    let direct = [
        &v["costUsd"],
        &v["cost_usd"],
        &v["usage"]["costUsd"],
        &v["_meta"]["costUsd"],
        &v["_meta"]["cost_usd"],
    ]
    .into_iter()
    .find_map(Value::as_f64);
    direct.or_else(|| {
        let c = &v["cost"];
        let usd = c["currency"]
            .as_str()
            .is_none_or(|cur| cur.eq_ignore_ascii_case("usd"));
        usd.then(|| c["amount"].as_f64()).flatten()
    })
}

/// A Claude-style MCP config (`{"mcpServers": {name: {command, args, env} | {url, headers}}}`, or just the inner map) as
/// ACP's `mcpServers` list.
pub fn mcp_servers(config: &Value) -> Value {
    let map = config.get("mcpServers").unwrap_or(config);
    let pairs = |v: &Value, key: &str| -> Vec<Value> {
        v.get(key)
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| json!({"name": k, "value": v.as_str().unwrap_or_default()}))
                    .collect()
            })
            .unwrap_or_default()
    };
    let list = map.as_object().into_iter().flatten().map(|(name, s)| {
        if let Some(url) = s["url"].as_str() {
            json!({"type": "http", "name": name, "url": url, "headers": pairs(s, "headers")})
        } else {
            json!({"name": name, "command": s["command"], "args": s.get("args").cloned().unwrap_or(json!([])),
                   "env": pairs(s, "env")})
        }
    });
    Value::Array(list.collect())
}

/// Runs the task and returns the final text, stop reason and what the engine reported. The agent is stopped
/// (and its process group with it) before this returns, whatever happened: a timeout, a refusal, a crash, or this
/// future being dropped. A sign-in need or a spent plan comes back as the errors [`crate::needs_sign_in`] and
/// [`crate::depleted`] read; a timeout as a [`Failure`] of kind [`FailureKind::Timeout`].
pub async fn run_once(spec: RunSpec) -> Result<Outcome> {
    run_once_spent(spec).await.0
}

/// [`run_once`], and with it what the engine had reported it cost when the run ended, whether it finished or not: a run
/// that crashed or timed out still spent that money, and a delegation's cost cap must count it.
pub async fn run_once_spent(spec: RunSpec) -> (Result<Outcome>, Option<f64>) {
    run_once_until(spec, std::future::pending()).await
}

/// [`run_once_spent`] that also stops, the agent with it, when `stop` completes (a TERM to the process that runs it): the
/// run then ends as a [`Failure`] of kind [`FailureKind::Exited`].
pub async fn run_once_until(
    spec: RunSpec,
    stop: impl std::future::Future<Output = ()>,
) -> (Result<Outcome>, Option<f64>) {
    let (acp, mut rx) =
        match Acp::spawn_without(&spec.command, &spec.cwd, &spec.env, &spec.env_remove) {
            Ok(spawned) => spawned,
            Err(e) => return (Err(e), None),
        };
    let mut cost = None;
    let run = tokio::time::timeout(spec.timeout, session(&acp, &mut rx, &spec, &mut cost));
    let result = tokio::select! {
        r = run => r,
        () = stop => Ok(Err(Failure::error(FailureKind::Exited, "stopped before it finished"))),
    };
    acp.kill().await;
    crate::stopped().await;
    let result = result.unwrap_or_else(|_| {
        Err(Failure::error(
            FailureKind::Timeout,
            format!(
                "the engine did not finish within {}s",
                spec.timeout.as_secs()
            ),
        ))
    });
    let spent = result.as_ref().ok().and_then(|o| o.cost_usd).or(cost);
    (result, spent)
}

async fn session(
    acp: &Acp,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    spec: &RunSpec,
    cost: &mut Option<f64>,
) -> Result<Outcome> {
    // The handshake is answered before anything else is read, but a hung agent still ends at the run's deadline.
    let mut asks = Asks::of(spec);
    let init = acp
        .request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "kith", "title": "Kith", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await?;
    let mut new_session = json!({"cwd": spec.cwd, "mcpServers": spec.mcp_servers});
    if !spec.dirs.is_empty() {
        if init["agentCapabilities"]["sessionCapabilities"]["additionalDirectories"].is_object() {
            new_session["additionalDirectories"] = json!(spec.dirs);
        } else {
            eprintln!(
                "kith: this agent works in one directory over ACP; this run is without {}",
                spec.dirs.join(", ")
            );
        }
    }
    let loadable = init["agentCapabilities"]["loadSession"].as_bool() == Some(true);
    let resumed = spec.resume.as_deref().filter(|_| loadable);
    if let Some(id) = spec.resume.as_deref().filter(|_| !loadable) {
        eprintln!("kith: this agent cannot resume session {id}; starting a new session");
    }
    let (created, sid) = if let Some(id) = resumed {
        new_session["sessionId"] = json!(id);
        let loaded = acp.request("session/load", new_session).await?;
        // The agent replays the session's history before it answers: none of it is this run's answer or cost.
        let (mut old_text, mut old_cost) = (String::new(), None);
        while let Ok(inc) = rx.try_recv() {
            if matches!(inc, Incoming::Closed { .. }) {
                break;
            }
            handle(acp, Some(inc), &mut old_text, &mut old_cost, &mut asks).await?;
        }
        // Only calls made in this run count towards an ask, not the ones the history replay reported.
        asks.servers.clear();
        (loaded, id.to_owned())
    } else {
        let created = acp.request("session/new", new_session).await?;
        let sid = created["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("agent returned no sessionId"))?
            .to_owned();
        (created, sid)
    };
    if !spec.modes.is_empty() {
        select_mode(acp, &created, &sid, &spec.modes, spec.approve).await?;
    }
    if let Some(model) = spec.model.as_deref().filter(|m| !m.is_empty()) {
        select_model(acp, &created, &sid, model).await?;
    }
    if let Some(effort) = spec.effort.as_deref().filter(|e| !e.is_empty()) {
        select_effort(acp, &created, &sid, effort).await?;
    }
    let task = match (&spec.fresh_task, resumed) {
        (Some(fresh), None) => fresh,
        _ => &spec.task,
    };
    let prompt = acp.request(
        "session/prompt",
        json!({"sessionId": sid, "prompt": [{"type": "text", "text": task}]}),
    );
    tokio::pin!(prompt);
    let mut text = String::new();
    let result = loop {
        // Biased: when the process dies, `Closed` (with its stderr) is queued before the request fails.
        tokio::select! {
            biased;
            inc = rx.recv() => {
                // An agent that answers and exits at once queues `Closed` behind its answer: the answer is in already
                // (the reader fills it before it reads the close), so take it instead of failing the finished run.
                if matches!(inc, None | Some(Incoming::Closed { .. }))
                    && let Ok(answered) = tokio::time::timeout(Duration::ZERO, &mut prompt).await
                    && answered.is_ok()
                {
                    break answered?;
                }
                handle(acp, inc, &mut text, cost, &mut asks).await?;
            }
            r = &mut prompt => break r?,
        }
    };
    // Whatever the agent streamed before its answer is already queued; a close after the answer is not a failure.
    while let Ok(inc) = rx.try_recv() {
        if matches!(inc, Incoming::Closed { .. }) {
            break;
        }
        handle(acp, Some(inc), &mut text, cost, &mut asks).await?;
    }
    Ok(Outcome {
        text,
        stop_reason: result["stopReason"].as_str().map(str::to_owned),
        usage: Usage::of(&result),
        cost_usd: cost_of(&result).or(*cost),
        session_id: sid,
    })
}

/// The first of `modes` the agent lists in `session/new`. None listed: a run without `approve` ends before its task is
/// sent (the agent's own mode may reach further than asked); an approved one stays as it started, said on stderr.
async fn select_mode(
    acp: &Acp,
    created: &Value,
    sid: &str,
    modes: &[String],
    approve: bool,
) -> Result<()> {
    let listed = created["modes"]["availableModes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let Some(mode) = modes
        .iter()
        .find(|m| listed.iter().any(|l| l["id"].as_str() == Some(m.as_str())))
    else {
        if !approve {
            bail!(
                "this agent offers none of the session modes {}; the task was not sent",
                modes.join(", ")
            );
        }
        eprintln!(
            "kith: this agent offers none of the session modes {}; this run is in its own mode",
            modes.join(", ")
        );
        return Ok(());
    };
    acp.request(
        "session/set_mode",
        json!({"sessionId": sid, "modeId": mode}),
    )
    .await
    .map(|_| ())
    .map_err(|e| e.context(format!("Couldn't set session mode {mode}")))
}

/// The agent's model list may be a config option (`category: model`) or the older `session/set_model`.
async fn select_model(acp: &Acp, created: &Value, sid: &str, model: &str) -> Result<()> {
    let option = created["configOptions"].as_array().and_then(|options| {
        let is_select = |v: &&Value| v["type"] == "select" || v.get("type").is_none();
        options
            .iter()
            .filter(is_select)
            .find(|v| v["category"] == "model")
            .or_else(|| {
                options
                    .iter()
                    .filter(is_select)
                    .find(|v| v["id"] == "model" || v["id"] == "models")
            })
    });
    let (method, params) = match option.and_then(|o| o["id"].as_str()) {
        Some(id) => (
            "session/set_config_option",
            json!({"sessionId": sid, "configId": id, "value": model}),
        ),
        None => (
            "session/set_model",
            json!({"sessionId": sid, "modelId": model}),
        ),
    };
    acp.request(method, params)
        .await
        .map(|_| ())
        .map_err(|e| e.context(format!("Couldn't select model {model}")))
}

/// Sets the effort through the agent's `thought_level` config option when it has one that takes the value; otherwise says
/// on stderr that the run is without it.
async fn select_effort(acp: &Acp, created: &Value, sid: &str, effort: &str) -> Result<()> {
    let option = created["configOptions"]
        .as_array()
        .and_then(|o| o.iter().find(|o| o["category"] == "thought_level"));
    let takes = |o: &Value| {
        // Flat `{value}` entries, or groups of them; an option that lists none takes any value.
        let mut values = Vec::new();
        for item in o["options"].as_array().into_iter().flatten() {
            match item["options"].as_array() {
                Some(group) => values.extend(group.iter().filter_map(|g| g["value"].as_str())),
                None => values.extend(item["value"].as_str()),
            }
        }
        values.is_empty() || values.contains(&effort)
    };
    match option.filter(|o| takes(o)).and_then(|o| o["id"].as_str()) {
        Some(id) => acp
            .request(
                "session/set_config_option",
                json!({"sessionId": sid, "configId": id, "value": effort}),
            )
            .await
            .map(|_| ())
            .map_err(|e| e.context(format!("Couldn't set effort {effort}"))),
        None => {
            eprintln!(
                "kith: this agent takes no effort per run over ACP; this run is without effort {effort}"
            );
            Ok(())
        }
    }
}

/// Keeps reply text and the reported cost; answers what the agent asks of us. Errors once the agent is gone.
async fn handle(
    acp: &Acp,
    inc: Option<Incoming>,
    text: &mut String,
    cost: &mut Option<f64>,
    asks: &mut Asks,
) -> Result<()> {
    match inc {
        None => Err(Failure::error(FailureKind::Exited, "agent process exited")),
        Some(Incoming::Closed { stderr_tail }) => {
            let said = stderr_tail.trim();
            let message = if said.is_empty() {
                "agent process exited".to_owned()
            } else {
                format!("agent process exited: {said}")
            };
            Err(Failure::error(FailureKind::Exited, message))
        }
        Some(Incoming::Notification { method, params }) => {
            let u = &params["update"];
            if method == "session/update" {
                match u["sessionUpdate"].as_str() {
                    Some("agent_message_chunk") => text.push_str(&content_text(&u["content"])),
                    Some("usage_update") => *cost = cost_of(u).or(*cost),
                    Some("tool_call") => {
                        // An MCP call names its server and its tool; anything less is not one.
                        if let (Some(id), Some(server), Some(_)) = (
                            u["toolCallId"].as_str(),
                            u["rawInput"]["server"].as_str(),
                            u["rawInput"]["tool"].as_str(),
                        ) {
                            asks.servers.insert(id.to_owned(), server.to_owned());
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        Some(Incoming::Request { id, method, params }) => {
            if method == "session/request_permission" {
                let allow = asks.allows(&params);
                let outcome = match allow {
                    Some(option) => json!({"outcome": "selected", "optionId": option}),
                    None => json!({"outcome": "cancelled"}),
                };
                acp.respond(id, json!({"outcome": outcome})).await
            } else {
                acp.respond_error(id, -32601, "not supported").await
            }
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
        assert_eq!(
            u,
            Some(Usage {
                input: 7,
                output: 3,
                cache_read: 2,
                cache_write: 0
            })
        );
        let meta = Usage::of(&json!({"_meta": {"usage": {"input_tokens": 5, "output_tokens": 1}}}));
        assert_eq!(
            meta,
            Some(Usage {
                input: 5,
                output: 1,
                cache_read: 0,
                cache_write: 0
            })
        );
        assert_eq!(Usage::of(&json!({"stopReason": "end_turn"})), None);
        assert_eq!(Usage::of(&json!({"usage": {}})), None);
    }

    #[test]
    fn cost_comes_from_the_result_or_a_usage_update_in_dollars_only() {
        assert_eq!(cost_of(&json!({"_meta": {"costUsd": 0.5}})), Some(0.5));
        assert_eq!(
            cost_of(&json!({"cost": {"amount": 0.25, "currency": "USD"}})),
            Some(0.25)
        );
        assert_eq!(
            cost_of(&json!({"cost": {"amount": 3.0, "currency": "EUR"}})),
            None
        );
        assert_eq!(cost_of(&json!({"stopReason": "end_turn"})), None);
    }

    #[test]
    fn mcp_config_becomes_acp_servers() {
        let cfg = json!({"mcpServers": {
            "a": {"command": "node", "args": ["x.js"], "env": {"K": "v"}},
            "b": {"url": "http://h/mcp", "headers": {"Authorization": "t"}},
        }});
        let list = mcp_servers(&cfg);
        assert_eq!(
            list[0],
            json!({"name": "a", "command": "node", "args": ["x.js"], "env": [{"name": "K", "value": "v"}]})
        );
        assert_eq!(
            list[1],
            json!({"type": "http", "name": "b", "url": "http://h/mcp", "headers": [{"name": "Authorization", "value": "t"}]})
        );
        assert_eq!(mcp_servers(&json!({})), json!([]));
    }
}
