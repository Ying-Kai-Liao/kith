//! `kith memory summary show | forget | write --thread <id> [--json]`: the stored summary of a thread, from the host
//! store (`serve.db`) without the host running. `write` needs the host (it runs the agent's harness), so it hands the
//! request to `kith summarise`, which asks the running host and says when it can't be reached.

use anyhow::{Context, Result};
use clap::Subcommand;
use kith_mem::rusqlite::{Connection, OpenFlags};
use kith_mem::summary as rules;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// The folders the kernel's kith-memory passes to every host verb; a summary lives in the host store, so unused.
#[derive(clap::Args, Debug, Default)]
pub struct Folders {
    #[arg(long, hide = true, global = true)]
    state: Option<PathBuf>,
    #[arg(long, hide = true, global = true)]
    agents: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum SummaryCmd {
    /// A thread's stored summary: text, when it was written, who may see it, and whether the thread was forgotten.
    Show {
        /// The thread's root entry id, or `main:<bot>` for a bot's own chat.
        #[arg(long)]
        thread: String,
        #[arg(long)]
        json: bool,
    },
    /// Drop a thread's summary and write none for it again; its row stops carrying it. Twice changes nothing more.
    Forget {
        #[arg(long)]
        thread: String,
        #[arg(long)]
        json: bool,
    },
    /// Write the thread's summary now (the running host does it; a shell forces, an agent's call doesn't).
    Write {
        #[arg(long)]
        thread: String,
        #[arg(long)]
        json: bool,
    },
}

/// Whether the run environment is the owner's: at a shell or in their own 1:1 run. Mirrors `thread_command` in
/// kernel/bin/kith-recall, which stays the check for `summarise` and `forget --thread`.
fn owner_only(env: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    let room = env("KITH_ROOM").unwrap_or_default();
    let via = env("KITH_VIA").unwrap_or_default();
    !(set("KITH_RUN_TOKEN")
        || set("KITH_RUN_SOCKET")
        || set("KITH_PERSON")
        || (!room.is_empty() && !room.starts_with("1:1:"))
        || ["telegram", "slack", "discord", "group"].contains(&via.as_str()))
}

fn thread_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

fn db_path() -> PathBuf {
    std::env::var_os("KITH_SERVE_DB").map_or_else(|| crate::service::data_dir().join("serve.db"), PathBuf::from)
}

fn open(write: bool) -> Result<Option<Connection>> {
    let path = db_path();
    if !path.is_file() {
        return Ok(None);
    }
    let flags = if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let conn = Connection::open_with_flags(&path, flags).with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(Some(conn))
}

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |_| Ok(())).is_ok()
}

pub fn run(cmd: SummaryCmd) -> Result<i32> {
    let (SummaryCmd::Show { thread, .. } | SummaryCmd::Forget { thread, .. } | SummaryCmd::Write { thread, .. }) = &cmd;
    if !owner_only(|k| std::env::var(k).ok()) {
        eprintln!("kith memory summary: only the owner may do that");
        return Ok(1);
    }
    if !thread_ok(thread) {
        eprintln!("kith memory summary: not a thread id");
        return Ok(2);
    }
    match cmd {
        SummaryCmd::Show { thread, json } => show(&thread, json),
        SummaryCmd::Forget { thread, json } => forget(&thread, json),
        SummaryCmd::Write { thread, json } => write(&thread, json),
    }
}

fn show(thread: &str, as_json: bool) -> Result<i32> {
    let mut out = std::io::stdout().lock();
    let (summary, forgotten) = match open(false)? {
        Some(conn) => (
            if has_table(&conn, "thread_summaries") { rules::get(&conn, thread)? } else { None },
            has_table(&conn, "thread_forgotten") && rules::forgotten(&conn, thread)?,
        ),
        None => (None, false),
    };
    if as_json {
        let s = summary.as_ref().map(|s| {
            json!({"text": s.text, "writtenAt": s.written_at, "audience": s.audience, "room": s.room,
                   "agent": s.agent, "reason": s.reason.as_str(), "coveredSeq": s.covered_seq})
        });
        writeln!(out, "{}", json!({"thread": thread, "summary": s, "forgotten": forgotten}))?;
        return Ok(0);
    }
    match summary {
        Some(s) => {
            writeln!(out, "{}", s.text)?;
            writeln!(out, "-- written {} for {} (audience: {})", s.written_at, s.agent, s.audience)?;
            Ok(0)
        }
        None if forgotten => {
            writeln!(out, "forgotten: {thread} (no summary is written for it)")?;
            Ok(1)
        }
        None => {
            writeln!(out, "no summary for {thread}")?;
            Ok(1)
        }
    }
}

fn forget(thread: &str, as_json: bool) -> Result<i32> {
    let Some(conn) = open(true)? else {
        eprintln!("kith memory summary forget: no Kith host store on this Mac");
        return Ok(1);
    };
    // The tables the host's migration makes (crates/memory/src/schema.rs): a host that never ran it has none yet.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS thread_summaries (thread_id TEXT PRIMARY KEY, room TEXT, agent TEXT NOT NULL,
            audience TEXT NOT NULL, projects TEXT NOT NULL DEFAULT '[]', text TEXT NOT NULL,
            links TEXT NOT NULL DEFAULT '[]', covered_seq INTEGER NOT NULL DEFAULT 0, reason TEXT NOT NULL DEFAULT 'end',
            written_at INTEGER NOT NULL DEFAULT 0, cost_via TEXT NOT NULL DEFAULT 'summary');
         CREATE TABLE IF NOT EXISTS thread_forgotten (thread_id TEXT PRIMARY KEY, at INTEGER NOT NULL);",
    )?;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    rules::forget(&conn, thread, at)?;
    // The row's copy goes too, so the apps stop showing it; the host also strips it when it next serves the row.
    if has_table(&conn, "entries") {
        conn.execute(
            "UPDATE entries SET data = json_remove(data, '$.thread.summary', '$.thread.summaryAudience')
             WHERE id = ?1 AND json_extract(data, '$.thread.summary') IS NOT NULL",
            [thread],
        )?;
    }
    let mut out = std::io::stdout().lock();
    if as_json {
        writeln!(out, "{}", json!({"forgotten": true, "thread": thread}))?;
    } else {
        writeln!(out, "forgotten: {thread} (its entries are kept; nothing about it is indexed again)")?;
    }
    Ok(0)
}

fn write(thread: &str, as_json: bool) -> Result<i32> {
    let exe = std::env::current_exe().context("find kith")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["summarise", "--thread", thread]);
    if as_json {
        cmd.arg("--json");
    }
    Ok(cmd.status().context("run kith summarise")?.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn the_owner_at_a_shell_or_in_a_one_to_one_run_may() {
        assert!(owner_only(env(&[])));
        assert!(owner_only(env(&[("KITH_AGENT", "echo"), ("KITH_ROOM", "1:1:echo")])));
    }

    #[test]
    fn a_guest_run_a_person_or_a_group_may_not() {
        assert!(!owner_only(env(&[("KITH_RUN_TOKEN", "t")])));
        assert!(!owner_only(env(&[("KITH_RUN_SOCKET", "/s")])));
        assert!(!owner_only(env(&[("KITH_PERSON", "ann")])));
        assert!(!owner_only(env(&[("KITH_ROOM", "group:x")])));
        assert!(!owner_only(env(&[("KITH_VIA", "slack")])));
    }

    #[test]
    fn thread_ids_are_plain() {
        assert!(thread_ok("main:echo") && thread_ok("0a1b-2c"));
        assert!(!thread_ok("") && !thread_ok("a b") && !thread_ok("../x"));
    }
}
