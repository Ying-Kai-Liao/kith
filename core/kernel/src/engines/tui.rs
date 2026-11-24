//! The terminal session of the two engines Kith runs interactively, Codex and Hermes: the engine's own CLI with the
//! run's prompt, folders, MCP servers, model and bypass flags. ACP has no TUI, so this is argv and env only, pure and
//! golden-tested (design 2026-11-21-harness-oneshots, section 3.3). One-shots do not come here: they run over ACP.

use serde_json::{Map, Value};

/// What bin/kith hands a run (`KITH_RUN_*`, `KITH_MODEL`, `KITH_EFFORT`, the permission mode).
#[derive(Debug, Default, Clone)]
pub struct RunInputs {
    pub prompt: String,
    /// Real paths, once each; the first is the folder the run starts in.
    pub dirs: Vec<String>,
    /// The merged `mcpServers` of the run's config files.
    pub mcp: Map<String, Value>,
    pub model: String,
    pub effort: String,
    pub bypass: bool,
    /// The engine's own skills and plugins stay on (`KITH_RUN_GLOBAL_SKILLS=1`).
    pub global_skills: bool,
}

/// The command to run, its extra environment, and what to tell the person on stderr before it starts.
#[derive(Debug, PartialEq, Eq)]
pub struct Tui {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// `None`: Kith has no terminal session for this engine.
pub fn tui_argv(engine: &str, run: &RunInputs) -> Option<Tui> {
    match engine {
        "codex" => Some(codex(run)),
        "hermes" => Some(hermes(run)),
        _ => None,
    }
}

fn effort_of(effort: &str) -> &str {
    if effort == "max" { "xhigh" } else { effort }
}

/// A TOML value for codex's `-c key=value`: JSON strings, lists and numbers are valid TOML. Non-ASCII is escaped, as
/// the Python `json.dumps` it replaces did, so the argv is the same bytes.
fn toml(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, x)| format!("{} = {}", toml(&Value::String(k.clone())), toml(x)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(toml).collect::<Vec<_>>().join(", ")),
        other => {
            let mut out = String::new();
            for c in other.to_string().chars() {
                if c.is_ascii() {
                    out.push(c);
                } else {
                    let mut units = [0u16; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{u:04x}"));
                    }
                }
            }
            out
        }
    }
}

fn codex(run: &RunInputs) -> Tui {
    let mut cfg = vec![format!(
        "developer_instructions={}",
        toml(&Value::String(run.prompt.clone()))
    )];
    if !run.effort.is_empty() {
        cfg.push(format!(
            "model_reasoning_effort={}",
            toml(&Value::String(effort_of(&run.effort).to_owned()))
        ));
    }
    if !run.bypass {
        cfg.push("sandbox_mode=\"workspace-write\"".to_owned());
        let roots = Value::Array(run.dirs.iter().cloned().map(Value::String).collect());
        cfg.push(format!(
            "sandbox_workspace_write.writable_roots={}",
            toml(&roots)
        ));
    }
    for (name, s) in &run.mcp {
        let mut entry = Map::new();
        if let Some(url) = s["url"].as_str().filter(|u| !u.is_empty()) {
            entry.insert("url".into(), url.into());
        } else {
            entry.insert("command".into(), s["command"].clone());
            entry.insert(
                "args".into(),
                s.get("args")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
        }
        if s["env"].as_object().is_some_and(|e| !e.is_empty()) {
            entry.insert("env".into(), s["env"].clone());
        }
        // The terminal session has no one to approve a call it did not ask for, and codex's default "prompt" refuses
        // every tool of a server the owner granted; only owner runs reach codex.
        entry.insert("default_tools_approval_mode".into(), "approve".into());
        // A bare key: codex keeps the quotes of mcp_servers."name" in the server's name and the run gets no tools.
        let bare = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        let key = if bare {
            name.clone()
        } else {
            toml(&Value::String(name.clone()))
        };
        cfg.push(format!("mcp_servers.{key}={}", toml(&Value::Object(entry))));
    }
    let mut argv = vec!["codex".to_owned()];
    for c in cfg {
        argv.extend(["-c".to_owned(), c]);
    }
    if !run.model.is_empty() {
        argv.extend(["-m".to_owned(), run.model.clone()]);
    }
    if run.bypass {
        argv.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    }
    if !run.global_skills {
        argv.extend(["--disable", "plugins", "--disable", "apps"].map(str::to_owned));
    }
    for d in &run.dirs {
        argv.extend(["--add-dir".to_owned(), d.clone()]);
    }
    Tui {
        argv,
        env: Vec::new(),
        notes: Vec::new(),
    }
}

fn hermes(run: &RunInputs) -> Tui {
    let mut notes = Vec::new();
    if !run.mcp.is_empty() {
        let names: Vec<&str> = run.mcp.keys().map(String::as_str).collect();
        notes.push(format!(
            "hermes takes no MCP servers per run; this run is without {} (Claude Code runs have them)",
            names.join(", ")
        ));
    }
    let mut argv = vec!["hermes".to_owned(), "chat".to_owned()];
    if !run.model.is_empty() {
        argv.extend(["-m".to_owned(), run.model.clone()]);
    }
    if !run.effort.is_empty() {
        argv.extend(["--reasoning".to_owned(), effort_of(&run.effort).to_owned()]);
    }
    if run.bypass {
        argv.push("--yolo".to_owned());
    }
    // In the environment, never written to the session's history.
    Tui {
        argv,
        env: vec![(
            "HERMES_EPHEMERAL_SYSTEM_PROMPT".to_owned(),
            run.prompt.clone(),
        )],
        notes,
    }
}

#[cfg(test)]
mod tests;
