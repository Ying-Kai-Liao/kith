//! What the wake rules ask of the world outside core.db: the clock, the process table, `gh`, the terminal claims. A trait,
//! so a test answers for them; [`System`] is the real one. [`Transcripts`] is the one question the recovery rule asks of a
//! conversation's transcript (was the `[kith wake RUN]` marker seen); M3-6 implements it over the held transcripts, and
//! until then [`Files`] reads the Python's way (`~/.claude/projects/*/<sid>.jsonl`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Seconds between two looks at a PR's checks (the tick is a minute).
pub(crate) const CI_POLL: f64 = 50.0;

/// A local date and time, as typed in `at:2026-10-27T15:00`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
}

/// `gh pr view --json headRefOid,state`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrView {
    pub sha: Option<String>,
    pub state: Option<String>,
}

/// One entry of `gh pr checks --json name,bucket`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub name: String,
    pub bucket: String,
}

pub(crate) trait Outside {
    /// Epoch seconds.
    fn now(&self) -> f64;
    /// A new 12-hex-digit id (a turn's run, a waiter).
    fn new_id(&self) -> String;
    /// Whether `pid` runs and, when `started` is given and the system can say, started then.
    fn alive(&self, pid: i64, started: Option<&str>) -> bool;
    /// The start time of `pid` as the system gives it; `None` when it cannot be read.
    fn proc_start(&self, pid: i64) -> Option<String>;
    /// Whether a live writer holds the conversation `sid`: the one-writer claim a turn would need (W3).
    fn writer_live(&self, sid: &str) -> bool;
    /// `None` when `gh` fails or says something that is not JSON.
    fn pr_view(&self, repo: &str, pr: i64) -> Option<PrView>;
    fn pr_checks(&self, repo: &str, pr: i64) -> Option<Vec<Check>>;
    /// A local time as epoch seconds; `None` when this machine cannot say.
    fn local_epoch(&self, at: Civil) -> Option<f64>;
    /// Seconds between two looks at a PR's checks.
    fn ci_poll(&self) -> f64 {
        CI_POLL
    }
}

pub(crate) trait Transcripts {
    /// Whether the transcript of the conversation `sid` holds `marker`.
    fn has(&self, sid: &str, marker: &str) -> bool;
}

/// A random id of 12 hex digits.
pub(crate) fn random_id() -> String {
    let mut raw = [0u8; 6];
    // A failed read leaves zeros: an id that repeats is dropped as a duplicate rather than doing harm.
    let _ = getrandom::fill(&mut raw);
    raw.iter().fold(String::with_capacity(12), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The real world.
pub(crate) struct System {
    /// `$KITH_STATE`: the terminals' claim files are under `live/terminal/`.
    pub state: PathBuf,
    /// `gh`, when the caller names one (the Python's `KITH_WAKE_GH`).
    pub gh: Option<PathBuf>,
}

fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl System {
    fn gh_path(&self) -> PathBuf {
        if let Some(p) = &self.gh {
            return p.clone();
        }
        let on_path = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join("gh"))
                .find(|p| p.is_file())
        });
        on_path
            .or_else(|| {
                ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
                    .into_iter()
                    .map(PathBuf::from)
                    .find(|p| p.exists())
            })
            .unwrap_or_else(|| PathBuf::from("gh"))
    }

    /// `gh ARGS`, its stdout as JSON; `None` for a failure to run, a timeout (20 s) or output that is not JSON.
    fn gh_json(&self, args: &[&str]) -> Option<Value> {
        let mut child = Command::new(self.gh_path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < Duration::from_secs(20) => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let out = child.wait_with_output().ok()?;
        serde_json::from_slice(&out.stdout).ok()
    }

    fn claim_alive(&self, name: &str) -> bool {
        let path = self.state.join("live").join("terminal").join(name);
        let Some(claim) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            return false;
        };
        let started = claim.get("procStart").and_then(Value::as_str);
        claim
            .get("pid")
            .and_then(Value::as_i64)
            .is_some_and(|pid| self.alive(pid, started))
    }
}

impl Outside for System {
    fn now(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }

    fn new_id(&self) -> String {
        random_id()
    }

