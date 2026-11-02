//! Which coding harnesses this machine has, and how to drive each over ACP.
//!
//! Detection works like Orca's preflight: hydrate PATH from the user's login
//! shell (GUI apps and launchd get a bare PATH), add the usual install dirs
//! (Homebrew, ~/.local/bin, nvm, bun, volta, asdf, mise, pnpm…), then look for
//! each known CLI. Launching prefers the installed CLI's own ACP mode and falls
//! back to the official ACP registry build (npx / uvx / downloaded binary).

use crate::LockExt;
use crate::agent::{acp, node, registry};
use crate::shell;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The table lives in the kernel crate (`kith_kernel::engines`), shared with the `kith` program. The host lists the
/// rows that speak ACP; the kernel's other engines (hermes, dsh) run from the CLI only.
pub use kith_kernel::engines::{Engine as Harness, Install};

/// Every engine the phone can pick.
pub fn harnesses() -> impl Iterator<Item = &'static Harness> {
    kith_kernel::engines::ENGINES.iter().filter(|h| h.has_acp())
}

/// The command that installs `install` here: an npm package goes through Kith's own Node when the computer has none.
pub fn install_command(install: Install) -> Option<String> {
    install.command(cfg!(unix), &|pkg| {
        crate::service::current_exe()
            .ok()
            .map(|exe| format!("{} npm install -g {pkg}", shell::quote(&exe.to_string_lossy())))
    })
}

/// By id only: a stored bot's backend is never an alias.
pub fn harness(id: &str) -> Option<&'static Harness> {
    harnesses().find(|h| h.id == id)
}

// MARK: PATH hydration

static SEARCH_PATH: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// `…/v22.1.0` → `[22, 1, 0]` (unparseable parts count as 0).
fn node_version(dir: &Path) -> Vec<u32> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect())
        .unwrap_or_default()
}

fn well_known_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return vec![] };
    let mut dirs: Vec<PathBuf> =
        ["/opt/homebrew/bin", "/usr/local/bin", "/home/linuxbrew/.linuxbrew/bin", "/usr/bin", "/bin"]
            .iter()
            .map(PathBuf::from)
            .collect();
    if cfg!(windows) {
        // Global npm CLIs (%APPDATA%\npm), and Node itself.
        dirs.extend(dirs::data_dir().map(|d| d.join("npm")));
        dirs.extend(std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join("nodejs")));
        // oh-my-pi's binary install (%LOCALAPPDATA%\omp); a Bun install lands in ~/.bun/bin below.
        dirs.extend(dirs::data_local_dir().map(|d| d.join("omp")));
    }
    for rel in [
        ".local/bin",
        ".bun/bin",
        ".cargo/bin",
        ".volta/bin",
        ".asdf/shims",
        ".local/share/mise/shims",
        ".fnm/aliases/default/bin",
        ".local/share/pnpm",
        "Library/pnpm",
        ".npm-global/bin",
        ".opencode/bin",
        ".deno/bin",
        "go/bin",
    ] {
        dirs.push(home.join(rel));
    }
    // Every nvm-installed node, newest first (global npm CLIs live next to node).
    if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut versions: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        // Numeric, not lexicographic: v22.1.0 is newer than v9.11.2.
        versions.sort_by_key(|p| std::cmp::Reverse(node_version(p)));
        dirs.extend(versions.into_iter().map(|p| p.join("bin")));
    }
    dirs
}

