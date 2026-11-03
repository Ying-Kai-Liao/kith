//! `kith-holder serve --dir D [--idle-secs N]`: run a holder in the foreground (what `start` launches).
//! `kith-holder start --dir D [--idle-secs N]`: start one detached if none answers, then exit.
//! `kith-holder --stdio --dir D [--idle-secs N]`: relay stdin/stdout to the holder of D (what a remote core runs
//! over ssh); starts the holder if none answers. `--dir` is required: a default outside the core folder would make a second holder.
//! The secret is the first line of stdin, never the command line or the environment (both stay readable by this
//! user's other processes for the holder's whole life).

use kith_holder::{holder, launch, stdio, supervise};
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
    if command == "supervise" {
        return supervise_command(args);
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

/// `supervise --dir D [--secret-file F] [--backoff-ms MIN:MAX] -- PROGRAM [ARGS]`: serve as the holder of D and keep
/// PROGRAM running beside it. A service cannot pipe the secret in, so it names a file (owner-only, the core's own);
/// without `--secret-file` the secret is the first line of stdin like for `serve`. The holder never idles out here.
fn supervise_command(mut args: impl Iterator<Item = String>) -> ExitCode {
    let (mut dir, mut secret_file) = (None, None);
    let mut policy = supervise::Policy::default();
    let mut argv = Vec::new();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--" => {
                argv.extend(args.by_ref());
            }
            "--dir" | "--secret-file" | "--backoff-ms" => {
                let Some(v) = args.next() else {
                    return usage();
                };
                match flag.as_str() {
                    "--dir" => dir = Some(PathBuf::from(v)),
                    "--secret-file" => secret_file = Some(PathBuf::from(v)),
                    _ => {
                        let parsed = v.split_once(':').and_then(|(a, b)| {
                            Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
                        });
                        let Some((min, max)) = parsed.filter(|(a, b)| a <= b) else {
                            return usage();
                        };
                        policy.min = Duration::from_millis(min);
                        policy.max = Duration::from_millis(max);
                    }
                }
            }
            _ => return usage(),
        }
    }
    let (Some(dir), false) = (dir, argv.is_empty()) else {
        return usage();
    };
    let secret = match secret_file {
        Some(f) => std::fs::File::open(&f)
            .map(std::io::BufReader::new)
            .and_then(launch::read_secret),
        None => launch::read_secret(std::io::stdin().lock()),
    };
    let secret = match secret {
        Ok(s) => s,
        Err(e) => {
            eprintln!("kith-holder: {e}");
            return usage();
        }
    };
    let cfg = holder::Config {
        dir,
        secret,
        idle: Duration::ZERO,
    };
    match supervise::run(cfg, &argv, &policy) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kith-holder: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: echo SECRET | kith-holder (serve|start|--stdio) --dir DIR [--idle-secs N]");
    eprintln!(
        "       kith-holder supervise --dir DIR [--secret-file FILE] [--backoff-ms MIN:MAX] -- PROGRAM [ARGS]"
    );
    ExitCode::from(2)
}
