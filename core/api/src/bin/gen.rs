//! `kith-api-gen`: writes the documents the table produces.
//!
//! - no argument: the OpenRPC document on stdout;
//! - `--write`: `core/api/openrpc.json` (what the drift test compares against);
//! - `--cut`: `core/api/released.json`, the snapshot of a release. Refuses when the table breaks the snapshot it
//!   would replace (a release that breaks the API needs a new major version first), unless `--force`.

use std::path::PathBuf;
use std::process::ExitCode;

use kith_api::{compat, openrpc};

fn root() -> PathBuf {
    // core/api -> repo root
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = openrpc::render(&openrpc::generate());
    let force = args.iter().any(|a| a == "--force");
    match args.first().map(String::as_str) {
        None => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Some("--write") => write(&root().join(openrpc::OPENRPC_PATH), &text),
        Some("--cut") => {
            let path = root().join(openrpc::RELEASED_PATH);
            if let Some(old) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                let broken = compat::breaks(&old, &openrpc::generate());
                if !broken.is_empty() && !force {
                    eprintln!(
                        "kith-api-gen: this table breaks the last release:\n  {}",
                        broken.join("\n  ")
                    );
                    return ExitCode::FAILURE;
                }
            }
            write(&path, &text)
        }
        Some(_) => {
            eprintln!("usage: kith-api-gen [--write | --cut [--force]]");
            ExitCode::from(2)
        }
    }
}

fn write(path: &std::path::Path, text: &str) -> ExitCode {
    match std::fs::write(path, text) {
        Ok(()) => {
            println!("wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("kith-api-gen: {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}
