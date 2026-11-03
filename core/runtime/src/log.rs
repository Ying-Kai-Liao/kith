//! The core log: one line each time a verb ran without the core because the core would not open (not when a daemon
//! holds the lock, not before the first core.db). It sits in the logs folder, beside the other logs and outside the
//! core folder, so it can be written when the core folder is the thing that is wrong. It also takes the kernel's notes
//! of a lock or write it could not do and carried on without (`kith_kernel::log`). A failure to write is ignored:
//! the verb's answer matters more than its note.

use std::io::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Error;

pub(crate) const FILE: &str = "core.log";

pub(crate) fn fell_back(logs: &Path, e: &Error) {
    // One line: the error text may carry newlines.
    let what = e.to_string().replace(['\r', '\n'], " ");
    append(logs, &format!("verb ran without the core: {what}"));
}

/// Sends what the kernel's rules note when a lock or a write fails and they carry on (`kith_kernel::log`) to this log, so
/// those failures are not silent. kith-kernel has no log of its own and depends on nothing here.
pub(crate) fn route_kernel_notes(logs: &Path) {
    let logs = logs.to_path_buf();
    kith_kernel::log::set_hook(move |what| append(&logs, &what.replace(['\r', '\n'], " ")));
}

fn append(logs: &Path, what: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = format!("unix={secs} pid={} {what}\n", std::process::id());
    let _ = std::fs::create_dir_all(logs);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(FILE))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// One line about the daemon front (it did not start, the holder is missing, ...). Same file, same rules: one line, a
/// failed write is ignored.
pub(crate) fn note(logs: &Path, what: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let what = what.replace(['\r', '\n'], " ");
    let line = format!("unix={secs} pid={} {what}\n", std::process::id());
    let _ = std::fs::create_dir_all(logs);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(FILE))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `note`, unless the last line with this prefix says the same: a condition that stays (a daemon of another build)
/// is one line, not one per command.
pub(crate) fn note_once(logs: &Path, prefix: &str, what: &str) {
    if last_note(logs, prefix).as_deref() != Some(&what.replace(['\r', '\n'], " ")[prefix.len()..])
    {
        note(logs, what);
    }
}

/// The last line `note` wrote that starts with `prefix` after its pid, for `kith doctor` ("why is there no daemon").
pub(crate) fn last_note(logs: &Path, prefix: &str) -> Option<String> {
    let text = std::fs::read_to_string(logs.join(FILE)).ok()?;
    text.lines()
        .rev()
        .find_map(|l| {
            l.split_once(" pid=")?
                .1
                .split_once(' ')?
                .1
                .strip_prefix(prefix)
        })
        .map(str::to_owned)
}
