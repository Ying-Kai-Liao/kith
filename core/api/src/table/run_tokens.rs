//! M2 lane F (run tokens on the run socket): its rows go here. The `run.*` rows of M0e stay in `runs`.

use super::build::*;
use crate::Method;

pub(super) const ROWS: &[Method] = &[plumbing(
    "run.resolve",
    "Who a run token stands for, for the host's own socket: the token must be live, bound, the peer pid must be the run's process or below it and the run's person (and every member of a room turn's basis) must still be allowed. Answers the identity, or is refused.",
    &[
        caller("token", "string", "The run's token."),
        caller("pid", "int", "The connecting process on the host's socket."),
    ],
    "json",
    &["kernel", "core-run", "resolve"],
)];
