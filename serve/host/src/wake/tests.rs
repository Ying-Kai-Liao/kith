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
        let daemon = serve_for_test(&folders);
        crate::terminal::core_claims::enable(&folders.state, folders.clone());
        if switched {
            let dir = folders.machine.join("core/cutover");
            std::fs::create_dir_all(&dir).expect("cutover dir");
            std::fs::write(dir.join("wake.switch"), "state=switched\nat=1\n").expect("switch record");
        }
        Self { home, folders, daemon: Some(daemon) }
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
fn a_switch_still_stepping_is_the_files_and_a_damaged_one_is_the_cores() {
    let core = Core::start(false);
    let st = core.state();
    let record = core.folders.machine.join("core/cutover/wake.switch");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "state=stepping\nat=1\n").unwrap();
    assert_eq!(enqueue(st, &ask("acp:c1:t", "card:1", None)).unwrap(), Queued::New);
    assert_eq!(spawns(st), 1, "Python is still the writer while stepping");
    assert!(matches!(via_core::ls(&core.folders), Some(items) if items.is_empty()));
    for (n, text) in ["state=rolling-back\n", "{}"].into_iter().enumerate() {
        std::fs::write(&record, text).unwrap();
        let before = spawns(st);
        let (session, id) = (format!("acp:d{n}:t"), format!("card:d{n}"));
        assert_eq!(enqueue(st, &ask(&session, &id, None)).unwrap(), Queued::New);
        assert_eq!(spawns(st), before, "{text:?} is the core's");
    }
}
