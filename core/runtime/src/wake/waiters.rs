//! Waiters: code that watches for one thing and turns it into one wake request, then is gone (`kernel/bin/kith-wake`,
//! "waiters"). A waiter is not a model: `poll` looks at each one through [`Outside`] (the clock, the process table, `gh`)
//! and, when what it waits for happened or its timeout passed, it becomes a request. A CI wake's id names the PR's head
//! commit, so two waiters on one PR wake the session once (rule 13 drops the second).
//!
//! Waiters come from the owner's terminal and one-shot runs for now: a host chat's or a guest's session wakes through the
//! host. A guest's `ci:` would reach GitHub as the owner (rule 10), so a waiter for a `KITH_PERSON` is refused. Rule T: Kith
//! delivers only the results of waits it registered; an MCP task is Claude's to deliver.

use std::fmt::Write as _;

use super::model::{State, Waiter, Watch, days_from_civil, stamp};
use super::outside::{Civil, Outside};
use super::queue::{New, enqueue, refused};
use crate::error::Result;

/// A waiter always has a timeout.
pub(crate) const WAIT_TIMEOUT: &str = "6h";
/// The `KITH_VIA` of runs whose ended session the run door resumes.
pub(crate) const RUN_VIAS: &[&str] = &["terminal", "ask", "schedule", "wake"];

/// Removes the waiters of a session. Returns how many.
pub(crate) fn unwait(st: &mut State, session: &str) -> usize {
    let before = st.waiters.len();
    st.waiters.retain(|_, w| w.session != session);
    before - st.waiters.len()
}

/// Python's `repr` of a string, enough for an error message: single quotes unless the text holds one and no double quote.
fn repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "\\'"))
    }
}

/// `30s`, `10m`, `6h`, `2d`, as seconds.
fn seconds(s: &str) -> Result<f64> {
    let bad = || refused(format!("{} is not a duration (30s, 10m, 6h, 2d)", repr(s)));
    let t = s.trim();
    let unit = t.chars().last().ok_or_else(bad)?;
    let each: f64 = match unit {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        _ => return Err(bad()),
    };
    let digits = &t[..t.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    #[allow(clippy::cast_precision_loss)]
    let n = digits.parse::<u64>().map_err(|_| bad())? as f64;
    Ok(n * each)
}

/// A time as `datetime.fromisoformat` takes it: `2026-10-27`, `2026-10-27T15:00[:00[.5]]`, with a space for the `T`, and
/// `Z` or `+hh:mm` after. The offset in seconds when it has one.
fn parse_iso(s: &str) -> Option<(Civil, Option<i64>)> {
    fn num(s: &str, len: usize) -> Option<i64> {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    }
    let date = s.get(..10)?;
    let year = num(date.get(..4)?, 4)?;
    let month = num(date.get(5..7)?, 2)?;
    let day = num(date.get(8..10)?, 2)?;
    if date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return None;
    }
    let mut c = Civil {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
    };
    let mut offset = None;
    let rest = &s[10..];
    if !rest.is_empty() {
        let rest = rest.strip_prefix(['T', ' '])?;
        let (clock, tz) = match rest.find(['Z', '+', '-']) {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let clock = clock.split_once('.').map_or(clock, |(h, frac)| {
            if frac.bytes().all(|b| b.is_ascii_digit()) && !frac.is_empty() {
                h
            } else {
                "?"
            }
        });
        let mut parts = clock.split(':');
        c.hour = num(parts.next()?, 2)?;
        if let Some(m) = parts.next() {
            c.minute = num(m, 2)?;
        }
        if let Some(sec) = parts.next() {
            c.second = num(sec, 2)?;
        }
        if parts.next().is_some() {
            return None;
        }
        if !tz.is_empty() {
            offset = Some(if tz == "Z" {
                0
            } else {
                let sign = if tz.starts_with('-') { -1 } else { 1 };
                let body = tz[1..].replace(':', "");
                let h = num(body.get(..2)?, 2)?;
                let m = if body.len() > 2 {
                    num(body.get(2..4)?, 2)?
                } else {
                    0
                };
                sign * (h * 3600 + m * 60)
            });
        }
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let ok = (1..=12).contains(&month)
        && day >= 1
        && usize::try_from(month - 1).is_ok_and(|m| day <= days[m])
        && c.hour < 24
        && c.minute < 60
        && c.second < 60
        && year >= 1;
    ok.then_some((c, offset))
}

/// WHAT as the waiter's fields; refuses what cannot be watched.
fn parse_on(out: &dyn Outside, now: f64, on: &str) -> Result<Watch> {
    let (kind, arg) = on.split_once(':').unwrap_or((on, ""));
    match kind {
        "ci" => {
            let word = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
            };
            let parsed = arg.split_once('#').and_then(|(slug, pr)| {
                let (owner, repo) = slug.split_once('/')?;
                let ok = word(owner)
                    && word(repo)
                    && !pr.is_empty()
                    && pr.bytes().all(|b| b.is_ascii_digit());
                ok.then(|| {
                    Some(Watch::Ci {
                        repo: slug.to_owned(),
                        pr: pr.parse().ok()?,
                    })
                })
                .flatten()
            });
            parsed.ok_or_else(|| refused("ci:OWNER/REPO#PR, e.g. ci:Ying-Kai-Liao/kith#345"))
        }
        "pid" => {
            let pid = (!arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()))
                .then(|| arg.parse::<i64>().ok())
                .flatten()
                .filter(|p| out.alive(*p, None));
            let Some(pid) = pid else {
                return Err(refused(format!("pid {arg} is not running")));
            };
            Ok(Watch::Pid {
                pid: Some(pid),
                proc_start: out.proc_start(pid),
            })
        }
        "at" => {
            let t = if let Some(rel) = arg.strip_prefix('+') {
                now + seconds(rel)?
            } else {
                let not_a_time = || {
                    refused(format!(
                        "at:{arg} is not a time (+30m, or 2026-10-27T15:00)"
                    ))
                };
                let (civil, offset) = parse_iso(arg).ok_or_else(not_a_time)?;
                #[allow(clippy::cast_precision_loss)]
                match offset {
                    Some(off) => {
                        let days = days_from_civil(civil.year, civil.month, civil.day);
                        (days * 86_400 + civil.hour * 3600 + civil.minute * 60 + civil.second - off)
                            as f64
                    }
                    None => out.local_epoch(civil).ok_or_else(not_a_time)?,
                }
            };
            Ok(Watch::At { at: t })
        }
        "task" | "mcp" | "mcp-task" => Err(refused(
            "an MCP task is Claude's to deliver (its sidecar, in the session's own turn); Kith never waits on or delivers it. \
             Use a waiter on a process, a PR or a time instead",
        )),
        _ => Err(refused(format!(
            "can't wait on {}: ci:OWNER/REPO#PR, pid:N or at:TIME",
            repr(on)
        ))),
    }
}

