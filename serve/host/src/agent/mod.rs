//! Driving coding agents: harness discovery, the ACP registry, sign-in, setup
//! terminals and the per-bot actor that runs turns over ACP.

pub mod acp;
pub mod auth;
pub mod backends;
pub mod backups;
pub mod bot;
pub mod folder;
pub mod gate;
pub mod hq;
#[cfg(test)]
mod hq_person_tests;
pub mod node;
pub mod own_memory;
pub mod project;
pub mod registry;
pub mod term;
pub mod workspace;
