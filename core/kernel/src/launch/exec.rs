//! `kith kernel launch AGENT --exec ARGS`: a one-shot run in this process (`kith AGENT "task"`, what the host's chat turns, card
//! resumes, `kith ask` and the schedules start), in the order of the bash entry (`kernel/bin/kith`) and the one-run-path design
//! (docs of 2026-11-21, section 3). The pid is this process's: every claim and token names it and the engine descends from it.
//!
//! The order, which the thread-writers and revoke-access models check (I1-I7):
//!
//! 1. a person's revoke epoch is read (I1), before the plan asks whether they may run (and holds for the sync, I6);
//! 2. the plan: who the run is for, `--resume`'s claim ([`Hooks::resume`]), the account walk, the limit, the prompt, the login;
//! 3. run state, the person's `proc` claim for the revoke sweep (I5);
//! 4. the session id, the lock, the claim (I3), the permit token and a guest's grants, then the engine ([`super::start`]).
//!
//! A run with no task is the terminal's ([`super::terminal`]), and `--held` hands it to the core's holder ([`super::held`]): the
//! entry sends them here only when the program lists `launch-exec-term` (`kith_term_flag` in `lib/kith-core.sh`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::args::{Files, run_args};
use super::assemble::{Env, get, pj_indent1, sync_env};
use super::held;
use super::plan::{self, ExecHooks, ExecOpts, Hooks, Mode, Resumed};
use super::sched_turn::{self, Sched};
use super::session;
use super::start::{self, Adapter, Engine, Go};
use super::terminal::{self, Term};
use crate::secrets::pyjson;
use crate::threads::core::Core;
use crate::threads::resolve::{Host, resolve_taking};
use crate::threads::writer::{ProcErr, Taker};
use crate::threads::{self, Ctx, claim_wait};
use crate::{accounts, paths};

/// What the host hands a run for its claimed turn: the claim and the conversation it is on. The launch attaches it; neither
/// reaches the engine or a nested `kith`.
const TURN_VARS: [&str; 4] = [
    "KITH_TURN_CLAIM",
    "HQ_TURN_CLAIM",
    "KITH_TURN_CONVERSATION",
    "HQ_TURN_CONVERSATION",
];

/// What a run here asks of the world outside the kernel crate: the host (to name a thread), the core (the claims) and, for
/// `--held`, the core's terminal calls and the attach client (none: nothing can hold a terminal).
pub struct Deps<'a> {
    pub host: &'a dyn Host,
    pub core: Option<&'a dyn Core>,
    pub held: Option<&'a dyn held::Port>,
}

/// How `--resume` named its session.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    /// `--resume ID`, `-r ID`: a thread id or a Claude session id.
    Id(String),
    /// `-r` alone: the agent's recent sessions, one chosen.
    Choose,
    /// `-c`: the latest begun in this folder.
    Latest,
}

/// The flags of `kith AGENT [flags] [--] TASK...` the run here reads itself (the plan reads the others).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub resume: Option<Pick>,
    pub held: bool,
    /// `--print`, `--launch-json`: the entry answers these, not a run.
    pub no_run: bool,
    /// The flags that stand before the task, without the ones only the entry knows (`--held`, `--direct`).
    pub plan_argv: Vec<String>,
    pub task: Vec<String>,
}

