//! `kith people grant|revoke|suspend|undo|requests` on a switched Mac, on real processes (M2 owner-access CLI). A real
//! `kith serve` (and the core daemon it starts), the real `kith kernel cutover` verbs and the real `kith-people` (python) from
//! a copy of the kernel. Throwaway home in /tmp, `env_clear` for every process, no launchd: the two plists are files the
//! gate reads. The cloud is off, so a suspend is made here only (a person with no Kith account).
//!
//! Not stubbed: the step, the host's `enforcePersonAccess`, `revokePerson` and `accessRequests`, the core's people.grant /
//! revoke / listPeople, `kith kernel core-call`, `kith-people`. Not covered here (unit-tested in the host): a running turn
//! ending when its agent is taken away.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const KITH: &str = env!("CARGO_BIN_EXE_kith");

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// The built holder next to `kith`: built on demand when only the host's tests were built.
fn ensure_holder() {
    let beside = Path::new(KITH).parent().unwrap().join("kith-holder");
    if !beside.is_file() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let built = Command::new(cargo)
            .args(["build", "--manifest-path", concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"), "-p", "kith-holder"])
            .status()
            .unwrap();
        assert!(built.success() && beside.is_file(), "kith-holder is not built");
    }
}

/// A host, as a process: killed when the test ends any way.
struct Host(Child, u16);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Lab {
    dir: PathBuf,
    home: PathBuf,
    kith_home: PathBuf,
    serve: PathBuf,
    kernel: PathBuf,
    machine: PathBuf,
}

impl Lab {
    fn new() -> Lab {
        ensure_holder();
        let dir = PathBuf::from("/tmp").join(format!("kpe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let (home, kernel) = (dir.join("home"), dir.join("k"));
        let kith_home = home.join("kith");
        let machine = home.join(".config/kith");
        for d in [&kernel.join("bin"), &machine.join("cli"), &kith_home.join("state/live/terminal")] {
            std::fs::create_dir_all(d).unwrap();
        }
        for agent in ["mel", "house"] {
            let role = kith_home.join("agents").join(agent).join("role");
            std::fs::create_dir_all(&role).unwrap();
            std::fs::write(role.join("agent.md"), format!("---\nname: {agent}\ndescription: test\n---\nbody\n"))
                .unwrap();
        }
        let real = repo().join("kernel");
        std::os::unix::fs::symlink(real.join("lib"), kernel.join("lib")).unwrap();
        for b in ["kith-people", "kith-auth", "kith-serve"] {
            std::fs::copy(real.join("bin").join(b), kernel.join("bin").join(b)).unwrap();
        }
        // The host's entry point (`KITH_KERNEL_BIN`): it finds kith-auth beside it. Nothing here runs it.
        let entry = kernel.join("bin/kith");
        std::fs::write(&entry, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&entry, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&kernel, machine.join("cli/current")).unwrap();
        let plists = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&plists).unwrap();
        let words = |w: &[&str]| {
            let s = format!("<string>{}</string>", w.join("</string><string>"));
            format!("<plist><dict><key>ProgramArguments</key><array>{s}</array></dict></plist>")
        };
        std::fs::write(plists.join("dev.kith.serve.plist"), words(&[KITH, "serve", "--port", "1"])).unwrap();
        let sched = machine.join("cli/current/bin/kith-sched");
        std::fs::write(plists.join("dev.kith.sched.plist"), words(&[sched.to_str().unwrap(), "tick"])).unwrap();
        // The owner, Sam with one row (so there is a person to take it from), and a bot of Sam's.
        let auth = json!({"people": {"kai": {"owner": true}, "sam": {"name": "Sam"}, "bot-s": {"name": "Scout", "bot_of": "sam"}},
                          "members": {"house": {"sam": {"preset": "member"}}}});
        std::fs::write(machine.join("auth.json"), format!("{auth}\n")).unwrap();
        Lab { serve: dir.join("serve"), dir, home, kith_home, kernel, machine }
    }

    fn env(&self, port: Option<u16>) -> Vec<(String, String)> {
        let p = |x: &Path| x.to_string_lossy().into_owned();
        let mut env = vec![
            ("HOME", p(&self.home)),
            ("KITH_HOME", p(&self.kith_home)),
            ("KITH_SERVE_HOME", p(&self.serve)),
            ("KITH_KERNEL", p(&self.kernel)),
            ("KITH_BIN", KITH.to_owned()),
            ("KITH_KERNEL_BIN", p(&self.kernel.join("bin/kith"))),
            ("KITH_AUTH", p(&self.machine.join("auth.json"))),
            ("PATH", "/usr/bin:/bin".to_owned()),
            ("TZ", "UTC".to_owned()),
            ("PYTHONDONTWRITEBYTECODE", "1".to_owned()),
            ("KITH_SERVE_CLOUD", "off".to_owned()),
            // Never this Mac's master key item.
            ("KITH_SECRETS_KEYSTORE_ACCOUNT", format!("test-{}", self.dir.display().to_string().replace('/', "-"))),
            ("CLAUDE_CONFIG_DIR", p(&self.dir.join("claude-config"))),
            ("KITH_SERVE_MAX_RUNNING_AGENTS", "64".to_owned()),
            ("KITH_SERVE_MIN_FREE_MEMORY_PERCENT", "0".to_owned()),
            ("KITH_SERVE_MAX_SWAP_USED_GB", "0".to_owned()),
            ("RUST_LOG", "kith=info".to_owned()),
        ];
        if let Some(port) = port {
            env.push(("KITH_SERVE_DIR", p(&self.serve)));
            env.push(("KITH_SERVE_PORT", port.to_string()));
        }
        env.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    fn kith(&self, args: &[&str]) -> Command {
        let mut c = Command::new(KITH);
        c.env_clear().envs(self.env(None)).args(args);
        c
    }

    fn step_ok(&self, module: &str) {
        let out = self.kith(&["kernel", "cutover", "step", module]).output().unwrap();
        assert!(out.status.success(), "step {module}: {}", text(&out));
    }

    fn start_host(&self) -> Host {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // A port picked here is only free for a moment: a host that finds it taken exits, and this tries another.
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let log = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("host.log")).unwrap();
            let mut host = Host(
                self.kith(&["serve", "--bind", "127.0.0.1", "--port", &port.to_string()])
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
                port,
            );
            while host.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
                let token = std::fs::read_to_string(self.serve.join("token")).unwrap_or_default();
                if !token.trim().is_empty() && answers(port, token.trim()) {
                    return host;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(Instant::now() < deadline, "host didn't start:\n{}", self.host_log());
        }
    }

    fn host_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("host.log")).unwrap_or_default()
    }

    /// `kith-people ARGS --json` as one JSON answer and its exit code.
    fn people(&self, port: Option<u16>, args: &[&str]) -> (i32, Value) {
        let out = Command::new("python3")
            .env_clear()
            .envs(self.env(port))
            .arg(self.kernel.join("bin/kith-people"))
            .arg("people")
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {}", text(&out)));
        (out.status.code().unwrap_or(-1), v)
    }

    /// The core's list of people, as the CLI reads them; None until the daemon answers.
    fn people_rows(&self, port: Option<u16>) -> Option<Vec<Value>> {
        let params = json!({"viaSwitch": true, "kernel": self.kernel, "home": self.home, "env": {}});
        let mut c = Command::new(KITH);
        c.env_clear()
            .envs(self.env(port))
            .args(["kernel", "core-call", "people.listPeople", "--params-stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), params.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        if !out.status.success() {
            return None;
        }
        let v: Value = serde_json::from_slice(&out.stdout).ok()?;
        v["rows"].as_array().cloned()
    }

    fn person(&self, port: u16, id: &str) -> Value {
        self.people_rows(Some(port)).unwrap().into_iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("no {id}"))
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        // The holder the host started is detached: ended with the test.
        if let Ok(out) =
            Command::new(KITH).env_clear().envs(self.env(None)).args(["kernel", "core-status", "--json"]).output()
            && let Some(pid) =
                serde_json::from_slice::<Value>(&out.stdout).ok().and_then(|v| v["holder"]["pid"].as_u64())
        {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Whether the host at `port` answers an API call with something other than a refusal of the token (as the other e2e's probe).
fn answers(port: u16, token: &str) -> bool {
    use std::io::{Read as _, Write as _};
    let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return false };
    s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let req = format!(
        "POST /api/accessRequests HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    let mut got = String::new();
    s.write_all(req.as_bytes()).is_ok() && s.read_to_string(&mut got).is_ok() && got.starts_with("HTTP/1.1 200")
}

fn agents_of(p: &Value) -> Vec<String> {
    p["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["via"] == "own")
        .map(|a| a["agent"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_people_verbs_on_a_switched_mac_write_the_core_and_ask_the_host() {
    let lab = Lab::new();
    lab.step_ok("claims");
    lab.step_ok("authority");
    let host = lab.start_host();
    let port = host.1;
    assert!(std::fs::read_to_string(lab.machine.join("auth.json")).unwrap().contains("kith_tombstone"));
    assert_eq!(agents_of(&lab.person(port, "sam")), ["house"], "the step imported Sam's row");

    // grant: a row in the core, then the host's enforce (which refreshed the people list's copy).
    let (rc, granted) = lab.people(Some(port), &["grant", "sam", "mel", "--preset", "member"]);
    assert_eq!(rc, 0, "{granted}\n{}", lab.host_log());
    assert_eq!((granted["ok"].clone(), granted["enforced"].clone()), (json!(true), json!(true)));
    let mut agents = agents_of(&lab.person(port, "sam"));
    agents.sort();
    assert_eq!(agents, ["house", "mel"]);

    // revoke takes it away again; a second revoke is as good as the first.
    for _ in 0..2 {
        let (rc, gone) = lab.people(Some(port), &["revoke", "sam", "mel"]);
        assert_eq!(rc, 0, "{gone}\n{}", lab.host_log());
    }
    assert_eq!(agents_of(&lab.person(port, "sam")), ["house"]);

    // The owner, a bot, someone nobody invited and `*` are refused before anything is written.
    for who in ["kai", "bot-s", "ghost", "*"] {
        let (rc, no) = lab.people(Some(port), &["grant", who, "mel"]);
        assert!(rc == 1 || rc == 2, "{who}: {no}");
        assert_eq!(no["ok"], false, "{who}: {no}");
    }
    assert_eq!(agents_of(&lab.person(port, "sam")), ["house"]);

    // suspend goes through the host (cloud off: here only) and shows as removed access; undo gives it back.
    let (rc, s) = lab.people(Some(port), &["suspend", "sam"]);
    assert_eq!(rc, 0, "{s}\n{}", lab.host_log());
    assert!(lab.person(port, "sam")["revoked"].as_bool().unwrap_or(false), "{}", lab.person(port, "sam"));
    let (rc, u) = lab.people(Some(port), &["undo", "sam"]);
    assert_eq!(rc, 0, "{u}");
    assert!(!lab.person(port, "sam")["revoked"].as_bool().unwrap_or(false));
    assert_eq!(agents_of(&lab.person(port, "sam")), ["house"], "undo gives back exactly what they had");

    // requests: the host's list, empty here.
    let (rc, r) = lab.people(Some(port), &["requests"]);
    assert_eq!(rc, 0, "{r}");
    assert_eq!(r["requests"], json!([]));

    // Without the host nothing is done to a person, and the line says so (exit 3: no core daemon answers).
    drop(host);
    std::thread::sleep(Duration::from_secs(2));
    let (rc, down) = lab.people(Some(port), &["grant", "sam", "mel"]);
    assert_eq!((rc, down["ok"].clone()), (3, json!(false)), "no core daemon: nothing ran: {down}");
    // A revoke is written by the embedded core with no daemon, and then the host cannot be asked: it says the rows changed
    // and to run the same line again (fail closed), and the row is gone all the same.
    let (rc, partial) = lab.people(Some(port), &["revoke", "sam", "house"]);
    assert_eq!(rc, 1, "{partial}");
    assert!(partial["error"].as_str().unwrap().contains("the rows were changed"), "{partial}");
    let row = lab.people_rows(Some(port)).unwrap().into_iter().find(|r| r["id"] == "sam").unwrap();
    assert!(agents_of(&row).is_empty(), "{row}");
}
