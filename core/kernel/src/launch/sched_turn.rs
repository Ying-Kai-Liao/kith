//! `kith kernel launch AGENT --exec --turn N --claim C`: the turn the core fired for a cron job, run by the launch itself
//! (design 2026-11-24-sched-turn, S1). What `kith-sched run-turn` built and exec'd (kernel/bin/kith-sched: `run_env`, `turn_job`,
//! `run_turn`) is built here, in this process: the host hands the turn on stdin
//! (`{prompt, settings, person, job: {id, cron, ...}, origin: {NAME: value}}`) and has already opened the log as stdout and stderr.
//!
//! The order is the old one (PromptOnce): the person is refused first, then the claim is attached by this very pid (the engine
//! descends from it), and only then is the log truncated and headed. A claim that is gone leaves the log as the winner wrote it.
//!
//! The same module attaches a claimed run that has no conversation to name (a `KITH_TURN_CLAIM` handed to a non-Claude one-shot).

use std::cell::Cell;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::assemble::{Env, get, sync_env};
use crate::threads::writer::{AttachErr, TURN_CLAIM_EXIT, TURN_CLAIM_REFUSED, Taker};

/// The exits `run-turn` has, which the host reads.
const BAD: i32 = 2;
pub const REFUSED: i32 = 3;
const GONE: i32 = 4;
const NO_ANSWER: i32 = 5;

/// A turn, ready to run: the claim to attach and what to do when the run does not happen.
pub struct Sched {
    claim: i64,
    /// The first line of the log, written once the attach has won.
    header: String,
    /// The outbox the host made for this run (`KITH_RUN_OUTBOX`) and its meta file: removed when the run does not happen.
    outbox: Option<PathBuf>,
    attached: Cell<bool>,
}

/// `--turn N --claim C`, the two flags straight after the agent. `None` when the arguments are not a turn's.
fn flags(rest: &[String]) -> Option<Result<(i64, i64), String>> {
    if rest.first().map(String::as_str) != Some("--turn") {
        return None;
    }
    let num = |name: &str, at: usize| {
        rest.get(at)
            .filter(|_| rest.get(at - 1).is_some_and(|f| f == name))
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| *n > 0)
    };
    Some(match (num("--turn", 1), num("--claim", 3)) {
        (Some(t), Some(c)) if rest.len() == 4 => Ok((t, c)),
        _ => Err("usage: kith kernel launch --exec AGENT --turn N --claim C   (the turn as JSON on stdin)".to_owned()),
    })
}

/// The owner is no person, whatever the core calls it.
fn who(p: Option<&str>) -> &str {
    match p {
        None | Some("" | "owner") => "",
        Some(p) => p,
    }
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The names `run_env` takes out of what the host's own environment carries into a job's run (and their `HQ_` twins).
const INHERITED: [&str; 10] = [
    "KITH_AGENT",
    "KITH_MODEL",
    "KITH_EFFORT",
    "KITH_HARNESS",
    "KITH_DISCORD_WEBHOOK_URL",
    "KITH_DISCORD_THREAD_ID",
    "KITH_BRIDGE_OUTBOX",
    "KITH_RUN_OUTBOX",
    "KITH_CALL_CHAT",
    "KITH_CALL_KEY",
];

/// The local time as `kith-sched`'s `clock()` read it, to the minute (`KITH_SCHED_NOW` fixes it for a test).
fn now_minute(env: &Env) -> String {
    let fixed = get(env, "KITH_SCHED_NOW").trim();
    let at = if fixed.is_empty() {
        chrono::Local::now().naive_local()
    } else if let Ok(secs) = fixed.parse::<f64>() {
        #[allow(clippy::cast_possible_truncation)]
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).naive_local())
            .unwrap_or_default()
    } else {
        chrono::NaiveDateTime::parse_from_str(fixed, "%Y-%m-%dT%H:%M")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(fixed, "%Y-%m-%dT%H:%M:%S"))
            .unwrap_or_default()
    };
    at.format("%Y-%m-%dT%H:%M").to_string()
}

