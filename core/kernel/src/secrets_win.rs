//! The Windows build of `secrets`: only the pieces other modules stand on (`pyjson`, the timeout helper, the portable `access` re-export). The access decision itself (auth.json with its
//! flock and modes, the password database, `kith-auth`) stays Unix-only until retire-python P3 puts it in the core; nothing
//! that spawns `kith-auth` is reachable from here, and the callers that would are refused by name where they start.

#[path = "secrets/cmd.rs"]
mod cmd;
#[path = "secrets/pyjson.rs"]
pub mod pyjson;

pub use cmd::run_command_with_timeout;

// The access rules are portable (`kith_kernel::access`); the Unix build re-exports them from here too, so a caller names one path.
pub use crate::access::*;
