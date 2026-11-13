//! Threads of terminal sessions: the kernel resolves a thread to its Claude session (`kith <agent> --resume`), and
//! the apps take a thread over from the terminal that holds it. Owner only (not on the member list).

use super::str_arg;
use crate::agent::project;
use crate::hub::Hub;
use crate::store::{EntryKind, Lane};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        // `{botId, threadId, agent, sessionId, cwd, hostBusy}` for a thread id or a session id.
        "resolveThread" => crate::terminal::resolve(hub, str_arg(&b, "thread")?).await,
        // Ends the terminal that holds the thread; refused while it works, a no-op once it is closed.
        "takeOverThread" => crate::terminal::take_over(hub, str_arg(&b, "botId")?, str_arg(&b, "threadId")?).await?,
        // `{projects, by, pinned}`: the project set the thread runs under now (by `inferred` or `person`; empty when it
        // has none or was never looked at). `pinned`: the bot's settings fix it, and neither inference nor a correction moves it.
        "threadProjects" => {
            let (lane, pinned) = place(hub, &b)?;
            match (pinned, project::read(&hub.store, &lane)) {
                (Some(pin), _) => json!({"projects": [pin], "by": "pinned", "pinned": true}),
                (None, Some(s)) => json!({"projects": s.projects, "by": s.by, "pinned": false}),
                (None, None) => json!({"projects": [], "by": "inferred", "pinned": false}),
            }
        }
        // `{botId, threadId?, projects: [id…]}`: the person's correction of the thread's project set (an empty list
        // is "no project"). It holds until the subject clearly moves again or they change it. A change writes the
        // divider notice with `projectChange.by: "person"`. Owner, or the person in their own 1:1 (see `roles::guard`).
        "setThreadProjects" => {
            let (lane, pinned) = place(hub, &b)?;
            if pinned.is_some() {
                bail!("This chat's project is set in its settings");
            }
            let mut projects: Vec<String> = Vec::new();
            for id in b["projects"].as_array().ok_or_else(|| anyhow::anyhow!("`projects` is required"))? {
                let id = id.as_str().ok_or_else(|| anyhow::anyhow!("`projects` are ids"))?;
                if !projects.iter().any(|p| p == id) {
                    projects.push(id.to_owned());
                }
            }
            let known = crate::agent::hq::projects_soon().await;
            if let Some(bad) = projects.iter().find(|p| !known.iter().any(|k| &k.id == *p)) {
                bail!("unknown project '{bad}'");
            }
            let (state, change) = project::correct(&hub.store, &lane, projects);
            if let Some(change) = &change {
                let data = json!({
                    "text": project::divider_text(&change.to),
                    "style": "divider",
                    "projectChange": project::change_data(&change.from, &change.to, "person"),
                });
                hub.add_entry(&lane, EntryKind::Notice, hub.store.max_turn(&lane.chat), &data);
            }
            json!({"projects": state.projects, "by": state.by, "changed": change.is_some()})
        }
        // Writes the thread's summary now (`kith summarise --thread`): `{written: bool}`.
        "summariseThread" => {
            let written = matches!(
                crate::hqmem::summary::summarise_now(
                    hub,
                    str_arg(&b, "thread")?,
                    b["force"].as_bool().unwrap_or(false)
                )
                .await?,
                crate::hqmem::summary::Wrote::Yes
            );
            serde_json::json!({"written": written})
        }
        // The owner forgets a thread (`kith forget --thread`): its summary and its row's copy go.
        "forgetThread" => {
            let thread = str_arg(&b, "thread")?;
            crate::hqmem::summary::forget_now(hub, thread)?;
            serde_json::json!({"forgotten": thread})
        }
        // A citation link opened by the owner or, as `asPerson` (set by the host, never the client), an invitee:
        // `{thread, seq, at, entries}`, or `{error: "not_shared"}`.
        "resolveCitation" => resolve_citation(hub, b["asPerson"].as_str(), str_arg(&b, "citation")?).await,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

