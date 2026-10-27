//! Backup subscriptions: when the account an hq agent runs on has no usage left, its turns move to the next entry of
//! the user's chain (`kith <agent> --backups-json`: more Claude accounts, an API key, Codex…) until the limit lifts.
//!
//! This file is the memory of it: which entries are out until when (`backups.json` in the host's data folder, so a
//! restart keeps it), the chain as hq last said it, and the per-account usage readings the early switch reads. The
//! turn lifecycle that uses it is in `bot::hq` and `bot::failover`. Nothing here knows Claude from Codex: an entry is
//! an id, and a limit is a number of seconds.

use crate::LockExt;
use crate::usage::Window;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Why an entry is out: its provider refused a turn, or its usage reached [`EARLY_PERCENT`] before that.
pub const RATE_LIMIT: &str = "rate_limit";
pub const USAGE_95: &str = "usage_95";
/// The usage at which an entry stops being started on (a window of the 5-hour or weekly limit).
pub const EARLY_PERCENT: f64 = 95.0;
/// A turn moves on at most this many times; every move marks the entry it leaves, so it is also the chain's length
/// that ends it. Shared by the bots' turns and the channels' runs.
pub const MAX_MOVES: usize = 3;
/// How long an entry stays out when the provider didn't say when its limit lifts; it is checked again then.
pub const UNKNOWN_RESET_SECS: i64 = 3600;
/// A chain with backups is asked of hq again after this long, in the background (hq checks each entry's login, which
/// can take a while); one without backups after [`CHAIN_FRESH_EMPTY`].
const CHAIN_FRESH: Duration = Duration::from_secs(5 * 60);
const CHAIN_FRESH_EMPTY: Duration = Duration::from_secs(15 * 60);
/// An entry hq couldn't launch (not logged in, not allowed for this person) isn't asked again for this long.
const UNAVAILABLE_FOR: Duration = Duration::from_secs(120);
/// A usage reading older than this says nothing about now.
const READING_STALE_SECS: i64 = 30 * 60;

/// One line of `kith <agent> --backups-json`'s chain.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Default)]
pub struct ChainEntry {
    pub id: String,
    /// `null` from hq for an id it could not make sense of: not runnable.
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub why: Option<String>,
}

impl ChainEntry {
    /// The host can start these harnesses; others in the registry are skipped until it learns them.
    fn runnable(&self) -> bool {
        matches!(self.harness.as_deref(), Some("claude" | "codex"))
    }
}

/// `kith <agent> --backups-json`, entry by entry: one that doesn't parse is left out, not the whole chain.
#[derive(Deserialize)]
pub struct ChainDoc {
    #[serde(default)]
    chain: Vec<serde_json::Value>,
}

impl ChainDoc {
    pub fn entries(self) -> Vec<ChainEntry> {
        self.chain
            .into_iter()
            .filter_map(|v| {
                serde_json::from_value(v)
                    .inspect_err(|error| tracing::warn!(%error, "a backup chain entry didn't parse; left out"))
                    .ok()
            })
            .collect()
    }
}

/// An entry that is out, as `backups.json` keeps it.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[allow(clippy::struct_field_names, reason = "`out_until` is the name the state file and the kernel's contract use")]
pub struct Out {
    /// Epoch seconds the entry is back.
    pub out_until: i64,
    pub reason: String,
    pub since: i64,
}

/// A turn that has no account to run on: the text is the notice for the user.
#[derive(Debug)]
pub struct NoAccount(pub String);

impl std::fmt::Display for NoAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoAccount {}

/// Which entry a turn starts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// The agent's own login (also: no chain, so nothing changes).
    Primary,
    Backup(ChainEntry),
    /// Nothing is available: every entry that could run is out, with when each is back.
    AllOut(Vec<(String, i64)>),
}

struct Reading {
    windows: Vec<Window>,
    at: i64,
}

#[derive(Default)]
struct Inner {
    loaded: bool,
    out: BTreeMap<String, Out>,
    chains: HashMap<String, (Instant, Vec<ChainEntry>)>,
    readings: HashMap<String, Reading>,
    /// (agent, entry) -> when to ask again.
    unavailable: HashMap<(String, String), Instant>,
    /// Agents whose chain is being fetched in the background.
    refreshing: HashSet<String>,
    /// Counts changes of `out`, so the file is never overwritten by an older copy.
    seq: u64,
}

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

