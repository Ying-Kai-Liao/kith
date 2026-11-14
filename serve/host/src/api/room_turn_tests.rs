//! Room-person turns (rooms with people, PR C) at the places the rules are decided: what a turn reads and what it writes,
//! who answers, who answers its cards, what a revoke ends, and the carried SHOULDs of PR B. Each test names the rule and
//! the model's broken variant it holds the code to (verification/rooms/checks.tsv): a rule taken out of the code makes
//! its test fail.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::rig::Rig;
use crate::chat::room_input::{readable, unreadable};
use crate::chat::room_people::FLAG;
use crate::chat::room_post::{seen_by, shown_entry};
use crate::chat::room_turn::{Level, RoomBasis, is_room_person, room_person_id};
use crate::store::{BotConfig, Entry, EntryKind, Lane, Stay};
use serde_json::{Value, json};

fn room_of(r: &Rig, agents: &[&str]) -> String {
    let members: Vec<String> = agents.iter().map(|a| r.owner_bot(a)).collect();
    let cfg: BotConfig =
        serde_json::from_value(json!({"id": "", "kind": "group", "name": "Crew", "members": members})).unwrap();
    r.hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned()
}

fn line(what: &str) -> Value {
    json!({"notice": what, "style": "info", "text": what})
}

fn join(r: &Rig, room: &str, person: &str) -> i64 {
    r.hub.store.join_room_person(room, person, &line("personJoined")).unwrap().unwrap().seq
}

fn leave(r: &Rig, room: &str, person: &str) -> i64 {
    r.hub.store.leave_room_person(room, person, &line("personLeft")).unwrap().unwrap().seq
}

/// A message of the room as the real writers leave it: the owner's post, a person's post or an agent's reply, stamped as
/// each is (`origin` is what the writer names).
fn say(r: &Rig, room: &str, kind: EntryKind, text: &str, origin: Option<i64>) -> Entry {
    let data = json!({"text": text, "final": true, "author": "agentbot"});
    r.hub.add_entry_from(&Lane::main(room), kind, 1, &data, origin).unwrap()
}

async fn rig() -> (Rig, String) {
    let r = Rig::new().await;
    r.hub.store.kv_set(FLAG, "on").unwrap();
    let room = room_of(&r, &["house"]);
    (r, room)
}

fn basis_of(r: &Rig, room: &str) -> (RoomBasis, Vec<Stay>) {
    (RoomBasis::read(&r.hub.store, room), r.hub.store.room_stays(room))
}

fn texts(lines: &[Entry]) -> Vec<String> {
    lines.iter().map(|e| e.data["text"].as_str().unwrap().to_owned()).collect()
}

/// Rule 1 and R1: a turn's lines take the gen the turn STARTED in, whenever they land. Broken variant: stamped at landing
/// (the known break; model doors-stampland, notag-owner) shows the new person what was written before they came.
#[tokio::test]
async fn a_lines_stamp_is_its_turns_start_gen_and_a_person_who_joined_meanwhile_is_not_shown_it() {
    let (r, room) = rig().await;
    join(&r, &room, "ann");
    let (started, _) = basis_of(&r, &room);
    // Ben joins while the turn runs; its reply lands afterwards, stamped with the gen it started in.
    let ben_joined = join(&r, &room, "ben");
    let reply = say(&r, &room, EntryKind::Agent, "from the turn that started before ben", Some(started.room_gen));
    assert!(started.room_gen < ben_joined);
    assert_eq!(r.hub.store.origin_gen(&reply.id), Some(started.room_gen), "never the gen at landing");
    let shown = |who: &str, seat: i64| shown_entry(&r.hub.store, who, seat, &serde_json::to_value(&reply).unwrap());
    assert!(shown("ann", started.room_gen).is_some(), "ann was there when it started");
    assert!(shown("ben", ben_joined).is_none(), "ben was not");
}

