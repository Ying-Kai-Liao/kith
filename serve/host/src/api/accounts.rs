//! The managed account set for the apps (`docs/accounts.md`): the host is a client of `kith account ...`; the kernel owns
//! the set, the policy, the pins and which accounts are out. Owner only (nothing here is on the member or bot lists in
//! `roles.rs`). No token, folder path or environment value is read or sent: kith's JSON has none, and the account
//! folder (`dir`) is dropped.
//!
//! `accounts` reads it all in one call; every write answers with the same read, fresh:
//!
//! ```text
//! {"policy": "fill"|"spread", "crossHarness": bool,                       // the global setting
//!  "accounts": [{"id", "harness", "account", "kind", "inSet", "order", "status", "ready", "why", "fix",
//!                "outUntil", "reason", "usage": {"readAt", "windows": [{"name", "percent", "resetsAt"?}]} | null,
//!                "lastStarted", "pins": [{"agent", "only"}], "loggedIn"}],  // one per account, set or not
//!  "agents": {"<agent>": {"harness", "pin": {"id", "only"} | null, "own": [ids] | null, "order": [ids], "main",
//!                         "candidates": [ids], "allOut": bool, "crossHarness": bool,
//!                         "skipped": [{"id", "status", "why", "outUntil", "reason"}]}}}
//! ```
//!
//! Times are milliseconds like the rest of the API (the kernel's are seconds); `outUntil`, `reason` only when the
//! status is `out`. `status` is `ready`, `out`, `signed_out`, `unsupported` or `invalid`. An agent's `order` is its
//! static order (pin first), `candidates` what a run would try now (`kith account pick`), `own` its own list or `null`
//! when it follows the global set. An agent whose view could not be read is left out of `agents`.
//!
//! Writes (each takes the same lock as `backups`' writes, one `kith` write at a time, the last wins):
//! `accountsSet {agent?, ids}` the whole list (`agent` absent: the global set), `accountsSet {agent, reset: true}` drops
//! the agent's own list; `accountPolicy {policy}`; `accountPin {agent, id, only?}` / `accountUnpin {agent}`;
//! `accountCrossHarness {agent?, value: "on"|"off"|"reset"}` (`reset` only with an agent); `accountClear {id}` ("it is
//! back"); `accountAdd {harness, name, yes?, cols?, rows?}` / `accountRemove {id}`. Logins are the harness's own, in a
//! terminal the host opens (`kith account add ...`); a new Claude account first answers `{ok: false, warning}` and
//! is made only when asked again with `yes`.

use super::backups::{READ_TIMEOUT, WRITE_TIMEOUT, WRITES, kith, refresh_known, valid_agent, valid_id};
use crate::agent::hq;
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "accounts" => read(&kith().await?).await?,
        "accountsSet" => write(hub, set_args(&b)?).await?,
        "accountPolicy" => {
            let policy = b["policy"].as_str().filter(|p| matches!(*p, "fill" | "spread"));
            write(hub, vec!["policy".into(), policy.ok_or_else(|| anyhow!("`policy` must be fill or spread"))?.into()])
                .await?
        }
        "accountPin" => {
            let (agent, id) = (agent_of(&b["agent"])?.ok_or_else(|| anyhow!("`agent` is required"))?, id_of(&b["id"])?);
            let mut args = vec!["pin".to_owned(), agent.to_owned(), id.to_owned()];
            if b["only"].as_bool() == Some(true) {
                args.push("--only".into());
            }
            write(hub, args).await?
        }
        "accountUnpin" => {
            let agent = agent_of(&b["agent"])?.ok_or_else(|| anyhow!("`agent` is required"))?;
            write(hub, vec!["unpin".into(), agent.to_owned()]).await?
        }
        "accountCrossHarness" => {
            let agent = agent_of(&b["agent"])?;
            let value = b["value"].as_str().filter(|v| matches!(*v, "on" | "off" | "reset"));
            let value = value.ok_or_else(|| anyhow!("`value` must be on, off or reset"))?;
            if value == "reset" && agent.is_none() {
                bail!("`reset` needs an `agent`");
            }
            let mut args = vec!["cross-harness".to_owned(), value.to_owned()];
            args.extend(agent.map(|a| ["--agent".to_owned(), a.to_owned()]).into_iter().flatten());
            write(hub, args).await?
        }
        "accountClear" => {
            let id = id_of(&b["id"])?;
            let out = write(hub, vec!["clear".into(), id.to_owned()]).await?;
            // The host's own early mark (usage readings) goes too, or the next turn would put it back. Only once the
            // kernel has cleared: after a failure the account is still out.
            hub.backups.clear_local(id);
            out
        }
        "accountAdd" => add(hub, &b).await?,
        "accountRemove" => {
            let id = account_id(&b)?;
            write(hub, vec!["remove".into(), id, "--yes".into()]).await?
        }
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

