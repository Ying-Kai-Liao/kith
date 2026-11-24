//! What `kith-auth mcp-config` and `kith-auth exec` share, as one run's view (retire-python P1-15): the auth tables, the
//! environment the Python tool keeps in `os.environ` (it edits it as it goes, and later reads see the edits), the service
//! specs and what a service's settings come to. The grants and the secret values are in `grants.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::{Ctx, Die, R, Rules, die, get, pyjson, truthy};
use crate::paths::Paths;

/// `os.environ`.
pub type Env = HashMap<String, String>;

/// One run of `mcp-config` or `exec` over one snapshot of the tables.
pub(super) struct Session<'a> {
    /// The context, with the run's person taken from `env` (`acting()` reads `KITH_PERSON` there).
    pub c: Ctx,
    pub auth: &'a Value,
    /// `os.environ`: edited by [`Self::settings`] as `kith-auth`'s `settings()` edits it.
    pub env: Env,
    pub paths: Paths,
}

impl<'a> Session<'a> {
    pub fn new(c: &Ctx, auth: &'a Value, env: Env) -> Self {
        let home = env
            .get("HOME")
            .filter(|h| !h.is_empty())
            .map_or_else(crate::paths::process_home, PathBuf::from);
        let paths = crate::paths::paths(&home, &c.kernel, &|k: &str| env.get(k).cloned());
        let mut c = c.clone();
        c.person = env.get("KITH_PERSON").filter(|p| !p.is_empty()).cloned();
        Session {
            c,
            auth,
            env,
            paths,
        }
    }

    /// `acting(auth)`.
    pub fn acting(&self) -> String {
        Rules::new(&self.c, self.auth).acting()
    }

    /// `kith-auth`'s `service_file`: `<kernel>/services/<name>.json`, else this Mac's services folder (the host's connectors),
    /// else one the agent brings in its own package's `services/`. Other agents' folders are not read.
    pub fn service_file(&self, name: &str, agent: Option<&str>) -> Option<PathBuf> {
        let file = format!("{name}.json");
        let own = [
            self.c.kernel.join("services"),
            self.paths.machine.join("services"),
        ]
        .into_iter()
        .map(|d| d.join(&file))
        .find(|f| f.is_file());
        own.or_else(|| {
            let agent = agent.filter(|a| self.c.agent_exists(a))?;
            Some(self.c.package_dir(agent).join("services").join(&file)).filter(|f| f.is_file())
        })
    }

    /// `service(name, agent)`: the spec, or the refusal of an unknown service.
    pub fn service(&self, name: &str, agent: Option<&str>) -> R<Value> {
        match self.service_file(name, agent) {
            Some(f) => read_spec(&f),
            None => die(format!(
                "no service '{name}' (kernel/services/, or an agent's services/)"
            )),
        }
    }

