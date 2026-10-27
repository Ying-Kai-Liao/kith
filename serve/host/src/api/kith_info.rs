//! What a kith bot is, for its detail page: the agent's `--info-json` passed through, a read of one file of its
//! memory folder, and a search of that memory. The contract is in `serve/docs/bot-info.md` (beside `place-folders.md`,
//! which `folders.rs` points to).
//! The reads are open to a paired phone (none is in `LOCAL_ONLY`); `kithSetPermission` writes through to hq, like `updateBot`
//! it needs the Control scope. The agent's standing "always" access grants are listed and taken away here too
//! (`kithAccessGrants`, `kithRemoveAccessGrant`). Not Kith's own bot memory (`memory`).

use super::str_arg;
use crate::agent::hq;
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::Read as _;
use std::ops::ControlFlow;
use std::path::{Component, Path};
use std::sync::Arc;

/// The most of a memory file one call returns.
const FILE_CAP: u64 = 512 * 1024;
const SEARCH_DEFAULT: u64 = 20;
const SEARCH_MAX: u64 = 50;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "kithInfo" => match bot_agent(hub, str_arg(&b, "botId")?)? {
            Agent::None => json!({"info": null, "fetchedAt": now_ms()}),
            Agent::Gone(name) => json!({
                "info": null, "fetchedAt": now_ms(),
                "error": format!("{name} is no longer one of kith's agents"),
            }),
            Agent::Kith(name) => {
                let got = hq::info(&name, b["refresh"] == true).await;
                match got.info {
                    Ok(info) => json!({"info": info, "fetchedAt": got.fetched_at}),
                    Err(e) => json!({"info": null, "error": e, "fetchedAt": got.fetched_at}),
                }
            }
        },
        // Write-through for the one setting hq keeps per Mac: the app's choice goes to hq (`kith <agent> set
        // permission`), and the answer is hq's info read back, so every device shows what the next turn will use.
        "kithSetPermission" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            let mode = b["mode"].as_str().ok_or_else(|| anyhow!("mode is required"))?;
            let got = hq::set_permission(&name, mode).await?;
            match got.info {
                Ok(info) => json!({"info": info, "fetchedAt": got.fetched_at}),
                Err(e) => json!({"info": null, "error": e, "fetchedAt": got.fetched_at}),
            }
        }
        // The model a voice call runs on (`kith <agent> set call-model`): sonnet, haiku, opus, `own` (the agent's own
        // model) or `default` (back to kith's default); hq's info read back, as with the permission mode.
        "kithSetCallModel" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            let model = b["model"].as_str().map(str::trim).ok_or_else(|| anyhow!("model is required"))?;
            if model.is_empty() || model.starts_with('-') || model.contains(char::is_whitespace) {
                bail!("model must be a model name, own or default");
            }
            let got = hq::set_call_model(&name, model).await?;
            match got.info {
                Ok(info) => json!({"info": info, "fetchedAt": got.fetched_at}),
                Err(e) => json!({"info": null, "error": e, "fetchedAt": got.fetched_at}),
            }
        }
        // The "always" answers to the agent's escape cards that kith-auth keeps (`cards::access`): what, for whom, since
        // when. Owner-only (not in `MEMBER_METHODS`): a member's own grants aren't theirs to list or take back.
        "kithAccessGrants" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            json!({"grants": crate::cards::access::standing(&name).await?})
        }
        // Takes one away; the next run of that person no longer gets it. The answer is the list read back.
        "kithRemoveAccessGrant" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            let (person, kind, target) = (str_arg(&b, "person")?, str_arg(&b, "kind")?, str_arg(&b, "target")?);
            json!({"grants": crate::cards::access::take_away(&name, person, kind, target).await?})
        }
        "kithMemoryFile" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            let path = str_arg(&b, "path")?.to_owned();
            let info = hq::info(&name, false).await.info;
            let info = info.map_err(|e| anyhow!("can't tell where {name}'s memory is: {e}"))?;
            let dir = info["memory"]["dir"].as_str().ok_or_else(|| anyhow!("{name} has no memory folder"))?;
            let dir = dir.to_owned();
            tokio::task::spawn_blocking(move || read_memory_file(Path::new(&dir), &path)).await??
        }
        "kithMemorySearch" => {
            let name = kith_only(bot_agent(hub, str_arg(&b, "botId")?)?)?;
            let query = b["query"].as_str().unwrap_or_default().trim();
            if query.is_empty() {
                return Ok(ControlFlow::Break(json!({"hits": []})));
            }
            let limit = b["limit"].as_u64().unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX) as usize;
            match hq::memory_search(&name, query, limit).await {
                Ok(hits) => {
                    // Learned-skill and shared-file hits keep absolute paths: only the ones inside the memory
                    // folder can be opened with `kithMemoryFile`.
                    let dir = hq::info(&name, false)
                        .await
                        .info
                        .ok()
                        .and_then(|i| i["memory"]["dir"].as_str().map(str::to_owned));
                    let hits = tokio::task::spawn_blocking(move || mark_openable(hits, dir.as_deref())).await?;
                    json!({"hits": hits})
                }
                Err(e) => json!({"hits": [], "error": format!("{e:#}")}),
            }
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

