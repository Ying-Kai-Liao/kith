//! `kith-holder serve --dir D [--idle-secs N]`: run a holder in the foreground (what `start` launches).
//! `kith-holder start --dir D [--idle-secs N]`: start one detached if none answers, then exit.
//! `kith-holder --stdio --dir D [--idle-secs N]`: relay stdin/stdout to the holder of D (what a remote core runs
//! over ssh); starts the holder if none answers. `--dir` is required: a default outside the core folder would make a second holder.
//! The secret is the first line of stdin, never the command line or the environment (both stay readable by this
//! user's other processes for the holder's whole life).

use kith_holder::{holder, launch, stdio};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return usage();
    };
    if command == "--version" || command == "-V" {
        println!(
            "kith-holder {} (wire {})",
            kith_holder::BUILD,
            kith_holder::proto::VERSION
        );
        return ExitCode::SUCCESS;
    }
    let stdio = command == "--stdio";
    let (mut dir, mut idle) = (None, 120u64);
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--dir", Some(v)) => dir = Some(PathBuf::from(v)),
            ("--idle-secs", Some(v)) => match v.parse() {
                Ok(n) => idle = n,
                Err(_) => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(dir) = dir else {
        return usage();
    };
    if stdio {
        // stdout carries frames only; anything to say goes to stderr, which ssh passes on.
        let result = std::env::current_exe().and_then(|exe| {
            stdio::relay(
                &exe,
                &dir,
                Duration::from_secs(idle),
                std::io::stdin(),
                std::io::stdout(),
            )
        });
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("kith-holder: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let secret = match launch::read_secret(std::io::stdin().lock()) {
        Ok(secret) => secret,
        Err(e) => {
            eprintln!("kith-holder: {e}");
            return usage();
        }
    };
    let idle = Duration::from_secs(idle);
    let result = match command.as_str() {
        "serve" => holder::run(holder::Config { dir, secret, idle }),
        "start" => match std::env::current_exe() {
            Ok(exe) => launch::ensure(&exe, &dir, &secret, idle),
            Err(e) => Err(e),
        },
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kith-holder: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: echo SECRET | kith-holder (serve|start|--stdio) --dir DIR [--idle-secs N]");
    ExitCode::from(2)
}
