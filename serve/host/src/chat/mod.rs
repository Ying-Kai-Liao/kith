//! Conversation features on top of bots: messages to the user, group room turns,
//! bot-to-bot requests, prompt snapshots, long-term memory, automatic bot names and
//! routing messages sent while a bot works.

pub mod context;
pub mod files;
pub mod group;
pub mod handoff;
pub mod invite_rooms;
// C5: the host-native session start was the only caller of the chat-memory opening, change notice and file scan;
// c5-host-native-chatmem deletes them and this allow with them.
#[allow(dead_code)]
pub mod memory;
pub mod naming;
pub mod oneshot;
pub mod outbox;
pub mod profile;
pub mod prompt;
pub mod room_input;
pub mod room_people;
pub mod room_pointer;
pub mod room_post;
pub mod room_turn;
pub mod router;
pub mod tasks;
pub mod team;
pub mod topics;
pub mod uploads;
