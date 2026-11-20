//! Bot names: a bot created without a name is called [`PLACEHOLDER`]. The host no longer names a bot from its
//! conversations (that ran a one-shot of the bot's harness, which only host-native bots had, C5); the user, or the
//! bot through `profile`, renames it.

use serde::{Deserialize, Serialize};

/// A finished user ↔ bot exchange.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub user: String,
    pub agent: String,
}

pub const PLACEHOLDER: &str = "New Bot";
