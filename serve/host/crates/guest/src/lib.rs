//! `kith-guest`: the client-only `kith` a cloud bot runs on its own VM (docs/kith-bot-guests.md). It joins a Kith computer
//! with an invite link and asks its agents over the same end-to-end relay the apps use. As a library so a Mac `kith` can
//! mount it later; the binary is `src/main.rs`.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;

pub mod ask;
pub mod cli;
pub mod cloud;
pub mod commands;
pub mod config;
pub mod exit;
pub mod join;
pub mod link;
pub mod output;
pub mod relay;
pub mod room;
pub mod session;
pub mod thread;

use cli::{Cli, Command};
use config::Store;
use exit::{CliError, Exit};
use output::Out;
use session::Tuning;

fn store_of(cli: &Cli) -> Result<Store, CliError> {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    Store::open(config::config_dir(cli.home.as_deref(), xdg.as_deref(), home.as_deref()), cli.credentials.as_deref())
}

async fn dispatch(cli: Cli, out: &Out, tuning: Tuning) -> Result<(), CliError> {
    let store = store_of(&cli)?;
    let cloud = cli.cloud.as_deref().filter(|c| !c.is_empty()).map(|c| c.trim_end_matches('/').to_string());
    let cloud = cloud.as_deref();
    match cli.command {
        Command::Join(args) => join::run(args, store, cloud, out, tuning).await,
        Command::Ask(args) => ask::run(args, store, cloud, out, tuning).await,
        Command::Read(args) => commands::read(args, store, cloud, out, tuning).await,
        Command::Threads => commands::threads(store, cloud, out, tuning).await,
        Command::Stop(args) => commands::stop(args, store, cloud, out, tuning).await,
        Command::Room(cmd) => room::run(cmd, store, cloud, out, tuning).await,
        Command::Agents => commands::agents(store, cloud, out, tuning).await,
        Command::Status => commands::status(store, cloud, out, tuning).await,
        Command::Leave => commands::leave(store, cloud, out, tuning).await,
    }
}

/// Ctrl-C and SIGTERM stop waiting. A turn already sent keeps running on the computer.
async fn interrupted() -> CliError {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).ok();
        let terminated = async {
            match term.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => CliError::new(Exit::Interrupted, "interrupted", "Stopped waiting. A turn already sent keeps running on the computer."),
            () = terminated => CliError::new(Exit::Terminated, "terminated", "Stopped waiting. A turn already sent keeps running on the computer."),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        CliError::new(
            Exit::Interrupted,
            "interrupted",
            "Stopped waiting. A turn already sent keeps running on the computer.",
        )
    }
}

/// Runs one parsed command line to its exit code, reporting a failure on `out`.
pub async fn execute(cli: Cli, out: &Out, tuning: Tuning) -> u8 {
    let result = tokio::select! {
        r = dispatch(cli, out, tuning) => r,
        e = interrupted() => Err(e),
    };
    match result {
        Ok(()) => Exit::Ok.code(),
        Err(e) => {
            out.error(&e);
            e.exit.code()
        }
    }
}

pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { Exit::Usage.code() } else { Exit::Ok.code() };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    let out = Out::std(cli.json);
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            out.error(&CliError::unexpected(format!("Can't start the runtime: {e}")));
            return ExitCode::from(Exit::Unexpected.code());
        }
    };
    ExitCode::from(runtime.block_on(execute(cli, &out, Tuning::default())))
}
