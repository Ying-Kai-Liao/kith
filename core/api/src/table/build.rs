//! The constructors every area file builds its rows with.

use crate::{Arg, Cli, Principal};
pub(super) use crate::{Method, Param, Served, Stability};

pub(super) const OWNER: &[Principal] = &[Principal::Owner];
/// The host process that holds the relay connection, proving the host secret: the rows that report a fact only it can
/// know (the sync lease). Not the owner, so the documented owner routes (a terminal, an owner agent) are refused; a same-OS-user process can still read the secret file.
pub(super) const HOST: &[Principal] = &[Principal::Host];
/// A row the owner may read and the host takes as part of its own flow.
pub(super) const OWNER_HOST: &[Principal] = &[Principal::Owner, Principal::Host];
/// A running agent holding a run token, on the run socket (M0e). It reaches nothing but the `run.*` rows marked so.
pub(super) const RUN: &[Principal] = &[Principal::RunToken];

pub(super) const fn pos(
    name: &'static str,
    ty: &'static str,
    required: bool,
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty,
        required,
        arg: Arg::Pos,
        summary,
    }
}
pub(super) const fn flag(
    name: &'static str,
    ty: &'static str,
    spellings: &'static [&'static str],
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty,
        required: false,
        arg: Arg::Flag(spellings),
        summary,
    }
}
pub(super) const fn sw(
    name: &'static str,
    spellings: &'static [&'static str],
    summary: &'static str,
) -> Param {
    Param {
        name,
        ty: "bool",
        required: false,
        arg: Arg::Switch(spellings),
        summary,
    }
}
/// A param the client fills in from where it runs (see [`Arg::Caller`]); never written on a command line.
pub(super) const fn caller(name: &'static str, ty: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty,
        required: false,
        arg: Arg::Caller,
        summary,
    }
}
pub(super) const fn rest(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::Rest,
        summary,
    }
}
pub(super) const fn words(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::Words,
        summary,
    }
}
pub(super) const fn after(name: &'static str, summary: &'static str) -> Param {
    Param {
        name,
        ty: "[string]",
        required: false,
        arg: Arg::After,
        summary,
    }
}

pub(super) const fn row(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
    served: Served,
    stability: Stability,
) -> Method {
    Method {
        name,
        summary,
        params,
        result,
        principals: OWNER,
        cli: Cli {
            path,
            aliases: &[],
            formats: &[],
            requires: &[],
        },
        stability,
        served,
    }
}

impl Method {
    pub(super) const fn alias(mut self, a: &'static [&'static [&'static str]]) -> Self {
        self.cli.aliases = a;
        self
    }
    pub(super) const fn formats(mut self, f: &'static [&'static str]) -> Self {
        self.cli.formats = f;
        self
    }
    pub(super) const fn requires(mut self, r: &'static [&'static str]) -> Self {
        self.cli.requires = r;
        self
    }
    /// A row no command line reaches: the CLI always connects as the owner, so a host-only row's verb could only be
    /// refused. An empty path is how the table says so (`cli::find` skips it).
    pub(super) const fn no_cli(mut self) -> Self {
        self.cli.path = &[];
        self
    }
    pub(super) const fn principals(mut self, p: &'static [Principal]) -> Self {
        self.principals = p;
        self
    }
}

/// A switched verb (family 1).
pub(super) const fn core(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Core,
        Stability::Stable,
    )
}
/// Kernel plumbing (`kith kernel ...`): switched, but not frozen. Only `paths`, `kernel.version` and the contract
/// verbs are stable; these may change shape until they have been used by a client other than the CLI.
pub(super) const fn plumbing(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Core,
        Stability::Experimental,
    )
}
/// A verb the host reads from the kernel (family 2).
pub(super) const fn contract(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    result: &'static str,
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        result,
        path,
        Served::Kernel,
        Stability::Stable,
    )
}
/// A `--json` form of a kernel tool (family 3).
pub(super) const fn b6(
    name: &'static str,
    summary: &'static str,
    params: &'static [Param],
    path: &'static [&'static str],
) -> Method {
    row(
        name,
        summary,
        params,
        "json",
        path,
        Served::Kernel,
        Stability::Experimental,
    )
    .formats(&["--json"])
}

pub(super) const KERNEL: Param = flag(
    "kernel",
    "string",
    &["--kernel"],
    "The kernel folder (default: the one this program finds).",
);
pub(super) const J: &[&str] = &["--json"];
