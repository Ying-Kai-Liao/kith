//! `kith-serve`: runs coding-agent bots over ACP and serves the Kith apps.

mod access;
mod agent;
mod analytics;
mod api;
mod background;
mod calls;
mod cards;
mod channels;
mod chat;
mod compat;
mod core_front;
mod delegation;
mod doors;
mod environment;
mod hqmem;
mod hub;
mod kernel;
mod launch_cmd;
mod limits;
mod market;
mod mcp;
mod pkg_cmd;
mod remote;
mod rich_card;
mod routines;
mod run_outbox;
mod runauth;
mod runonce;
mod samples;
mod screen;
mod service;
mod shell;
mod store;
mod terminal;
mod threads_cmd;
mod tui;
mod update;
mod usage;
mod voice;
mod voice_local;
mod wake;
mod watchdog;

use agent::backends;
use remote::identity;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::fmt::Write as _;
use std::future::IntoFuture;
use std::io::Read;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Poison-tolerant locking. Every critical section in the host is a short,
/// self-contained update, so a panic elsewhere never leaves the data half
/// written — and a daemon must not wedge every later caller over one panic.
pub trait LockExt<T> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One shared HTTP client (connection pool + TLS config) for the whole process.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        // Only fails without system CA certificates (bare containers).
        reqwest::Client::builder().build().expect("TLS setup failed: install the ca-certificates package")
    });
    &CLIENT
}

#[derive(Parser)]
#[command(name = "kith-serve", version, about = "Kith host: runs your coding-agent bots and serves the Kith phone app")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Sub>,
}

