//! One agent turn as a subprocess (`hq <agent> <task>`), exactly like the bridge's `run()`, and the outbox the
//! agent's `hq-send` calls leave posts in.
//!
//! Each run holds a resource-gate permit from before its process starts until it ends, so channel turns count
//! against `max_running_agents` like every other agent turn.

use super::adapter::{Adapter, Ended};
use super::core::Core;
use super::hq::{self, Paths};
use super::queue::kill;
use super::text::{context_pct, short};
use crate::LockExt;
use crate::agent::acp::{self, Depleted};
use crate::agent::backups::{Backups, ChainEntry, MAX_MOVES, Pick, RATE_LIMIT};
use crate::agent::gate::Permit;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;

/// While a run waits for the gate, its working line says why this often.
const WAITING_EVERY: Duration = Duration::from_secs(3);

/// What a run inherits of the host's environment: the plain login basics and hq's own settings. Nothing else
/// (API keys, tokens, `KITH_*`): the bridge's runs had launchd's PATH and HOME and HQ_*, no more (KITH_* is the same settings under their new names).
/// The room a channel run is for, as `KITH_ROOM`: a DM is the 1:1 of the agent and the person (the owner when `None`);
/// any other chat is a group room named by its `<connection>:<chat>`. Unknown: none, and the kernel reads a group VIA
/// without a room as shared only.
fn run_room(agent: &str, person: Option<&str>, private: bool, chat: &str) -> Option<String> {
    if private {
        return Some(crate::agent::hq::room_of(agent, person, None));
    }
    (!chat.is_empty()).then(|| crate::agent::hq::room_of(agent, person, Some(chat)))
}

pub(crate) fn inherited(name: &str) -> bool {
    let basic = ["PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR", "TZ", "TERM"].contains(&name)
        || name.starts_with("LC_");
    // No bot tokens or keys, and this is no agent's helper.
    let kith =
        ["KITH_HOME", "KITH_STATE", "KITH_MACHINE", "KITH_MODEL", "KITH_KERNEL_BIN", "KITH_KERNEL"].contains(&name);
    let hq = name.starts_with("HQ_")
        && !name.starts_with("HQ_DISCORD_")
        && !["HQ_AGENT", "HQ_PERSON", "HQ_RESUME", "HQ_BRIDGE_OUTBOX"].contains(&name);
    basic || hq || kith
}

/// Reads a pipe into a buffer that stays readable if the reader is stopped.
async fn read_into(mut pipe: impl tokio::io::AsyncRead + Unpin, into: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    while let Ok(n) = pipe.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        into.locked().extend_from_slice(&chunk[..n]);
    }
}

fn tail(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let skip = text.chars().count().saturating_sub(n);
    text.chars().skip(skip).collect()
}

/// What one process of the agent left.
struct Ran {
    out: Vec<u8>,
    err: Vec<u8>,
    /// Its exit code; None when a signal ended it or it never finished.
    code: Option<i32>,
    /// How many `kith-send` posts it made to the thread while it ran.
    posted: usize,
}

enum Attempt {
    Ran(Ran),
    /// Stop was pressed or the work was interrupted.
    Stopped(Ended),
    /// The process could not start; the thread was told.
    NotStarted,
}

/// Whether a person's turn may still reach them, asked again before its answer and before each post it leaves in the
/// outbox (revoke-person R5), in sync with the Kith cloud (`hq::resolve_in_sync`, R8). The owner's turns aren't asked.
#[derive(Clone)]
pub(super) struct Recheck {
    paths: Paths,
    /// None for the owner.
    person: Option<String>,
    agent: String,
    hold: Duration,
    poll: Duration,
}

impl Recheck {
    async fn still(&self) -> bool {
        let Some(person) = &self.person else { return true };
        let access = hq::resolve_in_sync(&self.paths, person, &self.agent, self.hold, self.poll).await;
        if !access.allowed {
            tracing::info!("{person} lost {} during the turn ({}): nothing more is delivered", self.agent, access.why);
        }
        access.allowed
    }
}

