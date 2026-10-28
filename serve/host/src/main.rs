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
mod environment;
mod hqmem;
mod hub;
mod kernel;
mod limits;
mod market;
mod mcp;
mod remote;
mod routines;
mod runauth;
mod screen;
mod service;
mod shell;
mod store;
mod terminal;
mod tui;
mod update;
mod usage;
mod voice;
mod voice_local;
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

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let cli = match kernel::route(&args) {
        kernel::Route::Host(a) => Cli::parse_from(a),
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
                // Wrap only from the stable installed binary of the Main build; dev builds and
                // cargo target paths would leave a dangling wrapper, so they only repair one.
                let host = service::current_exe().ok().filter(|exe| {
                    environment::Environment::current() == environment::Environment::Main && update::is_standalone(exe)
                });
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
                serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "protocol": compat::PROTOCOL, "minProtocol": compat::MIN_PROTOCOL, "environment": environment::Environment::current().name()})
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
            if environment::Environment::current() == environment::Environment::Main
                && let Some(home) = dirs::home_dir()
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
        Sub::Memory { cmd } => std::process::exit(hqmem::run(cmd)?),
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
        Sub::Tui { url, token, port } => {
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
    cards::start(&hub);
    background::start(&hub);
    watchdog::start();
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
