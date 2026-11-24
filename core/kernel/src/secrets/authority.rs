//! One reader of the auth tables for every verb of this module (retire-python P1-15, design 2026-11-21-mcp-exec 2.1):
//!
//! - **Files mode**: auth.json is the authority; the verbs read it as `load_auth` does.
//! - **Tombstone**: auth.json is a cutover note and the core is the authority (`kith kernel cutover step authority`). The
//!   verbs never read the replaced file. `may` and `resolve` are one call of the core each (`people.may`, `people.resolve`,
//!   the params `kith-auth` sends); `mcp-config` and `exec` read the core's tables (`people.secretsView`) and run the same
//!   code over them as over the file. The core is asked in process, through the client the program installs (the crate
//!   reaches no socket itself, like `threads::core`).
//!
//! A core that does not answer, a guest's sandbox, or a program with no client is exit 3 with `kith-auth`'s `route_fail`
//! text: it fails closed and never answers from the file.

#[cfg(test)]
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{Ctx, Die, Outcome, R, fill_tables, pyjson, read_json, tombstone_die};
use crate::threads::core::Core;

/// The core call's own limit (`CORE_CALL_SECS` in `kith-auth`); the program may be slower to answer than this and then the
/// verb is refused, never guessed.
const CALL_LIMIT: Duration = Duration::from_secs(4);
/// The folders the core reads from the caller's environment (`MIRROR_ENV` in `kith-auth`), then the serve folder.
const ENV_BLOCK: [&str; 7] = [
    "KITH_STATE",
    "HQ_STATE",
    "KITH_HOME",
    "HQ_HOME",
    "KITH_AGENTS",
    "HQ_AGENTS",
    "KITH_SERVE_DIR",
];
/// The tables `kith-auth` gives every view of the core's tables, `agent_access` besides the file's own.
const VIEW_TABLES: [&str; 10] = [
    "profiles",
    "grants",
    "env",
    "people",
    "presets",
    "members",
    "invites",
    "retired",
    "once",
    "agent_access",
];

/// The program's door onto the core: one `method`, its params, and a limit; the answer is a JSON object.
pub type Client = Arc<dyn Core + Send + Sync>;

static CLIENT: OnceLock<Client> = OnceLock::new();

/// The program installs its core client once, before it asks for a [`Ctx::from_process`]. Without one a tombstone is exit 3.
pub fn install_core(client: Client) {
    let _ = CLIENT.set(client);
}

/// The core's door for this call: the installed client, when `c.auth` is this home's own auth.json (only its tombstone is the
/// core's to answer; a tombstone anywhere else is refused, as `kith-auth` does).
fn client_for(c: &Ctx) -> Option<Client> {
    #[cfg(test)]
    if let Some((client, own)) = TEST.with(|t| t.borrow().clone()) {
        return if own == c.auth { client } else { None };
    }
    let own = crate::paths::from_process(&c.kernel)
        .secrets
        .join("auth.json");
    if own == c.auth {
        CLIENT.get().cloned()
    } else {
        None
    }
}

/// Whether `c.auth` is this home's own auth.json.
fn is_home_auth(c: &Ctx) -> bool {
    #[cfg(test)]
    if let Some((_, own)) = TEST.with(|t| t.borrow().clone()) {
        return own == c.auth;
    }
    crate::paths::from_process(&c.kernel)
        .secrets
        .join("auth.json")
        == c.auth
}

#[cfg(test)]
thread_local! {
    /// A test's own client and the auth.json it calls its home's.
    static TEST: std::cell::RefCell<Option<(Option<Client>, PathBuf)>> = const { std::cell::RefCell::new(None) };
}

/// A fake core for this test's thread, for the auth.json named.
#[cfg(test)]
pub(super) fn test_client<C: Core + Send + Sync + 'static>(client: Arc<C>, own_auth: PathBuf) {
    TEST.with(|t| *t.borrow_mut() = Some((Some(client as Client), own_auth)));
}

/// This test's thread has no core client, as a program that installed none, for the auth.json named.
#[cfg(test)]
pub(super) fn test_no_client(own_auth: PathBuf) {
    TEST.with(|t| *t.borrow_mut() = Some((None, own_auth)));
}

/// Who answers: the file (its tables), or the core.
enum Source {
    File(Value),
    Core,
}

/// The authority of this home, opened once per verb.
pub struct Authority<'a> {
    c: &'a Ctx,
    source: Source,
}

/// The refusal for a core that can't be asked or gave no answer (`route_fail`): exit 3, never a guess.
fn route_fail(why: &str) -> Die {
    Die {
        msg: format!("authority moved to the core, and it gave no answer: {why}"),
        code: 3,
    }
}

impl<'a> Authority<'a> {
    /// Looks at auth.json: its tables (files mode), or the core when it is this home's tombstone. Any other tombstone is the
    /// refusal of [`super::load_auth`].
    pub fn open(c: &'a Ctx) -> R<Self> {
        let data = read_json(&c.auth)?;
        let Some(refusal) = tombstone_die(&c.auth, &data) else {
            return Ok(Self {
                c,
                source: Source::File(fill_tables(&c.auth, data)?),
            });
        };
        let authority = data.get("module").and_then(Value::as_str) == Some("authority");
        if authority && is_home_auth(c) {
            return Ok(Self {
                c,
                source: Source::Core,
            });
        }
        Err(refusal)
    }

    /// Whether the tables come from the core.
    pub fn is_core(&self) -> bool {
        matches!(self.source, Source::Core)
    }

