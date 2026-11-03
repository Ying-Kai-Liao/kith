//! kith-api: the core's method table (docs/plans/runtime-design.md, "Execution additions", "Crates and binaries").
//!
//! One row per method: its name, params, result, who may call it, its CLI shape and its stability. Three things
//! come out of the table and nothing else decides them:
//! - the CLI: [`cli::parse_argv`] turns a command line into a method and its params;
//! - the OpenRPC document: [`openrpc::generate`], checked in as `core/api/openrpc.json` and diffed by a test;
//! - the break test: [`compat::breaks`] compares a document with `core/api/released.json`, the snapshot cut at each
//!   release, and allows additions only.
//!
//! `kith-core` depends on this crate and calls through the table with a principal; this crate depends on
//! `serde_json` and nothing else, so the holder, the kernel and the host never reach core state through it.
//!
//! Types: params and results are written in the contract's own small type language (`string`, `bool`, `int`,
//! `json`, `Name`, `[T]`, `{T}`, `T?`; see [`schema`]), seeded from `serve/contract/kernel/contract.json`. Serde
//! structs, TypeScript and Swift types generated from the same rows are the follow-up (plan M0b: "types follow-up").

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::needless_pass_by_value,
        clippy::assert_is_empty
    )
)]

pub mod cli;
pub mod compat;
pub mod openrpc;
pub mod params;
pub mod schema;
mod table;
mod types;

pub use table::{METHODS, method};

/// Who is calling. Until M2 only the owner may call anything; the others exist so the rows can say who is next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Principal {
    /// The person whose Mac this is, at a terminal, the app or the daemon's owner socket.
    Owner,
    /// An invited person.
    Member,
    /// A bot guest.
    Bot,
    /// A running agent, with a run token.
    RunToken,
}

impl Principal {
    pub const ALL: [Principal; 4] = [
        Principal::Owner,
        Principal::Member,
        Principal::Bot,
        Principal::RunToken,
    ];

    /// The name the OpenRPC document uses.
    pub fn name(self) -> &'static str {
        match self {
            Principal::Owner => "owner",
            Principal::Member => "member",
            Principal::Bot => "bot",
            Principal::RunToken => "run-token",
        }
    }
}

/// Whether a method may change between releases. The break test holds a `Stable` row to the released snapshot and
/// lets an `Experimental` one change shape until it is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stability {
    Stable,
    Experimental,
}

/// Who answers the method today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// The Rust core (the verbs the CLI already switched).
    Core,
    /// The Python kernel; the CLI falls back to it exactly as today. The row exists so the shape is fixed before
    /// the verb moves.
    Kernel,
}

/// How one param is written on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    /// The next bare word.
    Pos,
    /// `--name VALUE` or `--name=VALUE`; any of the spellings.
    Flag(&'static [&'static str]),
    /// `--name`, no value: true when present.
    Switch(&'static [&'static str]),
    /// Bare words that fill no `Pos`, in order, as `[string]`; flags are still read among them.
    Words,
    /// Everything from the first bare word on, hyphen words included, passed through (`[string]`).
    Rest,
    /// Everything after a bare `--` (`[string]`).
    After,
}

/// One param of a method. `name` is camelCase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: &'static str,
    pub required: bool,
    pub arg: Arg,
    pub summary: &'static str,
}

/// How a method is spelled as a command: the words in front, and the switches that are not params.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cli {
    /// The leading words (`["kernel", "accounts", "list"]`).
    pub path: &'static [&'static str],
    /// Other leading words that mean the same (`kith paths` and `kith kernel paths`).
    pub aliases: &'static [&'static [&'static str]],
    /// Presentation switches the verb accepts (`--json`, `--sh`): the caller wants the result in that form.
    pub formats: &'static [&'static str],
    /// Literal switches that must be there, and mean nothing else (`kith ls --json`, `kith AGENT --launch-json`).
    pub requires: &'static [&'static str],
}

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    /// Dotted, stable: `kernel.accounts.list`.
    pub name: &'static str,
    pub summary: &'static str,
    pub params: &'static [Param],
    /// A type in the contract's type language (see [`schema`]).
    pub result: &'static str,
    pub principals: &'static [Principal],
    pub cli: Cli,
    pub stability: Stability,
    pub served: Served,
}

impl Method {
    pub fn allows(&self, who: Principal) -> bool {
        self.principals.contains(&who)
    }
}

/// An enum of the contract: the values a field may hold, and the word for one this version does not know.
pub struct EnumDef {
    pub name: &'static str,
    pub doc: &'static str,
    pub values: &'static [&'static str],
    pub unknown: &'static str,
}

/// A named object type: `(field, type)` pairs in the contract's type language.
pub struct TypeDef {
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: &'static [(&'static str, &'static str)],
    pub may_omit: &'static [&'static str],
}

/// The enums and types the rows refer to: the kernel contract's, then the ones the switched verbs add.
pub fn enums() -> &'static [EnumDef] {
    types::ENUMS
}

pub fn named_types() -> impl Iterator<Item = &'static TypeDef> {
    types::TYPES.iter().chain(table::EXTRA_TYPES.iter())
}
