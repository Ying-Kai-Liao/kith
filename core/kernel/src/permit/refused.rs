//! What Windows says to the guest and bug halves of `kith permit`, by name (windows-owner WI1, WI6). Each leans on something
//! Windows has no counterpart of yet: the launch's guard rules (`apply`), a Python helper (`report_bug`), a sandbox
//! (`run_door`, P3-12) or a guest's own run (`ask_helper`). Guests are refused on a Windows host, so none of them has a caller
//! there today; if one appears it gets a plain refusal, never a quiet skip.

use super::{Env, Out};
use serde_json::{Map, Value};
use std::path::Path;

/// The refusal of `report_bug` (P4-17: its draft and redaction are a Python helper).
pub const BUG: &str = "not on Windows yet: bug reports (P4-17)";
/// The refusal of `run_door` (P3-12: a door needs a sandbox, and guests are refused on Windows).
pub const DOOR: &str = "not on Windows yet: doors (P3-12)";
/// The refusal of `ask_helper` and `apply`: they serve a guest's run, and guests are refused on a Windows host (WI6).
pub const GUEST: &str = "not on Windows: guests are refused on a Windows host (windows-owner WI6)";

pub fn report_bug(_: &Map<String, Value>, _: &Env, _: &Path) -> String {
    format!("Could not prepare the bug report ({BUG}); nothing was filed.")
}

pub fn run_door(_: &Map<String, Value>, _: &Env, _: &Path) -> String {
    format!("The door did not run: {DOOR}")
}

pub fn ask_helper(_: &Map<String, Value>, _: &Env, _: &Path) -> String {
    format!("Could not ask the helper: {GUEST}.")
}

/// `kith permit apply SETTINGS_FILE`: a guest launch's own step. Exit 1 and a line, and the settings file is left as it was.
pub fn apply() -> Out {
    Out {
        stderr: format!("kith permit apply: {GUEST}\n"),
        code: 1,
        ..Out::default()
    }
}
