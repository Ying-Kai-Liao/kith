use crate::Error;
use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Creates the folder 0700 if absent. An existing one of ours is tightened to 0700; someone else's is refused.
pub fn private_folder(path: &Path) -> Result<(), Error> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let meta = std::fs::metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(Error::Folder(format!(
            "{} belongs to uid {}, not this user",
            path.display(),
            meta.uid()
        )));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn file_private(open: &mut OpenOptions) {
    open.mode(0o600);
}
