//! The run socket's rows. The command line is the owner's way to try one by hand; a run reaches them on the socket.

use super::build::*;
use crate::Method;

pub(super) const ROWS: &[Method] = &[
    row(
        "run.recall",
        "`kith recall` as the run's own identity (query words and the narrowing flags only).",
        &[caller(
            "args",
            "[string]",
            "The arguments, already narrowed by the run socket.",
        )],
        "json",
        &["kernel", "core-run", "recall"],
        Served::Kernel,
        Stability::Experimental,
    )
    .principals(RUN),
    row(
        "run.memorySearch",
        "`kith-memory search` as the run's own person and agent.",
        &[caller(
            "args",
            "[string]",
            "The arguments, already narrowed by the run socket.",
        )],
        "json",
        &["kernel", "core-run", "memory-search"],
        Served::Kernel,
        Stability::Experimental,
    )
    .principals(RUN),
    row(
        "run.projectId",
        "The project the run was launched in (from its token), or `--info` of that one.",
        &[caller(
            "args",
            "[string]",
            "Nothing, or `--info` and the run's own project.",
        )],
        "json",
        &["kernel", "core-run", "project-id"],
        Served::Core,
        Stability::Experimental,
    )
    .principals(RUN),
];
