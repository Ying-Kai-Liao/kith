//! `kith ask` (sections 2.5, 5.8, 5.9): send the question with an idempotency key, follow the turn's events (catching up
//! with `botThread` after any break), and print the answer. Ctrl-C only stops waiting; the turn goes on at the computer.

use std::io::{IsTerminal, Read};
use std::time::Duration;

use kith_wire::rpc::{BotAskParams, BotAsked, BotThreadParams, ConversationState, method};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::cli::AskArgs;
use crate::config::Store;
use crate::exit::{CliError, Exit};
use crate::output::Out;
use crate::session::{Client, Ev, Tuning, answer, params};
use crate::thread::{Reply, entry_of_event, parse_thread, suffix};

const MAX_CHARS: usize = 16_000;
/// Silence this long on the subscription and we read the thread anyway, in case an event was lost.
const QUIET_POLL: Duration = Duration::from_secs(10);

fn stdin_text() -> Result<String, CliError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_CHARS as u64 * 4 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CliError::usage(format!("Can't read stdin: {e}")))?;
    String::from_utf8(bytes).map_err(|_| CliError::usage("The question on stdin isn't UTF-8 text."))
}

/// The question from the argument or stdin, trimmed and checked against the host's limits.
pub fn question(
    arg: Option<&str>,
    read_stdin: impl FnOnce() -> Result<String, CliError>,
    stdin_is_terminal: bool,
) -> Result<String, CliError> {
    let raw = match arg {
        Some("-") => read_stdin()?,
        Some(t) => t.to_string(),
        None if stdin_is_terminal => {
            return Err(CliError::usage(
                "No question given. Pass it as an argument, or pipe it in (`... | kith ask @agent -`).",
            ));
        }
        None => read_stdin()?,
    };
    let text = raw.trim();
    if text.is_empty() {
        return Err(CliError::usage("The question is empty."));
    }
    if text.chars().count() > MAX_CHARS {
        return Err(CliError::usage(format!("The question is longer than {MAX_CHARS} characters.")));
    }
    Ok(text.to_string())
}

pub fn agent_name(arg: &str) -> Result<&str, CliError> {
    let name = arg.trim().trim_start_matches('@');
    if name.is_empty() { Err(CliError::usage("Name the agent: `kith ask @house \"...\"`.")) } else { Ok(name) }
}

/// What gets sent for one ask. `nonce` is the idempotency key.
pub fn ask_body(args: &AskArgs, agent: &str, text: &str, nonce: &str) -> Value {
    params(&BotAskParams {
        agent: Some(agent.to_string()),
        text: text.to_string(),
        conversation: args.thread.clone(),
        r#ref: args.r#ref.clone(),
        room: args.room.clone(),
        speaker: args.speaker.clone(),
        client_nonce: nonce.to_string(),
        no_queue: args.no_queue.then_some(true),
    })
}

/// What `botThread` is sent: the conversation after `after`, up to 500 entries.
pub fn thread_body(conversation: &str, after: i64) -> Value {
    params(&BotThreadParams { conversation: conversation.to_string(), after_seq: Some(after), limit: Some(500) })
}

struct Asked {
    conversation: String,
    turn: i64,
    seq: i64,
}

struct Printer<'a> {
    out: &'a Out,
    stream: bool,
    printed: String,
    told_waiting: Option<String>,
}