/// Re-detects the search path and exports it as this process's PATH, so every
/// agent we spawn (and the `npx`/`node` they need) resolves the same way.
pub fn hydrate_path() {
    let mut seen = HashSet::new();
    let mut path: Vec<PathBuf> = vec![];
    let current = std::env::var("PATH").unwrap_or_default();
    let login = shell::user_path().unwrap_or_default();
    for p in std::env::split_paths(&current).chain(std::env::split_paths(&login)).chain(well_known_dirs()) {
        if p.as_os_str().is_empty() || !p.is_dir() {
            continue;
        }
        if seen.insert(p.clone()) {
            path.push(p);
        }
    }
    // Kith's Node last, so the computer's own comes first; listed even before it exists, so
    // adding it (a separate `kith-serve node` process) counts without another refresh.
    for p in node::search_dirs() {
        if seen.insert(p.clone()) {
            path.push(p);
        }
    }
    if let Ok(joined) = std::env::join_paths(&path) {
        // SAFETY: called at startup and from the single refresh command, before/between spawns.
        unsafe { std::env::set_var("PATH", joined) };
    }
    *SEARCH_PATH.locked() = path;
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let dirs = {
        let cached = SEARCH_PATH.locked();
        if cached.is_empty() {
            std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
        } else {
            cached.clone()
        }
    };
    dirs.into_iter().flat_map(|d| executable_names(bin).map(move |name| d.join(name))).find(|p| is_executable(p))
}

/// What `bin` can be called on disk: Windows adds the extensions it runs.
fn executable_names(bin: &str) -> impl Iterator<Item = String> + '_ {
    let extensions: &[&str] =
        if cfg!(windows) && Path::new(bin).extension().is_none() { &[".exe", ".cmd", ".bat"] } else { &[""] };
    extensions.iter().map(move |e| format!("{bin}{e}"))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

pub fn on_path(bin: &str) -> bool {
    which(bin).is_some()
}

// MARK: listing + launching

/// Every harness the phone can pick, installed ones first.
pub fn list() -> Vec<Value> {
    list_from(&registry::agents())
}

/// `list()` over a given registry (the test feeds a fixed one).
fn list_from(registry: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = harnesses()
        .map(|h| {
            let path = h.bins.iter().find_map(|b| which(b));
            let reg = h.registry.and_then(|id| registry.iter().find(|a| a["id"] == id));
            let native = path.is_some() && h.local.is_some();
            let runnable = native || reg.is_some_and(|a| registry::launch_kind(a).is_some());
            let node = if native { None } else { reg.filter(|a| registry::needs_node(a)).and_then(|_| node::state()) };
            json!({
                "id": h.id,
                "name": h.name,
                "installed": path.is_some(),
                "available": path.is_some() && runnable,
                "path": path.map(|p| p.to_string_lossy().into_owned()),
                "description": reg.and_then(|a| a["description"].as_str()).unwrap_or_default(),
                "installHint": h.setup,
                "signedIn": signed_in(h.id),
                "canInstall": h.install.and_then(install_command).is_some(),
                "node": node.map(node::State::name),
                "command": h.local.unwrap_or_default(),
                "registry": h.registry,
                "curated": true,
            })
        })
        .collect();
    // Registry agents we don't know by name: runnable, but you sign in yourself.
    let known: HashSet<&str> = harnesses().filter_map(|h| h.registry).collect();
    for a in registry {
        let id = a["id"].as_str().unwrap_or_default();
        if id.is_empty() || known.contains(id) || id == "github-copilot" {
            continue;
        }
        out.push(json!({
            "id": id,
            "name": a["name"],
            "installed": false,
            "available": registry::launch_kind(a).is_some(),
            "path": null,
            "description": a["description"],
            "installHint": format!("Kith installs it automatically. Sign in to {} on this computer first if it needs an account.", a["name"].as_str().unwrap_or(id)),
            "signedIn": signed_in(id),
            "node": node::state().filter(|_| registry::needs_node(a)).map(node::State::name),
            "command": "",
            "registry": id,
            "curated": false,
        }));
    }
    out.sort_by_key(|v| (!v["installed"].as_bool().unwrap_or(false), !v["curated"].as_bool().unwrap_or(false)));
    out
}

// MARK: sign-in

/// Last sign-in check per harness id; missing = can't tell.
static SIGNED_IN: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

pub fn signed_in(id: &str) -> Option<bool> {
    SIGNED_IN.locked().iter().find(|(h, _)| h == id).map(|(_, ok)| *ok)
}

/// Records what a check found (`None`: can't tell).
pub fn set_signed_in(id: &str, state: Option<bool>) {
    let mut map = SIGNED_IN.locked();
    map.retain(|(h, _)| h != id);
    if let Some(ok) = state {
        map.push((id.to_owned(), ok));
    }
}

