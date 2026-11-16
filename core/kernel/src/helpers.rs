//! Where an installed kith keeps the helper programs it ships beside itself (kith-stub, kith-holder). The host and the
//! core that runs inside it both ask here, so the holder a launchd job names is the one the core connects to.

use std::path::{Path, PathBuf};

/// The helper `name` (`kith-holder`, `kith-stub`; `.exe` on Windows) for a program at `exe`: beside it with its symlinks
/// resolved (a release's `bin/` holds kith, kith-stub and kith-holder together, and install.sh links only `kith` into
/// `~/.local/bin`), else in the Mac app's CLI folder beside it (the app runs `Resources/kith-serve`; its helpers are in
/// `Resources/kith/bin/`). Only a file that can run counts.
pub fn beside(exe: &Path, name: &str) -> Option<PathBuf> {
    let exe = exe.canonicalize().ok()?;
    let dir = exe.parent()?;
    [dir.join(name), dir.join("kith/bin").join(name)]
        .into_iter()
        .find(|p| executable(p))
}

/// [`beside`] for this process's own program.
pub fn beside_this(name: &str) -> Option<PathBuf> {
    beside(&std::env::current_exe().ok()?, name)
}

/// An executable file: on Unix any exec bit, elsewhere any file.
pub fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[cfg(unix)]
    fn program(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn helpers_are_found_beside_the_real_program_and_in_the_app_bundle() {
        let t = std::env::temp_dir().join(format!("kith-helpers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        // install.sh: ~/.local/bin/kith -> cli/<version>/bin/kith, the helpers beside the link's target.
        let (cli, local) = (t.join("cli/0.1.0/bin"), t.join(".local/bin"));
        program(&cli.join("kith"), 0o755);
        std::fs::create_dir_all(&local).unwrap();
        std::os::unix::fs::symlink(cli.join("kith"), local.join("kith")).unwrap();
        assert_eq!(beside(&local.join("kith"), "kith-holder"), None);
        program(&cli.join("kith-holder"), 0o644);
        assert_eq!(
            beside(&local.join("kith"), "kith-holder"),
            None,
            "a file that cannot run is not a helper"
        );
        program(&cli.join("kith-holder"), 0o755);
        assert_eq!(
            beside(&local.join("kith"), "kith-holder"),
            Some(cli.canonicalize().unwrap().join("kith-holder"))
        );
        // The Mac app: Resources/kith-serve, Resources/kith/bin/<helper>.
        let res = t.join("Kith.app/Contents/Resources");
        program(&res.join("kith-serve"), 0o755);
        program(&res.join("kith/bin/kith-holder"), 0o755);
        assert_eq!(
            beside(&res.join("kith-serve"), "kith-holder"),
            Some(res.canonicalize().unwrap().join("kith/bin/kith-holder"))
        );
        // One beside the host wins over the bundle's.
        program(&res.join("kith-holder"), 0o755);
        assert_eq!(
            beside(&res.join("kith-serve"), "kith-holder"),
            Some(res.canonicalize().unwrap().join("kith-holder"))
        );
        let _ = std::fs::remove_dir_all(&t);
    }
}