/// `kith <agent> --backups-json` now, remembered in `backups` (a failure keeps what was known before).
async fn fetch_chain(backups: &Backups, bin: &Path, agent: &str) -> Vec<ChainEntry> {
    let got = crate::agent::hq::backups(bin, agent)
        .await
        .inspect_err(|error| tracing::warn!(agent, error = format!("{error:#}"), "couldn't read the backup chain"))
        .ok();
    backups.end_refresh(agent, got);
    backups.known_chain(agent).unwrap_or_default()
}

/// Whether the run ended on a usage limit, and when it lifts if the account said. From the structure of what the
/// harness printed: Claude Code's `is_error` result with the API's 429 status, kith's own `usage_limit` for Codex
/// (parsed from Codex's `usage_limit_reached` error). The one reading of words is Claude Code's own limit message, for
/// versions that print no status; it needs `is_error` as well and Claude's wording ("hit your limit", "usage limit
/// reached", "credit balance is too low"), so an answer that talks about limits is never taken for one.
fn depleted(res: &Value) -> Option<Depleted> {
    if !res.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let text = res.get("result").and_then(Value::as_str).unwrap_or("");
    // Claude Code's older limit message ends in "|<epoch>".
    let epoch = text.rsplit_once('|').and_then(|(_, t)| t.trim().parse::<i64>().ok()).map(acp::epoch_secs);
    if let Some(limit) = res.get("usage_limit") {
        return Some(Depleted { resets_at: limit.get("resets_at").and_then(Value::as_i64).map(acp::epoch_secs) });
    }
    let lower = text.to_lowercase();
    let worded = ["hit your limit", "hit your usage limit", "usage limit reached", "credit balance is too low"]
        .iter()
        .any(|p| lower.contains(p));
    (res.get("api_error_status").and_then(Value::as_i64) == Some(429) || worded)
        .then_some(Depleted { resets_at: epoch })
}

/// "15:00" for a time today, "Sat 15:00" for one further away.
fn clock_time(at: i64, now: i64) -> String {
    use chrono::TimeZone as _;
    let Some(time) = chrono::Local.timestamp_opt(at, 0).single() else { return String::new() };
    let soon = at - now < 18 * 3600
        && time.date_naive()
            == chrono::Local.timestamp_opt(now, 0).single().map_or(time.date_naive(), |n| n.date_naive());
    time.format(if soon { "%H:%M" } else { "%a %H:%M" }).to_string()
}

impl Core {
    /// Waits for the gate's permit, with the working line saying why it waits. None: stopped while waiting.
    pub(super) async fn admit(
        &self,
        ad: &Arc<dyn Adapter>,
        wk: &str,
        cancel: &tokio::sync::Notify,
        invitee: bool,
    ) -> Option<Permit> {
        let mut admission = self.gate.enter_as(&format!("channels:{wk}"), invitee);
        let waiting = async {
            loop {
                tokio::select! {
                    permit = admission.granted() => break permit,
                    () = tokio::time::sleep(WAITING_EVERY) => {
                        let status = self.with(|i| i.busy.get(wk).map(|b| b.status.clone()));
                        if let Some(status) = status.filter(|s| !s.is_null()) {
                            ad.status_text(&status, &self.gate.waiting_text()).await;
                        }
                    }
                }
            }
        };
        tokio::select! {
            permit = waiting => Some(permit),
            () = cancel.notified() => None,
        }
    }

