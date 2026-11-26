//! The kernel's rules in Rust, one module at a time. The CLI (`kith`) and the host both link this crate, so the
//! two answer the same question the same way. The Python kernel keeps its own copies while it moves; the
//! differential harness (`kernel/tests/difftest`) fails when they differ.

pub mod access;
pub mod accounts;
// Not Unix-only: `kith ask` on a Mac with no Python (a Windows owner) is this thin client of the core's ask rows; what needs a Unix
// program (a callee started from a bash entry, ssh) says so by name.
pub mod ask;
// On Windows the ledger (tag, record, sync) and the reports of one agent run; the verbs that ask `kith-auth` for people
// (`people`, `members`, `bots`) are refused by name until retire-python P3-1.
pub mod cost;
pub mod delegation;
pub mod engines;
pub mod helpers;
pub mod hostlocal;
// On Windows the launch is the plan and its settings (`launch::assemble`, what the host launches in process); the one-shot run
// (`launch::exec`, `launch::start`) stands on claims and `kith-permit` and stays Unix only until W12. A guest's launch is
// refused there (guests are not offered on Windows).
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
// has just the pieces the other modules stand on (`pyjson`, the timeout helper, the portable `access` re-export): see `secrets_win.rs`.
#[cfg(unix)]
pub mod secrets;
#[cfg(not(unix))]
#[path = "secrets_win.rs"]
pub mod secrets;
pub mod settings;
#[cfg(unix)]
pub mod threads;
// Not Unix-only: `kith wake` on a Mac with no Python (a Windows owner) is this thin client of the core's wake rows.
pub mod wake;

/// What every guest and bot-guest entry point says on Windows (Kai 2026-11-27: the guest sandbox is parked). One text, so a
/// refusal reads the same wherever it stops them; the refusals are `cfg(windows)` and the Mac never reads it.
pub const GUESTS_NOT_ON_WINDOWS: &str = "Guests aren't on Windows yet (Kai 2026-11-27: the guest sandbox is parked). Run guests on a Mac.";

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
        // `kernel ask`: `kith ask` as a client of the core's ask rows (the ledger). A token of its own, not a wider `asks`: `kith-ask`
        // forwards here only on a program that lists it and a Mac whose `asks` module is switched.
        "ask-ledger",
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
        // `kernel launch-exec` also runs an interactive terminal session and `--held` (no task): the entry sends those only to a
        // program that lists this one, so one that knows just the one-shots never receives one.
        "launch-exec-term",
        "lifecycle",
        "paths",
        "permit",
        "pkg",
        "project",
        // `kernel project-id` answers a guest run (the run-socket client, project/guest.rs); the stub bin/project-id needs it.
        "project-guest",
        "readfence",
        "room-ledger",
        // `kernel launch AGENT --exec --kernel K --turn N --claim C`: the core's turn of a cron job, its spec on stdin; the launch attaches
        // the claim before the engine. Listed only by a program whose `launch --exec` takes the flags (the host checks it).
        "sched-turn",
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
        // `KITH_TURN_CONVERSATION` beside the claim: the launch attaches it naming the turn's conversation (C3). A door sets the variable only for a program that lists it.
        "turn-conversation",
        // `kernel wake VERB ...`: `kith wake` as a client of the core's wake rows, for a Mac whose queue is the core's (the tombstone
        // in sessions/queue/). `kith_wake_exec` (lib/kith-core.sh) uses it only on a program that lists it, else kith-wake answers.
        "wake",
    ]
} else {
    &[
        "account-state",
        "accounts",
        "accounts-json",
        "accounts-main-id",
        "accounts-run-chain",
        "acp",
        "ask-ledger",
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
        "wake",
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
            r#"{"abi":1,"modules":["account","account-state","accounts","accounts-json","accounts-main-id","accounts-run-chain","acp","ask-ledger","cost","cost-ledger","delegation","engine-run","engine-run-tui","engines","launch","launch-exec","launch-exec-term","lifecycle","paths","permit","pkg","project","project-guest","readfence","room-ledger","sched-turn","secrets-exec","settings","threads","threads-run","threads-writer","turn-claim","turn-conversation","wake"]}"#
        );
    }
}
