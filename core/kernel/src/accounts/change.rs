//! What changes the accounts' state: a mark out, a clear, a start, a usage reading (`mark_out`, `clear_out`,
//! `record_started`, `record_usage` of `kernel/lib/kith_accounts.py`). Each is one pure edit of the `accounts` object, so
//! the file (under `state.json.lock`) and the core (inside one transaction on `account_state`) apply the very same code
//! and end in the same state. Where the state lives is a [`Store`]: the file until `kith migrate step accounts`, the core
//! after (the host installs the core's store for the verbs it runs, see [`use_store`]).

use super::{Map, clock, state};
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;

/// How long an account stays out when the provider did not say when its limit lifts (`OUT_FOR`).
pub const OUT_FOR: i64 = 3600;

/// One edit of an account's entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Out until `until` (epoch seconds; none or 0: an hour from now). With `extend_only` an entry already out for longer
    /// stays so.
    Out {
        id: String,
        until: Option<i64>,
        reason: String,
        extend_only: bool,
    },
    /// Ready again; `host_until` is the host's own mark that was standing then (0: none), so a host mark made before this
    /// clear is ignored.
    Clear { id: String, host_until: i64 },
    /// A launch started on it.
    Started { id: String },
    /// A validated usage reading ([`validate_usage`]).
    Usage { id: String, usage: Value },
}

impl Change {
    pub fn id(&self) -> &str {
        match self {
            Change::Out { id, .. }
            | Change::Clear { id, .. }
            | Change::Started { id }
            | Change::Usage { id, .. } => id,
        }
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `accts.setdefault(id, {})`: a missing (or damaged) entry becomes an empty one.
fn entry<'a>(accts: &'a mut Map, id: &str) -> &'a mut Map {
    let e = accts
        .entry(id.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !e.is_object() {
        *e = Value::Object(Map::new());
    }
    match e {
        Value::Object(m) => m,
        _ => unreachable!("made an object just above"),
    }
}

/// Applies `change` to the accounts as the Python writers do, `now` in epoch seconds. Like them it makes the entry even
/// when the change then leaves it alone (`mark_out` of an account already out for longer).
#[allow(clippy::cast_precision_loss)] // epoch seconds fit an f64
pub fn apply(accts: &mut Map, change: &Change, now: i64) {
    let e = entry(accts, change.id());
    match change {
        Change::Out {
            until,
            reason,
            extend_only,
            ..
        } => {
            let until = until.filter(|u| *u != 0).unwrap_or(now + OUT_FOR);
            // A missing `out_until` reads as 0, as `e.get('out_until', 0)`.
            let cur = e.get("out_until").map_or(Some(0.0), number);
            if *extend_only && cur.is_some_and(|c| c > until.max(now) as f64) {
                return;
            }
            let since = match cur {
                Some(c) if c > now as f64 => {
                    e.get("since").cloned().unwrap_or_else(|| Value::from(now))
                }
                _ => Value::from(now),
            };
            e.insert("since".to_owned(), since);
            e.insert("out_until".to_owned(), Value::from(until));
            e.insert("reason".to_owned(), Value::from(reason.as_str()));
        }
        Change::Clear { host_until, .. } => {
            for k in ["out_until", "reason", "since"] {
                e.shift_remove(k);
            }
            e.insert("cleared_at".to_owned(), Value::from(now));
            e.insert("cleared_host_until".to_owned(), Value::from(*host_until));
        }
        Change::Started { .. } => {
            e.insert("last_started".to_owned(), Value::from(now));
        }
        Change::Usage { usage, .. } => {
            e.insert("usage".to_owned(), usage.clone());
        }
    }
}

/// The host's own out mark for `id` while the host still keeps one (`backups.json`), for a clear to remember:
/// `int(out_until)`, or 0.
pub fn host_until(machine: &Path, id: &str) -> i64 {
    state::host_out_until(machine, id)
}

// --- usage readings and `--until` -------------------------------------------------------------------------------------

/// What Python's `$` allows before the end: one trailing newline.
fn dollar(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

/// `WINDOW_RE`: `^[A-Za-z0-9][A-Za-z0-9 _.-]{0,31}$`.
fn window_name_ok(s: &str) -> bool {
    let s = dollar(s);
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().count() <= 32
        && cs.all(|c| c.is_ascii_alphanumeric() || " _.-".contains(c))
}

/// `REASON_RE`: `^[a-z0-9][a-z0-9 _.-]{0,39}$`.
pub fn reason_ok(s: &str) -> bool {
    let s = dollar(s);
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars().count() <= 40
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || " _.-".contains(c))
}

fn time_value(v: &Value) -> Option<f64> {
    number(v).filter(|f| *f > 0.0)
}

const READING: &str =
    "a usage reading is {\"windows\": [{\"name\", \"percent\", \"resets_at\"?}], \"read_at\"?}";
const WINDOW: &str = "a window is {\"name\", \"percent\", \"resets_at\"?}";

/// `validate_usage`: a reading as the state keeps it, or what is wrong with it. `now` supplies a missing `read_at`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn validate_usage(reading: &Value, now: i64) -> Result<Value, String> {
    let Some(r) = reading.as_object() else {
        return Err(READING.to_owned());
    };
    if r.keys().any(|k| k != "windows" && k != "read_at") {
        return Err(READING.to_owned());
    }
    let wins = match r.get("windows") {
        Some(Value::Array(w)) if !w.is_empty() && w.len() <= 8 => w,
        _ => return Err("\"windows\" is a list of 1 to 8 windows".to_owned()),
    };
    let mut out = Vec::new();
    for w in wins {
        let Some(w) = w.as_object() else {
            return Err(WINDOW.to_owned());
        };
        if w.keys()
            .any(|k| !["name", "percent", "resets_at"].contains(&k.as_str()))
        {
            return Err(WINDOW.to_owned());
        }
        let name = match w.get("name") {
            Some(Value::String(n)) if window_name_ok(n) => n.clone(),
            _ => return Err("a window name is up to 32 letters, digits, space . _ -".to_owned()),
        };
        let Some(pct) = w
            .get("percent")
            .and_then(number)
            .filter(|p| (0.0..=100.0).contains(p))
        else {
            return Err("percent is a number from 0 to 100".to_owned());
        };
        let mut item = Map::new();
        item.insert("name".to_owned(), Value::from(name));
        item.insert("percent".to_owned(), Value::from(pct));
        match w.get("resets_at") {
            None | Some(Value::Null) => {}
            Some(v) => {
                let Some(t) = time_value(v) else {
                    return Err("resets_at is a Unix time in seconds".to_owned());
                };
                item.insert("resets_at".to_owned(), Value::from(t as i64));
            }
        }
        out.push(Value::Object(item));
    }
    let at = match r.get("read_at") {
        None => now as f64,
        Some(v) => time_value(v).ok_or_else(|| "read_at is a Unix time in seconds".to_owned())?,
    };
    let mut m = Map::new();
    m.insert("read_at".to_owned(), Value::from(at as i64));
    m.insert("windows".to_owned(), Value::Array(out));
    Ok(Value::Object(m))
}