impl Printer<'_> {
    fn update(&mut self, reply: &Reply) {
        if let Some(w) = &reply.waiting
            && self.told_waiting.as_ref() != Some(w)
        {
            self.out.info(&w.to_lowercase_first());
            self.out.event(&json!({"type": "waiting", "text": w}));
            self.told_waiting = Some(w.clone());
        }
        if !self.stream {
            return;
        }
        let text = reply.text();
        if self.out.json {
            if text != self.printed {
                self.out.event(&json!({"type": "text", "text": text, "final": reply.has_final()}));
                self.printed = text;
            }
        } else if let Some(more) = suffix(&self.printed, &text) {
            // Only the new suffix; a rewritten answer waits for `finish`.
            if !more.is_empty() {
                self.out.stdout(more);
                self.printed = text;
            }
        }
    }

    fn finish(&mut self, conversation: &str, reply: &Reply) {
        let text = reply.text();
        self.out.event(&json!({"type": "done", "conversation": conversation, "turn": reply.turn, "text": text}));
        if self.out.json {
            return;
        }
        if self.stream && !self.printed.is_empty() {
            // If the answer was rewritten after we printed part of it, show the whole thing again rather than a mix.
            if suffix(&self.printed, &text).is_none() {
                self.out.stdout(&format!("\n{text}"));
            } else if let Some(more) = suffix(&self.printed, &text) {
                self.out.stdout(more);
            }
            self.out.stdout("\n");
        } else {
            self.out.stdout(&format!("{text}\n"));
        }
    }
}

/// A line the host's `waiting` text reads better as on stderr: "waiting for Sam to approve a step".
trait LowerFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowerFirst for String {
    fn to_lowercase_first(&self) -> String {
        let mut chars = self.chars();
        chars.next().map_or_else(String::new, |c| c.to_lowercase().chain(chars).collect())
    }
}

/// Reads the conversation since the ask and folds it into `reply`. True when the turn is over.
async fn catch_up(
    client: &mut Client<'_>,
    conversation: &str,
    since: i64,
    reply: &mut Reply,
    state: &mut ConversationState,
) -> Result<bool, CliError> {
    let thread = parse_thread(&client.call(method::BOT_THREAD, thread_body(conversation, since)).await?)?;
    for e in &thread.entries {
        reply.apply(e);
    }
    *state = thread.state;
    if thread.state == ConversationState::Stopped && !thread.done && !reply.has_final() {
        reply.failed.get_or_insert_with(|| "The turn was stopped.".to_string());
    }
    Ok(thread.done)
}

async fn follow(
    client: &mut Client<'_>,
    out: &Out,
    stream: bool,
    asked: &Asked,
    timeout: Duration,
) -> Result<(), CliError> {
    let deadline = Instant::now() + timeout;
    let mut reply = Reply::new(asked.turn, asked.seq);
    let mut printer = Printer { out, stream, printed: String::new(), told_waiting: None };
    let mut state = ConversationState::Idle;
    client.open_events(asked.seq).await?;
    // The answer may already be partly there: events resume from the cursor, but reading is cheap and certain.
    let mut done = catch_up(client, &asked.conversation, asked.seq, &mut reply, &mut state).await?;
    printer.update(&reply);
    loop {
        if let Some(why) = &reply.failed {
            // Whatever arrived before the failure was printed already in stream mode.
            return Err(CliError::new(Exit::Unexpected, "turnFailed", why.clone()));
        }
        if done {
            printer.finish(&asked.conversation, &reply);
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            let waiting = reply.waiting.is_some() || state == ConversationState::WaitingApproval;
            let hint = format!(
                "The turn keeps running on the computer. Pick it up with: kith read {} --wait",
                asked.conversation
            );
            return Err(if waiting {
                CliError::new(
                    Exit::Approval,
                    "waitingApproval",
                    format!("Still waiting for the owner to approve a step. {hint}"),
                )
            } else {
                CliError::timeout(format!("No answer after {} s. {hint}", timeout.as_secs()))
            });
        }
        match client.next_event(deadline.min(now + QUIET_POLL)).await? {
            Some(Ev::Event(ev)) => {
                if let Some((conversation, entry)) = entry_of_event(&ev)
                    && conversation.as_deref().is_none_or(|c| c == asked.conversation)
                    && reply.apply(&entry)
                {
                    client.advance_events(reply.last_seq);
                    printer.update(&reply);
                    // An answer marked final is confirmed against the thread: a turn may have more than one entry.
                    if reply.has_final() {
                        done = catch_up(client, &asked.conversation, asked.seq, &mut reply, &mut state).await?;
                        printer.update(&reply);
                    }
                }
            }
            Some(Ev::Resync) | None => {
                done = catch_up(client, &asked.conversation, asked.seq, &mut reply, &mut state).await?;
                client.advance_events(reply.last_seq);
                printer.update(&reply);
            }
        }
    }
}