#[derive(Subcommand)]
enum Sub {
    /// Run the host in the foreground (what the background service runs).
    #[command(alias = "run")]
    Serve {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        /// Address to bind. Defaults to all interfaces so the phone can reach it over Tailscale/LAN.
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
    },
    /// Show a one-time pairing QR code for the Kith iOS app (the host must be running).
    Pair {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        /// Print machine-readable JSON instead of a QR code.
        #[arg(long)]
        json: bool,
    },
    /// This computer's identity and local API details (for SSH setups).
    Info {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        json: bool,
    },
    /// List or revoke the devices allowed to use this computer.
    Devices {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[command(subcommand)]
        action: Option<DevicesAction>,
    },
    /// List, approve or deny account devices asking to use this computer.
    Access {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[command(subcommand)]
        action: Option<AccessAction>,
    },
    /// Show or change the Kith cloud (reach this computer from anywhere).
    Cloud {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
        /// How devices on this computer's account get in: `code` (compare the 6-digit code,
        /// the default) or `auto` (no check: anyone who gets into the account, or controls
        /// the cloud, can then add a device).
        #[arg(long, value_parser = ["code", "auto"])]
        approval: Option<String>,
        /// Accept the owner the cloud now names for this computer (or one set from the cloud): asks
        /// you to type the account at this terminal, and refuses when stdin is not a terminal.
        #[arg(long)]
        confirm_owner: bool,
    },
    /// Pair with another computer of this account (Mac link), list them, or answer one asking to pair with this one.
    Link {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        /// The other computer, by name or id. Without it, lists the account's computers and who asks to pair.
        computer: Option<String>,
        /// Allow a computer asking to pair with this one, by typing the code it shows (asked for when left out).
        /// Refused when stdin is not a terminal. Name the computer before the flag (`kith link NAME --accept`):
        /// after it, the name would be read as the code.
        #[arg(long, num_args = 0..=1, value_name = "CODE", conflicts_with = "deny")]
        #[allow(clippy::option_option)] // clap's flag with an optional value
        accept: Option<Option<String>>,
        /// Refuse a computer asking to pair with this one.
        #[arg(long)]
        deny: bool,
    },
    /// End this computer's pairing with another one, both ways.
    Unlink {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        computer: String,
    },
    /// Post one of every card into a "Samples" chat (replacing the last one); their buttons do nothing.
    SampleCards {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Install and start the host as a background service (launchd / systemd --user).
    Install {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Stop and remove the background service.
    Uninstall,
    /// Stop the background service, keeping its configuration and all data.
    Stop,
    /// Update a standalone host, or inspect/configure its automatic updates.
    Update {
        #[arg(long, conflicts_with_all = ["status", "worker"])]
        check: bool,
        /// Read update progress without checking or installing a release.
        #[arg(long, conflicts_with = "worker")]
        status: bool,
        #[arg(long, value_parser = ["on", "off"], conflicts_with_all = ["check", "status", "worker"])]
        auto: Option<String>,
        /// Allow a manual update to interrupt active bots (their sessions resume).
        #[arg(long)]
        force: bool,
        /// Install even if this release stops serving the paired iPhones' app (they then ask
        /// for an app update).
        #[arg(long)]
        skip_app_check: bool,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        json: bool,
        #[arg(long, hide = true)]
        worker: bool,
    },
    /// The versions this host works with (JSON), asked of a downloaded update before installing it.
    #[command(hide = true)]
    Compat,
    /// Show whether the host is installed and running.
    Status {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Claude Code statusLine command: forwards usage limits to the host, then prints
    /// the wrapped command's status line (`statusline -- <command>`) or a short default.
    Statusline {
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        #[arg(last = true)]
        wrapped: Vec<String>,
    },
    /// Rotate the local API token used by this computer's own apps and helpers.
    ResetToken,
    /// Message your bots from this terminal.
    Tui {
        /// Host to connect to (default: this computer). The API only answers local
        /// callers: for another computer, forward its port with `ssh -L` and use the tunnel.
        #[arg(long)]
        url: Option<String>,
        /// Token for `--url` (default: this computer's `~/.kith/serve/token`).
        #[arg(long, env = "KITH_SERVE_TOKEN", hide_env_values = true)]
        token: Option<String>,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        /// Agent turns each message to a group may cause, rounds included (default 10).
        #[arg(long)]
        budget: Option<i64>,
    },
    /// npm on this computer's Node, else on Kith's own (downloaded first): what setup
    /// terminals install npm CLIs with.
    #[command(hide = true)]
    Npm {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Add Kith's own Node.js, for agents that run on it (an agent's setup step).
    #[command(hide = true)]
    Node,
    /// Check and version the agents' memory files: `check`, `log`, `diff`, `undo`, `forget`.
    Memory {
        #[command(subcommand)]
        cmd: hqmem::Cmd,
    },
    /// Where Kith's folders are (the same answer as the kernel's `lib/kith_paths.py`): one `NAME  folder` per line.
    Paths {
        /// Print JSON, with the old HQ_ names too.
        #[arg(long, conflicts_with = "sh")]
        json: bool,
        /// Print `export NAME=folder` lines for bash.
        #[arg(long)]
        sh: bool,
        /// The kernel folder, for the fallback when neither ~/kith nor ~/hq exists (default: the one this program finds).
        #[arg(long, hide = true)]
        kernel: Option<std::path::PathBuf>,
    },
    /// The kernel's rules in Rust, as the `kith` router in `kernel/bin/kith` calls them.
    #[command(hide = true)]
    Kernel {
        #[command(subcommand)]
        cmd: KernelCmd,
    },
    /// Built-in MCP servers that bots are started with (stdio).
    #[command(hide = true)]
    Mcp {
        #[command(subcommand)]
        server: McpServer,
    },
}

#[derive(Subcommand)]
enum DevicesAction {
    /// Show every allowed device (the default).
    List,
    /// Remove a device's access; it disconnects at once.
    Revoke { key: String },
}

#[derive(Subcommand)]
enum AccessAction {
    /// Show pending requests with the code each device shows (the default).
    List,
    /// Approve a request, by its id or by the 6-digit code shown on the device.
    Approve { request: String },
    /// Deny a request.
    Deny { request: String },
}

#[derive(Subcommand)]
enum McpServer {
    /// Launch a local connector without exposing its credentials in ACP config.
    Local {
        #[arg(long)]
        connector: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Invite the user to connect services securely.
    Connectors {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Scheduled and event-triggered bot routines.
    Routines {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Send the user chat messages.
    Chat {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Share files with the user (an hq agent's way to send an image or a document in its chat).
    Files {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Discover and ask the user's other bots for help.
    Team {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Search the bot's own chat history.
    Memory {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// Tools of the apps connected through Composio.
    Composio {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// A remote connector, with its sign-in handled by the host.
    Remote {
        #[arg(long)]
        connector: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
    /// See and operate this computer's desktop.
    Computer {
        #[arg(long)]
        bot: String,
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
    },
}

/// The database id + local API token live in the database; the token is mirrored to
/// `~/.kith/serve/token` (0600) for local helpers like the statusline command.
fn open_store() -> Result<(store::Store, String, String)> {
    let dir = service::data_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let db = dir.join("serve.db");
    let store = store::Store::open(&db).with_context(|| format!("opening {}", db.display()))?;
    let host_id = if let Some(v) = store.kv_get("host_id") {
        v
    } else {
        let v = uuid::Uuid::new_v4().to_string();
        store.kv_set("host_id", &v)?;
        v
    };
    let token = match store.kv_get("token") {
        Some(v) => v,
        None => rotate_token(&store)?,
    };
    Ok((store, host_id, token))
}

fn rotate_token(store: &store::Store) -> Result<String> {
    let t = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    store.kv_set("token", &t)?;
    write_token_file(&t)?;
    Ok(t)
}

fn write_token_file(token: &str) -> Result<()> {
    let path = service::data_dir().join("token");
    // Written aside and renamed, so a reader never sees the file empty or half-written.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, token).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// What `kith kernel <module> ...` can be asked.
#[derive(Subcommand)]
enum KernelCmd {
    /// The interface version and modules of this build: `{"abi":1,"modules":[...]}`.
    Version,
    /// Where Kith's folders are, as `kith paths`.
    Paths {
        #[arg(long, conflicts_with = "sh")]
        json: bool,
        #[arg(long)]
        sh: bool,
        /// The kernel folder, for the fallback when neither ~/kith nor ~/hq exists (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
    },
    /// Makes (or migrates) this Mac's core database, as the daemon's first start would, and releases it. The difftest uses it
    /// to run the same corpus through an embedded core.
    ///
    /// `--authority` also imports auth.json's access tables and the Keychain items its profiles name into the core database and the
    /// secrets envelope (M2-8): idempotent, a backup first, refuses rather than store a different answer. `--dry-run`
    /// prints what it would do and writes nothing; `--force` replaces authority rows the core already holds.
    #[command(hide = true)]
    CoreMigrate {
        #[arg(long)]
        authority: bool,
        #[arg(long, requires = "authority")]
        dry_run: bool,
        #[arg(long, requires = "authority")]
        force: bool,
    },
    /// The cutover framework (M3-10a): `step MODULE [--dry-run] [--force]`, `rollback MODULE [--force]`, `status`,
    /// `check MODULE`, `modules`. `kith migrate step|status` and `kith rollback` run it. No module is registered yet.
    #[command(hide = true)]
    Cutover {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        args: Vec<String>,
    },
    /// Runs the daemon front alone, in the foreground (what `kith serve` starts inside itself); ends when stdin closes.
    #[command(hide = true)]
    CoreDaemon,
    /// The daemon, its owner socket and the holder, or why there is no daemon and the verb that recovers it.
    #[command(hide = true)]
    CoreStatus {
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// The coding engines an agent can run on: installed or not, and how to install or sign in.
    Engines {
        /// Print JSON.
        #[arg(long, global = true)]
        json: bool,
        #[command(subcommand)]
        action: Option<EnginesAction>,
    },
    /// Backup-chain entries and the main account: readiness and the environment they launch with (read-only; the same
    /// answers as the kernel's `lib/kith_accounts.py`).
    Accounts {
        /// The kernel folder (default: the one this program finds).
        #[arg(long, global = true)]
        kernel: Option<std::path::PathBuf>,
        #[command(subcommand)]
        action: AccountsAction,
    },
    /// How far a chain of `kith ask`s may go: the hop caps and the chain cost cap, read-only (the answers of `kith-ask`'s
    /// own checks; not routed from `kith ask` yet).
    Delegation {
        /// The kernel folder (default: the one this program finds).
        #[arg(long, global = true)]
        kernel: Option<std::path::PathBuf>,
        #[command(subcommand)]
        action: DelegationAction,
    },
    /// Who may use which agent, and with what settings: the read half of `kith-auth` (`resolve`, `may`; a "just this once"
    /// helper grant is used up by `resolve --take-once`). Prints what `kith-auth` prints, with its exit codes.
    // -h/--help belong to kith-auth's own usage text, so clap must not answer them.
    #[command(disable_help_flag = true)]
    Secrets {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        /// `resolve <agent> [--person ID] [--via CALLER] [--take-once] [--json]` or `may <id> <agent> [--via CALLER] [--once]`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// What Claude Code usage costs, the verbs the launch and the apps call (`kernel/bin/kith-cost`): `limit AGENT [--json|--sh]`,
    /// `spent PERSON [AGENT]`, `usage [PERIOD] --json`, `agent NAME [PERIOD] --json`, `members AGENT`, `bots --json` and `sync`.
    /// Prints what `kith-cost` prints, with its exit codes (2: a verb the core does not have yet). Not routed from `kith` yet.
    // -h/--help belong to kith-cost's own usage text, so clap must not answer them.
    #[command(disable_help_flag = true)]
    Cost {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// The writer claim of a Claude session and the session registry: `record`, `resolve`, `claim`, `terminal`, `proc`,
    /// `sweep` as `kernel/bin/kith-sessions` runs them, plus `release` and `holder` (not routed from `kith` yet).
    // -h/--help belong to kith-sessions' own usage text, so clap must not answer them.
    #[command(disable_help_flag = true)]
    Threads {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        args: Vec<String>,
    },
    /// The kith project of a directory, as `kernel/bin/project-id` answers it (not routed from `kith` yet; a guest run is
    /// refused here and stays with the Python, which asks the host over the run socket).
    #[command(name = "project-id", disable_help_flag = true)]
    ProjectId {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// `kith-pkg resolve`: the pinned package folder a run starts from (not routed from `kith` yet).
    #[command(disable_help_flag = true)]
    Pkg {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        args: Vec<String>,
    },
    /// `kernel/bin/kith-permit`'s command-line verbs: `issue AGENT SESSION PID`, `denied --dir DIR`, `once --dir DIR` and
    /// `apply SETTINGS_FILE`; any other line is the stdio MCP server (`approve`, `request_access`, `request_secret`, `report_bug`,
    /// `ask_helper`, `run_door`). Prints what `kith-permit` prints, with its exit codes. Not routed from `kith` yet.
    #[command(disable_help_flag = true)]
    Permit {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// What a launch assembles, one piece at a time (not routed from `kith AGENT` yet).
    Launch {
        #[command(subcommand)]
        what: LaunchCmd,
    },
    /// kith's own settings, the command line of `kernel/lib/kith_settings.py`: `KEY [AGENT]`, `call_model`, `accounts`, `pin`,
    /// `limit AGENT`, `delegation.chain_cost_cap_usd`, `cap MODE CAPS_FILE`, `reads-others CAPS_FILE` and the writes `set permission_mode|
    /// call_model|at_limit [AGENT] VALUE`, `set limit AGENT N|none`, `set delegation.chain_cost_cap_usd N|none|default`. Prints what
    /// `kith_settings.py` prints, with its exit codes. Not routed from `kith` yet.
    // -h/--help belong to kith_settings.py's own usage text, so clap must not answer them.
    #[command(disable_help_flag = true)]
    Settings {
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
        args: Vec<String>,
    },
    /// Attaches this terminal to an agent's held terminal through the relay (M1-6): raw mode until detach or exit.
    /// The ticket (from `terminal.open`) is one line on stdin. Exit: the engine's code, 75 busy, 0 detached, 76 cut as too slow (attach again), 77 taken over, 78 holder lost, 1 otherwise.
    #[command(hide = true)]
    Attach {
        /// The relay's attach endpoint: a socket path, or a pipe name on Windows.
        #[arg(long)]
        endpoint: std::path::PathBuf,
        /// Read the ticket from the first line of stdin (the only way to give it; never argv).
        #[arg(long)]
        ticket_stdin: bool,
        /// Watch only: no keys are sent and the size never changes.
        #[arg(long)]
        observer: bool,
        /// Take the owner place from a client that has it (an owner ticket only).
        #[arg(long)]
        takeover: bool,
        /// The key that leaves the terminal running: `ctrl-<char>`.
        #[arg(long, env = "KITH_DETACH_KEY", default_value = "ctrl-\\")]
        detach_key: String,
        /// Read keys from the controlling terminal (/dev/tty, CONIN$) instead of stdin, for when stdin is the ticket pipe.
        #[arg(long)]
        tty: bool,
    },
    /// Calls one of the held-terminal methods (`terminal.open`, `terminal.attach`, `terminal.list`) on the running core
    /// daemon's owner socket and prints its JSON answer (M1-7, the bash `kith --held` and `kith attach`). Owner-only by
    /// construction: it speaks the owner socket and nothing else, and refuses any other method. Params are JSON on stdin
    /// (never argv: the launch env is in them). Exit: 0 answered; 3 no daemon answers (nothing ran); 4 the core refused
    /// the call (stderr says why); 5 the connection broke after the call went out (it may have run); 2 usage.
    #[command(hide = true)]
    CoreCall {
        /// `terminal.open`, `terminal.attach` or `terminal.list`.
        method: String,
        /// Read the params (a JSON object) from stdin; the only way to give them.
        #[arg(long)]
        params_stdin: bool,
    },
    /// One task on one engine over ACP: the answer on stdout (the task after `--`, or on stdin).
    RunOnce {
        engine: String,
        #[arg(long)]
        model: Option<String>,
        /// The folder the engine works in (default: here).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// MCP servers for the run: `{"mcpServers": {...}}` as JSON or a file holding it.
        #[arg(long)]
        mcp_config: Option<String>,
        /// Print `{text, stop_reason, usage, cost_usd, session_id}`.
        #[arg(long)]
        json: bool,
        /// Allow the tools the engine asks to use (otherwise it is refused).
        #[arg(long)]
        approve: bool,
        /// Seconds the whole run may take.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Start this ACP command line instead of the engine's own (tests; also `KITH_RUN_ONCE_ACP_COMMAND`).
        #[arg(long)]
        acp_command: Option<String>,
        #[arg(last = true)]
        task: Vec<String>,
    },
}

/// `kith kernel attach ...`; the exit code is the client's (see `kith-attach`).
// The flags are the command line's own switches.
#[allow(clippy::fn_params_excessive_bools)]
fn run_attach(
    endpoint: &std::path::Path,
    ticket_stdin: bool,
    observer: bool,
    takeover: bool,
    detach_key: &str,
    tty: bool,
) -> i32 {
    use std::io::BufRead;
    let key = match kith_attach::parse_detach_key(detach_key) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if !ticket_stdin {
        eprintln!("the ticket is read from stdin: pass --ticket-stdin");
        return 2;
    }
    // One line, from the shared stdin buffer: whatever follows stays there for the keys (when stdin is also the keyboard).
    let mut ticket = String::new();
    if std::io::stdin().lock().read_line(&mut ticket).unwrap_or(0) == 0 || ticket.trim().is_empty() {
        eprintln!("no ticket on stdin");
        return 2;
    }
    let opts = kith_attach::Opts {
        ticket: ticket.trim().to_string(),
        role: if observer { kith_attach::Role::Observer } else { kith_attach::Role::Owner },
        takeover,
        detach_key: key,
        tty_input: tty,
    };
    kith_attach::run(endpoint, &opts)
}

/// What `kith kernel delegation ...` can be asked.
#[derive(Subcommand)]
enum DelegationAction {
    /// The caps: `{max_local_hops, max_cross_hops, refused_exit, default_cap_usd}`.
    Limits,
    /// The chain cost cap in dollars (`none` for no cap), as `kith ask --cap` shows it.
    Cap,
    /// The delegation record of ask ID, as the code reads it (an old registration reads as a root ask); exit 1 when
    /// there is none.
    Record { id: String },
    /// Whether the ask running now (this environment's `KITH_ASK_*` variables) may start another ask. Prints the chain, hop and
    /// cross hops its record would get, or the refusal on stderr and exit 4.
    Check {
        /// The ask's id (default: a probe).
        #[arg(long, default_value = "ask-check")]
        id: String,
        /// The ask goes to an agent on another Mac.
        #[arg(long)]
        crossing: bool,
    },
    /// Whether ask ID waits on a card: `true` or `false`.
    Parked { id: String },
}

/// The verbs of `lib/kith_accounts.py` that read, and `kith account list`.
#[derive(Subcommand)]
enum AccountsAction {
    /// The chain for `kith AGENT --backups-json`.
    BackupsJson { agent: String, agent_md: String },
    /// Shell assignments for a backup chain entry; exit 4 and one line on stderr when it can't run.
    LaunchEnv {
        agent: String,
        entry: String,
        /// The launch is real: note `last_started` for the account.
        #[arg(long)]
        record: bool,
    },
    /// `main_id=ID` plus the assignments of the agent's main account; exit 4 when it can't run.
    MainEnv {
        agent: String,
        agent_md: String,
        /// Start on `pick`'s first candidate (accounts that are out are skipped), not on the static main.
        #[arg(long)]
        state: bool,
        /// The launch is real: note `last_started` for the account.
        #[arg(long)]
        record: bool,
        /// The account a handed-on session was made under: goes first when it is one of the candidates.
        #[arg(long)]
        prefer: Option<String>,
    },
    /// The harness, model and effort a session started with (`{}` when the cost ledger doesn't know it).
    SessionRun {
        sid: String,
        /// Shell assignments for `bin/kith`.
        #[arg(long)]
        sh: bool,
    },
    /// The account id a session was made under (empty when the cost ledger doesn't know it).
    SessionAccount { sid: String },
    /// Each extra account and whether it is logged in (`kith account list`).
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        /// That agent's order, and where `pick` would start.
        #[arg(long, default_value = "")]
        agent: String,
    },
}

/// `kith kernel accounts <verb>`; the exit code (4 for "can't run", as the Python verbs).
fn print_accounts(kernel: Option<std::path::PathBuf>, action: AccountsAction) -> i32 {
    use kith_kernel::accounts::{self, Ctx, System, sh_line};
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let p = kith_kernel::paths::paths(&home, &kernel, &kith_kernel::paths::process_env);
    let system = System { kernel: kernel.clone() };
    let ctx = Ctx {
        machine: p.machine.clone(),
        agents: p.agents.clone(),
        kernel,
        home: home.clone(),
        claude_config_dir: std::env::var("CLAUDE_CONFIG_DIR").ok(),
        out: &system,
    };
    let result: Result<String, String> = match action {
        AccountsAction::BackupsJson { agent, agent_md } => {
            Ok(accounts::chain(&ctx, &agent, std::path::Path::new(&agent_md)).dumps())
        }
        AccountsAction::LaunchEnv { agent, entry, record } => accounts::launch_env(&ctx, &agent, &entry, false, record),
        AccountsAction::MainEnv { agent, agent_md, state, record, prefer } => accounts::main_env(
            &ctx,
            &agent,
            std::path::Path::new(&agent_md),
            accounts::MainOpts { state, record, prefer: prefer.as_deref() },
        ),
        AccountsAction::SessionRun { sid, sh } => Ok(match (accounts::session_run(&ctx.machine, &sid), sh) {
            (Some(r), true) => r.sh(),
            (Some(r), false) => r.json().dumps(),
            (None, true) => sh_line("", None, None),
            (None, false) => "{}".to_owned(),
        }),
        AccountsAction::SessionAccount { sid } => Ok(accounts::session_account(&ctx.machine, &sid).unwrap_or_default()),
        AccountsAction::List { json, agent } => match accounts::list(&ctx, json, &agent) {
            Ok(text) => {
                print!("{text}");
                return 0;
            }
            Err(e) => {
                // a usage error, as `kith account list` dies with
                eprintln!("kith: {e}");
                return 2;
            }
        },
    };
    match result {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("kith: {e}");
            4
        }
    }
}

/// `kith kernel delegation <verb>`; the refusal exits 4 (`REFUSED`), as `kith ask` does.
fn print_delegation(kernel: Option<std::path::PathBuf>, action: DelegationAction) -> i32 {
    use kith_kernel::delegation as d;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let machine = kith_kernel::paths::paths(&home, &kernel, &kith_kernel::paths::process_env).machine;
    match action {
        DelegationAction::Limits => println!("{}", d::limits_json()),
        DelegationAction::Cap => {
            println!("{}", d::chain_cost_cap(&machine).map_or_else(|| "none".to_owned(), d::pct_g));
        }
        DelegationAction::Record { id } => {
            match d::safe_id(&id).then(|| d::read_reg(&d::box_dir(&machine), &id)).flatten() {
                Some(reg) => println!("{}", serde_json::Value::Object(reg)),
                None => return 1,
            }
        }
        DelegationAction::Check { id, crossing } => {
            match d::check(&machine, &kith_kernel::paths::process_env, &id, crossing) {
                Ok(v) => println!("{v}"),
                Err(d::Refusal(why)) => {
                    eprintln!("kith ask: {why}");
                    return d::REFUSED;
                }
            }
        }
        DelegationAction::Parked { id } => println!("{}", d::safe_id(&id) && d::parked(&machine, &id)),
    }
    0
}

/// `kith kernel secrets <verb> ...`: the answers of `kith-auth may` and `kith-auth resolve`, with its exit codes.
/// `kith kernel core-migrate --authority`: the import of auth.json and the Keychain items into the core (exit code).
#[cfg(unix)]
fn core_migrate_authority(p: &kith_kernel::paths::Paths, dry_run: bool, force: bool) -> i32 {
    let home = kith_kernel::paths::process_home();
    let kernel = kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
    let ctx = kith_kernel::secrets::Ctx::from_process(&kernel);
    match kith_core::migrate_authority(p, &ctx, kith_core::AuthorityOptions { dry_run, force }) {
        Ok(report) => {
            for line in &report.lines {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(not(unix))]
fn core_migrate_authority(_: &kith_kernel::paths::Paths, _: bool, _: bool) -> i32 {
    eprintln!("the authority import is not available on this operating system yet");
    1
}

/// `kith kernel cutover ...`: the lines of the framework's answer (exit code).
#[cfg(unix)]
fn cutover_verb(p: &kith_kernel::paths::Paths, args: &[String]) -> i32 {
    let home = kith_kernel::paths::process_home();
    let kernel = kernel::current_kernel().unwrap_or_else(|| home.join("kith/kernel"));
    match kith_core::cutover(p, &kernel, args) {
        Ok(lines) => {
            for line in &lines {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(not(unix))]
fn cutover_verb(_: &kith_kernel::paths::Paths, _: &[String]) -> i32 {
    eprintln!("the cutover framework is not available on this operating system yet");
    1
}

fn print_secrets(kernel: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    use std::io::Write as _;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let ctx = kith_kernel::secrets::Ctx::from_process(&kernel);
    // No verb is `kith-auth` with no arguments: its usage text and exit 0 (not clap's usage error).
    let (verb, rest) = args.split_first().map_or(("--help", &[][..]), |(v, r)| (v.as_str(), r));
    let out = kith_kernel::secrets::run(&ctx, verb, rest);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

/// `kith kernel cost <verb ...>`: `kith-cost`'s `limit`, `spent` and the apps' `--json` views from the `kith-kernel` crate.
fn print_cost(kernel: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    use std::io::Write as _;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let paths = kith_kernel::cost::Paths::from_process(&kernel);
    let out = kith_kernel::cost::run(&paths, &kith_kernel::cost::Env::from_process(), args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

/// `kith kernel permit ARGS...`: what `kith-permit`'s verbs print, with its exit codes. The hooks read their payload from stdin.
fn print_permit(kernel: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    use std::io::Write as _;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let env = kith_kernel::permit::Env::from_process();
    if !kith_kernel::permit::is_verb(args) {
        // the stdio MCP server answers line by line, so it writes to the terminal itself
        return kith_kernel::permit::serve_stdio(&env, &kernel);
    }
    let out = kith_kernel::permit::run(&env, &kernel, args, &mut std::io::stdin().lock());
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

fn project_id_cmd(kernel: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    use std::io::Write as _;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let ctx = kith_kernel::project::Ctx {
        state: kith_kernel::paths::paths(&home, &kernel, &kith_kernel::paths::process_env).state,
        home,
        cwd: std::env::current_dir().unwrap_or_default(),
        guest: kith_kernel::project::is_guest(|n| std::env::var_os(n)),
    };
    let out = kith_kernel::project::run(&ctx, args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

/// `kith kernel settings ARGS...`: what `kith_settings.py` prints, with its exit codes.
fn print_settings(kernel: Option<std::path::PathBuf>, args: &[String]) -> i32 {
    use kith_kernel::accounts::{Ctx, System};
    use std::io::Write as _;
    let home = kith_kernel::paths::process_home();
    let kernel = kernel.or_else(kernel::current_kernel).unwrap_or_else(|| home.join("kith/kernel"));
    let p = kith_kernel::paths::paths(&home, &kernel, &kith_kernel::paths::process_env);
    let system = System { kernel: kernel.clone() };
    let ctx = Ctx {
        machine: p.machine.clone(),
        agents: p.agents.clone(),
        kernel,
        home,
        claude_config_dir: std::env::var("CLAUDE_CONFIG_DIR").ok(),
        out: &system,
    };
    let out = kith_kernel::settings::run(&ctx, kith_kernel::settings::Env::from_process(), args);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    let _ = std::io::stdout().flush();
    out.code
}

/// `kith kernel launch <piece>`.
#[derive(Subcommand)]
enum LaunchCmd {
    /// The system prompt a run of the agent gets (`kith AGENT --launch-json`'s `system_prompt`). Reads the agent's
    /// folders; writes nothing and checks no access (`kith-auth` does at launch).
    Prompt {
        agent: String,
        /// Whom the run is for (`KITH_PERSON`); the owner when left out.
        #[arg(long)]
        person: Option<String>,
        /// The project the run is in (`-p`). Finding it from the working folder stays with `project-id`.
        #[arg(long)]
        project: Option<String>,
        /// The pinned package folder (default: the agent's live folder).
        #[arg(long)]
        pkg: Option<std::path::PathBuf>,
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        /// The working folder shown in the prompt (default: `$PWD`).
        #[arg(long)]
        cwd: Option<String>,
        /// The date shown in the prompt, `YYYY-MM-DD` (default: today).
        #[arg(long)]
        date: Option<String>,
        /// Print `{"prompt": "..."}`.
        #[arg(long)]
        json: bool,
    },
    /// The settings a run is handed (`kith AGENT --launch-json`'s `settings`, with `permission_mode`, `allowed_tools`,
    /// `add_dirs`, `disable_slash_commands`). Always JSON. Asks `kith-auth resolve` who a person is, as the launch does.
    Settings {
        agent: String,
        /// Whom the run is for (`KITH_PERSON`); the owner when left out.
        #[arg(long)]
        person: Option<String>,
        /// The permission mode the caller asked for (`KITH_PERMISSION_MODE`).
        #[arg(long)]
        mode: Option<String>,
        /// The pinned package folder (default: the agent's live folder).
        #[arg(long)]
        pkg: Option<std::path::PathBuf>,
        /// Pin the package first (`kith-pkg resolve`), as the launch does, and use the snapshot.
        #[arg(long)]
        snapshot: bool,
        /// The kernel folder (default: the one this program finds).
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        /// The project the run is in (`-p`).
        #[arg(long)]
        project: Option<String>,
        /// The working folder (default: `$PWD`).
        #[arg(long)]
        cwd: Option<String>,
        /// The engine (default: agent.md's `harness:`).
        #[arg(long)]
        harness: Option<String>,
        /// The kith program that answers `kith memory hook` (default: `KITH_BIN`, else this program).
        #[arg(long)]
        kith_bin: Option<String>,
        /// A file holding `claude plugin list --json` (default: ask `claude`).
        #[arg(long)]
        plugins_json: Option<std::path::PathBuf>,
        /// The owner at their own terminal: their plugins stay as their settings have them.
        #[arg(long)]
        terminal: bool,
        /// The `apiKeyHelper` of a claude-api backup.
        #[arg(long)]
        api_helper: Option<String>,
        /// The id for the folder of a once-allowed retry (default: a fresh one).
        #[arg(long)]
        once_id: Option<String>,
        /// A file holding the output of `kith-auth resolve` (default: run it).
        #[arg(long)]
        resolve: Option<std::path::PathBuf>,
        /// Print JSON (always).
        #[arg(long)]
        json: bool,
    },
    /// The MCP servers a run is handed (`kith AGENT --launch-json`'s `mcp_servers`): the granted shared services
    /// (`kith-auth mcp-config`, run here) plus the agent's own `mcp.json`. Always JSON.
    Mcp {
        agent: String,
        #[arg(long)]
        person: Option<String>,
        #[arg(long)]
        pkg: Option<std::path::PathBuf>,
        /// Pin the package first (`kith-pkg resolve`), as the launch does, and use the snapshot.
        #[arg(long)]
        snapshot: bool,
        #[arg(long)]
        kernel: Option<std::path::PathBuf>,
        /// A file holding the output of `kith-auth mcp-config` (default: run it).
        #[arg(long)]
        granted: Option<std::path::PathBuf>,
        /// A file holding the output of `kith-auth resolve` (default: run it).
        #[arg(long)]
        resolve: Option<std::path::PathBuf>,
        /// Print JSON (always).
        #[arg(long)]
        json: bool,
    },
    /// `kith kernel launch AGENT --launch-json [flags]`: the whole launch object, as `kith AGENT --launch-json` prints it
    /// (`--kernel DIR` may stand among the flags).
    #[command(external_subcommand)]
    Whole(Vec<String>),
}

/// What `kith kernel launch prompt` was asked.
struct LaunchPromptArgs {
    agent: String,
    person: Option<String>,
    project: Option<String>,
    pkg: Option<std::path::PathBuf>,
    kernel: Option<std::path::PathBuf>,
    cwd: Option<String>,
    date: Option<String>,
    json: bool,
}

/// `kith kernel launch prompt`: the prompt a launch assembles, from the `kith-kernel` crate.
fn print_launch_prompt(a: LaunchPromptArgs) -> Result<()> {
    use kith_kernel::launch::prompt;
    let run = |prog: &str, args: &[&str]| -> Option<String> {
        let o = std::process::Command::new(prog).args(args).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let agent = a.agent.as_str();
    let home = kith_kernel::paths::process_home();
    let (p, kernel_dir) = match a.kernel {
        Some(k) => (kith_kernel::paths::paths(&home, &k, &kith_kernel::paths::process_env), k),
        None => (
            kernel::folders(&home),
            kernel::current_kernel().ok_or_else(|| anyhow::anyhow!("kith: can't find the kernel folder (--kernel)"))?,
        ),
    };
    let src_pkg = prompt::agent_dir(&p, &kernel_dir, agent)
        .ok_or_else(|| anyhow::anyhow!("kith: no agent named \"{agent}\"."))?;
    let auth_file = kith_kernel::paths::process_env("KITH_AUTH")
        .filter(|f| !f.is_empty())
        .map_or_else(|| p.secrets.join("auth.json"), std::path::PathBuf::from);
    let auth = std::fs::read_to_string(auth_file).ok().and_then(|t| serde_json::from_str(&t).ok());
    let who = prompt::Who::resolve(auth.as_ref(), a.person.as_deref());
    let registry = prompt::Registry::load(&p.state).map_err(|e| anyhow::anyhow!(e))?;
    let ctmp_note = if who.owner {
        String::new()
    } else {
        use sha2::{Digest as _, Sha256};
        let hash = |s: &str| {
            use std::fmt::Write as _;
            Sha256::digest(s.as_bytes()).iter().fold(String::new(), |mut a, b| {
                let _ = write!(a, "{b:02x}");
                a
            })
        };
        let uid = run("id", &["-u"]).and_then(|u| u.parse().ok()).unwrap_or(0);
        let machine = kith_kernel::paths::realpath(&p.machine);
        if prompt::guest_tmp(&machine, &who.id, uid, &hash).is_some() {
            String::new()
        } else {
            prompt::ctmp_note(&p.machine, &who.id)
        }
    };
    let cwd = a
        .cwd
        .or_else(|| std::env::var("PWD").ok().filter(|d| !d.is_empty()))
        .or_else(|| std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let date = a.date.or_else(|| run("date", &["+%F"])).unwrap_or_default();
    let input = prompt::Input {
        agent,
        paths: &p,
        kernel: &kernel_dir,
        src_pkg: &src_pkg,
        pkg: a.pkg.as_deref().unwrap_or(&src_pkg),
        who: &who,
        project: a.project.as_deref().unwrap_or(""),
        registry: &registry,
        cwd: &cwd,
        date: &date,
        ctmp_note: &ctmp_note,
        user_home: &home,
        env: &kith_kernel::paths::process_env,
    };
    let text = prompt::build(&input).map_err(|e| anyhow::anyhow!(e))?;
    if a.json {
        println!("{}", json!({ "prompt": text }));
    } else {
        println!("{text}");
    }
    Ok(())
}

/// `kith engines <action> <id>`: print the command, never run it.
#[derive(Subcommand)]
enum EnginesAction {
    /// The command that installs the engine on this platform.
    Install { id: String },
    /// The command that signs in to the engine.
    Login { id: String },
    /// How to start the engine for a run (`kith <agent> --launch-json` copies it): its ACP program, role prompt delivery,
    /// whether it serves guests. Always JSON.
    Launch { id: String },
}

/// `kith engines`: the kernel crate's table with this computer's PATH.
fn print_engines(json: bool, action: Option<EnginesAction>) -> Result<()> {
    use kith_kernel::engines;
    let which = |bin: &str| backends::which(bin);
    let Some(action) = action else {
        println!(
            "{}",
            if json { engines::list_json(&which) } else { engines::list_text(&which).trim_end().to_owned() }
        );
        return Ok(());
    };
    let (EnginesAction::Install { id } | EnginesAction::Login { id } | EnginesAction::Launch { id }) = &action;
    let engine = engines::find(id).ok_or_else(|| anyhow::anyhow!("no engine called {id} (kith engines lists them)"))?;
    let line = match action {
        EnginesAction::Install { .. } => engines::install_text(engine, cfg!(unix)),
        EnginesAction::Login { .. } => engines::login_text(engine, &which),
        EnginesAction::Launch { .. } => Ok(engines::launch_spec(engine, &which).to_string()),
    };
    println!("{}", line.map_err(|e| anyhow::anyhow!(e))?);
    Ok(())
}

/// Runs a verb the Rust core already answers inside an embedded kith-core (its lock, its checks, a new generation); the
/// verb runs bare when no core opens here (a daemon holds the lock, or this is a first setup). M0b replaces this with
/// a call through the method table.
fn through_core<T>(verb: impl FnOnce() -> T) -> T {
    let p = kernel::folders(&kith_kernel::paths::process_home());
    kith_core::stateless_through_core(&p, verb).0
}

/// `kith paths`: the folders by the `kith-kernel` crate's rules, in the form `kith_paths.py` prints.
fn print_paths(json: bool, sh: bool, kernel: Option<&std::path::Path>) {
    let home = kith_kernel::paths::process_home();
    let p = match kernel {
        Some(k) => kith_kernel::paths::paths(&home, k, &kith_kernel::paths::process_env),
        None => kernel::folders(&home),
    };
    print!(
        "{}",
        if json {
            format!("{}\n", p.json())
        } else if sh {
            p.sh()
        } else {
            p.text()
        }
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    // Two crypto backends reach rustls (reqwest pulls aws-lc-rs, the guest crate and ureq pull ring), so rustls can't
    // pick one by itself and every TLS client without an explicit provider panics (the Discord gateway did, 0.1.1).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    agent::acp::init();
    let args: Vec<_> = std::env::args_os().collect();
    let cli = match kernel::route(&args) {
        kernel::Route::Host(a) => {
            // The daemon answers first when one runs and the table says the core serves the verb; otherwise clap and the
            // embedded verbs below do exactly what they did before.
            if let Some(code) = core_front::try_cli(&a) {
                std::process::exit(code);
            }
            Cli::parse_from(a)
        }
        kernel::Route::Kernel(a) => return Err(kernel::exec_kernel(&a)),
        kernel::Route::Version => {
            println!("kith {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
    };
    match cli.cmd.unwrap_or(Sub::Serve { port: service::DEFAULT_PORT, bind: "0.0.0.0".into() }) {
        Sub::Serve { port, bind } => serve(&bind, port).await,
        Sub::Pair { port, json } => pair(port, json).await,
        Sub::Info { port, json } => info(port, json).await,
        Sub::Devices { port, action } => match action.unwrap_or(DevicesAction::List) {
            DevicesAction::List => {
                let v = local_call(port, "devices", json!({})).await?;
                for d in v["devices"].as_array().into_iter().flatten() {
                    println!(
                        "{}  {} ({}, {}){}",
                        d["key"].as_str().unwrap_or_default(),
                        d["name"].as_str().unwrap_or_default(),
                        d["platform"].as_str().unwrap_or_default(),
                        d["source"].as_str().unwrap_or_default(),
                        if d["connected"] == true { "  connected" } else { "" }
                    );
                }
                Ok(())
            }
            DevicesAction::Revoke { key } => {
                local_call(port, "revokeDevice", json!({"key": key})).await?;
                println!("Revoked.");
                Ok(())
            }
        },
        Sub::Access { port, action } => access(port, action.unwrap_or(AccessAction::List)).await,
        Sub::Link { port, computer, accept, deny } => match (accept, deny) {
            (Some(code), _) => answer_link(port, computer, LinkAnswer::Accept(code)).await,
            (None, true) => answer_link(port, computer, LinkAnswer::Deny).await,
            (None, false) => match computer {
                Some(c) => link(port, &c).await,
                None => links(port).await,
            },
        },
        Sub::Unlink { port, computer } => {
            local_call(port, "unlinkComputer", json!({ "computerId": computer })).await?;
            println!("Unpaired.");
            Ok(())
        }
        Sub::Cloud { port, enable, disable, approval, confirm_owner } => {
            if let Some(approval) = approval {
                local_call(port, "setApproval", json!({ "approval": approval })).await?;
            }
            if confirm_owner {
                confirm_owner_here(port).await?;
            }
            let status = if enable || disable {
                local_call(port, "setCloud", json!({"enabled": !disable})).await?
            } else {
                local_call(port, "cloudStatus", json!({})).await?
            };
            let text = |k: &str| status[k].as_str().unwrap_or("-").to_owned();
            let owner = status["owner"]["email"].as_str().or(status["owner"]["userId"].as_str()).unwrap_or("none");
            println!(
                "enabled:    {}\nurl:        {}\nregistered: {}\nconnected:  {}\naccount:    {owner}\napproval:   {}\nlast error: {}",
                status["enabled"],
                text("url"),
                status["registered"],
                status["connected"],
                text("approval"),
                text("lastError"),
            );
            if let Some((_, says)) = owner_to_confirm(&status) {
                println!("{says} Confirm at this terminal: kith-serve cloud --confirm-owner");
            }
            Ok(())
        }
        Sub::Install { port } => {
            open_store()?;
            // Windows: the shell Claude Code runs status lines in varies (Git Bash, PowerShell), so
            // the line stays the user's; usage still comes from `claude -p /usage` and ACP.
            if cfg!(unix)
                && let Some(home) = dirs::home_dir()
            {
                // Wrap only from the stable installed binary; cargo target paths would leave a
                // dangling wrapper, so they only repair one.
                let host = service::current_exe().ok().filter(|exe| update::is_standalone(exe));
                match service::ensure_statusline(&home.join(".claude/settings.json"), host.as_deref()) {
                    Ok(true) => {
                        println!("Claude Code's status line settings updated (your own status line still shows).");
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!("Couldn't set Claude Code's status line: {e}"),
                }
            }
            service::install(port)?;
            println!("Kith host installed and started on port {port}. Run `kith-serve pair` to connect your phone.");
            Ok(())
        }
        Sub::Compat => {
            println!(
                "{}",
                serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "protocol": compat::PROTOCOL, "minProtocol": compat::MIN_PROTOCOL, "cli": kernel::current_cli().json()})
            );
            Ok(())
        }
        Sub::Update { check, status, auto, force, skip_app_check, port, json, worker } => {
            let status = if let Some(auto) = auto {
                update::set_automatic(auto == "on")?
            } else if status {
                update::status()?
            } else if check {
                update::check().await?
            } else if !worker && service::installed() {
                tokio::task::spawn_blocking(move || update::spawn_worker(port, force, skip_app_check)).await??;
                update::status()?
            } else {
                update::apply(port, force, skip_app_check, worker).await?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "Host {} · {}",
                    status["currentVersion"].as_str().unwrap_or("unknown"),
                    status["state"]["phase"].as_str().unwrap_or("ready")
                );
                println!("Automatic updates: {}", if status["automatic"] == true { "on" } else { "off" });
                if let Some(version) = status["state"]["availableVersion"].as_str() {
                    println!("Available version: {version}");
                }
                if let Some(required) = status["state"]["requiredProtocol"].as_u64() {
                    println!(
                        "Waiting for the iPhones: this release serves apps on protocol {required} and up. \
                         Update the iPhone app, then `kith-serve update --skip-app-check`"
                    );
                }
                if let Some(error) = status["state"]["error"].as_str() {
                    println!("Last error: {error}");
                }
            }
            Ok(())
        }
        Sub::Stop => {
            tokio::task::spawn_blocking(service::stop).await??;
            println!("Kith host stopped.");
            Ok(())
        }
        Sub::Uninstall => {
            service::uninstall();
            if let Some(home) = dirs::home_dir()
                && let Err(e) = service::restore_statusline(&home.join(".claude/settings.json"))
            {
                eprintln!("Couldn't restore Claude Code's status line: {e:#}");
            }
            println!("Kith host service removed. Data is kept in {}", service::data_dir().display());
            Ok(())
        }
        Sub::Status { port } => {
            let running = http()
                .get(format!("http://127.0.0.1:{port}/health"))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            println!(
                "installed: {}\nrunning:   {running}\nport:      {port}\ndata:      {}",
                service::installed(),
                service::data_dir().display()
            );
            Ok(())
        }
        Sub::SampleCards { port } => {
            let got = local_call(port, "sampleCards", json!({})).await?;
            println!(
                "Posted {} sample cards to the Samples chat. Its approval cards show under Needs you until you delete the chat.",
                got["cards"]
            );
            Ok(())
        }
        Sub::Memory { cmd } => std::process::exit(hqmem::run(cmd)?),
        Sub::Paths { json, sh, kernel } => {
            through_core(|| print_paths(json, sh, kernel.as_deref()));
            Ok(())
        }
        Sub::Kernel { cmd } => {
            if let KernelCmd::RunOnce { engine, model, cwd, mcp_config, json, approve, timeout, acp_command, task } =
                cmd
            {
                let args = runonce::Args { engine, model, cwd, mcp_config, json, approve, timeout, acp_command, task };
                std::process::exit(runonce::run(args).await?);
            }
            if let KernelCmd::CoreCall { method, params_stdin } = cmd {
                std::process::exit(core_front::core_call(&method, params_stdin));
            }
            if let KernelCmd::Attach { endpoint, ticket_stdin, observer, takeover, detach_key, tty } = cmd {
                std::process::exit(run_attach(&endpoint, ticket_stdin, observer, takeover, &detach_key, tty));
            }
            // Every kernel verb's notes (the accounts state, the settings migration) reach core.log, also the ones that
            // exit before `through_core`.
            kith_core::route_kernel_notes(&kernel::folders(&kith_kernel::paths::process_home()));
            if let KernelCmd::CoreMigrate { authority, dry_run, force } = cmd {
                let p = kernel::folders(&kith_kernel::paths::process_home());
                // A dry run writes nothing, so it does not migrate the schema either.
                if !dry_run {
                    match kith_core::migrate(&p) {
                        Ok(m) => println!("the core is at schema {}", m.to),
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
                if authority {
                    std::process::exit(core_migrate_authority(&p, dry_run, force));
                }
                return Ok(());
            }
            if let KernelCmd::Cutover { args } = cmd {
                std::process::exit(cutover_verb(&kernel::folders(&kith_kernel::paths::process_home()), &args));
            }
            if matches!(cmd, KernelCmd::CoreDaemon) {
                std::process::exit(core_front::run_foreground());
            }
            if let KernelCmd::CoreStatus { json } = cmd {
                core_front::print_status(json);
                return Ok(());
            }
            if let KernelCmd::Secrets { kernel, args } = cmd {
                std::process::exit(print_secrets(kernel, &args));
            }
            if let KernelCmd::Cost { kernel, args } = cmd {
                std::process::exit(print_cost(kernel, &args));
            }
            if let KernelCmd::Accounts { kernel, action } = cmd {
                std::process::exit(print_accounts(kernel, action));
            }
            if let KernelCmd::Delegation { kernel, action } = cmd {
                std::process::exit(print_delegation(kernel, action));
            }
            if let KernelCmd::Threads { kernel, args } = cmd {
                std::process::exit(threads_cmd::run(kernel, &args));
            }
            if let KernelCmd::ProjectId { kernel, args } = cmd {
                std::process::exit(project_id_cmd(kernel, &args));
            }
            if let KernelCmd::Pkg { kernel, args } = cmd {
                std::process::exit(pkg_cmd::run(kernel, &args));
            }
            if let KernelCmd::Permit { kernel, args } = cmd {
                std::process::exit(print_permit(kernel, &args));
            }
            through_core(|| -> Result<()> {
                match cmd {
                    KernelCmd::Version => println!("{}", kith_kernel::version_json()),
                    KernelCmd::Paths { json, sh, kernel } => print_paths(json, sh, kernel.as_deref()),
                    KernelCmd::Engines { json, action } => print_engines(json, action)?,
                    KernelCmd::Launch {
                        what: LaunchCmd::Prompt { agent, person, project, pkg, kernel, cwd, date, json },
                    } => {
                        print_launch_prompt(LaunchPromptArgs { agent, person, project, pkg, kernel, cwd, date, json })?;
                    }
                    KernelCmd::Launch {
                        what:
                            LaunchCmd::Settings {
                                agent,
                                person,
                                mode,
                                pkg,
                                snapshot,
                                kernel,
                                project,
                                cwd,
                                harness,
                                kith_bin,
                                plugins_json,
                                terminal,
                                api_helper,
                                once_id,
                                resolve,
                                json: _,
                            },
                    } => launch_cmd::print_settings(&launch_cmd::Args {
                        agent,
                        person,
                        mode,
                        pkg,
                        snapshot,
                        kernel,
                        project,
                        cwd,
                        harness,
                        kith_bin,
                        plugins_json,
                        terminal,
                        api_helper,
                        once_id,
                        resolve,
                        granted: None,
                    })?,
                    KernelCmd::Launch {
                        what: LaunchCmd::Mcp { agent, person, pkg, snapshot, kernel, granted, resolve, json: _ },
                    } => {
                        launch_cmd::print_mcp(&launch_cmd::Args {
                            agent,
                            person,
                            pkg,
                            snapshot,
                            kernel,
                            resolve,
                            granted,
                            ..launch_cmd::Args::default()
                        })?;
                    }
                    KernelCmd::Launch { what: LaunchCmd::Whole(args) } => {
                        std::process::exit(launch_cmd::print_whole(&args));
                    }
                    KernelCmd::Settings { kernel, args } => std::process::exit(print_settings(kernel, &args)),
                    KernelCmd::RunOnce { .. }
                    | KernelCmd::Attach { .. }
                    | KernelCmd::CoreCall { .. }
                    | KernelCmd::Accounts { .. }
                    | KernelCmd::Cost { .. }
                    | KernelCmd::Delegation { .. }
                    | KernelCmd::Threads { .. }
                    | KernelCmd::ProjectId { .. }
                    | KernelCmd::Pkg { .. }
                    | KernelCmd::Permit { .. }
                    | KernelCmd::Secrets { .. }
                    | KernelCmd::CoreMigrate { .. }
                    | KernelCmd::Cutover { .. }
                    | KernelCmd::CoreDaemon
                    | KernelCmd::CoreStatus { .. } => {
                        unreachable!("handled above")
                    }
                }
                Ok(())
            })
        }
        Sub::Node => agent::node::add().await,
        Sub::Npm { args } => std::process::exit(agent::node::npm(&args).await?),
        Sub::Statusline { port, wrapped } => {
            statusline(port, wrapped.join(" ")).await;
            Ok(())
        }
        Sub::ResetToken => {
            let (store, _, _) = open_store()?;
            rotate_token(&store)?;
            println!("Token rotated. Restart the host so local helpers pick it up.");
            Ok(())
        }
        Sub::Mcp { server: McpServer::Connectors { bot, port } } => {
            mcp::serve(bot, port, mcp::Server::Connectors).await
        }
        Sub::Mcp { server: McpServer::Computer { bot, port } } => mcp::serve(bot, port, mcp::Server::Computer).await,
        Sub::Mcp { server: McpServer::Routines { bot, port } } => mcp::serve(bot, port, mcp::Server::Routines).await,
        Sub::Mcp { server: McpServer::Chat { bot, port } } => mcp::serve(bot, port, mcp::Server::Chat).await,
        Sub::Mcp { server: McpServer::Files { bot, port } } => mcp::serve(bot, port, mcp::Server::Files).await,
        Sub::Mcp { server: McpServer::Team { bot, port } } => mcp::serve(bot, port, mcp::Server::Team).await,
        Sub::Mcp { server: McpServer::Memory { bot, port } } => mcp::serve(bot, port, mcp::Server::Memory).await,
        Sub::Mcp { server: McpServer::Composio { bot, port } } => mcp::serve(bot, port, mcp::Server::Composio).await,
        Sub::Mcp { server: McpServer::Local { connector, port } } => mcp::serve_local(connector, port).await,
        Sub::Mcp { server: McpServer::Remote { connector, port } } => mcp::serve_remote(connector, port).await,
        Sub::Tui { url, token, port, budget } => {
            if let Some(budget) = budget {
                let _ = tui::GROUP_BUDGET.set(budget);
            }
            tui::run(url.unwrap_or_else(|| format!("http://127.0.0.1:{port}")), token).await
        }
    }
}

/// launchd appends to `host.log` forever. The service already holds it open as
/// stdout (append mode), so truncate in place rather than rotating.
fn cap_log() {
    let log = service::data_dir().join("host.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 10 * 1024 * 1024)
        && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&log)
    {
        let _ = f.set_len(0);
    }
}

async fn serve(bind: &str, port: u16) -> Result<()> {
    let _instance_lock = watchdog::blocking("host-lock", || service::lock_host_within(service::LOCK_WAIT)).await??;
    watchdog::blocking("binary-identity", service::capture_binary_identity).await??;
    let listener =
        tokio::net::TcpListener::bind((bind, port)).await.with_context(|| format!("binding {bind}:{port}"))?;
    cap_log();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "kith_serve=info".into()),
        )
        // A failed log write would be reported with `eprintln!`, which panics when stderr is gone
        // too (a closed terminal, a full disk): a log line must never take a task down.
        .log_internal_errors(false)
        .init();
    let (store, host_id, token) = open_store()?;
    write_token_file(&token)?;
    let identity = identity::Identity::load_or_create(&service::data_dir())?;
    tokio::task::spawn_blocking(backends::hydrate_path).await?;
    // Before any run starts, so a guest's first run already gets its token.
    runauth::start();
    let hub = hub::Hub::new(store, host_id, identity, token, port);
    hub.start()?;
    runauth::attach(&hub);
    {
        let hub = hub.clone();
        tokio::task::spawn_blocking(move || market::mirror::start(&hub.store));
    }
    cards::start(&hub);
    background::start(&hub);
    watchdog::start();
    // The core's daemon front: the owner socket for the CLI and the link to the holder. It starts on its own thread,
    // never delays this startup, and a core that will not start is a line in the core log, not an error here.
    let core_front = core_front::start_beside_host();
    // `KITH_CORE_AUTHORITY=1` is honoured only while the core's gate says so (M2-18a).
    api::authority::start_gate();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), bind, port, "kith-serve listening");
    // Peer addresses: some settings may only be changed from this computer.
    let app = api::router(hub.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    // SSE/WebSocket clients can stay connected forever. Stop agents immediately
    // on the signal instead of waiting for those connections to finish.
    let server = axum::serve(listener, app).into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        () = shutdown_signal() => {},
    }
    service::mark_exiting();
    hub.shutdown().await;
    agent::acp::stopped().await;
    analytics::flush_before_exit(&hub).await;
    drop(core_front);
    tracing::info!("kith-serve stopped");
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "can't listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "can't listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}

/// What `cloudStatus` waits on a person here to confirm: the account to type (`none`: no owner) and what to tell them.
fn owner_to_confirm(status: &serde_json::Value) -> Option<(String, String)> {
    let named = |o: &serde_json::Value| match (o["userId"].as_str(), o["email"].as_str()) {
        (Some(id), Some(email)) => format!("{email} ({id})"),
        (id, _) => id.unwrap_or("none").to_owned(),
    };
    let change = &status["ownerChange"];
    if change.is_object() {
        let to = &change["owner"];
        let says = match to["userId"].as_str() {
            None => "has left its account".to_owned(),
            Some(_) => format!("now belongs to {}", named(to)),
        };
        let account = to["userId"].as_str().unwrap_or("none").to_owned();
        return Some((
            account,
            format!(
                "owner change: the cloud says this computer {says}. Nothing that depends on the owner is applied \
                 until it is confirmed on this computer."
            ),
        ));
    }
    let owner = &status["owner"];
    let account = owner["userId"].as_str().filter(|_| status["ownerFromCloud"] == true)?.to_owned();
    let says = format!(
        "owner set from the cloud: this computer knew no account of its own when it took {} as its owner.",
        named(owner)
    );
    Some((account, says))
}

/// `kith-serve cloud --confirm-owner`: only a person typing at a terminal confirms who owns this computer.
async fn confirm_owner_here(port: u16) -> Result<()> {
    use std::io::IsTerminal;
    remote::owner::require_terminal(std::io::stdin().is_terminal())?;
    let status = local_call(port, "cloudStatus", json!({})).await?;
    let Some((account, says)) = owner_to_confirm(&status) else {
        anyhow::bail!("There is no owner change to confirm on this computer.");
    };
    println!("{says}");
    remote::owner::typed_confirmation(&account, &mut std::io::stdin().lock(), &mut std::io::stdout())?;
    let user_id = (account != "none").then_some(account);
    local_call(port, "confirmOwner", json!({ "userId": user_id })).await?;
    println!("Confirmed.");
    Ok(())
}

/// Calls the running host's local API (`~/.kith/serve/token` over loopback).
async fn local_call(port: u16, method: &str, body: serde_json::Value) -> Result<serde_json::Value> {
    let not_running =
        || anyhow::anyhow!("kith-serve isn't running here. Start it with `kith-serve install` (or `serve`).");
    let token = std::fs::read_to_string(service::data_dir().join("token")).map_err(|_| not_running())?;
    let res = http()
        .post(format!("http://127.0.0.1:{port}/api/{method}"))
        .bearer_auth(token.trim())
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| not_running())?;
    let status = res.status();
    let v: serde_json::Value = res.json().await.context("reading the host's answer")?;
    if !status.is_success() {
        anyhow::bail!("{}", v["error"].as_str().unwrap_or("the host refused"));
    }
    Ok(v)
}

async fn pair(port: u16, as_json: bool) -> Result<()> {
    let hello = local_call(port, "hello", json!({})).await?;
    let p = local_call(port, "pairing", json!({})).await?;
    if as_json {
        println!(
            "{}",
            json!({
                "name": hello["name"], "computerId": hello["computerId"], "signKey": hello["signKey"],
                "boxKey": hello["boxKey"], "urls": p["urls"], "cloud": hello["cloud"],
                "pairingUrl": p["pairingUrl"], "expiresAt": p["expiresAt"],
            })
        );
        return Ok(());
    }
    let url = p["pairingUrl"].as_str().unwrap_or_default();
    let code = qrcode::QrCode::new(url.as_bytes())?;
    println!("{}", code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
    println!("Scan with the Kith iOS app within 10 minutes, or open this link on the phone:\n{url}\n");
    if hello["cloud"].is_null()
        && p["urls"].as_array().is_none_or(|u| {
            !u.iter().any(|u| u.as_str().is_some_and(|u| u.contains("://100.") || u.contains(".ts.net")))
        })
    {
        println!(
            "Tip: the phone reaches this computer on the same network only. Turn on the Kith cloud or Tailscale to reach it from anywhere."
        );
    }
    Ok(())
}

async fn access(port: u16, action: AccessAction) -> Result<()> {
    let list = local_call(port, "accessRequests", json!({})).await?;
    let requests = list["requests"].as_array().cloned().unwrap_or_default();
    let (request, approve) = match action {
        AccessAction::List => {
            if requests.is_empty() {
                println!("No devices are asking for access.");
            }
            for r in &requests {
                println!(
                    "{}  {} ({}, {})  code {}",
                    r["requestId"].as_str().unwrap_or_default(),
                    r["deviceName"].as_str().unwrap_or_default(),
                    r["platform"].as_str().unwrap_or_default(),
                    r["email"].as_str().unwrap_or("no email"),
                    r["code"].as_str().unwrap_or("(waiting for the device)"),
                );
            }
            return Ok(());
        }
        AccessAction::Approve { request } => (request, true),
        AccessAction::Deny { request } => (request, false),
    };
    // A 6-digit code picks the request showing it; it must be the only one.
    let matching: Vec<&serde_json::Value> =
        requests.iter().filter(|r| r["requestId"] == request.as_str() || r["code"] == request.as_str()).collect();
    let [r] = matching.as_slice() else {
        anyhow::bail!("No single request matches {request}. See `kith-serve access list`.");
    };
    let id = r["requestId"].as_str().unwrap_or_default();
    local_call(port, "decideAccessRequest", json!({"requestId": id, "approve": approve})).await?;
    println!("{}", if approve { "Approved." } else { "Denied." });
    Ok(())
}

/// `kith link`: the account's other computers, and the ones asking to pair with this one.
async fn links(port: u16) -> Result<()> {
    let list = local_call(port, "peerComputers", json!({})).await?;
    let computers = list["computers"].as_array().cloned().unwrap_or_default();
    if computers.is_empty() {
        println!("This account has no other computer.");
    }
    for c in &computers {
        let mut says = match c["link"].as_str() {
            Some("paired") => "paired".to_owned(),
            Some("pending") => "pairing".to_owned(),
            _ => "not paired".to_owned(),
        };
        if c["letsIn"] == true {
            says.push_str(", lets it ask here");
        }
        println!(
            "{}  {}  ({says})",
            c["computerId"].as_str().unwrap_or_default(),
            c["name"].as_str().unwrap_or_default()
        );
    }
    for r in local_call(port, "peerRequests", json!({})).await?["requests"].as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or_default();
        println!("{name} asks to pair with this computer. Allow it with `kith link '{name}' --accept`.");
    }
    Ok(())
}

/// `kith link NAME`: starts pairing and follows it until the other computer said yes or no.
async fn link(port: u16, which: &str) -> Result<()> {
    let mut v = local_call(port, "linkComputer", json!({ "computer": which })).await?;
    let cid = v["computerId"].as_str().unwrap_or_default().to_owned();
    let name = v["name"].as_str().unwrap_or(which).to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(15 * 60);
    let mut said = String::new();
    loop {
        let state = v["state"].as_str().unwrap_or_default().to_owned();
        if state != said {
            match state.as_str() {
                "waiting" => println!("Asked {name}. Waiting for it to answer…"),
                "code" => println!(
                    "Code {}. Type it on {name} to allow this computer: kith link --accept",
                    v["code"].as_str().unwrap_or_default()
                ),
                "connecting" => println!("{name} said yes. Connecting to it…"),
                _ => {}
            }
            said = state.clone();
        }
        match state.as_str() {
            "paired" => {
                let agents: Vec<&str> =
                    v["agents"].as_array().into_iter().flatten().filter_map(|a| a["agent"].as_str()).collect();
                println!(
                    "Paired with {name}. Its agents: {}.",
                    if agents.is_empty() { "none".into() } else { agents.join(", ") }
                );
                return Ok(());
            }
            "denied" => anyhow::bail!("{name} refused the pairing."),
            "expired" | "none" => {
                anyhow::bail!("The pairing with {name} ended before it finished. Run `kith link {which}` again.")
            }
            _ => {}
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("{name} hasn't finished pairing. `kith link {which}` again picks it up.");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        v = local_call(port, "linkStatus", json!({ "computerId": cid })).await?;
    }
}

enum LinkAnswer {
    /// With the code, or asked for at the terminal.
    Accept(Option<String>),
    Deny,
}

/// `kith link [NAME] --accept [CODE]` / `--deny`: answers a computer asking to pair with this one. A yes needs the code the
/// other computer shows, typed by a person at this one.
async fn answer_link(port: u16, which: Option<String>, answer: LinkAnswer) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    if matches!(answer, LinkAnswer::Accept(_)) && !std::io::stdin().is_terminal() {
        anyhow::bail!("A pairing is accepted by a person: run `kith link --accept` in a terminal on this computer.");
    }
    let list = local_call(port, "peerRequests", json!({})).await?;
    let requests = list["requests"].as_array().cloned().unwrap_or_default();
    let matching: Vec<&serde_json::Value> = requests
        .iter()
        .filter(|r| {
            which
                .as_deref()
                .is_none_or(|w| r["computerId"] == w || r["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(w)))
        })
        .collect();
    let r = match matching.as_slice() {
        [r] => *r,
        [] => anyhow::bail!("No computer is asking to pair with this one."),
        _ => anyhow::bail!("More than one computer asks to pair: name one, e.g. `kith link <name> --accept`."),
    };
    let id = r["requestId"].as_str().unwrap_or_default();
    let name = r["name"].as_str().unwrap_or_default();
    let LinkAnswer::Accept(code) = answer else {
        local_call(port, "decidePeerRequest", json!({"requestId": id, "decision": "deny"})).await?;
        println!("Refused {name}.");
        return Ok(());
    };
    let code = if let Some(c) = code {
        c
    } else {
        if r["codeShown"] != true {
            anyhow::bail!("{name} hasn't shown its code yet. Try again in a moment.");
        }
        print!("{name} asks to pair. Type the code {name} shows to allow it: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    local_call(port, "decidePeerRequest", json!({"requestId": id, "decision": "approve", "code": code.trim()})).await?;
    println!("Paired: {name} can ask this computer's agents.");
    Ok(())
}

/// Reads the identity and token only; never creates them (that's `serve`'s job).
async fn info(port: u16, as_json: bool) -> Result<()> {
    let dir = service::data_dir();
    let (Some(id), Ok(token)) = (identity::Identity::load(&dir)?, std::fs::read_to_string(dir.join("token"))) else {
        eprintln!("start kith-serve once first");
        std::process::exit(1);
    };
    let running = http()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    let v = json!({
        "name": service::host_name(),
        "computerId": id.computer_id(),
        "signKey": id.sign_pub_b64(),
        "boxKey": id.box_pub_b64(),
        "version": env!("CARGO_PKG_VERSION"),
        "port": port,
        "token": token.trim(),
        "running": running,
    });
    if as_json {
        println!("{v}");
    } else {
        println!(
            "name:     {}\ncomputer: {}\nversion:  {}\nport:     {port}\nrunning:  {running}",
            v["name"].as_str().unwrap_or_default(),
            v["computerId"].as_str().unwrap_or_default(),
            env!("CARGO_PKG_VERSION"),
        );
    }
    Ok(())
}

/// Never blocks Claude Code: 1 s budget, all errors ignored.
async fn statusline(port: u16, wrapped: String) {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    // The user's line prints first and unchanged; the usage POST comes after and is capped short.
    let ingest = || async {
        if let Ok(token) = std::fs::read_to_string(service::data_dir().join("token")) {
            let _ = http()
                .post(format!("http://127.0.0.1:{port}/ingest/statusline"))
                .bearer_auth(token.trim())
                .json(&v)
                .timeout(Duration::from_millis(300))
                .send()
                .await;
        }
    };
    if !wrapped.trim().is_empty() {
        // The user's own status line gets the same JSON on stdin.
        use std::io::Write;
        if let Ok(mut child) = shell::line(&wrapped).std().stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.as_bytes());
            }
            let _ = child.wait();
        }
        ingest().await;
        return;
    }
    let model = v["model"]["display_name"].as_str().unwrap_or("Claude");
    let mut line = model.to_owned();
    for (k, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
        if let Some(p) = v["rate_limits"][k]["used_percentage"].as_f64() {
            let _ = write!(line, " · {label} {p:.0}%");
        }
    }
    println!("{line}");
    ingest().await;
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[allow(clippy::option_option)] // mirrors Sub::Link's --accept
    fn link(args: &[&str]) -> (Option<String>, Option<Option<String>>, bool) {
        let cli = Cli::try_parse_from(["kith-serve", "link"].iter().chain(args)).unwrap();
        let Some(Sub::Link { computer, accept, deny, .. }) = cli.cmd else { panic!("not link") };
        (computer, accept, deny)
    }

    #[test]
    fn the_suggested_link_answers_parse_as_meant() {
        // What `kith link` and its errors tell a person to type: the name stays the computer, never the code.
        assert_eq!(link(&["Mac mini", "--accept"]), (Some("Mac mini".into()), Some(None), false));
        assert_eq!(link(&["--accept"]), (None, Some(None), false));
        assert_eq!(
            link(&["Mac mini", "--accept", "123456"]),
            (Some("Mac mini".into()), Some(Some("123456".into())), false)
        );
        assert_eq!(link(&["Mac mini", "--deny"]), (Some("Mac mini".into()), None, true));
    }
}
