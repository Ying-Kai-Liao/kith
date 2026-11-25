//! The launch's plan: every step of `kith AGENT --launch-json` up to the launch object, as typed fields. `emit_json`
//! (json.rs) turns a `Plan` into the JSON object; a run in this terminal can start from the same plan.
//!
//! Same steps, same order, same words on stderr and same exit codes as the bash entry (see assemble.rs for the list).

// A port of one long script: its short names and step-by-step shape are kept, so the two read side by side.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::format_collect,
    clippy::single_match_else,
    clippy::redundant_closure
)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::assemble::{
    AGENT_ACCESS_TIMEOUT, Env, Sys, assignments, call_model, claude_version, cost_core,
    cost_hook_bin, field, get, guest_mcp_arg, guest_tmp_ready, in_guest_sandbox, knows_module,
    knows_readfence, ledger_hook_bin, lists_module, parse_args, permit_hook_bin, pj_indent1,
    plain_number, pyrepr, pystr, random_hex, s, sync_env, threads_hook_bin, truthy,
    usable_kith_bin, which_in,
};
use super::{mcp, prompt, runenv, settings};
use crate::secrets::pyjson;
use crate::{accounts, engines, paths, secrets};

/// What the plan is for: the launch object the host asks for, or a run in this terminal.
#[derive(Clone, Debug, Default)]
pub enum Mode<'a> {
    /// `--launch-json`: no prompts, no state-picked account, no origin from the host.
    #[default]
    LaunchJson,
    /// A run that starts here (the terminal, a one-shot, `--print`).
    Exec(ExecOpts<'a>),
}

/// What `--resume` named, once its claim is taken.
#[derive(Debug, Default)]
pub struct Resumed {
    /// The Claude session id.
    pub sid: String,
    /// The folder the session ran in ("" for none).
    pub cwd: String,
    /// What the caller named (`--resume THREAD`), for the line that tells how to run it on another account.
    pub thread: String,
    /// What the session started with (the cost ledger's row): a built-in agent's thread keeps its engine and model.
    pub run: Option<accounts::SessionRun>,
}

/// The steps of a run here that sit between the plan's own: they take claims and start processes, which the plan does not.
/// The executor (`launch::exec`) supplies them, in the order of `kernel/bin/kith`.
pub trait ExecHooks {
    /// `--resume`, `-r`, `-c`: names the session and takes its writer claim for this process (`resolve`), after who the run is
    /// for and before the account, the limit and the prompt. The error is the exit code, said already.
    fn resume(&self, agent: &str) -> Result<Resumed, i32>;
    /// The owner's one-shot that no host is picking the account for (another run's `kith ask`, a schedule) walks the accounts
    /// that can run: each attempt is a whole run of its own. The code is the last attempt's.
    fn walk(&self, agent: &str, agent_md: &Path, argv: &[String]) -> i32;
}

/// The hooks of an exec plan, or none (the plans of `--plan-exec` and the tests take no claims).
#[derive(Clone, Copy, Default)]
pub struct Hooks<'a>(pub Option<&'a dyn ExecHooks>);

impl std::fmt::Debug for Hooks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Hooks(..)"
        } else {
            "Hooks(none)"
        })
    }
}

/// What `kith AGENT [args]` knows about its own run that the flags do not say.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ExecOpts<'a> {
    /// What the run takes claims and starts processes with (none for a plan alone).
    pub hooks: Hooks<'a>,
    /// A task was given (`$# -gt 0` after the flags): a one-shot, not a terminal.
    pub task: bool,
    /// `--held`: the session is held by a holder, which has a tty of its own.
    pub held: bool,
    /// Stdin and stdout are ttys.
    pub tty: bool,
    /// Stdin and stderr are ttys: the limit prompt can be asked and answered.
    pub prompt_tty: bool,
    /// The Claude session id `--resume`/`-r`/`-c` named ("" for none).
    pub resume_sid: String,
    /// The thread the session was found by, for the line that tells how to run it on another account.
    pub resume_thread: String,
}

impl ExecOpts<'_> {
    /// The options of this process: the ttys are the real ones.
    pub fn from_process(task: bool, held: bool, resume_sid: &str) -> Self {
        let (i, o, e) = (
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
        );
        Self {
            task,
            held,
            tty: i && o,
            prompt_tty: i && e,
            resume_sid: resume_sid.to_owned(),
            resume_thread: String::new(),
            hooks: Hooks::default(),
        }
    }
}

/// The owner at their own terminal (the same test `kith` makes twice): no task, a tty on both ends or a held session, not
/// started by another run, and not by the host, a schedule or an ask (they set `KITH_VIA`).
pub fn owner_terminal(x: &ExecOpts, print: bool, owner: bool, nested: bool, via: &str) -> bool {
    !print && !x.task && owner && !nested && via == "terminal" && (x.tty || x.held)
}

/// What a launch decided, ready to be written as the launch object or to start a run from.
#[allow(clippy::struct_excessive_bools)]
pub struct Plan {
    pub agent: String,
    /// The `--backup` account the run was asked for ("" for none).
    pub backup: String,
    pub harness: String,
    pub claude: bool,
    /// The run is the owner's (not a person's or a bot guest's).
    pub owner: bool,
    /// The names the person's membership put in the environment, space separated.
    pub person_env_names: String,
    /// The account's config-dir variable ("" for none).
    pub cfg_var: String,
    /// The names of the paths the launch exports.
    pub path_names: Vec<String>,
    /// The environment the run starts with, after `sync_env`.
    pub env: Env,
    pub system_prompt: String,
    pub out: settings::Output,
    /// The settings object (a guest's grants applied), null for an engine that is not Claude Code.
    pub settings: Option<Value>,
    pub mcp_servers: Value,
    pub mcp_arg: String,
    pub model: String,
    pub effort: String,
    pub call_model: String,
    pub physical_cwd: String,
    /// The engine's launch spec when the harness is neither claude nor codex.
    pub spec: Option<Value>,
    /// The launch object was asked for (not a run here).
    pub launch: bool,
    /// The owner's interactive run in this terminal.
    pub owner_terminal: bool,
    /// The pinned package folder (its `mcp.json` is the terminal's own MCP config).
    pub pkg: PathBuf,
    /// The agent's own folder (`pkg` is a snapshot of it, or it).
    pub src_pkg: PathBuf,
    /// Who the run is for, as kith-auth named them ("" for the owner): the claims are theirs.
    pub person_id: String,
    /// Another run started this one (`KITH_AGENT` was set).
    pub nested: bool,
    /// The caller's agent name ("" for none or itself).
    pub caller: String,
    /// The Claude session the run continues (`--resume`, or the caller's `KITH_RESUME`), "" for a new one.
    pub resume: String,
    /// The caller's `KITH_OUTPUT` (`json` asks for `--output-format json`).
    pub out_format: String,
    /// `{"mcpServers": {"kithpermit": ...}}`, inline: how a run reaches the permit tools (a guest's only server).
    pub permit_mcp: String,
    /// The shared services the owner is granted, as `{"mcpServers": {...}}` (the file a run here hands Claude Code).
    pub granted: Value,
}

