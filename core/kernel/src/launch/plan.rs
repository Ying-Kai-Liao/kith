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
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::assemble::{
    AGENT_ACCESS_TIMEOUT, Env, Sys, assignments, call_model, claude_version, cost_core,
    cost_hook_bin, field, get, guest_mcp_arg, guest_tmp_ready, in_guest_sandbox, knows_module,
    knows_readfence, ledger_hook_bin, lists_module, parse_args, permit_hook_bin, pj_indent1,
    plain_number, pyrepr, pystr, random_hex, s, sync_env, threads_hook_bin, truthy,
    usable_kith_bin, which_in,
};
use super::{mcp, prompt, settings};
use crate::secrets::pyjson;
use crate::{accounts, engines, paths, secrets};

/// What a launch decided, ready to be written as the launch object or to start a run from.
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
}

/// `kith AGENT [args]` up to the launch object: the plan on success, or the exit code (what was said is already on stderr).
pub fn plan(agent: &str, argv: &[String], kernel: &Path, env0: Env) -> Result<Plan, i32> {
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
    if a.resume {
        say("kith: --resume is for a run in this terminal, not --launch-json or --print");
        return Err(1);
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
                "ask" => say(&format!(
                    "kith: {text}; running anyway (your runs never stop at a limit; kith {agent} set limit N raises it)."
                )),
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
        let hold = a
            .hold
            .clone()
            .unwrap_or_else(|| std::os::unix::process::parent_id().to_string());
        if hold.is_empty() || !hold.chars().all(|c| c.is_ascii_digit()) {
            say("kith: --hold needs a process id");
            return Err(1);
        }
        let _ = fs::create_dir_all(pkg.join(".runs"));
        let _ = fs::write(pkg.join(".runs").join(format!("{hold}.launch")), "");
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
    if !a.backup.is_empty() {
        use_account(&a.backup, &mut be)?;
        harness = bget(&be, "backup_harness");
        acct = bget(&be, "backup_account");
        account_id = a.backup.split('@').next().unwrap_or_default().to_owned();
    } else if harness == own {
        let opts = accounts::MainOpts {
            record,
            ..accounts::MainOpts::default()
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
    if !resume.is_empty() {
        let made = accounts::session_account(&p.machine, &resume).unwrap_or_default();
        if !made.is_empty() && made != account_id {
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
            owner_terminal: false,
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
    if claude
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
    })
}
