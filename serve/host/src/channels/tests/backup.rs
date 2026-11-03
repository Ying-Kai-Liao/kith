//! A channel turn on an account that is out of usage moves to the next one of the agent's backup chain, and says so.
//! The stub kith (`tests/fixtures/channels_stub.py`) plays the accounts; the state is a temporary home.

use super::{Rig, until};
use crate::agent::backups::{ChainEntry, RATE_LIMIT};
use serde_json::{Value, json};

fn entry(id: &str) -> ChainEntry {
    let (harness, account) = id.split_once(':').map_or((id, None), |(h, a)| (h, Some(a.to_owned())));
    let kind = if id == "claude" { "primary" } else { "subscription" };
    ChainEntry { id: id.into(), harness: Some(harness.into()), account, kind: kind.into(), ready: true, why: None }
}

/// The chain the stub prints for `--backups-json`, also known to the host already (as after a first look).
fn chain(r: &Rig, agent: &str, ids: &[&str]) {
    let entries: Vec<ChainEntry> = ids.iter().map(|id| entry(id)).collect();
    r.put("chain", &json!({"chain": entries}));
    assert!(r.backups.begin_refresh(agent));
    r.backups.end_refresh(agent, Some(entries));
}

/// Accounts a run on which ends on a usage limit; when it lifts (epoch seconds) is what comes back.
fn depleted(r: &Rig, accounts: &[&str]) -> i64 {
    let until = r.backups.now() + 7200;
    r.put("depleted", &accounts.iter().map(|a| ((*a).to_owned(), json!(until))).collect::<Value>());
    until
}

/// The accounts the stub's runs went on, in order (`None`: the agent's own).
fn runs(r: &Rig) -> Vec<Option<String>> {
    r.calls("kith")
        .iter()
        .filter(|c| c["backups_json"].is_null())
        .map(|c| c["backup"].as_str().map(str::to_owned))
        .collect()
}

