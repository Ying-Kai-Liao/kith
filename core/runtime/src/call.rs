//! Calling a method of the table (`kith-api`) on an open core. The door is a method of [`Core`], so no handler runs
//! without the lifetime lock and the generation that come with having one (M0b follow-up (a)).

use std::path::{Path, PathBuf};

use kith_kernel::paths::Paths;

use serde_json::{Value, json};

use crate::db::Core;
use crate::error::{Error, Result};
use crate::runs::RunIdentity;

impl Core {
    /// Calls a method of the table as `who`: the method must exist, be open to the principal and take these params.
    /// The core answers the methods that need no core state itself (`paths`, `kernel.version`); every other row answers
    /// [`Error::NotServed`] until the daemon's wiring registers its handler, and the caller falls back to what it did
    /// before the table.
    ///
    /// A method that depends on the caller's environment takes the caller's view in its params, never the process's own:
    /// the daemon's environment is not the environment of the CLI that asked (`paths` takes `home` and `env`).
    pub fn call(&self, who: kith_api::Principal, method: &str, params: &Value) -> Result<Value> {
        let row = check(who, method, params)?;
        match row.name {
            "paths" => self.paths(params),
            "kernel.version" => parse(&kith_kernel::version_json()),
            _ => Err(Error::NotServed(format!(
                "kith-core: {method} is not answered in the core yet"
            ))),
        }
    }

    /// A method of the run socket, as the run its token names. The identity comes from the token table, never from the
    /// params. Only `run.projectId` is answered here (it needs nothing but the identity); the kernel tools stay with
    /// the host's host.sock until a later step switches them.
    // A method of Core on purpose, though it reads nothing of it yet: no handler runs without the lock and generation.
    #[allow(clippy::unused_self)]
    pub(crate) fn call_run(
        &self,
        who: &RunIdentity,
        method: &str,
        params: &Value,
    ) -> Result<Value> {
        let row = check(kith_api::Principal::RunToken, method, params)?;
        let args = params.get("args").and_then(Value::as_array);
        match (row.name, args.map(Vec::as_slice)) {
            ("run.projectId", None | Some([])) => Ok(match &who.project {
                Some(id) => json!({"stdout": format!("{id}\n"), "stderr": "", "code": 0}),
                None => json!({"stdout": "", "stderr": "", "code": 1}),
            }),
            _ => Err(Error::NotServed(format!(
                "kith-core: {method} is not answered in the core yet"
            ))),
        }
    }

    fn paths(&self, params: &Value) -> Result<Value> {
        parse(&self.folders_for(params)?.0.json())
    }

    /// The folders of the caller's view (see [`Self::call`]) and its kernel folder: the params' `kernel`, `home` and
    /// `env`, else the folders this core was opened with.
    pub(crate) fn folders_for(&self, params: &Value) -> Result<(Paths, PathBuf)> {
        let kernel = params.get("kernel").and_then(Value::as_str);
        let home = params.get("home").and_then(Value::as_str);
        let env = params.get("env").and_then(Value::as_object);
        if home.is_some() || env.is_some() {
            // The caller's view: its home and its variables, and nothing from this process's environment.
            let home = home
                .map(Path::new)
                .map_or_else(kith_kernel::paths::process_home, Path::to_path_buf);
            let kernel: PathBuf = kernel.map_or_else(|| home.join("kith/kernel"), Into::into);
            let lookup = |name: &str| {
                env.and_then(|e| e.get(name))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            Ok((kith_kernel::paths::paths(&home, &kernel, &lookup), kernel))
        } else if let Some(k) = kernel {
            Ok((
                kith_kernel::paths::paths(
                    &kith_kernel::paths::process_home(),
                    Path::new(k),
                    &kith_kernel::paths::process_env,
                ),
                PathBuf::from(k),
            ))
        } else {
            let folders = self.folders().cloned().ok_or_else(|| {
                Error::NotServed("kith-core: this core was opened without its folders".into())
            })?;
            let kernel = folders.home.join("kernel");
            Ok((folders, kernel))
        }
    }
}

/// The shared door check: the method exists, is open to `who` and takes these params.
pub(crate) fn check(
    who: kith_api::Principal,
    method: &str,
    params: &Value,
) -> Result<&'static kith_api::Method> {
    let row = kith_api::method(method)
        .ok_or_else(|| Error::NoSuchMethod(format!("kith-core: no method {method}")))?;
    if !row.allows(who) {
        return Err(Error::NotAllowed(format!(
            "kith-core: {method} is not open to {}",
            who.name()
        )));
    }
    kith_api::params::validate(row, params)
        .map_err(|e| Error::BadParams(format!("kith-core: {e}")))?;
    Ok(row)
}

fn parse(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::Io(format!("kith-core: unreadable answer: {e}")))
}
