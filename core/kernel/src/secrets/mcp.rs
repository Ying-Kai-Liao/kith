//! `kith-auth mcp-config AGENT` in process (retire-python P1-15, design 2026-11-21-mcp-exec 2.2): the `--mcp-config` servers of
//! the shared services an agent is granted. It prints byte for byte what the Python printed except for how a server starts:
//! through this program (`<kith> kernel secrets exec --for A SVC -- CMD...`), or, for a program that does not list the
//! module `secrets-exec` (or with `KITH_SECRETS_EXEC=python`), through `<kernel>/bin/kith-auth exec ...` as before.
//!
//! Secrets are resolved when the server starts, inside `exec`, so they never land in a config file or in the agent's own
//! environment. Here they are only looked up once to leave out a server whose secrets don't resolve.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::authority::Authority;
use super::session::{Env, Session};
use super::{Ctx, Die, R, arr, die, get, is_owner, pyjson};

/// How a server's start is written into the config (decided once per launch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Starter {
    /// `<kernel>/bin/kith-auth exec ...`: the script, the rollback and what a program without `secrets-exec` gets.
    Script,
    /// `<program> kernel secrets exec ...`.
    Program(String),
}

/// The program that starts the servers: `kith_bin` when it lists `secrets-exec` and `KITH_SECRETS_EXEC` is not `python`
/// (the rollback, deleted with the script). `lists` asks a program for its modules.
pub fn exec_program(
    kith_bin: Option<&str>,
    secrets_exec: Option<&str>,
    lists: impl Fn(&Path) -> bool,
) -> Starter {
    match kith_bin
        .filter(|b| !b.is_empty() && secrets_exec != Some("python") && lists(Path::new(b)))
    {
        Some(bin) => Starter::Program(bin.to_owned()),
        None => Starter::Script,
    }
}

/// What `mcp-config` made: the config and the lines it said on stderr (a server it left out, and why).
#[derive(Debug)]
pub struct McpConfig {
    pub config: Value,
    pub notes: Vec<String>,
}

/// `mcp_wanted`: `[(service, declared)]`, what a run may start as MCP servers. The package's `needs:` (declared), then, for the
/// owner's own runs, whatever else the agent's own row grants (connected with `kith <agent> connect`). A guest's run gets the
/// agent's needs only: a connect is the owner's choice for the owner's runs.
fn wanted(s: &Session<'_>, agent: &str) -> Vec<(String, bool)> {
    let needs = s.needs(agent);
    let mut out: Vec<(String, bool)> = needs.iter().map(|n| (n.clone(), true)).collect();
    if is_owner(s.auth, &s.acting())
        && let Value::Object(row) = get(get(s.auth, "grants"), agent)
    {
        let mut names: Vec<&String> = row.keys().collect();
        names.sort();
        out.extend(
            names
                .into_iter()
                .filter(|n| !needs.contains(n) && service_name(n))
                .map(|n| (n.clone(), false)),
        );
    }
    out
}

/// `SERVICE_RE`: `^[a-z0-9][a-z0-9._-]{0,39}$`.
fn service_name(n: &str) -> bool {
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    n.chars().next().is_some_and(ok)
        && n.len() <= 40
        && n.chars().all(|c| ok(c) || matches!(c, '.' | '_' | '-'))
}

/// `mcp_spec`: the service's spec when it is an MCP server, else `None`. A declared service with no file is a refusal as
/// ever; a granted one whose file is gone (a connector the host removed) is skipped with a note, never a failure.
fn mcp_spec(
    s: &Session<'_>,
    agent: &str,
    svc: &str,
    declared: bool,
    notes: &mut Vec<String>,
) -> R<Option<Value>> {
    let spec = if declared {
        Some(s.service(svc, Some(agent))?)
    } else {
        let spec = s
            .service_file(svc, Some(agent))
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        if spec.is_none() {
            notes.push(format!(
                "kith: {agent}: {svc} is granted but its service file is gone; skipped (kith {agent} connectors)"
            ));
        }
        spec
    };
    Ok(spec.filter(|sp| get(sp, "kind").as_str() == Some("mcp")))
}

/// The command of a server: a bare `kith` is this kernel's own `bin/kith`, so the proxy starts without kith on PATH.
fn mcp_command(kernel: &Path, spec: &Value) -> R<String> {
    let Some(cmd) = get(get(spec, "mcp"), "command").as_str() else {
        return die("a service spec has no mcp.command".to_owned());
    };
    Ok(if cmd == "kith" {
        kernel
            .join("bin")
            .join("kith")
            .to_string_lossy()
            .into_owned()
    } else {
        cmd.to_owned()
    })
}