async fn answered(r: &Rig, n: usize) {
    until("the answer", || r.fake.posts().len() >= n).await;
    r.idle().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_account_hands_the_run_to_the_next_and_the_answer_says_so() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex"]);
    let until = depleted(&r, &["primary"]);
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;

    assert_eq!(runs(&r), [None, Some("codex".to_owned())]);
    let (_, text) = &r.fake.posts()[0];
    assert!(text.starts_with("echo:[Kith] You are answering on a backup account"), "{text}");
    assert!(text.contains("hello"), "the thread's task still goes: {text}");
    let note = text.rsplit("\n\n").next().unwrap();
    assert!(note.starts_with("_answered by codex · Claude back at "), "{note}");
    assert!(note.ends_with('_'), "{note}");
    assert!(r.backups.is_out("claude") && !r.backups.is_out("codex"));
    let out = r.backups.out();
    assert_eq!(out["claude"].reason, RATE_LIMIT);
    assert_eq!(out["claude"].out_until, until, "the limit's own reset time is kept");

    // The next message starts on the backup straight away: the spent account is not tried again.
    r.send(&Rig::in_thread(Rig::msg("1", "and more"), "fake/x:room:1")).await;
    answered(&r, 2).await;
    assert_eq!(runs(&r), [None, Some("codex".to_owned()), Some("codex".to_owned())]);
    assert!(r.fake.posts()[1].1.contains("answered by codex"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agents_own_session_is_kept_apart_from_the_backups() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex"]);
    r.send(&Rig::msg("1", "@house first")).await;
    answered(&r, 1).await;
    assert!(!r.fake.posts()[0].1.contains("answered by"), "nothing extra when the primary answers");
    depleted(&r, &["primary"]);
    let key = "fake/x:room:1";
    r.send(&Rig::in_thread(Rig::msg("1", "second"), key)).await;
    answered(&r, 2).await;
    r.send(&Rig::in_thread(Rig::msg("1", "third"), key)).await;
    answered(&r, 3).await;

    let calls = r.calls("kith");
    // The primary resumed its session; the backup began its own (the primary's is another harness's) and then kept it.
    assert_eq!(calls[1]["resume"], "sess-house-1");
    assert!(calls[2]["backup"] == "codex" && calls[2]["resume"].is_null());
    assert!(calls[3]["backup"] == "codex" && calls[3]["resume"].as_str().is_some_and(|s| s.starts_with("sess-codex")));
    assert!(!calls[3]["task"].as_str().unwrap().contains("[Kith] You are answering on a backup"), "told once");
    let th = r.core.with(|i| i.st.threads[key].clone());
    assert_eq!(th["sessions"]["house"], "sess-house-1", "the agent's session isn't overwritten");
    assert!(th["backup_sessions"]["house"]["codex"].as_str().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_account_out_says_who_and_when_the_first_is_back() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex"]);
    depleted(&r, &["primary", "codex"]);
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;

    assert_eq!(runs(&r), [None, Some("codex".to_owned())], "each account once");
    let (_, text) = &r.fake.posts()[0];
    assert!(text.starts_with("⚠️ Every account for this agent is out of usage: "), "{text}");
    assert!(text.contains("claude (back in 1h") || text.contains("claude (back in 2h"), "{text}");
    assert!(text.contains("codex (back in"), "{text}");

    // Another message while everything is out runs nothing and says the same.
    r.send(&Rig::in_thread(Rig::msg("1", "again"), "fake/x:room:1")).await;
    answered(&r, 2).await;
    assert_eq!(runs(&r).len(), 2);
    assert!(r.fake.posts()[1].1.starts_with("⚠️ Every account"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_with_no_chain_runs_as_it_always_did() {
    let r = Rig::new();
    chain(&r, "house", &["claude"]);
    depleted(&r, &["primary"]);
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;
    // The limit is the agent's own error, shown as before; nothing is marked, nothing runs again.
    assert!(r.fake.posts()[0].1.starts_with("⚠️ Claude AI usage limit reached"), "{:?}", r.fake.posts());
    assert_eq!(runs(&r), [None]);
    assert!(r.backups.out().is_empty());

    r.put("depleted", &json!({}));
    r.send(&Rig::in_thread(Rig::msg("1", "later"), "fake/x:room:1")).await;
    answered(&r, 2).await;
    assert_eq!(r.fake.posts()[1].1, super::echo("later"), "no note, the footer only");
    let calls = r.calls("kith");
    assert!(calls.iter().all(|c| c["backup"].is_null()), "no flag");
    assert!(r.calls("kith-chain").is_empty(), "no extra process");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_message_reads_the_chain_before_the_run() {
    // The kernel may already have the account out (a terminal run, another agent), so the first run asks.
    let r = Rig::new();
    r.put("chain", &json!({"chain": [entry("claude"), entry("codex")]}));
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;
    // Nothing is out: the run is on the agent's own account.
    assert_eq!(r.fake.posts()[0].1, super::echo("hello"));
    assert!(r.backups.known_chain("house").is_some_and(|c| c.len() == 2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_something_out_the_chain_is_read_before_the_run() {
    let r = Rig::new();
    r.backups.mark_out("claude", Some(r.backups.now() + 600), RATE_LIMIT);
    r.put("chain", &json!({"chain": [entry("claude"), entry("codex")]}));
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;
    assert_eq!(runs(&r), [Some("codex".to_owned())]);
    assert!(r.fake.posts()[0].1.contains("answered by codex"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_run_again_after_the_run_posted() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex"]);
    depleted(&r, &["primary"]);
    r.send(&Rig::msg("1", "@house SEND hello")).await;
    until("the posts", || r.fake.posts().len() >= 2).await;
    r.idle().await;

    assert_eq!(runs(&r), [None], "the run had posted: it is not repeated");
    let posts = r.fake.posts();
    assert_eq!(posts[0].1, "tool says hi");
    assert!(posts.last().unwrap().1.starts_with("⚠️ Claude AI usage limit reached"), "{posts:?}");
    assert!(r.backups.is_out("claude"));

    // The next message starts on the backup.
    r.send(&Rig::in_thread(Rig::msg("1", "go on"), "fake/x:room:1")).await;
    until("the answer", || r.fake.posts().len() >= 3).await;
    r.idle().await;
    assert_eq!(runs(&r), [None, Some("codex".to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_kith_cannot_start_is_skipped() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex", "claude:work"]);
    depleted(&r, &["primary"]);
    r.put("unavailable", &json!(["codex"]));
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;
    assert_eq!(runs(&r), [None, Some("claude:work".to_owned())], "codex refused to start: nothing of it ran");
    assert!(r.fake.posts()[0].1.contains("answered by claude:work"));
    assert!(!r.backups.is_out("codex"), "not signed in is not out of usage");

    // It is left alone for a while, not tried again by the next message.
    r.send(&Rig::in_thread(Rig::msg("1", "more"), "fake/x:room:1")).await;
    answered(&r, 2).await;
    assert_eq!(runs(&r).last().unwrap().as_deref(), Some("claude:work"));
    assert_eq!(runs(&r).iter().filter(|a| a.as_deref() == Some("codex")).count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_that_is_spent_too_marks_itself_and_the_next_answers() {
    let r = Rig::new();
    chain(&r, "house", &["claude", "codex", "claude:work"]);
    depleted(&r, &["primary", "codex"]);
    r.send(&Rig::msg("1", "@house hello")).await;
    answered(&r, 1).await;
    assert_eq!(runs(&r), [None, Some("codex".to_owned()), Some("claude:work".to_owned())]);
    assert!(r.backups.is_out("claude") && r.backups.is_out("codex") && !r.backups.is_out("claude:work"));
    assert!(r.fake.posts()[0].1.contains("answered by claude:work"));
}
