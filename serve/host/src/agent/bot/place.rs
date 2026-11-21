//! The places a bot talks (its own chat, each thread, each group lane): the keys of their actors, and the turns a
//! previous host process cut off in them.

use crate::store::{Lane, Store, now_ms};

/// An interrupted turn older than this isn't resumed.
const STALE_RESUME_MS: i64 = 60 * 60 * 1000;

/// The runtime and kv name of the actor for `home`: the bot's id for its own chat, `bot|lane` for any other place.
pub fn actor_key(bot: &str, home: &Lane) -> String {
    if home.chat == bot && home.thread.is_none() { bot.to_owned() } else { format!("{bot}|{}", home.key()) }
}

/// [`actor_key`] of the room person's actor in `home`: `bot|lane#room-id`, one per room and join gen (rooms with people).
/// Without a person it is the owner's actor of that place.
pub fn actor_key_as(bot: &str, home: &Lane, person: Option<&str>) -> String {
    match person {
        Some(p) => format!("{}#{p}", actor_key(bot, home)),
        None => actor_key(bot, home),
    }
}

/// The room person an actor key runs as, if it is a room actor's.
pub fn person_of(key: &str) -> Option<&str> {
    key.split_once('|').and_then(|(_, place)| place.split_once('#')).map(|(_, p)| p)
}

/// The bot an actor key belongs to.
pub fn bot_of(key: &str) -> &str {
    key.split_once('|').map_or(key, |(bot, _)| bot)
}

/// The place an actor key names.
pub fn place_of(key: &str) -> Lane {
    let Some((_, place)) = key.split_once('|') else { return Lane::main(key) };
    let place = place.split_once('#').map_or(place, |(lane, _)| lane);
    match place.split_once('/') {
        Some((chat, root)) => Lane::in_thread(chat, root),
        None => Lane::main(place),
    }
}

/// kv key of the running-turn record of the actor `key`.
pub(super) fn inflight_key(key: &str) -> String {
    format!("turn.inflight.{key}")
}

/// Where the turn the previous host process never finished in `home` talked (crash, update, restart), unless
/// it's stale: an hour-old task is more likely unwanted than awaited (gawkbot's rule).
pub(super) fn interrupted(store: &Store, key: &str, home: &Lane) -> Option<Lane> {
    let v = store.kv_get(&inflight_key(key))?;
    let (started, thread) = v.split_once('\t').unwrap_or((&v, ""));
    let started: i64 = started.parse().ok()?;
    let thread = (!thread.is_empty()).then(|| thread.to_owned()).or_else(|| home.thread.clone());
    (now_ms() - started < STALE_RESUME_MS).then(|| Lane { chat: home.chat.clone(), thread })
}

/// A turn the previous host process cut off in `lane` must not resume once the place's folder changed: drops the
/// record of its own actor, and the bot's own chat's when that one was running a turn there (a routine in a thread).
pub fn forget_inflight(store: &Store, bot: &str, lane: &Lane) {
    let own = actor_key(bot, lane);
    let mut keys = vec![own.clone()];
    if own != bot
        && let Some(v) = store.kv_get(&inflight_key(bot))
        && v.split_once('\t').is_some_and(|(_, root)| lane.chat == bot && lane.thread.as_deref() == Some(root))
    {
        keys.push(bot.to_owned());
    }
    for key in keys {
        if let Err(error) = store.kv_set(&inflight_key(&key), "") {
            tracing::warn!(bot, error = format!("{error:#}"), "couldn't clear a running-turn record");
        }
    }
}

/// The places besides its own chat where the previous host process cut off one of the bot's turns. Records too old
/// to resume are cleared, so they don't start an actor again on every host start.
pub fn interrupted_places(store: &Store, bot: &str) -> Vec<Lane> {
    let all = inflight_key("");
    let mut places = vec![];
    for key in store.kv_prefix(&inflight_key(&format!("{bot}|"))) {
        let Some(actor) = key.strip_prefix(&all) else { continue };
        let home = place_of(actor);
        if interrupted(store, actor, &home).is_some() {
            places.push(home);
        } else if store.kv_get(&key).is_some_and(|v| !v.is_empty())
            && let Err(error) = store.kv_set(&key, "")
        {
            tracing::warn!(actor, error = format!("{error:#}"), "couldn't clear an old running-turn record");
        }
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_actor_key_names_its_bot_and_place() {
        for home in [Lane::main("b"), Lane::in_thread("b", "root"), Lane::main("group"), Lane::in_thread("group", "r")]
        {
            let key = actor_key("b", &home);
            assert_eq!(bot_of(&key), "b");
            assert_eq!(place_of(&key), home);
        }
        assert_eq!(actor_key("b", &Lane::main("b")), "b", "the bot's own chat is the bot");
    }

    #[test]
    fn a_room_persons_actor_has_a_key_of_its_own_in_the_same_place() {
        let home = Lane::main("room");
        let key = actor_key_as("b", &home, Some("room-room-9"));
        assert_ne!(key, actor_key("b", &home), "never the owner's actor");
        assert_eq!((bot_of(&key), place_of(&key), person_of(&key)), ("b", home.clone(), Some("room-room-9")));
        assert_eq!(actor_key_as("b", &home, None), actor_key("b", &home));
        assert_eq!(person_of(&actor_key("b", &home)), None);
    }

    #[test]
    fn only_recent_interrupted_places_resume_and_old_records_are_cleared() {
        let dir = std::env::temp_dir().join(format!("kith-place-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("db.sqlite")).unwrap();
        let recent = actor_key("b", &Lane::in_thread("b", "fresh"));
        let old = actor_key("b", &Lane::main("group"));
        store.kv_set(&inflight_key(&recent), &now_ms().to_string()).unwrap();
        store.kv_set(&inflight_key(&old), &(now_ms() - STALE_RESUME_MS - 1).to_string()).unwrap();
        store.kv_set(&inflight_key(&actor_key("b", &Lane::in_thread("b", "done"))), "").unwrap();

        assert_eq!(interrupted_places(&store, "b"), vec![Lane::in_thread("b", "fresh")]);
        assert_eq!(store.kv_get(&inflight_key(&old)).as_deref(), Some(""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
