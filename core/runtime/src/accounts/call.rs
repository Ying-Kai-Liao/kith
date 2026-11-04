//! The `account.*` rows the core answers (M3-9a): read only, over core.db, unswitched. Nothing calls them yet: account
//! state is still written to (and read from) `state.json` by everything that runs. The params were checked against the row.

use std::path::Path;

use kith_kernel::accounts::{self, Ctx, Pick};
use serde_json::{Map, Value, json};

use super::CoreState;
use crate::db::Core;
use crate::error::{Error, Result};

/// `account.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(method, "account.state" | "account.pick")
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
                let ctx = Ctx {
                    machine: folders.machine,
                    agents: folders.agents,
                    kernel,
                    home: kith_kernel::paths::process_home(),
                    claude_config_dir: None,
                    out: &system,
                };
                pick_json(self, &ctx, params, now())
            }
            _ => Err(Error::NoSuchMethod(format!(
                "kith-core: no method {method}"
            ))),
        }
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
    Ok(pick_value(
        agent,
        accounts::settings::cross_harness(&s, agent),
        &p,
    ))
}

/// Python's `pick()` dict (a skipped entry lists its `fix`-less status, as `skipped` does there).
fn pick_value(agent: &str, cross_harness: bool, p: &Pick) -> Value {
    let skipped: Vec<Value> = p
        .skipped
        .iter()
        .map(|x| {
            let mut m = Map::new();
            m.insert("id".into(), json!(x.id));
            m.insert("status".into(), json!(x.status.status));
            if let Some(w) = &x.status.why {
                m.insert("why".into(), json!(w));
            }
            if let Some(u) = x.status.out_until {
                m.insert("out_until".into(), json!(u));
            }
            if let Some(r) = &x.status.reason {
                m.insert("reason".into(), json!(r));
            }
            Value::Object(m)
        })
        .collect();
    json!({
        "version": 1,
        "agent": agent,
        "cross_harness": cross_harness,
        "harness": p.harness,
        "policy": p.policy,
        "pin": p.pin.as_ref().map(|x| json!({"id": x.id, "only": x.only})),
        "candidates": p.candidates,
        "skipped": skipped,
        "all_out": p.all_out,
    })
}
