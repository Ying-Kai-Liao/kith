//! The asker's mailbox: rows of `ask_rows` (migration 102), one per answer, progress report or lost notice of an ask,
//! numbered from 1 per ask. The wake owed for a row is `ask:<ask>/<n>` (R3, as `kith-ask hand_over` made them), and a row
//! reaches the asker once: `--wait` ([`read_in`]) or the wake's take ([`mark_read_by_wake_in`]) marks it read, in the
//! transaction that delivers it (R5, invariant `AnswerOnce`).

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

use crate::error::Result;

/// How long a settled ask and its rows are kept, as `kith-ask --sweep` forgot them.
pub(super) const KEEP_MS: i64 = 31 * 24 * 3600 * 1000;

/// What kind of row: an `answer` or a `lost` notice owes the asker a wake; a `progress` report waits for `--wait`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Answer,
    Progress,
    Lost,
}

impl Kind {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Progress => "progress",
            Self::Lost => "lost",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<Self> {
        match word {
            "answer" => Some(Self::Answer),
            "progress" => Some(Self::Progress),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }
}

/// One row as it is read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub n: i64,
    pub who: Option<String>,
    pub kind: Kind,
    pub text: String,
    pub at_ms: i64,
}

impl Row {
    pub(crate) fn json(&self) -> Value {
        json!({"n": self.n, "as": self.who, "kind": self.kind.word(), "text": self.text, "at": self.at_ms})
    }
}

/// The wake id of row `n` of ask `ask`.
pub(crate) fn wake_id(ask: &str, n: i64) -> String {
    format!("ask:{ask}/{n}")
}

/// Appends a row and answers its number. The same row again (same author, kind and text as the last one) is a duplicate: it
/// adds nothing and answers `None`, so a replayed post wakes no one twice.
pub(super) fn add_in(
    tx: &Connection,
    ask: &str,
    who: Option<&str>,
    kind: Kind,
    text: &str,
    now: i64,
) -> Result<Option<i64>> {
    let last: Option<(i64, Option<String>, String, String)> = tx
        .query_row(
            "SELECT n, \"as\", kind, text FROM ask_rows WHERE ask = ?1 ORDER BY n DESC LIMIT 1",
            [ask],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((_, w, k, t)) = &last
        && w.as_deref() == who
        && k == kind.word()
        && t == text
    {
        return Ok(None);
    }
    let n = last.map_or(1, |(n, ..)| n + 1);
    tx.execute(
        "INSERT INTO ask_rows (ask, n, \"as\", kind, text, at_ms, read) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![ask, n, who, kind.word(), text, now],
    )?;
    Ok(Some(n))
}

/// Row `n` of a peer ask, numbered by the computer that wrote it. True when it is new: a row copied twice is one row
/// (unique on (ask, n)), so a repeated `ask.peerApply` adds nothing and wakes no one again.
pub(super) fn add_numbered_in(
    tx: &Connection,
    ask: &str,
    n: i64,
    who: Option<&str>,
    kind: Kind,
    text: &str,
    at_ms: i64,
) -> Result<bool> {
    Ok(tx.execute(
        "INSERT OR IGNORE INTO ask_rows (ask, n, \"as\", kind, text, at_ms, read) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![ask, n, who, kind.word(), text, at_ms],
    )? == 1)
}

/// Whether the ask has an `answer` or a `lost` row: its asker was told how it ended.
pub(super) fn has_final_in(tx: &Connection, ask: &str) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM ask_rows WHERE ask = ?1 AND kind IN ('answer', 'lost'))",
        [ask],
        |r| r.get(0),
    )?)
}

/// The rows nobody has read, in order.
pub(super) fn unread_in(tx: &Connection, ask: &str) -> Result<Vec<Row>> {
    let mut st = tx.prepare(
        "SELECT n, \"as\", kind, text, at_ms FROM ask_rows WHERE ask = ?1 AND read = 0 ORDER BY n",
    )?;
    let rows = st.query_map([ask], |r| {
        let kind: String = r.get(2)?;
        Ok(Row {
            n: r.get(0)?,
            who: r.get(1)?,
            kind: Kind::parse(&kind).unwrap_or(Kind::Answer),
            text: r.get(3)?,
            at_ms: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Marks row `n` read. An `answer` or `lost` row also marks the ask delivered: its caller has what it asked for, so a later
/// stop keeps the answer as history (`end_tree`). True when the row was unread.
pub(super) fn mark_read_in(tx: &Connection, ask: &str, n: i64) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE ask_rows SET read = 1 WHERE ask = ?1 AND n = ?2 AND read = 0",
        params![ask, n],
    )?;
    if changed == 1 {
        tx.execute(
            "UPDATE asks SET delivered = 1
             WHERE id = ?1 AND EXISTS (SELECT 1 FROM ask_rows WHERE ask = ?1 AND n = ?2 AND kind IN ('answer', 'lost'))",
            params![ask, n],
        )?;
    }
    Ok(changed == 1)
}

/// A wake's take carries these request ids: the rows among them (`ask:<ask>/<n>`) are read, in the take's transaction.
pub(crate) fn mark_read_by_wake_in(tx: &Connection, ids: &[String]) -> Result<()> {
    for id in ids {
        let Some((ask, n)) = id
            .strip_prefix("ask:")
            .and_then(|rest| rest.rsplit_once('/'))
            .and_then(|(ask, n)| Some((ask, n.parse::<i64>().ok()?)))
        else {
            continue;
        };
        mark_read_in(tx, ask, n)?;
    }
    Ok(())
}

/// Whether this is the first time the cost of `turn` of ask `ask` is counted (0: the run of an ask with no turn). The run's
/// cost joins the chain once per turn, so a report, the tick and a retry add it once.
pub(super) fn count_cost_in(tx: &Connection, ask: &str, turn: i64) -> Result<bool> {
    Ok(tx.execute(
        "INSERT OR IGNORE INTO ask_costs (ask, turn) VALUES (?1, ?2)",
        params![ask, turn],
    )? == 1)
}

/// Forgets what is settled and older than [`KEEP_MS`], with its rows. Answers how many asks went.
pub(super) fn forget_old_in(tx: &Connection, now: i64) -> Result<usize> {
    let before = now - KEEP_MS;
    tx.execute(
        "DELETE FROM ask_rows WHERE ask IN
            (SELECT id FROM asks WHERE state <> 'open' AND COALESCE(answered_ms, ended_ms, at_ms, 0) < ?1)",
        [before],
    )?;
    tx.execute(
        "DELETE FROM ask_turns WHERE ask IN
            (SELECT id FROM asks WHERE state <> 'open' AND COALESCE(answered_ms, ended_ms, at_ms, 0) < ?1)",
        [before],
    )?;
    tx.execute(
        "DELETE FROM ask_costs WHERE ask IN
            (SELECT id FROM asks WHERE state <> 'open' AND COALESCE(answered_ms, ended_ms, at_ms, 0) < ?1)",
        [before],
    )?;
    Ok(tx.execute(
        "DELETE FROM asks WHERE state <> 'open' AND COALESCE(answered_ms, ended_ms, at_ms, 0) < ?1",
        [before],
    )?)
}
