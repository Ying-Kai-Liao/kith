//! The wake queue as values: what one session is owed, what a waiter waits for, what was answered. [`State`] is all of it
//! (core.db's `wake_*` and `waiters` tables, or the Python's files read in), and the rules in `queue.rs` and `waiters.rs`
//! are functions over it, so they run the same over rows, over files and in a test.
//!
//! `to_json` / `from_json` are the Python's file formats (`kernel/bin/kith-wake`, "Files"): `ls --json` prints them and the
//! import reads them.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value, json};
use sha2::{Digest as _, Sha256};

/// How long an answered id is remembered.
pub(crate) const KEEP_DAYS: f64 = 7.0;

/// One thing that happened for a session. `run` is the turn that carries it (it stays in the queue until that turn ends).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Request {
    pub id: String,
    pub why: String,
    pub text: String,
    pub at: String,
    pub run: Option<String>,
}

/// The turn in flight for a session: held by `pid` (with its start time, so a reused pid is not mistaken for it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Turn {
    pub run: String,
    pub pid: Option<i64>,
    pub proc_start: Option<String>,
    pub at: String,
    pub sid: Option<String>,
}

/// What one session is owed: its requests, merged into one turn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub session: String,
    pub agent: String,
    pub person: Option<String>,
    pub room: Option<String>,
    /// `run` or `host`.
    pub door: String,
    pub requests: Vec<Request>,
    pub turn: Option<Turn>,
    pub tries: i64,
    /// Epoch seconds before which it is not offered again.
    pub next: f64,
    /// The version of the file this came from.
    pub format: i64,
}

/// An id that left the queue: how, when and by which turn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Answered {
    pub how: String,
    pub t: f64,
    pub run: Option<String>,
}

/// What a waiter waits for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Watch {
    Ci {
        repo: String,
        pr: i64,
    },
    Pid {
        pid: Option<i64>,
        proc_start: Option<String>,
    },
    At {
        at: f64,
    },
}

impl Watch {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Ci { .. } => "ci",
            Self::Pid { .. } => "pid",
            Self::At { .. } => "at",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Waiter {
    pub id: String,
    /// What the caller asked for, as typed (`ci:o/r#5`).
    pub on: String,
    pub session: String,
    pub agent: String,
    pub why: String,
    pub note: String,
    pub watch: Watch,
    pub created: f64,
    pub checked: f64,
    /// The head commit a CI waiter saw finished on its last look.
    pub seen: Option<String>,
    pub until: f64,
    pub format: i64,
}

/// Everything. Items and waiters are keyed by session and id; `order` is the order `ls` prints them in.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct State {
    pub items: BTreeMap<String, Item>,
    pub answered: BTreeMap<String, Answered>,
    pub waiters: BTreeMap<String, Waiter>,
}

