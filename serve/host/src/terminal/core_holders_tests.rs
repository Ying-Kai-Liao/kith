//! The Writer-mode readers (`core_holders.rs`) against a scripted core: each moved reader with the module switched and the
//! core up, switched and the core down, and not switched with the core up. The core's rows themselves are tested on a real
//! core (`core/runtime`); this tests what the host asks and what it does with each answer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::core_front::Answer;
use crate::terminal::core_claims::tests::{Fake, register};
use crate::terminal::registry::{pid_runs, proc_start};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const GRACE: Duration = Duration::from_secs(1);
const LIVE: [&str; 3] = ["claimed", "running", "ending"];

#[derive(Clone, Debug)]
struct C {
    id: i64,
    conv: String,
    state: String,
    kind: String,
    person: String,
    pid: Option<u64>,
    started: Option<String>,
    instance: Option<String>,
    agent: Option<String>,
}

#[derive(Default)]
struct World {
    claims: Vec<C>,
    log: Vec<String>,
    /// `cutover.mayServe` says `ok`.
    switched: bool,
    /// The gate answers `ok: false` with a warning.
    gate_warning: bool,
    down: bool,
    /// `settle` never ends anything (a group that still has processes).
    stuck: bool,
    /// The next `hostclaim.stop` finds the claim already ended (released between the read and the stop).
    released_first: bool,
}

impl World {
    fn live(&self, conv: &str) -> Option<usize> {
        self.claims.iter().position(|c| c.conv == conv && LIVE.contains(&c.state.as_str()))
    }

    fn shown(c: &C) -> Value {
        json!({"claimId": c.id, "conversation": c.conv, "state": c.state, "kind": c.kind, "person": c.person,
               "pid": c.pid, "started": c.started, "holderInstance": c.instance, "agent": c.agent})
    }

    fn answer(&mut self, method: &str, p: &Value) -> Answer {
        self.log.push(method.to_owned());
        if self.down {
            return Answer::Down { why: "the core is down".into(), lost: false };
        }
        let conv = p["conversation"].as_str().unwrap_or_default().to_owned();
        let id = p["claimId"].as_i64().unwrap_or_default();
        Answer::Done(match method {
            "cutover.mayServe" if self.gate_warning => json!({"ok": false, "warning": "stepping"}),
            "cutover.mayServe" => json!({"ok": self.switched}),
            "hostclaim.show" => {
                let c = if p["claimId"].is_i64() {
                    self.claims.iter().find(|c| c.id == id)
                } else {
                    self.live(&conv).map(|i| &self.claims[i])
                };
                json!({"claim": c.map(Self::shown)})
            }
            "hostclaim.settle" => {
                let stuck = self.stuck;
                for c in &mut self.claims {
                    let gone = c.pid.and_then(|p| u32::try_from(p).ok()).is_some_and(|p| !pid_runs(p));
                    if !stuck && gone && LIVE.contains(&c.state.as_str()) && c.state != "claimed" {
                        c.state = "ended".into();
                    }
                }
                json!({"ended": []})
            }
            "hostclaim.stop" => {
                if self.released_first {
                    self.released_first = false;
                    self.claims.iter_mut().filter(|c| c.id == id).for_each(|c| c.state = "ended".into());
                }
                match self.claims.iter_mut().find(|c| c.id == id && ["claimed", "running"].contains(&c.state.as_str())) {
                    Some(c) if c.pid.is_none() => {
                        c.state = "ended".into();
                        json!({"kill": null})
                    }
                    Some(c) => {
                        c.state = "ending".into();
                        json!({"kill": {"claimId": id, "pid": c.pid, "started": c.started}})
                    }
                    None => json!({"kill": null}),
                }
            }
            "claim.list" => {
                json!({"live": self.claims.iter().filter(|c| LIVE.contains(&c.state.as_str())).map(Self::shown).collect::<Vec<_>>(),
                       "recent": []})
            }
            "claim.sweepPerson" => {
                let who: Vec<String> = p["persons"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_owned()).collect();
                let mut kill = Vec::new();
                for c in self.claims.iter_mut().filter(|c| who.contains(&c.person) && ["claimed", "running"].contains(&c.state.as_str())) {
                    c.state = if c.pid.is_some() { "ending" } else { "ended" }.into();
                    if c.pid.is_some() {
                        kill.push(json!({"claimId": c.id, "pid": c.pid, "started": c.started}));
                    }
                }
                json!({"kill": kill})
            }
            other => return Answer::Refused(format!("the scripted core has no {other}")),
        })
    }
}