/// The chat and thread a call names, and the project its bot is pinned to. A group is refused: its lanes carry more
/// than one person's words, so they have no inferred project and nobody's correction to keep.
fn place(hub: &Hub, b: &Value) -> Result<(Lane, Option<String>)> {
    let bot = str_arg(b, "botId")?;
    let row = hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow::anyhow!("unknown bot"))?;
    if row.config.is_group() {
        bail!("A group's project isn't set from here");
    }
    let lane = match b["threadId"].as_str().filter(|t| !t.is_empty()) {
        Some(root) => {
            if hub.store.entry(root).is_none_or(|r| r.bot_id != bot || r.thread_id.is_some()) {
                bail!("unknown message");
            }
            Lane::in_thread(bot, root)
        }
        None => Lane::main(bot),
    };
    Ok((lane, row.config.project))
}

/// What every refusal of [`resolve_citation`] says, whether the thread is someone else's, missing, forgotten or
/// the link malformed: nothing tells the caller which.
fn not_shared() -> Value {
    serde_json::json!({"error": "not_shared"})
}

/// `kith://thread/<id>#entry=<seq>` as (thread id, seq).
fn parse_citation(link: &str) -> Option<(&str, Option<i64>)> {
    let rest = link.trim().strip_prefix("kith://thread/")?;
    let (id, frag) = rest.split_once('#').unwrap_or((rest, ""));
    let seq = frag.strip_prefix("entry=").and_then(|s| s.parse().ok());
    (!id.is_empty()).then_some((id, seq))
}

/// Opens a trajectory citation for `person` (None: the owner). Only the people in the thread's audience get the
/// thread; anyone else, and every thread that doesn't exist, gets `{error: "not_shared"}`.
pub(super) async fn resolve_citation(hub: &Arc<Hub>, person: Option<&str>, link: &str) -> Value {
    use crate::hqmem::summary::{facets, thread_key};
    let Some((id, seq)) = parse_citation(link) else { return not_shared() };
    let (bot, root, root_entry) = match id.strip_prefix("main:") {
        Some(bot) => (bot.to_owned(), None, None),
        None => match hub.store.entry(id).filter(|e| e.thread_id.is_none()) {
            Some(e) => (e.bot_id.clone(), Some(id.to_owned()), Some(e)),
            None => return not_shared(),
        },
    };
    let Some(cfg) = hub.store.bot(&bot).ok().flatten().filter(|b| !b.deleted).map(|b| b.config) else {
        return not_shared();
    };
    let key = thread_key(&bot, root.as_deref());
    if hub.store.with_db(|c| kith_mem::summary::forgotten(c, &key).unwrap_or(true)) {
        return not_shared();
    }
    if !super::roles::may_see_summary(person, &facets(&cfg, root_entry.as_ref()).audience) {
        return not_shared();
    }
    // A member's chat must still be theirs and its agent still granted (the same check as their other chat calls).
    if let Some(p) = person
        && super::roles::own_bot(hub, p, &bot).await.is_err()
    {
        return not_shared();
    }
    let entries = match &root {
        Some(root) => hub.store.thread(&bot, root, 500),
        None => hub.store.history(&bot, seq.map_or(i64::MAX, |s| s.saturating_add(20)), 40),
    };
    let Ok(mut entries) = entries else { return not_shared() };
    crate::hqmem::summary::hide_forgotten(&hub.store, &mut entries);
    let at = seq.and_then(|s| entries.iter().find(|e| e.seq == s)).map(|e| e.id.clone());
    serde_json::json!({"thread": {"id": key, "botId": bot, "rootId": root}, "seq": seq, "at": at, "entries": entries})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_parse_with_or_without_an_entry() {
        assert_eq!(parse_citation("kith://thread/abc#entry=12"), Some(("abc", Some(12))));
        assert_eq!(parse_citation("kith://thread/main:b1#entry=3"), Some(("main:b1", Some(3))));
        assert_eq!(parse_citation("kith://thread/abc"), Some(("abc", None)));
        assert_eq!(parse_citation("kith://thread/"), None);
        assert_eq!(parse_citation("https://x/abc"), None);
    }
}
