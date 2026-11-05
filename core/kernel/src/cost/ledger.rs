//! The cost ledger (`<machine>/cost.db`): `db()` and `sync()` of `kernel/bin/kith-cost`. One row per API request, copied
//! out of Claude Code's transcripts so it outlives their cleanup; `runs` says which kith agent (and person) a session was.
//! Sync is the only write this module makes besides the schema.

use super::Paths;
use rusqlite::{Connection, params, types::Value as Sql};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The tables, indexes and the person/provider columns exactly as `kith-cost` creates them (the text is what `sqlite_master`
/// keeps, and the difftest compares it).
const SCHEMA: &str = include_str!("schema.sql");
const VIEW: &str = include_str!("view.sql");

/// Dollars per `MTok`: input, output, cache read, cache write 5m, cache write 1h. First matching key wins.
const PRICES: [(&str, [f64; 5]); 8] = [
    ("fable", [10.0, 50.0, 0.25, 12.5, 20.0]),
    ("mythos", [10.0, 50.0, 0.25, 12.5, 20.0]),
    ("opus-5-5", [4.0, 20.0, 0.20, 5.0, 8.0]),
    ("opus", [5.0, 25.0, 0.50, 6.25, 10.0]),
    ("sonnet-5", [2.0, 10.0, 0.20, 2.5, 4.0]),
    ("sonnet", [3.0, 15.0, 0.30, 3.75, 6.0]),
    ("haiku-5", [0.10, 0.50, 0.01, 0.125, 0.20]),
    ("haiku", [1.0, 5.0, 0.10, 1.25, 2.0]),
];

/// Opens the ledger, creating what is missing (`db()`).
pub fn open(p: &Paths) -> rusqlite::Result<Connection> {
    let path = p.db();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let c = Connection::open(path)?;
    c.execute_batch(SCHEMA)?;
    let has = |c: &Connection, col: &str| -> rusqlite::Result<bool> {
        let mut st = c.prepare("pragma table_info(runs)")?;
        let names = st
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names.iter().any(|n| n == col))
    };
    // who the run was for: KITH_PERSON; null is the owner
    if !has(&c, "person")? {
        c.execute("alter table runs add column person text", [])?;
    }
    // which harness and which login answered, and the login's whole id
    for col in ["provider", "account", "account_id"] {
        if !has(&c, col)? {
            c.execute(&format!("alter table runs add column {col} text"), [])?;
        }
    }
    c.execute_batch(VIEW)?;
    Ok(c)
}

/// What Python's `bool(x)` says of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The whitespace of Python's `str.strip()` and `str.split()`: Unicode `White_Space`, plus the four separators Python adds (U+001C to U+001F).
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `' '.join(t.split())[:100]`
fn turn_text(t: &str) -> String {
    let words: Vec<&str> = t.split(py_space).filter(|w| !w.is_empty()).collect();
    words.join(" ").chars().take(100).collect()
}

/// A number from a transcript: `0` when it is missing, an integer or a float as it is.
fn count(u: &Value, key: &str) -> Sql {
    match u.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .map(Sql::Integer)
            .or_else(|| n.as_f64().map(Sql::Real))
            .unwrap_or(Sql::Integer(0)),
        _ => Sql::Integer(0),
    }
}

fn num(u: Option<&Value>) -> f64 {
    u.and_then(Value::as_f64).unwrap_or(0.0)
}

/// `price(model, u)`: list price in dollars of one request; `None` for a model with no price.
pub fn price(model: Option<&str>, u: &Value) -> Option<f64> {
    let model = model.unwrap_or("");
    let p = PRICES
        .iter()
        .find(|(k, _)| model.contains(k))
        .map(|(_, p)| *p)?;
    let cc = u.get("cache_creation").filter(|c| c.is_object());
    let cc_get = |k: &str| cc.and_then(|c| c.get(k));
    let w1h = num(cc_get("ephemeral_1h_input_tokens"));
    let w5m = match cc_get("ephemeral_5m_input_tokens") {
        Some(v) => num(Some(v)),
        None => num(u.get("cache_creation_input_tokens")) - w1h,
    };
    let c = (num(u.get("input_tokens")) * p[0]
        + num(u.get("output_tokens")) * p[1]
        + num(u.get("cache_read_input_tokens")) * p[2]
        + w5m * p[3]
        + w1h * p[4])
        / 1e6;
    Some(
        c * if u.get("speed").and_then(Value::as_str) == Some("fast") {
            2.0
        } else {
            1.0
        },
    )
}

