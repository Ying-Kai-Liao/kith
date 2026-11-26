//! `agentRules`: the permission rules an agent's runs start with, as data, for the app's Permissions screen. Owner only
//! (on neither the member nor the bot list) and read-only: nothing here writes a rule. The rules are the raw strings of
//! the files the launcher merges at launch (`role/settings.json` permissions, then `allowed.json`), placeholders such as
//! `{{HOME_REAL}}` left as written.

use super::str_arg;
use crate::hub::Hub;
use anyhow::{Context, Result, anyhow, bail};
use kith_kernel::launch::prompt;
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

pub(super) async fn call(_hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    if method != "agentRules" {
        return Ok(ControlFlow::Continue(b));
    }
    let agent = str_arg(&b, "agent")?.to_owned();
    let answer = tokio::task::spawn_blocking(move || {
        let home = kith_kernel::paths::known_home().unwrap_or_default();
        let p = crate::kernel::folders(&home);
        let kernel = crate::kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
        rules_of(&p, &kernel, &agent)
    })
    .await??;
    Ok(ControlFlow::Break(answer))
}

/// The agent's rules: its role folder's settings.json, then the allowed.json beside its role folder.
fn rules_of(p: &kith_kernel::paths::Paths, kernel: &Path, agent: &str) -> Result<Value> {
    if agent.is_empty() || agent.starts_with('.') || agent.contains(['/', '\\']) {
        bail!("no agent named \"{agent}\"");
    }
    let role_dir = prompt::agent_dir(p, kernel, agent).ok_or_else(|| anyhow!("no agent named \"{agent}\""))?;
    let mut rules = Vec::new();
    if let Some(doc) = read_json(&role_dir.join("settings.json"), "settings.json")? {
        for list in ["allow", "ask", "deny"] {
            for rule in doc["permissions"][list].as_array().into_iter().flatten().filter_map(Value::as_str) {
                rules.push(json!({"list": list, "rule": rule, "source": "role/settings.json"}));
            }
        }
    }
    if let Some(doc) = read_json(&p.agents.join(agent).join("allowed.json"), "allowed.json")? {
        for e in doc["allowed"].as_array().into_iter().flatten() {
            let Some(rule) = e["rule"].as_str() else { continue };
            let mut one = json!({"list": "allow", "rule": rule, "source": "allowed.json"});
            for key in ["id", "kind"] {
                if let Some(v) = e[key].as_str() {
                    one[key] = json!(v);
                }
            }
            rules.push(one);
        }
    }
    Ok(json!({"agent": agent, "rules": rules}))
}

/// A JSON file, `None` when it isn't there; malformed JSON is an error that names the file.
fn read_json(path: &Path, name: &str) -> Result<Option<Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {name}")),
    };
    serde_json::from_str(&text).map(Some).map_err(|e| anyhow!("{name} is not valid JSON ({e})"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn setup() -> (kith_kernel::paths::Paths, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("kith-agent-rules-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let p = kith_kernel::paths::paths(&root, &root.join("kernel"), &|_| None);
        (p, root.join("kernel"))
    }

    #[test]
    fn rules_come_from_both_files_tagged_by_source() {
        let (p, kernel) = setup();
        let home = p.agents.join("rm");
        std::fs::create_dir_all(home.join("role")).unwrap();
        std::fs::write(home.join("role/agent.md"), "x").unwrap();
        std::fs::write(
            home.join("role/settings.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"],"deny":["Read({{HOME_REAL}}/x)"],"ask":["Bash(rm:*)"]}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join("allowed.json"),
            r#"{"version":1,"extra":1,"allowed":[{"id":"ab","rule":"do one thing","kind":"prose","more":1},{"rule":"Bash(x)"}]}"#,
        )
        .unwrap();
        let got = rules_of(&p, &kernel, "rm").unwrap();
        assert_eq!(
            got,
            json!({"agent": "rm", "rules": [
                {"list": "allow", "rule": "Bash(ls:*)", "source": "role/settings.json"},
                {"list": "ask", "rule": "Bash(rm:*)", "source": "role/settings.json"},
                {"list": "deny", "rule": "Read({{HOME_REAL}}/x)", "source": "role/settings.json"},
                {"list": "allow", "rule": "do one thing", "source": "allowed.json", "id": "ab", "kind": "prose"},
                {"list": "allow", "rule": "Bash(x)", "source": "allowed.json"},
            ]})
        );
    }

    #[test]
    fn missing_files_are_empty_unknown_agents_and_bad_json_are_errors() {
        let (p, kernel) = setup();
        let home = p.agents.join("rm");
        std::fs::create_dir_all(home.join("role")).unwrap();
        std::fs::write(home.join("role/agent.md"), "x").unwrap();
        assert_eq!(rules_of(&p, &kernel, "rm").unwrap(), json!({"agent": "rm", "rules": []}));
        assert!(rules_of(&p, &kernel, "nobody").unwrap_err().to_string().contains("no agent named"));
        assert!(rules_of(&p, &kernel, "../rm").is_err());
        std::fs::write(home.join("allowed.json"), "{oops").unwrap();
        assert!(rules_of(&p, &kernel, "rm").unwrap_err().to_string().contains("allowed.json"));
        std::fs::write(home.join("allowed.json"), "{}").unwrap();
        std::fs::write(home.join("role/settings.json"), "[").unwrap();
        assert!(rules_of(&p, &kernel, "rm").unwrap_err().to_string().contains("settings.json"));
    }
}
