//! The daemon front (M0c): `kith serve` takes the lifetime lock as the daemon, checks the file, serves the method table
//! on the owner socket and keeps a link to the holder. Everything here runs on threads of its own, and no failure of
//! it reaches the host: a core that will not start is a line in the core log and a status (`kith doctor`), and
//! `kith serve` goes on without it.
//!
//! Against embedded commands: the lock is one lock. If an embedded command holds it when the daemon starts, the daemon
//! waits it out with bounded backoff (and says so in the log, once) instead of failing or taking it, and keeps trying
//! for as long as the holder is an embedded command: a command is short, and a daemon that gave up would leave the
//! machine without one until the next `kith serve`. Only a holder that does not say it is an embedded command (an
//! unknown process) is waited for no longer than `wait_for_embedded`. A second daemon finds the first
//! one's name in core.holder and refuses at once. While the daemon holds the lock, an embedded call goes to the socket.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kith_kernel::paths::Paths;
use serde_json::{Value, json};

use crate::db::{Core, MIGRATIONS, Mode, Opened};
use crate::error::{Error, Result};
use crate::holder_link::{self, HolderConfig, HolderState};
use crate::layout::Layout;
use crate::lock::{self, Role};
use crate::owner::{self, Handler, Reached, Server};
use crate::runs::RunTokens;
use crate::runsock::{self, RunHandler};
use crate::terminal::Terminals;
use crate::{busy, log};

pub(crate) struct Config {
    pub holder: HolderConfig,
    /// How long a holder that is not a known embedded command may hold the lock before the daemon gives up waiting.
    /// An embedded command is waited for without limit.
    pub wait_for_embedded: Duration,
    /// Off for tests that are not about the holder.
    pub holder_on: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            holder: HolderConfig::default(),
            wait_for_embedded: Duration::from_secs(30),
            holder_on: true,
        }
    }
}

enum Outcome {
    Pending,
    Serving,
    Failed(Error),
}

pub(crate) struct Shared {
    core: Mutex<Option<Core>>,
    stop: AtomicBool,
    served: AtomicU64,
    holder: Mutex<HolderState>,
    pub(crate) logs: PathBuf,
    outcome: Mutex<Outcome>,
    changed: Condvar,
    server: Mutex<Option<Server>>,
    /// The run socket (M0e) and its token table. The table is memory: a restart ends every token, and the runs that
    /// still want one are minted a new token later.
    run_server: Mutex<Option<runsock::Server>>,
    /// The attach socket (M1-5): clients of held terminals.
    attach_server: Mutex<Option<crate::attach::Server>>,
    runs: Arc<RunTokens>,
    socket: PathBuf,
    run_socket: PathBuf,
    started: Instant,
    pub(crate) terminals: Terminals,
}

impl Shared {
    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Runs `f` on the core. The core is gone once the daemon has stopped.
    pub(crate) fn with_core<R>(&self, f: impl FnOnce(&mut Core) -> Result<R>) -> Result<R> {
        let mut guard = self
            .core
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_mut() {
            Some(core) => f(core),
            None => Err(Error::Io("kith-core: the daemon is not running".into())),
        }
    }

