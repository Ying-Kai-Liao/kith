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
            "secrets.open"
            | "secrets.has"
            | "secrets.get"
            | "secrets.put"
            | "secrets.delete"
            | "secrets.serviceKeys" => {
                crate::secrets::call(self.conn(), &self.secrets, row.name, params)
            }
            // Pure rules, no state: the clock is the only thing read.
            "schedule.preview" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|d| i64::try_from(d.as_millis()).ok())
                    .unwrap_or_default();
                kith_kernel::schedule::preview(params, now)
                    .map_err(|e| Error::BadParams(format!("kith-core: schedule.preview: {e}")))
            }
            m if crate::schedules::serves(m) => crate::schedules::call(self, m, params),
            #[cfg(unix)]
            "conversation.transcript" => self.transcript(params),
            m if crate::accounts::call::serves(m) => self.account_call(m, params),
            #[cfg(unix)]
            "cutover.mayServe" => crate::cutover::call_may_serve(self, params),
            _ => Err(Error::NotServed(format!(
                "kith-core: {method} is not answered in the core yet"
            ))),
        }
    }

    /// The checks every run-socket call passes, under the core lock: the row exists and is open to a run token, the
    /// params fit, and the run's person (and a room turn's basis) is still allowed. A second door behind the token table:
    /// `people.suspend` ends the tokens inside the same locked core call, and this runs under that lock too, so a request
    /// that resolved just before the end is still refused here. `switched` is whether the authority is the core's.
    // A method of Core on purpose: no handler runs without the lock and generation.
    pub(crate) fn run_gate(
        &self,
        who: &RunIdentity,
        method: &str,
        params: &Value,
        switched: bool,
    ) -> Result<&'static kith_api::Method> {
        let row = check(kith_api::Principal::RunToken, method, params)?;
        run_identity_allowed(self.conn(), who, switched, method)?;
        Ok(row)
    }

    /// A method of the run socket, as the run its token names, for the rows the core answers from the identity alone:
    /// `run.projectId` with no arguments. The kernel tools (`run.recall`, `run.memorySearch`, `run.projectId --info`) run
    /// in `runtools`, outside this lock, between two [`Self::run_gate`] checks.
    pub(crate) fn call_run(
        &self,
        who: &RunIdentity,
        method: &str,
        params: &Value,
        switched: bool,
    ) -> Result<Value> {
        let row = self.run_gate(who, method, params, switched)?;
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

/// Whether a run may still have a token: its person is allowed (`authority::run_allowed_in`) and, for a room person's
/// turn, so is every member of the basis. A room person (`room-<room>-<n>`, the host's id, never a human's) is no one the
/// authority tables know, so it is the basis that counts: the id must be of the turn's own room and the basis must not be
/// empty, a room person is always for somebody. Used at mint, at every run-socket call and in `run.resolve`, under the
/// core lock a revoke commits under. Where the authority tables are not served (not Unix yet) nothing can be checked, so
/// it refuses: a run token is never minted or honoured unchecked.
pub(crate) fn run_identity_allowed(
    conn: &rusqlite::Connection,
    who: &RunIdentity,
    switched: bool,
    method: &str,
) -> Result<()> {
    #[cfg(unix)]
    {
        let refused = |id: &str| {
            Error::NotAllowed(format!(
                "kith-core: {method}: {id} is revoked or not a person here, so no run of theirs is served"
            ))
        };
        let room_person = who.person.starts_with("room-");
        if room_person {
            let own_room = who
                .room
                .as_deref()
                .is_some_and(|r| is_room_person_of(&who.person, r));
            if !own_room || who.members.is_empty() {
                return Err(refused(&who.person));
            }
        } else if !who.members.is_empty() {
            // A basis belongs to a room person only.
            return Err(refused(&who.person));
        }
        let ids = if room_person {
            who.members.as_slice()
        } else {
            std::slice::from_ref(&who.person)
        };
        for id in ids {
            if !crate::authority::run_allowed_in(conn, id, switched)? {
                return Err(refused(id));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (conn, who, switched);
        Err(Error::NotAllowed(format!(
            "kith-core: {method}: run tokens are not served on this platform yet"
        )))
    }
}

/// Whether `person` is a room person of `room`: `room-<slug>-<join gen>`, the id the host gives a room's people
/// (`chat::room_turn::room_person_id`: the room lowercased with every character that is not a letter or digit as a dash,
/// at most 32 of them, then the join gen). The parts are compared, not a prefix: room `a` is not room `a-b`.
#[cfg(unix)]
pub(crate) fn is_room_person_of(person: &str, room: &str) -> bool {
    let slug: String = room
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    person
        .strip_prefix("room-")
        .and_then(|rest| rest.strip_prefix(slug.as_str()))
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|generation| {
            !generation.is_empty() && generation.bytes().all(|b| b.is_ascii_digit())
        })
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