/// `kith AGENT [args]` up to the launch object: the plan on success, or the exit code (what was said is already on stderr).
///
/// A run that should not start (the owner said no to the limit prompt) is `Err(0)`.
pub fn plan(
    agent: &str,
    argv: &[String],
    kernel: &Path,
    env0: Env,
    mode: &Mode<'_>,
) -> Result<Plan, i32> {
    let exec = match mode {
        Mode::LaunchJson => None,
        Mode::Exec(x) => Some(x),
    };
    let a = parse_args(argv);
    let mut env = env0;
    sync_env(&mut env);
    let kernel_s = s(kernel);
    env.insert("KITH_ROOT".to_owned(), kernel_s.clone());
    let home_dir = PathBuf::from(get(&env, "HOME"));
    let p = {
        let e = env.clone();
        paths::paths(&home_dir, kernel, &move |k| e.get(k).cloned())
    };
    for (k, v) in p.entries() {
        env.insert(k, s(v));
    }
    let say = |m: &str| eprintln!("{m}");
    let Some(src_pkg) = prompt::agent_dir(&p, kernel, agent) else {
        say(&format!("kith: no agent named \"{agent}\"."));
        return Err(2);
    };
    let home = if src_pkg.file_name().is_some_and(|n| n == "role") {
        src_pkg.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        p.state.join("agents").join(agent)
    };
    let nested = !get(&env, "KITH_AGENT").is_empty();
    let caller = {
        let c = get(&env, "KITH_AGENT");
        if c == agent { "" } else { c }.to_owned()
    };

    // ---- who the run is for
    let mut p_owner = true;
    let mut p_model = String::new();
    let (mut p_line, mut p_id, mut p_was, mut p_rooms) =
        (String::new(), String::new(), String::new(), String::new());
    let mut p_env_names = String::new();
    let mut member = settings::Member::owner();
    let person = get(&env, "KITH_PERSON").to_owned();
    if !person.is_empty() {
        let mut sctx = secrets::Ctx::from_process(kernel);
        sctx.person = Some(person.clone());
        let hold: u64 = get(&env, "KITH_SYNC_HOLD")
            .parse()
            .ok()
            .filter(|_| {
                get(&env, "KITH_SYNC_HOLD")
                    .chars()
                    .all(|c| c.is_ascii_digit())
            })
            .unwrap_or(120);
        let started = std::time::Instant::now();
        // A room person (the turns of a group room with people) has no row in kith-auth: its answer is the meet of the
        // room's people (`KITH_ROOM_MEMBERS`), holding the cap of whoever asked (`KITH_ROOM_AUTHOR`), and it is the only
        // way such a person launches. Anything that does not resolve is no launch.
        let room_run = super::room::is_room_person(&person);
        let spend_who = if room_run {
            get(&env, "KITH_ROOM_AUTHOR").trim().to_owned()
        } else {
            person.clone()
        };
        let r: Value = if room_run {
            let answer = super::room::launch_answer(
                &sctx,
                agent,
                &person,
                get(&env, "KITH_ROOM"),
                get(&env, "KITH_ROOM_MEMBERS"),
                &spend_who,
            );
            match answer {
                Ok(r) => r,
                Err(refusal) => {
                    say(&format!("kith: {refusal}"));
                    return Err(3);
                }
            }
        } else {
            loop {
                let mut args = vec![agent.to_owned()];
                if !caller.is_empty() {
                    args.extend(["--via".to_owned(), caller.clone(), "--take-once".to_owned()]);
                }
                let o = secrets::run(&sctx, "resolve", &args);
                eprint!("{}", o.stderr);
                let parsed = (o.code == 0)
                    .then(|| serde_json::from_str::<Value>(&o.stdout).ok())
                    .flatten();
                let Some(r) = parsed else {
                    say(&format!(
                        "kith: can't check whether {person} may use {agent} (kith-auth resolve failed)"
                    ));
                    return Err(1);
                };
                if !r.get("held").is_some_and(truthy) {
                    break r;
                }
                if started.elapsed().as_secs() >= hold {
                    say(&format!(
                        "kith: out of sync with the Kith cloud, so {person} can't use {agent} on this computer yet; try again in a minute"
                    ));
                    return Err(3);
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        };
        // What the run costs is written under whoever asked: the cap above is theirs, and so is the ledger it reads.
        if room_run {
            env.insert(
                "KITH_COST_PERSON".to_owned(),
                if spend_who.is_empty() {
                    "owner".to_owned()
                } else {
                    spend_who.clone()
                },
            );
        }
        let txt = |k: &str| r.get(k).map(pystr).unwrap_or_default();
        p_owner = r.get("owner").is_some_and(truthy);
        p_id = txt("person");
        p_model = txt("model");
        let clean = |v: &str| -> String {
            if p_owner {
                v.replace('"', "")
            } else {
                v.chars()
                    .filter(|c| !matches!(c, '<' | '>' | '"' | '\u{0}'..='\u{1f}' | '\u{7f}'))
                    .collect()
            }
        };
        let attrs: Vec<String> = ["person", "name", "lang", "tz"]
            .iter()
            .filter_map(|k| {
                r.get(*k)
                    .filter(|v| truthy(v))
                    .map(|v| format!("{k}=\"{}\"", clean(&pystr(v))))
            })
            .collect();
        p_line = format!(" {}", attrs.join(" "));
        p_was = r
            .get("was")
            .and_then(Value::as_array)
            .map(|w| w.iter().map(pystr).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        p_rooms = r
            .get("rooms")
            .and_then(Value::as_array)
            .map(|w| {
                w.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let mut names = vec![];
        if let Some(Value::Object(e)) = r.get("env") {
            for (k, v) in e {
                let plain = !k.is_empty()
                    && k.chars().enumerate().all(|(i, c)| {
                        c == '_' || c.is_ascii_uppercase() || (i > 0 && c.is_ascii_digit())
                    });
                if !p_owner
                    && (!plain
                        || ["PATH", "HOME", "SHELL"].contains(&k.as_str())
                        || ["DYLD_", "LD_", "KITH_", "HQ_"]
                            .iter()
                            .any(|x| k.starts_with(x)))
                {
                    say(&format!(
                        "kith: skipping membership env {} (not allowed)",
                        pyrepr(k)
                    ));
                    continue;
                }
                env.insert(k.clone(), pystr(v));
                names.push(k.clone());
            }
        }
        p_env_names = names.join(" ");
        if !r.get("allowed").is_some_and(truthy) {
            say(&format!(
                "kith: {} (kith-auth member {agent} {person})",
                txt("why")
            ));
            return Err(3);
        }
        member = settings::Member::from_resolve(&r);
        let p_budget = txt("budget");
        let p_budget_agent = r
            .get("budget_agent")
            .filter(|v| truthy(v))
            .map(pystr)
            .unwrap_or_default();
        if !p_budget.is_empty() {
            // A limit that can't be read is not "no limit": for anyone but the owner the run stops here.
            if !p_owner && !plain_number(&p_budget) {
                let on = if p_budget_agent.is_empty() {
                    agent
                } else {
                    &p_budget_agent
                };
                say(&format!(
                    "kith: your spending limit on {on} can't be read; ask the owner to set it again"
                ));
                return Err(3);
            }
            // The core's own `spent`, in process (there is no program to ask and no Python to go back to).
            let spent = cost_core(
                kernel,
                &p,
                &env,
                &home_dir,
                &p_was,
                &["spent", &spend_who, &p_budget_agent],
            );
            let spent = match spent {
                None if p_owner => "0".to_owned(),
                None => {
                    say(&format!(
                        "kith: can't read what {person} has spent this month (kith-cost failed); not running without the budget check"
                    ));
                    return Err(3);
                }
                Some(t) => {
                    if !p_owner && !plain_number(&t) {
                        say(&format!(
                            "kith: can't read what {person} has spent this month (kith-cost gave '{t}'); not running without the budget check"
                        ));
                        return Err(3);
                    }
                    t
                }
            };
            let f = |x: &str| x.trim().parse::<f64>().ok();
            if let (Some(sp), Some(bu)) = (f(&spent), f(&p_budget))
                && sp >= bu
            {
                if p_budget_agent.is_empty() {
                    say(&format!(
                        "kith: your bots have used ${sp:.2} of their ${p_budget} this month together ({person} was stopped); the owner can raise it (kith auth bots-cap ..)"
                    ));
                } else {
                    say(&format!(
                        "kith: {person} has used ${sp:.2} of their ${p_budget} this month on {p_budget_agent}; the owner can raise it (kith auth member {p_budget_agent} {person} budget=..)"
                    ));
                }
                if !p_owner {
                    return Err(3);
                }
            }
        }
    }
    if p_owner && in_guest_sandbox() {
        say("kith: this is inside a guest's run, which can't start a run as the owner");
        return Err(3);
    }
    for d in ["projects", "sessions", "shared"] {
        let _ = fs::create_dir_all(home.join("memory").join(d));
    }
    env.remove("KITH_WORK");
    let (mut files, mut mem) = (home.join("files"), home.join("memory"));
    let mut ctmp = String::new();
    let mut ctmp_note = String::new();
    if !p_owner {
        let plain = p_id.starts_with(|c: char| c.is_ascii_lowercase())
            && p_id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !plain {
            say(&format!("kith: bad person id '{p_id}' from kith-auth"));
            return Err(3);
        }
        mem = home.join("memory/people").join(&p_id);
        files = home.join("files/people").join(&p_id);
        for d in [
            mem.join("projects"),
            mem.join("sessions"),
            files.clone(),
            p.state.join("people"),
        ] {
            let _ = fs::create_dir_all(d);
        }
        let w = files.join(".work");
        let _ = fs::create_dir_all(w.join("tmp"));
        let _ = fs::create_dir_all(w.join("cache/uv"));
        env.insert("KITH_WORK".to_owned(), s(&w));
        env.insert(
            "CLAUDE_ENV_FILE".to_owned(),
            format!("{kernel_s}/lib/guest-env.sh"),
        );
        let uid: u32 = Sys { env: &env }
            .out("id", &["-u"], false)
            .and_then(|u| u.parse().ok())
            .unwrap_or(0);
        if let Some(d) = guest_tmp_ready(&p, &p_id, uid) {
            env.insert("CLAUDE_CODE_TMPDIR".to_owned(), d.clone());
            ctmp = d;
        } else {
            ctmp_note = prompt::ctmp_note(&p.machine, &p_id);
            say(&format!("kith: {ctmp_note}"));
        }
    }
    let _ = (&mem, &files);

    // ---- the pinned package
    let mut pkg = src_pkg.clone();
    // The core's own `resolve`, in process. The Python `kith-pkg resolve` is no longer a fallback (retire-python R2).
    let resolved = {
        let ctx = crate::pkg::Ctx {
            kernel: kernel.to_path_buf(),
            agents: p.agents.clone(),
            state: p.state.clone(),
            store: p.store.clone(),
            github: env
                .get("KITH_GITHUB_GIT")
                .map_or("https://github.com", String::as_str)
                .trim_end_matches('/')
                .to_owned(),
        };
        let out = crate::pkg::cli::run(&ctx, &["resolve".to_owned(), agent.to_owned()]);
        eprint!("{}", out.stderr);
        (out.code == 0).then(|| out.stdout.trim_end_matches('\n').to_owned())
    };
    match resolved
        .map(PathBuf::from)
        .filter(|d| d.join("prompt.md").is_file())
    {
        Some(d) => pkg = d,
        None => say(&format!(
            "kith: {agent}: package snapshot failed; running the live folder"
        )),
    }
    let pkg_s = s(&pkg);
    let version = if pkg == src_pkg {
        "live".to_owned()
    } else {
        pkg_s.rsplit('@').next().unwrap_or_default().to_owned()
    };
    for (k, v) in [
        ("KITH_AGENT", agent.to_owned()),
        ("KITH_AGENT_DIR", pkg_s.clone()),
        ("KITH_AGENT_HOME", s(&home)),
        ("KITH_AGENT_VERSION", version.clone()),
    ] {
        env.insert(k.to_owned(), v);
    }

    // ---- the flags
    if let Some((code, msg)) = &a.error {
        say(msg);
        return Err(*code);
    }
    // The thread a terminal resumes is `ExecOpts::resume_sid`; the flag itself only says a run is asked for.
    if a.resume && (exec.is_none() || a.print) {
        say("kith: --resume is for a run in this terminal, not --launch-json or --print");
        return Err(1);
    }
    if a.resume && !p_owner {
        say("kith: --resume is for the owner's own sessions");
        return Err(1);
    }
    // --resume takes the session's claim here and goes to the folder it ran in (before the account, the limit and the prompt, as
    // the entry does); a built-in agent's thread keeps the engine and model it started with unless -H/-m say otherwise.
    let hooks = exec.and_then(|x| x.hooks.0);
    let mut resumed: Option<Resumed> = None;
    if a.resume
        && let Some(h) = hooks
    {
        let r = h.resume(agent)?;
        if !r.cwd.is_empty() {
            std::env::set_current_dir(&r.cwd).map_err(|_| 1)?;
        }
        if src_pkg.starts_with(kernel.join("agents"))
            && let Some(s) = &r.run
        {
            if a.harness.is_none() && get(&env, "KITH_HARNESS").is_empty() {
                env.insert("KITH_HARNESS".to_owned(), s.harness.clone());
            }
            let model_ok = a.model.is_none() && get(&env, "KITH_MODEL").is_empty();
            if model_ok
                && let Some(m) = &s.model
                && a.harness.as_deref().unwrap_or(get(&env, "KITH_HARNESS")) == s.harness
            {
                env.insert("KITH_MODEL".to_owned(), m.clone());
            }
            if a.effort.is_none()
                && get(&env, "KITH_EFFORT").is_empty()
                && let Some(e) = &s.effort
            {
                env.insert("KITH_EFFORT".to_owned(), e.clone());
            }
        }
        resumed = Some(r);
    }
    // The owner's one-shot of another run or a schedule walks the accounts that can run, each attempt a run of its own.
    if let Some(h) = hooks
        && a.task
        && a.backup.is_empty()
        && !a.print
        && !a.resume
        && get(&env, "KITH_CHAIN_CHILD").is_empty()
        && p_owner
        && (nested || !get(&env, "KITH_BACKUP_CHAIN").is_empty())
    {
        return Err(h.walk(agent, &src_pkg.join("agent.md"), argv));
    }
    let mut kith_model = a
        .model
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_MODEL").to_owned());
    let mut kith_effort = a
        .effort
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_EFFORT").to_owned());
    let kith_harness = a
        .harness
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_HARNESS").to_owned());
    let mut perm_mode = a
        .perm_mode
        .clone()
        .unwrap_or_else(|| get(&env, "KITH_PERMISSION_MODE").to_owned());

    // ---- the agent's monthly limit
    let limit_line = if p_owner {
        cost_core(kernel, &p, &env, &home_dir, "", &["limit", agent, "--sh"])
    } else {
        None
    };
    if let Some(lim) = limit_line.filter(|l| !l.is_empty()) {
        let v = assignments(&lim);
        if v.get("l_over").map(String::as_str) == Some("1") {
            let at = v.get("l_at").map_or("ask", String::as_str);
            let text = v.get("l_text").cloned().unwrap_or_default();
            match at {
                "hold" => {
                    say(&format!(
                        "kith: {text}. Its runs are held until you raise the limit (kith {agent} set limit N) or the month turns; kith {agent} set at-limit ask lets them ask instead."
                    ));
                    return Err(3);
                }
                "ask" => {
                    // A terminal run asks here; a one-shot or the host's launch says so and goes on.
                    if exec.is_some_and(|x| !a.print && !x.task && x.prompt_tty) {
                        eprint!("kith: {text}. Run anyway? [Y/n] ");
                        let _ = std::io::stderr().flush();
                        let mut l = String::new();
                        let got = std::io::stdin().lock().read_line(&mut l).unwrap_or(0);
                        // End of input answers yes, as `read -r ... || l_yes=y` does.
                        if got > 0 && l.starts_with(['n', 'N']) {
                            say(&format!(
                                "kith: not run. Raise it with kith {agent} set limit N."
                            ));
                            return Err(0);
                        }
                    } else {
                        say(&format!(
                            "kith: {text}; running anyway (your runs never stop at a limit; kith {agent} set limit N raises it)."
                        ));
                    }
                }
                _ => say(&format!("kith: {text}; running anyway.")),
            }
        }
    }

    // ---- the project
    let reg = prompt::Registry::load(&p.state).map_err(|e| {
        say(&e);
        1
    })?;
    let mut project = a.project.clone();
    if project.is_empty() {
        // The core answers, in process. Never as a guest (`guest: None`): the launch's own lookup is the kernel's, whoever the run
        // is for; a guest's own `project-id` calls go through the stub to `kith kernel project-id`, which asks the host.
        let ctx = crate::project::Ctx {
            state: p.state.clone(),
            home: home_dir.clone(),
            cwd: std::env::current_dir().unwrap_or_default(),
            guest: None,
        };
        let out = crate::project::run(&ctx, &[]);
        project = if out.code == 0 {
            out.stdout.trim_end_matches('\n').to_owned()
        } else {
            String::new()
        };
    } else if !reg.has(&project) {
        let known: String = reg.known().iter().map(|k| format!("{k} ")).collect();
        say(&format!(
            "kith: unknown project '{project}'. Known: {known}"
        ));
        say(&format!(
            "    register one with: {kernel_s}/bin/project-id --register {project} [dir]"
        ));
        return Err(1);
    }

    // ---- the prompt
    let cwd_logical = {
        let phys = std::env::current_dir().unwrap_or_default();
        match env.get("PWD").filter(|d| !d.is_empty()) {
            Some(d) if fs::canonicalize(d).ok() == fs::canonicalize(&phys).ok() => d.clone(),
            _ => s(&phys),
        }
    };
    let date = Sys { env: &env }
        .out("date", &["+%F"], false)
        .unwrap_or_default();
    let who = prompt::Who {
        owner: p_owner,
        id: p_id.clone(),
        line: p_line.clone(),
        rooms: p_rooms,
    };
    let envf = {
        let e = env.clone();
        move |k: &str| e.get(k).cloned()
    };
    let text = prompt::build(&prompt::Input {
        agent,
        paths: &p,
        kernel,
        src_pkg: &src_pkg,
        pkg: &pkg,
        who: &who,
        project: &project,
        registry: &reg,
        cwd: &cwd_logical,
        date: &date,
        ctmp_note: &ctmp_note,
        user_home: &home_dir,
        env: &envf,
    })
    .map_err(|e| {
        say(&e);
        1
    })?;

    // ---- run files: the snapshot is held by --hold's pid
    if pkg != src_pkg {
        // A run here is held by its own pid (the marker named for the shell in the entry); a launch for a program by --hold's.
        let (hold, marker) = if exec.is_some() {
            let pid = std::process::id().to_string();
            (pid.clone(), pid)
        } else {
            let hold = a.hold.clone().unwrap_or_else(parent_pid);
            let marker = format!("{hold}.launch");
            (hold, marker)
        };
        if hold.is_empty() || !hold.chars().all(|c| c.is_ascii_digit()) {
            say("kith: --hold needs a process id");
            return Err(1);
        }
        let _ = fs::create_dir_all(pkg.join(".runs"));
        let _ = fs::write(pkg.join(".runs").join(marker), "");
    }

    // ---- harness and login
    let agent_md = pkg.join("agent.md");
    let norm = |h: &str| {
        if h == "claude-code" {
            "claude".to_owned()
        } else {
            h.to_owned()
        }
    };
    let own = {
        let o = field(&agent_md, "harness");
        norm(if o.is_empty() { "claude" } else { &o })
    };
    let mut harness = norm(if kith_harness.is_empty() {
        &own
    } else {
        &kith_harness
    });
    if matches!(
        get(&env, "KITH_ACCOUNT_VAR"),
        "CLAUDE_CONFIG_DIR" | "CODEX_HOME"
    ) {
        let v = get(&env, "KITH_ACCOUNT_VAR").to_owned();
        env.remove(&v);
    }
    env.remove("KITH_ACCOUNT_VAR");
    env.remove("HQ_ACCOUNT_VAR");
    let mut account_id = harness.clone();
    let mut acct = String::new();
    let system = accounts::System {
        kernel: kernel.to_path_buf(),
    };
    let actx = accounts::Ctx {
        machine: p.machine.clone(),
        agents: p.agents.clone(),
        kernel: kernel.to_path_buf(),
        home: PathBuf::from(get(&env, "HOME")),
        claude_config_dir: env.get("CLAUDE_CONFIG_DIR").cloned(),
        out: &system,
    };
    // Every launch notes last_started for its account (not --print, which starts nothing).
    let record = !a.print;
    let mut be: HashMap<String, String> = HashMap::new();
    let use_account = |id: &str, be: &mut HashMap<String, String>| -> Result<(), i32> {
        match accounts::launch_env(&actx, agent, id, false, record) {
            Ok(t) => {
                *be = assignments(&t);
                Ok(())
            }
            Err(e) => {
                say(&format!("kith: {e}"));
                Err(4)
            }
        }
    };
    let bget = |be: &HashMap<String, String>, k: &str| be.get(k).cloned().unwrap_or_default();
    // The account a session was made under (the cost ledger's): --resume runs under it, a handed-on session (KITH_RESUME) is
    // continued on it when it is a ready candidate.
    let r_sid = resumed
        .as_ref()
        .map(|r| r.sid.as_str())
        .or_else(|| exec.map(|x| x.resume_sid.as_str()))
        .unwrap_or_default();
    let made = {
        let sid = if r_sid.is_empty() {
            get(&env, "KITH_RESUME")
        } else {
            r_sid
        };
        if sid.is_empty() {
            String::new()
        } else {
            accounts::session_account(&p.machine, sid).unwrap_or_default()
        }
    };
    if !a.backup.is_empty() {
        use_account(&a.backup, &mut be)?;
        harness = bget(&be, "backup_harness");
        acct = bget(&be, "backup_account");
        account_id = a.backup.split('@').next().unwrap_or_default().to_owned();
    } else if !r_sid.is_empty() && !made.is_empty() {
        // Straight to the session's account: no pick (it would note a start on an account this run never uses).
        let main_now = accounts::main_id(&actx, agent, Some(&agent_md), None);
        if made != main_now {
            say(&format!(
                "kith: that session was made under {made}, so it runs under {made} (main account for {agent}: {main_now})"
            ));
        }
        use_account(&made, &mut be)?;
        harness = bget(&be, "backup_harness");
        acct = bget(&be, "backup_account");
        account_id = made.split('@').next().unwrap_or_default().to_owned();
    } else if harness == own {
        // A run in a terminal (owner's) starts on the first account that is not out, and on the session's own when it is one
        // of them; a launch for a program, or a guest's run, on the static main (the host picks per turn itself).
        let state = exec.is_some() && p_owner;
        let opts = accounts::MainOpts {
            state,
            record,
            prefer: (state && !made.is_empty()).then_some(made.as_str()),
        };
        match accounts::main_env(&actx, agent, &agent_md, opts) {
            Ok(t) => be = assignments(&t),
            Err(e) => {
                say(&format!("kith: {e}"));
                return Err(4);
            }
        }
        acct = bget(&be, "backup_account");
        account_id = bget(&be, "main_id");
        // The pick can be an account of another harness: it runs there, as --account would.
        let on = bget(&be, "backup_harness");
        if !on.is_empty() && on != harness {
            harness = on;
            kith_model.clear();
            env.remove("HQ_MODEL");
        }
    }
    let mut resume = get(&env, "KITH_RESUME").to_owned();
    if !made.is_empty() && made != account_id && (!r_sid.is_empty() || !resume.is_empty()) {
        if !r_sid.is_empty() && a.backup.is_empty() {
            say(&format!(
                "kith: that session was made under {made}, so it runs under {made} (main account for {agent}: {account_id})"
            ));
            use_account(&made, &mut be)?;
            harness = bget(&be, "backup_harness");
            acct = bget(&be, "backup_account");
            account_id = made.split('@').next().unwrap_or_default().to_owned();
        } else if !r_sid.is_empty() {
            let thread = resumed
                .as_ref()
                .map(|r| r.thread.as_str())
                .or_else(|| exec.map(|x| x.resume_thread.as_str()))
                .unwrap_or_default();
            say(&format!(
                "kith: that session was made under {made}, not {}: kith {agent} --resume {thread} --account {made}",
                a.backup
            ));
            return Err(4);
        } else {
            say(&format!(
                "kith: session {resume} was made under {made}, not {account_id}; starting a new one"
            ));
            resume.clear();
            env.remove("KITH_RESUME");
            env.remove("HQ_RESUME");
        }
    }
    if !a.backup.is_empty() && harness != own {
        kith_model.clear();
        env.remove("HQ_MODEL");
    }
    if !a.backup.is_empty() && !bget(&be, "backup_model").is_empty() {
        kith_model = bget(&be, "backup_model");
        env.remove("HQ_MODEL");
    }
    let cfg_var = bget(&be, "backup_cfg_var");
    if !cfg_var.is_empty() {
        env.insert(cfg_var.clone(), bget(&be, "backup_cfg_dir"));
        env.insert("KITH_ACCOUNT_VAR".to_owned(), cfg_var.clone());
    }
    env.insert("KITH_PROVIDER".to_owned(), harness.clone());
    env.insert("KITH_ACCOUNT".to_owned(), acct);
    env.insert("KITH_ACCOUNT_ID".to_owned(), account_id);
    if harness == own && kith_model.is_empty() {
        kith_model = field(&agent_md, "model");
    }
    if !p_model.is_empty() {
        kith_model.clone_from(&p_model);
    }
    if harness != "claude" {
        let m = kith_model.as_str();
        if ["opus", "sonnet", "haiku", "fable", "claude-"]
            .iter()
            .any(|x| m.starts_with(x))
            || m == "best"
            || m == "opusplan"
        {
            say(&format!(
                "kith: {m} is a Claude model; {agent} runs on {harness}'s default model (kith {agent} set harness {harness} --model M picks one)"
            ));
            kith_model.clear();
            env.remove("HQ_MODEL");
        }
    }
    if kith_effort.is_empty() {
        kith_effort = field(&agent_md, "effort");
    }
    env.insert("KITH_MODEL".to_owned(), kith_model.clone());
    env.insert("KITH_EFFORT".to_owned(), kith_effort.clone());
    env.insert("KITH_PROJECT".to_owned(), project.clone());
    let out_format = get(&env, "KITH_OUTPUT").to_owned();
    for k in [
        "KITH_HARNESS",
        "KITH_OUTPUT",
        "KITH_RESUME",
        "KITH_PERMISSION_MODE",
        "HQ_HARNESS",
        "HQ_OUTPUT",
        "HQ_RESUME",
        "HQ_PERMISSION_MODE",
    ] {
        env.remove(k);
    }
    // --resume THREAD wins over a caller's KITH_RESUME, and only Claude sessions can be taken up.
    if !r_sid.is_empty() {
        if harness != "claude" {
            say(&format!(
                "kith: --resume continues Claude sessions; {agent} runs on {harness}"
            ));
            return Err(1);
        }
        resume = r_sid.to_owned();
    }
    // a guest continues only a session the cost ledger has as theirs
    if !p_owner && !resume.is_empty() && !resume.contains(':') {
        let ok = (|| -> Option<bool> {
            let db = p.machine.join("cost.db");
            if !db.exists() {
                return None;
            }
            let c = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            let mut ids = vec![p_id.clone()];
            ids.extend(p_was.split(',').map(str::to_owned));
            ids.retain(|x| !x.is_empty());
            let sql = format!(
                "select 1 from runs where session = ? and agent = ? and person in ({})",
                vec!["?"; ids.len()].join(",")
            );
            let mut params: Vec<String> = vec![resume.clone(), agent.to_owned()];
            params.extend(ids);
            let mut st = c.prepare(&sql).ok()?;
            let found = st.exists(rusqlite::params_from_iter(params)).ok()?;
            Some(found)
        })();
        if ok != Some(true) {
            say(&format!(
                "kith: session {resume} is not one of {person}'s with {agent}; starting a new one"
            ));
            resume.clear();
        }
    }
    for k in [
        "KITH_TERMINAL_SESSION",
        "KITH_RESUMED_FROM",
        "KITH_LAUNCH_KIND",
        "KITH_LAUNCH_LABEL",
        "KITH_RUN_KIND",
        "KITH_RUN_LABEL",
        "KITH_LAUNCH_ASK_ID",
        "KITH_ASK_ID",
        "KITH_LAUNCH_SID_FILE",
        "KITH_CLAUDE_ARGS",
        "HQ_CLAUDE_ARGS",
        // (the twins `sync_env` made: it would fill the KITH_ names back from them)
        "HQ_LAUNCH_KIND",
        "HQ_LAUNCH_LABEL",
        "HQ_RUN_KIND",
        "HQ_RUN_LABEL",
        "HQ_LAUNCH_ASK_ID",
        "HQ_ASK_ID",
        "HQ_LAUNCH_SID_FILE",
    ] {
        env.remove(k);
    }
    if nested || !p_owner {
        for k in [
            "KITH_ASK_TOKEN",
            "KITH_ASK_SOCKET",
            "HQ_ASK_TOKEN",
            "HQ_ASK_SOCKET",
        ] {
            env.remove(k);
        }
    }

    // ---- the engine
    let backup_exit = if a.backup.is_empty() { 1 } else { 4 };
    let via = if get(&env, "KITH_VIA").is_empty() {
        "terminal".to_owned()
    } else {
        get(&env, "KITH_VIA").to_owned()
    };
    env.insert("KITH_VIA".to_owned(), via);
    let which = |x: &str| which_in(&env, x);
    let engine_spec = |h: &str| engines::find(h).map(|e| engines::launch_spec(e, &which));
    let mut spec: Option<Value> = None;
    if harness != "claude" && harness != "codex" {
        spec = engine_spec(&harness)
            .filter(|d| d.get("acp").is_some_and(truthy) || d.get("registry").is_some_and(truthy));
        if spec.is_none() {
            say(&format!(
                "kith: {agent} runs on {harness}; --launch-json covers claude and codex only for now"
            ));
            return Err(backup_exit);
        }
    }
    if !p_owner
        && harness != "claude"
        && engine_spec(&harness).and_then(|d| d["serves_guests"].as_bool()) != Some(true)
    {
        say(&format!(
            "kith: {agent} for {person}: runs for anyone but the owner need Claude Code (path rules), not {harness}"
        ));
        return Err(if a.backup.is_empty() { 3 } else { 4 });
    }

    // ---- the settings
    let claude = harness == "claude";
    let is_terminal =
        exec.is_some_and(|x| owner_terminal(x, a.print, p_owner, nested, get(&env, "KITH_VIA")));
    let sys = Sys { env: &env };
    let project_dirs = settings::project_dirs(&reg, &project, &home_dir);
    let plugins_json = if claude {
        sys.out("claude", &["plugin", "list", "--json"], false)
            .unwrap_or_else(|| "[]".to_owned())
    } else {
        "[]".to_owned()
    };
    let developer_dir = if p_owner {
        String::new()
    } else {
        sys.out("xcode-select", &["-p"], false).unwrap_or_default()
    };
    let cver = if p_owner { None } else { claude_version(&sys) };
    // The hosts every guest of this agent reaches without a card, asked of kith-auth here (not of the host), so a launch with
    // no run socket has them too. No answer is none.
    let guest_domains: Result<Vec<String>, String> = if p_owner || !claude {
        Ok(vec![])
    } else {
        // A hung kith-auth must not hang the launch: on the timeout the guest gets no extra domains and the run goes
        // on with a warning (the same as a failure; kernel/bin/kith waits 20 s too).
        let answer = sys.out_within(
            kernel.join("bin/kith-auth"),
            &["agent-access", agent, "--json"],
            AGENT_ACCESS_TIMEOUT,
        );
        settings::guest_domains(answer.as_deref())
    };
    let kith_bin = Some(get(&env, "KITH_BIN"))
        .filter(|b| usable_kith_bin(Path::new(b)))
        .map(str::to_owned)
        .or_else(|| std::env::current_exe().ok().map(|e| s(&e)));
    // A guest's `project-id` is the stub at bin/project-id, which only takes a program whose `kernel version` lists project-guest
    // and runs inside the sandbox, where the program this opens is the only one it can start: so a guest's program is the launch's
    // only when it lists the module, else this one (it always does).
    let kith_bin = if p_owner {
        kith_bin
    } else {
        kith_bin
            .filter(|b| lists_module(Path::new(b), "project-guest"))
            .or_else(|| std::env::current_exe().ok().map(|e| s(&e)))
    };
    // An older kith has no `kernel readfence`; this program always has it.
    let fence_bin = kith_bin
        .clone()
        .filter(|b| knows_readfence(Path::new(b)))
        .or_else(|| std::env::current_exe().ok().map(|e| s(&e)));
    let ledger_bin = ledger_hook_bin(kernel, &env, kith_bin.as_deref());
    let cost_bin = cost_hook_bin(kith_bin.as_deref());
    let permit_bin = permit_hook_bin(kernel, &env, kith_bin.as_deref());
    let threads_bin = threads_hook_bin(kernel, &env, kith_bin.as_deref());
    if perm_mode.is_empty() {
        perm_mode.clear();
    }
    let once_id = match random_hex() {
        Ok(id) => id,
        Err(m) => {
            say(&m);
            return Err(1);
        }
    };
    let out = {
        let envf = {
            let e = env.clone();
            move |k: &str| e.get(k).cloned()
        };
        settings::build(&settings::Input {
            agent,
            paths: &p,
            kernel,
            src_pkg: &src_pkg,
            pkg: &pkg,
            member: &member,
            claude,
            caller_mode: &perm_mode,
            project: &project,
            project_dirs: &project_dirs,
            cwd: &cwd_logical,
            user_home: &home_dir,
            kith_bin: kith_bin.as_deref(),
            fence_bin: fence_bin.as_deref(),
            ledger_bin: ledger_bin.as_deref(),
            cost_bin: cost_bin.as_deref(),
            permit_bin: permit_bin.as_deref(),
            threads_bin: threads_bin.as_deref(),
            plugins_json: &plugins_json,
            owner_terminal: is_terminal && claude,
            api_helper: &bget(&be, "backup_api_helper"),
            once_id: &once_id,
            developer_dir: &developer_dir,
            ctmp: &ctmp,
            claude_version: cver.as_deref(),
            guest_domains: guest_domains.as_deref().map_err(String::as_str),
            env: &envf,
        })
    };
    let out = match out {
        Ok(o) => o,
        Err(r) => {
            say(&r.message);
            return Err(r.code);
        }
    };
    for w in &out.warnings {
        say(w);
    }
    let physical_cwd = if out.cwd == cwd_logical {
        std::env::current_dir()
            .map(|d| s(&d))
            .unwrap_or(out.cwd.clone())
    } else {
        let _ = std::env::set_current_dir(&out.cwd);
        std::env::current_dir()
            .map(|d| s(&d))
            .unwrap_or(out.cwd.clone())
    };
    let mut settings_val = out.settings.clone();
    if claude {
        env.insert("CLAUDE_CODE_ARTIFACT".to_owned(), "1".to_owned());
    }
    env.insert(
        "KITH_OWNER".to_owned(),
        if p_owner { "1" } else { "0" }.to_owned(),
    );
    // A guest's `project-id` is the stub at bin/project-id, which runs `kith kernel project-id`: it needs the program the launch
    // checked, and the settings open it (and the links to it) to the sandbox's reads.
    if claude
        && !p_owner
        && let Some(b) = &kith_bin
    {
        env.insert("KITH_BIN".to_owned(), b.clone());
    }
    // (A run here applies them after its claim instead: a once grant is spent by the one run about to start.)
    if claude
        && exec.is_none()
        && !p_owner
        && !get(&env, "KITH_RUN_TOKEN").is_empty()
        && !get(&env, "KITH_RUN_SOCKET").is_empty()
        && let Some(sv) = &settings_val
    {
        // F11 2b: what the session was given beyond the box goes into the settings (`kernel permit apply` rewrites the file).
        let Ok(tag) = random_hex().inspect_err(|m| say(m)) else {
            return Err(1);
        };
        let tmp = std::env::temp_dir().join(format!("kith-launch-{tag}.settings.json"));
        if fs::write(&tmp, pj_indent1(sv)).is_ok() {
            let sys = Sys { env: &env };
            let (prog, mut args) = mcp::permit_command(permit_bin.as_deref(), kernel);
            args.extend(["apply".to_owned(), s(&tmp)]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let _ = sys.run_loud(prog, &args);
            if let Some(v) = fs::read_to_string(&tmp)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(t.trim_end_matches('\n')).ok())
            {
                settings_val = Some(v);
            }
        }
        let _ = fs::remove_file(&tmp);
    }

    // ---- the MCP servers
    // The granted shared services, in process (P1-15). Only the owner's run gets them (`mcp::merged`), so a guest's launch
    // neither reads the keys nor needs the core for them.
    let granted: Option<Value> = if p_owner {
        let mut sctx = secrets::Ctx::from_process(kernel);
        sctx.person = Some(person.clone()).filter(|p| !p.is_empty());
        let starter = secrets::exec_program(
            kith_bin.as_deref(),
            env.get("KITH_SECRETS_EXEC").map(String::as_str),
            |b| knows_module(b, "secrets-exec"),
        );
        match secrets::mcp_config(&sctx, &env, agent, &starter) {
            Ok(made) => {
                made.notes.iter().for_each(|n| say(n));
                Some(made.config)
            }
            Err(d) => {
                say(&format!(
                    "kith: {agent}: no shared services this run (kith-auth: {})",
                    d.msg
                ));
                None
            }
        }
    } else {
        None
    };
    let merged = mcp::merged(
        p_owner,
        permit_bin.as_deref(),
        kernel,
        granted.as_ref(),
        &pkg,
    )
    .map_err(|e| {
        say(&e);
        1
    })?;
    let mcp_servers = merged["mcpServers"].clone();
    let mcp_arg = if p_owner {
        pyjson::dumps(&json!({"mcpServers": mcp_servers}))
    } else {
        guest_mcp_arg(permit_bin.as_deref(), kernel)
    };
    let call_model = if claude && p_model.is_empty() {
        call_model(&p.machine, agent)
    } else {
        String::new()
    };
    // The origin of an owner's terminal run: the agent's app chat with them, if a host has one (nothing set otherwise).
    if is_terminal
        && ["KITH_RUN_OUTBOX", "KITH_BRIDGE_OUTBOX", "KITH_CALL_KEY"]
            .iter()
            .all(|k| get(&env, k).is_empty())
    {
        for (k, v) in runenv::outbox_env(agent, kernel, &env) {
            if k == "KITH_RUN_OUTBOX" {
                env.insert("HQ_RUN_OUTBOX".to_owned(), v.clone());
            }
            env.insert(k, v);
        }
    }
    sync_env(&mut env);
    Ok(Plan {
        agent: agent.to_owned(),
        backup: a.backup,
        harness,
        claude,
        owner: p_owner,
        person_env_names: p_env_names,
        cfg_var,
        path_names: p.entries().into_iter().map(|(k, _)| k).collect(),
        env,
        system_prompt: text,
        out,
        settings: settings_val,
        mcp_servers,
        mcp_arg,
        model: kith_model,
        effort: kith_effort,
        call_model,
        physical_cwd,
        spec,
        launch: exec.is_none(),
        owner_terminal: is_terminal,
        pkg,
        src_pkg,
        person_id: p_id,
        nested,
        caller,
        resume,
        out_format,
        permit_mcp: guest_mcp_arg(permit_bin.as_deref(), kernel),
        granted: granted.unwrap_or_else(|| json!({"mcpServers": {}})),
    })
}

/// The pid that `--hold` defaults to: the process that started this one.
#[cfg(unix)]
fn parent_pid() -> String {
    std::os::unix::process::parent_id().to_string()
}

/// Windows has no parent pid in std; `kith --exec` names its own holder with `--hold`.
#[cfg(not(unix))]
fn parent_pid() -> String {
    std::process::id().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(task: bool, held: bool, tty: bool) -> ExecOpts<'static> {
        ExecOpts {
            task,
            held,
            tty,
            ..ExecOpts::default()
        }
    }

    #[test]
    fn the_terminal_is_the_owners_with_no_task_and_a_tty_or_a_holder() {
        let t = owner_terminal;
        assert!(t(&opts(false, false, true), false, true, false, "terminal"));
        assert!(
            t(&opts(false, true, false), false, true, false, "terminal"),
            "held needs no tty"
        );
        assert!(
            !t(&opts(false, false, false), false, true, false, "terminal"),
            "no tty, no holder"
        );
        assert!(
            !t(&opts(true, false, true), false, true, false, "terminal"),
            "a task is a one-shot"
        );
        assert!(
            !t(&opts(false, false, true), true, true, false, "terminal"),
            "--print"
        );
        assert!(
            !t(&opts(false, false, true), false, false, false, "terminal"),
            "a guest"
        );
        assert!(
            !t(&opts(false, false, true), false, true, true, "terminal"),
            "nested"
        );
        assert!(
            !t(&opts(false, false, true), false, true, false, "chat"),
            "the host's turns"
        );
    }
}