/// A writer that names no gen is hidden from every person (NULL = unstamped), the owner still reads it.
#[tokio::test]
async fn an_unstamped_line_is_hidden_from_people_and_a_turn_at_room_level() {
    let (r, room) = rig().await;
    let old = say(&r, &room, EntryKind::User, "from before any stay", None);
    let joined = join(&r, &room, "ann");
    assert!(!seen_by(joined, "user", r.hub.store.origin_gen(&old.id)));
    let (b, stays) = basis_of(&r, &room);
    assert_eq!(b.level(), Level::Room);
    assert!(readable(&r.hub, &b, &stays, vec![old]).is_empty());
}

/// Rule 1: the owner's own post in a group is stamped with the room's gen in the same step as the append.
#[tokio::test]
async fn the_owners_post_in_a_room_is_stamped_with_the_rooms_gen() {
    let (r, room) = rig().await;
    let joined = join(&r, &room, "ann");
    let data = json!({"text": "kai says", "status": "sent"});
    let e = r.hub.add_owner_post(&Lane::main(&room), 1, &data, true).unwrap();
    assert_eq!(r.hub.store.origin_gen(&e.id), Some(joined));
    let one_to_one = r.hub.add_owner_post(&Lane::main(&r.owner_bot("house")), 1, &data, false).unwrap();
    assert_eq!(r.hub.store.origin_gen(&one_to_one.id), None, "a 1:1 chat is not a room: nothing stamped");
}

/// Rule 3 / model design-flow: a message left unanswered across a join is not answered by a room turn, and its author is
/// told with a line no later joiner is shown. Broken variant (the trigger not checked): the turn answers Kai's pre-join post.
#[tokio::test]
async fn a_post_from_before_a_join_is_not_answered_in_the_room_and_its_author_is_told() {
    let (r, room) = rig().await;
    let pre = say(&r, &room, EntryKind::User, "kai asked before ben came", Some(r.hub.store.room_gen(&room)));
    let ben = join(&r, &room, "ben");
    let (b, stays) = basis_of(&r, &room);
    let said = unreadable(&r.hub, &b, &stays, std::slice::from_ref(&pre), "owner").expect("the conversation ends");
    let text = said.expect("its author is told why");
    assert!(text.contains("joined the room first") && !text.contains('@'), "{text}");
    // A post written after the join is fine.
    let after = say(&r, &room, EntryKind::User, "kai asks again", Some(ben));
    assert!(unreadable(&r.hub, &b, &stays, std::slice::from_ref(&after), "owner").is_none());
    // The note is stamped with the asking's own gen: ben, who came later, is not shown it.
    crate::chat::room_input::unanswered_note(&r.hub, &Lane::main(&room), None, &[pre], &text);
    let notes: Vec<Entry> =
        r.hub.store.history(&room, i64::MAX, 50).unwrap().into_iter().filter(|e| e.data["unanswered"] == true).collect();
    assert_eq!(notes.len(), 1);
    assert!(!seen_by(ben, &notes[0].kind, r.hub.store.origin_gen(&notes[0].id)));
}

/// Rule 3 / model design-r2: an owner-level turn after the last leave reads nothing that was written while the person
/// was in. Broken variant (the input not cut): the owner turn answers the person's unanswered post.
#[tokio::test]
async fn an_owner_turn_after_the_last_leave_reads_nothing_written_while_a_person_was_in() {
    let (r, room) = rig().await;
    let joined = join(&r, &room, "ben");
    let post = say(&r, &room, EntryKind::User, "ben asks the planner", Some(joined));
    let reply = say(&r, &room, EntryKind::Agent, "a room turn's reply", Some(joined));
    let left = leave(&r, &room, "ben");
    let kai = say(&r, &room, EntryKind::User, "kai, alone again", Some(left));
    let (b, stays) = basis_of(&r, &room);
    assert_eq!(b.level(), Level::Owner);
    assert_eq!(texts(&readable(&r.hub, &b, &stays, vec![post.clone(), reply, kai.clone()])), ["kai, alone again"]);
    let said = unreadable(&r.hub, &b, &stays, &[post], "ben").expect("ends");
    assert!(said.is_none() || said.unwrap().contains("left the room first"));
    assert!(unreadable(&r.hub, &b, &stays, &[kai], "owner").is_none());
}

