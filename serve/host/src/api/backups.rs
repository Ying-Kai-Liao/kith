//! The backup chain for the apps: which entries kith will move a bot's turns to when its own account runs out, whether
//! each can run, and when one that ran out is back. `backups` reads it (a paired phone may too); `backupsSet` writes
//! it, one call that replaces a whole list. Above the chain is each agent's main account, the one its turns start on
//! (`kith account use`); `accountSet` chooses it. Logins stay in the terminal: an entry that isn't ready carries the one
//! command to run (`fix`), and no token or environment value is ever read here (kith's JSON has none).
//! Not the bot's own harness (`BotConfig.backend`) and not usage bars (`usage.rs`).

use crate::agent::hq;
use crate::hub::Hub;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Each entry's login is asked of its harness, which can take a few seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Two quick reorders: one `kith` write at a time, the last one to arrive wins.
static WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "backups" => read(hub).await?,
        "backupsSet" => {
            set(hub, &b).await?;
            read(hub).await?
        }
        "accountSet" => {
            set_main(hub, &b).await?;
            read(hub).await?
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

async fn kith() -> Result<std::path::PathBuf> {
    tokio::task::spawn_blocking(hq::bin)
        .await?
        .ok_or_else(|| anyhow!("kith isn't installed on this computer (set KITH_KERNEL_BIN, or put `kith` on PATH)"))
}

fn json_of(out: &str, what: &str) -> Result<Value> {
    serde_json::from_str(out.trim()).with_context(|| format!("`{what}` didn't print JSON"))
}

/// `kith backup list [--agent A] --json`: (source, rows, main account). The main account is null from a kith before
/// main accounts.
async fn chain(bin: &Path, agent: Option<&str>) -> Result<(String, Vec<Value>, Value)> {
    let mut args = vec!["backup", "list", "--json"];
    if let Some(agent) = agent {
        args.extend(["--agent", agent]);
    }
    let out = hq::run(bin, &args, None, READ_TIMEOUT).await?;
    let doc = json_of(&out, "kith backup list --json")?;
    let rows = doc["backups"].as_array().ok_or_else(|| anyhow!("`kith backup list --json` printed no `backups`"))?;
    Ok((doc["source"].as_str().unwrap_or("global").to_owned(), rows.clone(), doc["main"].clone()))
}

/// An agent's main account as the apps get it: its id, where the choice comes from (`agent`, `global`, or `default`
/// for the harness's own login), the agent's harness (which ids it may take) and, when it can't run, why and the
/// command that fixes it.
fn main_row(main: &Value) -> Option<Value> {
    let id = main["id"].as_str()?;
    Some(json!({
        "id": id,
        "source": main["source"].as_str().unwrap_or("default"),
        "harness": main["harness"].as_str(),
        "ready": main["ready"] != false,
        "why": main["why"].as_str(),
        "fix": main["fix"].as_str(),
    }))
}

fn ids(rows: &[Value]) -> Vec<Value> {
    rows.iter().filter_map(|r| r["id"].as_str()).map(|i| json!(i)).collect()
}

/// An entry id split the way kith does: `claude-api:main` runs on claude, as a key.
fn parts(id: &str) -> (String, Option<String>, &'static str) {
    let (harness, account) = id.split_once(':').map_or((id, None), |(h, a)| (h, Some(a.to_owned())));
    let kind = if harness == "claude-api" { "api-key" } else { "subscription" };
    let harness = if harness == "claude-api" { "claude" } else { harness };
    (harness.to_owned(), account, kind)
}

async fn read(hub: &Hub) -> Result<Value> {
    let bin = kith().await?;
    let agents = hq::list(&bin).await.map(|a| a.into_iter().map(|a| a.name).collect::<Vec<_>>());
    let (global, accounts, own) = tokio::join!(
        chain(&bin, None),
        async {
            json_of(
                &hq::run(&bin, &["account", "list", "--json"], None, READ_TIMEOUT).await?,
                "kith account list --json",
            )
        },
        futures::future::join_all(agents.unwrap_or_default().into_iter().map(|name| {
            let bin = &bin;
            async move { (name.clone(), chain(bin, Some(&name)).await) }
        })),
    );
    let (_, global, global_main) = global?;
    // An agent that follows the global order isn't listed; one whose chain failed to read is left out too.
    let mut per_agent = Map::new();
    let mut mains = Map::new();
    let mut rows: BTreeMap<String, Value> = BTreeMap::new();
    for row in &global {
        if let Some(id) = row["id"].as_str() {
            rows.entry(id.to_owned()).or_insert_with(|| row.clone());
        }
    }
    for (name, got) in own {
        let Ok((source, list, main)) = got else { continue };
        if let Some(main) = main_row(&main) {
            mains.insert(name.clone(), main);
        }
        if source == "agent" {
            for row in &list {
                if let Some(id) = row["id"].as_str() {
                    rows.entry(id.to_owned()).or_insert_with(|| row.clone());
                }
            }
            per_agent.insert(name, Value::Array(ids(&list)));
        }
    }
    // Extra accounts that exist but aren't in any chain: what the app offers to add.
    if let Ok(Value::Array(accounts)) = accounts {
        for a in accounts {
            let Some(id) = a["id"].as_str() else { continue };
            rows.entry(id.to_owned()).or_insert_with(|| {
                let ready = a["logged_in"] == true;
                let mut row = json!({"id": id, "ready": ready});
                if !ready {
                    row["why"] = a["why"].clone();
                    if let Some((h, n)) = id.split_once(':') {
                        row["fix"] = json!(format!("kith account add {h} {n}"));
                    }
                }
                row
            });
        }
    }
    let out = hub.backups.out();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(id, row)| {
            let (harness, account, kind) = parts(&id);
            let gone = out.get(&id);
            json!({
                "id": id, "harness": harness, "account": account, "kind": kind,
                "ready": row["ready"] == true,
                "why": row["why"].as_str(),
                "fix": row["fix"].as_str(),
                // Milliseconds, like the rest of the API's times; the state file keeps seconds.
                "outUntil": gone.map(|o| o.out_until.saturating_mul(1000)),
                "reason": gone.map(|o| o.reason.as_str()),
            })
        })
        .collect();
    let main = json!({"global": global_main["id"].as_str(), "agents": mains});
    Ok(json!({"global": ids(&global), "agents": per_agent, "entries": entries, "main": main}))
}

