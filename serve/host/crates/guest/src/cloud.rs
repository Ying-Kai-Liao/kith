//! The one plain HTTPS call: `POST {cloud}/v1/bot-invites/:id/accept`, signed with the bot's own key (no Clerk account).
//! The cloud gets `sha256hex(token)` and never the token; it stores the hash of that hash (section 3).

use std::time::Duration;

use ed25519_dalek::SigningKey;
use kith_wire::cloud::{AcceptBotInvite, BotInviteAccepted, ErrorCode, path};
use kith_wire::crypto::sha256;
use serde_json::Value;
use url::Url;

use crate::exit::{CliError, Exit};
use crate::relay::{Fail, authority, signature_header};

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// What the invite proves to the cloud: `sha256hex(t)`. The same string the host holds as its `token_hash`.
pub fn token_proof(token: &str) -> String {
    hex(&sha256(&[token.as_bytes()]))
}

/// The cloud's answer to an accept: the cloud contract's `BotInviteAccepted`.
pub type Accepted = BotInviteAccepted;

/// Maps the cloud's `{error:{code,message}}` answer to what the bot should do about it.
pub fn accept_error(status: u16, body: &str) -> CliError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = parsed["error"]["code"].as_str().unwrap_or_default();
    let message = parsed["error"]["message"].as_str().unwrap_or_default();
    let say = |fallback: &str| if message.is_empty() { fallback.to_string() } else { message.to_string() };
    match (status, ErrorCode::from_code(code)) {
        (_, ErrorCode::InviteExpired) => CliError::invite(say("This invite has expired. Ask for a new one.")),
        // A bot whose access was ended and then repeats its accept hears this, and it is not about the invite.
        (_, ErrorCode::InviteRevoked) if message.contains("removed") => CliError::removed(),
        (_, ErrorCode::InviteRevoked) => CliError::invite(say("This invite was cancelled. Ask for a new one.")),
        (_, ErrorCode::InviteUsed) => CliError::invite(say("This invite was already used. Ask for a new one.")),
        (404, _) => CliError::invite(say("This invite doesn't exist.")),
        (403, _) => CliError::invite(say("This invite link isn't valid.")),
        (409, _) => CliError::invite(say("The computer can't take another bot right now.")),
        (429, _) => CliError::new(Exit::Busy, "rateLimited", say("Too many join attempts. Try again in a minute.")),
        (401, _) => CliError::network("The cloud refused this device's signature. Is this VM's clock right?"),
        (426, _) => CliError::version(),
        (s, _) => CliError::network(format!("The Kith cloud answered {s}.")),
    }
}

pub async fn accept(
    cloud: &str,
    key: &SigningKey,
    computer_id: &str,
    invite_id: &str,
    token: &str,
    name: &str,
    provider: Option<&str>,
) -> Result<Accepted, Fail> {
    let base = Url::parse(cloud).map_err(|e| Fail::Final(CliError::unexpected(format!("bad cloud address: {e}"))))?;
    let path = path::accept_bot_invite(invite_id);
    let url = base.join(&path).map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?.to_string();
    let body =
        AcceptBotInvite { token: token_proof(token), name: name.to_owned(), provider: provider.map(str::to_owned) };
    let body = serde_json::to_vec(&body).map_err(|e| Fail::Final(CliError::unexpected(e.to_string())))?;
    let sig = signature_header(key, "POST", &authority(&base), &path, &body);

    // ureq is blocking: keep it off the async threads.
    let (status, text) = tokio::task::spawn_blocking(move || -> Result<(u16, String), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(concat!("kith-guest/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut resp = agent
            .post(&url)
            .header("Kith-Sig", &sig)
            .content_type("application/json")
            .send(&body[..])
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
        Ok((status, text))
    })
    .await
    .map_err(|e| Fail::Network(e.to_string()))?
    .map_err(Fail::Network)?;

    if status != 200 {
        // 5xx and gateway errors are the network's; the rest are answers about the invite.
        return Err(match status {
            500..=599 => Fail::Network(format!("the cloud answered {status}")),
            _ => Fail::Final(accept_error(status, &text)),
        });
    }
    let accepted: Accepted =
        serde_json::from_str(&text).map_err(|_| Fail::Network("the cloud's answer was incomplete".into()))?;
    if accepted.computer_id != computer_id {
        // The invite belongs to another computer than the link says.
        return Err(Fail::Final(CliError::identity()));
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_proof_is_sha256_hex() {
        assert_eq!(token_proof("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn accept_errors_are_invite_problems_except_rate_and_network() {
        let body = |code: &str| format!(r#"{{"error":{{"code":"{code}","message":""}}}}"#);
        assert_eq!(accept_error(410, &body("inviteExpired")).exit, Exit::Invite);
        assert_eq!(accept_error(410, &body("inviteRevoked")).exit, Exit::Invite);
        assert_eq!(accept_error(409, &body("inviteUsed")).exit, Exit::Invite);
        assert_eq!(accept_error(404, &body("notFound")).exit, Exit::Invite);
        assert_eq!(accept_error(403, &body("forbidden")).exit, Exit::Invite);
        assert_eq!(accept_error(409, &body("tooMany")).exit, Exit::Invite);
        assert_eq!(accept_error(429, &body("rateLimited")).exit, Exit::Busy);
        assert_eq!(accept_error(401, &body("badSignature")).exit, Exit::Network);
        assert_eq!(accept_error(418, "not json").exit, Exit::Network);
    }
}
