//! The kernel's rules in Rust, one module at a time. The CLI (`kith`) and the host both link this crate, so the
//! two answer the same question the same way. The Python kernel keeps its own copies while it moves; the
//! differential harness (`kernel/tests/difftest`) fails when they differ.

pub mod accounts;
// Unix only: it runs `kith-auth` through the secrets module's timeout helper.
#[cfg(unix)]
pub mod cost;
pub mod delegation;
pub mod engines;
// Unix only: these write modes with mode(2), sync directories, send signals and read `ps`. The Windows build of this crate
// (the `windows` CI job) is accounts, delegation, engines and paths; the core runtime needs paths.
#[cfg(unix)]
pub mod launch;
pub mod log;
pub mod paths;
// Unix only, like `cost` and `secrets`: it runs `kith-auth` through the secrets module's timeout helper.
#[cfg(unix)]
pub mod people;
// Unix only: `resolve` clones with cp, sets modes and asks `kill(2)` whether a run is alive.
#[cfg(unix)]
pub mod pkg;
// Unix only: the run socket is a Unix socket and the one-shots are modes.
#[cfg(unix)]
pub mod permit;
pub mod procstart;
// Unix only in the module list below: the hook is written into a unix launch's settings.
pub mod readfence;
// Unix only: it writes projects.json through `secrets::pyjson`.
#[cfg(unix)]
pub mod project;
pub mod schedule;
#[cfg(unix)]
pub mod secrets;
#[cfg(unix)]
pub mod settings;
#[cfg(unix)]
pub mod threads;

/// The version of the `kith kernel` interface the router in `kernel/bin/kith` speaks to.
pub const ABI: u32 = 1;

/// The modules this build has, as `kith kernel version` lists them. A verb in `kernel/lib/core-switched.txt` moves to
/// the core only when its module is here (`acp`: the `run-once` verb, whose code lives in the kith-acp crate; `launch`: `kith AGENT --launch-json`).
pub const MODULES: &[&str] = if cfg!(unix) {
    &[
        "account-state",
        "accounts",
        "acp",
        "delegation",
        "engines",
        "launch",
        "paths",
        "pkg",
        "project",
        "readfence",
        "settings",
        "threads",
    ]
} else {
    &[
        "account-state",
        "accounts",
        "acp",
        "delegation",
        "engines",
        "paths",
    ]
};

/// What `kith kernel version` prints.
pub fn version_json() -> String {
    let modules: Vec<String> = MODULES.iter().map(|m| format!("\"{m}\"")).collect();
    format!("{{\"abi\":{ABI},\"modules\":[{}]}}", modules.join(","))
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(unix)]
    fn version_lists_the_modules() {
        assert_eq!(
            super::version_json(),
            r#"{"abi":1,"modules":["account-state","accounts","acp","delegation","engines","launch","paths","pkg","project","readfence","settings","threads"]}"#
        );
    }
}
