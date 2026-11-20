//! The apps' Memory tab for an hq agent's bot: the agent's memory files and the shared user layer, as files with
//! versions, and the runs of its tidying. Contract in `serve/docs/bot-info.md`.
//!
//! Every method takes `botId`; those about a file also `shared` (the user layer's `user.md`, `people/`,
//! `projects/`; false: the agent's own `memory/`) and `name` (the path inside that layer).

use crate::hqmem::{self, Layout, tidy};
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

/// Tidy-ups a call lists.
const RUNS_DEFAULT: usize = 20;
const RUNS_MAX: usize = 100;
const VERSIONS_DEFAULT: usize = 50;
const VERSIONS_MAX: usize = 500;

pub const METHODS: &[&str] = &[
    "kithMemoryList",
    "kithMemoryRead",
    "kithMemorySave",
    "kithMemoryHistory",
    "kithMemoryDiff",
    "kithMemoryRestore",
    "kithMemoryDelete",
    "kithMemoryForget",
    "kithMemoryTidyUps",
    "kithMemoryUndoTidy",
    "kithMemoryPromotions",
    "kithMemoryAnswerPromotion",
    // Deprecated names of the two above, still called by app builds that predate the rename.
    "kithMemoryDreams",
    "kithMemoryUndoDream",
];

/// The agent of a bot, when it is an hq agent that still exists.
fn agent_of(hub: &Hub, id: &str) -> Result<String> {
    let cfg = hub.store.bot(id)?.filter(|b| !b.deleted).ok_or_else(|| anyhow!("no such bot"))?.config;
    let agent =
        cfg.hq_agent.clone().filter(|_| !cfg.is_group()).ok_or_else(|| anyhow!("this bot isn't a kith agent"))?;
    if hub.store.kv_get(&crate::hub::hq::gone_key(id)).is_some() {
        bail!("{agent} is no longer one of kith's agents");
    }
    Ok(agent)
}

fn str_arg<'a>(b: &'a Value, k: &str) -> Result<&'a str> {
    b[k].as_str().ok_or_else(|| anyhow!("{k} required"))
}

fn count(b: &Value, k: &str, default: usize, max: usize) -> usize {
    b[k].as_u64().map_or(default, |n| usize::try_from(n).unwrap_or(max)).clamp(1, max)
}

/// Blocking: reads files and the history database.
pub fn call(hub: &Hub, method: &str, b: &Value) -> Result<Value> {
    let agent = agent_of(hub, str_arg(b, "botId")?)?;
    let layout = Layout::from_env();
    let (shared, name) = (b["shared"] == true, b["name"].as_str().unwrap_or_default());
    let file = || -> Result<&str> { if name.is_empty() { Err(anyhow!("name required")) } else { Ok(name) } };
    let id = |k: &str| b[k].as_i64();
    Ok(match method {
        "kithMemoryList" => hqmem::list_files(&layout, &agent)?,
        "kithMemoryRead" => hqmem::read_file(&layout, &agent, shared, file()?)?,
        "kithMemorySave" => {
            let out = hqmem::save(&hqmem::open(&layout)?, &layout, &agent, shared, file()?, str_arg(b, "text")?)?;
            hub.emit_bot(str_arg(b, "botId")?);
            out
        }
        "kithMemoryHistory" => {
            let limit = count(b, "limit", VERSIONS_DEFAULT, VERSIONS_MAX);
            hqmem::history(&hqmem::open(&layout)?, &layout, &agent, shared, file()?, limit)?
        }
        "kithMemoryDiff" => {
            hqmem::diff(&hqmem::open(&layout)?, &layout, &agent, shared, file()?, id("from"), id("to"))?
        }
        "kithMemoryRestore" => {
            let version = id("version").ok_or_else(|| anyhow!("version required"))?;
            let out = hqmem::restore(&hqmem::open(&layout)?, &layout, &agent, shared, file()?, version)?;
            hub.emit_bot(str_arg(b, "botId")?);
            out
        }
        "kithMemoryDelete" => {
            let out = hqmem::delete(&hqmem::open(&layout)?, &layout, &agent, shared, file()?)?;
            hub.emit_bot(str_arg(b, "botId")?);
            out
        }
        // Owner-only like the rest of this tab: the fact is kept out of every agent's memory, as `kith memory forget`.
        "kithMemoryForget" => {
            let text = str_arg(b, "text")?.trim();
            if text.is_empty() {
                bail!("text required");
            }
            hqmem::forget(&hqmem::open(&layout)?, text)?;
            json!({"forgotten": true, "text": text})
        }
        "kithMemoryTidyUps" | "kithMemoryDreams" => {
            let runs = tidy::runs(&hqmem::open(&layout)?, &agent, count(b, "limit", RUNS_DEFAULT, RUNS_MAX))?;
            json!({"tidyUps": runs, "dreams": runs})
        }
        "kithMemoryUndoTidy" | "kithMemoryUndoDream" => {
            let run = id("id").ok_or_else(|| anyhow!("id required"))?;
            let undone = tidy::undo(&hqmem::open(&layout)?, &layout, &agent, run)?;
            tidy::reindex();
            hub.emit_bot(str_arg(b, "botId")?);
            let refused: Vec<Value> = undone.refused.iter().map(|(f, why)| json!({"file": f, "reason": why})).collect();
            json!({"restored": undone.restored, "refused": refused, "undone": undone.undone})
        }
        // Retags that tidying wants to make but that would add readers wait here for the person to answer (`asks`:
        // empty is the owner). "keep" leaves the note private; "approve" writes the wider tags as a tidy-up that
        // can be undone.
        "kithMemoryPromotions" => {
            let status = b["status"].as_str().unwrap_or("pending");
            let limit = count(b, "limit", RUNS_DEFAULT, RUNS_MAX);
            json!({"promotions": tidy::promotions(&hqmem::open(&layout)?, &agent, status, limit)?})
        }
        "kithMemoryAnswerPromotion" => {
            let id = id("id").ok_or_else(|| anyhow!("id required"))?;
            let approve = match str_arg(b, "answer")? {
                "approve" => true,
                "keep" => false,
                other => bail!("answer is approve or keep, not {other}"),
            };
            // This call comes from the owner's apps: it answers for the owner. A question for another person is
            // approved by that person, never through here.
            let done = tidy::answer(&hqmem::open(&layout)?, &layout, &agent, id, approve, "")?;
            if done.run.is_some() {
                tidy::reindex();
            }
            hub.emit_bot(str_arg(b, "botId")?);
            json!({"status": done.status, "run": done.run})
        }
        _ => bail!("unknown method: {method}"),
    })
}