/// What a caller asks to wait for. `via` and `person` are the caller's `KITH_VIA` and `KITH_PERSON`.
pub(crate) struct Ask<'a> {
    pub on: &'a str,
    pub session: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub why: &'a str,
    pub note: &'a str,
    pub timeout: &'a str,
    pub via: Option<&'a str>,
    pub person: Option<&'a str>,
}

/// Sets a waiter for a session. One session waiting twice on the same thing keeps the first waiter.
pub(crate) fn wait(st: &mut State, out: &dyn Outside, now: f64, a: &Ask<'_>) -> Result<Waiter> {
    if a.person.is_some_and(|p| !p.is_empty()) {
        // Rule 10 wants a guest's waiter checked inside that person's sandbox. None exists to run it in: a waiter here is code
        // of the tick, which runs as the owner outside every guest sandbox.
        return Err(refused(
            "a guest's session can't set waiters yet: the check would run as the owner, outside their sandbox",
        ));
    }
    let via = a.via.filter(|v| !v.is_empty()).unwrap_or("terminal");
    if !RUN_VIAS.contains(&via) {
        return Err(refused(format!(
            "a {via} session wakes through the host, which doesn't set waiters yet"
        )));
    }
    let (Some(session), Some(agent)) = (
        a.session.filter(|s| !s.is_empty()),
        a.agent.filter(|s| !s.is_empty()),
    ) else {
        return Err(refused(
            "no session to wake: run it in a kith agent's session, or pass --session and --agent",
        ));
    };
    let watch = parse_on(out, now, a.on)?;
    if let Some(old) = st
        .waiters
        .values()
        .find(|w| w.session == session && w.on == a.on)
    {
        return Ok(old.clone());
    }
    let until = match &watch {
        Watch::At { at } => *at,
        _ => now + seconds(a.timeout)?,
    };
    let w = Waiter {
        id: out.new_id(),
        on: a.on.to_owned(),
        session: session.to_owned(),
        agent: agent.to_owned(),
        why: a.why.to_owned(),
        note: a.note.to_owned(),
        watch,
        created: now,
        checked: 0.0,
        seen: None,
        until,
        format: 1,
    };
    st.waiters.insert(w.id.clone(), w.clone());
    Ok(w)
}

fn what(w: &Waiter) -> String {
    match &w.watch {
        Watch::Ci { repo, pr } => format!("the checks of {repo}#{pr} finish"),
        Watch::Pid { pid, .. } => format!(
            "pid {} exits",
            pid.map_or_else(|| "None".into(), |p| p.to_string())
        ),
        Watch::At { at } => format!("it is {}", stamp(*at)),
    }
}