/// Reads the turn from `spec` into `env` (the run's environment, as `run_env` built it) and returns the task text.
/// `Err` is the exit code, said on stderr.
fn build(
    agent: &str,
    turn: i64,
    spec: &Value,
    kernel: &Path,
    env: &mut Env,
) -> Result<(String, String), i32> {
    let say = |code: i32, m: &str| {
        eprintln!("kith: sched turn: {m}");
        code
    };
    let prompt = text(spec, "prompt").ok_or_else(|| say(BAD, "the turn has no prompt"))?;
    let Some(job) = spec.get("job").filter(|j| j.is_object()) else {
        return Err(say(BAD, "the turn names no job"));
    };
    let Some(id) = text(job, "id") else {
        return Err(say(BAD, "the turn names no job"));
    };
    // `turn_job`: the settings fixed when the core fired the turn win over the job's current ones, and a turn with none
    // (fired before they were kept) takes the job's.
    let mut run = ["model", "effort", "harness", "person"].map(|k| (k, text(job, k)));
    if let Some(settings) = spec.get("settings").filter(|s| s.is_object()) {
        for (k, v) in &mut run {
            *v = text(settings, k);
        }
    }
    let [model, effort, harness, person] = run.map(|(_, v)| v);
    // The claim is swept by the turn's person: a run as another one would be missed by that person's revoke.
    if spec.get("person").is_some()
        && who(person) != who(spec.get("person").and_then(Value::as_str))
    {
        return Err(say(
            REFUSED,
            &format!(
                "the job runs as {person:?}, not as the turn's person {:?}",
                spec.get("person")
            ),
        ));
    }
    let why = format!("core turn {turn}");
    let cron = text(job, "cron").unwrap_or_default();
    let task = format!(
        "{prompt}\n\n(Scheduled job \"{id}\" of {agent}, cron `{cron}`, {why}. No one is watching this session: finish the task and \
         deliver the result with {}/bin/kith-send. If there is nothing worth sending, send nothing.)",
        kernel.display()
    );
    let header = format!("# {} {agent} {id} ({why})\n", now_minute(env));
    for k in INHERITED {
        env.remove(k);
        env.remove(&format!("HQ_{}", &k[5..]));
    }
    // The run's origin, built by the host (the call key and chat, or an outbox, or nothing). It goes in
    // after what is taken out of the environment and before what the turn sets itself, so nothing it names can replace those.
    if let Some(origin) = spec.get("origin").and_then(Value::as_object) {
        for (k, v) in origin {
            if matches!(k.as_str(), "KITH_PERSON" | "HQ_PERSON") {
                continue;
            }
            if let Some(v) = v.as_str().filter(|v| !v.is_empty()) {
                env.insert(k.clone(), v.to_owned());
            }
        }
    }
    // The test clock is the host's, not the run's; the claim is this process's to attach, so the engine and a nested kith never
    // see it.
    for k in ["SCHED_NOW", "TURN_CLAIM", "TURN_CONVERSATION"] {
        env.remove(&format!("KITH_{k}"));
        env.remove(&format!("HQ_{k}"));
    }
    for (k, v) in [
        ("KITH_VIA", "schedule"),
        ("KITH_SCHED_JOB", id),
        ("KITH_LAUNCH_KIND", "schedule"),
        ("KITH_LAUNCH_LABEL", id),
    ] {
        env.insert(k.to_owned(), v.to_owned());
    }
    // Unset: the agent's mode, else kith's default.
    let mode = get(env, "KITH_SCHED_PERMISSION_MODE").to_owned();
    if !mode.is_empty() {
        env.insert("KITH_PERMISSION_MODE".to_owned(), mode);
    }
    env.insert("KITH_BACKUP_CHAIN".to_owned(), "on".to_owned());
    // None: whoever started this (the host's own environment: the owner).
    if let Some(p) = person {
        env.insert("KITH_PERSON".to_owned(), p.to_owned());
        env.insert("HQ_PERSON".to_owned(), p.to_owned());
    }
    for (k, v) in [
        ("KITH_MODEL", model),
        ("KITH_EFFORT", effort),
        ("KITH_HARNESS", harness),
    ] {
        if let Some(v) = v {
            env.insert(k.to_owned(), v.to_owned());
        }
    }
    sync_env(env);
    Ok((task, header))
}

