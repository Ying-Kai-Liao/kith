//! The wake calls against a real core (the daemon the host tests start), not a stub: the core checks every param of a row
//! strictly, so a call the host spells wrong fails here (lesson #1072). With the switch on no `kith-wake` starts.

use super::*;
use crate::core_front::{TestDaemon, serve_for_test};
use kith_kernel::paths::Paths;

struct Core {
    home: PathBuf,
    folders: Paths,
    daemon: Option<TestDaemon>,
}

impl Core {
    /// A core of its own, its state folder known to the host, and (when `switched`) the `wake` switch record in place.
    fn start(switched: bool) -> Self {
        // Short, for the core's socket; and with a `.kith` in it, or every home's state is the kernel checkout's.
        let home = PathBuf::from("/tmp").join(format!("kwc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(home.join(".kith")).expect("home");
        let folders = crate::kernel::folders(&home);
        if switched {
            // The record the host reads (a Windows core has no marker to write: `mark_wake_switched_for_test` is a no-op there),
            // then, on Unix, the record and its marker in core.db, before the daemon holds the core.
            let dir = folders.machine.join("core/cutover");
            std::fs::create_dir_all(&dir).expect("cutover dir");
            std::fs::write(dir.join("wake.switch"), "state=switched\nat=1\n").expect("switch record");
            kith_core::mark_wake_switched_for_test(&folders).expect("the wake switch");
        }
        let daemon = serve_for_test(&folders);
        crate::terminal::core_claims::enable(&folders.state, folders.clone());
        Self { home, folders, daemon: Some(daemon) }
    }

    /// The holder's first hello, which this holder-less core never gets: until it, every claim is answered `waiting`.
    fn reconciled(&self) {
        self.daemon.as_ref().expect("the core runs").reconciled();
    }

    fn state(&self) -> &Path {
        &self.folders.state
    }

    fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop();
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn ask<'a>(session: &'a str, id: &'a str, person: Option<&'a str>) -> Request<'a> {
    Request {
        session,
        id,
        agent: "builder",
        person,
        room: Some("room-1"),
        host_door: true,
        why: "a card was answered",
        text: "go on",
    }
}

#[test]
fn a_switched_queue_is_the_cores_and_starts_no_process() {
    let core = Core::start(true);
    let st = core.state();
    assert_eq!(enqueue(st, &ask("acp:c1:t", "card:1", Some("sam"))).unwrap(), Queued::New);
    assert_eq!(
        enqueue(st, &ask("acp:c1:t", "card:1", Some("sam"))).unwrap(),
        Queued::Duplicate,
        "rule 13 is the core's"
    );
    assert_eq!(status(st, "card:1").as_deref(), Some("queued"));
    assert!(has_items(st));
    assert_eq!(ls(st).len(), 1);
    assert_eq!(statuses(st).unwrap().get("card:1").map(String::as_str), Some("queued"));

    let turn = take_session(st, "acp:c1:t").unwrap().expect("a turn is owed");
    assert_eq!(
        (turn.session.as_str(), turn.person.as_deref(), turn.ids.as_slice()),
        ("acp:c1:t", Some("sam"), ["card:1".to_owned()].as_slice())
    );
    assert_eq!(statuses(st).unwrap().get("card:1").map(String::as_str), Some("running"));
    back(st, &turn.run, true);
    assert_eq!(status(st, "card:1").as_deref(), Some("queued"), "given back, owed again");
    assert!(take(st).unwrap().is_none(), "a held session waits before it is offered again");
    enqueue(st, &ask("acp:c9:t", "card:9", None)).unwrap();
    let other = take(st).unwrap().expect("another session is owed");
    done(st, &other.run, "answered");
    assert_eq!(status(st, "card:9").as_deref(), Some("answered"));
    withdraw(st, "acp:c1:t", Some("card:1"));
    assert_eq!(status(st, "card:1").as_deref(), Some("withdrawn"));

    // The list holds no answered ids: they are asked for by name, and an id the queue never saw is not there.
    let asked = vec!["card:1".to_owned(), "card:9".to_owned(), "card:nobody".to_owned()];
    let known = statuses_for(st, &asked).unwrap();
    assert_eq!(known.get("card:1").map(String::as_str), Some("withdrawn"));
    assert_eq!(known.get("card:9").map(String::as_str), Some("answered"));
    assert!(!known.contains_key("card:nobody"), "{known:?}");
    assert!(!has_items(st));
    assert!(take(st).unwrap().is_none());
    recover(st);

    assert_eq!(spawns(st), 0, "no kith-wake was started");
    assert!(!st.join("sessions/queue").exists(), "the folder is not the store");
}

#[test]
fn a_revoke_withdraws_what_a_person_is_owed_in_the_core() {
    let core = Core::start(true);
    let st = core.state();
    enqueue(st, &ask("acp:c1:t", "card:a", Some("sam"))).unwrap();
    enqueue(st, &ask("acp:c2:t", "card:b", Some("sam"))).unwrap();
    enqueue(st, &ask("acp:c3:t", "card:c", Some("lee"))).unwrap();
    withdraw_person(st, &["sam".to_owned()]);
    assert_eq!(status(st, "card:a").as_deref(), Some("withdrawn"));
    assert_eq!(status(st, "card:b").as_deref(), Some("withdrawn"));
    assert_eq!(status(st, "card:c").as_deref(), Some("queued"), "only that person's");
    // Stop of one session keeps the id so a late copy is dropped.
    withdraw(st, "acp:c3:t", Some("card:c"));
    assert_eq!(status(st, "card:c").as_deref(), Some("withdrawn"));
    assert_eq!(enqueue(st, &ask("acp:c3:t", "card:c", Some("lee"))).unwrap(), Queued::Duplicate);
    assert_eq!(spawns(st), 0);
}

#[test]
fn a_switched_queue_whose_core_is_down_fails_closed_and_never_reaches_the_files() {
    let mut core = Core::start(true);
    core.stop();
    let st = core.state().to_path_buf();
    assert!(enqueue(&st, &ask("acp:c1:t", "card:1", None)).is_err());
    assert!(take(&st).is_err());
    assert_eq!(status(&st, "card:1"), None);
    assert!(!has_items(&st));
    assert!(statuses(&st).is_none(), "an unread queue is not an empty one");
    assert!(statuses_for(&st, &["card:1".to_owned()]).is_none());
    withdraw_person(&st, &["sam".to_owned()]);
    done(&st, "run-1", "answered");
    assert_eq!(spawns(&st), 0);
    assert!(!st.join("sessions/queue").exists(), "no file was written");
}

#[test]
fn a_tombstone_with_no_core_to_name_refuses() {
    let state = std::env::temp_dir().join(format!("kwt-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    std::fs::create_dir_all(state.join("sessions/queue")).unwrap();
    std::fs::write(state.join("sessions/queue/.tombstone"), "").unwrap();
    assert!(enqueue(&state, &ask("acp:c1:t", "card:1", None)).is_err());
    assert!(!has_items(&state));
    assert_eq!(spawns(&state), 0);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
#[cfg_attr(
    windows,
    ignore = "spawns the Python kith-wake as the files writer (os error 193); returns with retire-python P2 (wake port, kith-wake deleted)"
)]
fn with_the_switch_off_the_files_are_still_the_store() {
    let core = Core::start(false);
    let st = core.state();
    assert_eq!(enqueue(st, &ask("acp:c1:t", "card:1", Some("sam"))).unwrap(), Queued::New);
    assert!(spawns(st) > 0, "kith-wake ran");
    assert!(has_items(st));
    assert_eq!(status(st, "card:1").as_deref(), Some("queued"));
    // The core was never asked: it holds nothing.
    assert!(matches!(via_core::ls(&core.folders), Some(items) if items.is_empty()));
}

#[test]
#[cfg_attr(
    windows,
    ignore = "spawns the Python kith-wake as the files writer (os error 193); returns with retire-python P2 (wake port, kith-wake deleted)"
)]
fn a_switch_still_stepping_is_the_files_and_a_damaged_one_is_the_cores() {
    let core = Core::start(false);
    let st = core.state();
    let record = core.folders.machine.join("core/cutover/wake.switch");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "state=stepping\nat=1\n").unwrap();
    assert_eq!(enqueue(st, &ask("acp:c1:t", "card:1", None)).unwrap(), Queued::New);
    assert_eq!(spawns(st), 1, "Python is still the writer while stepping");
    assert!(matches!(via_core::ls(&core.folders), Some(items) if items.is_empty()));
    // As the core reads it: no trimming (a padded line is no state line: damaged), the last state wins. Each of these is Core
    // mode, so no Python starts; and a core with no marker behind the record refuses the write (WK6), so the caller fails
    // closed and keeps the request.
    for (n, text) in
        ["state=rolling-back\n", "{}", " state=stepping\n", "state=stepping\nstate=switched\n"].into_iter().enumerate()
    {
        std::fs::write(&record, text).unwrap();
        let before = spawns(st);
        let (session, id) = (format!("acp:d{n}:t"), format!("card:d{n}"));
        let said = enqueue(st, &ask(&session, &id, None));
        assert!(said.is_err_and(|e| e.to_string().contains("not switched")), "{text:?}: the core refuses");
        assert_eq!(spawns(st), before, "{text:?} is the core's");
    }
}