    /// One agent turn in this thread. The permit is held until it ends.
    ///
    /// An agent with a backup chain (see `agent::backups`) starts on the first account that is not out of usage. A run
    /// that ends on a usage limit marks its account out and, when it has done nothing yet that can't be done twice
    /// (nothing was posted to the thread), starts again on the next one, at most once per entry.
    pub(super) async fn run(
        &self,
        ad: &Arc<dyn Adapter>,
        key: &str,
        agent: &str,
        wk: &str,
        task: &str,
        _permit: Permit,
    ) -> Result<Ended> {
        let conv = self.with(|i| i.st.threads.get(key).and_then(|t| t.get("conv").cloned()).unwrap_or(Value::Null));
        let outbox = self.paths.data.join("outbox").join(short(&format!("{wk}{}", super::core::now())));
        std::fs::create_dir_all(&outbox).with_context(|| format!("creating {}", outbox.display()))?;
        let ended = self.turn(ad, key, agent, wk, task, &conv, &outbox).await;
        let _ = std::fs::remove_dir(&outbox);
        ended
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one turn: its inputs, and the account loop
    async fn turn(
        &self,
        ad: &Arc<dyn Adapter>,
        key: &str,
        agent: &str,
        wk: &str,
        task: &str,
        conv: &Value,
        outbox: &Path,
    ) -> Result<Ended> {
        let person = self.with(|i| {
            i.st.threads.get(key).and_then(|t| t.get("person")).and_then(Value::as_str).unwrap_or("").to_owned()
        });
        let owner = if person.is_empty() { true } else { hq::resolve(&self.paths, &person, agent).await.owner };
        let still = Recheck {
            paths: self.paths.clone(),
            person: (!owner).then(|| person.clone()),
            agent: agent.to_owned(),
            hold: self.settings.sync_hold,
            poll: self.settings.sync_poll,
        };
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
            std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(inherited)).collect();
        // Both spellings of a kernel setting: the kernel reads KITH_<X>, role repos written before the rename HQ_<X>.
        let set = |env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>, name: &str, v: &str| {
            for (k, v) in crate::kernel::both(name, v) {
                env.push((k.into(), v.into()));
            }
        };
        set(&mut env, "VIA", ad.platform());
        let (private, chat) = self.with(|i| {
            let th = i.st.threads.get(key);
            (
                th.and_then(|t| t.get("private")).and_then(Value::as_bool).unwrap_or(false),
                th.and_then(|t| t.get("room")).and_then(Value::as_str).unwrap_or("").to_owned(),
            )
        });
        if let Some(room) = run_room(agent, (!owner).then_some(person.as_str()), private, &chat) {
            set(&mut env, "ROOM", &room);
        }
        set(&mut env, "OUTPUT", "json");
        if let Some(mode) = &self.settings.permission_mode {
            set(&mut env, "PERMISSION_MODE", mode); // unset: the agent's mode, else hq's default
        }
        if !owner {
            set(&mut env, "PERSON", &person); // bin/hq checks the membership again and sets their mode
        }
        set(&mut env, "BRIDGE_OUTBOX", &outbox.to_string_lossy());
        // To ring the person this run serves, and nobody else (`kith-call`).
        if let Some(call_env) = self.call_env.get() {
            for (k, v) in call_env(agent, (!owner).then_some(person.as_str())) {
                env.push((k.into(), v.into()));
            }
        }
        for (k, v) in ad.env(conv, agent) {
            env.push((k.into(), v.into())); // already under both spellings
        }

        let mut chain = self.chain(agent).await;
        let mut tried: HashSet<String> = HashSet::new();
        let mut posted = 0;
        loop {
            // None: the agent's own account. A chain of one entry (or none) never changes anything.
            let (backup, serving) = match self.backups.pick(agent, &chain, &tried) {
                Pick::Primary => (None, chain.first().map(|e| e.id.clone()).unwrap_or_default()),
                Pick::Backup(entry) => (Some(entry.id.clone()), entry.id),
                Pick::AllOut(outs) => {
                    let text = format!("⚠️ {}", self.backups.all_out_text(&outs));
                    self.say(ad, conv, agent, &text, None).await;
                    return Ok(Ended::Done);
                }
            };
            tried.insert(serving.clone());
            // A session is one harness's: the backup's own is kept apart, so the agent's resumes once it is back.
            let session = self.with(|i| {
                let th = i.st.threads.get(key)?;
                let sid = match &backup {
                    None => th.get("sessions")?.get(agent),
                    Some(id) => th.get("backup_sessions")?.get(agent)?.get(id),
                };
                sid.and_then(Value::as_str).map(str::to_owned)
            });
            let fresh_backup = backup.is_some() && session.is_none();
            let mut run_env = env.clone();
            if let Some(session) = &session {
                set(&mut run_env, "RESUME", session); // any harness: bin/hq starts a new session when this one is another harness's
            }
            let attempt = match self
                .attempt(ad, agent, wk, task, conv, outbox, &still, run_env, backup.as_deref(), fresh_backup)
                .await?
            {
                Attempt::Stopped(ended) => return Ok(ended),
                Attempt::NotStarted => return Ok(Ended::Done),
                Attempt::Ran(attempt) => attempt,
            };
            posted += attempt.posted;
            if let Some(id) = &backup
                && attempt.code == Some(4)
            {
                // kith can't run this entry (not signed in, not allowed for this person): the next one answers.
                tracing::info!(agent, entry = %id, "backup account skipped");
                self.backups.note_unavailable(agent, id);
                continue;
            }
            let res = serde_json::from_slice::<Value>(&attempt.out).ok();
            // A card's wake rides in this task: no result means the turn never ran (held session, failed spawn).
            let failed = self.settle_wake(task, res.is_some());
            if !failed.is_empty()
                && let Some(hub) = crate::runauth::hub()
            {
                crate::cards::resume::channel_failed(&hub, &failed);
            }
            let limit = res.as_ref().and_then(depleted);
            if let Some(limit) = limit {
                if chain.len() < 2 && self.backups.known_chain(agent).is_none() {
                    // The first run to need the chain: it is only fetched in the background otherwise.
                    chain = fetch_chain(&self.backups, &self.paths.hq_bin, agent).await;
                }
                if chain.len() >= 2 {
                    self.backups.mark_out(&serving, limit.resets_at, RATE_LIMIT);
                    // Nothing posted yet and not past its first turn (no tools ran): the whole run can start again.
                    let started_work = res.as_ref().and_then(|r| r.get("num_turns")).and_then(Value::as_i64) > Some(1);
                    if posted == 0 && !started_work && tried.len() <= MAX_MOVES {
                        tracing::info!(agent, entry = %serving, "run ran out of usage; moving to the next account");
                        continue;
                    }
                }
            }
            return Ok(self
                .finish(
                    ad,
                    key,
                    agent,
                    conv,
                    &still,
                    attempt,
                    res,
                    backup.as_deref(),
                    &chain,
                    limit.is_some() && chain.len() >= 2,
                )
                .await);
        }
    }

