//! The holder's record in core.db (M3-10b3). Experimental, owner only, not in `released.json`.
//!
//! The core notes the processes the holder lists (`meta.holder_procs`: pid, start stamp, conversation) so that, when a
//! new holder instance meets an old one's record, it ends only the processes it can tell from a stranger's. A record
//! whose start cannot be told (old `ps` text from before the stamps, or a pid whose start cannot be read) is never
//! signalled and blocks the hello: `kith doctor` says the holder is `blocked`. `holder.forget` is the owner's way out:
//! it drops one recorded process from the record when the owner has seen it is not the holder's any more.

use super::build::*;

pub(super) const ROWS: &[Method] = &[row(
    "holder.forget",
    "Drops a process from the holder record in core.db (for a `blocked` holder whose old process cannot be told from a stranger). Nothing is signalled. Answers `{forgot}`: whether the pid was in the record; asked again it is false.",
    &[pos("pid", "int", true, "The recorded process's pid.")],
    "json",
    &["holder", "forget"],
    Served::Core,
    Stability::Experimental,
)
.formats(J)];