pub async fn run(args: AskArgs, store: Store, cloud: Option<&str>, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let text = question(args.text.as_deref(), stdin_text, std::io::stdin().is_terminal())?;
    let agent = agent_name(&args.agent)?.to_string();
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = ask(&mut client, &args, &agent, &text, out).await;
    client.close().await;
    result
}

async fn ask(client: &mut Client<'_>, args: &AskArgs, agent: &str, text: &str, out: &Out) -> Result<(), CliError> {
    client.ensure_usable().await?;
    let nonce = args.idempotency_key.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let resp: BotAsked = answer(client.call(method::BOT_ASK, ask_body(args, agent, text, &nonce)).await?)?;
    let conversation = resp.conversation.as_str();
    out.info(&format!("conversation: {conversation}"));
    out.event(&json!({
        "type": "conversation", "conversation": conversation, "entryId": resp.entry_id, "turn": resp.turn,
        "state": resp.state,
    }));
    if resp.queued > 0 {
        out.info(&format!("queued behind {} other ask(s) in this conversation", resp.queued));
    }
    let asked = Asked { conversation: resp.conversation.clone(), turn: resp.turn, seq: resp.seq };
    let stream = if args.wait { false } else { args.stream || std::io::stdout().is_terminal() };
    follow(client, out, stream, &asked, Duration::from_secs(args.timeout)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(extra: &[&str]) -> AskArgs {
        let argv: Vec<&str> = ["kith", "ask", "house"].iter().copied().chain(extra.iter().copied()).collect();
        let Command::Ask(a) = Cli::try_parse_from(argv).unwrap().command else { panic!() };
        a
    }

    #[test]
    fn question_rules() {
        let none = || Ok(String::new());
        assert_eq!(question(Some("  hi  "), none, false).unwrap(), "hi");
        assert_eq!(question(Some("-"), || Ok("from stdin\n".into()), true).unwrap(), "from stdin");
        assert_eq!(question(None, || Ok("piped".into()), false).unwrap(), "piped");
        assert_eq!(question(None, none, true).unwrap_err().exit, Exit::Usage);
        assert_eq!(question(Some("   "), none, false).unwrap_err().exit, Exit::Usage);
        let long = "x".repeat(MAX_CHARS + 1);
        assert_eq!(question(Some(&long), none, false).unwrap_err().exit, Exit::Usage);
        assert!(question(Some(&"é".repeat(MAX_CHARS)), none, false).is_ok(), "the limit counts characters, not bytes");
    }

    #[test]
    fn agent_names() {
        assert_eq!(agent_name("@house").unwrap(), "house");
        assert_eq!(agent_name("house").unwrap(), "house");
        assert!(agent_name("@").is_err());
    }

    #[test]
    fn body_carries_only_what_was_asked() {
        let b = ask_body(&args(&["x"]), "house", "hello", "n-1");
        assert_eq!(b, json!({"agent": "house", "text": "hello", "clientNonce": "n-1"}));
        let b =
            ask_body(&args(&["--thread", "cv_1", "--speaker", "Alice", "--no-queue", "x"]), "house", "hello", "n-1");
        assert_eq!(b["conversation"], "cv_1");
        assert_eq!(b["speaker"], "Alice");
        assert_eq!(b["noQueue"], true);
        assert!(b.get("ref").is_none() && b.get("room").is_none());
        let b = ask_body(&args(&["--ref", "grok:conv-8841", "x"]), "house", "hello", "n-1");
        assert_eq!(b["ref"], "grok:conv-8841");
    }

    #[test]
    fn waiting_text_reads_as_a_stderr_line() {
        assert_eq!(
            "Waiting for Sam to approve a step".to_string().to_lowercase_first(),
            "waiting for Sam to approve a step"
        );
    }
}