    /// One process of the agent, on `backup` or its own account, until it ends. Posts what its `kith-send` calls leave
    /// in the outbox while it runs.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // the process and everything it is watched with
    async fn attempt(
        &self,
        ad: &Arc<dyn Adapter>,
        agent: &str,
        wk: &str,
        task: &str,
        conv: &Value,
        outbox: &Path,
        still: &Recheck,
        mut env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        backup: Option<&str>,
        fresh_backup: bool,
    ) -> Result<Attempt> {
        // A person's run (the env names one) gets a token of its own, revoked when this attempt ends.
        let grant = crate::runauth::grant_for_env(&mut env, agent, wk);
        if let Some(ended) = self.with(|i| i.busy.get(wk).and_then(|b| b.ended)) {
            return Ok(Attempt::Stopped(ended)); // stopped before the process existed
        }
        // A backup's new session knows nothing of the thread: it is told so, rather than left to guess.
        let preface = if fresh_backup {
            "[Kith] You are answering on a backup account: the usual one is out of usage. Earlier messages in this \
             thread were answered in another session you cannot see; ask if you need what was said.\n\n"
        } else {
            ""
        };
        let mut cmd = tokio::process::Command::new(&self.paths.hq_bin);
        cmd.args(run_args(agent, backup, &format!("{preface}{task}{}", ad.footer())))
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(home) = dirs::home_dir() {
            cmd.current_dir(home);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let text = format!("⚠️ {agent} failed:\n```\n{e}\n```");
                self.say(ad, conv, agent, &text, None).await;
                return Ok(Attempt::NotStarted);
            }
        };
        let pid = child.id();
        if let (Some(grant), Some(pid)) = (&grant, pid) {
            grant.bind_pid(pid);
        }
        let stop_now = self.with(|i| {
            let b = i.busy.get_mut(wk)?;
            b.pid = pid;
            b.exited = false;
            b.ended.is_some().then_some(())
        });
        if stop_now.is_some()
            && let Some(pid) = pid
        {
            kill(pid); // Stop was pressed before the process existed
        }
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            if let Some(pid) = pid {
                kill(pid);
            }
            anyhow::bail!("no pipes on the agent process");
        };
        let (out, err) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let readers = [tokio::spawn(read_into(stdout, out.clone())), tokio::spawn(read_into(stderr, err.clone()))];
        let stop_pump = Arc::new(tokio::sync::Notify::new());
        let pump = {
            let (poll, ad, conv, outbox, still, stop) =
                (self.settings.poll, ad.clone(), conv.clone(), outbox.to_path_buf(), still.clone(), stop_pump.clone());
            tokio::spawn(async move {
                let mut posted = 0;
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(poll) => posted += flush(&ad, &conv, &outbox, &still).await,
                        () = stop.notified() => break posted,
                    }
                }
            })
        };
        // One budget for the process and for its output: an agent that leaves a background process holding the
        // pipes open can't hang the turn.
        let grace = self.settings.reader_grace;
        let finished = async {
            let status = child.wait().await.ok();
            self.with(|i| i.busy.get_mut(wk).map(|b| b.exited = true));
            let [out_reader, err_reader] = readers;
            let closed = tokio::time::timeout(grace, async {
                let _ = tokio::join!(out_reader, err_reader);
            })
            .await
            .is_ok();
            (closed, status.and_then(|s| s.code()))
        };
        let outcome = tokio::time::timeout(self.settings.run_timeout, finished).await;
        let timed_out = outcome.is_err();
        if timed_out || matches!(outcome, Ok((false, _))) {
            // Out of time, or output still open after the process left: whatever is left of its group goes too.
            if let Some(pid) = pid {
                kill(pid);
            }
            self.with(|i| i.busy.get_mut(wk).map(|b| b.exited = true));
        }
        let code = outcome.ok().and_then(|(_, code)| code);
        stop_pump.notify_one();
        let mut posted = pump.await.unwrap_or(0); // a post in flight finishes before the last flush
        posted += flush(ad, conv, outbox, still).await;
        if let Some(ended) = self.with(|i| i.busy.get(wk).and_then(|b| b.ended)) {
            return Ok(Attempt::Stopped(ended));
        }
        let (out, err) = if timed_out {
            (Vec::new(), b"timed out after 60 min".to_vec())
        } else {
            (out.locked().clone(), err.locked().clone())
        };
        Ok(Attempt::Ran(Ran { out, err, code, posted }))
    }

    /// Posts what the run ended with: the answer (and, from a backup, the line saying so), or its error. Remembers the
    /// session for the next message. Nothing, when the person lost the agent while it ran.
    #[allow(clippy::too_many_arguments)] // what the turn knows once it has its output
    async fn finish(
        &self,
        ad: &Arc<dyn Adapter>,
        key: &str,
        agent: &str,
        conv: &Value,
        still: &Recheck,
        ran: Ran,
        res: Option<Value>,
        backup: Option<&str>,
        chain: &[ChainEntry],
        limit_hit: bool,
    ) -> Ended {
        if !still.still().await {
            return Ended::Done; // revoked while it ran: the late answer is not delivered
        }
        let (mut text, sid, pct) = if let Some(res) = res {
            let mut text = res.get("result").and_then(Value::as_str).unwrap_or("").to_owned();
            if res.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                let why = if text.is_empty() {
                    res.get("subtype").and_then(Value::as_str).unwrap_or("error").to_owned()
                } else {
                    text
                };
                text = format!("⚠️ {why}");
            }
            let sid = res.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
            (text, sid, context_pct(&res))
        } else {
            let shown = tail(if ran.err.is_empty() { &ran.out } else { &ran.err }, 1500);
            (format!("⚠️ {agent} failed:\n```\n{shown}\n```"), None, None)
        };
        // A session that ended on the limit has nothing to resume.
        if let Some(sid) = sid.filter(|_| !limit_hit) {
            self.with(|i| {
                if let Some(th) = i.st.threads.get_mut(key) {
                    match backup {
                        None => th["sessions"][agent] = Value::String(sid),
                        Some(id) => th["backup_sessions"][agent][id] = Value::String(sid),
                    }
                    i.save();
                }
            });
        }
        if let Some(id) = backup
            && !text.starts_with('⚠')
        {
            let note = self.answered_by(ad, id, chain);
            text = format!("{text}\n\n{note}");
        }
        let show = pct.filter(|p| self.settings.context_show.is_some_and(|min| *p >= min));
        self.say(ad, conv, agent, &text, show).await;
        Ended::Done
    }

    /// "answered by codex · Claude back at 15:00": which account answered, and when the agent's own is back if that
    /// is known. Times are the host's local time.
    fn answered_by(&self, ad: &Arc<dyn Adapter>, id: &str, chain: &[ChainEntry]) -> String {
        let mut line = format!("answered by {id}");
        if let Some(primary) = chain.first()
            && let Some(out) = self.backups.out().get(&primary.id)
        {
            let name = primary.id.split_once(':').map_or(primary.id.as_str(), |(h, _)| h);
            let name = name[..1].to_uppercase() + &name[1..];
            let _ = write!(line, " · {name} back at {}", clock_time(out.out_until, self.backups.now()));
        }
        ad.note_line(&line)
    }

    /// The agent's backup chain as last known. A run never waits for hq: a chain gone stale is fetched again in the
    /// background, and one never fetched is too, unless backups are in use (something is out), when this run needs it
    /// to pick. No chain, or any failure, is an empty chain: nothing changes.
    async fn chain(&self, agent: &str) -> Vec<ChainEntry> {
        let later = || {
            if self.backups.begin_refresh(agent) {
                let (backups, bin, agent) = (self.backups.clone(), self.paths.hq_bin.clone(), agent.to_owned());
                tokio::spawn(async move { drop(fetch_chain(&backups, &bin, &agent).await) });
            }
        };
        match self.backups.chain_state(agent) {
            Some((age, chain)) => {
                if Backups::chain_stale(age, &chain) {
                    later();
                }
                chain
            }
            None if self.backups.any_out() => fetch_chain(&self.backups, &self.paths.hq_bin, agent).await,
            None => {
                later();
                Vec::new()
            }
        }
    }

    async fn say(&self, ad: &Arc<dyn Adapter>, conv: &Value, agent: &str, text: &str, ctx: Option<f64>) {
        if let Err(e) = ad.post(conv, agent, text, ctx).await {
            tracing::warn!("{}: posting {agent}'s answer failed: {e:#}", ad.conn());
        }
    }
}

