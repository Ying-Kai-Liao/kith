//! `run-chain`: a non-interactive one-shot over `pick`'s candidates (`run_chain` in `kernel/lib/kith_accounts.py`). The
//! first candidate that can run runs; exit 4 (it can't run: not logged in) or a usage-limit message moves to the next, and
//! the limit also marks the account out. Each attempt is a whole child command the caller names (`kith AGENT [--account ID]
//! ARGS...`), so it claims its session and starts the harness as any run does. Output is held until the attempt ends, so
//! a failed attempt's stdout is never shown: only the last one's is.

use super::change::{self, Change};
use super::{Ctx, clock, in_family, pick_from, state};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// A failed run whose short output says the account is out of usage (a long output that mentions limits is a task).
pub fn limited(code: i32, text: &str) -> bool {
    code != 0 && text.chars().count() < 4000 && limit_re().is_match(text)
}

fn limit_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)you've hit your .{0,30}limit|usage limit|weekly limit|rate.?limit")
            .expect("LIMIT_RE is a valid pattern")
    })
}

/// What one attempt printed and how it ended.
struct Attempt {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Runs `argv` with `KITH_CHAIN_CHILD=1` added, stdin as it is, stdout and stderr held.
fn attempt(argv: &[String]) -> std::io::Result<Attempt> {
    let o = Command::new(&argv[0])
        .args(&argv[1..])
        .env("KITH_CHAIN_CHILD", "1")
        .stdin(Stdio::inherit())
        .output()?;
    Ok(Attempt {
        code: exit_code(o.status),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    })
}

/// A process a signal ended reads as Python's negative return code taken as an exit status (`sys.exit(-9)` is 247).
#[cfg(unix)]
fn exit_code(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    s.code()
        .unwrap_or_else(|| (-s.signal().unwrap_or(0)) & 0xff)
}

#[cfg(not(unix))]
fn exit_code(s: std::process::ExitStatus) -> i32 {
    s.code().unwrap_or(1)
}

/// The walk. `kith` is the child program; each attempt is `kith AGENT [--account ID] ARGS...`. Returns the exit code of the
/// last attempt (4 and a line on `err` when the state can't be read or a mark can't be made).
pub fn run(
    ctx: &Ctx<'_>,
    agent: &str,
    agent_md: &Path,
    kith: &Path,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let state = match change::read_state(&ctx.machine) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(err, "kith: {e}");
            return 4;
        }
    };
    let s = super::settings::read(ctx);
    let p = pick_from(
        ctx,
        &s,
        agent,
        Some(agent_md),
        false,
        clock::now(),
        &state::Fixed(state),
    );
    let todo = &p.candidates;
    let (mut code, mut last) = (4, String::new());
    for (n, entry) in todo.iter().enumerate() {
        // The first attempt of its own harness's family is the plain run (it picks the same account); any other is named.
        let plain = n == 0 && in_family(entry, &p.harness);
        let mut cmd = vec![kith.to_string_lossy().into_owned(), agent.to_owned()];
        if !plain {
            cmd.extend(["--account".to_owned(), entry.clone()]);
        }
        cmd.extend(args.iter().cloned());
        let a = match attempt(&cmd) {
            Ok(a) => a,
            Err(e) => {
                let _ = writeln!(err, "kith: can't run {}: {e}", cmd[0]);
                return 1;
            }
        };
        let _ = err.write_all(a.stderr.as_bytes());
        let seen = format!("{}{}", a.stdout, a.stderr);
        (code, last) = (a.code, a.stdout);
        let more = n + 1 < todo.len();
        if code == 4 && more {
            // this account can't run (not logged in): the next one
            let _ = writeln!(err, "kith: {entry} can't run; trying {}", todo[n + 1]);
            continue;
        }
        if limited(code, &seen) {
            let c = Change::Out {
                id: entry.clone(),
                until: None,
                reason: "rate_limit".to_owned(),
                extend_only: true,
            };
            if let Err(e) = change::make(&ctx.machine, &c, change::now_secs()) {
                let _ = writeln!(err, "kith: {e}");
                return 4;
            }
            if more {
                let _ = writeln!(
                    err,
                    "kith: {entry} is out of usage; moving to {}",
                    todo[n + 1]
                );
                continue;
            }
        }
        break;
    }
    let _ = out.write_all(last.as_bytes());
    let _ = out.flush();
    code
}

#[cfg(all(test, unix))]
mod tests {
    use super::limited;

    #[test]
    fn a_short_failure_naming_a_limit_is_limited() {
        assert!(limited(1, "You've hit your session limit"));
        assert!(limited(1, "Rate-limit exceeded"));
        assert!(limited(2, "usage limit reached"));
        assert!(!limited(0, "usage limit"));
        assert!(!limited(1, "boom"));
        assert!(!limited(1, &format!("usage limit {}", "x".repeat(4000))));
    }
}
