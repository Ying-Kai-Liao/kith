//! The marker: a small file beside core.db saying "a core.db existed here", with the database id that core.db also keeps
//! in its meta table. A start that finds the marker but no core.db (or another id) refuses to begin empty: an empty
//! database would read as "no grants" (runtime-design section 2).

use crate::error::{Error, Result};
use crate::layout::Layout;
use crate::perms;

const HEADER: &str = "kith-core marker 1";

/// The database id the marker records, or `None` when there is no marker.
pub(crate) fn read(layout: &Layout) -> Result<Option<String>> {
    let text = match std::fs::read_to_string(&layout.marker) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let id = text
        .lines()
        .find_map(|l| l.strip_prefix("id="))
        .map(str::trim)
        .filter(|i| !i.is_empty());
    match (text.lines().next(), id) {
        (Some(HEADER), Some(id)) => Ok(Some(id.to_owned())),
        _ => Err(Error::Corrupt(format!(
            "kith-core: {} is not a kith core marker; not starting. Delete it only if you know core.db is gone for good.",
            layout.marker.display()
        ))),
    }
}

/// Writes the marker through a temp file and a rename, so a crash leaves the old one or the new one.
pub(crate) fn write(layout: &Layout, id: &str) -> Result<()> {
    let tmp = layout.marker.with_extension("marker.tmp");
    perms::create_private_file(&tmp)?;
    std::fs::write(&tmp, format!("{HEADER}\nid={id}\n"))?;
    std::fs::rename(&tmp, &layout.marker)?;
    Ok(())
}