    pub(crate) fn set_holder(&self, f: impl FnOnce(&mut HolderState)) {
        f(&mut self
            .holder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    fn set_outcome(&self, o: Outcome) {
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = o;
        self.changed.notify_all();
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value> {
        if method == "kernel.coreStatus" {
            return self.running_status();
        }
        if method == "terminal.open" {
            let answer = self.terminals.open(self, params)?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        if method == "terminal.attach" {
            let answer = self.terminals.attach_ticket(params)?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        if method == "terminal.list" {
            let answer = self.terminals.list(params)?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        if method == "run.mint" {
            // Minting is a claim on authority too: refuse a revoked person, inside the lock a revoke commits under,
            // so a mint cannot slip in beside a suspension.
            crate::call::check(kith_api::Principal::Owner, method, params)?;
            let person = params
                .get("person")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut answer = self.with_core(|c| {
                crate::call::run_person_allowed(c.conn(), person, method)?;
                self.runs.owner_call(method, params).unwrap_or_else(|| {
                    Err(Error::NoSuchMethod("kith-core: no method run.mint".into()))
                })
            })?;
            // Where the run asks, for a host that hands the path to the run.
            if let Some(o) = answer.as_object_mut() {
                o.insert("socket".into(), json!(self.run_socket.to_string_lossy()));
            }
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        if let Some(answer) = self.runs.owner_call(method, params) {
            let answer = answer?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        // The gate needs `&mut Core` (its state changes are claim transactions), which `Core::call` does not hold.
        if method.starts_with("gate.") {
            crate::call::check(kith_api::Principal::Owner, method, params)?;
            let answer =
                self.with_core(|c| crate::gate::call(c, method, params, &crate::gate::System))?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        // The owner's authority writes are claim transactions too (M2-2).
        #[cfg(unix)]
        {
            if crate::authority::call::serves(method) {
                crate::call::check(kith_api::Principal::Owner, method, params)?;
                // The tokens of a revoked or removed person end in the same locked call that committed it.
                let answer = self.with_core(|c| {
                    let answer = crate::authority::call::call(c, method, params)?;
                    self.runs.after_authority_write(method, params, &answer);
                    Ok(answer)
                })?;
                self.served.fetch_add(1, Ordering::SeqCst);
                return Ok(answer);
            }
        }
        // The guest doors: start, mark live, end and the sweep are claim transactions (M2-14).
        #[cfg(unix)]
        {
            if crate::doors::serves(method) {
                crate::call::check(kith_api::Principal::Owner, method, params)?;
                let answer = self
                    .with_core(|c| crate::doors::call(c, method, params, &crate::gate::System))?;
                self.served.fetch_add(1, Ordering::SeqCst);
                return Ok(answer);
            }
        }
        // The wake queue (M3-7): nothing calls these until kith-wake is switched; the daemon runs no drain and fires no waiter.
        if crate::wake::serves(method) {
            crate::call::check(kith_api::Principal::Owner, method, params)?;
            let answer = self.with_core(|c| crate::wake::call(c, method, params))?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        // The ask tree: open, settle and end are claim transactions (M3-8a).
        #[cfg(unix)]
        {
            if crate::asks::serves(method) {
                crate::call::check(kith_api::Principal::Owner, method, params)?;
                let answer = self.with_core(|c| crate::asks::call(c, method, params))?;
                self.served.fetch_add(1, Ordering::SeqCst);
                return Ok(answer);
            }
        }
        // Same for the sync lease: a change is a claim transaction.
        if method.starts_with("syncLease.") {
            crate::call::check(kith_api::Principal::Owner, method, params)?;
            let answer = self.with_core(|c| {
                crate::sync_lease::call(c, method, params, &crate::sync_lease::System)
            })?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        // So do the cards: raise, claim and close are claim transactions.
        if method.starts_with("cards.") {
            crate::call::check(kith_api::Principal::Owner, method, params)?;
            let answer =
                self.with_core(|c| crate::cards::call(c, method, params, &crate::gate::System))?;
            self.served.fetch_add(1, Ordering::SeqCst);
            return Ok(answer);
        }
        // Counts answers only: a refused or failed call was not served.
        let answer = self.with_core(|c| c.call(kith_api::Principal::Owner, method, params))?;
        self.served.fetch_add(1, Ordering::SeqCst);
        Ok(answer)
    }

    fn running_status(&self) -> Result<Value> {
        let (generation, schema) = self.with_core(|c| Ok((c.generation(), c.schema_version())))?;
        let h = self
            .holder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(json!({
            "core": {"state": "daemon", "recovery": null, "note": "the daemon is running"},
            "daemon": {
                "running": true,
                "pid": std::process::id(),
                "generation": generation,
                "schema_version": schema,
                "socket": self.socket,
                "protocol": owner::PROTOCOL,
                "version": owner::VERSION,
                "api_schema": kith_api::schema_hash(),
                "served": self.served.load(Ordering::SeqCst),
                "uptime_secs": self.started.elapsed().as_secs(),
            },
            "holder": holder_json(&h),
        }))
    }
}

fn holder_json(h: &HolderState) -> Value {
    json!({
        "state": if h.state.is_empty() { "starting" } else { h.state },
        "instance": h.instance,
        "pid": h.pid,
        "tied_to_starter": h.tied_to_starter,
        "processes": h.procs,
        "binary": h.binary,
        "error": h.error,
    })
}

/// The daemon front of one `kith serve` (or of `kith kernel core-daemon`). Dropping it stops it.
pub struct Daemon {
    shared: Arc<Shared>,
}

impl Daemon {
    /// Starts the daemon on a thread of its own and returns at once: it never blocks the startup of what called it.
    /// Whether it came up is [`Daemon::ready`]; if it did not, the core log says why and the caller carries on.
    pub fn spawn(paths: &Paths) -> Daemon {
        Self::spawn_with(paths, Config::default())
    }

    pub(crate) fn spawn_with(paths: &Paths, cfg: Config) -> Daemon {
        let layout = Layout::new(&paths.machine);
        let shared = Arc::new(Shared {
            core: Mutex::new(None),
            stop: AtomicBool::new(false),
            served: AtomicU64::new(0),
            holder: Mutex::new(HolderState {
                state: "starting",
                ..HolderState::default()
            }),
            logs: paths.logs.clone(),
            outcome: Mutex::new(Outcome::Pending),
            changed: Condvar::new(),
            server: Mutex::new(None),
            run_server: Mutex::new(None),
            attach_server: Mutex::new(None),
            runs: Arc::new(RunTokens::new()),
            socket: owner::endpoint(&layout),
            run_socket: runsock::endpoint(&layout),
            started: Instant::now(),
            terminals: Terminals::new(),
        });
        let (s, p) = (shared.clone(), paths.clone());
        let spawned = std::thread::Builder::new()
            .name("kith-core-daemon".into())
            .spawn(move || run(&s, &p, &layout, &cfg));
        if let Err(e) = spawned {
            let err = Error::Io(format!("kith-core: can't start the daemon thread: {e}"));
            log::note(&shared.logs, &format!("daemon: not started: {err}"));
            shared.set_outcome(Outcome::Failed(err));
        }
        Daemon { shared }
    }

    /// Waits until the daemon serves, or says why it will not.
    pub fn ready(&self, timeout: Duration) -> Result<()> {
        let end = Instant::now() + timeout;
        let mut guard = self
            .shared
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match &*guard {
                Outcome::Serving => return Ok(()),
                Outcome::Failed(e) => return Err(e.clone()),
                Outcome::Pending => {}
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Io("kith-core: the daemon is not up yet".into()));
            }
            guard = self
                .shared
                .changed
                .wait_timeout(guard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// How many calls the owner socket has run (not counting the status question).
    pub fn served(&self) -> u64 {
        self.shared.served.load(Ordering::SeqCst)
    }

    /// Stops accepting, removes the socket and lets go of the lock.
    pub fn stop(&self) {
        let shared = &self.shared;
        shared.stop.store(true, Ordering::SeqCst);
        let server = shared
            .server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(server) = server {
            server.stop();
        }
        let run_server = shared
            .run_server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(server) = run_server {
            server.stop();
        }
        let attach_server = shared
            .attach_server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(server) = attach_server {
            server.stop();
        }
        let core = shared
            .core
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if core.is_some() {
            log::note(&shared.logs, "daemon: stopped");
        }
        drop(core);
        let mut outcome = shared
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*outcome, Outcome::Failed(_)) {
            *outcome = Outcome::Failed(Error::Io("kith-core: the daemon has stopped".into()));
        }
        shared.changed.notify_all();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(shared: &Arc<Shared>, paths: &Paths, layout: &Layout, cfg: &Config) {
    let core = match acquire(shared, layout, cfg) {
        Ok(core) => core.with_folders(paths),
        Err(e) => return failed(shared, e),
    };
    let mut core = core;
    // The relay connection of the last run is gone, so a lease row it left held must not be read as held.
    if let Err(e) = crate::sync_lease::restart(&mut core, &crate::sync_lease::System) {
        log::note(
            &shared.logs,
            &format!("daemon: could not end the sync lease of the last run: {e}"),
        );
    }
    let generation = core.generation();
    {
        // Checked under the lock `stop` takes the core with: a stop that came first leaves nothing held here.
        let mut slot = shared
            .core
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.stopping() {
            return;
        }
        *slot = Some(core);
    }
    let for_calls = shared.clone();
    let handler: Arc<Handler> = Arc::new(move |method, params| for_calls.dispatch(method, params));
    match Server::bind(layout, generation, handler) {
        Ok(server) => {
            let mut slot = shared
                .server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // `stop` sets the flag before it looks in this slot, so a server put here after that is stopped here.
            if shared.stopping() {
                drop(slot);
                server.stop();
            } else {
                *slot = Some(server);
            }
        }
        Err(e) => {
            // Holding the lock with nothing listening would only make every CLI call wait for nothing.
            shared
                .core
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            return failed(shared, e);
        }
    }
    // The run socket is not the owner's way in: a daemon that cannot open it still serves the owner, and says so.
    let for_runs = shared.clone();
    let run_handler: Arc<RunHandler> = Arc::new(move |who, row, params| {
        for_runs.with_core(|c| {
            // Under the lock an end or a revoke commits under: a run whose token is gone (ended, revoked, the
            // person removed) is refused even if its request resolved a moment before.
            if !for_runs.runs.is_live(&who.run_id) {
                return Err(Error::NotAllowed("kith-core: this run has ended".into()));
            }
            c.call_run(who, row, params)
        })
    });
    match runsock::Server::bind(layout, shared.runs.clone(), run_handler) {
        Ok(server) => {
            *shared
                .run_server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server);
        }
        Err(e) => log::note(&shared.logs, &format!("daemon: no run socket: {e}")),
    }
    // Same for the attach socket: without it terminals still run, they just cannot be attached to.
    match crate::attach::Server::bind(
        layout,
        shared.terminals.tickets.clone(),
        shared.terminals.relay.clone(),
        shared.logs.clone(),
    ) {
        Ok(server) => {
            let mut slot = shared
                .attach_server
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // `stop` sets the flag before it looks in this slot, so a server put here after that is stopped here.
            if shared.stopping() {
                drop(slot);
                server.stop();
            } else {
                *slot = Some(server);
            }
        }
        Err(e) => log::note(&shared.logs, &format!("daemon: no attach socket: {e}")),
    }
    log::note(
        &shared.logs,
        &format!(
            "daemon: serving, generation {generation}, socket {}",
            shared.socket.display()
        ),
    );
    if shared.stopping() {
        return;
    }
    shared.set_outcome(Outcome::Serving);
    if cfg.holder_on {
        holder_link::supervise(shared, layout, &cfg.holder);
    }
}

fn failed(shared: &Shared, e: Error) {
    log::note(
        &shared.logs,
        &format!("daemon: not started: {e} (recovery: {})", e.recovery()),
    );
    shared.set_outcome(Outcome::Failed(e));
}

/// Takes the lock as the daemon: waits out an embedded command, refuses a second daemon.
fn acquire(shared: &Shared, layout: &Layout, cfg: &Config) -> Result<Core> {
    let begun = Instant::now();
    let mut delay = Duration::from_millis(25);
    let mut told = false;
    loop {
        if shared.stopping() {
            return Err(Error::Io(
                "kith-core: stopped before the core was taken".into(),
            ));
        }
        match crate::db::open(layout, MIGRATIONS, Mode::Daemon)? {
            Opened::Core(core) => return Ok(*core),
            Opened::Busy(holder) => {
                let role = holder.as_ref().and_then(|h| h.role);
                let patient = role == Some(Role::Embedded);
                if role == Some(Role::Daemon)
                    || (!patient && begun.elapsed() >= cfg.wait_for_embedded)
                {
                    return Err(busy(holder));
                }
                if !told {
                    let who = holder
                        .as_ref()
                        .map_or(String::new(), |h| format!(" (pid {})", h.pid));
                    log::note(
                        &shared.logs,
                        &format!("daemon: a command{who} holds the core; waiting for it to finish"),
                    );
                    told = true;
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(1));
            }
        }
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "daemon_m1_tests.rs"]
mod m1_tests;

/// What `kith doctor` shows: asks the daemon when one answers, and otherwise says what the lock and the folder say.
/// Never takes the lock for good, never creates anything, never reads core.db.
pub fn status(paths: &Paths) -> Value {
    let layout = Layout::new(&paths.machine);
    let mut note = None;
    match owner::reach(&layout, "kernel.coreStatus", &json!({}), true) {
        Reached::Answered(v) => return v,
        Reached::Mismatch(n) => note = Some(n),
        _ => {}
    }
    let (state, recovery, text) = match lock::probe(&layout) {
        Ok(lock::Probe::Free) if !layout.db.is_file() => (
            "no-core",
            None,
            "no core yet: `kith serve` or `kith migrate` makes it; commands run without one"
                .to_owned(),
        ),
        Ok(lock::Probe::Free) => (
            "embedded",
            None,
            "no daemon is running; commands run in an embedded core".to_owned(),
        ),
        Ok(lock::Probe::Held(h)) if h.as_ref().is_some_and(|h| h.role == Some(Role::Embedded)) => (
            "busy",
            None,
            "a command holds the core for a moment; the daemon waits for it".to_owned(),
        ),
        Ok(lock::Probe::Held(h)) => (
            "down",
            Some("kith doctor"),
            format!(
                "core down: {} holds the lifetime lock but does not answer the owner socket. {}",
                h.map_or("a process".to_owned(), |h| format!("pid {}", h.pid)),
                core_down_list("kith doctor")
            ),
        ),
        Err(e) => (
            "down",
            Some(e.recovery()),
            format!("core down: {e}. {}", core_down_list(e.recovery())),
        ),
    };
    let last_problem = log::last_note(&paths.logs, "daemon: not started: ");
    json!({
        "core": {"state": state, "recovery": recovery, "note": text, "last_start_problem": last_problem, "mismatch": note},
        "daemon": {"running": false, "socket": owner::endpoint(&layout), "protocol": owner::PROTOCOL, "version": owner::VERSION, "api_schema": kith_api::schema_hash()},
        "holder": null,
    })
}

/// What an owner run keeps while no core can run (runtime-design, "The core-down list"), and what refuses.
fn core_down_list(recovery: &str) -> String {
    format!(
        "Owner runs keep the run's settings as last written, the must-ask classes, the `--direct` writer lock, the environment already given, and the recovery verbs (doctor, update, rollback, backup, restore, migrate); everything else refuses until `{recovery}`."
    )
}
