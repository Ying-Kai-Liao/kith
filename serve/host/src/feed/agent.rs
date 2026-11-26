//! `feed_post`, the agent's way onto the Feed (design section 2.1). The run socket's `feedPost` arm (`runauth`) has
//! already checked that the token is an owner's; what arrives here is the identity (host-issued, never the request's)
//! and the arguments. The author is `identity.agent`, the chat comes from the token's session, and the daily cap is
//! the host's. Every failure is an answer the agent can read, never a panic or a silent drop.

use super::{By, Fold, PostSource, PostWrite, fold, put_post};
use crate::chat::files::Snapshot;
use crate::hub::Hub;
use crate::runauth::RunIdentity;
use crate::store::{BotConfig, now_ms};
use anyhow::{Context as _, Result, bail};
use chrono::{Local, TimeZone};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Cards one agent's tool may start per local day. A post that replaces a card (a fold) is not one.
const DAILY_CAP: i64 = 12;
const LIMIT_ANSWER: &str = "feed limit for today: fold into an earlier post (fold: ...)";

/// Posts what a run's `feed_post` call says and answers it as the run socket does: `{ok, message}` or `{ok: false,
/// error, message}`.
pub async fn agent_post(hub: &Hub, who: &RunIdentity, args: &Value) -> Value {
    match post(hub, who, args).await {
        Ok(said) => json!({"ok": true, "message": said}),
        Err(error) => {
            let why = format!("{error:#}");
            json!({"ok": false, "error": why, "message": why})
        }
    }
}

async fn post(hub: &Hub, who: &RunIdentity, args: &Value) -> Result<String> {
    let agent = who.agent.as_str();
    let (chat, thread) = source_chat(hub, who)?;
    let text = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let (Some(kind), Some(title)) = (text("kind"), text("title")) else { bail!("a post needs a kind and a title") };
    let line = text("line").unwrap_or_default();
    let given = ["file", "url", "number"].iter().filter(|k| !args[**k].is_null()).count();
    if given > 1 {
        bail!("give at most one of file, url and number");
    }
    let pr = text("pr");
    let mut w = PostWrite {
        fold: Fold::Tool { fold: text("fold").map(str::to_owned) },
        agent: agent.to_owned(),
        kind: kind.to_owned(),
        title: title.to_owned(),
        line: line.to_owned(),
        attachment: None,
        source: PostSource { chat: chat.id.clone(), thread, entry: None },
        by: By::Agent,
        fact_at: now_ms(),
        fact_id: uuid::Uuid::new_v4().simple().to_string(),
    };
    if kind == "merges" || pr.is_some() {
        let (true, Some(pr)) = (kind == "merges", pr) else {
            bail!("pr goes with kind merges, and kind merges needs a pr")
        };
        if !pr_ok(pr) {
            bail!("pr is owner/repo#number");
        }
        // The fold ignores `fold` and the attachment: the day's card of the merging agent takes the PR into its set.
        w.fold = Fold::Merges { pr: pr.to_owned(), title: title.to_owned() };
        w.fact_id = pr.to_owned();
    } else if let Some(n) = args["number"].as_object() {
        let value = n.get("value").and_then(Value::as_f64).filter(|v| v.is_finite());
        let (Some(value), delta) = (value, n.get("delta").and_then(Value::as_f64)) else {
            bail!("number needs a value")
        };
        let mut a = json!({"type": "number", "value": value});
        if let Some(unit) = n.get("unit").and_then(Value::as_str).filter(|u| !u.is_empty()) {
            a["unit"] = unit.into();
        }
        if let Some(d) = delta.filter(|d| d.is_finite()) {
            a["delta"] = d.into();
        }
        w.attachment = Some(a);
    } else if !args["url"].is_null() {
        let url = text("url").filter(|u| u.starts_with("https://")).context("url must be https")?;
        w.attachment = Some(json!({"type": "link", "url": url}));
    }
    // A write under a key that has a card replaces it, so it is no new card: only new ones count against the day.
    // Checked before the file is copied. A call racing another one may pass it together; one card over is harmless.
    let key = fold_key_for(&w, text("file"));
    if !hub.store.post_key_exists(&key)? && hub.store.agent_posts_since(agent, day_start(now_ms()))? >= DAILY_CAP {
        bail!(LIMIT_ANSWER);
    }
    let mut snapshot = None;
    if let Some(file) = text("file") {
        let (copy, attachment, kind) = share_file(&chat, agent, file).await?;
        if kind == "page" {
            w.fold = Fold::Page { name: copy.meta.name.clone(), version: copy.meta.sha256.clone() };
        }
        w.attachment = Some(attachment);
        w.kind = kind.into();
        snapshot = Some(copy);
    }
    let put = put_post(hub, &w)?;
    // Only a card that was written keeps its copy: a repeat or an older file leaves nothing behind.
    if let (Some(_), Some(copy)) = (&put, snapshot.as_mut()) {
        copy.commit();
    }
    Ok(match put {
        Some(p) => format!("Posted to the Feed: {} ({})", p.title, p.id),
        None => "Already on the Feed: nothing changed.".to_owned(),
    })
}

