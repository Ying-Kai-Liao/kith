//! The `kith` entry point: one binary for the host (`kith serve ...`) and the kernel's bash commands.
//!
//! `kith serve <host subcommand>` runs the host here. Everything else (`kith <agent>`, `kith doctor`,
//! `kith serve start`, ...) is handed to the kernel's bash entry with the same arguments, so it behaves
//! exactly like `hq ...` does. Run as `kith-serve` (the launchd job's binary name) the host never looks
//! for a kernel.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The kernel's command, relative to the kernel folder. The only place that names it: change it here when
/// the kernel's entry point is renamed.
pub const KERNEL_ENTRY: &str = "bin/kith";
/// What a kernel from before the rename called its entry point: accepted when there is no `KERNEL_ENTRY`.
const OLD_KERNEL_ENTRY: &str = "bin/hq";
/// The kernel folder inside a packaged install, relative to the folder holding `bin/`.
const SHIPPED_KERNEL: &str = "share/kith/kernel";
/// Env var that names a kernel folder, first in the lookup.
pub const KERNEL_ENV: &str = "KITH_KERNEL";
/// `kith serve <verb>` that the kernel handles (building, installing and configuring the host; its bash
/// wrapper then calls this binary's host subcommands). Anything else after `serve` is the host's.
const KERNEL_SERVE_VERBS: &[&str] = &["start", "stop", "restart", "status", "relay", "help", "-h", "--help"];
/// Host subcommands that the host starts itself with its own binary (MCP servers, npm installs, Claude's
/// statusLine): never the kernel's, whatever the binary is called.
/// `kith memory <verb>` that the host answers (checks and history over the memory files); `ls` and `search` stay with
/// the kernel's kith-memory, which hands these here.
const MEMORY_VERBS: &[&str] = &["check", "log", "diff", "undo", "forget", "tidy", "hook", "summary"];
const HOST_INTERNAL: &[&str] = &["mcp", "npm", "node", "statusline", "compat"];

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Run the host with these arguments (the first is the program name for usage text).
    Host(Vec<OsString>),
    /// Run the kernel's entry point with these arguments.
    Kernel(Vec<OsString>),
    /// Print the version.
    Version,
}

