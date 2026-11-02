//! What a launch (`kith AGENT --launch-json`) assembles, one piece at a time: the system prompt first.

// The ports keep the shape of their Python and bash originals (one line each way) so a reader can hold them side by side.
#![allow(
    clippy::assigning_clones,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::format_push_string,
    clippy::implicit_hasher
)]

pub mod facets;
pub mod prompt;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::many_single_char_names)]
mod tests;