impl Flags {
    /// The flag loop of `kernel/bin/kith`: the flags end at the first word, at `--`, or at a word that starts with a dash and holds a
    /// blank (a task such as "- one thing"). A flag the entry refuses ends the loop too: the plan says so.
    pub fn parse(argv: &[String]) -> Self {
        let mut f = Self::default();
        let mut i = 0;
        while i < argv.len() {
            let x = argv[i].as_str();
            let next = argv.get(i + 1);
            let step = match x {
                "--print" | "--launch-json" => {
                    f.no_run = true;
                    1
                }
                "--held" => {
                    f.held = true;
                    1
                }
                "--direct" => {
                    f.held = false;
                    1
                }
                "--resume" => {
                    f.resume = next.map(|v| Pick::Id(v.clone()));
                    2
                }
                "-r" => {
                    if let Some(v) = next.filter(|n| !n.starts_with('-')) {
                        f.resume = Some(Pick::Id(v.clone()));
                        2
                    } else {
                        f.resume = Some(Pick::Choose);
                        1
                    }
                }
                "-c" | "--continue" => {
                    f.resume = Some(Pick::Latest);
                    1
                }
                "--hold" | "--account" | "--backup" | "-p" | "--project" | "-m" | "--model"
                | "-e" | "--effort" | "-H" | "--harness" | "--permission-mode" => 2,
                "--bypass" => 1,
                "--" => {
                    // The plan needs it too: what follows is a task even when it starts like a flag.
                    f.plan_argv.push("--".to_owned());
                    i += 1;
                    break;
                }
                _ => break,
            };
            if !matches!(x, "--held" | "--direct") {
                f.plan_argv
                    .extend(argv[i..(i + step).min(argv.len())].iter().cloned());
            }
            i += step;
        }
        f.task = argv.get(i..).unwrap_or_default().to_vec();
        f.plan_argv.extend(f.task.iter().cloned());
        f
    }

    /// What Claude is asked: the words after the flags, joined as `$*` joins them.
    pub fn task_text(&self) -> String {
        self.task.join(" ")
    }
}

/// The steps between the plan's own, with what they need.
struct Steps<'a> {
    ctx: &'a Ctx<'a>,
    deps: &'a Deps<'a>,
    flags: &'a Flags,
    pid: i64,
    kernel: &'a Path,
    wait: std::time::Duration,
    /// Where `latest` looks for the folder's session, as `$PWD` names it.
    pwd: String,
    machine: PathBuf,
    accounts_ctx: accounts::Ctx<'a>,
    /// The resumed session was claimed under a core that gave no answer.
    alone: RefCell<bool>,
    /// The Claude session `--resume`/`-r`/`-c` took the claim of ("" for none).
    resumed: RefCell<String>,
    /// A schedule's turn: its claim is attached before the walk starts any attempt.
    sched: Option<&'a Sched>,
    /// The run's own environment, which the walk's attempts get (not this process's).
    env: &'a Env,
}

impl ExecHooks for Steps<'_> {
    fn resume(&self, agent: &str) -> Result<Resumed, i32> {
        let host = self.deps.host;
        let core = self.deps.core;
        let stdin = || {
            let mut t = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut t);
            t
        };
        let ask = |prompt: &str| {
            eprint!("{prompt}");
            let _ = std::io::Write::flush(&mut std::io::stderr());
            let mut t = String::new();
            let _ = std::io::stdin().read_line(&mut t);
            t
        };
        // The id: named, picked from the agent's recent sessions, or the latest begun here. Never a fresh claude when it names none.
        let ask_verb = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let out = threads::cli::run_with(self.ctx, host, core, &args, &stdin, &ask);
            eprint!("{}", out.stderr);
            if out.code == 0 {
                Ok(out.stdout.trim().to_owned())
            } else {
                Err(1)
            }
        };
        let thread = match &self.flags.resume {
            Some(Pick::Id(id)) => id.clone(),
            Some(Pick::Choose) => ask_verb(&["pick", agent])?,
            Some(Pick::Latest) => ask_verb(&["latest", agent, &self.pwd])?,
            None => return Err(1),
        };
        if thread.is_empty() {
            return Err(1);
        }
        // Whatever named the session, it is claimed and checked here and nowhere else (a one-shot waits a few seconds for a held one).
        match resolve_taking(self.ctx, host, core, agent, &thread, self.pid, self.wait) {
            Ok(t) => {
                eprint!("{}", t.said);
                *self.alone.borrow_mut() = t.alone;
                self.resumed.borrow_mut().clone_from(&t.resolved.sid);
                let run = accounts::session_run(&self.machine, &t.resolved.sid);
                Ok(Resumed {
                    sid: t.resolved.sid,
                    cwd: t.resolved.cwd,
                    thread,
                    run,
                })
            }
            Err(m) => {
                eprintln!("kith: {m}");
                Err(1)
            }
        }
    }

    fn walk(&self, agent: &str, agent_md: &Path, argv: &[String]) -> i32 {
        // The attempts are runs of their own that send the prompt: the claim is this process's before the first one.
        let taker = Taker::new(self.ctx, self.deps.core);
        if let Some(s) = self.sched {
            if let Err(code) = s.attach(&taker, self.pid) {
                return code;
            }
        } else if let Some(code) = sched_turn::attach_env_claim(self.env, &taker, self.pid) {
            return code;
        }
        // The attempts are runs of the run's own environment, without the claim (it is attached above, and each attempt would
        // try to attach its own pid).
        let mut attempt_env = self.env.clone();
        for k in TURN_VARS {
            attempt_env.remove(k);
        }
        let Ok(me) = std::env::current_exe() else {
            eprintln!("kith: can't find this program to start the attempts");
            return 1;
        };
        let base = [
            me.to_string_lossy().into_owned(),
            "kernel".to_owned(),
            "launch-exec".to_owned(),
        ];
        let tail = [
            "--kernel".to_owned(),
            self.kernel.to_string_lossy().into_owned(),
        ];
        accounts::chain::run_with(
            &self.accounts_ctx,
            agent,
            agent_md,
            &base,
            &tail,
            argv,
            Some(&attempt_env),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
    }
}

