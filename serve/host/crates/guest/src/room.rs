//! `kith room read`, `kith room post` and `kith room secret`: what a bot that was woken for its turn in a room runs. All
//! are one-shot (a woken Grok can't keep a process running, so there is no listen mode): read what the room said since a
//! cursor, post one reply, take a secret the owner handed it.

use std::fmt::Write as _;
use std::io::Read;

use kith_wire::rpc::{
    BotEntryKind, BotPostParams, BotPosted, BotRoom, BotRoomReadParams, BotSecret, BotSecretFetchParams, method,
};
use serde_json::json;

use crate::cli::{RoomCommand, RoomPostArgs, RoomReadArgs, RoomSecretArgs};
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
    let hex = digest.iter().take(16).fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
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
        RoomCommand::Secret(args) => secret(&mut client, &args, out).await,
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
                      "entries": room.entries, "cursor": room.cursor, "secrets": room.secrets}));
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
        // Waiting for this bot to take: id, who from, the sender's label, when it lapses. Never the value.
        for s in room.secrets.iter().flatten() {
            let label = s.label.as_deref().map_or(String::new(), |l| format!(" \"{l}\""));
            out.stdout(&format!(
                "secret {} from {}{label}, lapses in {}: kith room secret {} --to-file PATH\n",
                s.id,
                s.from,
                lapses_in(s.expires_at),
                s.id
            ));
        }
        out.stdout(&format!("cursor {}\n", room.cursor));
    }
    Ok(())
}

/// "9 min" or "40 s", from an epoch-ms time; "0 s" once past.
fn lapses_in(expires_at_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let secs = ((expires_at_ms - now) / 1000).max(0);
    if secs >= 60 { format!("{} min", secs / 60) } else { format!("{secs} s") }
}

/// Opens the file the value will go to before asking the host: the host hands the value over once, so a path that
/// can't be written must be found out first. New file only, mode 0600; a symlink or an existing file is refused.
fn open_new(path: &std::path::Path) -> Result<std::fs::File, CliError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => CliError::usage(format!(
            "{} already exists: pick a path that doesn't, so no existing file is overwritten. The secret was not taken.",
            path.display()
        )),
        _ => CliError::usage(format!("Can't write {}: {e}. The secret was not taken.", path.display())),
    })
}

async fn secret(client: &mut Client<'_>, args: &RoomSecretArgs, out: &Out) -> Result<(), CliError> {
    use std::io::Write;
    let file = args.to_file.as_deref().map(|p| open_new(p).map(|f| (p, f))).transpose()?;
    let got = match fetch(client, &args.id).await {
        Ok(got) => got,
        Err(e) => {
            if let Some((path, _)) = &file {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };
    let mut event = json!({"type": "secret", "id": got.id, "room": got.room, "from": got.from, "label": got.label});
    if let Some((path, mut f)) = file {
        if let Err(e) = f.write_all(got.value.as_bytes()).and_then(|()| f.flush()) {
            let _ = std::fs::remove_file(path);
            return Err(CliError::unexpected(format!(
                "Can't write {}: {e}. The secret is gone: ask for a new one.",
                path.display()
            )));
        }
        event["file"] = json!(path);
        out.event(&event);
        if !out.json {
            out.stdout(&format!("{}\n", path.display()));
        }
    } else {
        event["value"] = json!(got.value);
        out.event(&event);
        if !out.json {
            out.stdout(&format!("{}\n", got.value));
        }
    }
    Ok(())
}

async fn fetch(client: &mut Client<'_>, id: &str) -> Result<BotSecret, CliError> {
    client.ensure_usable().await?;
    answer(client.call(method::BOT_SECRET_FETCH, params(&BotSecretFetchParams { id: id.to_string() })).await?)
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

    #[test]
    fn open_new_is_private_and_never_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        drop(open_new(&path).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(open_new(&path).is_err());
        let link = dir.path().join("l");
        std::os::unix::fs::symlink(dir.path().join("target"), &link).unwrap();
        assert!(open_new(&link).is_err());
        assert!(!dir.path().join("target").exists());
    }

    #[test]
    fn lapses_in_reads_minutes_then_seconds() {
        let now =
            i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis())
                .unwrap();
        assert_eq!(lapses_in(now - 5000), "0 s");
        assert!(lapses_in(now + 300_000).ends_with("min"));
        assert!(lapses_in(now + 30_000).ends_with(" s"));
    }
}
