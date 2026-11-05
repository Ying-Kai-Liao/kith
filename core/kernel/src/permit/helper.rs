//! `ask_helper(agent, task)`: how a guest's run reaches a helper its membership grants. The guest's shell cannot start another
//! run (the OS sandbox closes the network, the login and the other agents' folders), so this runs `kith ask AGENT` outside the
//! sandbox as the run's own person and agent, taken from the environment, which the guest cannot change. The kernel decides as
//! for any helper call (`kith-auth resolve`, with the in-sync wait and `--take-once` in the launch); a helper the membership does
//! not grant is asked of the owner on a card first, which this call waits for.

use super::Env;
use super::decide::{allowed, decide, message_of};
use super::proc::{Failed, run};
use super::pyval::strip;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// How long a helper gets to answer.
pub const HELPER_WAIT_S: u64 = 900;

/// `re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', s)`.
fn is_agent(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// The last `n` characters of `s`.
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

/// The text the agent is told for one `ask_helper` call: the helper's answer, or why there is none.
pub fn ask_helper(a: &Map<String, Value>, env: &Env, kernel: &Path) -> String {
    ask_helper_within(a, env, kernel, Duration::from_secs(HELPER_WAIT_S))
}

pub fn ask_helper_within(
    a: &Map<String, Value>,
    env: &Env,
    kernel: &Path,
    wait: Duration,
) -> String {
    let (agent, task) = (a.get("agent"), a.get("task"));
    let (Some(Value::String(agent)), Some(Value::String(task))) = (agent, task) else {
        return "Give the helper agent and the task.".to_owned();
    };
    if !is_agent(agent) || strip(task).is_empty() {
        return "Give the helper agent and the task.".to_owned();
    }
    if task.chars().count() > 20000 {
        return "The task is too long.".to_owned();
    }
    // who is asking comes from this process's environment, which the run's shell cannot change; kith-auth checks the rest
    let (Some(person), Some(caller)) = (env.get("KITH_PERSON"), env.get("KITH_AGENT")) else {
        return "Helpers are asked from a guest's run only.".to_owned();
    };
    if agent == caller {
        return format!("You are {agent}; ask another agent.");
    }
    let child = env.without(&["KITH_ASK_TOKEN", "KITH_ASK_SOCKET", "HQ_ASK_TOKEN", "HQ_ASK_SOCKET"]);
    if let Some(refused) = helper_card(&child, kernel, person, caller, agent, task) {
        return refused;
    }
    let mut cmd = Command::new(kernel.join("bin/kith-ask"));
    cmd.args([agent.as_str(), "--", task.as_str()]);
    let r = match run(cmd, &child, wait) {
        Ok(r) => r,
        Err(Failed::Timeout) => {
            return format!("{agent} did not answer in time; carry on without it.");
        }
        Err(Failed::Os(why)) => return format!("Could not ask {agent} ({why})."),
    };
    if r.code != 0 && r.stderr.contains("out of sync with the Kith cloud") {
        return format!(
            "{agent} did not run: Kith on this computer is out of sync with the Kith cloud, so it cannot tell yet whether \
             you may still use it. Try again in a minute."
        );
    }
    if r.code != 0 {
        let said = [&r.stderr, &r.stdout]
            .into_iter()
            .find(|s| !s.is_empty())
            .map_or("no answer", String::as_str);
        return format!("Could not ask {agent}: {}", tail(strip(said), 1500));
    }
    let out = strip(&r.stdout);
    if out.is_empty() {
        format!("{agent} gave no answer.")
    } else {
        out.to_owned()
    }
}

/// A helper the caller's membership doesn't grant is asked of the owner on a card while this call waits (the helper, once, for
/// the person's agent in the next 5 minutes; Always for the person; No): the host writes the grant through kith-auth before it
/// answers yes, and the helper's launch checks it again (and uses up a once) with revoke. `None`: go on and ask; else what the
/// agent is told. A person who may not use the caller gets no card: kith ask refuses them in its own words.
fn helper_card(
    child: &Env,
    kernel: &Path,
    person: &str,
    caller: &str,
    agent: &str,
    task: &str,
) -> Option<String> {
    let may = |args: &[&str]| {
        let mut cmd = Command::new(kernel.join("bin/kith-auth"));
        cmd.arg("may").args(args);
        match run(cmd, child, Duration::from_secs(30)) {
            Ok(r) => r.code == 0,
            // kith ask asks kith-auth again and says why
            Err(_) => true,
        }
    };
    if may(&[person, agent, "--via", caller, "--once"]) || !may(&[person, caller]) {
        return None;
    }
    let d = decide(
        child,
        kernel,
        "mcp__kithpermit__ask_helper",
        &json!({"agent": agent, "task": task.chars().take(2000).collect::<String>()}),
    );
    (!allowed(&d)).then(|| message_of(&d))
}
