//! The edge asks of a shared agent's run that the owner can keep (guest workbench 1.4): a host the sandbox asked to
//! reach, and a helper the run asked for (kith-permit's `ask_helper`). The owner's card adds "Always for <person>", which
//! keeps that one thing, and, when a job of the agent holds it, the job: "Turn on <job>" with every piece listed, or
//! "Keep <job> on" when a package update added the piece to a job already on. Nothing turns a whole job on silently,
//! and a path never names a job. What the card offered is kept on its entry (`edge`), so the answer grants what was shown.

use super::{Card, access};
use serde_json::{Value, json};

/// kith-permit's helper tool: its card waits in kith-permit, and a yes is written before the run hears it.
pub const HELPER_TOOL: &str = "mcp__kithpermit__ask_helper";
/// Claude Code's sandbox network ask. In a bot chat it has no tool name and is titled with the host.
const NETWORK_TOOL: &str = "SandboxNetworkAccess";
pub const ALWAYS: &str = "kith-always";
pub const JOB: &str = "kith-job";

/// A card's added line of text, its added options, and what they keep (`edge` on the entry).
pub type Extra = (String, Vec<Value>, Value);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    Domain(String),
    Helper(String),
}

impl Edge {
    fn kind(&self) -> &'static str {
        match self {
            Self::Domain(_) => "domains",
            Self::Helper(_) => "helpers",
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::Domain(v) | Self::Helper(v) => v,
        }
    }
}

/// The edge a card is about, from the card's own tool, input and resource (never from what a client sent).
pub fn of(tool: &str, input: &Value, resource: &str) -> Option<Edge> {
    if tool == access::TOOL {
        let host = resource.strip_prefix("access:domain:")?;
        return access::valid_domain(host).then(|| Edge::Domain(host.to_owned()));
    }
    if tool == HELPER_TOOL {
        let name = input["agent"].as_str()?;
        let ok = !name.is_empty()
            && name.len() <= 64
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && name.starts_with(|c: char| c.is_ascii_alphanumeric());
        return ok.then(|| Edge::Helper(name.to_owned()));
    }
    let obj = input.as_object()?;
    let host = obj.get("host")?.as_str()?;
    let only_host = obj.keys().all(|k| k == "host" || k == "port");
    (only_host && (tool == NETWORK_TOOL || tool == host) && access::valid_domain(host))
        .then(|| Edge::Domain(host.to_owned()))
}

/// What the owner's card adds for an edge: a line of text, the options to put before its No, and what they grant
/// (kept on the entry as `edge`). `always`: offer "Always for <person>" (an escape card has its own).
pub async fn extras(person: &str, name: &str, agent: &str, edge: &Edge, always: bool) -> Extra {
    let job = job_of(agent, person, edge).await;
    let mut text = String::new();
    let mut options = Vec::new();
    if always {
        options.push(json!({"optionId": ALWAYS, "name": format!("Always for {name}"), "kind": "allow_always"}));
    }
    let mut kept = json!({"kind": edge.kind(), "value": edge.value()});
    if let Some(j) = job {
        let label = j["label"].as_str().unwrap_or_default();
        let ticked = j["ticked"].as_bool() == Some(true);
        let added = j["added"][edge.kind()].as_array().is_some_and(|a| a.iter().any(|v| v == edge.value()));
        let pieces = json!({"domains": j["domains"], "helpers": j["helpers"], "doors": j["doors"]});
        if !ticked {
            text = format!("Part of {label}, not on for {name}.");
            options.push(json!({"optionId": JOB, "name": format!("Turn on {label}: {}", listed(&pieces)), "kind": "allow_always"}));
            kept["job"] = j["name"].clone();
            kept["shown"] = pieces;
        } else if added && j["declined"].as_bool() != Some(true) {
            text = format!("{label} is on for {name}. A package update added {} to it.", listed(&j["added"]));
            options.push(json!({"optionId": JOB, "name": format!("Keep {label} on, with {}", listed(&j["added"])), "kind": "allow_always"}));
            kept["job"] = j["name"].clone();
            kept["shown"] = pieces;
            kept["hash"] = j["hash"].clone();
        }
    }
    (text, options, kept)
}

/// The tool a bot chat's (ACP) ask is about: Claude Code's tool name, else its title (a sandbox network ask is titled
/// with its host). The card keeps it as its `tool`, which is what [`of`] reads again at the answer.
pub fn acp_tool<'a>(tool: &'a Value, title: &'a str) -> &'a str {
    tool["_meta"]["claudeCode"]["toolName"].as_str().unwrap_or(title)
}

