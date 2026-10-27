//! What the channels ask hq: where its tools and agents are, who a sender is (`kith-auth who`), whether a person may
//! use an agent (`kith-auth resolve`), and each agent's card (`agent.md`).

use crate::LockExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// New agents show up without a restart.
const AGENT_REFRESH: Duration = Duration::from_secs(30);

/// Where hq's tools and folders are on this computer.
#[derive(Clone, Debug)]
pub struct Paths {
    pub hq_bin: PathBuf,
    pub auth_bin: PathBuf,
    /// This Mac's Kith folder (`~/.kith`, or `~/.hq` until it is migrated): the bridge's state.
    pub machine: PathBuf,
    /// Folders with one folder per agent.
    pub agent_roots: Vec<PathBuf>,
    /// `<data dir>/channels`: this host's state, run outboxes and downloaded attachments.
    pub data: PathBuf,
}

impl Paths {
    /// Blocking: looking for `hq` may ask the login shell. None when there is no hq to run.
    pub fn discover() -> Option<Self> {
        let hq_bin = crate::agent::hq::bin()?;
        // A tool next to the entry point, else in the kernel's bin/ (a packaged host's entry is the binary, the tools
        // are in the kernel it carries), else on PATH; the old hq-<x> name when the kernel has not been renamed.
        let real = std::fs::canonicalize(&hq_bin).unwrap_or_else(|_| hq_bin.clone());
        let kernel = crate::kernel::current_kernel();
        let beside = |name: &str| {
            let find = |name: &str| {
                tool_near(name, &hq_bin, &real, kernel.as_deref()).or_else(|| crate::agent::backends::which(name))
            };
            let old = name.replacen("kith-", "hq-", 1);
            find(name).or_else(|| find(&old)).unwrap_or_else(|| hq_bin.with_file_name(name))
        };
        let home = dirs::home_dir()?;
        let hq_home = crate::kernel::home_dir(&home);
        let mut agent_roots = vec![hq_home.join("agents")];
        agent_roots.extend(kernel_agents(&real));
        let machine = crate::kernel::var("MACHINE").map_or_else(|| crate::kernel::machine_dir(&home), PathBuf::from);
        Some(Self {
            auth_bin: beside("kith-auth"),
            hq_bin,
            machine,
            agent_roots,
            data: crate::service::data_dir().join("channels"),
        })
    }

    /// The bridge's state file. Read only: the bridge keeps writing it for the connections it still runs.
    pub fn bridge_state(&self) -> PathBuf {
        self.machine.join("bridge").join("state.json")
    }
}

/// The agents folder of the kernel the selected command belongs to (next to its bin folder). Not the running host's own
/// kernel: with the Mac app running and a checkout selected, that would put the app's built-in agents first.
fn kernel_agents(real_bin: &Path) -> Option<PathBuf> {
    Some(real_bin.parent()?.parent()?.join("agents"))
}

/// `name` beside the entry point (as given, or with symlinks resolved), else in the packaged kernel's bin/.
fn tool_near(name: &str, hq_bin: &Path, real: &Path, kernel: Option<&Path>) -> Option<PathBuf> {
    let near = [hq_bin.with_file_name(name), real.with_file_name(name)];
    let packaged = kernel.map(|k| k.join("bin").join(name));
    near.into_iter().chain(packaged).find(|p| p.is_file())
}

