//! A guest run's kernel tools answered by the core (M2(c)): a run minted by the owner socket reads recall, memory search
//! and project-id through run.sock, as the identity its token names, with a real daemon, real sockets and a fake kernel.
//! A revoke in the middle of a run drops its output; `run.resolve` is the host's question for the cards that stay on
//! host.sock.

use super::*;
use std::os::unix::fs::PermissionsExt as _;

/// A kernel folder whose three tools print who they were run as. `kith` sleeps when `kith.slow` exists, and says it
/// started by making `kith.started`.
fn fake_kernel(root: &Path) -> std::path::PathBuf {
    let bin = root.join("kernel-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tool = |name: &str, body: &str| {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let who = "$KITH_PERSON:$KITH_AGENT:${KITH_ROOM-}:${KITH_ROOM_MEMBERS-}:${KITH_PROJECT-}:${KITH_RUN_TOKEN-unset}:${KITH_CORE_RUN_SOCKET-unset}";
    tool(
        "kith",
        &format!(
            "if [ -f \"$0.slow\" ]; then touch \"$0.started\"; sleep 4; fi\necho \"recall:{who}:$*\""
        ),
    );
    tool("kith-memory", &format!("echo \"memory:{who}:$*\""));
    tool("project-id", &format!("echo \"project:{who}:$*\""));
    bin.join("kith")
}

struct Rig {
    _t: tempfile::TempDir,
    p: kith_kernel::paths::Paths,
    sock: std::path::PathBuf,
    d: crate::daemon::Daemon,
    kernel: std::path::PathBuf,
    asks: AtomicU32,
}

fn rig() -> Rig {
    use crate::daemon::{Config, Daemon};
    let (t, m) = machine();
    let p = paths(&m);
    let kernel = fake_kernel(t.path());
    let d = Daemon::spawn_with(
        &p,
        Config {
            holder_on: false,
            ..Config::default()
        },
    );
    d.ready(Duration::from_secs(10)).expect("serving");
    Rig {
        sock: endpoint(&Layout::new(&m)),
        _t: t,
        p,
        d,
        kernel,
        asks: AtomicU32::new(0),
    }
}

impl Rig {
    fn owner(&self, method: &str, params: &Value) -> Value {
        match crate::reach(&self.p, method, params, false) {
            crate::Reached::Answered(v) => v,
            other => panic!("{method}: {other:?}"),
        }
    }

    fn mint(&self, person: &str, extra: &Value) -> (String, String) {
        let mut params =
            json!({"agent": "echo", "person": person, "conversation": "t1", "project": "alpha"});
        for (k, v) in extra.as_object().into_iter().flatten() {
            params[k] = v.clone();
        }
        let v = self.owner("run.mint", &params);
        (
            v["token"].as_str().unwrap().to_owned(),
            v["runId"].as_str().unwrap().to_owned(),
        )
    }

    /// One request from a shell bound as the run. `between` runs after the request was released and before the answer is read.
    fn ask(
        &self,
        run: &(String, String),
        method: &str,
        args: &[&str],
        between: impl FnOnce(),
    ) -> Value {
        let go = self
            ._t
            .path()
            .join(format!("go-{}", self.asks.fetch_add(1, Ordering::SeqCst)));
        let mut sh = shell(
            &self.sock,
            &Tree::Child,
            &json!({"token": run.0, "method": method, "args": args}),
            &go,
        );
        let _ = self.owner("run.bind", &json!({"runId": run.1, "pid": sh.id()}));
        std::fs::write(&go, "go").unwrap();
        between();
        let out = lines(&mut sh);
        let _ = sh.wait();
        out[0].clone()
    }
}

#[test]
fn recall_memory_and_project_id_run_as_the_tokens_identity_through_run_sock() {
    let r = rig();
    let _kernel = crate::runtools::use_kernel(Some(r.kernel.clone()));
    r.owner("people.addPerson", &json!({"id": "sam"}));
    let run = r.mint("sam", &json!({}));
    // The words are the run's; who asks is the token's. No token or socket name reaches the tool.
    let got = r.ask(&run, "recall", &["zebra", "--json"], || {});
    assert_eq!(got["ok"], true, "{got}");
    let out = got["stdout"].as_str().unwrap();
    assert!(
        out.starts_with("recall:sam:echo::::unset:unset:recall ")
            && out.contains("zebra")
            && out.contains("--json"),
        "{out}"
    );
    let got = r.ask(&run, "memorySearch", &["zebra"], || {});
    assert_eq!(
        got["stdout"], "memory:sam:echo:::alpha:unset:unset:search zebra\n",
        "{got}"
    );
    let got = r.ask(&run, "projectId", &["--info", "alpha"], || {});
    assert_eq!(
        got["stdout"], "project:sam:echo::::unset:unset:--info alpha\n",
        "{got}"
    );
    // The project is the token's own: another one is refused before anything runs.
    let got = r.ask(&run, "projectId", &["--info", "beta"], || {});
    assert_eq!(
        got["error"],
        "only this run's own project is available here"
    );
    // A flag that names a caller is not an argument.
    let got = r.ask(&run, "recall", &["zebra", "--person", "ann"], || {});
    assert_eq!(got["ok"], false, "{got}");
    drop(r.d);
}

#[test]
fn a_revoke_in_the_middle_of_a_recall_drops_its_output() {
    let r = rig();
    let _kernel = crate::runtools::use_kernel(Some(r.kernel.clone()));
    r.owner("people.addPerson", &json!({"id": "sam"}));
    let slow = r.kernel.clone();
    std::fs::write(format!("{}.slow", slow.display()), "").unwrap();
    let run = r.mint("sam", &json!({}));
    let started = format!("{}.started", slow.display());
    let got = r.ask(&run, "recall", &["zebra"], || {
        // The tool is running; the suspension commits now.
        let begun = std::time::Instant::now();
        while !Path::new(&started).exists() && begun.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(Path::new(&started).exists(), "the tool started");
        r.owner("people.suspend", &json!({"id": "sam"}));
    });
    assert!(not_authorized(&got), "{got}");
    assert!(
        !got.to_string().contains("recall:"),
        "no output of the tool: {got}"
    );
    drop(r.d);
}

#[test]
fn a_room_persons_basis_reaches_the_tool_and_a_revoked_member_ends_the_token() {
    let r = rig();
    let _kernel = crate::runtools::use_kernel(Some(r.kernel.clone()));
    r.owner("people.addPerson", &json!({"id": "sam"}));
    r.owner("people.addPerson", &json!({"id": "ann"}));
    let run = r.mint(
        "room-grp1-9",
        &json!({"room": "grp1", "members": ["sam", "ann"]}),
    );
    let got = r.ask(&run, "recall", &["zebra"], || {});
    let out = got["stdout"].as_str().unwrap();
    assert!(
        out.starts_with("recall:room-grp1-9:echo:grp1:sam,ann::unset:unset:recall "),
        "{got}"
    );
    // A room person without a basis, or of another room, is never minted for.
    for extra in [
        json!({"room": "grp1"}),
        json!({"room": "grp2", "members": ["sam"]}),
    ] {
        let params = json!({"agent": "echo", "person": "room-grp1-9", "conversation": "t1",
                            "room": extra["room"], "members": extra.get("members").cloned().unwrap_or(json!([]))});
        assert!(matches!(
            crate::reach(&r.p, "run.mint", &params, false),
            crate::Reached::Refused(Error::NotAllowed(_))
        ));
    }
    r.owner("people.suspend", &json!({"id": "ann"}));
    let got = r.ask(&run, "recall", &["zebra"], || {});
    assert!(not_authorized(&got), "{got}");
    drop(r.d);
}

#[test]
fn the_host_asks_the_core_who_a_token_is_and_a_revoke_or_a_stranger_gets_no() {
    let r = rig();
    let _kernel = crate::runtools::use_kernel(Some(r.kernel.clone()));
    r.owner("people.addPerson", &json!({"id": "sam"}));
    let run = r.mint("sam", &json!({"project": "alpha"}));
    let me = std::process::id();
    // Unbound: nobody matches.
    assert!(matches!(
        crate::reach(
            &r.p,
            "run.resolve",
            &json!({"token": run.0, "pid": me}),
            false
        ),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    // Bound to a live process; asked from that process, it is the run's identity.
    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    assert_eq!(
        r.owner("run.bind", &json!({"runId": run.1, "pid": sleeper.id()}))["bound"],
        true
    );
    let who = r.owner("run.resolve", &json!({"token": run.0, "pid": sleeper.id()}));
    assert_eq!(
        (
            who["person"].as_str(),
            who["agent"].as_str(),
            who["runId"].as_str(),
            who["conversation"].as_str()
        ),
        (Some("sam"), Some("echo"), Some(run.1.as_str()), Some("t1")),
        "{who}"
    );
    // Not the run's process (this test process is not below the sleeper), a wrong token, and the revoked person.
    assert!(matches!(
        crate::reach(
            &r.p,
            "run.resolve",
            &json!({"token": run.0, "pid": me}),
            false
        ),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    assert!(matches!(
        crate::reach(
            &r.p,
            "run.resolve",
            &json!({"token": "nope", "pid": sleeper.id()}),
            false
        ),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    r.owner("people.suspend", &json!({"id": "sam"}));
    assert!(matches!(
        crate::reach(
            &r.p,
            "run.resolve",
            &json!({"token": run.0, "pid": sleeper.id()}),
            false
        ),
        crate::Reached::Refused(Error::NotAllowed(_))
    ));
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    drop(r.d);
}

#[test]
fn a_daemon_restart_ends_every_token() {
    let r = rig();
    let _kernel = crate::runtools::use_kernel(Some(r.kernel.clone()));
    r.owner("people.addPerson", &json!({"id": "sam"}));
    let run = r.mint("sam", &json!({}));
    assert_eq!(r.ask(&run, "recall", &["zebra"], || {})["ok"], true);
    let (p, sock, kernel) = (r.p.clone(), r.sock.clone(), r.kernel.clone());
    drop(r.d);
    let d = crate::daemon::Daemon::spawn_with(
        &p,
        crate::daemon::Config {
            holder_on: false,
            ..crate::daemon::Config::default()
        },
    );
    d.ready(Duration::from_secs(10)).expect("serving again");
    let _ = kernel;
    let go = r._t.path().join("go-restart");
    let mut sh = shell(
        &sock,
        &Tree::Child,
        &json!({"token": run.0, "method": "recall", "args": ["zebra"]}),
        &go,
    );
    std::fs::write(&go, "go").unwrap();
    let got = lines(&mut sh);
    let _ = sh.wait();
    assert!(
        not_authorized(&got[0]),
        "a token of the earlier daemon is nothing: {got:?}"
    );
    drop(d);
}