/// `kith kernel launch-exec AGENT [--kernel DIR] ARGS...`: the exit code (only returned when the run did not start or, off Unix, ended).
pub fn run(argv: &[String], kernel: &Path, env0: Env, deps: &Deps<'_>) -> i32 {
    run_with(argv, kernel, env0, deps, Engine::native())
}

/// [`run`], with the way the engine starts named (the tests wait for it).
pub fn run_with(
    argv: &[String],
    kernel: &Path,
    mut env0: Env,
    deps: &Deps<'_>,
    how: Engine,
) -> i32 {
    let Some((agent, rest)) = argv.split_first() else {
        eprintln!("usage: kith kernel launch-exec AGENT [--kernel DIR] ARGS...");
        return 2;
    };
    sync_env(&mut env0);
    // `--turn N --claim C`: the core's turn of a cron job, which this launch attaches to its claim and runs (sched-turn).
    let (sched, argv) = match Sched::prepare(agent, rest, kernel, &mut env0) {
        Ok(Some((s, task))) => (Some(s), [std::slice::from_ref(agent), &task].concat()),
        Ok(None) => (None, argv.to_vec()),
        Err(code) => return code,
    };
    let code = run_one(&argv, kernel, &env0, deps, how, sched.as_ref());
    sched.map_or(code, |s| s.finish(code))
}

