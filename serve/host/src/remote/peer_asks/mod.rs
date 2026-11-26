//! Asks over the Mac link (design 2026-11-24-ask-ledger §9, A5b). The core holds the rows of both ends of an ask that crosses
//! between two computers; the host carries the words, because it holds the relay channel and the pairing keys (`remote::peers`).
//!
//! - **This computer asks** (A): [`Pass`] reads what the core says the link owes (`ask.peerOwed`: asks to send, asks to follow,
//!   stops and `--to` messages to resend) and carries each to the other computer, then tells the core what came back
//!   (`ask.peerApply`, one transaction). Every step is repeatable: the rows decide what is owed, so a host that restarts mid-way,
//!   or two passes at once, send the same words again and the other computer answers from its row.
//! - **This computer is asked** (B): [`callee`] answers `peerAsk`, `peerThread`, `peerStop` and `peerTo` from the core's rows.
//! - **Pairing** ([`fence`]): every path that ends or replaces a pairing tells the core first (`ask.endPeer`), pairing tells it
//!   before it writes its own row (`ask.peerLink`), and the level-triggered reconcile finishes what a crash between the two
//!   writes left (D5, `PeerFence.tla`), all under one lock.
//!
//! The core is reached as the host (`core_front::call_host_claims`), the other computer through [`Wire`], so the whole path is
//! tested with two real cores and no relay.

pub(crate) mod callee;
pub(crate) mod fence;
mod pass;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core_front::{Answer, call_host_claims};
use crate::hub::Hub;
use kith_kernel::paths::Paths;

#[cfg(all(test, unix))]
use pass::Pass;
pub(crate) use pass::start;

/// How a call to the other computer ended when it did not answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// It cannot be reached now (off, asleep, the relay down): nothing was settled, the next pass tries again.
    Away(String),
    /// This computer has no live pairing with it at that epoch, or it says it has none: the ask is cut off.
    NotPaired,
    /// It answered, and would not take this one call (a bad parameter, "still settling"): the item stays owed and goes again
    /// next pass, and the other items for that computer go on. The computer is not away.
    Refused(String),
}

/// A boxed future, as `Wire::call` answers it.
pub(crate) type Reached<'a> = Pin<Box<dyn Future<Output = Result<Value, Reach>> + Send + 'a>>;

/// The one way this computer sends the other something: a call of a peer method (`peerAsk`, ...) under the pairing `epoch`.
pub(crate) trait Wire: Send + Sync {
    fn call<'a>(&'a self, computer: &'a str, epoch: &'a str, method: &'static str, params: Value) -> Reached<'a>;
}

/// The wire of a running host: the channel pinned at pairing (`remote::peers::call_peer_typed`).
pub(crate) struct HubWire(pub Arc<Hub>);

impl Wire for HubWire {
    fn call<'a>(&'a self, computer: &'a str, epoch: &'a str, method: &'static str, params: Value) -> Reached<'a> {
        Box::pin(async move {
            // The pairing this computer holds with it now must be the one the row was opened under: a row of an old epoch is
            // the core's to cut off (the tick does, and `ask.endPeer` did), never sent on a newer pairing.
            let Some(p) = self.0.store.peer_out(computer).filter(|p| p.epoch.as_deref() == Some(epoch)) else {
                return Err(Reach::NotPaired);
            };
            if !p.paired {
                // Granted, and the first call over the channel not made yet: the pairing is not over, only not finished.
                return Err(Reach::Away("the pairing is not finished yet".to_owned()));
            }
            super::peers::call_peer_typed(&self.0, &p, method, params).await.map_err(|e| match e {
                super::peers::PeerFail::Refused(_) => Reach::NotPaired,
                super::peers::PeerFail::Host(why) => Reach::Refused(why),
                super::peers::PeerFail::Away(why) => Reach::Away(why),
            })
        })
    }
}

/// One host call of the core, blocking. The refusal and "no core" are told apart: a refusal is the core's rules, "no core" says
/// nothing was decided.
pub(crate) fn core_call(folders: &Paths, method: &str, params: &Value) -> Answer {
    call_host_claims(folders, method, params)
}

/// [`core_call`] off the async threads, as the text of the core's answer: `Err(Said::Refused)` is the core's rules,
/// `Err(Said::Down)` is a core that did not answer.
pub(crate) async fn core(folders: &Paths, method: &'static str, params: Value) -> Result<Value, Said> {
    let folders = folders.clone();
    let answer = tokio::task::spawn_blocking(move || core_call(&folders, method, &params))
        .await
        .map_err(|e| Said::Down(e.to_string()))?;
    said(answer)
}

/// What a call of the core said, when it was not an answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Said {
    Refused(String),
    Down(String),
}

impl std::fmt::Display for Said {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(w) | Self::Down(w) => f.write_str(w),
        }
    }
}

pub(crate) fn said(answer: Answer) -> Result<Value, Said> {
    match answer {
        Answer::Done(v) => Ok(v),
        Answer::Refused(w) => Err(Said::Refused(w)),
        Answer::Down { why, .. } => Err(Said::Down(why)),
    }
}

/// The folders of this host's core, when it has one (never on Windows yet: the host does not build there).
pub(crate) fn folders_of(hub: &Hub) -> Option<Paths> {
    crate::agent::bot::writer_core(hub).map(|c| c.folders().clone())
}

/// The first twelve hex digits of the digest of `parts` joined by NUL: what names a row here for a key another computer chose.
pub(crate) fn short_hash(parts: &[&str]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update([0u8]);
        }
        h.update(p.as_bytes());
    }
    let mut hex = String::new();
    for b in h.finalize().iter().take(8) {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// A peer method's answer when the epoch named is not the live pairing: nothing else is said.
pub(crate) fn unpaired(method: &str) -> Value {
    match method {
        "peerAsk" | "peerThread" => json!({"paired": false, "found": false, "rows": []}),
        "peerStop" => json!({"paired": false}),
        _ => json!({"paired": false, "taken": false}),
    }
}

#[cfg(all(test, unix))]
mod tests;