enum Agent {
    /// A bot that is not a kith agent (or a group).
    None,
    /// A kith bot whose agent kith no longer lists.
    Gone(String),
    Kith(String),
}

fn bot_agent(hub: &Hub, id: &str) -> Result<Agent> {
    let row = hub.store.bot(id)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("no such bot"))?;
    let cfg = row.config;
    let Some(name) = cfg.hq_agent.clone().filter(|_| !cfg.is_group()) else {
        return Ok(Agent::None);
    };
    let gone = hub.store.kv_get(&crate::hub::hq::gone_key(id)).is_some();
    Ok(if gone { Agent::Gone(name) } else { Agent::Kith(name) })
}

fn kith_only(agent: Agent) -> Result<String> {
    match agent {
        Agent::Kith(name) => Ok(name),
        Agent::None => bail!("this bot isn't a kith agent"),
        Agent::Gone(name) => bail!("{name} is no longer one of kith's agents"),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Adds `"openable"` to each hit: whether `kithMemoryFile` would accept its path.
fn mark_openable(hits: Vec<Value>, dir: Option<&str>) -> Vec<Value> {
    hits.into_iter()
        .map(|mut hit| {
            let ok = dir.zip(hit["path"].as_str()).is_some_and(|(d, p)| resolve_memory_file(Path::new(d), p).is_ok());
            hit["openable"] = ok.into();
            hit
        })
        .collect()
}

/// `path` (relative) inside `dir`, as a regular file. Both sides are resolved first, so a symlink that leaves the
/// folder is refused like `..` is.
fn resolve_memory_file(dir: &Path, path: &str) -> Result<(std::path::PathBuf, std::fs::Metadata)> {
    let rel = Path::new(path);
    if path.is_empty()
        || rel.is_absolute()
        || rel.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("`{path}` isn't a path inside the memory folder");
    }
    let root = dir.canonicalize().map_err(|_| anyhow!("the memory folder isn't readable here"))?;
    let file = root.join(rel).canonicalize().map_err(|_| anyhow!("`{path}` isn't in the memory folder"))?;
    if !file.starts_with(&root) {
        bail!("`{path}` is outside the memory folder");
    }
    let meta = std::fs::metadata(&file)?;
    if !meta.is_file() {
        bail!("`{path}` isn't a file");
    }
    Ok((file, meta))
}

/// `path` (relative) inside `dir`, as text, at most [`FILE_CAP`] bytes.
fn read_memory_file(dir: &Path, path: &str) -> Result<Value> {
    let (file, _) = resolve_memory_file(dir, path)?;
    // The checks above can go stale before the open (a FIFO or a link swapped in): never block on a pipe, never
    // follow a link, and trust only the open handle's own metadata.
    let handle = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(&file)
    }
    .map_err(|_| anyhow!("`{path}` can't be opened"))?;
    let meta = handle.metadata()?;
    if !meta.is_file() {
        bail!("`{path}` isn't a file");
    }
    let mut bytes = Vec::new();
    handle.take(FILE_CAP).read_to_end(&mut bytes)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Ok(json!({
        "path": path, "text": String::from_utf8_lossy(&bytes), "bytes": meta.len(),
        "modified": modified, "truncated": meta.len() > FILE_CAP,
    }))
}