struct Rig {
    state: PathBuf,
    world: Arc<Mutex<World>>,
    kids: Mutex<Vec<u32>>,
}

impl Rig {
    /// A scripted core for a fresh state folder. `switch_file`: the module's switch file exists (a switched Mac).
    fn new(switched: bool, switch_file: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("kith-holders-{}", uuid::Uuid::new_v4()));
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let folders = Paths {
            home: dir.clone(),
            state: state.clone(),
            agents: dir.join("agents"),
            home_real: dir.clone(),
            state_real: state.clone(),
            agents_real: dir.join("agents"),
            machine: dir.join("machine"),
            secrets: dir.join("secrets"),
            logs: dir.join("logs"),
            cache: dir.join("cache"),
            store: dir.join("store"),
        };
        if switch_file {
            let marker = folders.machine.join("core").join("cutover");
            std::fs::create_dir_all(&marker).unwrap();
            std::fs::write(marker.join("claims.switch"), "1").unwrap();
        }
        let world = Arc::new(Mutex::new(World { switched, ..World::default() }));
        let w = world.clone();
        let fake: Fake = Arc::new(move |m, p| w.lock().unwrap().answer(m, p));
        register(&state, fake);
        crate::terminal::core_claims::enable(&state, folders.clone());
        Self { state, world, kids: Mutex::default() }
    }

    fn w(&self) -> std::sync::MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    fn log(&self) -> Vec<String> {
        self.w().log.clone()
    }

    /// A live process standing for a terminal or a turn's engine.
    fn proc(&self) -> (u32, String) {
        // A grandchild (its shell exits at once, init adopts it): not a child of this test process, which Stop would call the
        // host's own turn, and reaped by init once killed.
        let out = Command::new("sh")
            .args(["-c", "sleep 60 >/dev/null 2>&1 & echo $!"])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        self.kids.lock().unwrap().push(pid);
        (pid, proc_start(pid).expect("a start stamp"))
    }

    /// A process that has exited and been reaped.
    fn gone(&self) -> (u32, String) {
        let mut kid = Command::new("sleep").arg("60").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let (pid, started) = (kid.id(), proc_start(kid.id()).unwrap());
        kid.kill().unwrap();
        kid.wait().unwrap();
        (pid, started)
    }

    fn put(&self, conv: &str, kind: &str, person: &str, p: Option<(u32, String)>, instance: Option<&str>) -> i64 {
        let mut w = self.w();
        let id = i64::try_from(w.claims.len()).unwrap() + 1;
        w.claims.push(C {
            id,
            conv: conv.into(),
            state: "running".into(),
            kind: kind.into(),
            person: person.into(),
            pid: p.as_ref().map(|p| u64::from(p.0)),
            started: p.map(|p| p.1),
            instance: instance.map(str::to_owned),
            agent: None,
        });
        id
    }

    fn state_of(&self, id: i64) -> String {
        self.w().claims.iter().find(|c| c.id == id).unwrap().state.clone()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for pid in self.kids.lock().unwrap().iter() {
            // SAFETY: kill(2) on a child this rig started; a pid already gone answers ESRCH.
            unsafe { libc::kill(i32::try_from(*pid).unwrap(), libc::SIGKILL) };
        }
    }
}

fn file_claim(state: &Path, sid: &str) {
    let me = std::process::id();
    claim::take(state, sid, Kind::Terminal, me, false).unwrap().unwrap();
}

// MARK: the readers: switched or not, core up or down

#[test]
fn writer_reads_the_core_and_never_the_files() {
    let r = Rig::new(true, true);
    let (pid, started) = r.proc();
    r.put("s1", "terminal", "owner", Some((pid, started)), None);
    // A claim file nobody holds a core claim for is not consulted.
    file_claim(&r.state, "s2");
    let reader = Reader::new(&r.state);
    assert_eq!(reader.mode(), Mode::Writer);
    assert!(matches!(reader.read("s1"), Reading::By(c, Some(_)) if c.kind == Kind::Terminal && c.pid == pid));
    assert_eq!(reader.read("s2"), Reading::Free, "the file claim of s2 is not read in Writer mode");
}