    /// `needs(agent)`: the services capabilities.yaml's `needs: [a, b]` names.
    pub fn needs(&self, agent: &str) -> Vec<String> {
        static NEEDS: OnceLock<Regex> = OnceLock::new();
        let re = NEEDS.get_or_init(|| {
            Regex::new(r"(?m)^needs:\s*\[([^\]]*)\]").expect("the pattern is a constant")
        });
        let file = self.c.package_dir(agent).join("capabilities.yaml");
        let Ok(text) = std::fs::read_to_string(file) else {
            return vec![];
        };
        re.captures(&text).map_or_else(Vec::new, |m| {
            m[1].split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
    }

    /// `expand(text)`: `~` and `$VAR` in a string; anything that is no string stays as it is.
    pub fn expand(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(expandvars(&self.env, &expanduser(&self.env, s))),
            other => other.clone(),
        }
    }

    /// `settings(auth, agent, svc)`: the service's env with this agent's own overrides on top. `$KITH_AGENT` in a value is this
    /// agent, `$KITH_BROWSER` and `$KITH_BROWSER_PROFILE` its browser profile. A browser service that can't drive the profile's
    /// browser is a refusal. Like the Python it edits the environment: later reads of `$VAR` see it.
    pub fn settings(&mut self, agent: &str, svc: &str) -> R<Map<String, Value>> {
        self.env.insert("KITH_AGENT".to_owned(), agent.to_owned());
        for (k, v) in self.paths.entries() {
            if k.starts_with("KITH_") {
                self.env
                    .entry(k)
                    .or_insert_with(|| v.to_string_lossy().into_owned());
            }
        }
        let spec = self.service(svc, Some(agent))?;
        let can = spec.get("browsers").filter(|b| truthy(b)).cloned();
        if let Some(can) = can {
            let (name, browser, folder) = self.browser_profile(agent)?;
            // a new profile starts in the service's first browser
            let browser = browser
                .or_else(|| can.get(0).and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            if !drives(&can, &browser) {
                return die(format!(
                    "{svc} can't drive {browser}, the browser of {name} ({agent}'s profile; kith-auth browser)"
                ));
            }
            if let Some(parent) = folder.parent() {
                private_dir(parent)?;
            }
            self.env.insert("KITH_BROWSER".to_owned(), browser);
            self.env.insert(
                "KITH_BROWSER_PROFILE".to_owned(),
                folder.to_string_lossy().into_owned(),
            );
        }
        // `$HQ_X` in a service's env values (written before the rename) expands too
        crate::launch::assemble::sync_env(&mut self.env);
        let mut env = match get(get(&spec, "mcp"), "env") {
            Value::Object(o) => o.clone(),
            _ => Map::new(),
        };
        if let Value::Object(own) = get(get(get(self.auth, "env"), agent), svc) {
            env.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        Ok(env.into_iter().map(|(k, v)| (k, self.expand(&v))).collect())
    }
}

/// A service spec file, parsed.
pub(super) fn read_spec(f: &Path) -> R<Value> {
    let text = std::fs::read_to_string(f).map_err(|e| Die {
        msg: format!("{}: {e}", f.display()),
        code: 1,
    })?;
    serde_json::from_str(&text).map_err(|e| Die {
        msg: format!(
            "{}: invalid JSON ({})",
            f.display(),
            pyjson::error_of(&text).unwrap_or_else(|| e.to_string())
        ),
        code: 1,
    })
}

/// `drives(spec, browser)`: whether a browser service can run that browser (an app path counts when it lists "path").
fn drives(can: &Value, browser: &str) -> bool {
    let list: Vec<&str> = can
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    list.contains(&browser) || (list.contains(&"path") && browser.contains('/'))
}

/// `folder.mkdir(mode=0o700, parents=True, exist_ok=True)`: logins are this user's only. Parents get the default mode.
fn private_dir(folder: &Path) -> R<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if folder.is_dir() {
        return Ok(());
    }
    let fail = |e: std::io::Error| Die {
        msg: format!("{}: {e}", folder.display()),
        code: 1,
    };
    if let Some(up) = folder.parent() {
        std::fs::create_dir_all(up).map_err(fail)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(folder) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(fail(e)),
        _ => Ok(()),
    }
}

/// `os.path.expanduser` for `~` and `~/...`: `$HOME`, else the password database's home. `~name` stays as written.
pub(super) fn expanduser(env: &Env, s: &str) -> String {
    let Some(rest) = s.strip_prefix('~') else {
        return s.to_owned();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return s.to_owned();
    }
    let home = env.get("HOME").cloned().unwrap_or_else(|| {
        super::os_home()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let joined = format!("{}{rest}", home.trim_end_matches('/'));
    if joined.is_empty() {
        "/".to_owned()
    } else {
        joined
    }
}

/// `os.path.expandvars`: `$name` and `${name}` that name a variable; one that does not stays as it is.
pub(super) fn expandvars(env: &Env, s: &str) -> String {
    static VAR: OnceLock<Regex> = OnceLock::new();
    let re = VAR.get_or_init(|| {
        Regex::new(r"\$([A-Za-z0-9_]+|\{[^}]*\})").expect("the pattern is a constant")
    });
    re.replace_all(s, |m: &regex::Captures<'_>| {
        let name = &m[1];
        let name = name
            .strip_prefix('{')
            .and_then(|n| n.strip_suffix('}'))
            .unwrap_or(name);
        env.get(name).cloned().unwrap_or_else(|| m[0].to_owned())
    })
    .into_owned()
}
