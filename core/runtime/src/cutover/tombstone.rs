//! The tombstone: a module's source file (auth.json, state.json ...) replaced by a small JSON note, so an old Python
//! reader that opens it finds out at once that the data moved, instead of reading nothing and carrying on. The reader
//! side is `kernel/lib/kith_tombstone.py` (same format: a JSON object with `"kith_tombstone": 1`).
//!
//! The original is not lost: it is hard-linked (else copied) to `<name>.pre-cutover-<unix time>` first, with its mode.
//! The rollback's export-back writes the real file again over the tombstone.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{Error, Result};

fn note_of(path: &Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|v| v.get("kith_tombstone").is_some())
}

pub(crate) fn is_tombstone(path: &Path) -> bool {
    note_of(path).is_some()
}

/// The module a tombstone at `path` names (`None`: not a tombstone).
pub(crate) fn module_of(path: &Path) -> Option<String> {
    note_of(path).map(|v| {
        v.get("module")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    })
}

fn note(module: &str) -> String {
    let v = json!({
        "kith_tombstone": 1,
        "module": module,
        "message": format!("this data moved to the Kith core (module {module}); the old reader must not use this file"),
        "rollback": format!("kith rollback {module}"),
    });
    format!("{v}\n")
}

/// Puts the tombstone at `path`. Idempotent: a path that already holds one is left alone. Returns the line to report.
pub(crate) fn ensure(path: &Path, module: &str, now: i64) -> Result<String> {
    if module_of(path).as_deref() == Some(module) {
        return Ok(format!("tombstone already at {}", path.display()));
    }
    let io = |e: std::io::Error| Error::Io(format!("cutover: {}: {e}", path.display()));
    let tmp = path.with_extension("tombstone.tmp");
    std::fs::write(&tmp, note(module)).map_err(io)?;
    crate::perms::make_private(&tmp)?;
    let kept = if path.exists() {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".pre-cutover-{now}"));
        let kept: PathBuf = path.with_file_name(name);
        if std::fs::hard_link(path, &kept).is_err() {
            std::fs::copy(path, &kept).map_err(io)?;
        }
        Some(kept)
    } else {
        None
    };
    std::fs::rename(&tmp, path).map_err(io)?;
    Ok(match kept {
        Some(k) => format!(
            "tombstoned {} (the original is kept at {})",
            path.display(),
            k.display()
        ),
        None => format!("tombstoned {} (it did not exist)", path.display()),
    })
}
