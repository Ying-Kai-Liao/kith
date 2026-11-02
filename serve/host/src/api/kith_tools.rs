//! Connectors, skills and shared bots on a kith agent, through the CLI (plan step B2): each method builds one
//! `kith … --json` line and hands back the JSON the CLI printed. The app does nothing the CLI cannot. Owner only:
//! none of these is on a member's or a bot guest's list, so `guard` refuses them for anyone else.
//!
//! `kithConnect` / `kithDisconnect` / `kithSkillAdd` / `kithSkillRemove` / `kithTools` name the bot by its `botId`
//! (the row must be a kith agent); the catalog, market and install methods are about this Mac and take no bot.

use super::kith_info::{bot_agent, kith_only};
use super::str_arg;
use crate::agent::hq;
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "kithTools" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            // Two reads; one failing must not hide the other.
            let (c, s) = ([name.as_str(), "connectors", "--json"], [name.as_str(), "skills", "--json"]);
            let (connectors, skills) = tokio::join!(hq::json_cmd(&c), hq::json_cmd(&s));
            let mut out = json!({"connectors": [], "skills": []});
            match connectors {
                Ok(v) => out["connectors"] = v,
                Err(e) => out["connectorsError"] = format!("{e:#}").into(),
            }
            match skills {
                Ok(v) => out["skills"] = v,
                Err(e) => out["skillsError"] = format!("{e:#}").into(),
            }
            out
        }
        "kithConnect" | "kithDisconnect" | "kithSkillAdd" | "kithSkillRemove" | "kithConnectorsAvailable"
        | "kithSkillsAvailable" | "kithMarket" | "kithInstallPreview" | "kithInstall" => {
            let agent = if needs_bot(method) {
                Some(kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?)
            } else {
                None
            };
            let args = args_for(method, agent.as_deref(), &b)?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            hq::json_cmd(&args).await?
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

fn needs_bot(method: &str) -> bool {
    matches!(method, "kithConnect" | "kithDisconnect" | "kithSkillAdd" | "kithSkillRemove")
}

/// A name the CLI takes as one word: a connector id.
fn word<'a>(b: &'a Value, k: &str) -> Result<&'a str> {
    let v = str_arg(b, k)?.trim();
    let ok = v.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if ok { Ok(v) } else { bail!("`{k}` isn't a name the kith command takes") }
}

/// A skill id or a GitHub link: `name`, `owner/repo/path@ref` or a URL. Never an option.
fn link<'a>(b: &'a Value, k: &str) -> Result<&'a str> {
    let v = str_arg(b, k)?.trim();
    if v.is_empty() || v.starts_with('-') || v.contains(char::is_whitespace) {
        bail!("`{k}` isn't a skill name or GitHub link");
    }
    Ok(v)
}