/// Re-checks every installed harness that can report its sign-in state.
pub async fn refresh_sign_in() {
    let checks = harnesses().filter_map(|h| {
        let check = h.signed_in?;
        let bin = h.bins.iter().find_map(|b| which(b))?;
        Some(async move {
            let run = tokio::process::Command::new(bin)
                .args(check.args.split_whitespace())
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .output();
            let ok = match tokio::time::timeout(Duration::from_secs(15), run).await {
                Ok(Ok(out)) => Some(check.ok.eval(out.status.success(), &String::from_utf8_lossy(&out.stdout))),
                Ok(Err(e)) => {
                    tracing::info!(backend = h.id, error = %e, "sign-in check failed");
                    None
                }
                Err(_) => None,
            };
            (h.id, ok)
        })
    });
    let results = futures::future::join_all(checks).await;
    for (id, ok) in results {
        set_signed_in(id, ok);
    }
}

/// `program` is some other harness's CLI (not `backend`'s own).
pub fn belongs_to_other(backend: &str, program: &str) -> bool {
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or(program);
    let own = |h: &Harness| h.id == backend || h.registry == Some(backend);
    harnesses().any(|h| !own(h) && h.bins.contains(&name)) && !harnesses().any(|h| own(h) && h.bins.contains(&name))
}

pub fn is_known(id: &str) -> bool {
    harness(id).is_some() || registry::agent(id).is_some()
}

/// Ways to start this backend, best first. Local CLIs can be too old to speak
/// ACP, so the registry build is kept as a fallback.
pub async fn launch_candidates(id: &str, progress: impl Fn(&str)) -> anyhow::Result<Vec<registry::Cmd>> {
    let mut out: Vec<registry::Cmd> = local_candidate(id).into_iter().collect();
    let h = harness(id);
    let reg_id = h.map_or(Some(id), |h| h.registry);
    if let Some(agent) = reg_id.and_then(registry::agent) {
        match registry::command(&agent, &progress).await {
            Ok(cmd) => out.push(cmd),
            Err(e) if out.is_empty() => return Err(e),
            Err(e) => tracing::info!(backend = id, error = format!("{e:#}"), "registry build unavailable"),
        }
    }
    if out.is_empty() {
        let hint = h.map_or("Check that it's installed on this computer.", |h| h.setup);
        let name = h.map_or(id, |h| h.name);
        return Err(acp::Failure::error(
            acp::FailureKind::NotSetUp,
            format!("{name} isn't set up on this computer. {hint}"),
        ));
    }
    Ok(out)
}

/// The registry build that *is* the harness's CLI (same npm package), so it can
/// sign in without the CLI installed. Adapters (claude-acp…) don't count.
fn registry_cli(h: &Harness) -> Option<Value> {
    let Some(Install::Npm(pkg)) = h.install else { return None };
    let agent = registry::agent(h.registry?)?;
    let npx = agent["distribution"]["npx"]["package"].as_str()?;
    (npx == pkg || npx.strip_prefix(pkg).is_some_and(|v| v.starts_with('@'))).then_some(agent)
}

/// Kith's own sign-in command for `id` can run now.
pub fn login_available(id: &str) -> bool {
    harness(id)
        .is_some_and(|h| h.bins.iter().any(|b| on_path(b)) || !h.login.contains("{bin}") || registry_cli(h).is_some())
}

/// Kith's own sign-in command for `id`, with `{bin}` resolved.
pub async fn login_command(id: &str) -> anyhow::Result<String> {
    let h = harness(id).ok_or_else(|| anyhow::anyhow!("no sign-in command for {id}"))?;
    let bin = match h.bins.iter().find_map(|b| which(b)) {
        Some(p) => shell::quote(&p.to_string_lossy()),
        None => match registry_cli(h) {
            Some(agent) => registry::command(&agent, |_| {}).await?.program,
            None if !h.login.contains("{bin}") => String::new(),
            None => anyhow::bail!("Install {} first.", h.name),
        },
    };
    Ok(h.login.replace("{bin}", &bin))
}