fn base(arg0: &OsString) -> String {
    Path::new(arg0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Decides what a command line means. `args[0]` is the name the program was started under.
pub fn route(args: &[OsString]) -> Route {
    let Some(name0) = args.first() else { return Route::Host(vec!["kith-serve".into()]) };
    let rest = &args[1..];
    let kernel = |a: &[OsString]| Route::Kernel(a.to_vec());
    match base(name0).as_str() {
        "kith" => {}
        // `hq` as a second name for the same binary.
        "hq" => return kernel(rest),
        // kith-serve, and anything else: the host, as before.
        _ => return Route::Host(args.to_vec()),
    }
    match rest.first().and_then(|a| a.to_str()) {
        Some("--version" | "-V") => Route::Version,
        Some("serve") => match rest.get(1).and_then(|a| a.to_str()) {
            None => kernel(rest),
            Some(v) if KERNEL_SERVE_VERBS.contains(&v) => kernel(rest),
            // `kith serve --port N`: the foreground host (what the background job runs).
            Some(v) if v.starts_with('-') => Route::Host(host_args(&rest[..1], &rest[1..])),
            Some(_) => Route::Host(host_args(&[], &rest[1..])),
        },
        Some("memory") if rest.get(1).and_then(|a| a.to_str()).is_some_and(|v| MEMORY_VERBS.contains(&v)) => {
            Route::Host(host_args(&[], rest))
        }
        // `kith paths` and `kith kernel ...`: answered here by the kith-kernel crate.
        Some("paths" | "kernel") => Route::Host(host_args(&[], rest)),
        Some(s) if HOST_INTERNAL.contains(&s) => Route::Host(host_args(&[], rest)),
        _ => kernel(rest),
    }
}

fn host_args(lead: &[OsString], rest: &[OsString]) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["kith serve".into()];
    v.extend(lead.iter().cloned());
    v.extend(rest.iter().cloned());
    v
}

/// The arguments that make this binary run a host subcommand: `install`, or `serve install` when it is
/// the `kith` binary. For the host starting itself.
pub fn self_args(sub: &[&str]) -> Vec<OsString> {
    let is_kith = std::env::current_exe().is_ok_and(|e| base(&e.into_os_string()) == "kith");
    let mut v: Vec<OsString> = Vec::new();
    if is_kith && !sub.first().is_some_and(|s| HOST_INTERNAL.contains(s)) {
        v.push("serve".into());
    }
    v.extend(sub.iter().map(OsString::from));
    v
}

/// The kernel's entry point in `dir`, if it is a kernel.
pub fn entry_in(dir: &Path) -> Option<PathBuf> {
    [KERNEL_ENTRY, OLD_KERNEL_ENTRY].iter().map(|e| dir.join(e)).find(|p| p.is_file())
}

fn is_kernel(dir: &Path) -> bool {
    entry_in(dir).is_some()
}

/// `KITH_<name>`, else the old `HQ_<name>`: how the host reads every setting the kernel names both ways. (`HQ_BIN` is
/// `KITH_KERNEL_BIN`: `KITH_BIN` already names the kith binary itself.)
pub fn var(name: &str) -> Option<OsString> {
    let new = if name == "BIN" { "KERNEL_BIN" } else { name };
    [format!("KITH_{new}"), format!("HQ_{name}")].iter().find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
}

/// Both spellings of a setting for a process the host starts: agents' role repos still read the HQ_ one.
pub fn both(name: &str, value: &str) -> [(String, String); 2] {
    let new = if name == "BIN" { "KERNEL_BIN" } else { name };
    [(format!("KITH_{new}"), value.to_owned()), (format!("HQ_{name}"), value.to_owned())]
}

/// The state folder, `KITH_STATE` in `kernel/lib/kith_paths.py` (the source of truth; the `kith-kernel` crate is its
/// port): `KITH_STATE`/`HQ_STATE`, else `~/.kith` once `~/.kith/.migrated` exists, else a real `~/.hq` (not migrated
/// yet), else `~/.kith`; in the old link layout (`~/.hq` links elsewhere) `<home>/state`, while the machine's own things
/// are in `~/.config/kith`: `machine_dir` is for those.
pub fn state_dir(home: &Path) -> PathBuf {
    folders(home).state
}

/// Where this Mac keeps what only it needs (`KITH_MACHINE`: auth, accounts, the host socket, live/ask): the state
/// folder on a real install, `~/.config/kith` in the old link layout.
pub fn machine_dir(home: &Path) -> PathBuf {
    folders(home).machine
}

/// All the kernel's folders for this process (`kith paths`): `KITH_HOME`/`HQ_HOME`, else `~/kith`, else `~/hq`, and
/// the rest of `kith_paths.py`'s rules. The folder around the kernel is the last fallback for the Kith folder.
pub fn folders(home: &Path) -> kith_kernel::paths::Paths {
    let kernel = current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
    kith_kernel::paths::paths(home, &kernel, &kith_kernel::paths::process_env)
}

/// The Kith folder: `KITH_HOME`/`HQ_HOME`, else `~/kith`, else `~/hq`.
pub fn home_dir(home: &Path) -> PathBuf {
    folders(home).home
}

/// Where the kernel is, in order: `$KITH_KERNEL`, a kernel shipped next to the (real, symlinks resolved)
/// binary (`<prefix>/share/kith/kernel`, or `<prefix>/kernel`; for the Mac app's `Resources/kith-serve`, the
/// release layout bundled beside it: `Resources/kith/share/kith/kernel`), then `~/kith/kernel` or `~/hq/kernel` (a checkout of the repo).
pub fn find_kernel(env: Option<OsString>, exe: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env.filter(|e| !e.is_empty()).map(PathBuf::from) {
        return is_kernel(&dir).then_some(dir);
    }
    let real = exe.and_then(|e| e.canonicalize().ok());
    // The Mac app keeps the host binary and the bundled release layout side by side.
    let bundled = real.as_deref().and_then(Path::parent).map(|d| d.join("kith").join(SHIPPED_KERNEL));
    if let Some(c) = bundled.filter(|c| is_kernel(c)) {
        return Some(c);
    }
    let prefix = real.and_then(|e| e.parent()?.parent().map(Path::to_path_buf));
    if let Some(prefix) = prefix {
        for c in [prefix.join(SHIPPED_KERNEL), prefix.join("kernel")] {
            if is_kernel(&c) {
                return Some(c);
            }
        }
    }
    let home = home?;
    ["kith/kernel", "hq/kernel"].iter().map(|c| home.join(c)).find(|c| is_kernel(c))
}

/// The kernel this process would run, by `find_kernel`'s rules for this binary and this machine.
pub fn current_kernel() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    find_kernel(std::env::var_os(KERNEL_ENV), exe.as_deref(), dirs::home_dir().as_deref())
}

