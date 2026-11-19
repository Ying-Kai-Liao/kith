//! `kith-cost tag` and `kith-cost record`: the two writers of the ledger besides `sync`. `tag` is the `SessionStart` hook of every
//! kith run (one `runs` row for the session on stdin), `record` is the row `adapters/harness.py` writes for a run on another
//! harness. Both open the ledger first (so bad input still leaves `cost.db` with its tables, as the Python's `db()` does), never
//! fail the session (a problem is one stderr line, exit 0) and write nothing at all when any of their statements fails.

use super::{Env, Out, Paths, ledger};
use chrono::Utc;
use rusqlite::{Connection, params_from_iter, types::Value as Sql};
use serde_json::Value;

/// The columns a run is written with, in the order both verbs bind them.
const RUN_COLUMNS: &str = "session, agent, version, model, effort, via, project, started, person, provider, account, account_id";

/// The line a failed hook says, and the exit code 0 that lets the session go on.
fn never_fails(verb: &str, why: &str) -> Out {
    Out {
        stderr: format!("kith-cost {verb}: {why}\n"),
        ..Out::default()
    }
}

/// `x or None` of an environment value: unset and empty are both nothing.
fn some(env: &Env, name: &str) -> Sql {
    env.var(name)
        .filter(|v| !v.is_empty())
        .map_or(Sql::Null, |v| Sql::Text(v.to_owned()))
}

/// `os.environ.get(name)`: unset is nothing, an empty value stays the empty text.
fn raw(env: &Env, name: &str) -> Sql {
    env.var(name).map_or(Sql::Null, |v| Sql::Text(v.to_owned()))
}

/// Whom a run is written down for. A room's run is the cost of whoever asked: `KITH_COST_PERSON` names them, `owner` is the
/// owner's own (written as none). Anyone else's is `KITH_PERSON`.
fn cost_person(env: &Env) -> Sql {
    match env.var("KITH_COST_PERSON").filter(|v| !v.is_empty()) {
        Some("owner") => Sql::Null,
        Some(who) => Sql::Text(who.to_owned()),
        None => some(env, "KITH_PERSON"),
    }
}

/// What sqlite3 binds a JSON value as: the text, number, `true` as 1 and `null` as NULL; an object or an array is no SQL value.
fn bind(v: &Value) -> Result<Sql, String> {
    match v {
        Value::Null => Ok(Sql::Null),
        Value::Bool(b) => Ok(Sql::Integer(i64::from(*b))),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .or_else(|| n.as_f64().map(Sql::Real))
            .ok_or_else(|| "Python int too large to convert to SQLite INTEGER".to_owned()),
        Value::String(s) => Ok(Sql::Text(s.clone())),
        Value::Array(_) => Err("Error binding parameter: type 'list' is not supported".to_owned()),
        Value::Object(_) => Err("Error binding parameter: type 'dict' is not supported".to_owned()),
    }
}

/// Python's name for the type of a JSON value, for the error a method call on the wrong one raises.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `json.load(sys.stdin)`.
fn read_json(env: &Env) -> Result<Value, String> {
    let text = env.stdin_text();
    serde_json::from_str(&text)
        .map_err(|e| crate::secrets::pyjson::error_of(&text).unwrap_or_else(|| e.to_string()))
}

/// `d.get(key)` of the hook's JSON, which must be an object.
fn field<'a>(d: &'a Value, key: &str) -> Result<Option<&'a Value>, String> {
    match d {
        Value::Object(m) => Ok(m.get(key)),
        other => Err(format!(
            "'{}' object has no attribute 'get'",
            type_name(other)
        )),
    }
}

/// `d[key]`.
fn item<'a>(d: &'a Value, key: &str) -> Result<&'a Value, String> {
    match d {
        Value::Object(m) => m.get(key).ok_or_else(|| format!("'{key}'")),
        Value::Array(_) => Err("list indices must be integers or slices, not str".to_owned()),
        Value::String(_) => Err("string indices must be integers, not 'str'".to_owned()),
        other => Err(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        )),
    }
}

