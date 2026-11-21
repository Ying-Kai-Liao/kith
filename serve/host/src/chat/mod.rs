//! Conversation features on top of bots: messages to the user, group room turns,
//! bot-to-bot requests, prompt snapshots, long-term memory, automatic bot names and
//! routing messages sent while a bot works.

pub mod files;
pub mod group;
pub mod handoff;
pub mod invite_rooms;
pub mod memory;
pub mod naming;
pub mod outbox;
pub mod prompt;
pub mod room_input;
pub mod room_people;
pub mod room_pointer;
pub mod room_post;
pub mod room_turn;
pub mod router;
pub mod status;
pub mod tasks;
pub mod topics;
pub mod uploads;