/// Posts what the run's hq-send calls left in its outbox, oldest first; once the person lost the agent, deletes it
/// instead. How many it posted.
pub(super) async fn flush(ad: &Arc<dyn Adapter>, conv: &Value, outbox: &Path, still: &Recheck) -> usize {
    let mut taken = 0;
    {
        let Ok(entries) = std::fs::read_dir(outbox) else { return 0 };
        let mut files: Vec<_> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        if !files.is_empty() && !still.still().await {
            for f in &files {
                let _ = std::fs::remove_file(f);
            }
            return 0;
        }
        for f in files {
            let Some(item) = std::fs::read(&f).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
                continue;
            };
            if std::fs::remove_file(&f).is_err() {
                continue;
            }
            taken += 1;
            let who = item.get("as").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("kith");
            let files: Vec<String> = item
                .get("files")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            let sent = if files.is_empty() {
                ad.post(conv, who, item.get("text").and_then(Value::as_str).unwrap_or(""), None).await
            } else {
                ad.send_files(conv, who, &files, item.get("caption").and_then(Value::as_str).unwrap_or("")).await
            };
            if let Err(e) = sent {
                // One failed post must not end the run's other posts.
                tracing::warn!("{}: kith-send post failed: {e:#}", ad.conn());
            }
        }
    }
    taken
}