#[test]
fn the_clock_the_pull_and_the_claim_of_a_wake_turn_run_through_the_cores_rows() {
    use crate::terminal::core_writer::{CoreWriter, RunClaim};
    let core = Core::start(true);
    let st = core.state();
    let owed = Request {
        session: "sess-run-1",
        id: "ask:job-1/1",
        agent: "builder",
        person: None,
        room: None,
        host_door: false,
        why: "an answer came",
        text: "go on",
    };
    assert_eq!(enqueue(st, &owed).unwrap(), Queued::New);
    // The clock: the core takes what is due as one run and one turn.
    let ticked = tick(st).expect("the host's tick is answered");
    assert_eq!(ticked["taken"].as_array().map(Vec::len), Some(1), "{ticked}");
    // The pull: the wake source lists it with the door, the request ids and the note (the marker names the run).
    let writer = CoreWriter::new(core.folders.clone());
    let turns = writer.queued_from(&["wake"]).expect("the wake turns are listed");
    assert_eq!(turns.len(), 1, "{turns:?}");
    let t = &turns[0];
    assert_eq!(
        (t["source"].as_str(), t["door"].as_str(), t["engine"].as_str()),
        (Some("wake"), Some("run"), Some("claude"))
    );
    assert_eq!(t["requestIds"], serde_json::json!(["ask:job-1/1"]));
    let note = t["note"].as_str().unwrap();
    let run = note.strip_prefix("[kith wake ").and_then(|n| n.split_once(']')).map(|(r, _)| r).expect("the marker");
    assert!(writer.queued().unwrap().is_empty(), "a host that runs only cron jobs never sees a wake's turn");
    // The turn the queue's item says it is, for the door's checks.
    let turn = turn_of(st, run, note).expect("the item of that run");
    assert_eq!(
        (turn.session.as_str(), turn.agent.as_str(), turn.ids.as_slice()),
        ("sess-run-1", "builder", ["ask:job-1/1".to_owned()].as_slice())
    );
    // The claim: widened to a wake's turn, on the session the wake resumes.
    let (person, id) = (t["person"].as_str().unwrap(), t["turn"].as_i64().unwrap());
    core.reconciled();
    let claimed = writer.claim_run_as("wakeRun", "sess-run-1", person, id);
    assert!(matches!(claimed, RunClaim::Claimed(_)), "{claimed:?}");
    assert_eq!(spawns(st), 0, "no kith-wake was started");
}