fn agent_of(v: &Value) -> Result<Option<&str>> {
    match v {
        Value::Null => Ok(None),
        Value::String(a) if valid_agent(a) => Ok(Some(a)),
        _ => bail!("`agent` must be an agent name"),
    }
}

fn id_of(v: &Value) -> Result<&str> {
    v.as_str().filter(|i| valid_id(i)).ok_or_else(|| anyhow!("`id` must be an account id kith would take: {v}"))
}

/// `accountsSet`'s `kith account set ...` arguments.
fn set_args(b: &Value) -> Result<Vec<String>> {
    let agent = agent_of(&b["agent"])?;
    let mut args = vec!["set".to_owned()];
    if b["reset"].as_bool() == Some(true) {
        let Some(agent) = agent else { bail!("`reset` needs an `agent`") };
        args.extend(["--reset".to_owned(), "--agent".to_owned(), agent.to_owned()]);
        return Ok(args);
    }
    let ids = b["ids"].as_array().ok_or_else(|| anyhow!("`ids` must be a list of account ids"))?;
    for id in ids {
        args.push(id_of(id)?.to_owned());
    }
    if let Some(agent) = agent {
        args.extend(["--agent".to_owned(), agent.to_owned()]);
    }
    Ok(args)
}

/// `kith account <args>` under the write lock, then the fresh read. The chains the turns use are asked for again.
async fn write(hub: &Hub, args: Vec<String>) -> Result<Value> {
    let bin = kith().await?;
    {
        let _one_at_a_time = WRITES.lock().await;
        let mut full: Vec<&str> = vec!["account"];
        full.extend(args.iter().map(String::as_str));
        hq::run(&bin, &full, None, WRITE_TIMEOUT).await?;
    }
    // Turns started from now on use the new set: the chains are read again (turns go on with the old ones meanwhile).
    refresh_known(hub, &bin).await;
    read(&bin).await
}

fn account_id(b: &Value) -> Result<String> {
    if let Some(id) = b["id"].as_str() {
        return valid_id(id)
            .then(|| id.to_owned())
            .ok_or_else(|| anyhow!("`id` must be an account id kith would take"));
    }
    let (harness, name) = (b["harness"].as_str(), b["name"].as_str());
    match (harness, name) {
        (Some(h), Some(n)) if matches!(h, "claude" | "codex") && valid_id(&format!("{h}:{n}")) => {
            Ok(format!("{h}:{n}"))
        }
        _ => bail!("give `id`, or `harness` (claude or codex) and a `name` of letters, digits, . _ -"),
    }
}

// MARK: the read

/// Kith's seconds as the API's milliseconds.
#[allow(clippy::cast_possible_truncation, reason = "epoch seconds times a thousand fit an i64")]
fn ms(v: &Value) -> Value {
    v.as_f64().map_or(Value::Null, |s| json!((s * 1000.0) as i64))
}

