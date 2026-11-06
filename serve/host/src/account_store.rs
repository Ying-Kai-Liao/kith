//! Where the accounts' state lives for the verbs this program runs (`kith kernel accounts ...`, `account-state ...`, the
//! launch). Until `kith migrate step accounts` it is `<machine>/accounts/state.json` and `kith-kernel` reads and writes it. After
//! the step that file is a tombstone and the state is the core's table: [`install`] then makes `kith-kernel` read and write
//! through the core's `account.*` rows (the daemon when one runs, else an embedded core), so nothing in this program reads
//! the tombstone and nothing writes over it.

use kith_kernel::accounts::{Change, Store};
use kith_kernel::paths::Paths;
use serde_json::{Value, json};

type Map = serde_json::Map<String, Value>;

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

/// One owner call, through the front door (`core_front`, the only file here that names the core).
fn call(folders: &Paths, method: &str, params: &Value, read: bool) -> Result<Value, String> {
    crate::core_front::account_call(folders, method, params, read)
}
