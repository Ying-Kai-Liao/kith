//! The `account.*` rows the core answers over core.db: the reads (M3-9a: `state`, `pick`; accounts-s1: `rows`) and the writers
//! `out`, `clear`, `usage` and `started`, which only a switched core accepts (`core_writes`): before `kith migrate step
//! accounts` the file is the truth and a write here would fork it. The params were checked against the row.

use std::path::Path;

use kith_kernel::accounts::{self, Ctx};
use serde_json::{Value, json};

use super::CoreState;
use crate::db::Core;
use crate::error::{Error, Result};

/// `account.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "account.state"
            | "account.pick"
            | "account.rows"
            | "account.out"
            | "account.clear"
            | "account.usage"
            | "account.started"
    )
}

fn word<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

impl Core {
    /// `account.state` and `account.pick`, over this core's database. `account.pick` starts the harness status
    /// commands in this process (how `pick` knows an account is signed in), only on an owner's call, never on its own.
    pub(crate) fn account_call(&self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "account.state" => state_json(self, word(params, "entry")),
            "account.pick" => {
                let (folders, kernel) = self.folders_for(params)?;
                let system = accounts::System {
                    kernel: kernel.clone(),
                };
                let ctx = ctx_of(folders, kernel, &system);
                pick_json(self, &ctx, params, now())
            }
            "account.rows" => {
                let (folders, kernel) = self.folders_for(params)?;
                let system = accounts::System {
                    kernel: kernel.clone(),
                };
                let ctx = ctx_of(folders, kernel, &system);
                rows_json(self, &ctx, params, now())
            }
            "account.out" | "account.clear" | "account.usage" | "account.started" => {
                write(self, method, params)
            }
            _ => Err(Error::NoSuchMethod(format!(
                "kith-core: no method {method}"
            ))),
        }
    }
}

fn ctx_of(
    folders: kith_kernel::paths::Paths,
    kernel: std::path::PathBuf,
    out: &accounts::System,
) -> Ctx<'_> {
    Ctx {
        machine: folders.machine,
        agents: folders.agents,
        kernel,
        home: kith_kernel::paths::process_home(),
        claude_config_dir: None,
        out,
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// One entry's state, or all of them: the shape of `state.json`'s `accounts`.
pub(crate) fn state_json(core: &Core, entry: Option<&str>) -> Result<Value> {
    let all = super::read_all(core.conn())?;
    Ok(match entry {
        Some(id) => json!({ "id": id, "state": all.get(id).cloned().unwrap_or(Value::Null) }),
        None => json!({ "accounts": all }),
    })
}

/// `kith account pick AGENT --json` over core.db: the pin, then the set in policy order, minus what is out or signed out.
pub(crate) fn pick_json(core: &Core, ctx: &Ctx<'_>, params: &Value, now: f64) -> Result<Value> {
    let agent = word(params, "agent")
        .filter(|a| !a.is_empty())
        .ok_or_else(|| Error::BadParams("kith-core: account.pick: agent is required".into()))?;
    let md = word(params, "agentMd").map(Path::new);
    let deep = params.get("deep").and_then(Value::as_bool).unwrap_or(false);
    let s = accounts::settings::read(ctx);
    let src = CoreState(core.conn());
    let p = accounts::pick_from(ctx, &s, agent, md, deep, now, &src);
    Ok(accounts::pick_value(
        agent,
        accounts::settings::cross_harness(&s, agent),
        &p,
    ))
}

/// `kith account list --json` over core.db: one row per account (the array `account.rows` answers).
pub(crate) fn rows_json(core: &Core, ctx: &Ctx<'_>, params: &Value, now: f64) -> Result<Value> {
    let agent = word(params, "agent").unwrap_or_default();
    if !agent.is_empty() && accounts::agent_md_of(ctx, agent).is_none() {
        return Err(Error::BadParams(format!(
            "kith-core: account.rows: no agent '{agent}' (kith ls)"
        )));
    }
    let deep = params.get("deep").and_then(Value::as_bool).unwrap_or(true);
    let s = accounts::settings::read(ctx);
    let src = CoreState(core.conn());
    let rows = accounts::rows_from(ctx, &s, agent, deep, now, &src);
    Ok(Value::Array(rows.into_iter().map(Value::Object).collect()))
}

fn bad(method: &str, why: impl std::fmt::Display) -> Error {
    Error::BadParams(format!("kith-core: {method}: {why}"))
}

/// `account.out|clear|usage|started`: checked as `kith account` checks them, then written in one transaction.
fn write(core: &Core, method: &str, params: &Value) -> Result<Value> {
    use accounts::Change;
    let id = word(params, "id")
        .filter(|i| !i.is_empty())
        .ok_or_else(|| bad(method, "id is required"))?;
    accounts::parse(id).map_err(|e| bad(method, e))?;
    let int = |k: &str| params.get(k).and_then(Value::as_i64);
    let now = int("now").unwrap_or_else(accounts::clock_secs);
    let (change, answer) = match method {
        "account.out" => {
            let reason = word(params, "reason").unwrap_or("manual");
            if !accounts::reason_ok(reason) {
                return Err(bad(
                    method,
                    "reason is a short word (letters, digits, space . _ -)",
                ));
            }
            let until = int("until")
                .filter(|u| *u != 0)
                .unwrap_or(now + accounts::OUT_FOR);
            if until <= now {
                return Err(bad(method, "until is in the past"));
            }
            (
                Change::Out {
                    id: id.to_owned(),
                    until: Some(until),
                    reason: reason.to_owned(),
                    extend_only: params
                        .get("extend")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
                json!({ "id": id, "outUntil": until, "reason": reason }),
            )
        }
        "account.clear" => (
            Change::Clear {
                id: id.to_owned(),
                host_until: int("hostUntil").unwrap_or(0),
            },
            json!({ "id": id }),
        ),
        "account.usage" => {
            let reading = params
                .get("reading")
                .ok_or_else(|| bad(method, "reading is required"))?;
            let usage = accounts::validate_usage(reading, now).map_err(|e| bad(method, e))?;
            (
                Change::Usage {
                    id: id.to_owned(),
                    usage: usage.clone(),
                },
                usage,
            )
        }
        _ => (Change::Started { id: id.to_owned() }, json!({ "id": id })),
    };
    if !super::user_writes(core.conn())? {
        return Err(Error::Refused(format!(
            "kith-core: {method}: account state is still in state.json; kith migrate step accounts moves it here"
        )));
    }
    super::make(core.conn(), &change, now)?;
    Ok(answer)
}