/// What a bot chat's card for an edge the owner is asked about adds: its options go before the No in `options`, and
/// the line of text and what they keep come back. None when the ask is not an edge.
pub async fn for_acp(
    person: &str,
    name: &str,
    agent: &str,
    tool: &str,
    input: &Value,
    options: &mut Vec<Value>,
) -> Option<(String, Value)> {
    let edge = of(tool, input, "")?;
    let (text, more, kept) = extras(person, name, agent, &edge, true).await;
    insert(options, more);
    Some((text, kept))
}

/// The option a bot chat's agent is sent for the answer `chosen` to its card. The host's "always" and job options are
/// a yes to this one request (what they keep is the card's answer's to write), so the agent gets its own allow_once:
/// never an id it didn't offer, and never its allow_always. None: it offered no allow_once, so it can only be cancelled.
pub fn adapter_option<'a>(options: &'a Value, chosen: &'a str) -> Option<&'a str> {
    if !matches!(chosen, ALWAYS | JOB) {
        return Some(chosen);
    }
    options.as_array()?.iter().find(|o| o["kind"] == "allow_once")?["optionId"].as_str()
}

/// Puts `more` before the first No of `options`.
pub fn insert(options: &mut Vec<Value>, more: Vec<Value>) {
    let at = options
        .iter()
        .position(|o| o["kind"].as_str().is_some_and(|k| k.starts_with("reject")))
        .unwrap_or(options.len());
    options.splice(at..at, more);
}

/// The pieces of a job, as a card lists them: hosts, helpers, then doors by name.
fn listed(pieces: &Value) -> String {
    let names = |k: &str| {
        pieces[k].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v| v.as_str().map(str::to_owned))
    };
    let doors = names("doors").map(|d| d.split('@').next().unwrap_or_default().to_owned());
    names("domains").chain(names("helpers")).chain(doors).collect::<Vec<_>>().join(", ")
}

/// `kith-auth jobs <agent> --person <p> --domain|--helper <v> --json`: the job that holds the edge, as this person has it.
async fn job_of(agent: &str, person: &str, edge: &Edge) -> Option<Value> {
    let bin = crate::remote::account::bin().await?;
    let flag = match edge {
        Edge::Domain(_) => "--domain",
        Edge::Helper(_) => "--helper",
    };
    let out =
        crate::channels::hq::output(&bin, &["jobs", agent, "--person", person, flag, edge.value(), "--json"]).await?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["jobs"].get(0).cloned()
}

/// Writes what the answer to an edge card keeps, before the run hears the yes. A helper's "Just this once" is a once
/// grant kith ask uses up at the helper's launch; a domain's is the live yes alone. No to a widening card declines that
/// package update. `Err` says why kith-auth refused.
pub async fn apply(card: &Card, kept: &Value, option: Option<&str>, allow: bool) -> Result<(), String> {
    let Some(edge) = of(&card.tool, &serde_json::from_str(&card.input).unwrap_or(Value::Null), &card.resource) else {
        return Ok(());
    };
    let job = kept["job"].as_str().filter(|_| kept["kind"] == edge.kind() && kept["value"] == edge.value());
    let shown = kept["shown"].to_string();
    let args: Vec<&str> = match (allow, option, &edge, job) {
        (true, Some(JOB), _, Some(job)) => vec!["job", &card.agent, &card.person, job, "--shown", &shown],
        (true, Some(ALWAYS), Edge::Domain(host), _) => vec!["access", &card.agent, &card.person, "domain", host],
        (true, Some(ALWAYS), Edge::Helper(h), _) => vec!["helper", &card.agent, &card.person, h],
        (true, _, Edge::Helper(h), _) => vec!["helper", &card.agent, &card.person, h, "--once"],
        (false, Some(_), _, Some(job)) if kept["hash"].as_str().is_some_and(|h| !h.is_empty()) => {
            vec!["job", &card.agent, &card.person, job, "--decline", kept["hash"].as_str().unwrap_or_default()]
        }
        _ => return Ok(()),
    };
    let bin = crate::remote::account::bin().await.ok_or("kith-auth can't be reached")?;
    let out = crate::channels::hq::output(&bin, &args).await.ok_or("kith-auth didn't answer")?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) }
}

/// A helper card's yes that couldn't be kept as asked: the once grant, so the run asked can still go on.
pub async fn once_instead(card: &Card) -> Result<(), String> {
    apply(card, &Value::Null, Some("allow"), true).await
}