/// The chat a run's post points back at, and its thread. A run of an app chat (`run:o:acp:<chat>:<thread>`) is that
/// chat; any other (a terminal, `kith ask`, a schedule) is the agent's owner chat. Never the chat of an invited
/// person: the owner never sees one, so no card may quote it (I2).
fn source_chat(hub: &Hub, who: &RunIdentity) -> Result<(BotConfig, Option<String>)> {
    let acp = who.session.strip_prefix("run:o:acp:").and_then(|s| s.split_once(':'));
    let (cfg, thread) = match acp {
        Some((chat, thread)) => {
            let row = hub.store.bot(chat)?.filter(|r| !r.deleted).context("that chat is gone")?;
            (row.config, Some(thread.to_owned()).filter(|t| !t.is_empty()))
        }
        None => (crate::cards::chat_for(hub, "", &who.agent), None),
    };
    if cfg.person.is_some() {
        bail!("the Feed is the owner's: nothing is posted from a chat of an invited person");
    }
    if hub.store.bot(&cfg.id)?.is_none_or(|r| r.deleted) {
        bail!("{} has no chat with the owner to post from", who.agent);
    }
    Ok((cfg, thread))
}

fn pr_ok(pr: &str) -> bool {
    let Some((repo, n)) = pr.split_once('#') else { return false };
    let part =
        |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'));
    repo.split_once('/').is_some_and(|(o, r)| part(o) && part(r)) && n.parse::<u32>().is_ok_and(|n| n > 0)
}

/// The fold key this write will have, for the cap: a file's page card is keyed by the file's name.
fn fold_key_for(w: &PostWrite, file: Option<&str>) -> String {
    match file.and_then(|f| Path::new(f).file_name()).and_then(|n| n.to_str()) {
        Some(name) if is_html(name) => format!("page:{}:{name}", w.agent),
        _ => fold::key(w),
    }
}

/// Midnight of the Mac's local day, in ms.
fn day_start(ms: i64) -> i64 {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .and_then(|t| t.date_naive().and_hms_opt(0, 0, 0))
        .and_then(|d| d.and_local_timezone(Local).earliest())
        .map_or(ms - 24 * 3600 * 1000, |t| t.timestamp_millis())
}

fn is_html(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".html") || n.ends_with(".htm")
}

/// Copies `file` into the chat's files root as `send_file` does (same snapshot, same 100 MiB cap, the same list of
/// places no run may read). Returns the copy, the attachment and the kind the file makes.
async fn share_file(chat: &BotConfig, agent: &str, file: &str) -> Result<(Snapshot, Value, &'static str)> {
    if !Path::new(file).is_absolute() {
        bail!("file is an absolute path");
    }
    let root = crate::chat::files::root(&chat.id);
    let folders = reach_folders(chat, agent);
    let file = file.to_owned();
    let copy = tokio::task::spawn_blocking(move || {
        let reach = crate::chat::files::Reach::new(folders, crate::cards::resource::roots());
        crate::chat::files::snapshot(&root, Path::new("/"), &file, None, Some(&reach), &AtomicBool::new(false))
    })
    .await??;
    let name = copy.meta.name.to_ascii_lowercase();
    let meta = json!(copy.meta);
    let (attachment, kind) = if is_html(&name) {
        (json!({"type": "page", "file": meta, "shot": null, "version": copy.meta.sha256}), "page")
    } else if name.ends_with(".mp4") || name.ends_with(".mov") {
        (json!({"type": "video", "file": meta}), "video")
    } else {
        (json!({"type": "file", "file": meta}), "file")
    };
    Ok((copy, attachment, kind))
}

/// Where a post's file may come from: the chat's folder and the agent's own, under the same secrets list as `send_file`.
fn reach_folders(chat: &BotConfig, agent: &str) -> Vec<PathBuf> {
    let mut folders = vec![PathBuf::from(&chat.cwd)];
    if !agent.contains(['/', '\\']) && agent != ".." {
        let home = dirs::home_dir().unwrap_or_default();
        folders.push(crate::kernel::folders(&home).agents.join(agent));
    }
    folders
}

#[cfg(test)]
mod tests;