/// Environment the tests give the tools `output` runs (a temp `KITH_AUTH` and so on). Tests used to set these on the
/// process, which raced with the tests that read the real environment; only the child gets them now.
#[cfg(test)]
pub(crate) static TEST_ENV: Mutex<Vec<(&'static str, PathBuf)>> = Mutex::new(Vec::new());

/// Runs a tool without a terminal: its output, or None when it can't start or takes too long.
pub async fn output(bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = tokio::process::Command::new(bin);
    #[cfg(test)]
    cmd.envs(TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().map(|(k, v)| (*k, v)));
    let child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    tokio::time::timeout(TOOL_TIMEOUT, child.wait_with_output()).await.ok()?.ok()
}

/// A sender, as `kith-auth who <platform> <uid>` knows them.
#[derive(Clone, Debug, Default)]
pub struct Who {
    pub person: Option<String>,
    pub owner: bool,
    /// Ids the person had before a rename.
    pub was: Vec<String>,
    pub owner_id: Option<String>,
    pub owner_linked: bool,
}

impl Who {
    pub(crate) fn parse(v: &Value) -> Self {
        let text = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        Self {
            person: text("person"),
            owner: v.get("owner").and_then(Value::as_bool).unwrap_or(false),
            was: v
                .get("was")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default(),
            owner_id: text("owner_id"),
            owner_linked: v.get("owner_linked").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Whether `person` is this sender (also under an id they had before a rename).
    pub fn is(&self, person: &str) -> bool {
        self.person.as_deref() == Some(person) || self.was.iter().any(|w| w == person)
    }
}

/// Whether a person may use an agent.
#[derive(Clone, Debug, Default)]
pub struct Access {
    pub allowed: bool,
    pub why: String,
    pub owner: bool,
}

pub async fn who(paths: &Paths, platform: &str, uid: &str) -> Who {
    let out = output(&paths.auth_bin, &["who", platform, uid]).await;
    out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .map_or_else(|| Who { owner: false, ..Who::default() }, |v| Who::parse(&v))
}

pub async fn resolve(paths: &Paths, person: &str, agent: &str) -> Access {
    let out = output(&paths.auth_bin, &["resolve", agent, "--person", person]).await;
    match out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()) {
        Some(v) => Access {
            allowed: v.get("allowed").and_then(Value::as_bool).unwrap_or(false),
            why: v.get("why").and_then(Value::as_str).unwrap_or("").to_owned(),
            owner: v.get("owner").and_then(Value::as_bool).unwrap_or(false),
        },
        None => Access { why: format!("can't check whether {person} may use {agent}"), ..Access::default() },
    }
}

/// Tries per sender at `/link` per window, so a stranger can't loop it (each try takes the auth.json lock).
pub(crate) struct Limiter {
    tries: usize,
    window: Duration,
    seen: Mutex<BTreeMap<String, Vec<Instant>>>,
}

impl Limiter {
    pub(crate) const fn new(tries: usize, window: Duration) -> Self {
        Self { tries, window, seen: Mutex::new(BTreeMap::new()) }
    }

    /// Counts a try for `key`; false when it is over the limit (the refused try isn't counted).
    pub(crate) fn allow(&self, key: &str) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        seen.retain(|_, v| v.last().is_some_and(|t| now.duration_since(*t) < self.window));
        let mine = seen.entry(key.to_owned()).or_default();
        mine.retain(|t| now.duration_since(*t) < self.window);
        if mine.len() >= self.tries {
            return false;
        }
        mine.push(now);
        true
    }
}

static LINK_TRIES: Limiter = Limiter::new(5, Duration::from_secs(10 * 60));

/// `kith-auth link <platform> <uid> <code>` -> the person it linked, or the reason it refused.
pub async fn link(paths: &Paths, platform: &str, uid: &str, code: &str) -> Result<String, String> {
    if !LINK_TRIES.allow(&format!("{platform}:{uid}")) {
        return Err("Too many tries. Wait a few minutes.".into());
    }
    // kith-auth rewrites auth.json whole: wait for any other writer, or a revoke could be undone.
    let _w = crate::api::people::AUTH_WRITES.lock().await;
    match output(&paths.auth_bin, &["link", platform, uid, code]).await {
        Some(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned()),
        Some(o) => Err(String::from_utf8_lossy(&o.stderr)
            .trim()
            .trim_start_matches("kith-auth: ")
            .trim_start_matches("hq-auth: ")
            .to_owned()),
        None => Err(String::new()),
    }
}

/// The agents on this computer and their cards.
pub struct Agents {
    roots: Vec<PathBuf>,
    cache: Mutex<Option<(Instant, BTreeMap<String, PathBuf>)>>,
}

impl Agents {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots, cache: Mutex::new(None) }
    }

    /// {agent: package folder}, looked up again at most every 30 s.
    fn dirs(&self) -> BTreeMap<String, PathBuf> {
        let mut cache = self.cache.locked();
        if let Some((at, dirs)) = cache.as_ref()
            && at.elapsed() < AGENT_REFRESH
        {
            return dirs.clone();
        }
        let mut out = BTreeMap::new();
        for root in &self.roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let dir = entry.path();
                // role/ next to the agent's life
                let pkg = if dir.join("role/agent.md").is_file() { dir.join("role") } else { dir };
                if !name.starts_with('.') && pkg.join("agent.md").is_file() {
                    out.entry(name).or_insert(pkg);
                }
            }
        }
        *cache = Some((Instant::now(), out.clone()));
        out
    }

    pub fn list(&self) -> Vec<String> {
        self.dirs().into_keys().collect()
    }

    /// `key: value` from the agent's agent.md.
    pub fn card(&self, agent: &str, key: &str) -> Option<String> {
        let pkg = self.dirs().remove(agent)?;
        let text = std::fs::read_to_string(pkg.join("agent.md")).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    pub fn description(&self, agent: &str) -> String {
        let d = self.card(agent, "description").unwrap_or_else(|| format!("hq {agent}"));
        super::text::first_chars(&d, 1024).to_owned()
    }

    /// The name an agent posts under (`display_name` in agent.md); anything that isn't an agent stays as it is.
    pub fn display_name(&self, agent: &str) -> String {
        self.card(agent, "display_name").unwrap_or_else(|| agent.to_owned())
    }

    pub fn find(&self, word: &str) -> Option<String> {
        let w = word.to_lowercase();
        self.list().into_iter().find(|a| w == *a || w == cmd_name(a))
    }
}

