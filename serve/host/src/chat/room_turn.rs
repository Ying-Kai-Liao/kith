//! What a turn in a group room fixes at its start (rooms with people, PR C; design 2026-11-12, rules 1, 3, 6, 13).
//!
//! The whole state of a room turn lives here and nowhere else, so the core can take it over later: its [`RoomBasis`]
//! (the people in the room, the room's gen, the newest join gen) and what follows from it, which are pure functions:
//!
//! - the **level**: with nobody in the room the turn is at owner level (today's turn, the owner's session); with
//!   people it is a room turn, run as the room person [`RoomBasis::person_id`] on an actor and session of its own;
//! - the **stamp**: every line the turn writes takes `gen`, the gen the turn STARTED in (never the gen at landing, or a
//!   person who joined meanwhile would be shown what was written before they came);
//! - the **input** ([`RoomBasis::may_read`]): a room turn reads only what every person in the room can see, an
//!   owner-level turn nothing that was written while any person was in the room (R2);
//! - the **session**: the room person's id carries the join gen, so each join starts a session and a workspace of its
//!   own (`files/people/room-<id>/`), and the older ones are never loaded again.
//!
//! A person sees an entry when their stay's `joined_gen <= entry.origin_gen` (`chat::room_post::sees`), so every one of
//! the basis sees an entry exactly when `floor <= origin_gen`, `floor` being the newest join among them. An entry
//! without a gen (`None`) was never stamped: hidden from a room turn, as from a person.

use super::group::membership_lock;
use super::room_post::seen_by;
use crate::store::{Stay, Store};

/// How a turn runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Nobody was in the room at the turn's start: today's turn on the owner's actor and session.
    Owner,
    /// People were in the room: the turn runs as the room person, with the grant they share.
    Room,
}

/// The room as a turn found it at its start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomBasis {
    pub room: String,
    /// The people with an open stay, sorted. Empty is owner level.
    pub people: Vec<String>,
    /// The room's gen (`Store::room_gen`): what the turn's lines are stamped with.
    pub room_gen: i64,
    /// The newest join of any stay, open or closed: the room person's session and workspace belong to it.
    pub join_gen: i64,
    /// The newest join among `people`: a line is for all of them when it is stamped with at least this.
    pub floor: i64,
}

impl RoomBasis {
    /// The room now, its people and gen read together under the membership lock (no join or leave lands between).
    pub fn read(store: &Store, room: &str) -> Self {
        let _members = membership_lock();
        let people = store.room_open_people(room);
        let stays = store.room_stays(room);
        Self::of(room, people, store.room_gen(room), &stays)
    }

    /// The basis of `room` with `people` open, from its stays and gen.
    pub fn of(room: &str, people: Vec<String>, room_gen: i64, stays: &[Stay]) -> Self {
        let open = |s: &&Stay| s.left_gen.is_none() && people.contains(&s.person);
        let floor = stays.iter().filter(open).map(|s| s.joined_gen).max().unwrap_or(0);
        let join_gen = stays.iter().map(|s| s.joined_gen).max().unwrap_or(0);
        Self { room: room.to_owned(), people, room_gen, join_gen, floor }
    }

    pub fn level(&self) -> Level {
        if self.people.is_empty() { Level::Owner } else { Level::Room }
    }

    /// The id the turn runs as: one per room and join gen. `None` at owner level. Never a person's id: kith-auth
    /// refuses any `room-` id, and the launch builds this one's grant from the basis alone.
    pub fn person_id(&self) -> Option<String> {
        (self.level() == Level::Room).then(|| room_person_id(&self.room, self.join_gen))
    }

    /// Whether a turn with this basis may read a line stamped `origin` (`stays`: the room's, from `room_stays`).
    pub fn may_read(&self, stays: &[Stay], kind: &str, origin: Option<i64>, seq: i64) -> bool {
        match self.level() {
            Level::Room => seen_by(self.floor, kind, origin),
            // A line that was never stamped is judged by where it stands in the room's order: the gen and the seq are
            // one space (a gen is the seq of its join or leave line).
            Level::Owner => !people_present_at(stays, origin.unwrap_or(seq)),
        }
    }
}

