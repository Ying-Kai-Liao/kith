//! The mode a Claude session starts in: the adapter starts every session in "default", so the host asks for the mode
//! `kith <agent> --launch-json` reports (`session/set_mode`). An invited person's run keeps the confined mode and is
//! never asked into bypass. Against the stub kith (its `permission` file is launch-json's `permission_mode`) and the
//! scripted ACP agent that lists the modes the way claude-agent-acp does.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::host::Host;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A host over the `house` stub agent, whose launch-json reports `permission` as its permission mode.
async fn start(permission: &str) -> (Host, PathBuf) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let home = std::env::temp_dir().join(format!("kith-mode-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("permission"), permission).unwrap();
    let acp = format!("{} claude", common::python_agent(&fixtures.join("instructions_agent.py")));
    let host = Host::start_env(&[
        ("KITH_KERNEL_BIN", &fixtures.join("stub_hq.py").to_string_lossy()),
        ("STUB_HQ_HOME", &home.to_string_lossy()),
        ("KITH_SERVE_HQ_ACP_COMMAND", &acp),
        ("KITH_AUTH", &home.join("auth.json").to_string_lossy()),
        ("KITH_STATE", &home.join("state").to_string_lossy()),
        ("KITH_HOME", &home.join("kith-home").to_string_lossy()),
        ("RUST_LOG", "kith_serve=debug,kith_serve::agent=trace,kith_core=debug,kith_kernel=debug"),
    ])
    .await;
    (host, home)
}

async fn house(host: &Host) -> String {
    for _ in 0..200 {
        if let Some(bot) = host.roster().await.into_iter().find(|b| b["hqAgent"] == "house") {
            return bot["id"].as_str().unwrap().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("house never appeared");
}

/// Sends `go` once and returns the ACP agent's requests once it has answered a prompt (so the session exists and its
/// mode is settled). The send is not repeated: the host answers a clientNonce it already saved with the saved entry
/// and starts nothing (api/bots.rs, `find_by_nonce`), so a repeat could not start a turn and would only hide a lost
/// one. The deadline is generous for a loaded CI box; a turn that never reaches the agent fails the test, with the
/// requests and the chat's entries (the host's own notice, if any) in the message.
async fn requests_after_a_turn(host: &Host, bot: &str, home: &Path) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(90);
    host.call("send", json!({"botId": bot, "text": "go", "clientNonce": "n1"})).await;
    loop {
        let text = std::fs::read_to_string(home.join("requests.jsonl")).unwrap_or_default();
        let all: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        if all.iter().any(|r| r["method"] == "session/prompt") {
            return all;
        }
        if Instant::now() >= deadline {
            probe_dump(host, home);
            let history = host.call("history", json!({"botId": bot})).await;
            panic!("the agent never got a prompt; requests {all:?}; chat {history}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// STRESS PROBE (never merged): what the host and the stub were doing when the turn never reached the agent.
fn probe_dump(host: &Host, home: &Path) {
    use std::hash::{Hash as _, Hasher as _};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    host.home.hash(&mut hash);
    let log = PathBuf::from("/tmp").join(format!("ks-{:x}", hash.finish())).join("host-output.log");
    eprintln!("PROBE-DUMP host home {}", host.home.display());
    eprintln!("PROBE-DUMP stub calls:\n{}", std::fs::read_to_string(home.join("calls.jsonl")).unwrap_or_default());
    let ps = std::process::Command::new("ps").args(["-eo", "pid,ppid,etimes,stat,wchan:20,args", "--forest"]).output();
    if let Ok(ps) = ps {
        let text = String::from_utf8_lossy(&ps.stdout);
        let keep: Vec<&str> = text.lines().filter(|l| l.contains("kith") || l.contains("python") || l.contains("PID")).collect();
        eprintln!("PROBE-DUMP ps:\n{}", keep.join("\n"));
    }
    let port = host.base().rsplit(':').next().unwrap_or_default().to_owned();
    let pgrep = std::process::Command::new("pgrep").args(["-f", &format!("kith-serve serve --bind 127.0.0.1 --port {port}$")]).output();
    if let Ok(out) = pgrep {
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let bt = std::process::Command::new("sudo")
                .args(["gdb", "-p", pid, "-batch", "-ex", "set pagination off", "-ex", "thread apply all bt 40"])
                .output();
            if let Ok(bt) = bt {
                let text = String::from_utf8_lossy(&bt.stdout).into_owned();
                let _ = std::fs::write(format!("/tmp/probe-gdb-{pid}.txt"), &text);
                eprintln!("PROBE-DUMP gdb {pid}: {} bytes, frames with kith:\n{}", text.len(),
                    text.lines().filter(|l| l.contains("kith") || l.starts_with("Thread")).take(400).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    eprintln!("PROBE-DUMP host log {} ({} lines):\n{}", log.display(), lines.len(), lines[lines.len().saturating_sub(300)..].join("\n"));
    let _ = std::fs::copy(&log, format!("/tmp/probe-host-{}.log", uuid::Uuid::new_v4()));
}

fn set_modes(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|r| r["method"] == "session/set_mode")
        .map(|r| r["params"]["modeId"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn an_owners_run_starts_in_the_mode_launch_json_reports() {
    let (host, home) = start("acceptEdits").await;
    let bot = house(&host).await;
    let requests = requests_after_a_turn(&host, &bot, &home).await;
    assert_eq!(set_modes(&requests), ["acceptEdits"], "{requests:?}");
    // The mode is asked for before the first prompt, not after.
    let order: Vec<&str> = requests.iter().map(|r| r["method"].as_str().unwrap()).collect();
    let set = order.iter().position(|m| *m == "session/set_mode").unwrap();
    let prompt = order.iter().position(|m| *m == "session/prompt").unwrap();
    assert!(set < prompt, "{order:?}");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn an_invitees_run_never_starts_in_bypass() {
    // The kernel's mode for the agent is a free shell; for sam's run it is confined to "default", which is where the
    // adapter starts anyway, so nothing is set.
    let t0 = Instant::now();
    let (mut host, home) = start("bypassPermissions").await;
    let _ = house(&host).await;
    eprintln!("PROBE house {:?} home {}", t0.elapsed(), host.home.display());
    let mut chat = String::new();
    host.restart(|dir| {
        let db = rusqlite::Connection::open(dir.join("serve.db")).unwrap();
        let n = db.execute("UPDATE bots SET config = json_set(config, '$.person', 'sam')", []).unwrap();
        assert_eq!(n, 1, "one bot, the house");
        chat = db.query_row("SELECT id FROM bots", [], |r| r.get(0)).unwrap();
    })
    .await;
    eprintln!("PROBE restarted {:?}", t0.elapsed());
    eprintln!("PROBE roster loop done {:?}", t0.elapsed());
    let requests = requests_after_a_turn(&host, &chat, &home).await;
    eprintln!("PROBE turn done {:?}", t0.elapsed());
    assert!(requests.iter().any(|r| r["method"] == "session/new" || r["method"] == "session/load"), "{requests:?}");
    assert!(set_modes(&requests).is_empty(), "{requests:?}");
    let _ = std::fs::remove_dir_all(&home);
}
