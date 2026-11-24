//! Where a host-native bot's memory files lived. The host no longer runs native bots (C5), so nothing reads, scans or
//! writes these files. They stay on disk, untouched, and `exportHostBot` copies them (`api::native_export`).

use std::path::PathBuf;

/// A layer's folder: the shared one (`None`), or a bot's own.
pub fn folder(bot: Option<&str>) -> PathBuf {
    let data = crate::service::data_dir();
    match bot {
        Some(id) => data.join("bots").join(id).join("bot-memory"),
        None => data.join("shared-memory"),
    }
}
