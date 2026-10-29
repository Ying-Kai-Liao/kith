//! `kith room read` and `kith room post`: what a bot that was woken for its turn in a room runs. Both are one-shot (a
//! woken Grok can't keep a process running, so there is no listen mode): read what the room said since a cursor, post
//! one reply.

use std::io::Read;

use kith_wire::rpc::{BotEntryKind, BotPostParams, BotPosted, BotRoom, BotRoomReadParams, method};
use serde_json::json;

use crate::cli::{RoomCommand, RoomPostArgs, RoomReadArgs};
use crate::config::Store;
use crate::exit::CliError;
use crate::output::Out;
use crate::session::{Client, Tuning, answer, params};

const MAX_CHARS: usize = 16_000;

fn stdin_text() -> Result<String, CliError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_CHARS as u64 * 4 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CliError::usage(format!("Can't read stdin: {e}")))?;
    String::from_utf8(bytes).map_err(|_| CliError::usage("The post on stdin isn't UTF-8 text."))
}

/// The post's text from the argument or stdin (`-`), trimmed and checked against the host's limit.
pub fn post_text(arg: &str, read_stdin: impl FnOnce() -> Result<String, CliError>) -> Result<String, CliError> {
    let raw = if arg == "-" { read_stdin()? } else { arg.to_string() };
    let text = raw.trim();
    if text.is_empty() {
        return Err(CliError::usage("The post is empty."));
    }
    if text.chars().count() > MAX_CHARS {
        return Err(CliError::usage(format!("The post is longer than {MAX_CHARS} characters.")));
    }
    Ok(text.to_string())
}

/// The key that makes a retried post send once: given, else a hash of everything that identifies the post, so running
/// the same command again (a woken bot's run that was retried) posts once.
pub fn nonce_of(args: &RoomPostArgs, text: &str) -> String {
    if let Some(n) = &args.nonce {
        return n.clone();
    }
    let epoch = args.epoch.to_string();
    let parts: [&[u8]; 5] = [
        args.room.as_bytes(),
        args.conversation.as_bytes(),
        epoch.as_bytes(),
        args.turn_key.as_deref().unwrap_or_default().as_bytes(),
        text.as_bytes(),
    ];
    // Length-prefixed, so "ab"+"c" and "a"+"bc" differ.
    let lens: Vec<[u8; 8]> = parts.iter().map(|p| (p.len() as u64).to_be_bytes()).collect();
    let mut all: Vec<&[u8]> = vec![];
    for (len, part) in lens.iter().zip(parts) {
        all.push(len);
        all.push(part);
    }
    let digest = kith_wire::crypto::sha256(&all);
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("room-{hex}")
}

/// What `botPost` is sent.
pub fn post_body(args: &RoomPostArgs, text: &str, nonce: &str) -> serde_json::Value {
    params(&BotPostParams {
        room: args.room.clone(),
        conversation: args.conversation.clone(),
        epoch: i64::try_from(args.epoch).unwrap_or(i64::MAX),
        turn_key: args.turn_key.clone(),
        text: text.to_string(),
        client_nonce: nonce.to_string(),
    })
}

pub async fn run(
    cmd: RoomCommand,
    store: Store,
    cloud: Option<&str>,
    out: &Out,
    tuning: Tuning,
) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = match cmd {
        RoomCommand::Read(args) => read(&mut client, &args, out).await,
        RoomCommand::Post(args) => post(&mut client, &args, out).await,
    };
    client.close().await;
    result
}