/// What `wait` tells the caller.
pub(crate) fn told(w: &Waiter) -> String {
    let when = if let Watch::At { .. } = w.watch {
        format!("at {}", stamp(w.until))
    } else {
        format!(
            "when {}, or at {} if that hasn't happened",
            what(w),
            stamp(w.until)
        )
    };
    format!(
        "waiting {}: this session gets a turn {when}; while a terminal holds it, the turn waits for the terminal to close",
        w.id
    )
}

/// The PR's checks, or `None` while they run. Finished means the same head commit had no pending check on two looks in a
/// row: just after a push, GitHub may not have listed the new runs yet.
fn ci_result(out: &dyn Outside, w: &mut Waiter, repo: &str, pr: i64) -> Option<(String, String)> {
    let sha = out.pr_view(repo, pr).unwrap_or_default();
    let (sha, state) = (sha.sha?, sha.state);
    if let Some(s) = state.filter(|s| s == "MERGED" || s == "CLOSED") {
        return Some((sha, format!("{repo}#{pr} was {}", s.to_lowercase())));
    }
    let checks = out.pr_checks(repo, pr).unwrap_or_default();
    if checks.is_empty() || checks.iter().any(|c| c.bucket == "pending") {
        w.seen = None;
        return None;
    }
    if w.seen.as_deref() != Some(&sha) {
        w.seen = Some(sha);
        return None;
    }
    let ran: Vec<_> = checks.iter().filter(|c| c.bucket != "skipping").collect();
    let bad: Vec<&str> = ran
        .iter()
        .filter(|c| c.bucket == "fail" || c.bucket == "cancel")
        .map(|c| c.name.as_str())
        .collect();
    let said = if bad.is_empty() {
        format!("all {} checks passed", ran.len())
    } else {
        format!(
            "{} of {} checks failed: {}",
            bad.len(),
            ran.len(),
            bad.join(", ")
        )
    };
    Some((sha, said))
}

/// `(wake id, why, text)` when the waiter's thing happened or its timeout passed.
fn fired(out: &dyn Outside, w: &mut Waiter, now: f64) -> Option<(String, String, String)> {
    let why_or = |w: &Waiter, d: String| if w.why.is_empty() { d } else { w.why.clone() };
    match w.watch.clone() {
        Watch::Pid { pid, proc_start } => {
            if !pid.is_some_and(|p| out.alive(p, proc_start.as_deref())) {
                let d = format!(
                    "pid {} exited",
                    pid.map_or_else(|| "None".into(), |p| p.to_string())
                );
                return Some((
                    format!("wait:{}", w.id),
                    why_or(w, d),
                    "the process you waited for has exited".into(),
                ));
            }
        }
        Watch::At { at } => {
            if now >= at {
                return Some((
                    format!("wait:{}", w.id),
                    why_or(w, "time".into()),
                    format!("it is {}, the time you asked to be woken", stamp(at)),
                ));
            }
        }
        Watch::Ci { repo, pr } => {
            if now - w.checked >= out.ci_poll() {
                w.checked = now;
                if let Some((sha, said)) = ci_result(out, w, &repo, pr) {
                    // One wake per PR head and session, however many waiters asked.
                    let d = format!(
                        "CI finished on {repo}#{pr} at {}",
                        sha.chars().take(7).collect::<String>()
                    );
                    return Some((
                        format!(
                            "ci:{repo}#{pr}@{}/{}",
                            sha.chars().take(12).collect::<String>(),
                            w.session
                        ),
                        why_or(w, d),
                        said,
                    ));
                }
            }
        }
    }
    (now >= w.until).then(|| {
        (
            format!("wait:{}", w.id),
            why_or(w, format!("waiting until {}", what(w))),
            format!(
                "gave up: it had not happened by {} (the waiter's timeout)",
                stamp(w.until)
            ),
        )
    })
}

/// `WaiterResult`: every waiter whose thing happened becomes one wake request. A waiter whose session's item belongs to
/// someone else is dropped (the refusal is the Python's `wait-error`). Returns how many fired.
pub(crate) fn poll(st: &mut State, out: &dyn Outside, now: f64) -> usize {
    let mut fired_n = 0;
    let ids: Vec<String> = st.waiters.keys().cloned().collect();
    for id in ids {
        let Some(mut w) = st.waiters.remove(&id) else {
            continue;
        };
        let Some((rid, why, mut text)) = fired(out, &mut w, now) else {
            st.waiters.insert(id, w);
            continue;
        };
        fired_n += 1;
        if !w.note.is_empty() {
            let _ = write!(text, "\nYour note when you set the wait: {}", w.note);
        }
        let _ = enqueue(
            st,
            now,
            &New {
                session: &w.session,
                id: &rid,
                agent: &w.agent,
                text: &text,
                person: None,
                room: None,
                door: "run",
                why: &why,
            },
        );
    }
    fired_n
}
