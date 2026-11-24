//! A reported detached run that leaves a process behind (R3a): the claim is `ending` with the report as its reason, and the
//! daemon's tick signals the leftover group so `end_gone` can end it. Child of `daemon_tests` for its helpers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

/// A dead leader whose group still has a member, the claim of a `source` turn reported `reason`. Answers (claim, turn, member).
#[cfg(unix)]
fn reported(d: &Daemon, source: &str, reason: &str) -> (i64, i64, u32) {
    use std::io::BufRead;
    let mut leader = std::process::Command::new("python3")
        .args([
            "-c",
            "import os,subprocess,time; os.setsid(); c=subprocess.Popen(['sleep','120']); print(c.pid, flush=True); time.sleep(1); os._exit(0)",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("python3");
    let pid = leader.id();
    let mut line = String::new();
    std::io::BufReader::new(leader.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("child pid");
    let member: u32 = line.trim().parse().expect("pid");
    let start = kith_kernel::procstart::start_stamp(pid).expect("stamp");
    let id = ending_claim(d, "a", crate::holder_link::HOST_INSTANCE, pid, &start);
    let turn = d
        .shared
        .with_core(|c| {
            c.conn().execute(
                "UPDATE claims SET kind = 'external', end_reason = ?2 WHERE claim_id = ?1",
                rusqlite::params![id, reason],
            )?;
            c.conn().execute(
                "UPDATE turns SET source = ?2, state = 'running' WHERE claim_id = ?1",
                rusqlite::params![id, source],
            )?;
            Ok(c.conn()
                .query_row("SELECT id FROM turns WHERE claim_id = ?1", [id], |r| {
                    r.get::<_, i64>(0)
                })?)
        })
        .expect("link");
    leader.wait().expect("leader exits");
    (id, turn, member)
}

#[cfg(unix)]
#[test]
fn a_done_report_that_left_its_group_alive_is_signalled_and_then_ends_the_claim_and_turn() {
    for source in ["wake", "schedule"] {
        let (_t, m) = machine();
        let d = serving(&paths(&m), quiet());
        let (id, turn, member) = reported(&d, source, "done");
        assert!(alive(member));
        assert_eq!(
            resend(&d).len(),
            1,
            "{source}: the leftover group is signalled"
        );
        until("the leftover to be gone", || !alive(member));
        d.shared
            .with_core(|c| crate::conversations::claims::end_gone(c, &crate::gate::System, 100))
            .expect("settle");
        assert_eq!(claim_state(&d, id), "ended", "{source}");
        let state: String = d
            .shared
            .with_core(|c| {
                Ok(c.conn()
                    .query_row("SELECT state FROM turns WHERE id = ?1", [turn], |r| {
                        r.get(0)
                    })?)
            })
            .expect("turn");
        assert_eq!(state, "done", "{source}");
        d.stop();
    }
}

#[cfg(unix)]
#[test]
fn a_done_report_on_a_claim_that_runs_no_detached_turn_is_still_not_signalled() {
    let (_t, m) = machine();
    let d = serving(&paths(&m), quiet());
    let (_id, _turn, member) = reported(&d, "app", "done");
    assert_eq!(resend(&d), Vec::<u32>::new());
    assert!(alive(member));
    signal(member, "KILL");
    d.stop();
}
