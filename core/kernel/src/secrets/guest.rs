//! `kith_paths.in_guest_folders` and `guest_scrub` for `exec`: a server started for a guest runs outside their sandbox, so
//! nothing in its environment, and not its working folder, may lead into a folder the guest writes (a planted npx package,
//! Chrome, `.npmrc` or `node_modules` would run as the owner). The guest's writable folders are their files folder of any
//! agent (`<agents>/<agent>/files/people/<id>`), their Claude Code temp folder, and the folders their membership was granted
//! to write.

use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::session::Env;
use crate::launch::prompt;
use crate::paths::{Paths, realpath};

/// `str.lower` on macOS, whose volumes fold case; nothing elsewhere.
fn fold(s: &str) -> String {
    if cfg!(target_os = "macos") {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// The folders one person's runs write in.
pub(super) struct Folders {
    /// The real path of the agents folder, folded.
    agents: String,
    /// The person's id, folded.
    pid: String,
    /// Their temp folder and the folders granted for writing: real paths, folded.
    roots: Vec<String>,
}

impl Folders {
    /// `extra`: the folders granted for writing (`access.write` of the membership row).
    pub fn new(paths: &Paths, pid: &str, extra: &[String]) -> Self {
        let tmp = guest_tmp(paths, pid);
        let roots = tmp
            .into_iter()
            .chain(extra.iter().filter(|r| !r.is_empty()).cloned())
            .map(|r| fold(&realpath(Path::new(&r)).to_string_lossy()))
            .collect();
        Folders {
            agents: fold(&realpath(&paths.agents).to_string_lossy()),
            pid: fold(pid),
            roots,
        }
    }

    /// `in_guest_folders(path, pid, extra)`: whether `path` is, by real path, in a folder the person writes in.
    pub fn contains(&self, path: &str) -> bool {
        let real = fold(&realpath(Path::new(path)).to_string_lossy());
        if let Ok(rest) = Path::new(&real).strip_prefix(&self.agents) {
            let parts: Vec<String> = rest
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.len() >= 4
                && parts[1] == "files"
                && parts[2] == "people"
                && parts[3] == self.pid
            {
                return true;
            }
        }
        self.roots
            .iter()
            .any(|r| real == *r || real.starts_with(&format!("{}/", r.trim_end_matches('/'))))
    }

    /// `guest_scrub(env, pid, extra)`: the environment without any variable that names a path in these folders (any entry of
    /// a list such as PATH counts).
    pub fn scrub(&self, env: &Env) -> Env {
        env.iter()
            .filter(|(_, v)| !v.split(':').any(|x| x.starts_with('/') && self.contains(x)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// `guest_tmp(pid)`: Claude Code's own temp folder for the person's runs, as `kith_paths.py` decides it.
fn guest_tmp(paths: &Paths, pid: &str) -> Option<String> {
    let hash = |x: &str| {
        use std::fmt::Write as _;
        Sha256::digest(x.as_bytes())
            .iter()
            .fold(String::new(), |mut a, b| {
                let _ = write!(a, "{b:02x}");
                a
            })
    };
    prompt::guest_tmp(
        &realpath(&paths.machine),
        pid,
        crate::threads::os::uid(),
        &hash,
    )
    .map(|d| d.to_string_lossy().into_owned())
}