fn valid_id(id: &str) -> bool {
    let (harness, name) = id.split_once(':').map_or((id, None), |(h, n)| (h, Some(n)));
    let word = |s: &str, first: fn(char) -> bool| {
        s.chars().next().is_some_and(first)
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    word(harness, |c| c.is_ascii_lowercase()) && name.is_none_or(|n| word(n, |c| c.is_ascii_alphanumeric()))
}

/// `accountSet {agent: string|null, account: string|null}`: the main account of one agent or (agent null) of every
/// agent of that harness. `account: null` goes back to the global choice (with an agent) or to each harness's default
/// login (without). kith checks that the id fits the agent's harness.
async fn set_main(hub: &Hub, b: &Value) -> Result<()> {
    let agent = match &b["agent"] {
        Value::Null => None,
        Value::String(a) if valid_id(a) && !a.contains(':') => Some(a.as_str()),
        _ => bail!("`agent` must be an agent name or null"),
    };
    let mut args = vec!["account", "use"];
    match &b["account"] {
        Value::Null => args.push("--reset"),
        Value::String(id) if valid_id(id) => args.push(id),
        other => bail!("`account` must be an account id kith would take, or null: {other}"),
    }
    if let Some(agent) = agent {
        args.extend(["--agent", agent]);
    }
    let bin = kith().await?;
    let _one_at_a_time = WRITES.lock().await;
    // kith asks the account's harness whether it is logged in, for its note.
    hq::run(&bin, &args, None, READ_TIMEOUT).await?;
    // The next turn starts on the new account. The chains are asked for again now, not left to the next turn: a bot
    // whose session is warm only learns its main account changed from its chain, and an unknown chain changes nothing.
    let agents = hub.backups.known_agents();
    hub.backups.forget_chains();
    futures::future::join_all(agents.iter().map(|agent| {
        let bin = &bin;
        async move {
            if let Ok(chain) = hq::backups(bin, agent).await {
                hub.backups.end_refresh(agent, Some(chain));
            }
        }
    }))
    .await;
    Ok(())
}

/// `backupsSet {agent: string|null, chain: [ids]|null}`: the whole list for one agent or (agent null) for every agent.
/// `chain: null` with an agent drops that agent's own list, so it follows the global order again.
async fn set(hub: &Hub, b: &Value) -> Result<()> {
    let agent = match &b["agent"] {
        Value::Null => None,
        Value::String(a) if valid_id(a) && !a.contains(':') => Some(a.as_str()),
        _ => bail!("`agent` must be an agent name or null"),
    };
    let bin = kith().await?;
    let mut args = vec!["account", "backup"];
    let list: Vec<&str>;
    if b["chain"].is_null() {
        let Some(agent) = agent else { bail!("`chain` is required for the global order") };
        args.extend(["reset", "--agent", agent]);
    } else {
        let given = b["chain"].as_array().ok_or_else(|| anyhow!("`chain` must be a list of entry ids"))?;
        list = given
            .iter()
            .map(|v| {
                v.as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("`chain` has an id kith wouldn't take: {v}"))
            })
            .collect::<Result<_>>()?;
        args.push("set");
        args.extend(list.iter().copied());
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
    }
    let _one_at_a_time = WRITES.lock().await;
    hq::run(&bin, &args, None, WRITE_TIMEOUT).await?;
    // Turns started from now on use the new order, not the chain kith gave up to minutes ago.
    hub.backups.forget_chains();
    Ok(())
}
