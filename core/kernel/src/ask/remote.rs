//! `kith ask AGENT@MACHINE "task"`: an agent on another computer of the owner (design 2026-11-24-ask-ledger, section 9, row X0).
//!
//! The interim shape, until the Mac link carries asks (S1 and A5: `peerAsk`): the row is opened here as a foreground ask (so the
//! hop and cost caps apply before anything leaves, and Stop finds it), `bin/kith-remote ask` runs over ssh as before, and the row
//! is finished from what kith-remote leaves. kith-remote is told the chain in `KITH_ASK_LINEAGE` (the JSON it used to ask
//! kith-ask's `delegation_of` for) and where to put the other computer's result in `KITH_REMOTE_RESULT` (a file with the JSON
//! `kith-remote` got back: answer, cost, error, waiting). A kith-remote that does not know the file leaves none, and the row then
//! ends by its exit code. Its output goes straight to this call's own.
//!
//! Unix only: ssh and bash are what the transport is. The Windows build refuses `@MACHINE` by name (no ssh, no bash).

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Host, R, die, dumps, fg, new_id};
use crate::procstart;

pub(super) fn ask(host: &Host, bg: bool, args: &[String], _err: &mut dyn Write) -> R<i32> {
    // TODO(retire-python P2-3 S1 #1194): `--bg AGENT@MACHINE` was the same call in the sh wrapper. The wrapper is gone; the host's
    // run door (S1) starts nothing on another computer yet, so until it does the foreground form is the one that works.
    if bg {
        return Err(die(
            "kith ask: --bg AGENT@MACHINE is not available here yet: use the foreground form",
        ));
    }
    if cfg!(windows) {
        return Err(die(
            "kith ask: AGENT@MACHINE is not available on Windows (it runs over ssh and bash, which Windows has none of)",
        ));
    }
    // The row is made here (the hop caps and the cost cap apply before anything leaves). A TERM between this and the open leaves
    // nothing open: the handler is in place first.
    let mid = new_id("ask");
    (host.watch_term)();
    let pid = std::process::id();
    let mut open = json!({ "id": mid, "asked": args[0], "crossing": true, "mode": "fg", "pid": pid, "cliPid": pid });
    if let Some(stamp) = procstart::start_stamp(pid) {
        open["cliStart"] = json!(stamp);
    }
    fg::open_row(host, open)?;
    let lineage = lineage_of(host, &mid);
    let result = std::env::temp_dir().join(format!("kith-ask-remote-{mid}.json"));
    let mut cmd = Command::new(host.kernel.join("bin/kith-remote"));
    cmd.arg("ask")
        .args(args)
        .env_clear()
        .envs(host.env.iter().cloned())
        .env("KITH_REMOTE_RESULT", &result);
    if let Some(l) = &lineage {
        cmd.env("KITH_ASK_LINEAGE", dumps(l));
    }
    let code = match cmd.spawn() {
        Ok(mut child) => loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(1),
                Ok(None) if (host.terminated)() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&result);
                    return Err(fg::stopped(host, &mid));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                Err(_) => break 1,
            }
        },
        Err(e) => {
            // Nothing ran: the row must not stay open.
            let _ = host.core(
                "ask.settle",
                &host.view(json!({ "id": mid, "cost": 0, "state": "stopped" })),
            );
            return Err(die(format!("kith ask: cannot run kith-remote ({e})")));
        }
    };
    let res = std::fs::read_to_string(&result)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object);
    let _ = std::fs::remove_file(&result);
    // The other computer's cost is the chain's cost here too; a run waiting on a card there has no answer yet.
    let (state, cost) = match &res {
        Some(r) => {
            let truthy = |k: &str| {
                r.get(k).is_some_and(|v| {
                    v.as_bool()
                        .unwrap_or_else(|| !v.is_null() && v.as_i64() != Some(0))
                })
            };
            let state = if truthy("waiting") {
                None
            } else if truthy("error") {
                Some("stopped")
            } else {
                Some("answered")
            };
            (
                state,
                r.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
            )
        }
        // kith-remote leaves a result for every answer it got: none is a call that failed (or an older kith-remote).
        None => (Some(if code == 0 { "answered" } else { "stopped" }), 0.0),
    };
    let mut settle = json!({ "id": mid, "cost": cost });
    if let Some(s) = state {
        settle["state"] = json!(s);
    }
    // kith-remote's own answer is already on this call's output; a row that cannot be settled is the tick's to find.
    let _ = host.core("ask.settle", &host.view(settle));
    Ok(code)
}

/// What kith-remote hands the other computer so its run continues this chain: the chain, this row's hop and cross hops, the row
/// to answer, and the chain's cost so far (`delegation_of`).
fn lineage_of(host: &Host, mid: &str) -> Option<Value> {
    let rec = (host.call)("ask.record", &json!({ "id": mid })).ok()?;
    let all = (host.call)("ask.delegations", &host.view(json!({}))).ok()?;
    let micro = all
        .get("delegations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|d| d.get("id").and_then(Value::as_str) == Some(mid))
        .and_then(|d| d.get("chain_cost_micro_usd"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Some(json!({
        "chain": rec.get("chain")?, "hop": rec.get("hop")?, "cross": rec.get("cross_hops")?,
        "from": mid, "cost": micro / 1e6,
    }))
}
