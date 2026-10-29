//! Exit codes and the one error type every command ends in (docs/kith-bot-guests.md section 9.4).

use std::fmt;

use kith_wire::rpc::BotErrorCode as Code;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    Ok = 0,
    /// Unexpected error.
    Unexpected = 1,
    /// Bad flags, empty or oversized text.
    Usage = 2,
    /// No config: run `kith join`.
    NotJoined = 3,
    /// Invalid, expired, revoked or already used invite.
    Invite = 4,
    /// Joined, but the owner has not allowed the bot yet.
    Pending = 5,
    /// Bot revoked or denied, or the agent isn't granted.
    Removed = 6,
    /// Busy or rate limited; also `heldByOtherWriter` and `--no-queue` with a running turn.
    Busy = 7,
    /// The turn waits for a tool approval from the owner and `--timeout` ran out.
    Approval = 8,
    /// No answer in time.
    Timeout = 9,
    /// The computer is offline.
    Offline = 10,
    /// The cloud or the network is unreachable.
    Network = 11,
    /// The computer's identity doesn't match the pinned key.
    Identity = 12,
    /// Incompatible versions: update `kith`.
    Version = 13,
    /// Ctrl-C, as a shell reports it.
    Interrupted = 130,
    /// SIGTERM, as a shell reports it.
    Terminated = 143,
}

impl Exit {
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// A failure with its exit code, a stable machine string for `--json`, and the words for a person.
#[derive(Clone, Debug)]
pub struct CliError {
    pub exit: Exit,
    pub code: &'static str,
    pub message: String,
}

impl CliError {
    pub fn new(exit: Exit, code: &'static str, message: impl Into<String>) -> Self {
        Self { exit, code, message: message.into() }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Exit::Usage, "usage", message)
    }

    pub fn unexpected(message: impl Into<String>) -> Self {
        Self::new(Exit::Unexpected, "error", message)
    }

    pub fn not_joined() -> Self {
        Self::new(Exit::NotJoined, "notJoined", "Not joined to a computer. Run `kith join <link>` first.")
    }

    pub fn invite(message: impl Into<String>) -> Self {
        Self::new(Exit::Invite, "invite", message)
    }

    pub fn removed() -> Self {
        Self::new(Exit::Removed, "revoked", "Your access to this computer was removed")
    }

    pub fn offline() -> Self {
        Self::new(Exit::Offline, "hostOffline", "The computer is offline.")
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(Exit::Network, "unreachable", message)
    }

    pub fn identity() -> Self {
        Self::new(
            Exit::Identity,
            "identityChanged",
            "This computer's identity doesn't match the key pinned when you joined, so kith won't connect to it.",
        )
    }

