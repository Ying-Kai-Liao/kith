//! The API the kernel calls for approval cards. A run that the kernel starts itself (`kith ask`, `kith-sched`) is
//! outside the host's own launch path, so before it starts `claude` it asks the host, with its owner credentials (the
//! serve token), for a token that can do one thing: `requestPermission` on the run socket.
//!
//! `issueAskToken {agent, session, pid, replyTo?, askedBy?}` -> `{token, socket, env, expiresInS}`
//!   - `agent`: the hq agent the run is for. `session`: what the run continues (a card is per session and resource, and a
//!     once grant is only good in its own session). `pid`: the process the run will be started under (usually the
//!     kernel's own pid): the socket only accepts the token from that process or one of its children.
//!   - `replyTo`: the `kith ask` id the run was started by, and `askedBy` the agent that asked (empty for a terminal). A
//!     card the run parks on answers that ask when it is answered, denied or expires (`kith-ask --answer`/`--reply`).
//!   - Give the run `env` (`KITH_ASK_TOKEN`, `KITH_ASK_SOCKET`) so the `--permission-prompt-tool` server it starts can
//!     call the socket. The token ends after `expiresInS`, when the process is gone, or on `revokeAskToken {token}`.
//!
//! Owner credentials only, and only from this computer: members, bot guests and paired phones never reach it.

use super::str_arg;
use crate::hub::Hub;
use crate::terminal::claim::Stopped;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;

/// How long a holder gets to end after SIGTERM before SIGKILL.
const HOLDER_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

pub(super) async fn call(hub: &Arc<Hub>, method: &str, mut b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "issueAskToken" => {
            let (agent, session) = (str_arg(&b, "agent")?, str_arg(&b, "session")?);
            let Some(pid) = b["pid"].as_u64().and_then(|p| u32::try_from(p).ok()).filter(|p| *p > 1) else {
                bail!("`pid` is required");
            };
            if agent.is_empty() || agent.len() > 128 || session.is_empty() || session.len() > 256 {
                bail!("`agent` and `session` must be short, non-empty words");
            }
            let reply = match b["replyTo"].as_str() {
                None => None,
                Some(to) => match crate::runauth::AskReply::parse(to, b["askedBy"].as_str()) {
                    Some(r) => Some(r),
                    None => bail!("`replyTo` must be a kith ask id"),
                },
            };
            let Some(socket) = crate::runauth::socket() else { bail!("the run socket isn't listening") };
            let ttl = crate::runauth::ASK_TOKEN_TTL;
            let token =
                crate::runauth::table().insert_ask(crate::runauth::ask_identity(agent, session, reply), pid, ttl)?;
            json!({
                "token": token,
                "socket": socket,
                "env": {
                    crate::runauth::ASK_TOKEN_ENV: token,
                    crate::runauth::ASK_SOCKET_ENV: socket,
                },
                "expiresInS": ttl.as_secs(),
            })
        }
        // L2: the owner's Stop for a session "held by an old run" (a card's resume waits behind it). Owner only: it is on
        // neither the member nor the bot method list, so `roles::guard` refuses everyone else.
        "stopHolder" => {
            let session = str_arg(&b, "session")?.to_owned();
            if !crate::terminal::claim::safe_session(&session) {
                bail!("`session` must be a Claude session id");
            }
            let state = hub.terminal.env().state;
            let result = tokio::task::spawn_blocking(move || {
                crate::terminal::claim::stop_holder(&state, &session, HOLDER_GRACE)
            })
            .await?;
            let shown = |c: &crate::terminal::claim::Claim| json!({"kind": c.kind, "pid": c.pid, "since": c.at});
            match result {
                Stopped::Nobody => json!({"stopped": null}),
                Stopped::OwnTurn(c) => json!({"stopped": null, "ownTurn": shown(&c)}),
                Stopped::Ended(c) => json!({"stopped": shown(&c)}),
            }
        }
        // Secret cards (cards/secret.rs). Owner only, like every method that is on neither the member nor the bot
        // list. `provideSecret` carries a key: its values are moved out of the body here and zeroed after the write.
        "provideSecret" => {
            let entry_id = str_arg(&b, "entryId")?.to_owned();
            let account = b["account"].as_str().map(str::to_owned);
            let mut values = std::collections::BTreeMap::new();
            if let Some(map) = b["values"].as_object_mut() {
                for (name, value) in std::mem::take(map) {
                    if let Value::String(s) = value {
                        values.insert(name, zeroize::Zeroizing::new(s));
                    }
                }
            }
            crate::cards::secret::provide(hub, &entry_id, account.as_deref(), values).await?
        }
        // The owner's own `kith keys add`: opens a key card for a named profile. Owner only, like provideSecret.
        "openKeyCard" => {
            crate::cards::secret::open_key_card(
                hub,
                str_arg(&b, "service")?,
                str_arg(&b, "profile")?,
                b["grant"].as_str().unwrap_or("serve"),
            )
            .await?
        }
        // Room secrets: the owner hands one member of a room a secret, once (cards/room_secret.rs). Owner only, on neither
        // the member nor the bot list. `sendSecret` carries the value: moved out of the body at once into a Zeroizing
        // buffer, and nothing here logs, audits or answers it.
        "sendSecret" => {
            let value = match std::mem::take(&mut b["value"]) {
                Value::String(s) => zeroize::Zeroizing::new(s),
                _ => bail!("`value` is required"),
            };
            crate::cards::room_secret::send(
                hub,
                str_arg(&b, "room")?,
                str_arg(&b, "to")?,
                value,
                b["label"].as_str(),
                b["ttlS"].as_i64(),
            )
            .await?
        }
        "withdrawSecret" => crate::cards::room_secret::withdraw(hub, str_arg(&b, "id")?).await,
        "roomSecrets" => crate::cards::room_secret::list(hub, str_arg(&b, "room")?),
        "listKeys" => crate::cards::secret::list_keys().await?,
        "revokeKey" => {
            crate::cards::secret::revoke_key(str_arg(&b, "agent")?, str_arg(&b, "service")?, str_arg(&b, "profile")?)
                .await?
        }
        // The owner's auto-mode "Always allow" rules for one agent (cards/autoallow.rs): list and forget. Owner only, on
        // neither the member nor the bot list.
        "allowedRules" => crate::cards::autoallow::list(str_arg(&b, "agent")?).await?,
        "removeAllowedRule" => crate::cards::autoallow::remove(str_arg(&b, "agent")?, str_arg(&b, "id")?).await?,
        "revokeAskToken" => json!({"revoked": crate::runauth::table().revoke_token(str_arg(&b, "token")?)}),
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}
