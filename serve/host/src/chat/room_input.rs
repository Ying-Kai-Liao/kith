//! What a room turn is told (rooms with people, PR C, rule 3): the lines it may read, and the line the author gets back
//! when what they asked can no longer be answered. The decisions are `room_turn::RoomBasis::may_read`; this reads the
//! entries' gens from the store and applies them.

use super::room_turn::{Level, RoomBasis};
use crate::hub::Hub;
use crate::store::{BotConfig, Entry, EntryKind, Lane, Stay};
use serde_json::json;

/// The lines of `lines` a turn at `basis` may read, in their order. A room with no stay ever, at owner level, is
/// today's room: everything.
pub fn readable(hub: &Hub, basis: &RoomBasis, stays: &[Stay], lines: Vec<Entry>) -> Vec<Entry> {
    if basis.level() == Level::Owner && stays.is_empty() {
        return lines;
    }
    let ids: Vec<&str> = lines.iter().map(|e| e.id.as_str()).collect();
    let gens = hub.store.origin_gens(&ids);
    lines.into_iter().filter(|e| basis.may_read(stays, &e.kind, gens.get(&e.id).copied(), e.seq)).collect()
}

/// Whether the conversation can go on: `None` when every line of `trigger` (what this round answers) is readable under
/// this step's basis and its author is still in the room. Otherwise it ends, and `Some(text)` is the line its author
/// gets back (no `@` in it, so it wakes nobody), `Some(None)` when it ends without one (its author left).
///
/// This is what a join or the last leave does to a message nobody answered yet (design-flow, design-r2): a person who
/// came meanwhile is never answered with what they were not shown, and an owner-level turn after the last leave never
/// reads what a person wrote.
#[allow(clippy::option_option, reason = "no cut, a cut without a line and a cut with one are three answers")]
pub fn unreadable(
    hub: &Hub,
    basis: &RoomBasis,
    stays: &[Stay],
    trigger: &[Entry],
    author: &str,
) -> Option<Option<String>> {
    if basis.level() == Level::Room && author != "owner" && !basis.people.contains(&author.to_owned()) {
        return Some(None);
    }
    let cut = readable(hub, basis, stays, trigger.to_vec()).len() != trigger.len();
    if !cut {
        return None;
    }
    let name = |person: &str| hub.store.person(person).map_or_else(|| person.to_owned(), |p| p.name);
    let who = match basis.level() {
        // Someone came in after the asking: the newest join is the one.
        Level::Room => {
            stays.iter().filter(|s| s.left_gen.is_none()).max_by_key(|s| s.joined_gen).map(|s| name(&s.person))
        }
        // The room emptied: the last one out.
        Level::Owner => stays.iter().filter_map(|s| s.left_gen.map(|l| (l, &s.person))).max().map(|(_, p)| name(p)),
    };
    let text = match (basis.level(), who) {
        (Level::Room, Some(who)) => format!("{who} joined the room first. Ask again."),
        (Level::Owner, Some(who)) => format!("{who} left the room first. Ask again."),
        (Level::Room, None) => "Someone joined the room first. Ask again.".to_owned(),
        (Level::Owner, None) => "The room changed first. Ask again.".to_owned(),
    };
    Some(Some(text))
}

/// Says why nobody answered, as an agent's line stamped with the gen the asking came from (never the room's gen now, so a
/// person who came later is not told that someone asked something before they came: R1). Not tagged with a conversation
/// and without an `@`, so it wakes nobody.
pub fn unanswered_note(hub: &Hub, lane: &Lane, speaker: Option<&BotConfig>, trigger: &[Entry], text: &str) {
    let ids: Vec<&str> = trigger.iter().map(|e| e.id.as_str()).collect();
    let gens = hub.store.origin_gens(&ids);
    let Some(stamp) = gens.values().max().copied() else { return };
    let (author, name) = speaker.map_or((String::new(), "The room".to_owned()), |m| (m.id.clone(), m.name.clone()));
    let turn = hub.store.max_turn(&lane.chat);
    let data =
        json!({"text": format!("{name} didn't answer: {text}"), "author": author, "final": true, "unanswered": true});
    hub.add_entry_from(lane, EntryKind::Agent, turn, &data, Some(stamp));
}
