//! `kith join <link>` (sections 2.3 and 5.5): make the key, tell the cloud the invite is accepted, connect through the
//! relay, show the token to the computer, and wait for the owner's allow if it is needed.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use kith_wire::crypto::random;
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::cli::JoinArgs;
use crate::cloud;
use crate::config::{GUEST_FILE_VERSION, Guest, State, Store, credentials_line};
use crate::exit::{CliError, Exit};
use crate::link;
use crate::output::Out;
use crate::relay::{Fail, now_ms};
use crate::session::{Backoff, Client, Patience, Tuning};

fn default_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().trim().to_string();
    if host.is_empty() { "kith-bot".to_string() } else { host.chars().take(64).collect() }
}

pub fn joined_line(g: &Guest) -> String {
    let owner = g.owner.as_deref().filter(|o| !o.is_empty()).map_or_else(|| "the".to_string(), |o| format!("{o}'s"));
    let agents = if g.agents.is_empty() {
        "none yet".to_string()
    } else {
        g.agents.iter().map(|a| a.agent.as_str()).collect::<Vec<_>>().join(", ")
    };
    format!("Joined {owner} computer as \"{}\". Agents: {agents}.", g.name)
}

const WAITING_LINE: &str = "Waiting for the computer's owner to allow this bot";

/// Polls `botStatus` until the owner has answered or `deadline` passes.
async fn await_admission(client: &mut Client<'_>, deadline: Instant) -> Result<(), CliError> {
    loop {
        match client.guest.state {
            State::Active => return Ok(()),
            State::Denied | State::Revoked => return Err(client.gone()),
            State::Pending | State::Joining => {}
        }
        let poll = client.tuning.poll;
        if Instant::now() + poll > deadline {
            return Err(CliError::new(Exit::Pending, "pending", WAITING_LINE));
        }
        client.info(WAITING_LINE);
        sleep(poll).await;
        client.refresh_status().await?;
    }
}