#[test]
fn writer_with_the_core_down_reads_unreadable_never_free() {
    let r = Rig::new(true, true);
    r.w().down = true;
    let reader = Reader::new(&r.state);
    assert_eq!(reader.mode(), Mode::Held, "a switched Mac whose core does not answer holds");
    assert!(matches!(reader.read("s1"), Reading::Unreadable(_)));
    // The gate answers, the next row does not.
    r.w().down = false;
    let reader = Reader::new(&r.state);
    assert_eq!(reader.mode(), Mode::Writer);
    r.w().down = true;
    assert!(matches!(reader.read("s1"), Reading::Unreadable(_)), "a row that does not answer is not 'no holder'");
}

#[test]
fn an_unclear_gate_holds_and_a_missing_switch_file_with_the_gate_saying_writer_is_still_writer() {
    let r = Rig::new(true, false);
    // The core also records the switch: the file can be missing while the gate says ok.
    assert_eq!(Reader::new(&r.state).mode(), Mode::Writer);
    let r = Rig::new(false, true);
    r.w().gate_warning = true;
    assert_eq!(Reader::new(&r.state).mode(), Mode::Held);
}

#[test]
fn not_switched_the_files_decide_and_the_core_is_asked_only_the_gate() {
    let r = Rig::new(false, false);
    file_claim(&r.state, "s2");
    r.w().log.clear();
    let reader = Reader::new(&r.state);
    assert_eq!(reader.mode(), Mode::Files);
    assert!(matches!(reader.read("s2"), Reading::By(c, None) if c.kind == Kind::Terminal));
    assert!(r.log().iter().all(|m| m == "cutover.mayServe"), "{:?}", r.log());
}