    fn alive(&self, pid: i64, started: Option<&str>) -> bool {
        let Ok(pid) = u32::try_from(pid) else {
            return false;
        };
        if pid == 0 || crate::gate::pid_gone(pid) {
            return false;
        }
        match (started, crate::lock::process_start(pid)) {
            (Some(was), Some(now)) if !was.trim().is_empty() => squeeze(was) == now,
            _ => true,
        }
    }

    fn proc_start(&self, pid: i64) -> Option<String> {
        crate::lock::process_start(u32::try_from(pid).ok()?)
    }

    fn writer_live(&self, sid: &str) -> bool {
        if sid.is_empty() || sid.contains('/') {
            return false;
        }
        self.claim_alive(&format!("{sid}.claim"))
            || self.claim_alive(&format!("{sid}.takeover.claim"))
    }

    fn pr_view(&self, repo: &str, pr: i64) -> Option<PrView> {
        let v = self.gh_json(&[
            "pr",
            "view",
            &pr.to_string(),
            "--repo",
            repo,
            "--json",
            "headRefOid,state",
        ])?;
        let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(PrView {
            sha: get("headRefOid"),
            state: get("state"),
        })
    }

    fn pr_checks(&self, repo: &str, pr: i64) -> Option<Vec<Check>> {
        let v = self.gh_json(&[
            "pr",
            "checks",
            &pr.to_string(),
            "--repo",
            repo,
            "--json",
            "name,bucket",
        ])?;
        Some(
            v.as_array()?
                .iter()
                .map(|c| {
                    let get = |k: &str| {
                        c.get(k)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    Check {
                        name: get("name"),
                        bucket: get("bucket"),
                    }
                })
                .collect(),
        )
    }

    /// Asks `date`, which knows this Mac's time zone (BSD first, then GNU): unsafe code is forbidden here, so no
    /// `localtime`.
    fn local_epoch(&self, at: Civil) -> Option<f64> {
        let when = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            at.year, at.month, at.day, at.hour, at.minute, at.second
        );
        let tries: [&[&str]; 2] = [
            &["-j", "-f", "%Y-%m-%d %H:%M:%S", &when, "+%s"],
            &["-d", &when, "+%s"],
        ];
        tries.iter().find_map(|args| {
            let out = Command::new("date").args(*args).output().ok()?;
            out.status
                .success()
                .then(|| {
                    String::from_utf8_lossy(&out.stdout)
                        .trim()
                        .parse::<f64>()
                        .ok()
                })
                .flatten()
        })
    }
}

/// The Python's `transcript_has`: the transcript of Claude session `sid` under any of the Claude config folders.
pub(crate) struct Files {
    pub roots: Vec<PathBuf>,
}

impl Files {
    /// The config folders the Python looks in: `$CLAUDE_CONFIG_DIR` (else `~/.claude`), `~/.claude`, then each Claude
    /// account of this Mac.
    pub(crate) fn of(home: &Path, machine: &Path, config_dir: Option<&str>) -> Self {
        let mut roots = vec![
            config_dir
                .filter(|c| !c.is_empty())
                .map_or_else(|| home.join(".claude"), PathBuf::from),
            home.join(".claude"),
        ];
        if let Ok(rd) = std::fs::read_dir(machine.join("accounts").join("claude")) {
            let mut accounts: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            accounts.sort();
            roots.extend(accounts);
        }
        let mut seen = Vec::new();
        roots.retain(|r| {
            let new = !seen.contains(r);
            seen.push(r.clone());
            new
        });
        Self { roots }
    }
}

impl Transcripts for Files {
    fn has(&self, sid: &str, marker: &str) -> bool {
        if sid.is_empty() || sid.contains('/') {
            return false;
        }
        let needle = marker.as_bytes();
        for root in &self.roots {
            let Ok(projects) = std::fs::read_dir(root.join("projects")) else {
                continue;
            };
            for dir in projects.filter_map(Result::ok) {
                let Ok(bytes) = std::fs::read(dir.path().join(format!("{sid}.jsonl"))) else {
                    continue;
                };
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
        }
        false
    }
}
