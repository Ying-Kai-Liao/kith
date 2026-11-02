//! The kernel's rules in Rust, one module at a time. The CLI (`kith`) and the host both link this crate, so the
//! two answer the same question the same way. The Python kernel keeps its own copies while it moves; a parity
//! test (`kernel/tests/kith-paths-parity-check`) fails when they differ.

pub mod paths;
