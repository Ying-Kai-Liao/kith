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
const MEMORY_VERBS: &[&str] = &["check", "log", "diff", "undo", "forget", "tidy", "hook"];
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

/// This Mac's Kith folder, by the kernel's rules (`kernel/lib/kith_paths.py`, which is the source of truth):
/// `KITH_STATE`/`HQ_STATE`, else `~/.kith` once `~/.kith/.migrated` exists, else a real `~/.hq` (not migrated yet),
/// else `~/.kith`.
pub fn machine_dir(home: &Path) -> PathBuf {
    if let Some(given) = var("STATE") {
        return PathBuf::from(given);
    }
    let (new, old) = (home.join(".kith"), home.join(".hq"));
    if new.join(".migrated").exists() {
        return new;
    }
    match std::fs::symlink_metadata(&old) {
        Ok(m) if m.is_dir() => old,
        Ok(m) if m.file_type().is_symlink() && old.is_dir() && new.is_dir() => {
            if old.canonicalize().ok() == new.canonicalize().ok() { new } else { old }
        }
        Ok(m) if m.file_type().is_symlink() && old.is_dir() => old,
        _ => new,
    }
}

/// The Kith folder: `KITH_HOME`/`HQ_HOME`, else `~/kith`, else `~/hq`.
pub fn home_dir(home: &Path) -> PathBuf {
    if let Some(given) = var("HOME") {
        return PathBuf::from(given);
    }
    let new = home.join("kith");
    if new.is_dir() { new } else { home.join("hq") }
}

/// Where the kernel is, in order: `$KITH_KERNEL` (a developer's checkout; the only way to run one), the installed CLI
/// (`<machine>/cli/current/share/kith/kernel`, the release the installer links), the old flat install
/// (`<machine>/share/kith/kernel`), then a kernel shipped next to the (real, symlinks resolved) binary
/// (`<prefix>/share/kith/kernel`: Homebrew and other packaged installs). No checkout is ever found by itself, and
/// the Mac app's own copy is not looked at: the app installs the CLI.
pub fn find_kernel(env: Option<OsString>, exe: Option<&Path>, machine: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = env.filter(|e| !e.is_empty()).map(PathBuf::from) {
        return is_kernel(&dir).then_some(dir);
    }
    let installed =
        machine.into_iter().flat_map(|m| [m.join("cli/current").join(SHIPPED_KERNEL), m.join(SHIPPED_KERNEL)]);
    let prefix =
        exe.and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent()?.parent().map(|p| p.join(SHIPPED_KERNEL)));
    installed.chain(prefix).find(|c| is_kernel(c))
}

/// The kernel this process would run, by `find_kernel`'s rules for this binary and this machine. Looked up on every
/// call: `kith update` flips `cli/current` while the host runs, and the next run must use the new release.
pub fn current_kernel() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let machine = dirs::home_dir().map(|h| machine_dir(&h));
    find_kernel(std::env::var_os(KERNEL_ENV), exe.as_deref(), machine.as_deref())
}

/// The oldest kith CLI this host runs agents on: this host's own major.minor. The one place that names it.
pub const MIN_CLI: &str = concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"), ".0");

/// Which kith CLI the host runs agents on, and whether it may (`cli` in `compat` and `hello`).
#[derive(Debug, PartialEq, Eq)]
pub struct Cli {
    /// The kernel's `VERSION`; none for a checkout or a kernel from before the file.
    pub version: Option<String>,
    pub kernel: Option<PathBuf>,
    /// `$KITH_KERNEL` is set: a developer's kernel, never gated.
    pub dev: bool,
    pub ok: bool,
}

fn triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().split(['-', '+']).next()?.split('.').map(|n| n.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Judges the kernel `find_kernel` gives against `MIN_CLI`.
pub fn cli_status(env: Option<OsString>, exe: Option<&Path>, machine: Option<&Path>) -> Cli {
    let dev = env.as_ref().is_some_and(|e| !e.is_empty());
    let kernel = find_kernel(env, exe, machine);
    let version = kernel
        .as_ref()
        .and_then(|k| std::fs::read_to_string(k.join("VERSION")).ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let new_enough = version.as_deref().and_then(triple).zip(triple(MIN_CLI)).is_some_and(|(v, min)| v >= min);
    Cli { ok: kernel.is_some() && (dev || new_enough), version, kernel, dev }
}

impl Cli {
    /// What the host tells apps: `{version, kernel, dev, min, ok}`.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": self.version,
            "kernel": self.kernel.as_ref().map(|k| k.to_string_lossy()),
            "dev": self.dev,
            "min": MIN_CLI,
            "ok": self.ok,
        })
    }

    /// Why agent runs are refused, when they are.
    pub fn refusal(&self) -> Option<String> {
        let found = if self.kernel.is_none() { "none" } else { self.version.as_deref().unwrap_or("none") };
        (!self.ok).then(|| {
            format!(
                "Kith needs the kith CLI {MIN_CLI} or newer (found {found}). Run `kith update`, or update the Mac app."
            )
        })
    }
}

