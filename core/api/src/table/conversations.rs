//! M3 (conversations): the owner's view of conversations and their writer claims (runtime-design section 4; tables in
//! `core/runtime/src/migrations/conversations/`). Experimental, owner only, not in `released.json`; the result is `json`
//! until the lane types it. Nothing serves these yet (`Core::call` answers NotServed, the caller falls back): M3-1 only
//! reserves the area so the later M3 lanes add their rows here without touching a shared list. Rows for the wake queue,
//! children and accounts go in their own area files when those lanes land.

use super::build::*;

pub(super) const ROWS: &[Method] = &[
    conversation(
        "conversation.list",
        "The conversations, newest first, with the engine, the account and whether a writer holds each.",
        &[],
        &["conversations", "list"],
    ),
    conversation(
        "conversation.show",
        "One conversation in full: its engine session pointer, the transcript hash, its live claim and its queued or parked turns. Refuses an id that is no conversation.",
        &[pos("id", "string", true, "The conversation.")],
        &["conversations", "show"],
    ),
    conversation(
        "claim.list",
        "The writer claims: the live ones (queued, claimed, running) and the recent ended ones, as `{live, recent}`.",
        &[],
        &["claims", "list"],
    ),
];

const fn conversation(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        path,
        Served::Core,
        Stability::Experimental,
    )
    .formats(J)
}
