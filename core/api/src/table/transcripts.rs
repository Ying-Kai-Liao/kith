//! M3-6 (transcripts held by the core): the engine sessions of a conversation (runtime-design section 4,
//! "Transcripts"; table `engine_sessions` in `core/runtime/src/migrations/transcripts/`). Experimental, owner only, not in
//! `released.json`; the result is `json` until the lane types it. The core answers it (a read, no state written); nothing
//! calls it yet, and `kith-sessions` stays the writer of `live/terminal/<sid>.json`.

use super::build::*;

pub(super) const ROWS: &[Method] = &[row(
    "conversation.transcript",
    "The engine sessions a conversation has, one per account: the engine, its session id, the pointer to where the engine keeps it, the hash and size as of the last read, and whether the engine's copy is there now and has changed since. Refuses an id that is no conversation. Only Claude's sessions are held; another engine's row says `held: false`.",
    &[
        pos("id", "string", true, "The conversation."),
        flag("account", "string", &["--account"], "Only this account's session (`claude`, `claude:work`)."),
    ],
    "json",
    &["conversations", "transcript"],
    Served::Core,
    Stability::Experimental,
)
.formats(J)];
