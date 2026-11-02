//! Bot memory ([design](../../../../docs/design/bot-memory.md)): it records the person, not the work. The rules
//! (store, checks, opening, tidying) live in the `kith_mem` crate; this module runs them in the host: the
//! session opening, the bot's memory tools, the client API, the chat lines that show and undo a bot's writes,
//! tidying, and the one-time move from the old Engram memory.

mod api;
mod dates;
mod file_api;
mod file_notices;
mod file_tools;
pub mod files;
mod hq_api;
mod hq_tidy;
mod import;
mod notices;
mod tidy;
mod tools;

pub use api::call;
pub use dates::ymd;
pub use hq_api::{METHODS as HQ_METHODS, call as hq_call};
pub use import::import_old_memory;
pub use tidy::{TidyEvent, spawn_tidier};
pub use tools::{INSTRUCTIONS, call as tool_call, tools};

use crate::hub::Hub;
use crate::store::{BotConfig, Store};

/// Whether a bot uses the memory all bots share. Unless the user chose, a bot in its own workspace does and one
/// working in a project folder doesn't.
pub fn shared(cfg: &BotConfig) -> bool {
    cfg.shared_memory.unwrap_or_else(|| crate::agent::workspace::is_managed(cfg))
}

/// Who the memory is loaded for: the owner, in the group room `room` when the turn is a group's. Only that room's notes
/// and the ones that belong to no room load (docs/memory-context.md, "Load rule at thread start").
fn scope(cfg: &BotConfig, room: Option<&str>) -> kith_mem::opening::Scope {
    let mut scope = room.map_or_else(kith_mem::opening::Scope::owner, kith_mem::opening::Scope::owner_group);
    scope.projects = cfg.project.iter().cloned().collect();
    scope
}

/// The memory section of a bot's instructions: its entries, then the index of its long-form files.
pub fn opening(store: &Store, cfg: &BotConfig, room: Option<&str>) -> String {
    let entries = match store.memory_read(|c| kith_mem::opening::load(c, &cfg.id, shared(cfg), &scope(cfg, room))) {
        Ok(opening) => opening.render(),
        Err(error) => {
            tracing::warn!(bot = %cfg.id, error = format!("{error:#}"), "couldn't read memory");
            String::new()
        }
    };
    format!("{entries}\n\n{}", files::index(store, cfg))
}

