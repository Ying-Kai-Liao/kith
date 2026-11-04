//! `kith-holder`: the small process with no policy that owns every agent process (docs/plans/runtime-design.md
//! section 1). It links neither kith-core nor kith-kernel (see `scripts/check-holder-deps.sh`).
//!
//! - `wire`: the frame layout, over any byte stream.
//! - `attach`: the frame kinds a client and the core's attach relay speak (the relay itself is in kith-core).
//! - `proto`: the messages a core and the holder exchange.
//! - `platform`: sockets and pipes, terminals, process trees, detaching, per OS.
//! - `holder`: the process itself. `client`: a blocking client (the core's side, and the tests').
//! - `supervise`: the holder as the supervised root, restarting the core (M1).
//! - `launch`: starting a holder detached when none answers. `stdio`: the byte relay behind `kith-holder --stdio`.

pub mod attach;
pub mod auth;
pub mod client;
pub mod holder;
pub mod launch;
pub mod platform;
pub mod proto;
pub mod stdio;
pub mod supervise;
pub mod wire;

/// Reported in `--version`; the wire's own version is `proto::VERSION`.
pub const BUILD: &str = env!("CARGO_PKG_VERSION");