    pub fn version() -> Self {
        Self::new(Exit::Version, "upgrade", "This kith and the computer are not compatible. Update kith.")
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(Exit::Timeout, "timeout", message)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// An error answer from the host (`{id, err:{status, message, code?}}`) or the cloud, before it is mapped to an exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostErr {
    pub status: u16,
    pub code: Option<String>,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl HostErr {
    /// The default mapping. Callers that know more (an invite proof, an accept) remap before using it. The host's `code`
    /// is the contract's `BotErrorCode`; one this version doesn't know (or none) falls back to the HTTP status.
    pub fn into_cli(self) -> CliError {
        let Self { status, code, message, retry_after } = self;
        let retry = retry_after.map(|s| format!(" (retry in {s} s)")).unwrap_or_default();
        let code =
            code.map_or(Code::Unknown, |c| serde_json::from_value(serde_json::Value::String(c)).unwrap_or_default());
        match code {
            Code::Pending => {
                CliError::new(Exit::Pending, "pending", "Waiting for the computer's owner to allow this bot")
            }
            Code::NotGranted => {
                CliError::new(Exit::Removed, "notGranted", non_empty(&message, "That agent isn't granted to this bot"))
            }
            Code::RateLimited => {
                CliError::new(Exit::Busy, "rateLimited", format!("{}{retry}", non_empty(&message, "Rate limited")))
            }
            Code::Busy => CliError::new(
                Exit::Busy,
                "busy",
                format!("{}{retry}", non_empty(&message, "That conversation is busy")),
            ),
            Code::HeldByOtherWriter => CliError::new(
                Exit::Busy,
                "heldByOtherWriter",
                non_empty(&message, "Someone took over this conversation in the Kith app"),
            ),
            Code::NotFound => CliError::new(Exit::Usage, "notFound", non_empty(&message, "Not found")),
            Code::BadRequest | Code::TooLong | Code::NotSupported => {
                CliError::new(Exit::Usage, "badRequest", non_empty(&message, "The computer refused the request"))
            }
            Code::NonceReused => CliError::new(
                Exit::Usage,
                "nonceReused",
                non_empty(&message, "This clientNonce was already used for a different message."),
            ),
            // The rest say no more than their status does. No wildcard: a new code in the contract is decided on here.
            Code::Syncing | Code::TooMany | Code::Unknown => match status {
                401 | 403 => CliError::new(
                    Exit::Removed,
                    "forbidden",
                    non_empty(&message, "Your access to this computer was removed"),
                ),
                404 => CliError::new(Exit::Usage, "notFound", non_empty(&message, "Not found")),
                400 => {
                    CliError::new(Exit::Usage, "badRequest", non_empty(&message, "The computer refused the request"))
                }
                409 | 429 => CliError::new(Exit::Busy, "busy", format!("{}{retry}", non_empty(&message, "Busy"))),
                426 => CliError::version(),
                _ => CliError::unexpected(non_empty(&message, &format!("The computer answered with error {status}"))),
            },
        }
    }
}

fn non_empty(message: &str, fallback: &str) -> String {
    if message.trim().is_empty() { fallback.to_string() } else { message.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(status: u16, code: Option<&str>) -> HostErr {
        HostErr { status, code: code.map(str::to_string), message: String::new(), retry_after: None }
    }

    #[test]
    fn codes_are_the_documented_numbers() {
        let table = [
            (Exit::Ok, 0),
            (Exit::Unexpected, 1),
            (Exit::Usage, 2),
            (Exit::NotJoined, 3),
            (Exit::Invite, 4),
            (Exit::Pending, 5),
            (Exit::Removed, 6),
            (Exit::Busy, 7),
            (Exit::Approval, 8),
            (Exit::Timeout, 9),
            (Exit::Offline, 10),
            (Exit::Network, 11),
            (Exit::Identity, 12),
            (Exit::Version, 13),
        ];
        for (exit, code) in table {
            assert_eq!(exit.code(), code);
        }
    }

    #[test]
    fn host_codes_map_to_exits() {
        assert_eq!(host(403, Some("pending")).into_cli().exit, Exit::Pending);
        assert_eq!(host(403, Some("notGranted")).into_cli().exit, Exit::Removed);
        assert_eq!(host(429, Some("rateLimited")).into_cli().exit, Exit::Busy);
        assert_eq!(host(409, Some("busy")).into_cli().exit, Exit::Busy);
        assert_eq!(host(409, Some("heldByOtherWriter")).into_cli().exit, Exit::Busy);
        assert_eq!(host(404, Some("notFound")).into_cli().exit, Exit::Usage);
        assert_eq!(host(400, Some("tooLong")).into_cli().exit, Exit::Usage);
        // A code this version doesn't know reads as its status.
        assert_eq!(host(426, Some("someNewCode")).into_cli().exit, Exit::Version);
        assert_eq!(host(503, Some("syncing")).into_cli().exit, Exit::Unexpected);
    }

    #[test]
    fn uncoded_statuses_fall_back_to_the_status() {
        assert_eq!(host(404, None).into_cli().exit, Exit::Usage);
        assert_eq!(host(403, None).into_cli().exit, Exit::Removed);
        assert_eq!(host(429, None).into_cli().exit, Exit::Busy);
        assert_eq!(host(426, None).into_cli().exit, Exit::Version);
        assert_eq!(host(500, None).into_cli().exit, Exit::Unexpected);
    }

    #[test]
    fn retry_after_goes_in_the_message() {
        let e = HostErr {
            status: 429,
            code: Some("rateLimited".into()),
            message: "Too many asks".into(),
            retry_after: Some(30),
        };
        assert_eq!(e.into_cli().message, "Too many asks (retry in 30 s)");
    }
}