pub async fn run(
    args: JoinArgs,
    store: Store,
    cloud_override: Option<&str>,
    out: &Out,
    tuning: Tuning,
) -> Result<(), CliError> {
    let invite = link::parse(&args.link)?;
    if store.is_env() {
        return Err(CliError::usage(
            "KITH_GUEST_CREDENTIALS is set, so this VM is already set up from stored credentials. Unset it to join with a link.",
        ));
    }
    let cloud = cloud_override.unwrap_or(&invite.cloud).trim_end_matches('/').to_string();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.wait);

    let existing = store.load()?;
    let (key, mut guest, resumed) = match existing {
        // The same link again on the same VM: carry on where it stopped. The cloud answers the same key's repeat with the
        // same device, and the computer answers a proved bot's repeat with its state.
        Some((key, g)) if g.invite_id == invite.invite_id && g.computer_id == invite.computer_id => (key, g, true),
        Some((_, g)) if matches!(g.state, State::Pending | State::Active) && !args.rekey => {
            return Err(CliError::usage(format!(
                "Already joined as \"{}\". Run `kith leave` to remove this bot first, or pass --rekey to join with a new invite.",
                g.name
            )));
        }
        other => {
            let keep = other.as_ref().filter(|_| args.rekey).map(|(_, g)| g.name.clone());
            let key = SigningKey::from_bytes(&random::<32>());
            let guest = Guest {
                v: GUEST_FILE_VERSION,
                cloud: cloud.clone(),
                computer_id: invite.computer_id.clone(),
                host_sign_key: invite.host_sign_key.clone(),
                bot_id: None,
                name: args.name.clone().or(keep).unwrap_or_else(default_name),
                state: State::Joining,
                invite_id: invite.invite_id.clone(),
                token: Some(invite.token.clone()),
                created_at: u64::try_from(now_ms()).unwrap_or(0),
                provider: args.provider.clone(),
                owner: None,
                agents: vec![],
            };
            (key, guest, false)
        }
    };
    if resumed {
        if let Some(name) = &args.name {
            guest.name.clone_from(name);
        }
        if guest.state == State::Joining && guest.token.is_none() {
            guest.token = Some(invite.token.clone());
        }
    }
    // Written before the first network call, so a crash or a dropped connection can be finished by the next command.
    store.save(Some(&key), &guest)?;

    if guest.state == State::Joining {
        out.info("Joining...");
        let give_up = deadline.max(started + tuning.dial_timeout);
        let mut backoff = Backoff::new(tuning.backoff);
        loop {
            let res = cloud::accept(
                &cloud,
                &key,
                &invite.computer_id,
                &invite.invite_id,
                &invite.token,
                &guest.name,
                guest.provider.as_deref(),
            )
            .await;
            match res {
                Ok(_) => break,
                Err(f @ (Fail::Network(_) | Fail::Dropped(_))) => {
                    let wait = backoff.delay();
                    if Instant::now() + wait > give_up {
                        return Err(f.into_cli());
                    }
                    sleep(wait).await;
                }
                Err(f) => return Err(f.into_cli()),
            }
        }
        if args.wait == 0 {
            let line = "Accepted by the cloud. Run `kith status` to finish joining.";
            out.event(&json!({"type": "accepted", "state": "joining", "name": guest.name}));
            if !out.json {
                out.stdout(&format!("{line}\n"));
            }
            return Ok(());
        }
    }

    let mut client = Client::new(store, key, guest, Some(&cloud), out, tuning)?;
    let status = finish(&mut client, deadline, args.wait).await;
    client.close().await;
    status?;

    let guest = &client.guest;
    let credentials = args.print_credentials.then(|| credentials_line(&client.key, guest));
    out.event(&json!({
        "type": "joined", "state": guest.state.as_str(), "botId": guest.bot_id, "name": guest.name,
        "owner": guest.owner, "agents": guest.agents, "credentials": credentials,
    }));
    if !out.json {
        match &credentials {
            // stdout is exactly the credentials line, so `$(kith join ... --print-credentials)` is the secret and nothing else.
            Some(line) => {
                out.info(&joined_line(guest));
                out.info("The line on stdout is this bot's key: keep it as a secret and pass it as KITH_GUEST_CREDENTIALS if this VM loses its files.");
                out.stdout(&format!("{line}\n"));
            }
            None => out.stdout(&format!("{}\n", joined_line(guest))),
        }
    }
    Ok(())
}

/// Steps 4 to 8 of the join sequence: connect, prove, and wait for admission.
async fn finish(client: &mut Client<'_>, deadline: Instant, wait: u64) -> Result<(), CliError> {
    let patience = if wait == 0 { Patience::FailFast } else { Patience::Until(deadline) };
    client.ensure(patience).await.map_err(|e| {
        if e.exit == Exit::Offline {
            CliError::new(
                Exit::Offline,
                "hostOffline",
                "Joined at the cloud; the computer is offline. Run `kith status` later to finish.",
            )
        } else {
            e
        }
    })?;
    if client.guest.state == State::Joining {
        client.prove().await?;
    } else {
        // Joined before (the same link again): ask for the current state.
        match client.refresh_status().await {
            Ok(_) => {}
            // The cached state is the answer when the computer can't be asked right now.
            Err(e) if matches!(e.exit, Exit::Offline | Exit::Network) && client.guest.state == State::Active => {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
    await_admission(client, deadline).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentRef;

    fn guest(owner: Option<&str>, agents: &[&str]) -> Guest {
        Guest {
            v: 1,
            cloud: "https://c".into(),
            computer_id: "c".into(),
            host_sign_key: "k".into(),
            bot_id: None,
            name: "Rex".into(),
            state: State::Active,
            invite_id: "i".into(),
            token: None,
            created_at: 0,
            provider: None,
            owner: owner.map(str::to_string),
            agents: agents.iter().map(|a| AgentRef { agent: (*a).to_string(), name: String::new() }).collect(),
        }
    }

    #[test]
    fn joined_line_is_the_documented_sentence() {
        assert_eq!(
            joined_line(&guest(Some("Sam"), &["house", "notes"])),
            "Joined Sam's computer as \"Rex\". Agents: house, notes."
        );
        assert_eq!(joined_line(&guest(None, &[])), "Joined the computer as \"Rex\". Agents: none yet.");
    }
}
