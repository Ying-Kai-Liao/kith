//! Conversation features on top of bots: messages to the user, group room turns,
//! bot-to-bot requests, prompt snapshots, long-term memory, automatic bot names and
//! routing messages sent while a bot works.

pub mod context;
pub mod files;
pub mod group;
pub mod handoff;
pub mod memory;
pub mod naming;
pub mod oneshot;
pub mod outbox;
pub mod profile;
pub mod prompt;
pub mod router;
pub mod tasks;
pub mod team;
pub mod topics;
pub mod uploads;
