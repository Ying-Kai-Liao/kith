//! The cost rows of background runs (summaries, tidy-ups): written to the agent's ledger through `kith cost record`.

use crate::chat::oneshot::Reply;
use anyhow::anyhow;
use std::time::Duration;

const COST_TIMEOUT: Duration = Duration::from_secs(15);

/// How a background run is tagged in the cost ledger: `via` (what kind of run), and for a run done for a person or
/// a project, who and which (`KITH_PERSON`, `KITH_PROJECT`, as for a run of that person's).
pub(crate) struct Tag<'a> {
    pub via: &'a str,
    pub turn: &'a str,
    pub person: Option<&'a str>,
    pub project: Option<&'a str>,
}

/// Writes the run's tokens to the agent's cost ledger (`kith cost record`). Nothing when the agent didn't report
/// tokens; a failure is logged and never fails the run.
pub(crate) fn record_cost(agent: &str, tag: &Tag, reply: &Reply) {
    let Some(usage) = reply.usage else {
        tracing::debug!(agent, "the one-shot reported no token usage: no cost row for this tidy");
        return;
    };
    let row = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(), "session": reply.session, "model": "haiku", "turn": tag.turn,
        "input": usage.input, "output": usage.output, "cache_read": usage.cache_read,
        "cache_write": usage.cache_write, "cost": null,
    });
    let agent = agent.to_owned();
    let (via, person, project) = (tag.via.to_owned(), tag.person.map(str::to_owned), tag.project.map(str::to_owned));
    tokio::spawn(async move {
        let result = async {
            let bin = tokio::task::spawn_blocking(crate::agent::hq::bin)
                .await?
                .ok_or_else(|| anyhow!("no kith to record with"))?;
            let mut child = tokio::process::Command::new(bin)
                .args(["cost", "record"])
                .env("KITH_AGENT", &agent)
                .env("KITH_VIA", &via)
                .env("KITH_PERSON", person.as_deref().unwrap_or_default())
                .env("KITH_PROJECT", project.as_deref().unwrap_or_default())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                tokio::io::AsyncWriteExt::write_all(&mut stdin, row.to_string().as_bytes()).await?;
            }
            tokio::time::timeout(COST_TIMEOUT, child.wait()).await??;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(agent, error = format!("{e:#}"), "couldn't record a background run's cost");
        }
    });
}
