//! `session.record` (transcripts P1): the core takes a session registry record into its row, then writes
//! `$KITH_STATE/live/terminal/<sid>.json` from that row. Once the `transcripts` module is switched the core is the only
//! writer of the registry, and each file is a projection of its `session_records` row (migration 111).
//!
//! The answers, as data (the hook tells them apart, architect MUST 5):
//!
//! | answer | meaning | the hook's spool entry |
//! |---|---|---|
//! | `{outcome: "recorded"}` | the row took the record, the file was written from it | deleted |
//! | `{outcome: "older"}` | a newer row kept; the file was still written from the row (MUST 2) | deleted |
//! | `{refused: "invalid", why}` | not a record ([`validate`]) | deleted, logged |
//! | `{refused: "not-serving", why}` | the transcripts module is not switched (stepping, rolling back, a marker with no switch) | kept |
//!
//! Order of work (MUST 2): one claim transaction does the newer-wins check and writes `session_records`, `engine_sessions`
//! and `conversations`; after the commit, under the session's lock ([`super::project::settle`], contract A2), the row is
//! read back and set against the file: a file newer than the row is taken into it, else the file is written from the row
//! (tmp + rename, 0600 in a 0700 folder). The call answers only after the rename, so the host's terminal
//! writer check (`serve/host/src/terminal/sync.rs` `writer()`) sees the live record as soon as the hook returns. Every caller
//! holds the core (`&mut Core`, or the daemon's core mutex) for the whole call, so two records of one session can never
//! commit in one order and rename in the other.
//!
//! The order key is `(recordedAt, procStart)`, later wins, and a full tie goes to the record that arrives last. A record
//! with no `recordedAt` (an older hook's file) is keyed by the file's `st_mtime_ns` where there is a file, else 0 (A1).
//!
//! `startedAt` survives a compact (`source: "compact"` keeps the row's `startedAt`), the rule `kith-sessions record` applied
//! to its own file until now.

use std::path::{Path, PathBuf};

use kith_kernel::threads::{Ctx, registry_dir, safe_id};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value, json};

use super::DEFAULT_ACCOUNT;
use crate::db::Core;
use crate::error::Result;

/// The spool folder under the registry (`inbox/<sid>.<recordedAt>.<pid>.json`); readers that glob `*.json` never see it.
pub(crate) const INBOX: &str = "inbox";

/// The order of two records of one session: `(recordedAt in ns, procStart)`, `None` sorting first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    pub at: i64,
    pub proc_start: Option<String>,
}

impl Key {
    /// A record's key; `fallback` stands for a missing `recordedAt` (the file's modification time, or 0).
    pub(crate) fn of(rec: &Map<String, Value>, fallback: i64) -> Self {
        let at = rec
            .get("recordedAt")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
            })
            .unwrap_or(fallback);
        let proc_start = match rec.get("procStart") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => Some(other.to_string()),
        };
        Self { at, proc_start }
    }
}

/// What the row did with a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Took {
    Recorded,
    Older,
}

impl Took {
    pub(crate) fn answer(self) -> Value {
        json!({"outcome": match self { Self::Recorded => "recorded", Self::Older => "older" }})
    }
}

pub(crate) fn invalid(why: impl Into<String>) -> Value {
    json!({"refused": "invalid", "why": why.into()})
}

pub(crate) fn not_serving(why: impl Into<String>) -> Value {
    json!({"refused": "not-serving", "why": why.into()})
}

/// Whether `v` is a record the core keeps. Never stricter than what `kith-sessions record` writes today (MUST 4): only the
/// file name rule (`safe_id`), `v: 1`, an object, and no `person` key (a person's run is never listed). `pid: null`, a null
/// or empty `transcriptPath` and any other field are kept as they are.
pub(crate) fn validate(
    v: Option<&Value>,
) -> std::result::Result<(String, Map<String, Value>), String> {
    let Some(Value::Object(rec)) = v else {
        return Err("the record is not a JSON object".into());
    };
    if rec.get("v").and_then(Value::as_i64) != Some(1) {
        return Err("the record is not version 1".into());
    }
    if rec.contains_key("person") {
        return Err("a person's session is never listed".into());
    }
    let sid = rec
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !safe_id(sid) {
        return Err(format!("{sid:?} is not a session id"));
    }
    Ok((sid.to_owned(), rec.clone()))
}

/// The row's record and its key.
pub(crate) fn row(conn: &Connection, sid: &str) -> Result<Option<(Map<String, Value>, Key)>> {
    let got: Option<(String, i64, Option<String>)> = conn
        .query_row(
            "SELECT record, recorded_at, proc_start FROM session_records WHERE sid = ?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(
        got.and_then(|(text, at, proc_start)| match serde_json::from_str(&text) {
            Ok(Value::Object(m)) => Some((m, Key { at, proc_start })),
            _ => None,
        }),
    )
}

