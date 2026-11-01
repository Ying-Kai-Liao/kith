//! The one host identity: its port, data folder, service names and link scheme.

pub const PORT: u16 = 19224;
pub const DATA_FOLDER: &str = ".kith/serve";
#[cfg(any(unix, test))]
pub const SERVICE_LABEL: &str = "dev.kith.serve";
#[cfg(any(unix, test))]
pub const SYSTEMD_UNIT: &str = "kith-serve.service";
#[cfg(any(windows, test))]
pub const WINDOWS_VALUE: &str = "KithServe";
#[cfg(any(windows, test))]
pub const SUPERVISOR: &str = "kith-servew.exe";
pub const URL_SCHEME: &str = "kith";

/// Where the web client is served; there is none yet. `KITH_WEB_ORIGIN` overrides it for local work (read here at
/// runtime; the cloud never supplies it, or a hostile cloud could point invites at a look-alike page).
pub fn web_origin() -> Option<String> {
    let own = std::env::var("KITH_WEB_ORIGIN").ok().map(|v| v.trim().trim_end_matches('/').to_owned());
    own.filter(|o| !o.is_empty())
}

pub fn require_release_updates() -> anyhow::Result<()> {
    anyhow::bail!("updates are off in this build; rebuild with `kith serve start`")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_host_runs_as_kith_serve() {
        assert_eq!(super::PORT, 19224);
        assert_eq!(super::DATA_FOLDER, ".kith/serve");
        assert_eq!(super::SERVICE_LABEL, "dev.kith.serve");
        assert_eq!(super::SYSTEMD_UNIT, "kith-serve.service");
        assert_eq!(super::WINDOWS_VALUE, "KithServe");
        assert_eq!(super::SUPERVISOR, "kith-servew.exe");
        assert_eq!(super::URL_SCHEME, "kith");
        assert!(super::require_release_updates().is_err());
    }
}
