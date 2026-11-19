//! The sweep: for each listed session with no live writer, the 54-minute decision of its idle period, once (rule 5):
//! compact, sleep or end. A port of `sweep`, `one`, `compact` and `end` of `kernel/bin/kith-lifecycle`.
//!
//! Compact runs `claude -p --resume SID "/compact"` only while holding the session's writer claim and after checking again
//! under it (rule 11), under kith-stub (the conversation's writer lock). The claim is taken through [`Taker`], the one
//! implementation of `kith-sessions`' claims (Files and Writer mode), with `lifecycle_owner` so the intent and decision
//! that every other taker clears are kept (rules 12, 14).

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as _;
use serde_json::{Map, Value, json};

use super::proc::{self, COMPACT_TIMEOUT};
use super::transcript::{self, truthy};
use super::{decision, state_dir};
use crate::threads::registry::live_pid;
use crate::threads::writer::Taker;
use crate::threads::{Claim, Ctx, Host, Take, claim_path, read_claim};

/// The registry kinds the sweep looks at.
const KINDS: [&str; 4] = ["terminal", "ask", "schedule", "helper"];

/// What the sweep did to one session: `(session, decision, what happened)`.
pub type Acted = (String, String, String);

pub struct Sweep<'a> {
    pub c: &'a Ctx<'a>,
    pub host: &'a dyn Host,
    pub taker: &'a Taker<'a>,
    /// The kith program (`KITH_BIN`, else this one): kith-stub is looked for beside it.
    pub kith_bin: Option<PathBuf>,
    /// What the Python prints to stderr on the way.
    pub said: RefCell<String>,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn num(v: f64) -> Value {
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

fn str_of<'v>(e: &'v Map<String, Value>, k: &str) -> Option<&'v str> {
    e.get(k).and_then(Value::as_str)
}

fn same(v: Option<&Value>, since: f64) -> bool {
    v.and_then(Value::as_f64)
        .is_some_and(|x| (x - since).abs() < f64::EPSILON)
}

