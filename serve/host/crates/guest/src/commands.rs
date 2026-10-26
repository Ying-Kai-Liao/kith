//! The smaller commands: status, leave, agents, threads, stop, read.

use std::time::Duration;

use kith_wire::rpc::{BotConversations, BotEntryKind, ConversationState, method};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use crate::ask::thread_body;
use crate::cli::{ReadArgs, StopArgs};
use crate::config::{State, Store};
use crate::exit::{CliError, Exit};
use crate::output::Out;
use crate::session::{CLIENT_VERSION, Client, Tuning, answer};
use crate::thread::{Reply, parse_thread};

const READ_POLL: Duration = Duration::from_secs(2);

fn short_key(client: &Client<'_>) -> String {
    kith_wire::crypto::b64(client.key.verifying_key().as_bytes()).chars().take(8).collect()
}

fn agents_json(client: &Client<'_>) -> Value {
    json!(client.guest.agents.iter().map(|a| json!({"agent": a.agent, "name": a.name})).collect::<Vec<_>>())
}

/// Name, state, owner, whether the computer answers, agents. Never a secret: the key shows as its first 8 characters.
pub async fn status(store: Store, cloud: Option<&str>, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let reached = match client.guest.state {
        State::Revoked | State::Denied => false,
        state => {
            let asked = if state == State::Joining { client.prove().await } else { client.refresh_status().await };
            match asked {
                Ok(_) => true,
                // Findings, not failures: the computer can't be reached, or it has just told us the bot was removed.
                Err(e) if matches!(e.exit, Exit::Offline | Exit::Network | Exit::Removed) => false,
                Err(e) => return Err(e),
            }
        }
    };
    client.close().await;
    let g = &client.guest;
    let host = if reached { "online" } else { "offline" };
    let key = short_key(&client);
    out.event(&json!({
        "state": g.state.as_str(), "botId": g.bot_id, "name": g.name, "owner": g.owner, "agents": agents_json(&client),
        "host": host, "version": CLIENT_VERSION,
    }));
    if !out.json {
        let owner = g.owner.as_deref().unwrap_or("unknown");
        let agents = if g.agents.is_empty() {
            "none".to_string()
        } else {
            g.agents.iter().map(|a| a.agent.as_str()).collect::<Vec<_>>().join(", ")
        };
        out.stdout(&format!(
            "name: {}\nstate: {}\nowner: {owner}\ncomputer: {host}\nagents: {agents}\nkey: {key}...\nversion: {CLIENT_VERSION}\n",
            g.name,
            g.state.as_str()
        ));
    }
    match g.state {
        State::Revoked | State::Denied => Err(client.gone()),
        _ => Ok(()),
    }
}

pub async fn agents(store: Store, cloud: Option<&str>, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = async {
        client.ensure_usable().await?;
        client.refresh_status().await
    }
    .await;
    client.close().await;
    result?;
    out.event(&json!({"agents": agents_json(&client)}));
    if !out.json {
        for a in &client.guest.agents {
            out.stdout(&format!("@{}\t{}\n", a.agent, a.name));
        }
    }
    Ok(())
}

pub async fn threads(store: Store, cloud: Option<&str>, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = async {
        client.ensure_usable().await?;
        let v = client.call(method::BOT_CONVERSATIONS, json!({"limit": 100})).await?;
        answer::<BotConversations>(v)
    }
    .await;
    client.close().await;
    let listed = result?;
    out.event(&json!({"conversations": listed.conversations}));
    if !out.json {
        for c in &listed.conversations {
            out.stdout(&format!("{}\t@{}\t{}\t{}\n", c.conversation, c.agent, c.state.as_str(), c.origin_label));
        }
    }
    Ok(())
}

pub async fn stop(
    args: StopArgs,
    store: Store,
    cloud: Option<&str>,
    out: &Out,
    tuning: Tuning,
) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = async {
        client.ensure_usable().await?;
        client.call(method::BOT_STOP, json!({"conversation": args.conversation})).await
    }
    .await;
    client.close().await;
    result?;
    out.event(&json!({"type": "stopped", "conversation": args.conversation}));
    if !out.json {
        out.info(&format!("Stopped the running turn of {}", args.conversation));
    }
    Ok(())
}

/// Catches up on a conversation: prints the latest turn's reply, and with `--wait` stays until that turn is over.
pub async fn read(
    args: ReadArgs,
    store: Store,
    cloud: Option<&str>,
    out: &Out,
    tuning: Tuning,
) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let result = read_inner(&mut client, &args, out).await;
    client.close().await;
    result
}

async fn read_inner(client: &mut Client<'_>, args: &ReadArgs, out: &Out) -> Result<(), CliError> {
    client.ensure_usable().await?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let mut shown = std::collections::HashSet::new();
    loop {
        let after = args.after.map_or(0, |a| i64::try_from(a).unwrap_or(i64::MAX));
        let thread = parse_thread(&client.call(method::BOT_THREAD, thread_body(&args.conversation, after)).await?)?;
        if out.json {
            for e in thread.entries.iter().filter(|e| e.kind != BotEntryKind::User) {
                if shown.insert((e.id.clone(), e.text.len())) {
                    out.event(&json!({
                        "type": "entry", "id": e.id, "seq": e.seq, "turn": e.turn, "kind": e.kind, "text": e.text,
                        "final": e.r#final.unwrap_or(false),
                    }));
                }
            }
        }
        if thread.done || !args.wait {
            let turn = thread.entries.iter().map(|e| e.turn).max().unwrap_or(0);
            let mut reply = Reply::new(turn, 0);
            for e in &thread.entries {
                reply.apply(e);
            }
            out.event(&json!({"type": "done", "conversation": args.conversation, "turn": turn, "text": reply.text(), "done": thread.done}));
            if !out.json {
                if !thread.done {
                    out.info(&format!("The turn is still {}.", thread.state.as_str()));
                }
                out.stdout(&format!("{}\n", reply.text()));
            }
            return Ok(());
        }
        if Instant::now() + READ_POLL > deadline {
            let waiting = thread.state == ConversationState::WaitingApproval;
            return Err(if waiting {
                CliError::new(Exit::Approval, "waitingApproval", "Still waiting for the owner to approve a step.")
            } else {
                CliError::timeout(format!("No answer after {} s.", args.timeout))
            });
        }
        sleep(READ_POLL).await;
    }
}

pub async fn leave(store: Store, cloud: Option<&str>, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let mut client = Client::from_store(store, cloud, out, tuning)?;
    let never_proved = client.guest.state == State::Joining;
    let ended = matches!(client.guest.state, State::Revoked | State::Denied);
    if !never_proved && !ended {
        match client.call(method::BOT_LEAVE, json!({})).await {
            Ok(_) => {}
            // Already removed on the computer's side: nothing left to tell it.
            Err(e) if e.code == "revoked" => {}
            Err(e) => {
                client.close().await;
                // Keep the key: the bot is still joined, and the owner can still remove it from the Kith app.
                return Err(e);
            }
        }
    }
    client.close().await;
    client.store.clear()?;
    out.event(&json!({"type": "left", "name": client.guest.name}));
    if !out.json {
        if client.store.is_env() {
            out.stdout(
                "Left. KITH_GUEST_CREDENTIALS is still set in this environment: unset it, and discard that secret.\n",
            );
        } else {
            out.stdout("Left. This bot's access to the computer is gone and its key is deleted.\n");
        }
    }
    Ok(())
}
