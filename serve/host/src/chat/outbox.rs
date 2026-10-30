//! How a bot talks to the user, Grok Bot's way: in its chat (and threads) the user sees
//! only the messages the bot sends with the built-in `chat` MCP server's `send_message`,
//! each one its own bubble. What the agent writes as its reply stays in the trace, unless
//! it sent nothing in the turn: then its last text becomes the reply, as before.

use crate::agent::bot::Cmd;
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// How to talk to the user: part of every bot's instructions (`context`), and the `chat` server's.
pub const INSTRUCTIONS: &str = "In your chat with the user and its threads, the user sees only what you send with send_message; \
text you write as your reply never reaches them. Send a message whenever you have something worth telling them: \
short and plain, one thought per message, in the user's language. Lead with the result, then what needs the user. \
Use send_file(path, name?) to share a generated regular file (any type, up to 100 MiB); a local path in text is not a download. To share several files at once, pass them together in one call with send_file(paths) (up to 10): they show as one group. Don't send just to acknowledge, to say you started or are still working, or to repeat yourself.";

/// An hq agent's: what it writes already reaches the user as its reply, so only files need a tool.
pub const FILES_INSTRUCTIONS: &str = "Use send_file(path, name?) to give the user an image or any other regular file (up to 100 MiB) in this chat or thread: \
images show inline, other files as a download card. To share several images or files at once, pass them together in one call with send_file(paths) (up to 10): they show as one group. A local path in your reply is not a download. \
Also use it on a call: the user sees the file in the chat, nothing is read aloud. A successful call is final; don't repeat it. \
Use call_person(reason) only for something that truly needs the user's voice now: it rings their open Kith app, and the answer says ringing, busy, rate-limited or not-allowed. Don't call again to chase it. \
Use request_secret(service, purpose) when you need a service's key: the user gets a private card, you never see the key, and you are told its name once it is saved. Ask once, then end your turn.";

/// Longest message accepted, in bytes.
const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// How long the bot's actor has to take the message.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a `send_file` call may take: up to ten copies. mcp.rs's chatCall timeout for it must stay above.
pub const SEND_FILE_TIMEOUT: Duration = Duration::from_secs(120);

pub fn tools() -> Value {
    json!([{
        "name": "send_message",
        "description": "Send the user one chat message (Markdown). It shows right away as its own bubble on their phone and computer; send several for several thoughts.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string", "description": "The message, as the user should read it."}},
            "required": ["text"],
        },
        "annotations": {"readOnlyHint": false, "idempotentHint": false},
    }, {
        "name": "send_file",
        "description": "Share regular files with the user in your own chat or thread. Any file type, including empty files, up to 100 MiB each. The host saves an immutable copy. Paths are absolute or relative to your working directory. Give one file as path, or several as paths (up to 10) in one call: they show as one group, and if any fails nothing is sent. A successful response means the file message was published; do not repeat it.",
        "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}, "paths": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 10, "description": "Several files to share together, instead of path."}, "name": {"type": "string", "description": "Optional display filename, without folders. Only with path."}}},
        "annotations": {"readOnlyHint": false, "idempotentHint": false},
    }])
}

/// `actor`: the bot's actor whose agent called (see `agent::bot::actor_key`); the message goes to its turn.
pub async fn call(hub: &Arc<Hub>, actor: &str, name: &str, args: &Value) -> Result<Value> {
    if name == "send_file" {
        return send_file(hub, actor, args).await;
    }
    if name != "send_message" {
        bail!("unknown chat tool: {name}");
    }
    let text = args["text"].as_str().unwrap_or_default().trim();
    if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
        bail!("text must be nonempty and at most {MAX_MESSAGE_BYTES} bytes");
    }
    let (reply, delivered) = tokio::sync::oneshot::channel();
    hub.send_to_actor(actor, Cmd::SendToUser { text: text.to_owned(), reply })?;
    tokio::time::timeout(DELIVERY_TIMEOUT, delivered)
        .await
        .map_err(|_| anyhow!("the bot didn't take the message in time"))?
        .map_err(|_| anyhow!("the bot shut down"))??;
    Ok(json!({"sent": true}))
}

struct CancelCopy(Arc<std::sync::atomic::AtomicBool>);
impl Drop for CancelCopy {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// The files a `send_file` call names, and the optional display name: exactly one of `path` / `paths`.
fn parse_files(args: &Value) -> Result<(Vec<String>, Option<String>)> {
    let name = match args.get("name") {
        Some(value) => Some(crate::chat::files::name(value.as_str().ok_or_else(|| anyhow!("name must be a string"))?)?),
        None => None,
    };
    let one = |value: &Value| value.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    match (args.get("path"), args.get("paths")) {
        (Some(path), None) => Ok((vec![one(path).ok_or_else(|| anyhow!("path must be a nonempty string"))?], name)),
        (None, Some(paths)) => {
            ensure!(name.is_none(), "name only works with path, not paths");
            let paths = paths.as_array().ok_or_else(|| anyhow!("paths must be an array of strings"))?;
            let max = crate::chat::files::MAX_BATCH;
            ensure!((1..=max).contains(&paths.len()), "paths must hold 1 to {max} files");
            let paths = paths
                .iter()
                .map(|p| one(p).ok_or_else(|| anyhow!("every path must be a nonempty string")))
                .collect::<Result<_>>()?;
            Ok((paths, None))
        }
        (None, None) => bail!("path or paths is required"),
        (Some(_), Some(_)) => bail!("give path or paths, not both"),
    }
}

async fn send_file(hub: &Arc<Hub>, actor: &str, args: &Value) -> Result<Value> {
    let (paths, name) = parse_files(args)?;
    let guard = CancelCopy(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let (reply, delivered) = tokio::sync::oneshot::channel();
    hub.send_to_actor(actor, Cmd::SendFile { paths, name, cancelled: guard.0.clone(), reply })?;
    let result = tokio::time::timeout(SEND_FILE_TIMEOUT, delivered).await;
    let delivered = result.map_err(|_| anyhow!("file sharing timed out; check the conversation before retrying"))?;
    delivered.map_err(|_| anyhow!("file sharing interrupted"))??;
    Ok(json!({"sent": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn single_path_and_name_work_as_before() {
        let (paths, name) = parse_files(&json!({"path": "a.png", "name": "x.png"})).unwrap();
        assert_eq!((paths, name), (strs(&["a.png"]), Some("x.png".to_owned())));
    }

    #[test]
    fn paths_keep_their_order_and_duplicates() {
        let (paths, name) = parse_files(&json!({"paths": ["b", "a", "b"]})).unwrap();
        assert_eq!((paths, name), (strs(&["b", "a", "b"]), None));
    }

    #[test]
    fn rejects_bad_shapes() {
        let eleven: Vec<String> = (0..11).map(|i| i.to_string()).collect();
        for args in [
            json!({}),
            json!({"path": "a", "paths": ["b"]}),
            json!({"paths": ["a"], "name": "x"}),
            json!({"paths": []}),
            json!({"paths": eleven}),
            json!({"paths": ["a", ""]}),
            json!({"paths": "a"}),
            json!({"path": ""}),
        ] {
            assert!(parse_files(&args).is_err(), "{args}");
        }
        let ten: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert!(parse_files(&json!({"paths": ten})).is_ok());
    }
}
