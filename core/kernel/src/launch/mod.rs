//! What a launch (`kith AGENT --launch-json`) assembles, one piece at a time: the system prompt first.

// The ports keep the shape of their Python and bash originals (one line each way) so a reader can hold them side by side.
#![allow(
    clippy::assigning_clones,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::format_push_string,
    clippy::implicit_hasher
)]

pub mod assemble;
pub mod facets;
pub mod guard;
pub mod json;
pub mod judge;
pub mod machine;
pub mod mcp;
pub mod plan;
pub mod prompt;
#[allow(clippy::many_single_char_names)]
pub mod pyport;
pub mod room;
pub mod settings;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::many_single_char_names)]
mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::many_single_char_names)]
mod settings_tests;

// The golden's /kt/... paths are not absolute on Windows.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod judge_tests;
