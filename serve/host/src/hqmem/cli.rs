//! `kith memory check | log | diff | undo | forget`: the commands over the agents' memory files. (`ls` and `search`
//! stay in the kernel's kith-memory, which hands these here.)

use super::hook::{self, Phase};
use super::{Layout, Target, Who, forget, guard, open, put, read, record_undo, sync, tidy};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use kith_mem::files::{self, Author, Cause, Version};
use kith_mem::rusqlite::Connection;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Run the write checks on text for a memory file: stdin, or --file. Exits 1 with the reason when it is refused.
    Check {
        /// The memory file the text is for.
        path: PathBuf,
        #[arg(long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        env: Env,
    },
    /// Every version of a memory file, newest first, with who made it and why.
    Log {
        file: PathBuf,
        #[arg(long)]
        json: bool,
        /// How many versions to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        env: Env,
    },
    /// What changed between two versions (default: the last change). Numbers are the ones `log` shows.
    Diff {
        file: PathBuf,
        v1: Option<usize>,
        v2: Option<usize>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        env: Env,
    },
    /// Put a memory file back to an earlier version (default: before the last change). The undo is a version too.
    Undo {
        file: PathBuf,
        version: Option<usize>,
        #[command(flatten)]
        env: Env,
    },
    /// Keep a fact out of memory: no agent or tidy writes it again.
    Forget {
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        #[command(flatten)]
        env: Env,
    },
    /// The tidy-ups of an agent's memory (what Kith changed between turns); `tidy undo` puts one back.
    #[command(args_conflicts_with_subcommands = true)]
    Tidy {
        /// The agent whose tidy-ups to list.
        agent: Option<String>,
        #[arg(long)]
        json: bool,
        /// How many tidy-ups to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        env: Env,
        #[command(subcommand)]
        undo: Option<TidySub>,
    },
    /// A thread's summary: `show`, `forget`, `write` (owner only).
    Summary {
        #[command(subcommand)]
        cmd: super::summary_cli::SummaryCmd,
        #[command(flatten)]
        folders: super::summary_cli::Folders,
    },
    /// Claude Code hook on `Write`, `Edit` and `MultiEdit` (stdin: the hook's JSON).
    #[command(hide = true)]
    Hook {
        phase: Phase,
        /// The agent whose run this is.
        #[arg(long)]
        agent: String,
        /// The write is the agent's tidying (`--dream` is the old name).
        #[arg(long, alias = "dream")]
        tidy: bool,
        #[command(flatten)]
        env: Env,
    },
}

/// Where the memory is; the kernel's commands pass what `lib/kith_paths.py` says, a terminal falls back to the env.
#[derive(Subcommand, Debug)]
pub enum TidySub {
    /// Put every file a tidy-up wrote back to what it was. A file changed since is left alone and named.
    Undo {
        agent: String,
        /// The id `kith memory tidy` shows.
        id: i64,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        env: Env,
    },
}

#[derive(clap::Args, Debug, Default)]
pub struct Env {
    #[arg(long, hide = true)]
    state: Option<PathBuf>,
    #[arg(long, hide = true)]
    agents: Option<PathBuf>,
}

impl Env {
    fn layout(&self) -> Layout {
        let found = Layout::from_env();
        let agents = self.agents.clone().unwrap_or_else(|| found.agents[0].clone());
        Layout::new(self.state.clone().unwrap_or(found.state), agents)
    }
}

