//! Owner-only folders, one way on every system. On Unix the folder is made 0700; on Windows it gets an access list with no
//! entry for anyone but the owner, SYSTEM and Administrators, set and read natively by SID through `kith-winsys`
//! (docs/plans/windows-owner.md WI4). A folder that cannot be made private is not made: nothing is left with the default
//! list.

use std::io;
use std::path::Path;

/// Makes one folder (its parent must exist), owner-only. An existing folder is `AlreadyExists`, as `mkdir` says: whoever made
/// it decided its list, and [`check_dir`] is how a caller finds out whether that was private.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(dir)?;
        if let Err(e) = kith_winsys::acl::restrict(dir, true) {
            // Fail closed: a folder with the inherited list is not left behind for the next caller to trust.
            let _ = std::fs::remove_dir(dir);
            return Err(io::Error::other(format!(
                "can't make {} private: {e}",
                dir.display()
            )));
        }
        Ok(())
    }
}

/// Refuses a folder that others can open, with the command that fixes it. Unix code has never judged the mode of a folder it
/// found, so there this is nothing; Windows refuses a list that lets anyone else in.
#[cfg_attr(unix, allow(clippy::unnecessary_wraps))]
pub fn check_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let _ = dir;
    #[cfg(not(unix))]
    {
        use kith_winsys::acl::{self, Check};
        match acl::check(dir)? {
            Check::OwnerOnly => {}
            Check::Open(why) => {
                // The fix names the owner by SID (`*S-1-5-...`), which icacls takes in any language.
                let fix = acl::current_user_text().map_or_else(
                    |_| String::new(),
                    |sid| {
                        format!(
                            ": icacls \"{}\" /inheritance:r /grant:r \"*{sid}:(OI)(CI)F\"",
                            dir.display()
                        )
                    },
                );
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} is open to other accounts ({why}); kith won't use it until it is private{fix}",
                        dir.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}