/// `cmd_mcp_config(agent)`. `env` is the environment of the run (`os.environ`); the person comes from its `KITH_PERSON`.
/// A refusal (no such service, a damaged file, the core down) is the `Die` a `kith-auth` command would end with.
pub fn mcp_config(c: &Ctx, env: &Env, agent: &str, starter: &Starter) -> R<McpConfig> {
    let tables = Authority::open(c)?.tables()?;
    let mut s = Session::new(c, &tables, env.clone());
    let mut notes = Vec::new();
    let mut servers = Map::new();
    for (svc, declared) in wanted(&s, agent) {
        let Some(spec) = mcp_spec(&s, agent, &svc, declared, &mut notes)? else {
            continue;
        };
        let (profile, why, refused) = s.granted(agent, &svc, None)?;
        let Some(profile) = profile else {
            notes.push(if refused {
                format!("kith: {agent}: {svc} left out this run: {why}")
            } else {
                format!(
                    "kith: {agent} needs {svc} but has no grant; running without it (kith-auth grant {agent} {svc} <profile>)"
                )
            });
            continue;
        };
        // skip a server whose secrets don't resolve instead of failing at start
        if let Err(d) = s.secrets(&svc, &profile, agent) {
            notes.push(line(&d));
            notes.push(format!(
                "kith: {agent}: {svc} skipped this run, its secrets don't resolve (kith-auth ls)"
            ));
            continue;
        }
        // also skip a browser tool that can't drive the agent's profile
        let mut server_env = match s.settings(agent, &svc) {
            Ok(e) => e,
            Err(d) => {
                notes.push(line(&d));
                notes.push(format!("kith: {agent}: {svc} skipped this run"));
                continue;
            }
        };
        if let Some(p) = s.env.get("KITH_PERSON").filter(|p| !p.is_empty()) {
            // the server's own exec decides for the same person
            server_env.insert("KITH_PERSON".to_owned(), p.clone().into());
        }
        let mcp = get(&spec, "mcp");
        let mut start = vec![Value::String(mcp_command(&c.kernel, &spec)?)];
        start.extend(arr(get(mcp, "args")).iter().map(|a| s.expand(a)));
        servers.insert(
            svc.clone(),
            server(c, starter, agent, &svc, &start, server_env),
        );
    }
    Ok(McpConfig {
        config: json!({ "mcpServers": servers }),
        notes,
    })
}

/// One server's entry: how it starts and its settings.
fn server(
    c: &Ctx,
    starter: &Starter,
    agent: &str,
    svc: &str,
    cmd: &[Value],
    env: Map<String, Value>,
) -> Value {
    let mut args: Vec<Value> = Vec::new();
    let command = match starter {
        Starter::Script => c
            .kernel
            .join("bin")
            .join("kith-auth")
            .to_string_lossy()
            .into_owned(),
        Starter::Program(bin) => {
            args.extend(["kernel", "secrets"].map(Value::from));
            bin.clone()
        }
    };
    args.extend(["exec", "--for", agent, svc, "--"].map(Value::from));
    args.extend(cmd.iter().cloned());
    let mut out = Map::new();
    out.insert("command".to_owned(), command.into());
    out.insert("args".to_owned(), args.into());
    out.insert("env".to_owned(), Value::Object(env));
    Value::Object(out)
}

/// A refusal as `kith-auth` says it on stderr.
fn line(d: &Die) -> String {
    format!("kith-auth: {}", d.msg)
}

/// `kith kernel secrets mcp-config AGENT`: the config on stdout (one line, as `json.dumps` writes it), the notes on stderr.
pub(super) fn run(c: &Ctx, args: &[String]) -> R<super::Outcome> {
    let [agent] = args else {
        return Err(Die {
            msg: "mcp-config <agent>: see kith-auth --help".to_owned(),
            code: 2,
        });
    };
    let env: Env = std::env::vars().collect();
    let made = mcp_config(c, &env, agent, &starter_of(&env))?;
    Ok(super::Outcome {
        stdout: pyjson::dumps(&made.config) + "\n",
        stderr: made.notes.iter().flat_map(|n| [n.as_str(), "\n"]).collect(),
        code: 0,
    })
}

/// The verb `mcp-config` names its own program: the one running (it lists `secrets-exec` by being this build), unless the
/// rollback is set.
fn starter_of(env: &Env) -> Starter {
    let program = std::env::current_exe()
        .ok()
        .map(|e| e.to_string_lossy().into_owned());
    exec_program(
        program.as_deref(),
        env.get("KITH_SECRETS_EXEC").map(String::as_str),
        |_| true,
    )
}