/// A transcript field as the text the ledger keeps: a string as it is, a number as its digits, else nothing.
fn text_of(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

type Row = Vec<Sql>;

/// The requests one transcript holds, by message id (a streamed message is logged once per content block; the last copy
/// has the final usage), in the order the ids first appeared.
fn requests(file: impl std::io::BufRead, sub: bool, agent: &str) -> Vec<Row> {
    let mut turn: Option<String> = None;
    let mut order: Vec<String> = Vec::new();
    let mut rows: HashMap<String, Row> = HashMap::new();
    // One line at a time, decoded as it is read: a transcript can be hundreds of megabytes.
    let mut file = file;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if !matches!(file.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        let Ok(Value::Object(d)) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let empty = Value::Object(serde_json::Map::new());
        let m = d
            .get("message")
            .filter(|m| truthy(Some(m)))
            .unwrap_or(&empty);
        let kind = d.get("type").and_then(Value::as_str);
        if kind == Some("user") && !truthy(d.get("isMeta")) && !sub {
            let t = match m.get("content") {
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .find(|b| {
                        b.is_object() && b.get("type").and_then(Value::as_str) == Some("text")
                    })
                    .and_then(|b| b.get("text").cloned()),
                other => other.cloned(),
            };
            if let Some(Value::String(t)) = t
                && !t.trim_matches(py_space).is_empty()
                && !t.trim_start_matches(py_space).starts_with('<')
            {
                turn = Some(turn_text(&t));
            }
        }
        let u = m.get("usage");
        if kind != Some("assistant")
            || !truthy(u)
            || !truthy(m.get("id"))
            || m.get("model").and_then(Value::as_str) == Some("<synthetic>")
        {
            continue;
        }
        let (Some(u), Some(id)) = (u.filter(|u| u.is_object()), text_of(m.get("id"))) else {
            continue;
        };
        let model = m.get("model").and_then(Value::as_str);
        let opt = |s: Option<String>| s.map_or(Sql::Null, Sql::Text);
        let row = vec![
            Sql::Text(id.clone()),
            opt(text_of(d.get("sessionId"))),
            opt(text_of(d.get("timestamp"))),
            Sql::Text(if truthy(d.get("cwd")) {
                text_of(d.get("cwd")).unwrap_or_default()
            } else {
                String::new()
            }),
            opt(model.map(str::to_owned)),
            Sql::Text(agent.to_owned()),
            opt(turn.clone()),
            count(u, "input_tokens"),
            count(u, "output_tokens"),
            count(u, "cache_read_input_tokens"),
            count(u, "cache_creation_input_tokens"),
            price(model, u).map_or(Sql::Null, Sql::Real),
        ];
        if rows.insert(id.clone(), row).is_none() {
            order.push(id);
        }
    }
    order
        .into_iter()
        .filter_map(|id| rows.remove(&id))
        .collect()
}

/// Python's `str(x)` for the two fields of a subagent's `.meta.json` that name it.
fn py_str(v: Option<&Value>, default: &str) -> String {
    match v {
        None => default.to_owned(),
        Some(Value::Null) => "None".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_owned(),
        Some(other) => crate::secrets::pyjson::dumps(other),
    }
}

/// `glob.glob(pattern)` for the three shapes the ledger reads: each `*` is one path part that does not start with a dot.
fn list(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

fn jsonl_in(dir: &Path) -> Vec<PathBuf> {
    list(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

/// Every transcript under the Claude config folders: the main one and each extra account's own.
fn transcripts(p: &Paths) -> Vec<PathBuf> {
    let mut roots = vec![p.projects()];
    roots.extend(
        list(&p.accounts_claude())
            .into_iter()
            .map(|d| d.join("projects")),
    );
    let mut files = Vec::new();
    for root in roots {
        for project in list(&root) {
            files.extend(jsonl_in(&project));
            for session in list(&project) {
                files.extend(jsonl_in(&session.join("subagents")));
            }
        }
    }
    files
}

/// `sync(c, quiet)`: copies new usage into the ledger; the line `kith-cost sync` prints (`None` when quiet).
pub fn sync(c: &mut Connection, p: &Paths, quiet: bool) -> rusqlite::Result<Option<String>> {
    let seen: HashMap<String, i64> = {
        let mut st = c.prepare("select path, size from seen")?;
        st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let files = transcripts(p);
    let tx = c.transaction()?;
    let mut updated = 0;
    for f in &files {
        let Ok(size) = std::fs::metadata(f).map(|m| m.len()) else {
            continue;
        };
        let key = f.to_string_lossy().into_owned();
        if seen
            .get(&key)
            .is_some_and(|s| i64::try_from(size).ok() == Some(*s))
        {
            continue;
        }
        let sub = key.contains("/subagents/");
        let agent = if sub {
            let meta = PathBuf::from(format!(
                "{}.meta.json",
                key.strip_suffix(".jsonl").unwrap_or(&key)
            ));
            match std::fs::read_to_string(&meta)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                Some(m) if m.is_object() => format!(
                    "{}: {}",
                    py_str(m.get("agentType"), "agent"),
                    py_str(m.get("description"), "")
                ),
                _ => "subagent".to_owned(),
            }
        } else {
            "main".to_owned()
        };
        let Ok(file) = std::fs::File::open(f) else {
            continue;
        };
        let rows = requests(std::io::BufReader::new(file), sub, &agent);
        for row in &rows {
            tx.execute(
                "insert or replace into req values (?,?,?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params_from_iter(row),
            )?;
        }
        tx.execute(
            "insert or replace into seen values (?, ?)",
            params![key, i64::try_from(size).unwrap_or(i64::MAX)],
        )?;
        updated += rows.len();
    }
    bridge_runs(&tx, p);
    tx.commit()?;
    Ok((!quiet).then(|| {
        format!(
            "synced {} transcripts, {updated} requests updated",
            files.len()
        )
    }))
}

/// Discord runs before tagging: the bridge's thread table names the session of each agent (`BRIDGE_STATE`). Any trouble
/// reading it skips all of it, as the `try` around the Python does.
fn bridge_runs(tx: &rusqlite::Transaction<'_>, p: &Paths) {
    let Some(state) = std::fs::read_to_string(p.bridge_state())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return;
    };
    let Some(top) = state.as_object() else {
        return;
    };
    let Some(Value::Object(threads)) = top.get("threads") else {
        return;
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut v2: Vec<(String, String)> = Vec::new();
    for t in threads.values() {
        let Some(t) = t.as_object() else {
            return;
        };
        if truthy(t.get("session_id"))
            && truthy(t.get("agent"))
            && let (Some(s), Some(a)) = (text_of(t.get("session_id")), text_of(t.get("agent")))
        {
            rows.push((s, a));
        }
        match t.get("sessions") {
            Some(Value::Object(m)) => {
                for (a, sid) in m {
                    if truthy(Some(sid))
                        && let Some(s) = text_of(Some(sid))
                    {
                        v2.push((s, a.clone()));
                    }
                }
            }
            Some(other) if truthy(Some(other)) => return,
            _ => {}
        }
    }
    rows.extend(v2);
    for (session, agent) in rows {
        let _ = tx.execute(
            "insert or ignore into runs (session, agent, via) values (?, ?, 'discord')",
            params![session, agent],
        );
    }
}