/// Rule 3: a room turn reads only what every current person can see. Ann joined first, ben later: a line of ann's stay
/// from before ben came is not in the prompt of a turn for both.
#[tokio::test]
async fn a_room_turn_reads_only_what_every_current_person_sees() {
    let (r, room) = rig().await;
    let ann = join(&r, &room, "ann");
    let early = say(&r, &room, EntryKind::User, "ann before ben", Some(ann));
    let ben = join(&r, &room, "ben");
    let late = say(&r, &room, EntryKind::User, "ann after ben", Some(ben));
    let (b, stays) = basis_of(&r, &room);
    assert_eq!(texts(&readable(&r.hub, &b, &stays, vec![early, late])), ["ann after ben"]);
    // Ben leaves: ann alone reads what she always could, and the session (the join gen) did not restart.
    let join_gen = b.join_gen;
    leave(&r, &room, "ben");
    let (b, _) = basis_of(&r, &room);
    assert_eq!((b.floor, b.join_gen), (ann, join_gen), "a leave moves no session");
}

/// Rule 6 and the architect's MUST 10: a room with people never falls back to "everyone answers".
#[tokio::test]
async fn nobody_answers_a_post_that_mentions_nobody_when_people_are_in_the_room() {
    let (r, room) = rig().await;
    let _ = (&r, &room);
    let named = |text: &str| -> BotConfig {
        let cfg: BotConfig = serde_json::from_value(json!({"id": "b1", "name": "Planner", "backend": "claude", "cwd": "/tmp"}))
            .unwrap();
        let _ = text;
        cfg
    };
    let members = [named("")];
    let msg = |text: &str| Entry {
        id: "m".into(),
        seq: 1,
        bot_id: "r".into(),
        thread_id: None,
        rev: 1,
        kind: "user".into(),
        turn: 1,
        data: json!({"text": text}),
        created_at: 0,
        updated_at: 0,
    };
    let who = |text: &str, people: bool| crate::chat::group::responders(&members, &[msg(text)], people).len();
    assert_eq!(who("hello everyone", false), 1, "today's rooms: no mention means all");
    assert_eq!(who("hello there", true), 0, "a room with people: nobody");
    assert_eq!(who("@all hello", true), 0, "@all is not a mention there");
    assert_eq!(who("@planner hello", true), 1);
}

/// Rule 7: only the owner answers a room person's cards, whoever wrote.
#[tokio::test]
async fn only_the_owner_answers_a_room_persons_cards() {
    let (r, _room) = rig().await;
    let id = room_person_id("grp", 9);
    assert!(is_room_person(&id));
    for ask in [true, false] {
        let req = crate::access::Request {
            person: &id,
            agent: "house",
            owner_run: false,
            ask: crate::access::Ask::Tool { in_grants: ask },
        };
        assert_eq!(crate::access::approver(&r.hub.store, &req).await, None, "the owner alone");
    }
}

/// Rule 12 / model jobs-revokekeepsturn: a revoke of p ends the room turn whose basis holds p, and only that one.
#[tokio::test]
async fn a_revoke_ends_the_room_turns_whose_basis_holds_the_person() {
    let (r, room) = rig().await;
    join(&r, &room, "ann");
    join(&r, &room, "ben");
    let (b, _) = basis_of(&r, &room);
    let bot = r.owner_bot("house");
    let mine = crate::agent::bot::actor_key_as(&bot, &Lane::main(&room), b.person_id().as_deref());
    let other = crate::agent::bot::actor_key_as(&bot, &Lane::main("other"), Some("room-other-3"));
    r.hub.room_turn_begins(&mine, &["ann".into(), "ben".into()]);
    r.hub.room_turn_begins(&other, &["cy".into()]);
    let ended = r.hub.end_room_turns_of("ben");
    assert_eq!(ended, [b.person_id().unwrap()], "the one whose basis holds ben");
    assert_eq!(r.hub.end_room_turns_of("dan"), Vec::<String>::new());
    // Every id a person has run as, for the sweep of what a restarted host lost track of.
    assert_eq!(r.hub.room_person_ids_of("ben"), [b.person_id().unwrap()]);
    r.hub.room_turn_ends(&mine);
    assert_eq!(r.hub.room_turn_of(&mine), None);
}