fn unreadable(text: &str) -> String {
    format!(
        "'{text}' is not a time: use a Unix time, a span like 90m or 2h, or a local date and time (2026-11-04 09:30)"
    )
}

/// `parse_until`: `--until T`: a Unix time, a span (90m, 2h, 1d, +30s), or a local date and time.
pub fn parse_until(text: &str, now: i64) -> Result<i64, String> {
    use chrono::TimeZone as _;
    let t = text.trim();
    if (9..=11).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse().map_err(|_| unreadable(text));
    }
    let span = t.strip_prefix('+').unwrap_or(t);
    if let Some(unit) = span.chars().last()
        && let Some(per) = match unit {
            's' => Some(1),
            'm' => Some(60),
            'h' => Some(3600),
            'd' => Some(86400),
            _ => None,
        }
    {
        let n = span[..span.len() - 1].trim_end();
        if !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = n.parse::<i64>()
        {
            return Ok(now + n * per);
        }
    }
    let local = |dt: chrono::NaiveDateTime| {
        chrono::Local
            .from_local_datetime(&dt)
            .earliest()
            .map_or_else(|| dt.and_utc().timestamp(), |l| l.timestamp())
    };
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, fmt) {
            return Ok(local(dt));
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d")
        && let Some(dt) = d.and_hms_opt(0, 0, 0)
    {
        return Ok(local(dt));
    }
    Err(unreadable(text))
}

// --- where the state lives --------------------------------------------------------------------------------------------

/// Where the accounts' state is kept once it is not the file: the core's table. The host installs one for the verbs it runs
/// after `kith migrate step accounts`; until then the file answers.
pub trait Store: Send + Sync {
    /// Every account's state, as `state.json`'s `accounts` object.
    fn accounts(&self) -> Result<Map, String>;
    /// One change, as one transaction.
    fn apply(&self, change: &Change, now: i64) -> Result<(), String>;
}

static STORE: OnceLock<Box<dyn Store>> = OnceLock::new();

/// Makes `store` the home of the state for the rest of this process. False when one was installed already.
pub fn use_store(store: Box<dyn Store>) -> bool {
    STORE.set(store).is_ok()
}

pub fn store() -> Option<&'static dyn Store> {
    STORE.get().map(AsRef::as_ref)
}

/// The accounts' state: from the store when there is one, else the file with the old kernel marks imported. An answer that
/// cannot be had is an error, never "no state": a core that refuses, is busy or needs a migration must not read as every
/// account ready. A tombstone with no store is the same error.
pub fn read_state(machine: &Path) -> Result<Map, String> {
    if let Some(s) = store() {
        return s.accounts();
    }
    if let Some(e) = tombstone_error(machine) {
        return Err(e);
    }
    Ok(state::read(machine))
}

/// [`read_state`] for the readers that cannot stop (the `StateSource` of `pick`): an error is written to the log and reads as
/// no state. The verbs and the launch's account choice check [`read_state`] first and stop.
pub fn read_state_lossy(machine: &Path) -> Map {
    read_state(machine).unwrap_or_else(|e| {
        crate::log::note(&format!("accounts state: unreadable: {e}"));
        Map::new()
    })
}

/// `kith_tombstone.Tombstoned`'s words, when `state.json` is a tombstone and the state is not read from the core: the file may
/// not be read (it says nothing) nor written (it would replace the note).
pub fn tombstone_error(machine: &Path) -> Option<String> {
    if store().is_some() || !state::tombstoned(machine) {
        return None;
    }
    Some(format!(
        "{} moved to the Kith core (module accounts); this reader must not use it (kith rollback accounts)",
        state::state_file(machine).display()
    ))
}

/// Makes `change`. The file side keeps the Python writers' rule that a disk that cannot be written loses the mark and the
/// run goes on (noted in the log); a tombstone, or the core's refusal, is an error, as `Tombstoned` is there.
pub fn make(machine: &Path, change: &Change, now: i64) -> Result<(), String> {
    if let Some(s) = store() {
        return s.apply(change, now);
    }
    if let Some(e) = tombstone_error(machine) {
        return Err(e);
    }
    state::update(machine, |accts| apply(accts, change, now));
    Ok(())
}

/// The clock, in whole seconds.
#[allow(clippy::cast_possible_truncation)]
pub fn now_secs() -> i64 {
    clock::now() as i64
}
