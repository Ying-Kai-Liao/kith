//! `kith-holder`: the small process with no policy that owns every agent process (docs/plans/runtime-design.md
//! section 1). It links neither kith-core nor kith-kernel (see `scripts/check-holder-deps.sh`).
//!
//! - `wire`: the frame layout, over any byte stream.
//! - `proto`: the messages a core and the holder exchange.
//! - `platform`: sockets and pipes, terminals, process trees, detaching, per OS.
//! - `holder`: the process itself. `client`: a blocking client (the core's side, and the tests').
//! - `launch`: starting a holder detached when none answers.

pub mod auth;
pub mod client;
pub mod holder;
pub mod launch;
pub mod platform;
pub mod proto;
pub mod wire;

/// Reported in `--version`; the wire's own version is `proto::VERSION`.
pub const BUILD: &str = env!("CARGO_PKG_VERSION");