/// Replaces this process with the kernel's entry point. Only returns on failure.
#[cfg(unix)]
pub fn exec_kernel(args: &[OsString]) -> anyhow::Error {
    use std::os::unix::process::CommandExt as _;
    let exe = std::env::current_exe().ok();
    let Some(kernel) = current_kernel() else {
        return anyhow::anyhow!(
            "kith: no kernel found. Set {KERNEL_ENV}, install a packaged kith (it ships share/kith/kernel), or clone kith to ~/kith"
        );
    };
    let entry = entry_in(&kernel).unwrap_or_else(|| kernel.join(KERNEL_ENTRY));
    let mut cmd = std::process::Command::new(&entry);
    cmd.args(args);
    // So the kernel's kith-serve installs this very binary on a packaged install, without cargo.
    if let Some(e) = exe.and_then(|e| e.canonicalize().ok()) {
        cmd.env("KITH_BIN", e);
    }
    anyhow::Error::new(cmd.exec()).context(format!("running {}", entry.display()))
}

#[cfg(not(unix))]
pub fn exec_kernel(_args: &[OsString]) -> anyhow::Error {
    anyhow::anyhow!("kith: the kernel's commands run on macOS and Linux only")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn kith_routes() {
        assert_eq!(route(&v(&["/x/kith", "--version"])), Route::Version);
        assert_eq!(route(&v(&["kith", "doctor"])), Route::Kernel(v(&["doctor"])));
        assert_eq!(route(&v(&["kith", "coder", "-p", "x"])), Route::Kernel(v(&["coder", "-p", "x"])));
        assert_eq!(
            route(&v(&["kith", "serve", "start", "--no-build"])),
            Route::Kernel(v(&["serve", "start", "--no-build"]))
        );
        assert_eq!(route(&v(&["kith", "serve"])), Route::Kernel(v(&["serve"])));
        assert_eq!(
            route(&v(&["kith", "serve", "install", "--port", "1"])),
            Route::Host(v(&["kith serve", "install", "--port", "1"]))
        );
        assert_eq!(
            route(&v(&["kith", "serve", "--port", "1"])),
            Route::Host(v(&["kith serve", "serve", "--port", "1"]))
        );
        assert_eq!(
            route(&v(&["kith", "statusline", "--", "x"])),
            Route::Host(v(&["kith serve", "statusline", "--", "x"]))
        );
        assert_eq!(route(&v(&["hq", "ls"])), Route::Kernel(v(&["ls"])));
        assert_eq!(route(&v(&["kith", "memory", "search", "x"])), Route::Kernel(v(&["memory", "search", "x"])));
        assert_eq!(route(&v(&["kith", "memory", "log", "f"])), Route::Host(v(&["kith serve", "memory", "log", "f"])));
        assert_eq!(route(&v(&["kith", "paths", "--json"])), Route::Host(v(&["kith serve", "paths", "--json"])));
        assert_eq!(route(&v(&["kith", "kernel", "version"])), Route::Host(v(&["kith serve", "kernel", "version"])));
    }

    #[test]
    fn machine_folder_follows_the_migration_marker() {
        let t = std::env::temp_dir().join(format!("kith-machine-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (new, old) = (t.join(".kith"), t.join(".hq"));
        // A Mac that has only kith serve's data in ~/.kith and its real state in ~/.hq: not migrated.
        std::fs::create_dir_all(new.join("serve")).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(machine_dir(&t), old);
        // `kith migrate` writes the marker (and links ~/.hq to ~/.kith).
        std::fs::write(new.join(".migrated"), "").unwrap();
        assert_eq!(machine_dir(&t), new);
        std::fs::remove_file(new.join(".migrated")).unwrap();
        std::fs::remove_dir(&old).unwrap();
        // ~/.hq a link to ~/.kith, or no ~/.hq at all: ~/.kith.
        std::os::unix::fs::symlink(&new, &old).unwrap();
        assert_eq!(machine_dir(&t), new);
        std::fs::remove_file(&old).unwrap();
        assert_eq!(machine_dir(&t), new);
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// The host's three readers of the state folder (`machine_dir`, the memory `Layout`, the crate) never disagree.
    #[test]
    fn host_readers_agree_with_the_crate() {
        let t = std::env::temp_dir().join(format!("kith-agree-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (new, old) = (t.join(".kith"), t.join(".hq"));
        let check = |want: &Path| {
            assert_eq!(state_dir(&t), want);
            assert_eq!(folders(&t).state, want);
            assert_eq!(crate::hqmem::Layout::for_home(&t).state, want);
        };
        // ~/.kith exists (kith serve made it) and ~/.hq is the real folder, no marker: ~/.hq. Layout used to say ~/.kith.
        std::fs::create_dir_all(new.join("serve")).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        check(&old);
        // Migrated while a real ~/.hq is still there: ~/.kith.
        std::fs::write(new.join(".migrated"), "").unwrap();
        check(&new);
        assert_eq!(machine_dir(&t), new);
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Old link layout (`~/.hq` links elsewhere): the state is `<home>/state`, the machine's own things `~/.config/kith`.
    #[test]
    fn machine_is_not_state_in_the_old_link_layout() {
        let t = std::env::temp_dir().join(format!("kith-oldlink-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("disk/kernel")).unwrap();
        std::os::unix::fs::symlink(t.join("disk"), t.join(".hq")).unwrap();
        assert_eq!(machine_dir(&t), t.join(".config/kith"));
        assert_eq!(state_dir(&t).file_name().unwrap(), "state");
        assert_eq!(crate::hqmem::Layout::for_home(&t).state, state_dir(&t));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn kith_home_is_preferred_over_hq() {
        let t = std::env::temp_dir().join(format!("kith-home-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("hq")).unwrap();
        assert_eq!(home_dir(&t), t.join("hq"));
        std::fs::create_dir_all(t.join("kith")).unwrap();
        assert_eq!(home_dir(&t), t.join("kith"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn both_spellings_of_a_setting() {
        assert_eq!(
            both("VIA", "serve"),
            [("KITH_VIA".to_owned(), "serve".to_owned()), ("HQ_VIA".to_owned(), "serve".to_owned())]
        );
        // KITH_BIN is the kith binary itself, so the kernel's command has its own name.
        assert_eq!(both("BIN", "/k")[0].0, "KITH_KERNEL_BIN");
        assert_eq!(both("BIN", "/k")[1].0, "HQ_BIN");
    }

    fn fake_kernel(dir: &Path) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(KERNEL_ENTRY), "").unwrap();
    }

    fn fake_exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn finds_the_kernel_in_each_layout() {
        let t = std::env::temp_dir().join(format!("kith-find-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let t = t.canonicalize().unwrap();
        // Release install: bin/kith with share/kith/kernel beside it.
        let rel = t.join("rel");
        fake_exe(&rel.join("bin/kith"));
        fake_kernel(&rel.join(SHIPPED_KERNEL));
        assert_eq!(find_kernel(None, Some(&rel.join("bin/kith")), None), Some(rel.join(SHIPPED_KERNEL)));
        // Mac app: Resources/kith-serve with the release layout in Resources/kith.
        let res = t.join("App.app/Contents/Resources");
        fake_exe(&res.join("kith-serve"));
        fake_kernel(&res.join("kith").join(SHIPPED_KERNEL));
        assert_eq!(find_kernel(None, Some(&res.join("kith-serve")), None), Some(res.join("kith").join(SHIPPED_KERNEL)));
        // A checkout under the home folder, migrated or not.
        fake_kernel(&t.join("home/hq/kernel"));
        assert_eq!(find_kernel(None, None, Some(&t.join("home"))), Some(t.join("home/hq/kernel")));
        fake_kernel(&t.join("home/kith/kernel"));
        assert_eq!(find_kernel(None, None, Some(&t.join("home"))), Some(t.join("home/kith/kernel")));
        // $KITH_KERNEL wins over everything.
        let given = t.join("given");
        fake_kernel(&given);
        assert_eq!(find_kernel(Some(given.clone().into()), Some(&rel.join("bin/kith")), None), Some(given));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn kith_serve_is_the_host() {
        assert_eq!(route(&v(&["/h/kith-serve", "install"])), Route::Host(v(&["/h/kith-serve", "install"])));
    }

    #[test]
    fn finds_kernel_in_order() {
        let t = std::env::temp_dir().join(format!("kith-kernel-test-{}", std::process::id()));
        let mk = |p: &Path| {
            std::fs::create_dir_all(p.join("bin")).unwrap();
            std::fs::write(p.join(KERNEL_ENTRY), "").unwrap();
        };
        let (shipped, checkout, envk) = (t.join("p/share/kith/kernel"), t.join("home/kith/kernel"), t.join("e"));
        mk(&checkout);
        std::fs::create_dir_all(t.join("p/bin")).unwrap();
        std::fs::write(t.join("p/bin/kith"), "").unwrap();
        let exe = t.join("p/bin/kith");
        let home = t.join("home");
        assert_eq!(find_kernel(None, Some(&exe), Some(&home)), Some(checkout));
        mk(&shipped);
        assert_eq!(
            find_kernel(None, Some(&exe), Some(&home)).map(|p| p.canonicalize().unwrap()),
            Some(shipped.canonicalize().unwrap())
        );
        mk(&envk);
        assert_eq!(find_kernel(Some(envk.clone().into()), Some(&exe), Some(&home)), Some(envk));
        assert_eq!(find_kernel(Some(t.join("nope").into()), Some(&exe), Some(&home)), None);
        std::fs::remove_dir_all(&t).unwrap();
    }
}
