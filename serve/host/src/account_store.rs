//! Where the accounts' state lives for the verbs this program runs (`kith kernel accounts ...`, `account-state ...`, the
//! launch). Until `kith migrate step accounts` it is `<machine>/accounts/state.json` and `kith-kernel` reads and writes it. After
//! the step that file is a tombstone and the state is the core's table: [`install`] then makes `kith-kernel` read and write
//! through the core's `account.*` rows (the daemon when one runs, else an embedded core), so nothing in this program reads
//! the tombstone and nothing writes over it.

use std::time::{Duration, Instant};

use kith_kernel::accounts::{Change, Store};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};

type Map = serde_json::Map<String, Value>;

/// How long a call waits for a core that is starting, or that another command holds (a launch's mark takes milliseconds).
const WAIT: Duration = Duration::from_secs(8);

/// Whether the `accounts` module has moved to the core on this Mac: its switch file says anything but `stepping`
/// (`kith_cutover_refuse` in `kernel/lib/kith-core.sh` reads it the same way: a rollback in progress counts as moved).
pub fn switched(folders: &Paths) -> bool {
    let f = folders.machine.join("core").join("cutover").join("accounts.switch");
    match std::fs::read_to_string(f) {
        Ok(text) => !text.lines().any(|l| l == "state=stepping"),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Makes the core the home of the accounts' state for the rest of this process when the module is switched. Returns
/// whether it did.
pub fn install(folders: &Paths) -> bool {
    // The switch file alone is not enough: between "record switched" and the tombstone (and mid-rollback) the file is still the
    // truth and Python still writes it.
    switched(folders)
        && kith_kernel::accounts::tombstoned(&folders.machine)
        && kith_kernel::accounts::use_store(Box::new(CoreStore { folders: folders.clone() }))
}

/// The core's table, reached over the owner socket or an embedded core.
struct CoreStore {
    folders: Paths,
}

impl Store for CoreStore {
    fn accounts(&self) -> Result<Map, String> {
        match call(&self.folders, "account.state", &json!({}), true)? {
            Value::Object(mut a) => match a.remove("accounts") {
                Some(Value::Object(m)) => Ok(m),
                _ => Err("the core's answer has no accounts".to_owned()),
            },
            _ => Err("the core's answer is not an object".to_owned()),
        }
    }

    fn apply(&self, change: &Change, now: i64) -> Result<(), String> {
        let (method, params) = match change {
            Change::Out { id, until, reason, extend_only } => {
                ("account.out", json!({"id": id, "until": until, "reason": reason, "extend": extend_only, "now": now}))
            }
            Change::Clear { id, host_until } => {
                ("account.clear", json!({"id": id, "hostUntil": host_until, "now": now}))
            }
            Change::Started { id } => ("account.started", json!({"id": id, "now": now})),
            Change::Usage { id, usage } => ("account.usage", json!({"id": id, "reading": usage, "now": now})),
        };
        call(&self.folders, method, &params, false).map(|_| ())
    }
}

/// One owner call. A read may be sent again when the connection breaks; a write never is, so a lost answer is an error that
/// says the write may have happened.
fn call(folders: &Paths, method: &str, params: &Value, read: bool) -> Result<Value, String> {
    let started = Instant::now();
    loop {
        match kith_core::reach(folders, method, params, read) {
            kith_core::Reached::Answered(v) => return Ok(v),
            kith_core::Reached::Refused(e) => return Err(e.to_string()),
            kith_core::Reached::Mismatch(note) => return Err(note),
            kith_core::Reached::Lost(why) => {
                return Err(format!(
                    "the connection to the core ended: {why}{}",
                    if read { "" } else { " (the change may have been made)" }
                ));
            }
            kith_core::Reached::NoDaemon => {}
        }
        #[cfg(unix)]
        match kith_core::call_account_embedded(folders, method, params) {
            Ok(kith_core::EmbeddedCall::Answered(v)) => return Ok(v),
            Ok(kith_core::EmbeddedCall::NoCore(dir)) => {
                return Err(format!("accounts moved to the core, but there is no core database at {}", dir.display()));
            }
            Ok(kith_core::EmbeddedCall::Busy) => {}
            Err(e) => return Err(e.to_string()),
        }
        if cfg!(not(unix)) {
            return Err("the core is not available on this system".to_owned());
        }
        if started.elapsed() >= WAIT {
            return Err("accounts moved to the core, but no core answers and another command holds it".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