#[test]
fn a_state_folder_with_no_core_of_its_own_is_files_and_asks_nothing() {
    let dir = std::env::temp_dir().join(format!("kith-holders-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    file_claim(&dir, "s1");
    let reader = Reader::new(&dir);
    assert_eq!(reader.mode(), Mode::Files);
    assert!(matches!(reader.read("s1"), Reading::By(..)));
}

#[test]
fn this_starts_own_owner_turn_is_no_holder_but_an_earlier_starts_is() {
    let r = Rig::new(true, true);
    let (pid, started) = r.proc();
    let id = r.put("s1", "turn", "owner", Some((pid, started.clone())), Some(own_instance()));
    let reader = Reader::new(&r.state);
    assert_eq!(reader.read("s1"), Reading::Free, "this start's own turn only keeps a terminal out");
    r.w().claims.iter_mut().find(|c| c.id == id).unwrap().instance = Some("kith-serve-host/1-old".into());
    assert!(matches!(reader.read("s1"), Reading::By(c, _) if c.kind == Kind::Other), "an older start's turn still writes");
    r.w().claims.iter_mut().find(|c| c.id == id).unwrap().instance = Some(super::super::core_claims::HOST_INSTANCE.into());
    assert!(matches!(reader.read("s1"), Reading::By(..)), "the shadow's fixed name is not this start's");
    // A person's turn of this start is a bot guest's holder, as the file claim of the bot is today.
    r.w().claims.iter_mut().find(|c| c.id == id).unwrap().person = "sam".into();
    r.w().claims.iter_mut().find(|c| c.id == id).unwrap().instance = Some(own_instance().into());
    assert!(matches!(reader.read("s1"), Reading::By(c, _) if c.kind == Kind::Bot));
}

#[test]
fn a_recorded_gone_claim_is_settled_free_unless_its_group_still_has_processes() {
    let r = Rig::new(true, true);
    r.put("s1", "terminal", "owner", Some(r.gone()), None);
    let reader = Reader::new(&r.state);
    assert_eq!(reader.read("s1"), Reading::Free, "settle ends it once the pid is confirmed gone");
    assert!(r.log().contains(&"hostclaim.settle".to_owned()));
    // The core keeps it live (its group still runs): the reader never calls it free.
    r.put("s2", "terminal", "owner", Some(r.gone()), None);
    r.w().stuck = true;
    assert!(matches!(reader.read("s2"), Reading::By(..)), "not free while the core still holds it");
}

// MARK: stop

#[test]
fn stop_ends_the_claim_it_read_by_id_and_only_when_the_core_shows_it_ended() {
    let r = Rig::new(true, true);
    let (pid, started) = r.proc();
    let id = r.put("s1", "terminal", "owner", Some((pid, started)), None);
    let got = claim::stop_holder(&r.state, "s1", GRACE);
    assert!(matches!(got, Stopped::Ended(_) | Stopped::StillEnding(_)), "{got:?}");
    assert!(r.log().contains(&"hostclaim.stop".to_owned()));
    assert!(!pid_runs(pid) || r.state_of(id) != "running", "the claimer was signalled");
    // Pressed twice: nothing is left to stop.
    let again = claim::stop_holder(&r.state, "s1", GRACE);
    assert!(matches!(again, Stopped::Nobody | Stopped::StillEnding(_)), "{again:?}");
}

#[test]
fn stop_of_a_claim_released_between_the_read_and_the_stop_signals_nothing() {
    let r = Rig::new(true, true);
    let (pid, started) = r.proc();
    r.put("s1", "terminal", "owner", Some((pid, started)), None);
    r.w().released_first = true;
    let got = claim::stop_holder(&r.state, "s1", GRACE);
    assert!(matches!(got, Stopped::Ended(_)), "{got:?}");
    assert!(pid_runs(pid), "the core gave no kill for a claim that was no longer live: the process is left alone");
}

#[test]
fn stop_with_the_core_down_is_unreadable_not_nobody() {
    let r = Rig::new(true, true);
    r.w().down = true;
    assert!(matches!(claim::stop_holder(&r.state, "s1", GRACE), Stopped::Unreadable(_)));
    // Files mode: the files path, as before.
    let f = Rig::new(false, false);
    assert!(matches!(claim::stop_holder(&f.state, "s1", GRACE), Stopped::Nobody));
}

#[test]
fn stop_never_signals_a_claim_whose_pid_was_reused() {
    let r = Rig::new(true, true);
    let (pid, _) = r.proc();
    r.put("s1", "terminal", "owner", Some((pid, "Thu Jan  1 00:00:00 1970".into())), None);
    let got = claim::stop_holder(&r.state, "s1", GRACE);
    assert!(!matches!(got, Stopped::Ended(_)), "{got:?}");
    assert!(pid_runs(pid));
}

#[test]
fn end_claimed_needs_only_the_cores_kill_list() {
    let r = Rig::new(true, true);
    let (pid, started) = r.proc();
    let ended = claim::end_claimed(&[(pid, Some(started))], GRACE);
    assert_eq!(ended, vec![pid]);
    // A claim with no recorded start is never signalled.
    let (pid2, _) = r.proc();
    assert!(claim::end_claimed(&[(pid2, None)], GRACE).is_empty());
    assert!(pid_runs(pid2));
}

// MARK: the take-over's mode read

#[test]
fn the_take_over_asks_the_core_only_in_writer_mode_and_refuses_a_gate_it_cannot_read() {
    let r = Rig::new(true, true);
    assert!(writer_folders(&r.state).unwrap().is_some());
    r.w().gate_warning = true;
    assert!(writer_folders(&r.state).is_err(), "an unclear gate: nothing is signalled");
    let f = Rig::new(false, false);
    assert!(writer_folders(&f.state).unwrap().is_none(), "not switched: the files path as today");
    let (pid, started) = r.proc();
    r.w().gate_warning = false;
    let id = r.put("s1", "terminal", "owner", Some((pid, started)), None);
    let folders = writer_folders(&r.state).unwrap().unwrap();
    assert_eq!(terminal_claim(&folders, "s1"), Ok(Some(id)));
    r.w().down = true;
    assert!(terminal_claim(&folders, "s1").is_err(), "a down core is not 'no claim'");
}

// MARK: the revoke sweep

fn test_hub(state: &Path) -> Arc<Hub> {
    let dir = std::env::temp_dir().join(format!("kith-holders-hub-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::Store::open(Path::new(":memory:")).unwrap();
    let hub = Hub::new(store, "t".into(), crate::remote::identity::Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
    hub.terminal.set_env(crate::terminal::Env { state: state.to_path_buf(), sessions: None });
    hub
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoke_sweep_in_writer_mode_ends_the_persons_claims_through_the_core() {
    let r = Rig::new(true, true);
    let (guest, g_start) = r.proc();
    let (owner, o_start) = r.proc();
    r.put("g1", "turn", "sam", Some((guest, g_start)), Some(own_instance()));
    r.put("o1", "terminal", "owner", Some((owner, o_start)), None);
    let hub = test_hub(&r.state);
    let got = sweep(&hub, &r.state, vec!["sam".into()], false).await;
    assert_eq!(got, Some(Swept::Done(vec![guest])), "{:?}", r.log());
    assert!(pid_runs(owner), "the owner's claim is never swept");
    assert!(r.log().contains(&"claim.sweepPerson".to_owned()));
    assert_eq!(owed_count(&hub), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sweep_the_core_did_not_take_is_owed_and_retried_and_never_reported_as_nothing() {
    let r = Rig::new(true, true);
    let (guest, g_start) = r.proc();
    r.put("g1", "turn", "sam", Some((guest, g_start)), Some(own_instance()));
    let hub = test_hub(&r.state);
    // The gate answers, the sweep does not.
    let w = r.world.clone();
    register(
        &r.state,
        Arc::new(move |m, p| {
            if m == "claim.sweepPerson" {
                w.lock().unwrap().log.push(m.to_owned());
                return Answer::Down { why: "gone".into(), lost: false };
            }
            w.lock().unwrap().answer(m, p)
        }),
    );
    let got = sweep(&hub, &r.state, vec!["sam".into()], false).await;
    assert!(matches!(got, Some(Swept::Owed(_))), "{got:?}");
    assert_eq!(owed_count(&hub), 1);
    assert!(pid_runs(guest), "nothing was ended");
    // The tick again, with the core answering: the owed sweep ends it.
    let w = r.world.clone();
    register(&r.state, Arc::new(move |m, p| w.lock().unwrap().answer(m, p)));
    retry_owed(&hub).await;
    assert_eq!(owed_count(&hub), 0);
    assert_eq!(r.w().claims[0].state, "ending");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owed_sweep_of_a_person_who_was_restored_kills_nothing() {
    let r = Rig::new(true, true);
    let (guest, g_start) = r.proc();
    r.put("g1", "turn", "sam", Some((guest, g_start)), Some(own_instance()));
    let hub = test_hub(&r.state);
    // Owed while sam was suspended; then the revoke was undone (nothing is kept for sam any more).
    hub.store
        .kv_set(OWED, &json!([{"persons": ["sam"], "narrow": false, "suspended": true}]).to_string())
        .unwrap();
    retry_owed(&hub).await;
    assert_eq!(owed_count(&hub), 0, "dropped");
    assert!(!r.log().contains(&"claim.sweepPerson".to_owned()), "the retry revoked nobody");
    assert!(pid_runs(guest));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_sweep_stops_only_the_claims_it_judged_by_id() {
    let r = Rig::new(true, true);
    let (guest, g_start) = r.proc();
    let (other, o_start) = r.proc();
    let id = r.put("g1", "turn", "sam", Some((guest, g_start)), Some(own_instance()));
    let kept = r.put("g2", "turn", "ann", Some((other, o_start)), Some(own_instance()));
    let hub = test_hub(&r.state);
    // No agent on the row: it cannot be judged and goes (the files path's rule).
    let got = sweep(&hub, &r.state, vec!["sam".into()], true).await;
    assert_eq!(got, Some(Swept::Done(vec![guest])));
    assert_eq!(r.state_of(id), "ending");
    assert_eq!(r.state_of(kept), "running", "another person's claim is not stopped");
    assert!(!r.log().contains(&"claim.sweepPerson".to_owned()), "a narrowing never revokes the person");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_sweeps_at_once_end_each_pid_once_and_the_files_mode_asks_nothing() {
    let r = Rig::new(true, true);
    let (guest, g_start) = r.proc();
    r.put("g1", "turn", "sam", Some((guest, g_start)), Some(own_instance()));
    let hub = test_hub(&r.state);
    let (a, b) = tokio::join!(
        sweep(&hub, &r.state, vec!["sam".into()], false),
        sweep(&hub, &r.state, vec!["sam".into()], false)
    );
    let ended: usize = [a, b].into_iter().flatten().map(|s| if let Swept::Done(p) = s { p.len() } else { 0 }).sum();
    assert_eq!(ended, 1, "the second finds nothing new");
    // Not switched: no sweep through the core (the caller runs the files code).
    let f = Rig::new(false, false);
    let hub = test_hub(&f.state);
    assert_eq!(sweep(&hub, &f.state, vec!["sam".into()], false).await, None);
}