/// Runs the command; returns the process exit code.
pub fn run(cmd: Cmd) -> Result<i32> {
    let mut out = std::io::stdout().lock();
    match cmd {
        Cmd::Summary { cmd, .. } => super::summary_cli::run(cmd),
        Cmd::Hook { phase, agent, tidy, env } => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let outcome = hook::run(phase, &input, &env.layout(), &agent, tidy);
            if !outcome.stdout.is_empty() {
                writeln!(out, "{}", outcome.stdout)?;
            }
            if !outcome.stderr.is_empty() {
                eprintln!("{}", outcome.stderr);
            }
            Ok(0)
        }
        Cmd::Check { path, file, env } => {
            let layout = env.layout();
            let text = if let Some(file) = file {
                std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?
            } else {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                text
            };
            let target = layout.locate(&path, &std::env::current_dir()?);
            // The history is an aid here: a check works without it, only the forgotten facts need it.
            let conn = open(&layout).ok();
            let old = target.as_ref().and_then(|t| read(&t.path));
            // A check asks what an agent's write would meet, so it is never the user's (unchecked) side.
            let who = match checker() {
                Who::Kai => Who::Agent("check"),
                agent => agent,
            };
            match guard(conn.as_ref(), target.as_ref(), who, old.as_deref(), &text) {
                Err(refusal) => {
                    eprintln!("Refused: {}", refusal.message());
                    Ok(1)
                }
                Ok(warnings) => {
                    for warning in &warnings {
                        writeln!(out, "warning: {warning}")?;
                    }
                    writeln!(out, "ok")?;
                    Ok(0)
                }
            }
        }
        Cmd::Log { file, json, limit, env } => {
            let (conn, target) = history(&env, &file)?;
            let list = ordered(&conn, &target, usize::MAX)?;
            let shown: Vec<_> = list.iter().rev().take(limit).collect();
            if json {
                let rows: Vec<_> = shown.iter().map(|(n, v)| entry(*n, v)).collect();
                writeln!(out, "{}", json!({"file": target.label(), "path": target.path, "versions": rows}))?;
            } else if shown.is_empty() {
                writeln!(out, "{}: no versions yet (the file doesn't exist).", target.label())?;
            } else {
                writeln!(out, "{}: {} versions", target.label(), list.len())?;
                for (n, v) in shown {
                    writeln!(out, "{n:>4}  {}  {:<22} {}", when(v.at), who_label(v), cause_label(v))?;
                }
            }
            Ok(0)
        }
        Cmd::Diff { file, v1, v2, json, env } => {
            let (conn, target) = history(&env, &file)?;
            let list = ordered(&conn, &target, usize::MAX)?;
            let count = list.len();
            let (from, to) = match (v1, v2) {
                (None, _) if count < 2 => bail!("{} has {count} version; there is nothing to compare", target.label()),
                (None, _) => (count - 1, count),
                (Some(a), None) => (a, count),
                (Some(a), Some(b)) => (a, b),
            };
            let text = |n: usize| -> Result<String> {
                let (_, v) = list.get(n.wrapping_sub(1)).with_context(|| format!("no version {n} ({count} so far)"))?;
                Ok(v.content.clone().unwrap_or_default())
            };
            let diff = files::diff(&text(from)?, &text(to)?);
            if json {
                writeln!(out, "{}", json!({"file": target.label(), "from": from, "to": to, "diff": diff}))?;
            } else {
                writeln!(out, "{} {from} -> {to}\n{diff}", target.label())?;
            }
            Ok(0)
        }
        Cmd::Undo { file, version, env } => {
            let (conn, target) = history(&env, &file)?;
            let list = ordered(&conn, &target, usize::MAX)?;
            let count = list.len();
            let n = match version {
                Some(n) => n,
                None if count >= 2 => count - 1,
                None => bail!("{} has {count} version; there is nothing to go back to", target.label()),
            };
            let (_, wanted) =
                list.get(n.wrapping_sub(1)).with_context(|| format!("no version {n} ({count} so far)"))?;
            let who = checker();
            if let Some(content) = &wanted.content {
                // An agent's undo brings back old text: the same checks apply as to any write it makes.
                if let Err(refusal) = guard(Some(&conn), Some(&target), who, read(&target.path).as_deref(), content) {
                    eprintln!("Refused: {}", refusal.message());
                    return Ok(1);
                }
            }
            put(&target.path, wanted.content.as_deref())?;
            let version = record_undo(&conn, &target, who)?;
            writeln!(
                out,
                "{}: back to version {n} (now version {})",
                target.label(),
                version.map_or(n, |_| count + 1)
            )?;
            Ok(0)
        }
        Cmd::Tidy { agent, json, limit, env, undo } => {
            let layout = env.layout();
            match (undo, agent) {
                (Some(TidySub::Undo { agent, id, json, env }), _) => {
                    let layout = env.layout();
                    let report = tidy::undo(&open(&layout)?, &layout, &agent, id)?;
                    if json {
                        let refused: Vec<_> =
                            report.refused.iter().map(|(f, why)| json!({"file": f, "reason": why})).collect();
                        let body = json!({"restored": report.restored, "refused": refused, "undone": report.undone});
                        writeln!(out, "{body}")?;
                    } else {
                        for file in &report.restored {
                            writeln!(out, "restored {file}")?;
                        }
                        for (file, why) in &report.refused {
                            writeln!(out, "left {file}: {why}")?;
                        }
                        writeln!(out, "{}", if report.undone { "Tidy-up undone." } else { "Not fully undone." })?;
                    }
                    Ok(0)
                }
                (None, Some(agent)) => {
                    let runs = tidy::runs(&open(&layout)?, &agent, limit)?;
                    if json {
                        writeln!(out, "{}", json!({"tidyUps": runs}))?;
                    } else if runs.is_empty() {
                        writeln!(out, "{agent}: no tidy-ups yet.")?;
                    } else {
                        for run in &runs {
                            let n = run["changes"].as_array().map_or(0, Vec::len);
                            let state = if run["undone"] == true { "  (undone)" } else { "" };
                            let at = when(run["at"].as_i64().unwrap_or(0));
                            writeln!(out, "{:>5}  {at}  {n} changes{state}", run["id"])?;
                        }
                    }
                    Ok(0)
                }
                (None, None) => bail!("which agent? kith memory tidy <agent>"),
            }
        }
        Cmd::Forget { text, env } => {
            let layout = env.layout();
            let conn = open(&layout)?;
            let text = text.join(" ");
            forget(&conn, &text)?;
            writeln!(out, "Forgotten: no agent or tidy will write that to memory again.")?;
            for (label, line) in lines_with(&layout, &text) {
                writeln!(out, "still written in {label}: {line}")?;
            }
            Ok(0)
        }
    }
}

