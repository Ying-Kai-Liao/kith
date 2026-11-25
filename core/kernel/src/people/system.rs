//! The real backend: `kith-auth people --json` for who is invited, the host's local endpoint for invites.

use super::Backend;
use crate::hostlocal;
use crate::secrets::pyjson;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// What the host call reads besides the command line.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// `KITH_SERVE_DIR`: the host's data folder (`token` lives there).
    pub serve_dir: Option<String>,
    /// `KITH_SERVE_PORT`.
    pub serve_port: Option<String>,
}

impl Env {
    pub fn from_process() -> Env {
        Env {
            serve_dir: crate::paths::process_env("KITH_SERVE_DIR"),
            serve_port: crate::paths::process_env("KITH_SERVE_PORT"),
        }
    }
}

pub struct System {
    pub kernel: PathBuf,
    pub home: PathBuf,
    pub env: Env,
}

/// How long the host gets to answer (`urlopen(timeout=30)`).
const HOST_WAIT: Duration = Duration::from_secs(30);

impl System {
    /// (data dir, port) of this install's `kith serve`: the env wins, else `kith-serve endpoint`, else the defaults.
    fn endpoint(&self) -> (PathBuf, String) {
        let given = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
        let (dir, port) = (given(&self.env.serve_dir), given(&self.env.serve_port));
        if let (Some(d), Some(p)) = (&dir, &port) {
            return (PathBuf::from(d), p.clone());
        }
        let (d, p) = self.ask_endpoint().unwrap_or_else(|| {
            (
                self.home
                    .join(".kith")
                    .join("serve")
                    .to_string_lossy()
                    .into_owned(),
                "19224".to_owned(),
            )
        });
        (PathBuf::from(dir.unwrap_or(d)), port.unwrap_or(p))
    }

    /// `kith-serve endpoint`: two words, within 4 seconds.
    fn ask_endpoint(&self) -> Option<(String, String)> {
        let mut cmd = Command::new(self.kernel.join("bin").join("kith-serve"));
        cmd.arg("endpoint");
        let out = crate::secrets::run_command_with_timeout(cmd, Duration::from_secs(4))?;
        let mut words = out.split_whitespace();
        let (d, p) = (words.next()?, words.next()?);
        words.next().is_none().then(|| (d.to_owned(), p.to_owned()))
    }
}

impl Backend for System {
    fn people(&self) -> Result<Value, String> {
        let r = Command::new(self.kernel.join("bin").join("kith-auth"))
            .args(["people", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|_| "kith-auth could not list people".to_owned())?;
        if !r.status.success() {
            let err = String::from_utf8_lossy(&r.stderr);
            let err = err.trim();
            return Err(if err.is_empty() {
                "kith-auth could not list people".to_owned()
            } else {
                err.strip_prefix("kith-auth: ").unwrap_or(err).to_owned()
            });
        }
        serde_json::from_slice(&r.stdout)
            .map_err(|_| "kith-auth people gave something that is not JSON".to_owned())
    }

    fn host_call(&self, method: &str, body: &Value) -> Result<Value, String> {
        let (dir, port) = self.endpoint();
        let unreachable = |why: &str| format!("kith serve is not reachable ({why})");
        let token =
            std::fs::read(dir.join("token")).map_err(|e| unreachable(&os_error_name(&e)))?;
        let token = String::from_utf8(token)
            .map_err(|_| unreachable("UnicodeDecodeError"))?
            .trim()
            .to_owned();
        // http.client refuses a header value with a line break before it sends anything.
        if token.contains(['\r', '\n']) {
            return Err(unreachable("ValueError"));
        }
        let reply = post(
            &dir,
            &port,
            &format!("/api/{method}"),
            &token,
            &pyjson::dumps(body),
        )
        .map_err(|why| unreachable(&why))?;
        if !(200..300).contains(&reply.status) {
            // `{"error": "..."}` says why; anything else is only the status.
            let why = serde_json::from_slice::<Value>(&reply.body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                });
            return Err(why.unwrap_or_else(|| format!("kith serve answered {}", reply.status)));
        }
        std::str::from_utf8(&reply.body)
            .map_err(|_| unreachable("UnicodeDecodeError"))
            .and_then(|t| serde_json::from_str(t).map_err(|_| unreachable("JSONDecodeError")))
    }
}

/// The name of the Python exception a failed file read raises.
fn os_error_name(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ if e.raw_os_error() == Some(21) => "IsADirectoryError",
        _ => "OSError",
    }
    .to_owned()
}

/// One `POST` over the host's local endpoint (`hostlocal`, never loopback TCP). The `Err` is the Python exception's name, or
/// why the host cannot be trusted or is older than this command.
fn post(
    data: &Path,
    port: &str,
    path: &str,
    token: &str,
    body: &str,
) -> Result<hostlocal::Reply, String> {
    hostlocal::post(data, path, token, body, HOST_WAIT).map_err(|e| match e {
        hostlocal::LocalErr::Down => {
            let older = port
                .parse()
                .is_ok_and(|p| hostlocal::status(data, p) == hostlocal::Status::Older);
            if older {
                hostlocal::OLDER_HOST.to_owned()
            } else {
                "URLError".to_owned()
            }
        }
        hostlocal::LocalErr::TimedOut => "TimeoutError".to_owned(),
        hostlocal::LocalErr::Untrusted(why) => format!("not this account's: {why}"),
        hostlocal::LocalErr::Io(_) => "URLError".to_owned(),
    })
}