/// R3: a revoke writes no room row, so its undo gives the room back exactly.
#[tokio::test]
async fn a_revoke_writes_no_room_row() {
    let (r, room) = rig().await;
    join(&r, &room, "ann");
    let before = r.hub.store.room_stays(&room);
    let room_gen = r.hub.store.room_gen(&room);
    let _ = r.hub.end_room_turns_of("ann");
    r.hub.store.suspend_person("ann").unwrap();
    r.hub.store.restore_person("ann").unwrap();
    assert_eq!((r.hub.store.room_stays(&room), r.hub.store.room_gen(&room)), (before, room_gen));
}

/// MUST 4: the person an actor runs as is the actor's own. A bot edit (its whole config arrives) leaves it.
#[test]
fn a_room_actors_key_and_session_are_its_own() {
    let cfg: BotConfig = serde_json::from_value(
        json!({"id": "b", "name": "B", "backend": "claude", "cwd": "/tmp", "person": "room-g-4"}),
    )
    .unwrap();
    assert_eq!(crate::agent::bot::session_owner(&cfg), "b#room-g-4");
    let mut owner = cfg.clone();
    owner.person = None;
    assert_eq!(crate::agent::bot::session_owner(&owner), "b");
}

/// PR B SHOULD 1: a nonce used in an earlier stay is not answered with that stay's line.
#[tokio::test]
async fn a_reused_nonce_in_a_new_stay_is_not_answered_with_the_old_stays_line() {
    let (r, room) = rig().await;
    let first = join(&r, &room, "ann");
    let data = json!({"text": "hi", "clientNonce": "n1", "sender": {"person": "ann", "name": "Ann"}});
    let old = r.hub.add_entry_from(&Lane::main(&room), EntryKind::User, 1, &data, Some(first)).unwrap();
    assert_eq!(r.hub.store.find_person_nonce(&room, "ann", "n1", first).map(|e| e.id), Some(old.id.clone()));
    leave(&r, &room, "ann");
    let second = join(&r, &room, "ann");
    assert!(r.hub.store.find_person_nonce(&room, "ann", "n1", second).is_none(), "the old stay's line is not theirs now");
}

/// PR B SHOULD 2: Kai's send is not answered with a person's line that has the same nonce.
#[tokio::test]
async fn the_owners_nonce_lookup_never_returns_a_persons_line() {
    let (r, room) = rig().await;
    let ann = join(&r, &room, "ann");
    let data = json!({"text": "hi", "clientNonce": "n1", "sender": {"person": "ann", "name": "Ann"}});
    r.hub.add_entry_from(&Lane::main(&room), EntryKind::User, 1, &data, Some(ann)).unwrap();
    assert!(r.hub.store.find_by_nonce(&room, "n1").is_none());
    let own = r.hub.add_owner_post(&Lane::main(&room), 2, &json!({"text": "kai", "clientNonce": "n1"}), true).unwrap();
    assert_eq!(r.hub.store.find_by_nonce(&room, "n1").map(|e| e.id), Some(own.id));
}

/// PR B SHOULD 3: the reaction is written in the same transaction as the stay and revoke check.
#[tokio::test]
async fn a_reaction_is_refused_once_a_revoke_is_recorded_even_after_the_check() {
    let (r, room) = rig().await;
    let ann = join(&r, &room, "ann");
    let e = say(&r, &room, EntryKind::Agent, "react to me", Some(ann));
    assert!(r.hub.react_as(&e.id, "👍", Some(("ann", ann))).is_ok());
    r.hub.store.suspend_person("ann").unwrap();
    let err = r.hub.react_as(&e.id, "🎉", Some(("ann", ann))).unwrap_err();
    assert!(err.is::<super::devices::NotAllowed>(), "{err:#}");
    assert_eq!(r.hub.store.entry(&e.id).unwrap().data["reactions"], json!(["👍"]), "nothing written");
}

/// MUST 9: a person's id is never a room person's: a name that would slug to `room-...` is given another id.
#[test]
fn a_name_never_makes_a_room_persons_id() {
    assert_eq!(super::people::slug("Room Mate"), "p-room-mate");
    assert_eq!(super::people::slug("Sam"), "sam");
    assert!(!is_room_person(&super::people::slug("ROOM-1")));
}
