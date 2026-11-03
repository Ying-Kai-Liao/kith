//! The kernel's rules in Rust, one module at a time. The CLI (`kith`) and the host both link this crate, so the
//! two answer the same question the same way. The Python kernel keeps its own copies while it moves; the
//! differential harness (`kernel/tests/difftest`) fails when they differ.

pub mod accounts;
pub mod delegation;
pub mod engines;
// Unix only: these write modes with mode(2), sync directories, send signals and read `ps`. The Windows build of this crate
// (the `windows` CI job) is accounts, delegation, engines and paths; the core runtime needs paths.
#[cfg(unix)]
pub mod launch;
pub mod paths;
#[cfg(unix)]
pub mod secrets;
#[cfg(unix)]
pub mod threads;

/// The version of the `kith kernel` interface the router in `kernel/bin/kith` speaks to.
pub const ABI: u32 = 1;

/// The modules this build has, as `kith kernel version` lists them. A verb in `kernel/lib/core-switched.txt` moves to
/// the core only when its module is here (`acp`: the `run-once` verb, whose code lives in the kith-acp crate; `launch`: `kith AGENT --launch-json`).
pub const MODULES: &[&str] = if cfg!(unix) {
    &[
        "accounts",
        "acp",
        "delegation",
        "engines",
        "launch",
        "paths",
        "threads",
    ]
} else {
    &["accounts", "acp", "delegation", "engines", "paths"]
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
            r#"{"abi":1,"modules":["accounts","acp","delegation","engines","launch","paths","threads"]}"#
        );
    }
}