/// [`cli_status`] for this process and this machine.
pub fn current_cli() -> Cli {
    let exe = std::env::current_exe().ok();
    let machine = dirs::home_dir().map(|h| machine_dir(&h));
    cli_status(std::env::var_os(KERNEL_ENV), exe.as_deref(), machine.as_deref())
}

/// Replaces this process with the kernel's entry point. Only returns on failure.
#[cfg(unix)]
pub fn exec_kernel(args: &[OsString]) -> anyhow::Error {
    use std::os::unix::process::CommandExt as _;
    let exe = std::env::current_exe().ok();
    let Some(kernel) = current_kernel() else {
        return anyhow::anyhow!(
            "kith: no kernel found. Install the CLI (curl -fsSL https://kithbot.app/install.sh | sh), or set {KERNEL_ENV} to a checkout's kernel folder"
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
    fn kith_serve_is_the_host() {
        assert_eq!(route(&v(&["/h/kith-serve", "install"])), Route::Host(v(&["/h/kith-serve", "install"])));
    }

    fn temp(name: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("kith-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t.canonicalize().unwrap()
    }

    #[test]
    fn finds_kernel_in_order() {
        let t = temp("kernel-order");
        let (m, exe) = (t.join("m"), t.join("p/bin/kith"));
        fake_exe(&exe);
        // A checkout under the home folder is never found by itself.
        fake_kernel(&t.join("home/kith/kernel"));
        assert_eq!(find_kernel(None, Some(&exe), Some(&m)), None);
        // Beside the binary (Homebrew), not `<prefix>/kernel`.
        fake_kernel(&t.join("p/kernel"));
        assert_eq!(find_kernel(None, Some(&exe), Some(&m)), None);
        let shipped = t.join("p").join(SHIPPED_KERNEL);
        fake_kernel(&shipped);
        assert_eq!(find_kernel(None, Some(&exe), Some(&m)), Some(shipped));
        // Old flat install wins over the packaged one, the installed CLI over both.
        let flat = m.join(SHIPPED_KERNEL);
        fake_kernel(&flat);
        assert_eq!(find_kernel(None, Some(&exe), Some(&m)), Some(flat));
        let cli = m.join("cli/current").join(SHIPPED_KERNEL);
        fake_kernel(&cli);
        assert_eq!(find_kernel(None, Some(&exe), Some(&m)), Some(cli));
        // $KITH_KERNEL wins over everything; a dir that is no kernel finds nothing.
        let envk = t.join("e");
        fake_kernel(&envk);
        assert_eq!(find_kernel(Some(envk.clone().into()), Some(&exe), Some(&m)), Some(envk));
        assert_eq!(find_kernel(Some(t.join("nope").into()), Some(&exe), Some(&m)), None);
        // The Mac app's own copy is not looked at.
        let app = t.join("App.app/Contents/Resources");
        fake_exe(&app.join("kith-serve"));
        fake_kernel(&app.join("kith").join(SHIPPED_KERNEL));
        assert_eq!(find_kernel(None, Some(&app.join("kith-serve")), None), None);
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn the_gate_judges_the_found_kernel() {
        let t = temp("gate");
        let m = t.join("m");
        let kernel = m.join("cli/current").join(SHIPPED_KERNEL);
        // No CLI at all.
        let none = cli_status(None, None, Some(&m));
        assert!(!none.ok && none.version.is_none() && !none.dev);
        assert!(none.refusal().unwrap().contains("found none"));
        // An old install without VERSION is too old.
        fake_kernel(&kernel);
        let old = cli_status(None, None, Some(&m));
        assert!(!old.ok && old.kernel.is_some());
        assert!(old.refusal().unwrap().contains(&format!("CLI {MIN_CLI} or newer (found none)")));
        // Below, at, and above the minimum.
        let version = |v: &str| {
            std::fs::write(kernel.join("VERSION"), format!("{v}\n")).unwrap();
            cli_status(None, None, Some(&m))
        };
        assert!(!version("0.0.9").ok);
        assert!(version(MIN_CLI).ok);
        assert!(version("99.0.0").ok && version("99.0.0").refusal().is_none());
        assert!(!version("garbage").ok);
        assert_eq!(version("99.1.2-rc1").version.as_deref(), Some("99.1.2-rc1"));
        // A developer's kernel is not gated, and says so.
        let checkout = t.join("checkout/kernel");
        fake_kernel(&checkout);
        let dev = cli_status(Some(checkout.clone().into()), None, Some(&m));
        assert!(dev.ok && dev.dev && dev.version.is_none());
        assert_eq!(dev.json()["dev"], true);
        // KITH_KERNEL naming no kernel reports none, and refuses.
        let bad = cli_status(Some(t.join("nope").into()), None, Some(&m));
        assert!(bad.dev && !bad.ok && bad.kernel.is_none());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(triple("0.10.0") > triple("0.9.9"));
        assert_eq!(triple("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(triple("1.2"), None);
    }
}
