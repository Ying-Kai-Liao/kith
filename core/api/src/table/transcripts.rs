//! M3-6 (transcripts held by the core): the engine sessions of a conversation (runtime-design section 4,
//! "Transcripts"; table `engine_sessions` in `core/runtime/src/migrations/transcripts/`). Experimental, owner only, not in
//! `released.json`; the result is `json` until the lane types it. The core answers it (a read, no state written); nothing
//! calls it yet, and `kith-sessions` stays the writer of `live/terminal/<sid>.json` until the `transcripts` module is
//! switched. `session.record` (transcripts P1) is how the `SessionStart` hook records a session after that: owner only,
//! experimental, no command line (the hook calls it through `kith kernel core-call`).

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    row(
        "session.record",
        "Records a session in the core's session registry (the `SessionStart` hook once the transcripts are the core's): in one transaction the newest record wins (order: `recordedAt` ns, then `procStart`, then the last to arrive) and `session_records`, `engine_sessions` and `conversations` are written; then `$KITH_STATE/live/terminal/<sid>.json` is written from the row (also when the record was older), and only then does the call answer. A compact keeps the row's `startedAt`. Answers `{outcome: recorded | older}`, or `{refused: invalid, why}` (the record is dropped) or `{refused: not-serving, why}` (the transcripts module is not switched: the hook keeps its spool entry).",
        &[caller(
            "record",
            "json",
            "The record `kith-sessions record` writes (`v: 1`, `sessionId`, `agent`, `cwd`, `transcriptPath`, `pid`, ...), with `recordedAt` in nanoseconds since the epoch.",
        )],
        "json",
        &[],
        Served::Core,
        Stability::Experimental,
    )
    .no_cli(),
    row(
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
.formats(J),
];