/// Whether any person had an open stay at room gen `g`: a stay holds from its join to just before its leave.
pub fn people_present_at(stays: &[Stay], g: i64) -> bool {
    stays.iter().any(|s| s.joined_gen <= g && s.left_gen.is_none_or(|left| left > g))
}

/// The id a room's people run as for one join gen: `room-<room>-<join_gen>`, a valid person id (lowercase letters,
/// digits, dashes).
pub fn room_person_id(room: &str, join_gen: i64) -> String {
    let slug: String = room
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .take(32)
        .collect();
    format!("room-{slug}-{join_gen}")
}

/// Whether `person` is a room person's id (the host's, never a human's).
pub fn is_room_person(person: &str) -> bool {
    person.starts_with("room-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stay(person: &str, joined: i64, left: Option<i64>) -> Stay {
        Stay { person: person.to_owned(), joined_gen: joined, left_gen: left }
    }

    #[test]
    fn the_floor_is_the_newest_join_among_the_people_in_and_the_join_gen_the_newest_of_all() {
        let stays = [stay("ann", 5, None), stay("ben", 8, Some(12)), stay("cy", 14, None)];
        let b = RoomBasis::of("r", vec!["ann".into(), "cy".into()], 14, &stays);
        assert_eq!((b.floor, b.join_gen, b.level()), (14, 14, Level::Room));
        let after_ben = RoomBasis::of("r", vec!["ann".into()], 12, &stays[..2]);
        assert_eq!((after_ben.floor, after_ben.join_gen), (5, 8), "a leave keeps the join_gen: no new session");
    }

    #[test]
    fn a_room_turn_reads_what_every_person_may_see_and_never_what_is_unstamped_or_not_a_message() {
        let stays = [stay("ann", 5, None), stay("ben", 8, None)];
        let b = RoomBasis::of("r", vec!["ann".into(), "ben".into()], 8, &stays);
        assert!(!b.may_read(&stays, "user", Some(7), 7), "written while ben was not there yet");
        assert!(b.may_read(&stays, "user", Some(8), 9));
        assert!(b.may_read(&stays, "agent", Some(9), 9));
        assert!(!b.may_read(&stays, "user", None, 99), "unstamped: never, whatever its seq");
        assert!(!b.may_read(&stays, "notice", Some(9), 9), "a Kai-only line is not what the people read");
        assert!(!b.may_read(&stays, "permission", Some(9), 9));
    }

    #[test]
    fn an_owner_turn_reads_nothing_written_while_any_person_was_in() {
        // ann in from 5 to 9: lines stamped 5..8 were written with her there; the leave line is 9.
        let stays = [stay("ann", 5, Some(9))];
        let b = RoomBasis::of("r", vec![], 9, &stays);
        assert!(b.may_read(&stays, "user", Some(4), 4), "before she came");
        assert!(!b.may_read(&stays, "user", Some(5), 6), "her join");
        assert!(!b.may_read(&stays, "agent", Some(8), 8));
        assert!(b.may_read(&stays, "user", Some(9), 10), "after the last leave");
        assert!(b.may_read(&stays, "user", None, 3), "an old line from before she came");
        assert!(!b.may_read(&stays, "user", None, 7), "an unstamped line written while she was in");
        assert!(b.may_read(&stays, "user", None, 12), "an unstamped line after she left");
    }

    #[test]
    fn the_room_persons_id_is_a_valid_person_id_per_room_and_join_gen() {
        let id = room_person_id("3F2A-bot.1", 12);
        assert_eq!(id, "room-3f2a-bot-1-12");
        assert!(is_room_person(&id));
        assert_ne!(room_person_id("a", 1), room_person_id("a", 2));
        assert_ne!(room_person_id("a", 1), room_person_id("b", 1));
        assert!(room_person_id(&"x".repeat(80), 123_456_789).len() <= 64);
    }
}
