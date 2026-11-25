//! What the tests of `authority`, `mcp` and `exec` share: a home in a temp folder, a fake core, and an environment for it.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::session::Env;
use super::{Ctx, Packages, Sandbox, ServeDir};
use crate::threads::core::{Core, CoreErr};

/// A throwaway home that goes away with the test: `<t>/home/.kith` is the machine folder, `<t>/kith/agents` the agents,
/// `<t>/kernel` the kernel folder.
pub(super) struct Home(pub PathBuf);

impl Home {
    pub fn new(name: &str) -> Home {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kith-mcp-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // the temp folder may sit behind a link (macOS): the paths below compare real paths
        let dir = std::fs::canonicalize(dir).unwrap();
        Home(dir)
    }

    pub fn auth(&self) -> PathBuf {
        self.0.join("home/.kith/secrets/auth.json")
    }

    pub fn kernel(&self) -> PathBuf {
        self.0.join("kernel")
    }

    pub fn agents(&self) -> PathBuf {
        self.0.join("kith/agents")
    }

    pub fn write(&self, rel: &str, text: &str) {
        let f = self.0.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }

    /// An agent with a package that needs these services.
    pub fn agent(&self, name: &str, needs: &[&str]) {
        self.write(
            &format!("kith/agents/{name}/role/agent.md"),
            &format!("---\nname: {name}\n---\n"),
        );
        self.write(
            &format!("kith/agents/{name}/role/capabilities.yaml"),
            &format!("needs: [{}]\n", needs.join(", ")),
        );
    }

    pub fn tables(&self, v: &Value) {
        self.write("home/.kith/secrets/auth.json", &v.to_string());
    }

    /// The environment of a run on this home.
    pub fn env(&self) -> Env {
        let t = &self.0;
        [
            ("HOME", t.join("home")),
            ("KITH_STATE", t.join("home/.kith")),
            ("KITH_HOME", t.join("kith")),
            ("KITH_AGENTS", t.join("kith/agents")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_string_lossy().into_owned()))
        .chain([("PATH".to_owned(), "/usr/bin:/bin".to_owned())])
        .collect()
    }

    pub fn ctx(&self) -> Ctx {
        Ctx {
            auth: self.auth(),
            agents: self.agents(),
            kernel: self.kernel(),
            person: None,
            now_ms: 1_800_000_000_000.0,
            sandbox: Sandbox::Fixed(false),
            serve: ServeDir::Fixed(self.0.join("serve")),
            packages: Packages::Disk,
        }
    }

    /// A shared service of the kernel.
    pub fn service(&self, name: &str, spec: &Value) {
        self.write(&format!("kernel/services/{name}.json"), &spec.to_string());
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A core that answers what it is told and remembers what it was asked.
pub(super) struct Fake {
    pub calls: Mutex<Vec<(String, Value)>>,
    answer: Mutex<Result<Value, (bool, String)>>,
}

impl Fake {
    pub fn answering(v: Value) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Ok(v)),
        })
    }

    /// No answer (`true`: refused, `false`: nothing answered) with this reason.
    pub fn failing(refused: bool, why: &str) -> Arc<Fake> {
        Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(Err((refused, why.to_owned()))),
        })
    }

    pub fn asked(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Core for Fake {
    fn call(
        &self,
        method: &str,
        params: &Value,
        _timeout: Option<Duration>,
    ) -> Result<Value, CoreErr> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_owned(), params.clone()));
        match &*self.answer.lock().unwrap() {
            Ok(v) => Ok(v.clone()),
            Err((true, why)) => Err(CoreErr::Refused(why.clone())),
            Err((false, why)) => Err(CoreErr::NoAnswer(why.clone())),
        }
    }
}

/// What `kith kernel cutover step authority` leaves in auth.json.
pub(super) const NOTE: &str = r#"{"kith_tombstone":1,"module":"authority","message":"this data moved to the Kith core (module authority); the old reader must not use this file","rollback":"kith rollback authority"}"#;

/// The tables of a small home: an owner, a guest with a row on `echo`, a profile of a service that needs one secret.
pub(super) fn tables() -> Value {
    json!({
        "profiles": {"p1": {"TOKEN": "env:SVC_TOKEN"}},
        "grants": {"echo": {"svca": "p1"}},
        "env": {},
        "people": {"kai": {"owner": true, "name": "Kai"}, "sam": {"name": "Sam"}},
        "presets": {},
        "members": {"echo": {"sam": {}}},
        "invites": {},
        "retired": {},
        "once": {},
    })
}

/// A service that is an MCP server needing one secret, `svca`.
pub(super) fn svca() -> Value {
    json!({
        "kind": "mcp",
        "secrets": ["TOKEN"],
        "mcp": {"command": "kith", "args": ["svc", "$KITH_AGENT", "~/x"], "env": {"WHO": "agent-$KITH_AGENT"}}
    })
}

pub(super) fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
