//! A group turn on its actor, under the room it started in (rooms with people, PR C; the decisions are in
//! `chat::room_turn`).
//!
//! The turn fixes its [`RoomBasis`] once, at its start ([`Actor::begin_room_turn`]): who is in the room, the gen, the
//! level. Everything after reads that one snapshot, never the room as it is by then: the lines it writes are stamped with
//! the gen it started in, the process it launches knows the people it started with (`run_members`), and a person who
//! joins meanwhile changes nothing of it. A turn at room level runs on the actor of the room person
//! (`hub::actors`), which holds no session, process or workspace of the owner's.

use super::{Actor, NoticeStyle};
use crate::chat::group::GroupTurn;
use crate::chat::room_turn::{Level, RoomBasis, is_room_person};
use crate::store::{BotConfig, Entry, EntryKind, Lane};
use serde_json::Value;

/// The owner of a place's session in the store: the bot, or, for a room person's actor, the bot and that person (so the
/// room's owner-level session and each join epoch's own are three different keys).
pub(crate) fn session_owner(cfg: &BotConfig) -> String {
    match cfg.person.as_deref().filter(|p| is_room_person(p)) {
        Some(person) => format!("{}#{person}", cfg.id),
        None => cfg.id.clone(),
    }
}

impl Actor {
    /// Whether this is a room person's actor (the owner's actors are never one, whatever their bot's config says).
    pub(super) fn is_room_actor(&self) -> bool {
        self.cfg.person.as_deref().is_some_and(is_room_person)
    }

    /// The gen a line written now by this turn is stamped with: the gen the turn started in. `None` outside a group turn.
    pub(super) fn turn_stamp(&self) -> Option<i64> {
        self.turn_basis.as_ref().map(|b| b.room_gen)
    }

    /// The room's snapshot for this turn, at room level: the list the process, its token and its recall see.
    pub(super) fn room_members(&self) -> Option<crate::agent::hq::Members> {
        let basis = self.turn_basis.as_ref().filter(|b| b.level() == Level::Room)?;
        let author = self.active_group.as_ref().map_or_else(|| "owner".to_owned(), |g| g.author.clone());
        Some(crate::agent::hq::Members::Room { people: basis.people.clone(), author })
    }

    /// Starts `turn` under the room as it is now, or says why not. Checked in this order:
    ///
    /// 1. the room's people are the ones the turn's prompt was composed for (a join, or a leave of the last, between
    ///    the prompt and now means the prompt was written for another room: the turn does not run), and this actor is
    ///    the one for that level;
    /// 2. a turn at room level is registered (`Hub::room_turn_begins`), and only then is every basis person asked live
    ///    whether they may still use this agent: a revoke writes no room row, so one that lands after the registration
    ///    finds the turn and stops it, and one before it is seen by the check;
    /// 3. the author is the owner or in the basis (a person who left asked nothing more).
    ///
    /// On `Ok` the basis is the turn's until [`Self::end_room_turn`].
    pub(super) async fn begin_room_turn(&mut self, turn: &GroupTurn) -> Result<(), &'static str> {
        let room = &turn.lane.chat;
        let now = RoomBasis::read(&self.hub.store, room);
        let composed = turn.basis.as_ref().map_or(&[][..], |b| &b.people[..]);
        if now.people != composed || self.is_room_actor() != (now.level() == Level::Room) {
            return Err("the room's people changed before the turn started");
        }
        if now.level() == Level::Room {
            if turn.author != "owner" && !now.people.contains(&turn.author) {
                return Err("the person who asked is not in the room any more");
            }
            self.hub.room_turn_begins(&self.rt, &now.people);
            let agent = self.cfg.hq_agent.clone().unwrap_or_default();
            for person in &now.people {
                if agent.is_empty() || !crate::api::roles::may_use(person, &agent).await {
                    self.hub.room_turn_ends(&self.rt);
                    return Err("somebody in the room may not use this agent any more");
                }
            }
        }
        self.turn_basis = Some(now);
        Ok(())
    }

    /// A card of this room turn (rule 7): it opens in the owner's own chat with the agent, where only they answer it, with
    /// the actor that waits on it named so the answer finds it (`Hub::send_cmd`); the room shows a line that it waits
    /// (a notice, which no person is shown). `data` is the card as the person's own chat would show it.
    pub(super) fn add_room_card(&self, mut data: Value) -> Option<Entry> {
        let agent = self.cfg.hq_agent.clone()?;
        let chat = crate::cards::chat_for(&self.hub, "", &agent);
        let room_name = self.hub.store.bot(&self.lane.chat).ok().flatten().map(|r| r.config.name).unwrap_or_default();
        data["author"] = chat.id.clone().into();
        data["actor"] = self.rt.clone().into();
        data["room"] = self.lane.chat.clone().into();
        data["personName"] = format!("everyone in {room_name}").into();
        let own = Lane::main(&chat.id);
        let turn = self.hub.store.max_turn(&chat.id);
        let entry = self.hub.add_entry_from(&own, EntryKind::Permission, turn, &data, None)?;
        self.notice(&format!("{} is waiting for the owner to answer a card.", self.cfg.name), NoticeStyle::Info);
        Some(entry)
    }

    /// The group turn is over (it answered, failed, was stopped or the bot shut down): its snapshot and its place in the
    /// registry go.
    pub(super) fn end_room_turn(&mut self) {
        self.turn_basis = None;
        if self.is_room_actor() {
            self.hub.room_turn_ends(&self.rt);
        }
    }
}