fn usage(u: &Value) -> Value {
    let Some(windows) = u["windows"].as_array() else { return Value::Null };
    let windows: Vec<Value> = windows
        .iter()
        .map(|w| {
            let mut out = json!({"name": w["name"], "percent": w["percent"]});
            if !w["resets_at"].is_null() {
                out["resetsAt"] = ms(&w["resets_at"]);
            }
            out
        })
        .collect();
    json!({"readAt": ms(&u["read_at"]), "windows": windows})
}

/// One row of `kith account list --json` as the apps get it: camelCase, milliseconds, and no folder.
fn row(r: &Value) -> Value {
    let mut out = json!({
        "id": r["id"], "harness": r["harness"], "account": r["account"], "kind": r["kind"],
        "inSet": r["in_set"] == true, "order": r["order"], "status": r["status"], "ready": r["ready"] == true,
        "why": r["why"], "fix": r["fix"], "usage": usage(&r["usage"]), "lastStarted": ms(&r["last_started"]),
        "pins": r["pins"], "loggedIn": r["logged_in"] != false,
    });
    if r["status"] == "out" {
        out["outUntil"] = ms(&r["out_until"]);
        out["reason"] = r["reason"].clone();
    }
    out
}

fn skipped(s: &Value) -> Value {
    let mut out = json!({"id": s["id"], "status": s["status"], "why": s["why"]});
    if s["status"] == "out" {
        out["outUntil"] = ms(&s["out_until"]);
        out["reason"] = s["reason"].clone();
    }
    out
}

/// One agent's view from `kith <agent> --accounts-json`. `own` is the kernel's: the agent's own list, else null.
fn agent_view(doc: &Value) -> Value {
    let strings = |v: &Value| -> Vec<Value> { v.as_array().cloned().unwrap_or_default() };
    json!({
        "harness": doc["harness"], "pin": doc["pin"], "order": strings(&doc["order"]), "main": doc["main"],
        "own": doc["own"].as_array().map(|_| doc["own"].clone()),
        "candidates": strings(&doc["candidates"]), "allOut": doc["all_out"] == true,
        "crossHarness": doc["cross_harness"] == true,
        "skipped": strings(&doc["skipped"]).iter().map(skipped).collect::<Vec<_>>(),
    })
}

async fn json_of(bin: &Path, args: &[&str]) -> Result<Value> {
    let out = hq::run(bin, args, None, READ_TIMEOUT).await?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("`kith {}` didn't print JSON: {e}", args.join(" ")))
}

/// The answer of `accounts`: the global rows, and each hq agent's view, read at once (the harnesses are asked whether
/// each login is good, which takes a second or two, so the agents are read side by side).
async fn read(bin: &Path) -> Result<Value> {
    let agents = hq::list(bin).await.map(|a| a.into_iter().map(|a| a.name).collect::<Vec<_>>()).unwrap_or_default();
    let (rows, policy, cross, views) = tokio::join!(
        json_of(bin, &["account", "list", "--json"]),
        hq::run(bin, &["account", "policy"], None, READ_TIMEOUT),
        hq::run(bin, &["account", "cross-harness"], None, READ_TIMEOUT),
        futures::future::join_all(agents.into_iter().map(|name| async move {
            let doc = hq::accounts_doc(bin, &name).await;
            (name, doc.ok().map(|d| agent_view(&d)))
        })),
    );
    let rows = rows?;
    let rows = rows.as_array().ok_or_else(|| anyhow!("`kith account list --json` printed no list"))?;
    let mut by_agent = Map::new();
    for (name, view) in views {
        if let Some(view) = view {
            by_agent.insert(name, view);
        }
    }
    Ok(json!({
        "policy": policy.ok().map_or_else(|| "fill".to_owned(), |p| p.trim().to_owned()),
        "crossHarness": cross.is_ok_and(|c| c.trim() == "on"),
        "accounts": rows.iter().map(row).collect::<Vec<_>>(),
        "agents": by_agent,
    }))
}

