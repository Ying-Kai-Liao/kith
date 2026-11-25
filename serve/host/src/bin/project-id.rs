//! `project-id`: the program twin of the sh stub `kernel/bin/project-id`, for Windows (a stub is a script there, and nothing
//! kith starts may be one) and equally runnable elsewhere. Agents run `project-id` by name; this answers it the way the stub
//! does, as `kith kernel project-id`: the same arguments, stdout, stderr and exit codes, through `kith_kernel::project`
//! (a guest run asks the core over its run socket there, never the registry).
//!
//! The one thing the stub does that this does not is look for a kith program: there is none to find, this is it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The kernel folder, for the state folder's last fallback: `KITH_KERNEL`, else the one this program ships in
/// (`<release>/bin/project-id` with `<release>/share/kith/kernel`), else a checkout's under the home folder.
fn kernel_folder(home: &Path) -> PathBuf {
    if let Some(k) = ["KITH_KERNEL", "HQ_KERNEL"].iter().find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty())) {
        return PathBuf::from(k);
    }
    let shipped = std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .and_then(|e| e.parent()?.parent().map(|r| r.join("share/kith/kernel")))
        .filter(|k| k.is_dir());
    shipped.unwrap_or_else(|| home.join("kith/kernel"))
}

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let home = kith_kernel::paths::process_home();
    let kernel = kernel_folder(&home);
    let ctx = kith_kernel::project::Ctx {
        state: kith_kernel::paths::paths(&home, &kernel, &kith_kernel::paths::process_env).state,
        home,
        cwd: std::env::current_dir().unwrap_or_default(),
        guest: kith_kernel::project::Guest::from_env(|n| std::env::var_os(n)),
    };
    let out = kith_kernel::project::run(&ctx, &args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    std::process::exit(out.code);
}