/// Every sid that has a row, in order.
pub(crate) fn sids(conn: &Connection) -> Result<Vec<String>> {
    let mut q = conn.prepare("SELECT sid FROM session_records ORDER BY sid")?;
    let out = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn text(rec: &Map<String, Value>, key: &str) -> String {
    rec.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Writes the row and what it stands for in `engine_sessions` and `conversations` (inside the caller's transaction). The
/// engine session keeps its hash while its pointer stays; a new pointer clears it (the core reads the files again).
pub(crate) fn put(
    conn: &Connection,
    ctx: &Ctx<'_>,
    sid: &str,
    rec: &Map<String, Value>,
    key: &Key,
) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "INSERT INTO session_records (sid, record, recorded_at, proc_start, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (sid) DO UPDATE SET record = ?2, recorded_at = ?3, proc_start = ?4, updated_at = ?5",
        params![sid, Value::Object(rec.clone()).to_string(), key.at, key.proc_start, now],
    )?;
    let given = text(rec, "account");
    let account = if given.is_empty() {
        DEFAULT_ACCOUNT.to_owned()
    } else {
        given
    };
    let engine = super::engine_of(&account).to_owned();
    let pointer = super::pointer_of(ctx, &account, sid, &text(rec, "transcriptPath"));
    conn.execute(
        "INSERT INTO engine_sessions (conversation, account, engine, session_id, pointer, cwd, created_at)
         VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?6)
         ON CONFLICT (conversation, account) DO UPDATE SET engine = ?3, session_id = ?1, cwd = ?5,
             hash = CASE WHEN pointer = ?4 THEN hash END,
             size = CASE WHEN pointer = ?4 THEN size END,
             last_read_at = CASE WHEN pointer = ?4 THEN last_read_at END,
             pointer = ?4",
        params![sid, account, engine, pointer, text(rec, "cwd"), now],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO conversations (id, agent, engine, engine_session, account, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?1, ?4, ?5, ?5)",
        params![sid, text(rec, "agent"), engine, account, now],
    )?;
    Ok(())
}

/// The newer-wins write in one claim transaction: the row takes `rec` unless it holds a newer one. A compact keeps the row's
/// `startedAt`. `fallback` keys a record without `recordedAt`.
pub(crate) fn commit(
    core: &mut Core,
    ctx: &Ctx<'_>,
    sid: &str,
    mut rec: Map<String, Value>,
    fallback: i64,
) -> Result<Took> {
    let tx = core.claim_tx()?;
    let have = row(&tx, sid)?;
    if rec.get("source").and_then(Value::as_str) == Some("compact")
        && let Some(started) = have
            .as_ref()
            .and_then(|(m, _)| m.get("startedAt"))
            .filter(|v| !v.is_null())
    {
        rec.insert("startedAt".into(), started.clone());
    }
    let key = Key::of(&rec, fallback);
    if have.as_ref().is_some_and(|(_, k)| key < *k) {
        return Ok(Took::Older);
    }
    put(&tx, ctx, sid, &rec, &key)?;
    tx.commit()?;
    Ok(Took::Recorded)
}

/// Commit, then the file from the row. `Ok(Err(why))` is an invalid record (nothing written).
pub(crate) fn record_in(
    core: &mut Core,
    ctx: &Ctx<'_>,
    dir: &Path,
    rec: Option<&Value>,
    fallback: i64,
) -> Result<std::result::Result<Took, String>> {
    let (sid, rec) = match validate(rec) {
        Ok(v) => v,
        Err(why) => return Ok(Err(why)),
    };
    let took = commit(core, ctx, &sid, rec, fallback)?;
    super::project::settle(core, ctx, dir, &sid)?;
    Ok(Ok(took))
}

/// The registry folder and the reading context of a core opened with its folders.
pub(crate) fn place(core: &Core) -> Option<(PathBuf, Ctx<'static>)> {
    let f = core.folders()?;
    Some((
        registry_dir(&f.state),
        Ctx {
            state: Some(f.state.clone()),
            machine: Some(f.machine.clone()),
            home: f.home.clone(),
            env: &kith_kernel::paths::process_env,
        },
    ))
}

/// Whether the transcripts are the core's on this Mac: the `transcripts` module is switched (its switch file and the marker
/// in core.db), read through the same `may_serve` as every module. `Err(why)` otherwise.
pub(crate) fn serving(core: &Core) -> std::result::Result<(), String> {
    let Some(folders) = core.folders() else {
        return Err("this core was opened without its folders".into());
    };
    let reg = crate::cutover::registry();
    let Some(m) = reg.iter().find(|m| m.name() == "transcripts") else {
        return Err("no transcripts module".into());
    };
    let s = crate::cutover::same_answer::may_serve(
        m.as_ref(),
        &crate::cutover::Env::with_paths(folders),
        core.conn(),
        false,
    );
    if s.ok {
        Ok(())
    } else {
        Err(s.warning.unwrap_or_else(|| {
            "the transcripts are not switched to the core on this Mac (kith migrate step transcripts)".into()
        }))
    }
}

/// `session.record` with params `{record}`: refused as `not-serving` until transcripts is switched.
pub(crate) fn call(core: &mut Core, params: &Value) -> Result<Value> {
    if let Err(why) = serving(core) {
        return Ok(not_serving(why));
    }
    let Some((dir, ctx)) = place(core) else {
        return Ok(not_serving("this core was opened without its folders"));
    };
    Ok(
        match record_in(core, &ctx, &dir, params.get("record"), 0)? {
            Ok(took) => took.answer(),
            Err(why) => invalid(why),
        },
    )
}

/// Whether `method` is one this module answers.
pub(crate) fn serves(method: &str) -> bool {
    method == "session.record"
}
