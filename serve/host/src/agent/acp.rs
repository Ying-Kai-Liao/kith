//! The ACP client lives in the `kith-acp` crate (core/acp), shared with the `kith` program; this keeps the host's
//! `agent::acp` path for its callers, and the host-only parts: how an agent's command line starts, and the test that
//! pins the run-token variables the crate scrubs.

pub use kith_acp::*;

/// Hands the crate the host's command-line rules (Windows quoting, `shell::exec`). Once, at startup.
pub fn init() {
    kith_acp::set_shell(|command| crate::shell::exec(command).tokio());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_crate_scrubs_the_hosts_run_token_variables() {
        // kith-acp spells them out (it cannot see runauth); a rename here must reach it.
        assert_eq!(crate::runauth::TOKEN_ENV, "KITH_RUN_TOKEN");
        assert_eq!(crate::runauth::SOCKET_ENV, "KITH_RUN_SOCKET");
    }
}
