//! `kith kernel lifecycle <verb>`: the sweep that compacts or ends sessions nobody runs (`kernel/bin/kith-lifecycle`) from the
//! `kith-kernel` crate. Like `threads_cmd`, this is the one place that asks a running host (`resolveThread`) and the core
//! (`claim.*` in Writer mode), because the crate never calls either itself; it reuses that module's loopback client.

use std::io::Write as _;
use std::path::PathBuf;

use kith_kernel::lifecycle::cli;
use kith_kernel::paths::{process_env, process_home};
use kith_kernel::threads::Ctx;

use crate::kernel;
use crate::threads_cmd::Loopback;

/// `kith kernel lifecycle <verb> ...`; returns the exit code.
pub fn run(kernel_dir: Option<PathBuf>, args: &[String]) -> i32 {
    let home = process_home();
    let kernel_dir = kernel_dir.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    // `KITH_MACHINE` first, as the Python reads it (where kith-stub's locks are), else where the paths put it.
    let machine = process_env("KITH_MACHINE")
        .filter(|m| !m.is_empty())
        .map_or_else(|| kith_kernel::paths::paths(&home, &kernel_dir, &process_env).machine, PathBuf::from);
    let ctx = Ctx {
        state: process_env("KITH_STATE").map(PathBuf::from),
        machine: Some(machine),
        home: home.clone(),
        env: &process_env,
    };
    let host = Loopback { kernel: kernel_dir, home };
    let out = cli::run(&ctx, &host, Some(&host), std::env::current_exe().ok(), args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}