impl Sched {
    /// `--turn N --claim C` after the agent: reads the turn from stdin and makes the run's environment in `env`. Returns the turn
    /// and the arguments the one-shot gets (`--`, then the task: it is a task even when it starts like a flag). `Ok(None)`: not a
    /// turn's arguments. Moves the process to the home folder, where `kith-sched` ran its jobs.
    pub fn prepare(
        agent: &str,
        rest: &[String],
        kernel: &Path,
        env: &mut Env,
    ) -> Result<Option<(Self, Vec<String>)>, i32> {
        let (turn, claim) = match flags(rest) {
            None => return Ok(None),
            Some(Err(m)) => {
                eprintln!("kith: {m}");
                return Err(BAD);
            }
            Some(Ok(t)) => t,
        };
        let mut raw = String::new();
        let spec: Value = std::io::stdin()
            .read_to_string(&mut raw)
            .ok()
            .and_then(|_| serde_json::from_str(&raw).ok())
            .filter(Value::is_object)
            .ok_or_else(|| {
                eprintln!("kith: sched turn: the turn's JSON is not on stdin");
                BAD
            })?;
        // The host made an outbox for this run (named in the spec's origin): a turn refused before the attach is a run that did
        // not happen, so it is removed on every way out here too.
        let outbox = spec
            .get("origin")
            .and_then(|o| text(o, "KITH_RUN_OUTBOX"))
            .map(PathBuf::from);
        let (task, header) = match build(agent, turn, &spec, kernel, env) {
            Ok(x) => x,
            Err(code) => {
                remove_outbox(outbox.as_deref());
                return Err(code);
            }
        };
        let home = get(env, "HOME").to_owned();
        if !home.is_empty() && std::env::set_current_dir(&home).is_ok() {
            env.insert("PWD".to_owned(), home);
        }
        let sched = Self {
            claim,
            header,
            outbox,
            attached: Cell::new(false),
        };
        Ok(Some((sched, vec!["--".to_owned(), task])))
    }

    /// Attaches this process to the claim (once), then truncates the log (stdout) and writes its header: a launch that loses the
    /// attach leaves the winner's log as it is. 4: the claim is gone. 5: the core did not answer.
    pub fn attach(&self, taker: &Taker<'_>, pid: i64) -> Result<(), i32> {
        if self.attached.get() {
            return Ok(());
        }
        match taker.attach_run(self.claim, pid) {
            Ok(()) => {}
            Err(AttachErr::Gone(state)) => {
                eprintln!(
                    "kith: sched turn: claim {} is gone ({state}); nothing ran",
                    self.claim
                );
                return Err(GONE);
            }
            Err(AttachErr::Down(why)) => {
                eprintln!("kith: sched turn: {why} for claim {}", self.claim);
                return Err(NO_ANSWER);
            }
        }
        self.attached.set(true);
        if let Ok(fd) = std::io::stdout().as_fd().try_clone_to_owned() {
            // The log is a file the host opened without truncating; stdout and stderr share its offset.
            let mut log = std::fs::File::from(fd);
            if log
                .set_len(0)
                .and_then(|()| log.seek(SeekFrom::Start(0)))
                .is_err()
            {
                eprintln!("kith: sched turn: can't reset the log");
            }
            let _ = log.write_all(self.header.as_bytes());
        }
        Ok(())
    }

    /// The run ended with `code`: when it never attached, the outbox made for it is for a run that did not happen.
    pub fn finish(&self, code: i32) -> i32 {
        if !self.attached.get() {
            remove_outbox(self.outbox.as_deref());
        }
        code
    }

    /// Whether the claim is this process's yet: the account walk's attempts do not carry it.
    pub fn attached(&self) -> bool {
        self.attached.get()
    }
}

fn remove_outbox(outbox: Option<&Path>) {
    let Some(box_dir) = outbox else { return };
    let _ = std::fs::remove_dir_all(box_dir);
    let mut meta = box_dir.to_path_buf().into_os_string();
    meta.push(".json");
    let _ = std::fs::remove_file(meta);
}