fn run_one(
    argv: &[String],
    kernel: &Path,
    env0: &Env,
    deps: &Deps<'_>,
    how: Engine,
    sched: Option<&Sched>,
) -> i32 {
    let Some((agent, rest)) = argv.split_first() else {
        return 2;
    };
    let flags = Flags::parse(rest);
    if flags.no_run {
        eprintln!(
            "kith: --exec runs a task or a terminal; --print and --launch-json answer elsewhere"
        );
        return 2;
    }
    // A claim that is no claim id is refused before anything else is looked at (a schedule's turn has its claim in its flags).
    if sched.is_none()
        && let Err(code) = sched_turn::env_claim(env0)
    {
        return code;
    }
    let person = get(env0, "KITH_PERSON").to_owned();
    // A room person (the meet of everyone in a room with people) has no row in auth.json: only the Rust launch builds its grant, and
    // it is the host's launch that starts those runs, not this one.
    if super::room::is_room_person(&person) {
        eprintln!(
            "kith: {person} is a room person; it only launches through the Rust launch, not this script"
        );
        return 3;
    }
    let home_dir = PathBuf::from(get(env0, "HOME"));
    let p = {
        let mut e = env0.clone();
        e.insert(
            "KITH_ROOT".to_owned(),
            kernel.to_string_lossy().into_owned(),
        );
        paths::paths(&home_dir, kernel, &move |k| e.get(k).cloned())
    };
    let seen = RefCell::new(String::new());
    let lookup = |k: &str| -> Option<String> {
        if k == "KITH_REVOKE_SEEN" {
            return Some(seen.borrow().clone()).filter(|s| !s.is_empty());
        }
        env0.get(k).cloned()
    };
    let ctx = Ctx {
        state: Some(p.state.clone()),
        machine: Some(p.machine.clone()),
        home: home_dir.clone(),
        env: &lookup,
    };
    let taker = Taker::new(&ctx, deps.core);
    let pid = i64::from(std::process::id());
    // I1: a person's revoke epoch is read BEFORE the plan decides they may run, and travels with every claim of this run
    // (KITH_REVOKE_SEEN): a revoke that lands in between refuses the claim instead of being missed by the sweep that ran before it.
    // Files mode reads none.
    if !person.is_empty() {
        match taker.epoch(&person) {
            Ok(t) => *seen.borrow_mut() = t.trim().to_owned(),
            Err(m) => {
                eprintln!("kith: {m}");
                return 1;
            }
        }
    }
    let system = accounts::System {
        kernel: kernel.to_path_buf(),
    };
    let steps = Steps {
        ctx: &ctx,
        deps,
        flags: &flags,
        pid,
        kernel,
        // A one-shot waits a few seconds for a held session; a terminal run refuses at once.
        wait: if flags.task.is_empty() {
            std::time::Duration::ZERO
        } else {
            claim_wait(&lookup)
        },
        pwd: {
            let cwd = std::env::current_dir().unwrap_or_default();
            match env0.get("PWD").filter(|d| !d.is_empty()) {
                Some(d) if std::fs::canonicalize(d).ok() == std::fs::canonicalize(&cwd).ok() => {
                    d.clone()
                }
                _ => cwd.to_string_lossy().into_owned(),
            }
        },
        machine: p.machine.clone(),
        accounts_ctx: accounts::Ctx {
            machine: p.machine.clone(),
            agents: p.agents.clone(),
            kernel: kernel.to_path_buf(),
            home: home_dir.clone(),
            claude_config_dir: env0.get("CLAUDE_CONFIG_DIR").cloned(),
            out: &system,
        },
        alone: RefCell::new(false),
        resumed: RefCell::new(String::new()),
        sched,
        env: env0,
    };
    // What the plan takes out of the environment and this run still needs (names of the caller's, for this run only).
    let launch_kind = get(env0, "KITH_LAUNCH_KIND").to_owned();
    let launch_label = get(env0, "KITH_LAUNCH_LABEL").to_owned();
    let ask_id = get(env0, "KITH_LAUNCH_ASK_ID").to_owned();
    let sid_file = get(env0, "KITH_LAUNCH_SID_FILE").to_owned();
    let claude_args = get(env0, "KITH_CLAUDE_ARGS").to_owned();
    let via = if get(env0, "KITH_VIA").is_empty() {
        "terminal".to_owned()
    } else {
        get(env0, "KITH_VIA").to_owned()
    };
    let opts = ExecOpts {
        hooks: Hooks(Some(&steps)),
        ..ExecOpts::from_process(!flags.task.is_empty(), flags.held, "")
    };
    let plan = match plan::plan(
        agent,
        &flags.plan_argv,
        kernel,
        env0.clone(),
        &Mode::Exec(opts),
    ) {
        Ok(p) => p,
        // A turn's run that the plan refuses (the person's auth did not resolve, or was revoked) is `refused()` to the host: 3.
        Err(1) if sched.is_some_and(|s| !s.attached()) => return sched_turn::REFUSED,
        Err(code) => return code,
    };
    let mut env = plan.env.clone();
    // The host's claim is this launch's to attach; the engine and a nested `kith` never see it.
    // A claimed turn on the very session it resumes (a wake) continues it or does not run: the engine is told, as it never sees the
    // turn's conversation.
    let resume_only = !plan.resume.is_empty() && get(&env, "KITH_TURN_CONVERSATION") == plan.resume;
    for k in TURN_VARS {
        env.remove(k);
    }
    env.remove("KITH_RESUME_ONLY");
    if resume_only {
        env.insert("KITH_RESUME_ONLY".to_owned(), "1".to_owned());
    }
    if let Some(s) = sched
        && let Err(code) = s.attach(&taker, pid)
    {
        return code;
    }
    // The plan names the origin `terminal` for its cost tag when none was said; the run's own environment keeps what it was given.
    if get(env0, "KITH_VIA").is_empty() {
        env.remove("KITH_VIA");
        env.remove("HQ_VIA");
    }
    let owner = plan.owner;
    // What kind of one-shot this is, for the apps' rooms: kith-ask and kith-sched pass KITH_LAUNCH_KIND to the run they start; a run
    // another run of the owner's started (from a terminal, an ask or a job) is a helper. The host's own runs are not listed here.
    let (mut kind, mut label) = (launch_kind, launch_label);
    if kind.is_empty() && plan.nested && matches!(via.as_str(), "terminal" | "ask" | "schedule") {
        kind = "helper".to_owned();
        label.clone_from(&plan.caller);
    }
    let owner_own = owner && get(&env, "KITH_PERSON").is_empty();
    if owner_own && !kind.is_empty() {
        env.insert("KITH_RUN_KIND".to_owned(), kind);
        env.insert("KITH_RUN_LABEL".to_owned(), label);
        // `kith ask -c` continues a session: the card joins the thread of the one it resumes (the registry's resumedFrom).
        let resumed_here = flags.resume.is_some();
        if !plan.resume.is_empty() && !plan.resume.contains(':') && !resumed_here {
            env.insert("KITH_RESUMED_FROM".to_owned(), plan.resume.clone());
        }
    }
    // The ask token belongs to one run (the owner's approval cards, KITH_ASK_*): the plan took it from a helper's and a guest's.
    if owner_own && !ask_id.is_empty() {
        env.insert("KITH_ASK_ID".to_owned(), ask_id);
    }
    // I5: every run with someone else's profile is findable by the revoke sweep: its pid is the one the engine descends from, so the
    // claim lives as long as the engine. Once the claims are the core's, any failure of `proc` means this person's run has no core
    // claim, so a revoke could not find it: it does not start.
    if !owner {
        let refused = match taker.proc(pid, &plan.person_id, agent) {
            Ok(()) => false,
            Err(ProcErr::Core(_)) => true,
            Err(ProcErr::Io(_)) => taker.writer(),
        };
        if refused {
            eprintln!(
                "kith: the Kith core cannot record this run, so {} cannot start it right now (is kith serve running?)",
                plan.person_id
            );
            return 1;
        }
    }
    let runs = match run_folder(&plan, pid) {
        Ok(d) => d,
        Err(m) => {
            eprintln!("kith: {m}");
            return 1;
        }
    };
    let (settings_file, mcp_file) = (
        runs.join(format!("{pid}.settings.json")),
        runs.join(format!("{pid}.mcp.json")),
    );
    // Run files: in the snapshot (so cleanup sees the run), or a temp dir when running the live folder.
    let _ = std::fs::write(&mcp_file, format!("{}\n", pyjson::dumps(&plan.granted)));
    let task = flags.task_text();
    let kbin = Some(get(&env, "KITH_BIN"))
        .filter(|b| !b.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    if !plan.claude {
        // Claude's claim step attaches a host-claimed turn; another engine has none, so it is attached here.
        if let Some(code) = sched_turn::attach_env_claim(env0, &taker, pid) {
            return code;
        }
        if flags.held {
            eprintln!(
                "kith: --held holds claude sessions only; {} runs here (--direct)",
                plan.harness
            );
        }
        return other_engine(&plan, env, &runs, pid, task, how);
    }
    if flags.held && !task.is_empty() {
        eprintln!("kith: --held is for an interactive session; this task runs here (--direct)");
    }
    if let Some(s) = &plan.settings {
        let _ = std::fs::write(&settings_file, pj_indent1(s));
    }
    let files = Files {
        settings: &settings_file.to_string_lossy(),
        mcp: &mcp_file.to_string_lossy(),
    };
    let mut engine_args = run_args(&plan, &files);
    // Extra claude flags from a caller such as kith serve (e.g. --output-format json --resume <session>); for them only.
    engine_args.extend(session::extra_args(&claude_args, owner));
    // Claude's claim step attaches by the session's id: a run that continues the folder's latest session (`-c`) names none, so
    // the claim is attached here, for this process, as another engine's is.
    if matches!(session::session_of(&engine_args), Ok((csid, _)) if csid.is_empty())
        && let Some(code) = sched_turn::attach_env_claim(env0, &taker, pid)
    {
        return code;
    }
    env.remove("KITH_CLAUDE_ARGS");
    env.remove("HQ_CLAUDE_ARGS");
    if task.is_empty() {
        return terminal::claude(
            Term {
                agent,
                kernel,
                env,
                args: engine_args,
                owner,
                nested: plan.nested,
                claim_person: plan.person_id.clone(),
                settings_file,
                resumed: steps.resumed.borrow().clone(),
                alone: *steps.alone.borrow(),
                held: flags.held,
                kbin,
                ctx: &ctx,
                deps,
            },
            &taker,
            pid,
        );
    }
    start::claude(
        Go {
            agent,
            kernel,
            env,
            args: engine_args,
            task,
            owner,
            claim_person: plan.person_id.clone(),
            settings_file,
            permit_mcp: plan.permit_mcp.clone(),
            alone: *steps.alone.borrow(),
            sid_file,
            kbin,
        },
        &taker,
        pid,
        how,
    )
}

/// The folder the run's files go in: the snapshot's `.runs` (so cleanup sees the run), or a new temp folder for the live folder.
fn run_folder(plan: &plan::Plan, pid: i64) -> Result<PathBuf, String> {
    if plan.pkg != plan.src_pkg {
        let d = plan.pkg.join(".runs");
        std::fs::create_dir_all(&d).map_err(|e| format!("can't make {}: {e}", d.display()))?;
        return Ok(d);
    }
    let tag = super::assemble::random_hex()?;
    let d = std::env::temp_dir().join(format!("kith-run-{pid}-{}", &tag[..8]));
    std::fs::create_dir_all(&d).map_err(|e| format!("can't make {}: {e}", d.display()))?;
    Ok(d)
}

/// Another harness: the same run, handed to `kernel engine-run` (prompt in a file, directories and MCP configs as lists).
/// Always written, so an inherited `KITH_RUN_OWNER=1` never reaches a guest's engine.
fn set_run_owner(env: &mut Env, owner: bool) {
    env.insert(
        "KITH_RUN_OWNER".to_owned(),
        if owner { "1" } else { "0" }.to_owned(),
    );
}

fn other_engine(
    plan: &plan::Plan,
    mut env: Env,
    runs: &Path,
    pid: i64,
    task: String,
    how: Engine,
) -> i32 {
    let mcp_file = runs.join(format!("{pid}.mcp.json"));
    let prompt_file = runs.join(format!("{pid}.prompt.md"));
    if let Err(e) = std::fs::write(&prompt_file, format!("{}\n", plan.system_prompt)) {
        eprintln!("kith: can't write {}: {e}", prompt_file.display());
        return 1;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut dirs = vec![cwd.to_string_lossy().into_owned()];
    dirs.extend(plan.out.add_dirs.iter().cloned());
    let mut mcp = mcp_file.to_string_lossy().into_owned();
    let own = plan.pkg.join("mcp.json");
    if own.is_file() {
        mcp.push('\n');
        mcp.push_str(&own.to_string_lossy());
    }
    env.insert(
        "KITH_RUN_PROMPT".to_owned(),
        prompt_file.to_string_lossy().into_owned(),
    );
    env.insert("KITH_RUN_DIRS".to_owned(), dirs.join("\n"));
    env.insert("KITH_RUN_MCP".to_owned(), mcp);
    // Only the owner's own run gets network inside the engine's sandbox (`owner` is the plan's, as for the other owner-only rules).
    set_run_owner(&mut env, plan.owner);
    // Global skills load unless the role turned them off.
    env.insert(
        "KITH_RUN_GLOBAL_SKILLS".to_owned(),
        if plan.out.disable_slash_commands && !plan.owner_terminal {
            "0"
        } else {
            "1"
        }
        .to_owned(),
    );
    let mut flags = vec!["--".to_owned()];
    if !plan.out_format.is_empty() {
        flags = ["--output", plan.out_format.as_str()]
            .into_iter()
            .map(str::to_owned)
            .chain(flags)
            .collect();
    }
    if !plan.resume.is_empty() {
        flags = ["--resume", plan.resume.as_str()]
            .into_iter()
            .map(str::to_owned)
            .chain(flags)
            .collect();
    }
    if !plan.out.mode_arg.is_empty() {
        flags = ["--permission-mode", plan.out.mode_arg.as_str()]
            .into_iter()
            .map(str::to_owned)
            .chain(flags)
            .collect();
    }
    if task.is_empty() {
        return terminal::adapter(&plan.agent, &plan.harness, flags, &env, how);
    }
    start::other(Adapter {
        env,
        harness: plan.harness.clone(),
        flags,
        task,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_run_owner_flag_follows_the_plan_not_the_inherited_env() {
        let mut env = Env::new();
        env.insert("KITH_RUN_OWNER".to_owned(), "1".to_owned());
        set_run_owner(&mut env, false);
        assert_eq!(env["KITH_RUN_OWNER"], "0");
        set_run_owner(&mut env, true);
        assert_eq!(env["KITH_RUN_OWNER"], "1");
    }

    #[test]
    fn a_task_is_what_stands_after_the_flags() {
        let f = Flags::parse(&v(&["do", "it"]));
        assert_eq!(f.task, v(&["do", "it"]));
        assert_eq!(f.task_text(), "do it");
        assert_eq!(f.plan_argv, v(&["do", "it"]));
        assert!(f.resume.is_none());
    }

    #[test]
    fn the_chat_turn_argv_ends_its_flags_at_the_dashes() {
        let f = Flags::parse(&v(&["--backup", "acct", "--", "-r", "text"]));
        assert_eq!(
            f.task,
            v(&["-r", "text"]),
            "a task may start with a flag's name"
        );
        assert!(f.resume.is_none());
        assert_eq!(f.plan_argv, v(&["--backup", "acct", "--", "-r", "text"]));
        assert_eq!(f.task_text(), "-r text");
    }

    #[test]
    fn a_resume_names_its_session_or_asks_for_one() {
        let f = Flags::parse(&v(&["--resume", "sid1", "note"]));
        assert_eq!(f.resume, Some(Pick::Id("sid1".to_owned())));
        assert_eq!(f.task, v(&["note"]));
        assert_eq!(
            Flags::parse(&v(&["-r", "sid2", "x"])).resume,
            Some(Pick::Id("sid2".to_owned()))
        );
        // `-r` followed by a flag picks.
        let f = Flags::parse(&v(&["-r", "-m", "opus", "x"]));
        assert_eq!(f.resume, Some(Pick::Choose));
        assert_eq!(f.task, v(&["x"]));
        assert_eq!(Flags::parse(&v(&["-c", "x"])).resume, Some(Pick::Latest));
    }

    #[test]
    fn the_entrys_own_flags_do_not_reach_the_plan() {
        let f = Flags::parse(&v(&["--held", "-m", "opus", "task"]));
        assert!(f.held);
        assert_eq!(f.plan_argv, v(&["-m", "opus", "task"]));
        let f = Flags::parse(&v(&["--held", "--direct", "task"]));
        assert!(!f.held);
    }

    #[test]
    fn a_word_that_starts_with_a_dash_and_holds_a_blank_is_a_task() {
        let f = Flags::parse(&v(&["- one thing", "more"]));
        assert_eq!(f.task, v(&["- one thing", "more"]));
        assert!(Flags::parse(&v(&["--print", "x"])).no_run);
    }
}