/// The installed CLI's own ACP mode, if it has one and it's installed.
pub fn local_candidate(id: &str) -> Option<registry::Cmd> {
    let h = harness(id)?;
    let template = h.local?;
    let path = h.bins.iter().find_map(|b| which(b))?;
    let args = template.strip_prefix("{bin}").unwrap_or(template).trim().to_owned();
    Some(registry::Cmd { program: shell::quote(&path.to_string_lossy()), args })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below set the search path and the sign-in cache, which are process-wide.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn detects_executables_on_search_path() {
        let _one = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!("kith-bin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("fake-harness");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        *SEARCH_PATH.locked() = vec![dir.clone()];
        assert!(which("fake-harness").is_none(), "not executable yet");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(which("fake-harness"), Some(bin));
        SEARCH_PATH.locked().clear();
    }

    #[test]
    fn every_harness_is_unique() {
        let ids: HashSet<_> = harnesses().map(|h| h.id).collect();
        assert_eq!(ids.len(), harnesses().count());
    }

    /// What the app sees, pinned: the table moved into the `kith-kernel` crate and this JSON must not change.
    /// `UPDATE_GOLDEN=1` rewrites the fixtures (only ever from a base whose behaviour is the intended one).
    #[test]
    fn wire_shape_is_pinned() {
        let _one = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!("kith-golden-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for bin in ["claude", "cursor-agent", "cbc", "pool"] {
            let p = dir.join(bin);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        *SEARCH_PATH.locked() = vec![dir.clone()];
        set_signed_in("claude", Some(true));
        set_signed_in("codex", Some(false));
        let registry = vec![
            json!({"id": "claude-acp", "name": "Claude", "description": "adapter", "distribution": {"npx": {"package": "x"}}}),
            json!({"id": "cursor", "name": "Cursor", "description": "c", "distribution": {"binary": {}}}),
            json!({"id": "kilo", "name": "Kilo", "description": "k", "distribution": {"npx": {"package": "@kilocode/cli@1.2.3"}}}),
            json!({"id": "somebody-else", "name": "Somebody", "description": "s", "distribution": {"uvx": {"package": "y"}}}),
        ];
        let mut listed = list_from(&registry);
        SEARCH_PATH.locked().clear();
        set_signed_in("claude", None);
        set_signed_in("codex", None);
        let root = dir.to_string_lossy().into_owned();
        for v in &mut listed {
            // Whether this machine has Kith's Node is not the table's business.
            if v.get("node").is_some_and(|n| !n.is_null()) {
                v["node"] = json!("<state>");
            }
            if let Some(p) = v["path"].as_str() {
                v["path"] = json!(p.replace(&root, "<DIR>"));
            }
        }
        std::fs::remove_dir_all(&dir).ok();
        let got = serde_json::to_string_pretty(&json!({"list": listed, "table": table_snapshot()})).unwrap() + "\n";
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/backends-wire.json");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(fixture, &got).unwrap();
        }
        assert_eq!(got, std::fs::read_to_string(fixture).unwrap(), "the wire shape of backends changed");
    }

    /// Every field of every row the host reads, in a form that does not depend on how the table is stored.
    fn table_snapshot() -> Value {
        let probes = [
            (true, "{\"loggedIn\":true}"),
            (false, "{\"loggedIn\":false}"),
            (true, "{\"logged_in\":true}"),
            (true, "Logged in as x"),
            (true, "Not logged in"),
            (true, "nothing"),
            (false, "Logged in"),
        ];
        Value::Array(
            harnesses()
                .map(|h| {
                    json!({
                        "id": h.id, "name": h.name, "bins": h.bins, "local": h.local, "registry": h.registry,
                        "setup": h.setup, "login": h.login,
                        "install": h.install.map(|i| match i {
                            Install::Script(s) => json!({"script": s}),
                            Install::Scripts { unix, windows } => json!({"unix": unix, "windows": windows}),
                            Install::Npm(p) => json!({"npm": p}),
                        }),
                        "signedIn": h.signed_in.map(|c| json!({
                            "args": c.args,
                            "probes": probes.iter().map(|(ok, out)| c.ok.eval(*ok, out)).collect::<Vec<_>>(),
                        })),
                    })
                })
                .collect(),
        )
    }
}
