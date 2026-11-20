//! Tidying in the host: when a bot's run starts, what it reads, and running its model on the bot's own harness.
//! What a run may change is the crate's decision (`kith_mem::tidy`).

use crate::LockExt;
use crate::chat::naming;
use crate::hub::Hub;
use crate::store::{BotConfig, Entry, EntryKind, Store};
use anyhow::Result;
use kith_mem::tidy::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Quiet time after the last turn before a run. A test host shortens it with `KITH_SERVE_TIDY_IDLE_MS`.
fn quiet_time() -> Duration {
    static IDLE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *IDLE.get_or_init(|| {
        std::env::var("KITH_SERVE_TIDY_IDLE_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(Duration::from_secs(5 * 60), Duration::from_millis)
    })
}
/// User messages that start a run without waiting, and the most one run reads.
const BATCH: usize = 8;
/// Entries (the user's messages and final replies) one run looks at.
const READ_LIMIT: i64 = 40;
const MESSAGE_CHARS: usize = 4_000;
/// Usage of the bot's plan above which runs wait, so tidying never spends the user's last headroom.
const HEADROOM_PERCENT: f64 = 75.0;
/// How often an idle bot checks whether its daily time review is due.
const REVIEW_CHECK: Duration = Duration::from_secs(60 * 60);

pub enum TidyEvent {
    /// A turn the user started has finished. Its words fed the automatic naming, which is gone; it only counts now.
    #[allow(dead_code)]
    Turn(naming::Turn),
    /// Run soon: a new session started or the context was compacted.
    Flush,
}

enum Ran {
    /// Read up to here; more may wait.
    Done { more: bool },
    /// Try again later without counting a failure: usage is high, or memory changed during the run.
    Later,
}

fn bookmark_key(bot: &str) -> String {
    format!("memory.tidy.{bot}")
}

fn reviewed_key(bot: &str) -> String {
    format!("memory.tidy.reviewed.{bot}")
}

// Keys written before the rename live under `memory.dream.`; they are read when the new key isn't there yet, and
// the next write moves them to the new one.
fn kv_get_or_old(store: &Store, key: &str) -> Option<String> {
    store.kv_get(key).or_else(|| store.kv_get(&key.replacen("memory.tidy.", "memory.dream.", 1)))
}

/// Tidying starts reading after `seq`.
fn set_bookmark(store: &Store, bot: &str, seq: i64) -> Result<()> {
    store.kv_set(&bookmark_key(bot), &seq.to_string())
}

fn bookmark(store: &Store, bot: &str) -> i64 {
    kv_get_or_old(store, &bookmark_key(bot)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Starts the bot's tidier. It runs once the bot has been quiet for [`quiet_time`] after the user's turns, or right
/// away when [`BATCH`] of them are waiting, and once a day for the time review. It stops when the bot actor drops
/// the sender; what it hasn't read waits for the next one.
pub fn spawn_tidier(hub: Arc<Hub>, bot_id: String) -> mpsc::UnboundedSender<TidyEvent> {
    let (tx, mut rx) = mpsc::unbounded_channel::<TidyEvent>();
    // Only an hq agent's memory is tidied by the host: a host-native row (C5) or a room has none, so events sent to
    // it are dropped.
    if !hub.store.bot(&bot_id).ok().flatten().is_some_and(|b| b.config.is_hq() && !b.config.is_native()) {
        return tx;
    }
    if kv_get_or_old(&hub.store, &bookmark_key(&bot_id)).is_none() {
        // An hq bot's months of earlier chat were never tidied over: the first run starts from now, not from them.
        if let Err(e) = set_bookmark(&hub.store, &bot_id, hub.store.last_seq(&bot_id)) {
            tracing::warn!(bot = %bot_id, error = format!("{e:#}"), "couldn't start the tidy-up bookmark");
        }
    }
    tokio::spawn(async move {
        let mut waiting = waiting(&hub.store, &bot_id);
        let idle = tokio::time::sleep(quiet_time());
        tokio::pin!(idle);
        let mut failures = 0u32;
        let mut retry_at: Option<Instant> = None;
        let quiet_until = |retry_at: Option<Instant>| {
            let after = Instant::now() + quiet_time();
            retry_at.map_or(after, |at| at.max(after))
        };
        loop {
            let due = waiting > 0 || review_due_blocking(&hub, &bot_id).await;
            let run = tokio::select! {
                event = rx.recv() => match event {
                    None => break,
                    Some(TidyEvent::Turn(_)) => {
                        waiting += 1;
                        idle.as_mut().reset(quiet_until(retry_at));
                        waiting >= BATCH && retry_at.is_none()
                    }
                    Some(TidyEvent::Flush) => {
                        idle.as_mut().reset(retry_at.unwrap_or_else(Instant::now));
                        false
                    }
                },
                () = &mut idle, if due => true,
                () = tokio::time::sleep(REVIEW_CHECK), if !due => false,
            };
            if !run {
                continue;
            }
            let next = match self::run(&hub, &bot_id).await {
                Ok(Ran::Done { more }) => {
                    failures = 0;
                    retry_at = None;
                    waiting = if more { waiting.max(1) } else { self::waiting(&hub.store, &bot_id) };
                    if more { Instant::now() } else { quiet_until(None) }
                }
                Ok(Ran::Later) => quiet_until(retry_at),
                Err(e) => {
                    failures += 1;
                    retry_at = Some(Instant::now() + quiet_time() * 2u32.pow(failures.min(5)));
                    tracing::warn!(bot = %bot_id, failures, error = format!("{e:#}"), "tidying failed; it retries later");
                    quiet_until(retry_at)
                }
            };
            idle.as_mut().reset(next);
        }
    });
    tx
}

/// The user's messages since the bookmark.
fn waiting(store: &Store, bot: &str) -> usize {
    store
        .owner_conversation_since(bot, bookmark(store, bot), READ_LIMIT)
        .map_or(0, |entries| entries.iter().filter(|e| e.kind == EntryKind::User.as_str()).count())
}

/// [`review_due`] off the async workers: an hq agent's answer reads files.
async fn review_due_blocking(hub: &Arc<Hub>, bot: &str) -> bool {
    let (hub, bot) = (hub.clone(), bot.to_owned());
    tokio::task::spawn_blocking(move || review_due(&hub.store, &bot)).await.unwrap_or(false)
}

/// Once a day, when the bot's memory holds a dated entry it may rewrite (the time review).
fn review_due(store: &Store, bot: &str) -> bool {
    if kv_get_or_old(store, &reviewed_key(bot)).as_deref() == Some(today().as_str()) {
        return false;
    }
    store
        .bot(bot)
        .ok()
        .flatten()
        .and_then(|b| b.config.hq_agent)
        .is_some_and(|agent| super::hq_tidy::review_due(&agent))
}

/// Whether a memory names a date the time review may have to turn into the past: a year, a month or day in
/// Chinese, or an English month next to a number. Other numbers (prices, counts) don't count.
pub(super) fn dated(text: &str) -> bool {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let chars: Vec<char> = text.chars().collect();
    let year =
        chars.windows(4).any(|w| matches!(w[..2], ['1', '9'] | ['2', '0']) && w[2..].iter().all(char::is_ascii_digit));
    let cjk = chars.windows(2).any(|w| w[0].is_ascii_digit() && "年月日號号".contains(w[1]));
    let lower = text.to_lowercase();
    let english = MONTHS.iter().any(|m| lower.contains(m)) && chars.iter().any(char::is_ascii_digit);
    year || cjk || english
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

async fn run(hub: &Arc<Hub>, bot_id: &str) -> Result<Ran> {
    let Some(cfg) = hub.store.bot(bot_id)?.filter(|b| !b.deleted).map(|b| b.config) else {
        return Ok(Ran::Done { more: false });
    };
    if near_limit(hub, &cfg) {
        return Ok(Ran::Later);
    }
    let after = bookmark(&hub.store, bot_id);
    if cfg.person.is_some() {
        // An invited person's chat isn't the owner's: nothing in it feeds the owner's memory. Skip past it.
        set_bookmark(&hub.store, bot_id, hub.store.last_seq(bot_id))?;
        return Ok(Ran::Done { more: false });
    }
    let entries = hub.store.owner_conversation_since(bot_id, after, READ_LIMIT)?;
    let read = within_batch(&entries);
    let more = read.len() < entries.len() || i64::try_from(entries.len()).is_ok_and(|n| n == READ_LIMIT);
    let last_seq = read.last().map_or(after, |e| e.seq);
    let messages: Vec<Message> = read.iter().filter_map(message).collect();
    let today = today();
    let review = review_due_blocking(hub, bot_id).await;
    let worth = messages.iter().any(|m| m.from_user && m.text.iter().any(|run| kith_mem::evidence::substantive(run)));
    if !worth && !review {
        set_bookmark(&hub.store, bot_id, last_seq)?;
        return Ok(Ran::Done { more });
    }

    if super::hq_tidy::tidy(hub, &cfg, &messages, &today).await? {
        return Ok(Ran::Later);
    }
    set_bookmark(&hub.store, bot_id, last_seq)?;
    if review {
        hub.store.kv_set(&reviewed_key(bot_id), &today)?;
    }
    Ok(Ran::Done { more })
}

/// Usage of the bot's harness above [`HEADROOM_PERCENT`] in any window.
fn near_limit(hub: &Hub, cfg: &BotConfig) -> bool {
    hub.usage
        .locked()
        .providers
        .iter()
        .filter(|p| p.id == cfg.backend)
        .flat_map(|p| &p.windows)
        .any(|w| w.percent > HEADROOM_PERCENT)
}

/// The entries up to the [`BATCH`]th user message and the replies after it.
fn within_batch(entries: &[Entry]) -> &[Entry] {
    let mut users = 0;
    for (i, e) in entries.iter().enumerate() {
        if e.kind == EntryKind::User.as_str() {
            if users == BATCH {
                return &entries[..i];
            }
            users += 1;
        }
    }
    entries
}

/// What a run sees of an entry. A user message counts only for what the user typed themselves; one the host wrote
/// in the user's place (`from: host`) isn't the user's words.
fn message(e: &Entry) -> Option<Message> {
    let clip = |text: &str| crate::agent::acp::truncate(text, MESSAGE_CHARS);
    let (from_user, text) = if e.kind == EntryKind::User.as_str() {
        if e.data["from"] == "host" {
            return None;
        }
        (true, typed(&e.data).iter().map(|run| clip(run)).collect::<Vec<_>>())
    } else {
        (false, vec![clip(e.data["text"].as_str().unwrap_or_default())])
    };
    let text: Vec<String> = text.into_iter().filter(|run| !run.trim().is_empty()).collect();
    (!text.is_empty()).then(|| Message { id: e.id.clone(), from_user, text, date: super::ymd(e.created_at) })
}

/// The runs of a user message the user typed themselves: not pasted, quoted or forwarded text.
fn typed(data: &serde_json::Value) -> Vec<String> {
    let pasted: Vec<String> =
        data["pasted"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_owned)).collect();
    kith_mem::evidence::typed(data["text"].as_str().unwrap_or_default(), &pasted)
}

#[cfg(test)]
mod tests {
    use super::{dated, message, waiting};
    use crate::store::Store;
    use crate::store::{EntryKind, Lane};
    use serde_json::json;

    #[test]
    fn tidying_reads_only_the_owners_turns() {
        let dir = std::env::temp_dir().join(format!("kith-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir.join("t.db")).unwrap();
        let main = Lane::main("b1");
        s.insert_entry(&main, EntryKind::User, 1, &json!({"text": "owner"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 1, &json!({"text": "ok", "final": true})).unwrap();
        s.insert_entry(&main, EntryKind::User, 2, &json!({"text": "sam says", "person": "sam"})).unwrap();
        s.insert_entry(&main, EntryKind::Agent, 2, &json!({"text": "reply to sam", "final": true})).unwrap();
        s.insert_entry(&main, EntryKind::User, 3, &json!({"text": "guest", "sender": {"botId": "g"}})).unwrap();
        assert_eq!(waiting(&s, "b1"), 1);
        let seen: Vec<String> =
            s.owner_conversation_since("b1", 0, 50).unwrap().iter().filter_map(message).flat_map(|m| m.text).collect();
        assert_eq!(seen, ["owner", "ok"]);
    }

    #[test]
    fn only_dates_trigger_the_time_review() {
        assert!(dated("Going to Singapore in July 2026"));
        assert!(dated("下個月 3號 要搬家"));
        assert!(dated("Dentist on Oct 21"));
        assert!(!dated("Prompt is too long: Provided 10,389 tokens"));
        assert!(!dated("Has 2 cats"));
    }
}
