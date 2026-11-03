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
const MEMORY_VERBS: &[&str] = &["check", "log", "diff", "undo", "rm", "forget", "tidy", "hook", "summary"];
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
    // The machine folder is found without a kernel (`folders` asks `current_kernel` for its last fallback, which
    // would loop back here), as `folders` itself does when there is none.
    let machine = dirs::home_dir()
        .map(|h| kith_kernel::paths::paths(&h, &h.join("kith/kernel"), &kith_kernel::paths::process_env).machine);
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

/// The conversation lock's launcher the kernel's `kith <agent>` starts claude under (`kernel/bin/kith`, `KITH_STUB_BIN`).
/// A run the host starts has an empty environment, so the host names it: beside this program (a service copy in
/// `~/.kith/serve/bin`, a checkout's target folder), in the Mac app's bundle (`Resources/kith-serve` with
/// `Resources/kith/bin/kith-stub`), or beside the real path of the `kith` on PATH (`install.sh` links only `kith`).
pub const STUB_ENV: &str = "KITH_STUB_BIN";

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// [`STUB_ENV`]'s program for a host at `exe` with `kith` first on PATH, symlinks resolved before looking beside either.
pub fn find_stub(exe: Option<&Path>, kith: Option<&Path>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "kith-stub.exe" } else { "kith-stub" };
    let beside = exe.and_then(|e| e.canonicalize().ok()).and_then(|e| e.parent().map(Path::to_path_buf));
    let from_kith = kith.and_then(|k| k.canonicalize().ok()).and_then(|k| k.parent().map(Path::to_path_buf));
    let mut places: Vec<PathBuf> = Vec::new();
    if let Some(dir) = beside {
        places.push(dir.join(name));
        places.push(dir.join("kith/bin").join(name)); // the Mac app: Resources/kith-serve, Resources/kith/bin/kith-stub
    }
    places.extend(from_kith.map(|d| d.join(name)));
    places.into_iter().find(|p| executable(p))
}

/// `(KITH_STUB_BIN, program)` for a process the host starts, resolved once: an explicit `KITH_STUB_BIN` that runs wins.
/// None when there is no stub (the kernel then warns and runs unlocked, or refuses under KITH_REQUIRE_STUB=1).
pub fn stub_env() -> Option<(String, String)> {
    // Unit tests compare the environments of the runs they see; `find_stub` has its own test.
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    stub_env_found()
}

#[cfg(not(test))]
fn stub_env_found() -> Option<(String, String)> {
    static STUB: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let found = STUB.get_or_init(|| {
        if let Some(given) = std::env::var_os(STUB_ENV).map(PathBuf::from).filter(|p| executable(p)) {
            return Some(given);
        }
        find_stub(std::env::current_exe().ok().as_deref(), crate::agent::backends::which("kith").as_deref())
    });
    found.as_ref().map(|p| (STUB_ENV.to_owned(), p.to_string_lossy().into_owned()))
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

    fn stub_at(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn the_stub_is_found_beside_the_host_in_the_app_bundle_and_through_the_kith_link() {
        let t = std::env::temp_dir().join(format!("kith-stub-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        // The app: Resources/kith-serve (the service runs it), Resources/kith/bin/{kith,kith-stub}.
        let res = t.join("Kith.app/Contents/Resources");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("kith-serve"), "").unwrap();
        stub_at(&res.join("kith/bin/kith-stub"));
        assert_eq!(
            find_stub(Some(&res.join("kith-serve")), None),
            Some(res.join("kith/bin/kith-stub").canonicalize().unwrap())
        );
        // A service copy with the stub beside it wins over the bundle layout.
        let svc = t.join("serve/bin");
        std::fs::create_dir_all(&svc).unwrap();
        std::fs::write(svc.join("kith-serve"), "").unwrap();
        assert_eq!(find_stub(Some(&svc.join("kith-serve")), None), None, "nothing beside it, nothing on PATH");
        stub_at(&svc.join("kith-stub"));
        assert_eq!(find_stub(Some(&svc.join("kith-serve")), None), Some(svc.join("kith-stub").canonicalize().unwrap()));
        // install.sh links only `kith` into ~/.local/bin; the stub is beside the link's target.
        #[cfg(unix)]
        {
            let (cli, local) = (t.join("cli/0.1.0/bin"), t.join(".local/bin"));
            std::fs::create_dir_all(&local).unwrap();
            std::fs::write(t.join("lone-serve"), "").unwrap();
            stub_at(&cli.join("kith-stub"));
            std::fs::write(cli.join("kith"), "").unwrap();
            std::os::unix::fs::symlink(cli.join("kith"), local.join("kith")).unwrap();
            assert_eq!(
                find_stub(Some(&t.join("lone-serve")), Some(&local.join("kith"))),
                Some(cli.join("kith-stub").canonicalize().unwrap())
            );
            // A stub that is not executable is not a stub.
            std::fs::set_permissions(
                cli.join("kith-stub"),
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
            )
            .unwrap();
            assert_eq!(find_stub(Some(&t.join("lone-serve")), Some(&local.join("kith"))), None);
        }
        let _ = std::fs::remove_dir_all(&t);
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
