//! A guest's doors (`kernel/lib/kith_door.py`): fixed commands kith-permit runs for a member in a sandbox of their own,
//! such as video's render. Each running door leaves a claim; `kith door sweep` kills the doors of the listed people,
//! every door whose supervisor is gone (a kith-permit or host killed with -9), and every door its person may no longer
//! run (D1). The host sweeps when it cuts someone off or narrows their access, at start, and every minute.
//!
//! On a switched Mac the person's own doors are rows in the core (`doors.*`, `kernel/lib/kith_door_core.py`): the sweep's
//! python asks the core, stops the boxes of the rows it answers, and confirms them. The kill stays in python (it needs the
//! `libproc`, `sandbox_check` and process-environment scan of `kith_door.py`), so this module still runs `kith door sweep`.
//! What it adds is the answer's `unknown`: the core (or kith-auth) did not answer in time, so no door was judged. That is
//! not "revoked": the running doors are left alone and the pass is asked again after [`RETRY`], not after [`EVERY`].

use std::time::Duration;

const EVERY: Duration = Duration::from_secs(60);
/// After a pass that could not judge a door (a slow or absent core).
const RETRY: Duration = Duration::from_secs(10);

/// What one `kith door sweep` said.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// How many doors it ended.
    pub ended: usize,
    /// No answer about at least one door: the sweep did not run, failed, timed out, or said `unknown`.
    pub unknown: bool,
}

/// The answer of `kith door sweep`: `{"ended": [claims], "unknown": bool}`. Text that is not that is unknown (the python
/// before this answer had no `unknown`, and said nothing about a check that got no answer).
fn read(stdout: &[u8]) -> Swept {
    let said: serde_json::Value = serde_json::from_slice(stdout).unwrap_or_default();
    Swept {
        ended: said["ended"].as_array().map_or(0, Vec::len),
        unknown: said["ended"].as_array().is_none() || said["unknown"].as_bool().unwrap_or(false),
    }
}

/// How long to wait before the next pass of the loop.
fn wait_after(swept: &Swept) -> Duration {
    if swept.unknown { RETRY } else { EVERY }
}

/// `kith door sweep [--person P]...`: `persons` lose every door; the rest are checked again against the core or kith-auth.
/// A named sweep that comes back unknown is not repeated: the python ends the named people's boxes by their claim files
/// without asking the core (and a revoke's own transaction swept their rows), and a repeat after an undo would end a door
/// the restored person started since. The loop's next pass confirms the rows.
pub async fn sweep(persons: &[String]) -> Swept {
    let Some(bin) = crate::remote::account::bin().await.map(|b| b.with_file_name("kith-door")) else {
        return Swept::default();
    };
    let mut args = vec!["sweep"];
    for p in persons {
        args.extend(["--person", p.as_str()]);
    }
    match crate::channels::hq::output(&bin, &args).await {
        Some(out) if out.status.success() => {
            let swept = read(&out.stdout);
            if swept.ended > 0 {
                tracing::info!(ended = swept.ended, "ended doors");
            }
            if swept.unknown {
                tracing::warn!("kith door sweep could not judge a door (the core did not answer); asking again soon");
            }
            swept
        }
        Some(out) => {
            tracing::warn!(why = %String::from_utf8_lossy(&out.stderr).trim(), "kith door sweep failed");
            Swept { ended: 0, unknown: true }
        }
        None => {
            tracing::debug!("kith door sweep didn't run");
            Swept { ended: 0, unknown: true }
        }
    }
}

/// At start (doors a killed host or kith-permit left), then every minute; after a pass that could not judge a door, in
/// [`RETRY`] instead.
pub async fn sweep_loop() {
    loop {
        let swept = sweep(&[]).await;
        tokio::time::sleep(wait_after(&swept)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_counts_the_ended_doors() {
        assert_eq!(read(br#"{"ended": ["a", "b"], "unknown": false}"#), Swept { ended: 2, unknown: false });
        // the python from before `unknown`: the same words, nothing unknown
        assert_eq!(read(br#"{"ended": []}"#), Swept { ended: 0, unknown: false });
    }

    #[test]
    fn a_pass_that_could_not_judge_a_door_says_unknown() {
        assert_eq!(read(br#"{"ended": [], "unknown": true}"#), Swept { ended: 0, unknown: true });
        // a named person's door still ended while the recheck of the rest got no answer
        assert_eq!(read(br#"{"ended": ["a"], "unknown": true}"#), Swept { ended: 1, unknown: true });
    }

    #[test]
    fn no_usable_answer_is_unknown_never_all_clear() {
        for said in [&b""[..], b"not json", b"[]", br#"{"unknown": false}"#, br#"{"ended": 3}"#] {
            assert!(read(said).unknown, "{}", String::from_utf8_lossy(said));
        }
    }

    #[test]
    fn an_unknown_pass_is_asked_again_sooner_but_never_ends_a_door_by_itself() {
        assert_eq!(wait_after(&Swept { ended: 0, unknown: true }), RETRY);
        assert_eq!(wait_after(&Swept { ended: 0, unknown: false }), EVERY);
        assert!(RETRY < EVERY);
    }
}
