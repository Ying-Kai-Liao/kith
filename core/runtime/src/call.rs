//! Calling a method of the table (`kith-api`) on an open core. The door is a method of [`Core`], so no handler runs
//! without the lifetime lock and the generation that come with having one (M0b follow-up (a)).

use std::path::Path;

use serde_json::Value;

use crate::db::Core;
use crate::error::{Error, Result};

impl Core {
    /// Calls a method of the table as `who`: the method must exist, be open to the principal and take these params.
    /// The core answers the methods that need no core state itself (`paths`, `kernel.version`); every other row answers
    /// [`Error::NotServed`] until the daemon's wiring registers its handler, and the caller falls back to what it did
    /// before the table.
    ///
    /// A method that depends on the caller's environment takes the caller's view in its params, never the process's own:
    /// the daemon's environment is not the environment of the CLI that asked (`paths` takes `home` and `env`).
    pub fn call(&self, who: kith_api::Principal, method: &str, params: &Value) -> Result<Value> {
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
        match row.name {
            "paths" => self.paths(params),
            "kernel.version" => parse(&kith_kernel::version_json()),
            _ => Err(Error::NotServed(format!(
                "kith-core: {method} is not answered in the core yet"
            ))),
        }
    }

    fn paths(&self, params: &Value) -> Result<Value> {
        let kernel = params.get("kernel").and_then(Value::as_str);
        let home = params.get("home").and_then(Value::as_str);
        let env = params.get("env").and_then(Value::as_object);
        let folders = if home.is_some() || env.is_some() {
            // The caller's view: its home and its variables, and nothing from this process's environment.
            let home = home.map(Path::new).map_or_else(kith_kernel::paths::process_home, Path::to_path_buf);
            let kernel = kernel.map_or_else(|| home.join("kith/kernel"), Into::into);
            let lookup = |name: &str| env.and_then(|e| e.get(name)).and_then(Value::as_str).map(str::to_owned);
            kith_kernel::paths::paths(&home, &kernel, &lookup)
        } else if let Some(k) = kernel {
            kith_kernel::paths::paths(
                &kith_kernel::paths::process_home(),
                Path::new(k),
                &kith_kernel::paths::process_env,
            )
        } else {
            self.folders().cloned().ok_or_else(|| {
                Error::NotServed("kith-core: this core was opened without its folders".into())
            })?
        };
        parse(&folders.json())
    }
}

fn parse(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::Io(format!("kith-core: unreadable answer: {e}")))
}
