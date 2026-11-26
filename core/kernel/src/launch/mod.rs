//! What a launch (`kith AGENT --launch-json`) assembles, one piece at a time: the system prompt first.

// The ports keep the shape of their Python and bash originals (one line each way) so a reader can hold them side by side.
#![allow(
    clippy::assigning_clones,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::format_push_string,
    clippy::implicit_hasher
)]

pub mod args;
pub mod assemble;
// Unix only until W12: the one-shot run takes the session's claim (`threads`), runs `kith-permit` and `exec`s the engine.
#[cfg(unix)]
pub mod exec;
pub mod facets;
pub mod guard;
// Unix only: it takes the session's claim through `threads` (W12).
#[cfg(unix)]
pub mod held;
pub mod json;
pub mod judge;
pub mod machine;
pub mod mcp;
pub mod plan;
pub mod prompt;
#[allow(clippy::many_single_char_names)]
pub mod pyport;
pub mod room;
pub mod runenv;
// Unix only, like `exec`, which is its one caller: it attaches the host's claim for a turn (`threads`) and holds fds.
#[cfg(unix)]
pub mod sched_turn;
pub mod session;
pub mod settings;
#[cfg(unix)]
pub mod start;
pub mod winenv;
// Windows: a run's launcher starts its engine inside a kill-on-close job it holds itself, in one call (the Unix `start` has `exec`).
#[cfg(windows)]
pub mod winstart;
// Unix only, with `exec` and `start` it stands on: a terminal run holds the session's claim (W12).
#[cfg(unix)]
pub mod terminal;

// The fixtures below are Unix trees (`/usr/local/bin/kith`, `/kt/...`, modes, symlinks); the Windows ones are `win_tests`.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::many_single_char_names)]
mod tests;

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::many_single_char_names)]
mod settings_tests;

// The settings of one owner agent on this machine against the Mac's, and the Windows rules (WI7).
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod win_tests;

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod start_tests;

// The golden's /kt/... paths are not absolute on Windows.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod judge_tests;