impl Sweep<'_> {
    fn say(&self, line: &str) {
        let mut s = self.said.borrow_mut();
        s.push_str(line);
        s.push('\n');
    }

    fn state(&self) -> Option<PathBuf> {
        state_dir(self.c)
    }

    /// The registry's sessions the lifecycle looks at, in file-name order.
    pub fn entries(&self) -> Vec<Map<String, Value>> {
        let Some(dir) = self.state().map(|s| crate::threads::registry_dir(&s)) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json") && p.is_file())
            .collect();
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        files
            .iter()
            .filter_map(|f| {
                match serde_json::from_str::<Value>(&std::fs::read_to_string(f).ok()?) {
                    Ok(Value::Object(o)) => Some(o),
                    _ => None,
                }
            })
            .filter(|e| {
                truthy(e.get("sessionId"))
                    && e.get("sessionId").is_some_and(Value::is_string)
                    && KINDS.contains(
                        &e.get("kind")
                            .map_or("terminal", |k| k.as_str().unwrap_or("")),
                    )
                    && truthy(e.get("transcriptPath"))
                    && e.get("transcriptPath").is_some_and(Value::is_string)
            })
            .collect()
    }

    /// A parked wake request or a waiter is still open for the session (a sign it will be back).
    fn waiting(&self, sid: &str) -> bool {
        let Some(s) = self.state() else { return false };
        let list = |sub: &str| -> Vec<PathBuf> {
            std::fs::read_dir(s.join("sessions").join(sub))
                .into_iter()
                .flatten()
                .flatten()
                .map(|f| f.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        };
        for f in list("waits") {
            if let Some(Ok(Value::Object(o))) = std::fs::read_to_string(&f)
                .ok()
                .map(|t| serde_json::from_str::<Value>(&t))
                && o.get("session").and_then(Value::as_str) == Some(sid)
            {
                return true;
            }
        }
        for f in list("queue") {
            if f.file_name().is_some_and(|n| n == "answered.json") {
                continue;
            }
            if let Ok(raw) = std::fs::read(&f)
                && String::from_utf8_lossy(&raw).contains(sid)
            {
                return true;
            }
        }
        false
    }

    /// The host keeps a conversation for this session: its own compactor handles that one, so this never does (rule 6, one
    /// compactor per session). False when the host is down or doesn't know it.
    fn host_owns(&self, sid: &str) -> bool {
        self.host.resolve_thread(sid).is_some_and(|r| {
            r.as_object().is_some_and(|o| {
                !o.is_empty() && !truthy(o.get("error")) && truthy(o.get("sessionId"))
            })
        })
    }

    /// Nobody else writes the session: no live claim but our own (`me`: the pid we claimed it for), no live claude process.
    fn writer_free(&self, e: &Map<String, Value>, me: Option<i64>) -> bool {
        let sid = str_of(e, "sessionId").unwrap_or("");
        let me = me.unwrap_or_else(|| i64::from(std::process::id()));
        // Writer mode: the claims are the core's, the claim we hold is the only writer; there are no files to look at.
        if self.taker.writer() {
            return live_pid(self.c, e).is_none();
        }
        let Some(dir) = self.c.reg_dir() else {
            return live_pid(self.c, e).is_none();
        };
        let held = [false, true].iter().any(|t| {
            read_claim(&claim_path(&dir, sid, *t))
                .filter(|c: &Claim| c.pid != Some(me))
                .is_some_and(|c| c.is_live())
        });
        !(held || live_pid(self.c, e).is_some())
    }

    fn decide_for(&self, e: &Map<String, Value>, at: Option<f64>) -> Option<&'static str> {
        let t = Path::new(str_of(e, "transcriptPath")?);
        let kind = str_of(e, "kind")
            .filter(|k| !k.is_empty())
            .unwrap_or("terminal");
        let life = transcript::cache_life(self.c.env);
        transcript::decide(t, kind, false, at, life, now())?;
        transcript::decide(
            t,
            kind,
            self.waiting(str_of(e, "sessionId")?),
            at,
            life,
            now(),
        )
    }

    /// This idle period already has its decision acted on (a compaction done, or ended).
    fn settled(&self, sid: &str, since: f64) -> bool {
        let Some(st) = self.state() else { return false };
        decision::read(&st, sid).is_some_and(|d| {
            same(d.get("idleSince"), since)
                && (truthy(d.get("compacted"))
                    || truthy(d.get("failed"))
                    || (str_of(&d, "decision") == Some("end") && truthy(d.get("ended"))))
        })
    }

    /// The checks under the claim (rules 11, 12): nothing changed since the decision was made.
    fn still(
        &self,
        e: &Map<String, Value>,
        want: &str,
        since: f64,
        at: Option<f64>,
        me: Option<i64>,
    ) -> bool {
        let (Some(sid), Some(t)) = (str_of(e, "sessionId"), str_of(e, "transcriptPath")) else {
            return false;
        };
        let Some(st) = self.state() else { return false };
        let t = Path::new(t);
        let Some(d) = decision::read(&st, sid) else {
            return false; // cleared by a resume
        };
        if str_of(&d, "decision") != Some(want)
            || !same(d.get("idleSince"), since)
            || truthy(d.get("compacted"))
        {
            return false; // cleared by a resume, or already done this period
        }
        if transcript::last_turn_at(t) != Some(since) || !self.writer_free(e, me) {
            return false;
        }
        if want == "compact" && transcript::compacted_since(t, since) {
            return false; // Claude compacted it itself since (rule 6, belt and braces)
        }
        if want == "compact" && transcript::context_tokens(t) < transcript::FLOOR_TOKENS {
            return false;
        }
        self.decide_for(e, at) == Some(want)
    }

    /// One try per idle period: a failed /compact is recorded and not retried until the session has a new turn.
    fn failed(&self, sid: &str, why: &str) -> String {
        if let Some(st) = self.state() {
            let mut rec = decision::read(&st, sid).unwrap_or_default();
            rec.insert("failed".to_owned(), json!(why));
            rec.insert("failedAt".to_owned(), num(now()));
            decision::write(&st, sid, &Value::Object(rec));
        }
        format!("failed: {why}")
    }

    fn mark(&self, sid: &str, keys: [(&str, Value); 2]) {
        if let Some(st) = self.state() {
            let mut rec = decision::read(&st, sid).unwrap_or_default();
            for (k, v) in keys {
                rec.insert(k.to_owned(), v);
            }
            decision::write(&st, sid, &Value::Object(rec));
        }
    }

    fn take(&self, sid: &str, pid: i64, e: &Map<String, Value>) -> Result<bool, String> {
        let how = Take {
            agent: str_of(e, "agent"),
            lifecycle_owner: true,
            ..Take::new("oneshot")
        };
        self.taker.take(sid, pid, &how).map(|held| held.is_some())
    }

    /// The compaction. The claim names claude's own pid (the stub's), not this sweep's (I11): a sweep that dies while claude
    /// writes leaves the claim held by the live writer, so nobody else gets in until claude has exited.
    pub fn compact(
        &self,
        e: &Map<String, Value>,
        since: f64,
        at: Option<f64>,
    ) -> Result<String, String> {
        let sid = str_of(e, "sessionId").unwrap_or("").to_owned();
        let t = str_of(e, "transcriptPath").unwrap_or("");
        let env = self.c.env;
        let claude = env("KITH_CLAUDE")
            .filter(|c| !c.is_empty())
            .or_else(|| proc::which("claude", env).map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "claude".to_owned());
        // the login the session was made under
        let login = Path::new(t)
            .ancestors()
            .nth(3)
            .map(Path::to_path_buf)
            .ok_or_else(|| "list index out of range".to_owned())?;
        let mut child_env: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(k, _)| {
                ![
                    "KITH_PERSON",
                    "KITH_VIA",
                    "CLAUDECODE",
                    "CLAUDE_CODE_ENTRYPOINT",
                ]
                .iter()
                .any(|n| k == n)
            })
            .collect();
        match child_env.iter_mut().find(|(k, _)| k == "CLAUDE_CONFIG_DIR") {
            Some(kv) => kv.1 = login.into_os_string(),
            None => child_env.push(("CLAUDE_CONFIG_DIR".into(), login.into_os_string())),
        }
        // The conversation's writer lock (a --direct run or a host turn on it is busy, not a second writer). The stub, not
        // claude, is the process the claim names: claude descends from it, and the claim ends with the stub.
        let kbin = env("KITH_BIN")
            .filter(|b| !b.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.kith_bin.clone());
        let (mut stub, problem) =
            proc::lock_stub(&sid, self.c.machine.as_deref(), kbin.as_deref(), env);
        // The stub says on a status pipe whether it holds the lock ("locked" / "busy"; nothing and EOF is a lock error):
        // claude can itself exit 75, 126 or 127, so the exit code alone cannot tell.
        let mut status = None;
        if !stub.is_empty() {
            let (r, w) = proc::pipe().map_err(|x| x.to_string())?;
            stub.pop();
            stub.push("--status-fd".into());
            stub.push(w.to_string().into());
            stub.push("--".into());
            status = Some((r, w));
        }
        let mut argv = stub.clone();
        argv.extend([
            claude.into(),
            "-p".into(),
            "--resume".into(),
            sid.clone().into(),
            "/compact".into(),
        ]);
        let cwd = str_of(e, "cwd").filter(|c| !c.is_empty()).unwrap_or("/");
        let group = !stub.is_empty();
        let parked = proc::parked(
            &argv,
            Path::new(cwd),
            &child_env,
            group,
            status.map(|s| s.1),
        );
        let p = match parked {
            Ok(p) => p,
            Err(x) => {
                if let Some((r, w)) = status {
                    proc::close(r);
                    proc::close(w);
                }
                return Err(x.to_string());
            }
        };
        if let Some((_, w)) = status {
            proc::close(w);
        }
        let mut reaped = false;
        let out = self.compact_parked(
            e,
            &sid,
            since,
            at,
            &p,
            status.map(|s| s.0),
            problem,
            group,
            &mut reaped,
        );
        if let Some((r, _)) = status {
            proc::close(r);
        }
        proc::close(p.go); // a child still parked exits without running claude
        proc::close(p.err);
        if !reaped {
            proc::reap(p.pid, None, false);
        }
        // Writer mode: the pid is gone, so the core's reaper ends the claim (as in the Python).
        self.taker.release_claims(i64::from(p.pid));
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn compact_parked(
        &self,
        e: &Map<String, Value>,
        sid: &str,
        since: f64,
        at: Option<f64>,
        p: &proc::Parked,
        status_r: Option<i32>,
        problem: Option<String>,
        group: bool,
        reaped: &mut bool,
    ) -> Result<String, String> {
        if self.take(sid, i64::from(p.pid), e)? {
            return Ok("held".to_owned());
        }
        if !self.still(e, "compact", since, at, Some(i64::from(p.pid))) {
            return Ok("skipped".to_owned());
        }
        if !str_of(e, "cwd").is_some_and(|c| !c.is_empty() && Path::new(c).is_dir()) {
            return Ok(self.failed(sid, "the session's folder is gone")); // --resume would look in the wrong project
        }
        if let Some(problem) = problem {
            if (self.c.env)("KITH_REQUIRE_STUB").as_deref() == Some("1") {
                return Ok(self.failed(sid, &problem));
            }
            self.say(&format!(
                "kith-lifecycle: {problem}; this compaction of {sid} is not locked against another run of the conversation"
            ));
        }
        proc::write_byte(p.go, b'1');
        let why = proc::read_to_end(p.err);
        if !why.is_empty() {
            return Ok(self.failed(sid, &p.why(&why))); // claude not on PATH, say
        }
        let rc = proc::reap(p.pid, Some(COMPACT_TIMEOUT), group);
        *reaped = true;
        let Some(rc) = rc else {
            return Ok(self.failed(sid, "timed out"));
        };
        if let Some(r) = status_r {
            let word = proc::stub_word(r);
            if word == "busy" {
                return Ok("busy".to_owned()); // no decision: the pending 'compact' stands, and the next sweep tries again
            }
            if word.is_empty() && rc != 0 {
                return Ok(self.failed(
                    sid,
                    &format!("the conversation lock could not be taken (kith-stub exit {rc})"),
                ));
            }
            if word == "locked" && (rc == 126 || rc == 127) {
                return Ok(self.failed(sid, &format!("claude did not start (exit {rc})")));
            }
        }
        if rc == 0 {
            self.mark(
                sid,
                [("compacted", json!(true)), ("compactedAt", num(now()))],
            );
            return Ok("compacted".to_owned());
        }
        Ok(self.failed(sid, &format!("exit {rc}")))
    }

    /// End is written only under the claim (rule 12).
    pub fn end(
        &self,
        e: &Map<String, Value>,
        since: f64,
        at: Option<f64>,
    ) -> Result<String, String> {
        let sid = str_of(e, "sessionId").unwrap_or("");
        let me = i64::from(std::process::id());
        if self.take(sid, me, e)? {
            return Ok("held".to_owned());
        }
        let out = if self.still(e, "end", since, at, None) {
            self.mark(sid, [("ended", json!(true)), ("endedAt", num(now()))]);
            "ended"
        } else {
            "skipped"
        };
        self.taker.release_claims(me);
        Ok(out.to_owned())
    }

    /// Nothing to look at: a decision (or a finished compaction, end or failure) is on record for this idle period and the
    /// transcript has not been written since, so the file is not parsed again. A pending 'compact' is always looked at.
    fn quiet(&self, e: &Map<String, Value>) -> bool {
        let (Some(sid), Some(t), Some(st)) = (
            str_of(e, "sessionId"),
            str_of(e, "transcriptPath"),
            self.state(),
        ) else {
            return true;
        };
        let d = decision::read(&st, sid);
        let Some(mtime) = std::fs::metadata(t)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
        else {
            return true;
        };
        let Some(d) = d else { return false };
        let decided = ["at", "compactedAt", "endedAt", "failedAt"]
            .iter()
            .map(|k| d.get(*k).and_then(Value::as_f64).unwrap_or(0.0))
            .fold(0.0, f64::max);
        mtime <= decided
            && (str_of(&d, "decision") != Some("compact")
                || truthy(d.get("compacted"))
                || truthy(d.get("failed")))
    }

    fn one(&self, e: &Map<String, Value>, at_sleep: bool) -> Result<Option<Acted>, String> {
        let sid = str_of(e, "sessionId").unwrap_or("").to_owned();
        let Some(st) = self.state() else {
            return Ok(None);
        };
        let Some(since) =
            transcript::last_turn_at(Path::new(str_of(e, "transcriptPath").unwrap_or("")))
        else {
            return Ok(None);
        };
        if self.settled(&sid, since) {
            return Ok(None);
        }
        let at = at_sleep.then(|| {
            since + {
                #[allow(clippy::cast_precision_loss)]
                let life = transcript::cache_life(self.c.env) as f64;
                life
            }
        });
        let Some(want) = self.decide_for(e, at) else {
            return Ok(None);
        };
        if self.host_owns(&sid) {
            return Ok(None);
        }
        let old = decision::read(&st, &sid);
        if old
            .as_ref()
            .is_none_or(|o| !same(o.get("idleSince"), since) || str_of(o, "decision") != Some(want))
        {
            decision::write(
                &st,
                &sid,
                &json!({"decision": want, "at": now(), "idleSince": since, "compacted": false}),
            );
            if want == "sleep" {
                return Ok(Some((sid, want.to_owned(), "asleep".to_owned())));
            }
        } else if want == "sleep" {
            return Ok(None);
        }
        let did = if want == "compact" {
            self.compact(e, since, at)?
        } else {
            self.end(e, since, at)?
        };
        Ok(Some((sid, want.to_owned(), did)))
    }

    /// One sweep at a time (a non-blocking lock: the tick starts one every minute and a compaction can take minutes);
    /// one session failing never ends the sweep.
    pub fn sweep(&self, only: Option<&str>, at_sleep: bool) -> Vec<Acted> {
        let Some(s) = self.state() else {
            return Vec::new();
        };
        let dir = s.join("sessions");
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir);
        let Ok(lock) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lifecycle.lock"))
        else {
            return Vec::new();
        };
        // flock, as the Python's: a Python sweep and a Rust one keep each other out
        if lock.try_lock_exclusive().is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for e in self.entries() {
            let sid = str_of(&e, "sessionId").unwrap_or("").to_owned();
            if only.is_some_and(|o| o != sid)
                || (only.is_none() && self.quiet(&e))
                || !self.writer_free(&e, None)
            {
                continue;
            }
            match self.one(&e, at_sleep) {
                Ok(Some(got)) => out.push(got),
                Ok(None) => {}
                Err(x) => out.push((sid, "?".to_owned(), format!("error: {x}"))),
            }
        }
        out
    }
}