pub struct Backups {
    path: PathBuf,
    inner: std::sync::Mutex<Inner>,
    /// The `seq` last written to the file; held while writing.
    written: std::sync::Mutex<u64>,
    now: Clock,
}

fn clock() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs().cast_signed())
}

impl Backups {
    pub fn new(path: PathBuf) -> Self {
        Self::with_clock(path, Box::new(clock))
    }

    pub fn with_clock(path: PathBuf, now: Clock) -> Self {
        Self { path, inner: std::sync::Mutex::default(), written: std::sync::Mutex::default(), now }
    }

    pub fn now(&self) -> i64 {
        (self.now)()
    }

    /// Runs `f` on the state with `backups.json` read on first use and entries already back dropped. What `f` (or
    /// the dropping) changed in the out list is written after the lock is released.
    fn with<R>(&self, f: impl FnOnce(&mut Inner, i64) -> R) -> R {
        let (result, write) = {
            let mut inner = self.inner.locked();
            if !inner.loaded {
                inner.loaded = true;
                inner.out = std::fs::read_to_string(&self.path)
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default();
            }
            let now = self.now();
            let before = inner.out.clone();
            inner.out.retain(|_, o| o.out_until > now);
            let result = f(&mut inner, now);
            let write = (inner.out != before).then(|| {
                inner.seq += 1;
                (inner.seq, serde_json::to_vec_pretty(&inner.out))
            });
            (result, write)
        };
        if let Some((seq, Ok(bytes))) = write {
            self.write(seq, &bytes);
        }
        result
    }