    /// The tables in auth.json's shape: the file's, or the core's `people.secretsView` (where a value lives, never the value).
    pub fn tables(&self) -> R<Value> {
        match &self.source {
            Source::File(v) => Ok(v.clone()),
            Source::Core => {
                let ans = ask(self.c, "people.secretsView", Map::new())?;
                let Some(Value::Object(view)) = ans.get("auth") else {
                    return Err(route_fail("the answer has no usable auth"));
                };
                let mut view = Value::Object(view.clone());
                if let Some(o) = view.as_object_mut() {
                    for t in VIEW_TABLES {
                        o.entry(t).or_insert_with(|| json!({}));
                    }
                }
                Ok(view)
            }
        }
    }

    /// `kith-auth may` on the tombstone: `people.may`, answered as `kith-auth` prints it.
    pub fn may(&self, person: &str, agent: &str, via: Option<&str>, once: bool) -> R<Outcome> {
        let mut p = Map::new();
        p.insert("person".to_owned(), person.into());
        p.insert("agent".to_owned(), agent.into());
        p.insert("once".to_owned(), once.into());
        p.insert("dies".to_owned(), true.into());
        if let Some(v) = via {
            p.insert("via".to_owned(), v.into());
        }
        let ans = ask(self.c, "people.may", p)?;
        if let Some(out) = died(&ans)? {
            return Ok(out);
        }
        let allowed = field_bool(&ans, "allowed")?;
        let why = field_str(&ans, "why")?;
        Ok(Outcome::out(why, i32::from(!allowed)))
    }

    /// `kith-auth resolve` on the tombstone: `people.resolve`; `person` is `--person`, else the run's (`KITH_PERSON`), else the
    /// core's own default, the owner. `take_once` is sent with a caller (`via`) only.
    pub fn resolve(
        &self,
        agent: &str,
        person: Option<&str>,
        via: Option<&str>,
        take_once: bool,
    ) -> R<Outcome> {
        let mut p = Map::new();
        p.insert("agent".to_owned(), agent.into());
        p.insert("dies".to_owned(), true.into());
        if let Some(who) = person.filter(|p| !p.is_empty()) {
            p.insert("person".to_owned(), who.into());
        }
        if let Some(v) = via.filter(|v| !v.is_empty()) {
            p.insert("via".to_owned(), v.into());
            if take_once {
                // decided and used up in one transaction of the core; sent with a caller only, and never again
                p.insert("takeOnce".to_owned(), true.into());
            }
        }
        let ans = ask(self.c, "people.resolve", p)?;
        if let Some(out) = died(&ans)? {
            return Ok(out);
        }
        field_bool(&ans, "allowed")?;
        field_bool(&ans, "held")?;
        field_str(&ans, "person")?;
        Ok(Outcome::out(pyjson::dumps(&Value::Object(ans)), 0))
    }
}

/// One call of the core, as `kith-auth`'s mirror makes it: marked `viaSwitch` (the core refuses it unless authority is
/// switched), with the folders it needs from this process. The answer is an object; anything else is [`route_fail`].
fn ask(c: &Ctx, method: &str, mut params: Map<String, Value>) -> R<Map<String, Value>> {
    if c.in_guest_sandbox() {
        return Err(route_fail("a guest run does not reach the core"));
    }
    let Some(core) = client_for(c) else {
        return Err(route_fail("no kith program to ask"));
    };
    let env: Map<String, Value> = ENV_BLOCK
        .iter()
        .filter_map(|k| {
            let v = std::env::var(k).ok().filter(|v| !v.is_empty())?;
            Some(((*k).to_owned(), Value::String(v)))
        })
        .collect();
    params.insert("viaSwitch".to_owned(), true.into());
    params.insert(
        "kernel".to_owned(),
        c.kernel.to_string_lossy().into_owned().into(),
    );
    params.insert(
        "home".to_owned(),
        crate::paths::process_home()
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    params.insert("env".to_owned(), Value::Object(env));
    match core.call(method, &Value::Object(params), Some(CALL_LIMIT)) {
        Err(e) => Err(route_fail(e.why())),
        Ok(Value::Object(ans)) => {
            if ans.get("noCore").is_some_and(super::truthy) {
                // impossible on a switched home: never read as empty or allowed
                return Err(route_fail("there is no core database"));
            }
            Ok(ans)
        }
        Ok(_) => Err(route_fail("the answer is not a JSON object")),
    }
}

/// `ans[key]` when it is a string; anything else fails closed.
fn field_str(ans: &Map<String, Value>, key: &str) -> R<String> {
    match ans.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(route_fail(&format!("the answer has no usable {key}"))),
    }
}

fn field_bool(ans: &Map<String, Value>, key: &str) -> R<bool> {
    match ans.get(key) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(route_fail(&format!("the answer has no usable {key}"))),
    }
}

/// The rules had no answer (`died`: a deleted preset...): `kith-auth` dies with the core's line and code, no "no".
fn died(ans: &Map<String, Value>) -> R<Option<Outcome>> {
    let Some(code) = ans.get("died").filter(|d| !d.is_null()) else {
        return Ok(None);
    };
    let why = field_str(ans, "why")?;
    let Some(code) = code.as_i64().and_then(|c| i32::try_from(c).ok()) else {
        return Err(route_fail("the answer has no usable died"));
    };
    Ok(Some(Outcome::died(&Die { msg: why, code })))
}