fn open(p: &Paths, verb: &str) -> Result<Connection, Out> {
    ledger::open(p).map_err(|e| never_fails(verb, &e.to_string()))
}

/// `tag`: the session on stdin is a run of `$KITH_AGENT`.
pub(super) fn tag(p: &Paths, env: &Env) -> Out {
    let mut c = match open(p, "tag") {
        Ok(c) => c,
        Err(out) => return out,
    };
    match write_tag(&mut c, env) {
        Ok(()) => Out::default(),
        Err(e) => never_fails("tag", &e),
    }
}

fn write_tag(c: &mut Connection, env: &Env) -> Result<(), String> {
    let hook = read_json(env)?;
    let session = field(&hook, "session_id")?;
    if !ledger::truthy(session) || env.var("KITH_AGENT").is_none_or(str::is_empty) {
        return Ok(());
    }
    let args = vec![
        bind(session.unwrap_or(&Value::Null))?,
        raw(env, "KITH_AGENT"),
        raw(env, "KITH_AGENT_VERSION"),
        some(env, "KITH_MODEL"),
        some(env, "KITH_EFFORT"),
        via(env),
        some(env, "KITH_PROJECT"),
        Sql::Text(Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
        cost_person(env),
        some(env, "KITH_PROVIDER"),
        some(env, "KITH_ACCOUNT"),
        some(env, "KITH_ACCOUNT_ID"),
    ];
    let tx = c.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        &format!("insert or replace into runs ({RUN_COLUMNS}) values (?,?,?,?,?,?,?,?,?,?,?,?)"),
        params_from_iter(args),
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// `env('KITH_VIA', 'terminal')`: only an unset variable gets the default.
fn via(env: &Env) -> Sql {
    Sql::Text(env.var("KITH_VIA").unwrap_or("terminal").to_owned())
}

/// `record`: a run on another harness, one request row and (when the session is new) its run row.
pub(super) fn record(p: &Paths, env: &Env) -> Out {
    let mut c = match open(p, "record") {
        Ok(c) => c,
        Err(out) => return out,
    };
    match write_record(&mut c, env) {
        Ok(()) => Out::default(),
        Err(e) => never_fails("record", &e),
    }
}

fn write_record(c: &mut Connection, env: &Env) -> Result<(), String> {
    let r = read_json(env)?;
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
    let id = item(&r, "id")?;
    let session = item(&r, "session")?;
    // a missing key is the default, a key that is null is NULL
    let get = |key: &str, default: Sql| -> Result<Sql, String> {
        field(&r, key)?.map_or(Ok(default), bind)
    };
    let model = get("model", Sql::Null)?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let req = vec![
        bind(id)?,
        bind(session)?,
        Sql::Text(now.clone()),
        Sql::Text(cwd.to_string_lossy().into_owned()),
        model.clone(),
        Sql::Text("main".to_owned()),
        get("turn", Sql::Null)?,
        get("input", Sql::Integer(0))?,
        get("output", Sql::Integer(0))?,
        get("cache_read", Sql::Integer(0))?,
        get("cache_write", Sql::Integer(0))?,
        get("cost", Sql::Null)?,
    ];
    let run = vec![
        bind(session)?,
        raw(env, "KITH_AGENT"),
        raw(env, "KITH_AGENT_VERSION"),
        model,
        some(env, "KITH_EFFORT"),
        via(env),
        some(env, "KITH_PROJECT"),
        Sql::Text(now.chars().take(19).collect()),
        cost_person(env),
        some(env, "KITH_PROVIDER"),
        some(env, "KITH_ACCOUNT"),
        some(env, "KITH_ACCOUNT_ID"),
    ];
    // one commit, as the Python: a failing second statement leaves nothing of the first
    let tx = c.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "insert or replace into req values (?,?,?,?,?,?,?,?,?,?,?,?)",
        params_from_iter(req),
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        &format!("insert or ignore into runs ({RUN_COLUMNS}) values (?,?,?,?,?,?,?,?,?,?,?,?)"),
        params_from_iter(run),
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