/// The arguments of one run: `--` ends kith's own flags, so a message that starts with a dash ("-r", "--resume x") is
/// the prompt and not a flag.
fn run_args(agent: &str, backup: Option<&str>, prompt: &str) -> Vec<String> {
    let mut args = vec![agent.to_owned()];
    if let Some(id) = backup {
        args.extend(["--backup".to_owned(), id.to_owned()]);
    }
    args.extend(["--".to_owned(), prompt.to_owned()]);
    args
}

#[cfg(test)]
mod tests {
    use super::{depleted, inherited, run_args, run_room};
    use serde_json::json;

    #[test]
    fn the_message_follows_a_double_dash_so_a_leading_dash_is_text() {
        let args = run_args("kai", Some("b1"), "-r foo\nsecond line");
        assert_eq!(args, ["kai", "--backup", "b1", "--", "-r foo\nsecond line"]);
        assert_eq!(run_args("kai", None, "--resume x"), ["kai", "--", "--resume x"]);
    }

    #[test]
    fn a_dm_is_a_1_1_and_a_group_chat_is_its_own_room() {
        assert_eq!(run_room("echo", None, true, "telegram:42").as_deref(), Some("1:1:echo:owner"));
        assert_eq!(run_room("echo", Some("sam"), true, "telegram:42").as_deref(), Some("1:1:echo:sam"));
        assert_eq!(run_room("echo", None, false, "telegram:-100").as_deref(), Some("telegram:-100"));
        assert_eq!(run_room("echo", Some("sam"), false, "slack:C1").as_deref(), Some("slack:C1"));
        assert_eq!(run_room("echo", None, false, ""), None, "unknown chat: the kernel treats the VIA as a group");
    }

