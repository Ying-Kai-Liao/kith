//! `kith kernel pkg <verb>`: `kernel/bin/kith-pkg resolve` (the pinned package folder a run starts from) from the `kith-kernel`
//! crate. Not routed from `kith` yet: the launch still asks the Python (`KITH_CORE=python` is the rollback either way).

use std::io::Write as _;
use std::path::PathBuf;

use kith_kernel::pkg::{Ctx, cli};

use crate::kernel;

/// `kith kernel pkg [--kernel DIR] <verb> ...`; returns the exit code.
pub fn run(kernel_dir: Option<PathBuf>, args: &[String]) -> i32 {
    let home = kith_kernel::paths::process_home();
    let kernel_dir = kernel_dir.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let ctx = Ctx::from_process(&kernel_dir);
    let out = cli::run(&ctx, args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}
