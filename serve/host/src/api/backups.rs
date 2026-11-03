//! The backup chain for the apps: which entries kith will move a bot's turns to when its own account runs out, whether
//! each can run, and when one that ran out is back. `backups` reads it (a paired phone may too); `backupsSet` writes
//! it, one call that replaces a whole list. Above the chain is each agent's main account, the one its turns start on
//! (`kith account use`); `accountSet` chooses it. An entry that isn't ready carries the one command that fixes it
//! (`fix`). `accountAdd` / `accountRemove` call `kith account add|remove --json` (owner only): the host runs the login
//! command the CLI returns in a terminal, never builds it, and no token is ever read here (kith's JSON has none).
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
        "accountAdd" => add_account(hub, &b).await?,
        "accountRemove" => remove_account(&b).await?,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

async fn kith() -> Result<std::path::PathBuf> {
    tokio::task::spawn_blocking(hq::bin).await?.ok_or_else(|| anyhow!("{}", hq::missing()))
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

/// What `kith account` takes: `<harness>[:<name>][@<model>]` (`kernel/lib/kith_accounts.py`'s `parse`), never a leading `-`.
fn valid_id(id: &str) -> bool {
    let (id, model) = id.split_once('@').map_or((id, None), |(i, m)| (i, Some(m)));
    let (harness, name) = id.split_once(':').map_or((id, None), |(h, n)| (h, Some(n)));
    let word = |s: &str, first: fn(char) -> bool| {
        s.chars().next().is_some_and(first)
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    // The model's own charset is wider (`MODEL_RE`): `claude-opus-4-5`, `gpt-5:high`, `a/b[1m]`.
    let model_ok = |m: &str| {
        m.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '[' | ']' | '/'))
    };
    word(harness, |c| c.is_ascii_lowercase())
        && name.is_none_or(|n| word(n, |c| c.is_ascii_alphanumeric()))
        && model.is_none_or(model_ok)
}

/// An agent name: an id with no account or model part.
fn valid_agent(id: &str) -> bool {
    valid_id(id) && !id.contains([':', '@'])
}

/// `accountSet {agent: string|null, account: string|null}`: the main account of one agent or (agent null) of every
/// agent of that harness. `account: null` goes back to the global choice (with an agent) or to each harness's default
/// login (without). kith checks that the id fits the agent's harness.
async fn set_main(hub: &Hub, b: &Value) -> Result<()> {
    let agent = match &b["agent"] {
        Value::Null => None,
        Value::String(a) if valid_agent(a) => Some(a.as_str()),
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
        Value::String(a) if valid_agent(a) => Some(a.as_str()),
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

/// An account's harness and name as `kith account add` takes them; kith is the authority, this only keeps odd
/// arguments from reaching it.
fn account_args(b: &Value) -> Result<(&str, &str)> {
    let harness = b["harness"].as_str().filter(|h| matches!(*h, "claude" | "codex"));
    let name = b["name"].as_str().filter(|n| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    });
    match (harness, name) {
        (Some(h), Some(n)) => Ok((h, n)),
        (None, _) => bail!("`harness` must be claude or codex"),
        _ => bail!("`name` can have letters, digits, . _ - and must start with a letter or digit"),
    }
}

/// Runs a `--json` kith command and returns what it printed even when it exited non-zero: the CLI says why it
/// refused (`ok: false`, a warning to show) on stdout, and `hq::run` keeps only the error text.
async fn run_json(bin: &Path, args: &[&str]) -> Result<Value> {
    let shown = format!("kith {}", args.join(" "));
    let mut command = tokio::process::Command::new(bin);
    command
        .args(args)
        .envs(crate::kernel::both("VIA", "serve"))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .env_remove(crate::runauth::TOKEN_ENV)
        .env_remove(crate::runauth::SOCKET_ENV);
    let out = tokio::time::timeout(WRITE_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("`{shown}` took longer than {}s", WRITE_TIMEOUT.as_secs()))?
        .with_context(|| format!("starting {}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(doc) if doc.is_object() => Ok(doc),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = stderr.trim();
            bail!(
                "`{shown}` failed ({}){}",
                out.status,
                if why.is_empty() { String::new() } else { format!(": {why}") }
            )
        }
    }
}

/// `accountAdd {harness, name, yes?, cols?, rows?}`: makes the account folder with `kith account add --json` and
/// opens a terminal that runs the login command kith returned. A new Claude account first answers
/// `{ok: false, warning, error}` (nothing made) so the app can show the warning; the app asks again with `yes`. A
/// second call while that login runs gets the same terminal.
async fn add_account(hub: &Hub, b: &Value) -> Result<Value> {
    let (harness, name) = account_args(b)?;
    let bin = kith().await?;
    let mut args = vec!["account", "add", harness, name, "--json"];
    if b["yes"].as_bool() == Some(true) {
        args.push("--yes");
    }
    let doc = {
        let _one_at_a_time = WRITES.lock().await;
        run_json(&bin, &args).await?
    };
    if doc["ok"] != true {
        // A warning is a question for the person; anything else (harness not installed, ...) is an error.
        if doc["warning"].is_string() {
            return Ok(json!({"ok": false, "error": doc["error"], "warning": doc["warning"]}));
        }
        bail!("{}", doc["error"].as_str().unwrap_or("kith couldn't add the account"));
    }
    let login = &doc["login"];
    let command: Vec<String> = login["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .filter(|a: &Vec<String>| !a.is_empty())
        .ok_or_else(|| anyhow!("`kith account add --json` printed no login command"))?;
    let env: Vec<(String, String)> = login["env"]
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
        .unwrap_or_default();
    let id = doc["id"].as_str().unwrap_or_default();
    let dim = |k: &str, default: u16| b[k].as_u64().and_then(|v| u16::try_from(v).ok()).unwrap_or(default);
    let term = hub.terms.start_argv(&format!("account:{id}"), &command, &env, dim("cols", 80), dim("rows", 24))?;
    // The folder and the login command stay on the computer: the app gets the account's name and the terminal.
    Ok(json!({
        "account": {
            "ok": true, "id": id, "harness": doc["harness"], "name": doc["name"], "created": doc["created"],
            "warning": doc["warning"],
        },
        "term": term,
    }))
}

/// `accountRemove {harness, name}`: `kith account remove --yes --json`; the app confirms first. Answers kith's JSON
/// (`changed`, `in_chain`) without the folder's path.
async fn remove_account(b: &Value) -> Result<Value> {
    let (harness, name) = account_args(b)?;
    let bin = kith().await?;
    let _one_at_a_time = WRITES.lock().await;
    let mut doc = run_json(&bin, &["account", "remove", harness, name, "--yes", "--json"]).await?;
    if doc["ok"] != true {
        bail!("{}", doc["error"].as_str().unwrap_or("kith couldn't remove the account"));
    }
    if let Some(o) = doc.as_object_mut() {
        o.remove("dir");
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_what_kith_account_takes() {
        for ok in
            ["claude", "claude:work", "claude@sonnet", "claude:work@opus", "codex@gpt-5:high", "claude@claude-opus-4-5"]
        {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "-x", "claude@", "claude@-x", "claude@a b", "claude :x", "Claude", "claude:@x", "claude@a;b"] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_agent("alice") && !valid_agent("claude@sonnet") && !valid_agent("claude:work"));
    }
}