/// After memory changed in `bot`'s own layer, or in the shared one (`None`): the apps fetch it again, and every
/// session of a bot that sees the layer hears it on its next message (see [`change_notice`]). The opening itself
/// stays as it is, so the prompt cache does too.
pub fn changed(hub: &Hub, bot: Option<&str>) {
    let bots: Vec<(String, bool)> = match bot {
        Some(bot) => vec![(bot.to_owned(), true)],
        None => hub
            .store
            .bots()
            .map(|rows| {
                rows.into_iter()
                    .filter(|r| !r.deleted && !r.config.is_group())
                    .map(|r| {
                        let sees = shared(&r.config);
                        (r.config.id, sees)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let revision = hub.store.memory_rev().to_string();
    for (id, sees) in &bots {
        if *sees && let Err(error) = hub.store.kv_set(&revision_key(id), &revision) {
            tracing::warn!(bot = %id, error = format!("{error:#}"), "couldn't flag the memory change");
        }
        hub.emit_bot(id);
    }
}

/// What a bot's memory screen shows changed outside the store (Claude Code's notes came or went): moves
/// `memoryRev` so the apps fetch it again, without telling the bot's sessions.
pub fn screen_changed(hub: &Hub, bot: &str) {
    if let Err(error) = hub.store.memory_write(|_, _, _| Ok(())) {
        tracing::warn!(bot, error = format!("{error:#}"), "couldn't move the memory revision");
    }
    hub.emit_bot(bot);
}

/// The memory revision a bot's layers are at.
fn revision_key(bot: &str) -> String {
    format!("memory.revision.{bot}")
}

/// The revision a session has heard: its opening, then each notice since.
fn announced_key(bot: &str, session: &str) -> String {
    format!("memory.announced.{bot}.{session}")
}

/// The revision a session's opening holds.
fn opening_key(bot: &str, session: &str) -> String {
    format!("memory.opening.{bot}.{session}")
}

/// When memory changed since this session last heard: the whole memory as it is now, to put before the next
/// message, and the revision to acknowledge once it was delivered.
pub fn change_notice(store: &Store, cfg: &BotConfig, session: &str, room: Option<&str>) -> Option<(String, String)> {
    let revision = current_revision(store, &cfg.id);
    if revision.is_empty() || store.kv_get(&announced_key(&cfg.id, session)).unwrap_or_default() == revision {
        return None;
    }
    let notice = format!(
        "<memory_update>\nYour memory changed. This is all of it now and replaces any earlier memory in this conversation.\n{}\n</memory_update>",
        opening(store, cfg, room)
    );
    Some((notice, revision))
}

/// The session heard memory up to `revision`, from a notice.
pub fn mark_announced(store: &Store, bot: &str, session: &str, revision: &str) -> anyhow::Result<()> {
    store.kv_set(&announced_key(bot, session), revision)
}

/// The session's opening now holds memory at `revision` (a new session, or instructions it got again).
pub fn opened(store: &Store, bot: &str, session: &str, revision: &str) -> anyhow::Result<()> {
    store.kv_set(&opening_key(bot, session), revision)?;
    mark_announced(store, bot, session, revision)
}

/// The agent summarized its context, so the notices it got since its opening may be gone: it is left knowing what
/// its opening holds, and hears anything newer again.
pub fn compacted(store: &Store, bot: &str, session: &str) -> anyhow::Result<()> {
    match store.kv_get(&opening_key(bot, session)) {
        Some(opening) => mark_announced(store, bot, session, &opening),
        None => Ok(()),
    }
}

/// A bot wrote memory itself in `session`, which therefore knows the change: when it had heard everything up to
/// `before`, it has heard this too.
pub fn heard_own_write(store: &Store, bot: &str, session: &str, before: &str) -> anyhow::Result<()> {
    if store.kv_get(&announced_key(bot, session)).unwrap_or_default() == before {
        mark_announced(store, bot, session, &current_revision(store, bot))
    } else {
        Ok(())
    }
}

/// The memory revision a bot's layers are at; an opening rendered now holds it.
pub fn current_revision(store: &Store, bot: &str) -> String {
    store.kv_get(&revision_key(bot)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_hears_each_change_once_and_again_after_a_compaction() {
        let store =
            Store::open(&std::env::temp_dir().join(format!("kith-memory-{}.db", uuid::Uuid::new_v4()))).unwrap();
        let cfg: BotConfig = serde_json::from_value(serde_json::json!({
            "id": "5f0c7a52-0000-4000-8000-000000000002", "name": "Fixer", "backend": "claude", "cwd": "/tmp",
        }))
        .unwrap();
        let changes = |revision: &str| store.kv_set(&revision_key(&cfg.id), revision).unwrap();
        opened(&store, &cfg.id, "s1", "").unwrap();
        assert!(change_notice(&store, &cfg, "s1", None).is_none());

        changes("7");
        let (notice, revision) = change_notice(&store, &cfg, "s1", None).expect("a change");
        assert!(notice.starts_with("<memory_update>"), "{notice}");
        mark_announced(&store, &cfg.id, "s1", &revision).unwrap();
        assert!(change_notice(&store, &cfg, "s1", None).is_none(), "told once");
        compacted(&store, &cfg.id, "s1").unwrap();
        assert!(change_notice(&store, &cfg, "s1", None).is_some(), "the summary may have dropped the notice");

        // What the session wrote itself, with nothing else new, needs no notice; unless it missed something before.
        opened(&store, &cfg.id, "s1", "7").unwrap();
        changes("8");
        heard_own_write(&store, &cfg.id, "s1", "7").unwrap();
        assert!(change_notice(&store, &cfg, "s1", None).is_none());
        changes("9");
        changes("10");
        heard_own_write(&store, &cfg.id, "s1", "9").unwrap();
        assert!(change_notice(&store, &cfg, "s1", None).is_some());
    }
}