/// The `kith` arguments for `method`. Pure, so the lines can be checked without running anything.
fn args_for(method: &str, agent: Option<&str>, b: &Value) -> Result<Vec<String>> {
    let agent = || agent.ok_or_else(|| anyhow!("this method needs a kith agent"));
    let s = |v: &str| v.to_owned();
    Ok(match method {
        "kithConnect" => {
            let mut a = vec![s(agent()?), s("connect"), s(word(b, "connector")?)];
            if let Some(p) = b["profile"].as_str().filter(|p| !p.is_empty()) {
                if !p.starts_with(|c: char| c.is_ascii_alphanumeric()) || p.contains(char::is_whitespace) {
                    bail!("`profile` isn't a profile name");
                }
                a.extend([s("--profile"), s(p)]);
            }
            a.push(s("--json"));
            a
        }
        "kithDisconnect" => vec![s(agent()?), s("disconnect"), s(word(b, "connector")?), s("--json")],
        "kithSkillAdd" => vec![s(agent()?), s("skill"), s("add"), s(link(b, "skill")?), s("--json")],
        "kithSkillRemove" => vec![s(agent()?), s("skill"), s("remove"), s(link(b, "skill")?), s("--json")],
        "kithConnectorsAvailable" => vec![s("connectors"), s("--json")],
        "kithSkillsAvailable" => {
            let mut a = vec![s("skills"), s("available")];
            if b["refresh"] == true {
                a.push(s("--refresh"));
            }
            a.push(s("--json"));
            a
        }
        "kithMarket" => {
            let mut a = vec![s("market")];
            // Search words only: one that looks like an option is dropped, never passed on.
            a.extend(b["query"].as_str().unwrap_or_default().split_whitespace().filter(|w| !w.starts_with('-')).map(s));
            if b["new"] == true {
                a.push(s("--new"));
            }
            a.push(s("--json"));
            a
        }
        "kithInstallPreview" => vec![s("install"), s(link(b, "link")?), s("--preview"), s("--json")],
        "kithInstall" => {
            let mut a = vec![s("install"), s(link(b, "link")?)];
            if let Some(name) = b["as"].as_str().filter(|n| !n.is_empty()) {
                let ok = name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                    && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if !ok {
                    bail!("`as` must be lowercase letters, digits and dashes");
                }
                a.extend([s("--as"), s(name)]);
            }
            a.extend([s("--yes"), s("--json")]);
            a
        }
        _ => bail!("no such method"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(method: &str, agent: Option<&str>, b: Value) -> String {
        args_for(method, agent, &b).unwrap().join(" ")
    }

    #[test]
    fn builds_the_cli_lines() {
        let a = Some("nova");
        assert_eq!(line("kithConnect", a, json!({"connector": "notion"})), "nova connect notion --json");
        assert_eq!(
            line("kithConnect", a, json!({"connector": "x", "profile": "x/work"})),
            "nova connect x --profile x/work --json"
        );
        assert_eq!(line("kithDisconnect", a, json!({"connector": "notion"})), "nova disconnect notion --json");
        assert_eq!(line("kithSkillAdd", a, json!({"skill": "pdf"})), "nova skill add pdf --json");
        assert_eq!(line("kithSkillRemove", a, json!({"skill": "pdf"})), "nova skill remove pdf --json");
        assert_eq!(line("kithConnectorsAvailable", None, json!({})), "connectors --json");
        assert_eq!(line("kithSkillsAvailable", None, json!({"refresh": true})), "skills available --refresh --json");
        assert_eq!(
            line("kithMarket", None, json!({"query": "video  --evil edit", "new": true})),
            "market video edit --new --json"
        );
        assert_eq!(line("kithInstallPreview", None, json!({"link": "a/b"})), "install a/b --preview --json");
        assert_eq!(line("kithInstall", None, json!({"link": "a/b", "as": "b2"})), "install a/b --as b2 --yes --json");
        assert_eq!(line("kithInstall", None, json!({"link": "a/b"})), "install a/b --yes --json");
    }

    #[test]
    fn refuses_anything_that_could_be_an_option() {
        let a = Some("nova");
        assert!(args_for("kithConnect", a, &json!({"connector": "--json"})).is_err());
        assert!(args_for("kithConnect", a, &json!({"connector": "x", "profile": "-x"})).is_err());
        assert!(args_for("kithSkillAdd", a, &json!({"skill": "--yes"})).is_err());
        assert!(args_for("kithSkillAdd", a, &json!({"skill": "a b"})).is_err());
        assert!(args_for("kithInstall", None, &json!({"link": "-x"})).is_err());
        assert!(args_for("kithInstall", None, &json!({"link": "a/b", "as": "Bad Name"})).is_err());
        assert!(args_for("kithConnect", None, &json!({"connector": "x"})).is_err());
    }

    #[test]
    fn only_the_owner_may_call_them() {
        for m in [
            "kithTools",
            "kithConnect",
            "kithDisconnect",
            "kithSkillAdd",
            "kithSkillRemove",
            "kithConnectorsAvailable",
            "kithSkillsAvailable",
            "kithMarket",
            "kithInstallPreview",
            "kithInstall",
        ] {
            assert!(!super::super::roles::MEMBER_METHODS.contains(&m), "{m} is on the member list");
            assert!(!super::super::roles::BOT_METHODS.contains(&m), "{m} is on the bot list");
            assert!(!super::super::roles::BOT_PENDING_METHODS.contains(&m), "{m} is on the pending list");
        }
    }
}
