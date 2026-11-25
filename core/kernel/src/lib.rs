//! The kernel's rules in Rust, one module at a time. The CLI (`kith`) and the host both link this crate, so the
//! two answer the same question the same way. The Python kernel keeps its own copies while it moves; the
//! differential harness (`kernel/tests/difftest`) fails when they differ.

pub mod accounts;
// On Windows the ledger (tag, record, sync) and the reports of one agent run; the verbs that ask `kith-auth` for people
// (`people`, `members`, `bots`) are refused by name until retire-python P3-1.
pub mod cost;
pub mod delegation;
pub mod engines;
pub mod helpers;
// Unix only: these write modes with mode(2), sync directories, send signals and read `ps`. The Windows build of this crate
// (the `windows` CI job) is the modules in the Windows list of `MODULES`; the core runtime needs paths.
#[cfg(unix)]
pub mod launch;
// Unix only: it forks the compaction (a parked child) and takes flocks, like `threads`, whose claims it takes.
#[cfg(unix)]
pub mod lifecycle;
pub mod log;
pub mod ownerfs;
pub mod paths;
// Unix only, like `cost` and `secrets`: it runs `kith-auth` through the secrets module's timeout helper.
#[cfg(unix)]
pub mod people;
pub mod permit;
pub mod pkg;
pub mod procstart;
pub mod project;
// Unix only in the module list below: the hook is written into a unix launch's settings.
pub mod readfence;
pub mod room_ledger;
pub mod schedule;
// Unix only, whole: the access decision runs `kith-auth`, reads the password database and writes auth.json with modes. Windows
// has just the pieces the other modules stand on (`pyjson`, the timeout helper): see `secrets_win.rs`.
#[cfg(unix)]
pub mod secrets;
#[cfg(not(unix))]
#[path = "secrets_win.rs"]
pub mod secrets;
pub mod settings;
#[cfg(unix)]
pub mod threads;

/// The version of the `kith kernel` interface the router in `kernel/bin/kith` speaks to.
pub const ABI: u32 = 1;

/// The modules this build has, as `kith kernel version` lists them. A verb in `kernel/lib/core-switched.txt` moves to
/// the core only when its module is here (`threads-writer`: every `kith-sessions` verb but `run`, in Files and in Writer mode; `threads-run`: that `run`; the older `threads` is the Files-mode port and is not what `kernel/bin/kith` routes to; `acp`: the `run-once` verb, whose code lives in the kith-acp crate; `engine-run`: the non-Claude one-shot `kernel/bin/kith` runs through it (`kith kernel engine-run`); `engine-run-tui`: its `--interactive`, the engine's own terminal session; `launch`: `kith AGENT --launch-json`; `lifecycle`: `kith-lifecycle`, the sweep that compacts or ends sessions nobody runs).
pub const MODULES: &[&str] = if cfg!(unix) {
    &[
        "account",
        "account-state",
        "accounts",
        "accounts-json",
        "accounts-main-id",
        "accounts-run-chain",
        "acp",
        "cost",
        "cost-ledger",
        "delegation",
        "engine-run",
        "engine-run-tui",
        "engines",
        "launch",
        // `kernel launch-exec`: a one-shot run in this process (the entry's one-shot branch). A token of its own: an older
        // program that lists `launch` must not be sent `--exec`.
        "launch-exec",
        "lifecycle",
        "paths",
        "permit",
        "pkg",
        "project",
        // `kernel project-id` answers a guest run (the run-socket client, project/guest.rs); the stub bin/project-id needs it.
        "project-guest",
        "readfence",
        "room-ledger",
        // `kernel secrets exec` (start a shared MCP server with its keys) and `kernel secrets mcp-config`: `mcp-config` names
        // this program as the server's command only when it lists the token, else `bin/kith-auth exec` (P1-15).
        "secrets-exec",
        "settings",
        "threads",
        // `kernel threads run AGENT -- CMD...`: the wrapper an interactive `kith AGENT` ends in (`kith-sessions run`); the router
        // uses it only on a program that lists it, else the Python `run` stays.
        "threads-run",
        "threads-writer",
        // `KITH_TURN_CLAIM`: the launch attaches the host's claim for its turn (`claim.attach`) before anything starts. The
        // router hands the variable only to a program that lists it (else exit 10); the host checks it before setting it.
        "turn-claim",
    ]
} else {
    &[
        "account-state",
        "accounts",
        "accounts-json",
        "accounts-main-id",
        "accounts-run-chain",
        "acp",
        // The ledger's writers; `cost` itself (the per-person views) waits for P3-1, which takes `kith-auth` out of them.
        "cost-ledger",
        "delegation",
        "engine-run",
        "engine-run-tui",
        "engines",
        "paths",
        // Owner runs only: the hooks, `issue`, and the MCP server's `approve` and key/access cards, over the host's named pipe.
        // The guest halves (apply, door, helper, bug) are refused by name.
        "permit",
        "pkg",
        "project",
        "room-ledger",
        "settings",
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
            r#"{"abi":1,"modules":["account","account-state","accounts","accounts-json","accounts-main-id","accounts-run-chain","acp","cost","cost-ledger","delegation","engine-run","engine-run-tui","engines","launch","launch-exec","lifecycle","paths","permit","pkg","project","project-guest","readfence","room-ledger","secrets-exec","settings","threads","threads-run","threads-writer","turn-claim"]}"#
        );
    }
}
