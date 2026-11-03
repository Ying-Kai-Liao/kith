//! `kith-holder serve --dir D [--idle-secs N]`: run a holder in the foreground (what `start` launches).
//! `kith-holder start --dir D [--idle-secs N]`: start one detached if none answers, then exit.
//! The secret comes in the KITH_HOLDER_SECRET environment variable, never on the command line.

use kith_holder::{holder, launch};
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
    let (Some(dir), Ok(secret)) = (dir, std::env::var(launch::SECRET_VAR)) else {
        return usage();
    };
    // Nothing else runs yet, so no thread can race the removal; the holder's own programs never see it.
    unsafe { std::env::remove_var(launch::SECRET_VAR) };
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
    eprintln!("usage: KITH_HOLDER_SECRET=... kith-holder (serve|start) --dir DIR [--idle-secs N]");
    ExitCode::from(2)
}
