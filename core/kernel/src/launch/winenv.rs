//! The environment of a process on Windows, as the launch reads it and as a run inherits it (WI1: a Windows branch does the work
//! or refuses by name). The names are matched without regard to case, which is how Windows keeps them (`Path`, `SystemRoot`).

use super::assemble::Env;

/// The variables a program on Windows needs to work at all, and none that carry a login: where the profile and its folders
/// are, the system folder (Winsock and the crypto libraries fail to load without `SystemRoot`), the shell and the extensions
/// a name is looked up with, the temp folders, and the program folders Git for Windows is found by. A run on the Mac gets the
/// POSIX counterparts (`HOME`, `PATH`, `TMPDIR`).
const BASICS: [&str; 17] = [
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "SystemRoot",
    "windir",
    "PATHEXT",
    "ComSpec",
    "TEMP",
    "TMP",
    "HOMEDRIVE",
    "HOMEPATH",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "USERNAME",
    "OS",
];

/// Whether a run inherits the host's `name` on Windows (the list above), whatever its case.
pub fn windows_basics(name: &str) -> bool {
    BASICS.iter().any(|b| b.eq_ignore_ascii_case(name))
}

/// This process's environment as a map: the plan reads `PATH` by that name, and Windows may have stored it as `Path`. The
/// hidden per-drive entries Windows keeps (`=C:`) are not variables and are left out.
pub fn process_env() -> Env {
    let mut env = Env::new();
    for (k, v) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with('=') {
            continue;
        }
        let k = if cfg!(windows) && k.eq_ignore_ascii_case("PATH") {
            "PATH".to_owned()
        } else {
            k
        };
        env.insert(k, v.to_string_lossy().into_owned());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_basics_are_the_profile_the_system_and_the_lookup_and_nothing_with_a_login() {
        for name in [
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "SystemRoot",
            "PATHEXT",
            "ComSpec",
            "TEMP",
        ] {
            assert!(windows_basics(name), "{name}");
        }
        // Windows keeps names without regard to case.
        assert!(windows_basics("SYSTEMROOT"));
        assert!(windows_basics("comspec"));
        for name in [
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
            "KITH_RUN_TOKEN",
            "PATH",
            "HOME",
        ] {
            assert!(!windows_basics(name), "{name}");
        }
    }

    #[test]
    fn the_process_environment_has_no_hidden_drive_entries() {
        assert!(process_env().keys().all(|k| !k.starts_with('=')));
    }
}
