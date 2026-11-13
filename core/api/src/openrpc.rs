//! The OpenRPC document of the table (https://spec.open-rpc.org/), generated, never written by hand.
//!
//! Checked in as `core/api/openrpc.json`; a test fails when it differs from a fresh [`generate`]. Everything the
//! table knows that OpenRPC has no word for (principals, CLI shape, stability, who serves it) is an `x-kith-*` key.

use serde_json::{Map, Value, json};

use crate::schema::{enum_schema, object_schema, schema};
use crate::{Arg, METHODS, Method, Principal, Served, Stability, enums, named_types};

/// Where the document is checked in, relative to the repo root.
pub const OPENRPC_PATH: &str = "core/api/openrpc.json";
/// The snapshot cut at each release.
pub const RELEASED_PATH: &str = "core/api/released.json";
/// What to run when the checked-in document is out of date.
pub const REGENERATE: &str =
    "cargo run --manifest-path serve/host/Cargo.toml -p kith-api --bin kith-api-gen -- --write";

pub fn generate() -> Value {
    let mut schemas = Map::new();
    for e in enums() {
        schemas.insert(e.name.to_owned(), enum_schema(e));
    }
    for t in named_types() {
        schemas.insert(t.name.to_owned(), object_schema(t));
    }
    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "Kith core API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Generated from core/api/src/table.rs by kith-api-gen. Do not edit.",
        },
        "methods": METHODS.iter().map(method_doc).collect::<Vec<_>>(),
        "components": { "schemas": schemas },
    })
}

/// The document as the file holds it: pretty, `\n` line ends, one trailing newline.
pub fn render(doc: &Value) -> String {
    let mut s = serde_json::to_string_pretty(doc).unwrap_or_default();
    s.push('\n');
    s
}

fn method_doc(m: &Method) -> Value {
    let params: Vec<Value> = m
        .params
        .iter()
        .map(|p| json!({"name": p.name, "summary": p.summary, "required": p.required, "schema": schema(p.ty)}))
        .collect();
    let args: Map<String, Value> = m
        .params
        .iter()
        .map(|p| {
            let how = match p.arg {
                Arg::Pos => json!({"kind": "positional"}),
                Arg::Flag(s) => json!({"kind": "flag", "spellings": s}),
                Arg::Switch(s) => json!({"kind": "switch", "spellings": s}),
                Arg::Words => json!({"kind": "words"}),
                Arg::Rest => json!({"kind": "rest"}),
                Arg::After => json!({"kind": "after"}),
                Arg::Caller => json!({"kind": "caller"}),
            };
            (p.name.to_owned(), how)
        })
        .collect();
    let mut row = json!({
        "name": m.name,
        "summary": m.summary,
        "params": params,
        "result": {"name": "result", "schema": schema(m.result)},
        "x-kith-principals": m.principals.iter().map(|p: &Principal| p.name()).collect::<Vec<_>>(),
        "x-kith-stability": match m.stability { Stability::Stable => "stable", Stability::Experimental => "experimental" },
        "x-kith-served": match m.served { Served::Core => "core", Served::Kernel => "kernel" },
        "x-kith-cli": {
            "path": m.cli.path,
            "aliases": m.cli.aliases,
            "formats": m.cli.formats,
            "requires": m.cli.requires,
            "args": args,
        },
    });
    // Said only where it is so, so that the other rows (and their hashes) stay as they were.
    if m.cli.hidden {
        row["x-kith-cli"]["hidden"] = json!(true);
    }
    row
}