async fn read(client: &mut Client<'_>, args: &RoomReadArgs, out: &Out) -> Result<(), CliError> {
    client.ensure_usable().await?;
    let body = params(&BotRoomReadParams {
        room: args.room.clone(),
        since: args.since.map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
        limit: None,
    });
    let room: BotRoom = answer(client.call(method::BOT_ROOM_READ, body).await?)?;
    out.event(&json!({"type": "room", "room": room.room, "conversation": room.conversation, "epoch": room.epoch,
                      "entries": room.entries, "cursor": room.cursor}));
    if !out.json {
        out.stdout(&format!("room {}\n", room.room));
        if let (Some(c), Some(e)) = (&room.conversation, room.epoch) {
            out.stdout(&format!("conversation {c} epoch {e}\n"));
        }
        for e in &room.entries {
            let who = match (e.kind, &e.author, &e.sender) {
                (BotEntryKind::Agent, Some(a), _) => a.clone(),
                (BotEntryKind::Agent, None, _) => "agent".to_string(),
                (_, _, Some(s)) => s.name.clone(),
                (BotEntryKind::User, _, None) => "user".to_string(),
                _ => e.kind.as_str().to_string(),
            };
            // The lane's own conversation and epoch, for a post that answers this line.
            let lane = match (&e.conversation, e.epoch) {
                (c, Some(ep)) if !c.is_empty() => format!(" [{c} epoch {ep}]"),
                _ => String::new(),
            };
            out.stdout(&format!("{who}{lane}: {}\n", e.text));
        }
        out.stdout(&format!("cursor {}\n", room.cursor));
    }
    Ok(())
}

async fn post(client: &mut Client<'_>, args: &RoomPostArgs, out: &Out) -> Result<(), CliError> {
    let text = post_text(&args.text, stdin_text)?;
    let nonce = nonce_of(args, &text);
    client.ensure_usable().await?;
    let posted: BotPosted = answer(client.call(method::BOT_POST, post_body(args, &text, &nonce)).await?)?;
    out.event(&json!({"type": "posted", "entryId": posted.entry_id, "room": posted.room,
                      "conversation": posted.conversation, "replied": posted.replied, "late": posted.late,
                      "duplicate": posted.duplicate.unwrap_or(false)}));
    if !out.json {
        let how = if posted.replied {
            "posted as your turn"
        } else if posted.late {
            "posted, but late: the room has moved on and nobody was woken"
        } else {
            "posted"
        };
        out.stdout(&format!("{how}{}\n", if posted.duplicate == Some(true) { " (already posted)" } else { "" }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn post_args(extra: &[&str]) -> RoomPostArgs {
        let argv: Vec<&str> = ["kith", "room", "post", "rm", "--conversation", "cv", "--epoch", "3"]
            .iter()
            .copied()
            .chain(extra.iter().copied())
            .collect();
        let Command::Room(RoomCommand::Post(a)) = Cli::try_parse_from(argv).unwrap().command else { panic!() };
        a
    }

    #[test]
    fn the_same_post_gets_the_same_nonce_and_another_does_not() {
        let a = post_args(&["--turn-key", "k1", "hello"]);
        assert_eq!(nonce_of(&a, "hello"), nonce_of(&post_args(&["--turn-key", "k1", "hello"]), "hello"));
        assert_ne!(nonce_of(&a, "hello"), nonce_of(&a, "hello!"));
        assert_ne!(nonce_of(&a, "hello"), nonce_of(&post_args(&["--turn-key", "k2", "hello"]), "hello"));
        assert!(nonce_of(&a, "hello").chars().count() <= 128);
        assert_eq!(nonce_of(&post_args(&["--nonce", "mine", "x"]), "x"), "mine");
    }

    #[test]
    fn the_body_names_the_turn_and_only_that() {
        let a = post_args(&["--turn-key", "k1", "hi"]);
        let b = post_body(&a, "hi", "n-1");
        assert_eq!(
            b,
            json!({"room": "rm", "conversation": "cv", "epoch": 3, "turnKey": "k1", "text": "hi", "clientNonce": "n-1"})
        );
        assert!(post_body(&post_args(&["hi"]), "hi", "n").get("turnKey").is_none());
    }

    #[test]
    fn post_text_rules() {
        let none = || Ok(String::new());
        assert_eq!(post_text("  hi ", none).unwrap(), "hi");
        assert_eq!(post_text("-", || Ok("from stdin\n".into())).unwrap(), "from stdin");
        assert!(post_text("  ", none).is_err());
        assert!(post_text(&"x".repeat(MAX_CHARS + 1), none).is_err());
    }
}