    #[test]
    fn a_usage_limit_is_read_from_the_structure_of_the_result() {
        // Claude Code: the API's status, with or without the epoch its older message ends in.
        let status = json!({"is_error": true, "api_error_status": 429, "result": "anything"});
        assert_eq!(depleted(&status).map(|d| d.resets_at), Some(None));
        let epoch = json!({"is_error": true, "result": "Claude AI usage limit reached|1900000000"});
        assert_eq!(depleted(&epoch).and_then(|d| d.resets_at), Some(1_900_000_000));
        // Codex through kith: its own usage_limit object, resets_at in seconds or milliseconds.
        let codex = json!({"is_error": true, "result": "x", "usage_limit": {"resets_at": 1_900_000_000_000_i64}});
        assert_eq!(depleted(&codex).and_then(|d| d.resets_at), Some(1_900_000_000));
        let codex_unknown = json!({"is_error": true, "result": "x", "usage_limit": {"resets_at": null}});
        assert_eq!(depleted(&codex_unknown).map(|d| d.resets_at), Some(None));
        let worded = json!({"is_error": true, "result": "You've hit your limit - resets 3pm"});
        assert!(depleted(&worded).is_some());
    }

    #[test]
    fn an_answer_that_talks_about_limits_is_not_one() {
        let talk = json!({"is_error": false, "result": "You've hit your limit, he said; usage limit reached"});
        assert!(depleted(&talk).is_none());
        let other_error = json!({"is_error": true, "api_error_status": 500, "result": "boom"});
        assert!(depleted(&other_error).is_none());
        let overloaded = json!({"is_error": true, "result": "Overloaded"});
        assert!(depleted(&overloaded).is_none());
        assert!(depleted(&json!({"result": "hi"})).is_none());
    }

    #[test]
    fn runs_get_the_login_basics_and_hq_settings_only() {
        for name in [
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "TZ",
            "TERM",
            "HQ_HOME",
            "HQ_MODEL",
            "HQ_BIN",
            "KITH_HOME",
            "KITH_MODEL",
            "KITH_KERNEL_BIN",
        ] {
            assert!(inherited(name), "{name} should reach a run");
        }
        for name in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "OPENAI_API_KEY",
            "KITH_SERVE_HOME",
            "JEV_API_KEY",
            "TELEGRAM_BOT_TOKEN",
            "SLACK_APP_TOKEN",
            "DISCORD_BOT_TOKEN",
            "HQ_DISCORD_BOT_TOKEN",
            "KITH_DISCORD_BOT_TOKEN",
            "KITH_AGENT",
            "KITH_PERSON",
            "KITH_BRIDGE_OUTBOX",
            "HQ_AGENT",
            "HQ_PERSON",
            "HQ_RESUME",
            "HQ_BRIDGE_OUTBOX",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!inherited(name), "{name} must not reach a run");
        }
    }
}
