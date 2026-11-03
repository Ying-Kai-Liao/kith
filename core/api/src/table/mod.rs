//! The method table. One row per method; `core/api/openrpc.json` is generated from it and `core/api/released.json`
//! holds it as it was at the last release.
//!
//! The rows live in area files, one per owner, so parallel work adds rows to its own file and never to a shared list.
//! [`METHODS`] is still one ordered list: the parts below, in this order (the order is the order of the generated
//! document, so a new part goes at its place in [`PARTS`], and a lane's file is already there).
//!
//! 1. `switched`: the verbs the CLI already switched to Rust (`kith paths`, `kith kernel ...`): [`Served::Core`];
//! 2. `contract`: the verbs the host reads from the kernel (`serve/contract/kernel/contract.json`), [`Served::Kernel`],
//!    typed from the contract;
//! 3. `json_forms`, `runs`, `json_forms_more`: the kernel tools' `--json` forms (B6 in docs/plans/one-bot-model.md),
//!    [`Served::Kernel`], [`Stability::Experimental`], result `json` until their types follow, and the run socket's rows;
//! 4. the M2 lanes (`people`, `cards`, `secrets`, `gate`, `run_tokens`, `doors`, `import`): experimental stubs, owner
//!    only, served by nobody in the core yet (`Core::call` answers NotServed, the caller falls back).
//!
//! Until M2 only the owner may call anything. Param names are camelCase; result fields keep the kernel's own
//! snake_case, as the contract does.

// The area files share the row builders of `build` by glob, on purpose: a row reads as a call, not as an import list.
#![allow(clippy::wildcard_imports)]

use crate::{Method, TypeDef};

mod build;
mod cards;
mod contract;
mod doors;
mod gate;
mod import;
mod json_forms;
mod json_forms_more;
mod people;
mod run_tokens;
mod runs;
mod secrets;
mod switched;

/// Types the switched verbs add to the contract's.
pub static EXTRA_TYPES: &[TypeDef] = &[
    TypeDef {
        name: "KernelVersion",
        doc: "`kith kernel version`: the interface version and the modules this build answers.",
        fields: &[("abi", "int"), ("modules", "[string]")],
        may_omit: &[],
    },
    TypeDef {
        name: "Engines",
        doc: "`kith kernel engines --json`: the coding engines, installed or not, as the engine table describes them.",
        fields: &[("engines", "[json]")],
        may_omit: &[],
    },
    TypeDef {
        name: "LaunchPrompt",
        doc: "`kith kernel launch prompt --json`: the system prompt a run gets.",
        fields: &[("prompt", "string")],
        may_omit: &[],
    },
];

/// The parts of [`METHODS`], in order.
const PARTS: &[&[Method]] = &[
    switched::ROWS,
    contract::ROWS,
    json_forms::ROWS,
    runs::ROWS,
    json_forms_more::ROWS,
    // The M2 lanes (see `crate::table` docs): one file each.
    people::ROWS,
    cards::ROWS,
    secrets::ROWS,
    gate::ROWS,
    run_tokens::ROWS,
    doors::ROWS,
    import::ROWS,
];

const LEN: usize = {
    let mut n = 0;
    let mut p = 0;
    while p < PARTS.len() {
        n += PARTS[p].len();
        p += 1;
    }
    n
};

// Evaluated at compile time, never on a stack at run time; the table passed clippy's 16 KiB array limit with M2-12's row.
#[allow(clippy::large_stack_arrays)]
const fn flatten() -> [Method; LEN] {
    // Any row will do to fill the array; every slot is overwritten below.
    let mut out = [PARTS[0][0]; LEN];
    let mut n = 0;
    let mut p = 0;
    while p < PARTS.len() {
        let mut i = 0;
        while i < PARTS[p].len() {
            out[n] = PARTS[p][i];
            n += 1;
            i += 1;
        }
        p += 1;
    }
    out
}

static ALL: [Method; LEN] = flatten();

pub static METHODS: &[Method] = &ALL;

/// The row called `name`.
pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name == name)
}