/// The Python names an item's file after the hash of its session and lists them in file-name order.
pub(crate) fn file_key(session: &str) -> String {
    let digest = Sha256::digest(session.as_bytes());
    digest
        .iter()
        .take(16)
        .fold(String::with_capacity(32), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl State {
    /// The sessions in the order `kith-wake ls` lists them.
    pub(crate) fn order(&self) -> Vec<String> {
        let mut v: Vec<(String, String)> = self
            .items
            .keys()
            .map(|s| (file_key(s), s.clone()))
            .collect();
        v.sort();
        v.into_iter().map(|(_, s)| s).collect()
    }

    /// Forgets the ids answered more than [`KEEP_DAYS`] ago (the Python prunes on every read).
    pub(crate) fn prune(&mut self, now: f64) {
        let cut = now - KEEP_DAYS * 86400.0;
        self.answered.retain(|_, a| a.t >= cut);
    }

    /// The id's request, with the session it is queued for.
    pub(crate) fn find_request(&self, id: &str) -> Option<(&Item, &Request)> {
        self.items
            .values()
            .find_map(|it| it.requests.iter().find(|r| r.id == id).map(|r| (it, r)))
    }

    /// Records ids as answered.
    pub(crate) fn mark_answered(&mut self, ids: &[String], how: &str, run: Option<&str>, now: f64) {
        for id in ids {
            self.answered.insert(
                id.clone(),
                Answered {
                    how: how.to_owned(),
                    t: now,
                    run: run.map(str::to_owned),
                },
            );
        }
    }

    /// The Python's `save`: an item with no request left is gone.
    pub(crate) fn settle(&mut self, session: &str) {
        if self
            .items
            .get(session)
            .is_some_and(|it| it.requests.is_empty())
        {
            self.items.remove(session);
        }
    }
}

// --- time ---------------------------------------------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-10-27T15:00:00Z`: the Python's `stamp`, UTC, whole seconds.
pub(crate) fn stamp(t: f64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    let secs = t.floor() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

// --- the Python's file formats ------------------------------------------------------------------------------------------

/// A JSON number the way Python writes it: whole numbers without a fraction.
pub(crate) fn number(x: f64) -> Value {
    #[allow(clippy::cast_possible_truncation)]
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return Value::Number(Number::from(x as i64));
    }
    Number::from_f64(x).map_or(Value::Null, Value::Number)
}

fn opt(s: Option<&String>) -> Value {
    s.map_or(Value::Null, |s| Value::String(s.clone()))
}

impl Request {
    fn to_json(&self) -> Value {
        json!({"id": self.id, "why": self.why, "text": self.text, "at": self.at, "run": opt(self.run.as_ref())})
    }
}

impl Item {
    /// The item as `ls --json` prints it (and as the Python's queue file holds it).
    pub(crate) fn to_json(&self) -> Value {
        let turn = self.turn.as_ref().map_or(Value::Null, |t| {
            json!({
                "run": t.run,
                "pid": t.pid,
                "procStart": opt(t.proc_start.as_ref()),
                "at": t.at,
                "sid": opt(t.sid.as_ref()),
            })
        });
        json!({
            "v": self.format,
            "session": self.session,
            "agent": self.agent,
            "person": opt(self.person.as_ref()),
            "room": opt(self.room.as_ref()),
            "door": self.door,
            "requests": self.requests.iter().map(Request::to_json).collect::<Vec<_>>(),
            "turn": turn,
            "tries": self.tries,
            "next": number(self.next),
        })
    }
}

impl Waiter {
    pub(crate) fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("v".into(), json!(self.format));
        o.insert("id".into(), json!(self.id));
        o.insert("on".into(), json!(self.on));
        o.insert("session".into(), json!(self.session));
        o.insert("agent".into(), json!(self.agent));
        o.insert("why".into(), json!(self.why));
        o.insert("note".into(), json!(self.note));
        o.insert("created".into(), number(self.created));
        o.insert("checked".into(), number(self.checked));
        o.insert("seen".into(), opt(self.seen.as_ref()));
        o.insert("until".into(), number(self.until));
        o.insert("kind".into(), json!(self.watch.kind()));
        match &self.watch {
            Watch::Ci { repo, pr } => {
                o.insert("repo".into(), json!(repo));
                o.insert("pr".into(), json!(pr));
            }
            Watch::Pid { pid, proc_start } => {
                o.insert("pid".into(), json!(pid));
                o.insert("procStart".into(), opt(proc_start.as_ref()));
            }
            Watch::At { at } => {
                o.insert("at".into(), number(*at));
            }
        }
        Value::Object(o)
    }
}

/// A non-empty string field (the Python reads an empty one as absent: `x or None`).
fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn plain(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn float(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

fn whole(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| {
        x.as_i64().or_else(|| {
            x.as_f64().map(|f| {
                #[allow(clippy::cast_possible_truncation)]
                let n = f as i64;
                n
            })
        })
    })
}

/// The version a file declares: absent means 1; anything but 1 is a format this build does not know.
fn version(v: &Value) -> Result<i64, String> {
    match v.get("v") {
        None | Some(Value::Null) => Ok(1),
        Some(x) if x.as_i64() == Some(1) => Ok(1),
        Some(x) => Err(format!("version {x} (this build reads version 1)")),
    }
}

impl Item {
    /// A queue file's content. `Ok(None)`: not an item (damaged, or no session: the Python lists no such file). `Err`: an
    /// item in a format this build does not know.
    pub(crate) fn from_json(v: &Value) -> Result<Option<Self>, String> {
        let Some(session) = text(v, "session") else {
            return Ok(None);
        };
        let format = version(v)?;
        let door = text(v, "door").unwrap_or_else(|| "run".into());
        if door != "run" && door != "host" {
            return Err(format!("door {door:?} (run or host)"));
        }
        let mut requests = Vec::new();
        for r in v
            .get("requests")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let Some(id) = text(r, "id") else {
                return Ok(None);
            };
            requests.push(Request {
                id,
                why: plain(r, "why"),
                text: plain(r, "text"),
                at: plain(r, "at"),
                run: text(r, "run"),
            });
        }
        let turn = v.get("turn").filter(|t| t.is_object()).and_then(|t| {
            Some(Turn {
                run: text(t, "run")?,
                pid: whole(t, "pid"),
                proc_start: text(t, "procStart"),
                at: plain(t, "at"),
                sid: text(t, "sid"),
            })
        });
        Ok(Some(Self {
            session,
            agent: plain(v, "agent"),
            person: text(v, "person"),
            room: text(v, "room"),
            door,
            requests,
            turn,
            tries: whole(v, "tries").unwrap_or(0),
            next: float(v, "next").unwrap_or(0.0),
            format,
        }))
    }
}

impl Waiter {
    /// A waiter file's content; `Ok(None)` when it is not one the Python could read.
    pub(crate) fn from_json(v: &Value) -> Result<Option<Self>, String> {
        let (Some(id), Some(session)) = (text(v, "id"), text(v, "session")) else {
            return Ok(None);
        };
        let format = version(v)?;
        let watch = match v.get("kind").and_then(Value::as_str) {
            Some("ci") => match (text(v, "repo"), whole(v, "pr")) {
                (Some(repo), Some(pr)) => Watch::Ci { repo, pr },
                _ => return Ok(None),
            },
            Some("pid") => Watch::Pid {
                pid: whole(v, "pid"),
                proc_start: text(v, "procStart"),
            },
            Some("at") => match float(v, "at") {
                Some(at) => Watch::At { at },
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        let Some(until) = float(v, "until") else {
            return Ok(None);
        };
        Ok(Some(Self {
            id,
            on: plain(v, "on"),
            session,
            agent: plain(v, "agent"),
            why: plain(v, "why"),
            note: plain(v, "note"),
            watch,
            created: float(v, "created").unwrap_or(0.0),
            checked: float(v, "checked").unwrap_or(0.0),
            seen: text(v, "seen"),
            until,
            format,
        }))
    }
}

impl Answered {
    /// One entry of `answered.json`; `None` when it is not a dict (the Python drops such entries).
    pub(crate) fn from_json(v: &Value) -> Option<Self> {
        v.is_object().then(|| Self {
            how: plain(v, "how"),
            t: float(v, "t").unwrap_or(0.0),
            run: text(v, "run"),
        })
    }
}