    /// Written whole to a neighbor and renamed over, so a crash leaves the old file or the new one; a copy older than
    /// the one on disk is dropped.
    fn write(&self, seq: u64, bytes: &[u8]) {
        let mut written = self.written.locked();
        if seq <= *written {
            return;
        }
        let save = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &self.path)
        };
        match save() {
            Ok(()) => *written = seq,
            Err(error) => tracing::warn!(%error, "couldn't save backups.json"),
        }
    }

    /// Marks `id` out until `until` (epoch seconds; `None`, or a time already past, means [`UNKNOWN_RESET_SECS`] from
    /// now). Idempotent: two turns hitting the same limit mark it once. A real refusal replaces an early mark, and a
    /// later reset replaces an earlier one. Whether anything changed.
    pub fn mark_out(&self, id: &str, until: Option<i64>, reason: &str) -> bool {
        self.with(|inner, now| {
            let until = until.filter(|u| *u > now).unwrap_or(now + UNKNOWN_RESET_SECS);
            let since = inner.out.get(id).map_or(now, |o| o.since);
            if let Some(o) = inner.out.get(id)
                && o.out_until >= until
                && (o.reason == reason || reason == USAGE_95)
            {
                return false;
            }
            let reason = if reason == USAGE_95 && inner.out.get(id).is_some_and(|o| o.reason == RATE_LIMIT) {
                RATE_LIMIT
            } else {
                reason
            };
            inner.out.insert(id.to_owned(), Out { out_until: until, reason: reason.to_owned(), since });
            tracing::info!(entry = id, reason, until, "backup entry marked out");
            true
        })
    }

    pub fn out(&self) -> BTreeMap<String, Out> {
        self.with(|inner, _| inner.out.clone())
    }

    #[cfg(test)]
    pub fn is_out(&self, id: &str) -> bool {
        self.with(|inner, _| inner.out.contains_key(id))
    }

    // MARK: the chain

    /// The last chain hq gave however old, and how long ago; `None` before the first.
    pub fn chain_state(&self, agent: &str) -> Option<(Duration, Vec<ChainEntry>)> {
        self.inner.locked().chains.get(agent).map(|(at, c)| (at.elapsed(), c.clone()))
    }

    /// The last chain hq gave however old (a turn that failed needs it, not a fresh one).
    pub fn known_chain(&self, agent: &str) -> Option<Vec<ChainEntry>> {
        self.chain_state(agent).map(|(_, c)| c)
    }

    /// The agents whose chain hq gave.
    pub fn known_agents(&self) -> Vec<String> {
        self.inner.locked().chains.keys().cloned().collect()
    }

    /// The chains hq gave are out of date (the user changed the order): the next turn asks again.
    pub fn forget_chains(&self) {
        self.inner.locked().chains.clear();
    }

    /// Whether the chain is old enough to ask hq again: an agent without backups is asked rarely.
    pub fn chain_stale(age: Duration, chain: &[ChainEntry]) -> bool {
        age >= if chain.len() > 1 { CHAIN_FRESH } else { CHAIN_FRESH_EMPTY }
    }

    /// Takes the right to fetch `agent`'s chain; false when someone already is.
    pub fn begin_refresh(&self, agent: &str) -> bool {
        self.inner.locked().refreshing.insert(agent.to_owned())
    }

    /// The fetch ended. A failure keeps the chain known before (and looks again after the usual wait), or none.
    pub fn end_refresh(&self, agent: &str, chain: Option<Vec<ChainEntry>>) {
        let mut inner = self.inner.locked();
        inner.refreshing.remove(agent);
        let chain = chain.or_else(|| inner.chains.get(agent).map(|(_, c)| c.clone())).unwrap_or_default();
        inner.chains.insert(agent.to_owned(), (Instant::now(), chain));
    }

    /// Whether anything is out now (the feature is in use).
    pub fn any_out(&self) -> bool {
        !self.out().is_empty()
    }

    /// Every account entry any agent's chain named, for the usage poll: `(id, harness, account)`.
    pub fn accounts(&self) -> Vec<(String, String, String)> {
        let inner = self.inner.locked();
        let mut all: Vec<_> = inner
            .chains
            .values()
            .flat_map(|(_, chain)| chain)
            .filter_map(|e| Some((e.id.clone(), e.harness.clone()?, e.account.clone()?)))
            .filter(|(id, harness, _)| id.starts_with(&format!("{harness}:")))
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// `agent` can't start `id` right now (hq refused its launch): left alone for a while.
    pub fn note_unavailable(&self, agent: &str, id: &str) {
        self.inner.locked().unavailable.insert((agent.to_owned(), id.to_owned()), Instant::now());
    }

    // MARK: usage

    /// Replaces the readings of entry `id` (`claude` and `codex` are the ambient logins).
    pub fn record_reading(&self, id: &str, windows: Vec<Window>) {
        let at = self.now();
        self.inner.locked().readings.insert(id.to_owned(), Reading { windows, at });
    }

    /// The ambient `claude` and `codex` logins, as the host's usage poll gets them: only the windows it names change.
    pub fn record_window_set(&self, id: &str, windows: &[Window]) {
        for window in windows {
            self.record_window(id, window.clone());
        }
    }

    /// One window's new reading, keeping the entry's others.
    pub fn record_window(&self, id: &str, window: Window) {
        let at = self.now();
        let mut inner = self.inner.locked();
        let reading = inner.readings.entry(id.to_owned()).or_insert_with(|| Reading { windows: Vec::new(), at });
        reading.windows.retain(|w| w.id != window.id);
        reading.windows.push(window);
        reading.at = at;
    }

    /// When `id` is back if its latest reading has a window at [`EARLY_PERCENT`] or more: the latest of those
    /// windows' resets. The per-model weeklies (`seven_day_opus`…) are left out: one model running out is not the
    /// account running out.
    fn early(inner: &Inner, id: &str, now: i64) -> Option<i64> {
        let reading = inner.readings.get(id).filter(|r| now - r.at < READING_STALE_SECS)?;
        let full: Vec<&Window> = reading
            .windows
            .iter()
            .filter(|w| w.percent >= EARLY_PERCENT && !w.id.starts_with("seven_day_"))
            // A reading whose own reset has passed is from before the reset.
            .filter(|w| w.resets_at.is_none_or(|ms| ms / 1000 > now))
            .collect();
        if full.is_empty() {
            return None;
        }
        let resets = full.iter().filter_map(|w| w.resets_at).max().map(|ms| ms / 1000);
        Some(resets.unwrap_or(now + UNKNOWN_RESET_SECS))
    }

    // MARK: choosing

    /// The first entry of `chain` (primary first) that is ready and not out, skipping `skip` (entries this turn already
    /// tried). An entry whose usage reads [`EARLY_PERCENT`] is marked out on the way. When every entry is out but some
    /// only by that early mark, the first of those still runs: it may well have room, and a refusal marks it for real.
    /// A chain without backups changes nothing.
    pub fn pick(&self, agent: &str, chain: &[ChainEntry], skip: &HashSet<String>) -> Pick {
        if chain.len() < 2 {
            return Pick::Primary;
        }
        self.with(|inner, now| Self::pick_in(inner, now, agent, chain, skip))
    }

    fn pick_in(inner: &mut Inner, now: i64, agent: &str, chain: &[ChainEntry], skip: &HashSet<String>) -> Pick {
        let mut outs = Vec::new();
        let mut soft = None;
        for (i, entry) in chain.iter().enumerate() {
            let unavailable = inner
                .unavailable
                .get(&(agent.to_owned(), entry.id.clone()))
                .is_some_and(|at| at.elapsed() < UNAVAILABLE_FOR);
            if !entry.runnable() || !entry.ready || unavailable {
                continue;
            }
            // An entry this turn already tried is out for the notice, but never started again.
            if let Some(o) = inner.out.get(&entry.id) {
                if o.reason == USAGE_95 && soft.is_none() && !skip.contains(&entry.id) {
                    soft = Some(i);
                }
                outs.push((entry.id.clone(), o.out_until));
                continue;
            }
            if skip.contains(&entry.id) {
                continue;
            }
            if let Some(until) = Self::early(inner, &entry.id, now) {
                inner.out.insert(entry.id.clone(), Out { out_until: until, reason: USAGE_95.into(), since: now });
                tracing::info!(entry = %entry.id, until, "backup entry marked out at {EARLY_PERCENT}% usage");
                outs.push((entry.id.clone(), until));
                soft.get_or_insert(i);
                continue;
            }
            return Self::chosen(chain, i);
        }
        match soft {
            Some(i) => Self::chosen(chain, i),
            None => Pick::AllOut(outs),
        }
    }

    fn chosen(chain: &[ChainEntry], i: usize) -> Pick {
        if i == 0 { Pick::Primary } else { Pick::Backup(chain[i].clone()) }
    }

    /// The notice for a turn with nothing to run on: who is out and when the first is back.
    pub fn all_out_text(&self, outs: &[(String, i64)]) -> String {
        let now = self.now();
        let when = |until: i64| humanize(until - now);
        if outs.is_empty() {
            return "No account is ready to run this agent: none of its backup accounts is signed in and available."
                .into();
        }
        let list =
            outs.iter().map(|(id, until)| format!("{id} (back in {})", when(*until))).collect::<Vec<_>>().join(", ");
        let first = outs.iter().map(|(_, until)| *until).min().unwrap_or(now);
        format!("Every account for this agent is out of usage: {list}. The first is back in {}.", when(first))
    }
}