/// The claim a run that is not a schedule's turn was handed (`KITH_TURN_CLAIM`), if it names one. A value that is not a claim id
/// is refused with [`TURN_CLAIM_EXIT`], before anything else is looked at.
pub fn env_claim(env: &Env) -> Result<Option<i64>, i32> {
    let raw = get(env, "KITH_TURN_CLAIM").trim();
    if raw.is_empty() {
        // The conversation of a claim that is not there names nothing: refused, nothing is started.
        if !get(env, "KITH_TURN_CONVERSATION").trim().is_empty() {
            eprintln!(
                "kith: {TURN_CLAIM_REFUSED}: KITH_TURN_CONVERSATION names no turn claim; nothing was started"
            );
            return Err(TURN_CLAIM_EXIT);
        }
        return Ok(None);
    }
    match raw.parse::<i64>() {
        Ok(c) if c > 0 => Ok(Some(c)),
        _ => {
            eprintln!("kith: {TURN_CLAIM_REFUSED}: KITH_TURN_CLAIM \"{raw}\" is not a claim id");
            Err(TURN_CLAIM_EXIT)
        }
    }
}

/// The claim a run that is not a schedule's turn was handed, attached here when the run has no claim step of its own to attach it
/// (another engine, an account walk, a Claude run that continues the folder's session and so names none). `Some(exit)`: it could
/// not be attached; nothing was started. The exit is [`TURN_CLAIM_EXIT`] for every refusal, as Claude's own claim step has it: a
/// host's chat turn reads one code for "the turn is no longer this run's" whatever engine ran. (A schedule's turn, whose spec has
/// its own contract with the host, keeps `run-turn`'s 4 and 5.)
pub fn attach_env_claim(env: &Env, taker: &Taker<'_>, pid: i64) -> Option<i32> {
    let claim = match env_claim(env) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(code) => return Some(code),
    };
    let conversation = Some(get(env, "KITH_TURN_CONVERSATION").trim()).filter(|c| !c.is_empty());
    match taker.attach_run_in(claim, pid, conversation) {
        Ok(()) => None,
        Err(AttachErr::Gone(s) | AttachErr::Down(s)) => {
            eprintln!("kith: {TURN_CLAIM_REFUSED}: claim {claim}: {s}; nothing was started");
            Some(TURN_CLAIM_EXIT)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    fn built(spec: &Value, env: &mut Env) -> Result<(String, String), i32> {
        build("house", 7, spec, Path::new("/k"), env)
    }

    #[test]
    fn the_flags_are_the_two_numbers_and_nothing_else() {
        assert!(flags(&v(&["x"])).is_none());
        assert_eq!(
            flags(&v(&["--turn", "7", "--claim", "9"]))
                .unwrap()
                .unwrap(),
            (7, 9)
        );
        assert!(flags(&v(&["--turn", "7"])).unwrap().is_err());
        assert!(
            flags(&v(&["--turn", "7", "--claim", "0"]))
                .unwrap()
                .is_err()
        );
        assert!(
            flags(&v(&["--turn", "7", "--claim", "9", "x"]))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn the_task_and_the_run_env_are_the_ones_run_env_built() {
        let spec = json!({"prompt": "do a", "settings": {"model": "m2"}, "person": null,
            "job": {"id": "a", "cron": "0 8 * * *", "model": "m1", "effort": "high"}});
        let mut env = Env::new();
        for (k, val) in [
            ("KITH_AGENT", "x"),
            ("KITH_EFFORT", "low"),
            ("KITH_SCHED_PERMISSION_MODE", "plan"),
            ("KITH_TURN_CLAIM", "9"),
            ("KITH_SCHED_NOW", "2026-11-04T08:00"),
        ] {
            env.insert(k.to_owned(), val.to_owned());
        }
        let (task, header) = built(&spec, &mut env).unwrap();
        assert_eq!(
            task,
            "do a\n\n(Scheduled job \"a\" of house, cron `0 8 * * *`, core turn 7. No one is watching this session: finish the task and deliver the result with /k/bin/kith-send. If there is nothing worth sending, send nothing.)"
        );
        assert_eq!(header, "# 2026-11-04T08:00 house a (core turn 7)\n");
        assert_eq!(get(&env, "KITH_MODEL"), "m2");
        // settings on the turn: the job's own effort is not carried over, nor the host's.
        assert_eq!(get(&env, "KITH_EFFORT"), "");
        assert_eq!(get(&env, "KITH_VIA"), "schedule");
        assert_eq!(get(&env, "KITH_PERMISSION_MODE"), "plan");
        assert_eq!(get(&env, "KITH_BACKUP_CHAIN"), "on");
        for gone in [
            "KITH_AGENT",
            "KITH_TURN_CLAIM",
            "HQ_TURN_CLAIM",
            "KITH_SCHED_NOW",
        ] {
            assert!(!env.contains_key(gone), "{gone}");
        }
    }

    #[test]
    fn no_settings_takes_the_jobs_current_ones_and_a_guest_gets_its_person_and_origin() {
        let spec = json!({"prompt": "p", "settings": null, "person": "sam",
            "job": {"id": "a", "cron": "c", "model": "m1", "harness": "codex", "person": "sam"},
            "origin": {"KITH_RUN_OUTBOX": "/o/run", "KITH_CALL_KEY": ""}});
        let mut env = Env::new();
        built(&spec, &mut env).unwrap();
        assert_eq!(get(&env, "KITH_MODEL"), "m1");
        assert_eq!(get(&env, "KITH_HARNESS"), "codex");
        assert_eq!(get(&env, "KITH_PERSON"), "sam");
        assert_eq!(get(&env, "HQ_PERSON"), "sam");
        assert_eq!(get(&env, "KITH_RUN_OUTBOX"), "/o/run");
        assert!(!env.contains_key("KITH_CALL_KEY"));
    }

    #[test]
    fn the_origin_cannot_name_the_person_or_the_claim_or_the_kind_of_run() {
        let spec = json!({"prompt": "p", "settings": null, "person": null,
            "job": {"id": "a", "cron": "c"},
            "origin": {"KITH_PERSON": "sam", "HQ_PERSON": "sam", "KITH_TURN_CLAIM": "5", "KITH_VIA": "terminal"}});
        let mut env = Env::new();
        built(&spec, &mut env).unwrap();
        for k in ["KITH_PERSON", "HQ_PERSON", "KITH_TURN_CLAIM"] {
            assert!(!env.contains_key(k), "{k}");
        }
        assert_eq!(get(&env, "KITH_VIA"), "schedule");
    }

    #[test]
    fn a_turn_conversation_needs_a_claim_and_a_bad_claim_is_still_refused() {
        let mut env = Env::new();
        assert_eq!(env_claim(&env), Ok(None));
        env.insert("KITH_TURN_CONVERSATION".to_owned(), "tg:7#house".to_owned());
        assert_eq!(env_claim(&env), Err(TURN_CLAIM_EXIT));
        env.insert("KITH_TURN_CLAIM".to_owned(), "41".to_owned());
        assert_eq!(env_claim(&env), Ok(Some(41)));
        env.insert("KITH_TURN_CLAIM".to_owned(), "soon".to_owned());
        assert_eq!(env_claim(&env), Err(TURN_CLAIM_EXIT));
    }

    #[test]
    fn a_job_of_another_person_than_the_turns_is_refused() {
        let job = json!({"id": "a", "cron": "c"});
        let mut env = Env::new();
        let spec = json!({"prompt": "p", "settings": null, "person": "sam", "job": job});
        assert_eq!(built(&spec, &mut env).unwrap_err(), REFUSED);
        let spec =
            json!({"prompt": "p", "settings": {"person": "sam"}, "person": "owner", "job": job});
        assert_eq!(built(&spec, &mut env).unwrap_err(), REFUSED);
        // The owner, said as null, as "owner" or not said at all.
        for spec in [
            json!({"prompt": "p", "settings": null, "person": null, "job": job}),
            json!({"prompt": "p", "settings": null, "person": "owner", "job": job}),
            json!({"prompt": "p", "settings": null, "job": job}),
        ] {
            assert!(built(&spec, &mut env).is_ok());
        }
        assert_eq!(
            built(&json!({"prompt": "", "job": job}), &mut env).unwrap_err(),
            BAD
        );
    }
}