/// An agent running this command (`KITH_AGENT`) is checked as itself; the user's terminal is not.
fn checker() -> Who<'static> {
    static AGENT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        std::env::var("KITH_AGENT").or_else(|_| std::env::var("HQ_AGENT")).ok().filter(|a| !a.is_empty())
    });
    match agent {
        Some(agent) => Who::Agent(agent),
        None => Who::Kai,
    }
}

/// The history connection and the file a path names, brought up to date with the disk.
fn history(env: &Env, path: &Path) -> Result<(Connection, Target)> {
    let layout = env.layout();
    let target = layout.locate(path, &std::env::current_dir()?).with_context(|| {
        format!(
            "{} is not a memory file (agents/<name>/memory/**, or user.md, people/, projects/ in the state folder)",
            path.display()
        )
    })?;
    let conn = open(&layout)?;
    sync(&conn, &target)?;
    Ok((conn, target))
}

/// The file's versions, oldest first, each with the number `log` shows.
fn ordered(conn: &Connection, target: &Target, limit: usize) -> Result<Vec<(usize, Version)>> {
    let mut all = files::versions(conn, target.bot.as_deref(), &target.name, limit)?;
    all.reverse();
    Ok(all.into_iter().enumerate().map(|(i, v)| (i + 1, v)).collect())
}

fn entry(n: usize, v: &Version) -> serde_json::Value {
    let (bytes, lines) = v.content.as_deref().map_or((0, 0), |c| (c.len(), c.lines().count()));
    json!({
        "version": n, "at": v.at, "author": who_label(v), "cause": cause_label(v),
        "deleted": v.content.is_none(), "bytes": bytes, "lines": lines,
    })
}

fn when(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map_or_else(|| "?".into(), |t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
}

fn who_label(v: &Version) -> String {
    match v.author {
        Author::Bot => v.author_bot.clone().unwrap_or_else(|| "agent".into()),
        Author::Tidy => format!("tidy ({})", v.author_bot.as_deref().unwrap_or("?")),
        Author::User => "kai".into(),
        Author::Computer => "unknown".into(),
    }
}

fn cause_label(v: &Version) -> &'static str {
    match (v.cause, v.author) {
        (Cause::Baseline, _) => "baseline (first seen)",
        (Cause::Undo, _) => "undo",
        (Cause::Restore, _) => "restore",
        (Cause::Tidy, _) => "tidy",
        (Cause::Delete, _) => "deleted",
        (Cause::Edit, Author::Bot) => "turn",
        (Cause::Edit, Author::User) => "hand edit",
        (Cause::Edit, _) => "changed outside the hook",
    }
}

/// Memory lines that hold the forgotten text, so the user can remove what is already written.
fn lines_with(layout: &Layout, text: &str) -> Vec<(String, String)> {
    let needle = text.trim().trim_end_matches(['.', '。']).to_lowercase();
    let mut found = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    for agents in &layout.agents {
        let Ok(dir) = std::fs::read_dir(agents) else { continue };
        roots.extend(dir.flatten().map(|e| e.path().join("memory")));
    }
    roots.extend(["user.md", "people", "projects"].map(|p| layout.state.join(p)));
    for root in roots {
        walk(&root, 0, &mut |path| {
            if path.extension().is_some_and(|e| e == "md")
                && let Some(target) = layout.locate(path, Path::new("/"))
                && let Some(content) = read(path)
            {
                for line in content.lines().filter(|l| l.to_lowercase().contains(&needle)) {
                    found.push((target.label(), line.trim().to_owned()));
                }
            }
        });
    }
    found
}

fn walk(path: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    if path.is_file() {
        visit(path);
    } else if depth < 8
        && let Ok(dir) = std::fs::read_dir(path)
    {
        for entry in dir.flatten() {
            let path = entry.path();
            // The role repos and snapshots under an agent's folder are not memory.
            if !entry.file_name().to_string_lossy().starts_with('.') && !path.is_symlink() {
                walk(&path, depth + 1, visit);
            }
        }
    }
}