/// "2h 15m", "40m", "less than a minute".
fn humanize(secs: i64) -> String {
    let minutes = (secs.max(0) + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "less than a minute".into(),
        (0, m) => format!("{m}m"),
        (h, _) if h >= 24 => format!("{}d {}h", h / 24, h % 24),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    struct Rig {
        dir: PathBuf,
        now: Arc<AtomicI64>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("kith-backups-{}", uuid::Uuid::new_v4()));
            Self { dir, now: Arc::new(AtomicI64::new(1_000_000)) }
        }

        fn backups(&self) -> Backups {
            let now = self.now.clone();
            Backups::with_clock(self.dir.join("backups.json"), Box::new(move || now.load(Ordering::SeqCst)))
        }

        fn advance(&self, secs: i64) {
            self.now.fetch_add(secs, Ordering::SeqCst);
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn entry(id: &str, ready: bool) -> ChainEntry {
        let (harness, account) = id.split_once(':').map_or((id, None), |(h, a)| (h, Some(a.to_owned())));
        let harness = Some(harness.trim_end_matches("-api").to_owned());
        let kind = if id == "claude" { "primary" } else { "subscription" };
        ChainEntry { id: id.into(), harness, account, kind: kind.into(), ready, why: None }
    }

    fn chain() -> Vec<ChainEntry> {
        vec![entry("claude", true), entry("claude:work", true), entry("codex", true)]
    }

    fn picked(b: &Backups, skip: &[&str]) -> Pick {
        b.pick("a", &chain(), &skip.iter().map(|s| (*s).to_owned()).collect())
    }

    fn backup(id: &str) -> Pick {
        Pick::Backup(entry(id, true))
    }

    #[test]
    fn one_entry_hq_could_not_read_does_not_drop_the_chain() {
        // What `kith_accounts.chain()` prints for a hand-edited bad id, between two good entries.
        let doc: ChainDoc = serde_json::from_value(serde_json::json!({"version": 1, "agent": "a", "chain": [
            {"id": "claude", "harness": "claude", "account": null, "kind": "primary", "ready": true},
            {"id": "nope:", "harness": null, "account": null, "kind": "unknown", "ready": false, "why": "bad id"},
            {"id": 7},
            {"id": "codex", "harness": "codex", "account": null, "kind": "subscription", "ready": true}
        ]}))
        .unwrap();
        let chain = doc.entries();
        assert_eq!(chain.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["claude", "nope:", "codex"]);
        assert!(!chain[1].runnable());
        let b = Rig::new().backups();
        assert_eq!(b.pick("a", &chain, &HashSet::new()), Pick::Primary);
    }

    #[test]
    fn the_kernels_own_output_parses() {
        // Printed by `kernel/lib/kith_accounts.py backups-json` for `backups: [claude:work, "bad id!", codex,
        // claude-api:main]`, none of the accounts set up.
        let real = r#"{"version": 1, "agent": "house", "chain": [{"id": "claude", "harness": "claude", "account": null, "kind": "primary", "ready": true}, {"id": "claude:work", "harness": "claude", "account": "work", "kind": "subscription", "ready": false, "why": "no account work"}, {"id": "bad id!", "harness": null, "account": null, "kind": "unknown", "ready": false, "why": "'bad id!' is not a backup id: use <harness> or <harness>:<name> (claude, claude:work, claude-api:main, codex)"}, {"id": "codex", "harness": "codex", "account": null, "kind": "subscription", "ready": true}, {"id": "claude-api:main", "harness": "claude", "account": "main", "kind": "api-key", "ready": false, "why": "no anthropic/main key granted to house"}]}"#;
        let chain = serde_json::from_str::<ChainDoc>(real).unwrap().entries();
        assert_eq!(chain.len(), 5);
        assert_eq!(chain[2].harness, None);
        assert!(chain[0].ready && !chain[1].ready && chain[3].ready);
        let b = Rig::new().backups();
        b.end_refresh("house", Some(chain.clone()));
        assert_eq!(b.accounts(), vec![("claude:work".to_owned(), "claude".to_owned(), "work".to_owned())]);
        assert_eq!(b.pick("house", &chain, &HashSet::new()), Pick::Primary);
        b.mark_out("claude", None, RATE_LIMIT);
        assert!(
            matches!(b.pick("house", &chain, &HashSet::new()), Pick::Backup(e) if e.id == "codex"),
            "the bad id and the unready ones are passed over"
        );
    }

    #[test]
    fn nothing_changes_without_backups() {
        let rig = Rig::new();
        let b = rig.backups();
        assert_eq!(b.pick("a", &[], &HashSet::new()), Pick::Primary);
        assert_eq!(b.pick("a", &[entry("claude", true)], &HashSet::new()), Pick::Primary);
        // Even a primary marked out has nowhere to go: its turn fails as it does today.
        b.mark_out("claude", Some(rig.now() + 600), RATE_LIMIT);
        assert_eq!(b.pick("a", &[entry("claude", true)], &HashSet::new()), Pick::Primary);
    }

    impl Rig {
        fn now(&self) -> i64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn the_first_entry_that_is_ready_and_not_out_runs() {
        let rig = Rig::new();
        let b = rig.backups();
        assert_eq!(picked(&b, &[]), Pick::Primary);
        b.mark_out("claude", Some(rig.now() + 600), RATE_LIMIT);
        assert_eq!(picked(&b, &[]), backup("claude:work"));
        b.mark_out("claude:work", Some(rig.now() + 900), RATE_LIMIT);
        assert_eq!(picked(&b, &[]), backup("codex"));
        // An entry this turn already tried is passed over too.
        assert_eq!(
            picked(&b, &["codex"]),
            Pick::AllOut(vec![("claude".into(), rig.now() + 600), ("claude:work".into(), rig.now() + 900)])
        );
    }

    #[test]
    fn an_entry_that_is_not_ready_or_not_runnable_is_skipped() {
        let rig = Rig::new();
        let b = rig.backups();
        let chain = vec![
            entry("claude", false),
            entry("claude:work", false),
            ChainEntry { harness: Some("cursor".into()), ..entry("cursor", true) },
            ChainEntry { harness: None, ready: false, ..entry("bogus", false) },
            entry("codex", true),
        ];
        assert_eq!(b.pick("a", &chain, &HashSet::new()), backup("codex"));
        b.note_unavailable("a", "codex");
        assert_eq!(b.pick("a", &chain, &HashSet::new()), Pick::AllOut(vec![]));
        assert_eq!(b.pick("b", &chain, &HashSet::new()), backup("codex"), "another agent may be allowed it");
    }

    #[test]
    fn an_entry_is_back_once_its_time_has_passed() {
        let rig = Rig::new();
        let b = rig.backups();
        b.mark_out("claude", Some(rig.now() + 600), RATE_LIMIT);
        rig.advance(599);
        assert_eq!(picked(&b, &[]), backup("claude:work"));
        rig.advance(2);
        assert_eq!(picked(&b, &[]), Pick::Primary, "recovered with no one asked");
        assert!(b.out().is_empty());
    }

    #[test]
    fn an_unknown_reset_is_an_hour_and_marking_twice_marks_once() {
        let rig = Rig::new();
        let b = rig.backups();
        assert!(b.mark_out("claude", None, RATE_LIMIT));
        assert_eq!(b.out()["claude"].out_until, rig.now() + UNKNOWN_RESET_SECS);
        assert!(!b.mark_out("claude", None, RATE_LIMIT), "a second turn hitting the same limit");
        assert!(!b.mark_out("claude", Some(rig.now() - 5), RATE_LIMIT), "a reset in the past is the unknown one");
        rig.advance(10);
        assert!(b.mark_out("claude", Some(rig.now() + 7200), RATE_LIMIT), "a later reset extends it");
        assert_eq!(b.out()["claude"].since, 1_000_000, "since stays when it was marked first");
    }

    #[test]
    fn state_survives_a_restart() {
        let rig = Rig::new();
        rig.backups().mark_out("claude:work", Some(rig.now() + 600), USAGE_95);
        let text = std::fs::read_to_string(rig.dir.join("backups.json")).unwrap();
        let file: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(file["claude:work"]["reason"], "usage_95");
        assert_eq!(file["claude:work"]["out_until"], rig.now() + 600);
        assert_eq!(file["claude:work"]["since"], rig.now());
        let again = rig.backups();
        assert!(again.is_out("claude:work"));
        rig.advance(601);
        assert!(!again.is_out("claude:work"));
        // A file nobody can read is no state.
        std::fs::write(rig.dir.join("backups.json"), "{").unwrap();
        assert!(rig.backups().out().is_empty());
    }

    fn windows(five: f64, week: f64, resets_ms: Option<i64>) -> Vec<Window> {
        vec![Window::new("five_hour", "5-hour", five, resets_ms), Window::new("seven_day", "Weekly", week, None)]
    }

    #[test]
    fn usage_at_95_percent_moves_on_until_the_window_resets() {
        let rig = Rig::new();
        let b = rig.backups();
        b.record_reading("claude", windows(94.0, 10.0, None));
        assert_eq!(picked(&b, &[]), Pick::Primary);
        let resets = rig.now() + 1800;
        b.record_reading("claude", windows(96.0, 10.0, Some(resets * 1000)));
        assert_eq!(picked(&b, &[]), backup("claude:work"));
        assert_eq!(b.out()["claude"], Out { out_until: resets, reason: USAGE_95.into(), since: rig.now() });
        rig.advance(1801);
        b.record_reading("claude", windows(3.0, 10.0, None));
        assert_eq!(picked(&b, &[]), Pick::Primary, "back after the reset");
    }

    #[test]
    fn the_weekly_window_counts_and_per_model_weeklies_do_not() {
        let rig = Rig::new();
        let b = rig.backups();
        b.record_reading("claude", vec![Window::new("seven_day_opus", "Weekly · Opus", 99.0, None)]);
        assert_eq!(picked(&b, &[]), Pick::Primary);
        b.record_reading("claude", windows(10.0, 97.0, None));
        assert_eq!(picked(&b, &[]), backup("claude:work"));
        assert_eq!(
            b.out()["claude"].out_until,
            rig.now() + UNKNOWN_RESET_SECS,
            "no reset known: look again in an hour"
        );
    }

    #[test]
    fn a_stale_reading_or_one_past_its_reset_says_nothing() {
        let rig = Rig::new();
        let b = rig.backups();
        b.record_reading("claude", windows(99.0, 10.0, Some((rig.now() - 60) * 1000)));
        assert_eq!(picked(&b, &[]), Pick::Primary, "its window has reset");
        b.record_reading("claude", windows(99.0, 10.0, None));
        rig.advance(READING_STALE_SECS + 1);
        assert_eq!(picked(&b, &[]), Pick::Primary, "nobody has polled for half an hour");
    }

    #[test]
    fn accounts_are_read_on_their_own() {
        let rig = Rig::new();
        let b = rig.backups();
        b.record_reading("claude:work", windows(99.0, 10.0, None));
        assert_eq!(picked(&b, &[]), Pick::Primary, "the primary's own reading is separate");
        b.mark_out("claude", Some(rig.now() + 600), RATE_LIMIT);
        assert_eq!(picked(&b, &[]), backup("codex"), "the work account is nearly spent");
    }

    #[test]
    fn when_everything_is_out_only_by_early_marks_the_first_still_runs() {
        let rig = Rig::new();
        let b = rig.backups();
        for id in ["claude", "claude:work", "codex"] {
            b.record_reading(id, windows(96.0, 10.0, None));
        }
        assert_eq!(picked(&b, &[]), Pick::Primary);
        // A real refusal changes that: that entry is out for good measure, the next soft one runs.
        b.mark_out("claude", Some(rig.now() + 600), RATE_LIMIT);
        assert_eq!(b.out()["claude"].reason, RATE_LIMIT);
        assert_eq!(picked(&b, &[]), backup("claude:work"));
        b.mark_out("claude:work", Some(rig.now() + 600), RATE_LIMIT);
        b.mark_out("codex", Some(rig.now() + 600), RATE_LIMIT);
        assert!(matches!(picked(&b, &[]), Pick::AllOut(o) if o.len() == 3));
    }

    #[test]
    fn a_refusal_replaces_an_early_mark_and_an_early_mark_never_weakens_a_refusal() {
        let rig = Rig::new();
        let b = rig.backups();
        b.mark_out("claude", Some(rig.now() + 600), USAGE_95);
        assert!(b.mark_out("claude", Some(rig.now() + 300), RATE_LIMIT));
        assert_eq!(b.out()["claude"].reason, RATE_LIMIT);
        b.mark_out("claude", Some(rig.now() + 1200), USAGE_95);
        assert_eq!(b.out()["claude"].reason, RATE_LIMIT);
    }

    #[test]
    fn the_notice_names_who_is_out_and_when_the_first_is_back() {
        let rig = Rig::new();
        let b = rig.backups();
        let text = b.all_out_text(&[("claude".into(), rig.now() + 8100), ("codex".into(), rig.now() + 600)]);
        assert!(text.contains("claude (back in 2h 15m)") && text.contains("codex (back in 10m)"), "{text}");
        assert!(text.ends_with("The first is back in 10m."), "{text}");
        assert_eq!(humanize(20), "1m");
        assert_eq!(humanize(0), "less than a minute");
        assert_eq!(humanize(3600 * 30), "1d 6h");
    }

    #[test]
    fn the_chain_is_asked_for_once_in_a_while() {
        let rig = Rig::new();
        let b = rig.backups();
        assert!(b.chain_state("a").is_none());
        assert!(b.begin_refresh("a") && !b.begin_refresh("a"), "one fetch at a time");
        b.end_refresh("a", Some(chain()));
        assert_eq!(b.known_chain("a").map(|c| c.len()), Some(3));
        assert!(!Backups::chain_stale(Duration::from_secs(60), &chain()));
        assert!(Backups::chain_stale(Duration::from_secs(600), &chain()));
        assert!(!Backups::chain_stale(Duration::from_secs(600), &[]), "an agent without backups is asked rarely");
        b.end_refresh("a", None);
        assert_eq!(b.known_chain("a").map(|c| c.len()), Some(3), "a failed fetch keeps what was known");
        assert_eq!(b.accounts(), vec![("claude:work".to_owned(), "claude".to_owned(), "work".to_owned())]);
    }
}