/// Telegram commands: lowercase letters, digits and `_`.
pub fn cmd_name(agent: &str) -> String {
    agent.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_come_from_their_cards() {
        let dir = std::env::temp_dir().join(format!("kith-agents-{}", uuid::Uuid::new_v4()));
        for (name, card, sub) in [
            ("house", "description: Keeps the house\ndisplay_name: Home\n", ""),
            ("my-agent", "description: x\n", "role/"),
            (".hidden", "description: no\n", ""),
        ] {
            std::fs::create_dir_all(dir.join(name).join(sub)).unwrap();
            std::fs::write(dir.join(name).join(sub).join("agent.md"), card).unwrap();
        }
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        let agents = Agents::new(vec![dir.clone(), dir.join("missing")]);
        assert_eq!(agents.list(), vec!["house", "my-agent"]);
        assert_eq!(agents.display_name("house"), "Home");
        assert_eq!(agents.display_name("nobody"), "nobody");
        assert_eq!(agents.description("my-agent"), "x");
        assert_eq!(agents.description("nobody"), "hq nobody");
        assert_eq!(agents.find("MY_AGENT").as_deref(), Some("my-agent"));
        assert_eq!(agents.find("house").as_deref(), Some("house"));
        assert!(agents.find("empty").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn agents_follow_the_selected_command_not_the_running_host() {
        let checkout = Path::new("/work/kith/kernel/bin/kith");
        assert_eq!(kernel_agents(checkout), Some(PathBuf::from("/work/kith/kernel/agents")));
        assert_eq!(kernel_agents(Path::new("kith")), None);
        // The app's kernel is only where a missing tool is looked for.
        let app = Path::new("/App/Resources/kith/share/kith/kernel");
        let tool = tool_near("kith-auth", Path::new("/nowhere/bin/kith"), Path::new("/nowhere/bin/kith"), Some(app));
        assert!(tool.is_none());
    }

    #[test]
    fn a_renamed_person_is_still_the_sender() {
        let who = Who::parse(&serde_json::json!({"person": "kai", "was": ["old"], "owner": true}));
        assert!(who.is("kai") && who.is("old") && !who.is("other"));
        assert!(who.owner);
    }
}

#[cfg(test)]
mod limiter_tests {
    use super::Limiter;
    use std::time::Duration;

    #[test]
    fn a_sender_gets_a_few_link_tries_per_window() {
        let l = Limiter::new(3, Duration::from_secs(60));
        assert!((0..3).all(|_| l.allow("telegram:1")));
        assert!(!l.allow("telegram:1"), "the fourth try is refused");
        assert!(l.allow("telegram:2"), "another sender is not affected");
        let short = Limiter::new(1, Duration::from_millis(20));
        assert!(short.allow("a") && !short.allow("a"));
        std::thread::sleep(Duration::from_millis(40));
        assert!(short.allow("a"), "the window passes");
    }
}

#[cfg(test)]
mod tool_tests {
    use super::tool_near;

    #[test]
    fn tools_are_found_in_the_packaged_kernel() {
        let t = std::env::temp_dir().join(format!("kith-tools-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let kernel = t.join("share/kith/kernel");
        std::fs::create_dir_all(kernel.join("bin")).unwrap();
        std::fs::create_dir_all(t.join("bin")).unwrap();
        std::fs::write(t.join("bin/kith"), "").unwrap();
        std::fs::write(kernel.join("bin/kith-auth"), "").unwrap();
        let bin = t.join("bin/kith");
        // Release install: the entry is the binary, the tool is in the kernel.
        assert_eq!(tool_near("kith-auth", &bin, &bin, Some(&kernel)), Some(kernel.join("bin/kith-auth")));
        assert_eq!(tool_near("kith-send", &bin, &bin, Some(&kernel)), None);
        // Git checkout: the tool sits beside the entry and wins.
        std::fs::write(t.join("bin/kith-auth"), "").unwrap();
        assert_eq!(tool_near("kith-auth", &bin, &bin, Some(&kernel)), Some(t.join("bin/kith-auth")));
        std::fs::remove_dir_all(&t).unwrap();
    }
}