// MARK: add

/// What a kith command printed on stdout, whatever its exit code: `account add` without `--yes` prints the warning and
/// then ends on the missing answer, and `hq::run` keeps only the error text of a failure. Nothing printed is an error.
async fn stdout_of(bin: &Path, args: &[&str]) -> Result<String> {
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
        .map_err(|_| anyhow!("`kith {}` took longer than {}s", args.join(" "), WRITE_TIMEOUT.as_secs()))??;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        bail!("`kith {}` printed nothing", args.join(" "));
    }
    Ok(text)
}

/// The warning `account add claude` prints, without the question it ends on ("Add this account? Type y to continue:"),
/// which the app asks itself.
fn warning_text(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("Add this account?")).collect();
    lines.join("\n").trim().to_owned()
}

/// `accountAdd {harness, name, yes?, cols?, rows?}`: `kith account add`'s own login, in a terminal of the host's. A
/// claude folder that isn't there yet first asks the CLI without `--yes`, which prints the warning it shows a person and
/// stops (nothing is made); the app shows it and asks again with `yes`. A second call while that login runs gets the
/// same terminal. When the login's terminal ends, the chains the turns use are read again.
async fn add(hub: &Arc<Hub>, b: &Value) -> Result<Value> {
    let id = account_id(b)?;
    let Some((harness, name)) = id.split_once(':').filter(|(h, _)| matches!(*h, "claude" | "codex")) else {
        bail!("`harness` must be claude or codex");
    };
    let bin = kith().await?;
    let known = json_of(&bin, &["account", "list", "--json"]).await?;
    // As the CLI decides it: the account's folder is there. An id in the set whose folder is gone is new.
    let exists = known.as_array().is_some_and(|a| {
        a.iter().any(|r| r["id"] == id.as_str() && r["dir"].as_str().is_some_and(|d| Path::new(d).is_dir()))
    });
    if harness == "claude" && !exists && b["yes"].as_bool() != Some(true) {
        // Nothing is made without `--yes`: the CLI prints its warning and ends on the missing answer.
        let warning = warning_text(&stdout_of(&bin, &["account", "add", harness, name]).await?);
        return Ok(json!({"ok": false, "error": "this needs a yes after the warning", "warning": warning}));
    }
    let dim = |k: &str, default: u16| b[k].as_u64().and_then(|v| u16::try_from(v).ok()).unwrap_or(default);
    let command = [
        bin.to_string_lossy().into_owned(),
        "account".into(),
        "add".into(),
        harness.into(),
        name.into(),
        "--yes".into(),
    ];
    let term = {
        let _one_at_a_time = WRITES.lock().await;
        hub.terms.start_argv(&format!("account:{id}"), &command, &[], dim("cols", 80), dim("rows", 24))?
    };
    refresh_known(hub, &bin).await;
    // The login adds the account to the set when it ends: the chains are read again then, not only at its start.
    if let Some(term) = hub.terms.get(&term) {
        let (hub, bin) = (hub.clone(), bin.clone());
        tokio::spawn(async move {
            let (head, mut rx) = term.attach();
            if !head.iter().any(|m| m["type"] == "exit") {
                while let Ok(m) = rx.recv().await {
                    if m["type"] == "exit" {
                        break;
                    }
                }
            }
            refresh_known(&hub, &bin).await;
        });
    }
    Ok(json!({"account": {"ok": true, "id": id, "harness": harness, "name": name, "created": !exists}, "term": term}))
}

#[cfg(test)]
mod tests {
    use super::warning_text;

    #[test]
    fn the_warning_is_the_clis_text_without_the_question_it_ends_on() {
        let printed = "Before adding a second Claude subscription account:\n\n  Whether to do this is your decision.\n\nAdd this account? Type y to continue: ";
        assert_eq!(
            warning_text(printed),
            "Before adding a second Claude subscription account:\n\n  Whether to do this is your decision."
        );
    }
}
