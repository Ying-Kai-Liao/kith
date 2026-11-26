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
// Unix-only until W22b/W23: the fixture is a scripted core on a Unix socket (std::os::unix, libc); Windows core pipes are W5b, guests W23.
#[cfg(test)]
#[cfg(unix)]
mod hq_person_tests;
pub mod node;
pub mod own_memory;
pub mod project;
pub mod registry;
pub mod term;
pub mod workspace;
