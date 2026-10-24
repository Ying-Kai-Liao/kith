//! Build identity. Development and production must never own the same service or data.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Main,
    Dev,
}

impl Environment {
    pub const fn current() -> Self {
        match option_env!("KITH_SERVE_ENV") {
            Some(value) => match value.as_bytes() {
                b"main" => Self::Main,
                _ => Self::Dev,
            },
            None => Self::Dev,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Dev => "dev",
        }
    }

    pub const fn port(self) -> u16 {
        match self {
            Self::Main => 19225,
            Self::Dev => 19224,
        }
    }

    pub const fn data_folder(self) -> &'static str {
        match self {
            Self::Main => ".kith/serve-main",
            Self::Dev => ".kith/serve",
        }
    }

    #[cfg(any(unix, test))]
    pub const fn service_label(self) -> &'static str {
        match self {
            Self::Main => "dev.kith.serve.main",
            Self::Dev => "dev.kith.serve",
        }
    }

    #[cfg(any(unix, test))]
    pub const fn systemd_unit(self) -> &'static str {
        match self {
            Self::Main => "kith-serve-main.service",
            Self::Dev => "kith-serve.service",
        }
    }

    #[cfg(any(windows, test))]
    pub const fn windows_value(self) -> &'static str {
        match self {
            Self::Main => "KithServeMain",
            Self::Dev => "KithServe",
        }
    }

    /// A Main Windows build must ship its `kith-servew.exe` under this name (a packaging step, not a
    /// cargo target): the names differ so stopping one environment's host never kills the other's.
    #[cfg(any(windows, test))]
    pub const fn supervisor(self) -> &'static str {
        match self {
            Self::Main => "kith-servew-main.exe",
            Self::Dev => "kith-servew.exe",
        }
    }

    pub const fn url_scheme(self) -> &'static str {
        match self {
            Self::Main => "kith",
            Self::Dev => "kith-dev",
        }
    }

    /// Where the web client for this environment is served; Main has none yet. `KITH_WEB_ORIGIN` overrides it for
    /// local work (read here at runtime; the cloud never supplies it, or a hostile cloud could point invites at a
    /// look-alike page).
    pub fn web_origin(self) -> Option<String> {
        let own = std::env::var("KITH_WEB_ORIGIN").ok().map(|v| v.trim().trim_end_matches('/').to_owned());
        if let Some(o) = own.filter(|o| !o.is_empty()) {
            return Some(o);
        }
        match self {
            Self::Main => None,
            Self::Dev => Some("https://kith-web-dev.lykevin890919.workers.dev".into()),
        }
    }

    #[allow(clippy::unused_self)]
    pub fn require_release_updates(self) -> anyhow::Result<()> {
        anyhow::bail!("updates are off in this build; rebuild with `kith serve start`")
    }
}

#[cfg(test)]
mod tests {
    use super::Environment::{Dev, Main};

    #[test]
    fn web_origin_is_dev_only_unless_overridden() {
        // The override is process-wide, so only assert the defaults when it is unset.
        if std::env::var_os("KITH_WEB_ORIGIN").is_none() {
            assert_eq!(Main.web_origin(), None);
            assert!(Dev.web_origin().unwrap().starts_with("https://"));
        }
    }

    #[test]
    fn development_cannot_replace_production_resources() {
        assert_eq!(Main.port(), 19225);
        assert_eq!(Main.data_folder(), ".kith/serve-main");
        assert_eq!(Main.service_label(), "dev.kith.serve.main");
        assert_eq!(Main.systemd_unit(), "kith-serve-main.service");
        assert_eq!(Main.windows_value(), "KithServeMain");
        assert_eq!(Main.supervisor(), "kith-servew-main.exe");
        assert_ne!(Main.port(), Dev.port());
        for (main, dev) in [
            (Main.data_folder(), Dev.data_folder()),
            (Main.service_label(), Dev.service_label()),
            (Main.systemd_unit(), Dev.systemd_unit()),
            (Main.windows_value(), Dev.windows_value()),
            (Main.supervisor(), Dev.supervisor()),
            (Main.url_scheme(), Dev.url_scheme()),
        ] {
            assert_ne!(main, dev);
        }
        assert!(Dev.require_release_updates().is_err());
        assert!(Main.require_release_updates().is_err());
    }

    #[test]
    fn development_runs_as_kith_serve() {
        assert_eq!(Dev.port(), 19224);
        assert_eq!(Dev.data_folder(), ".kith/serve");
        assert_eq!(Dev.service_label(), "dev.kith.serve");
        assert_eq!(Dev.systemd_unit(), "kith-serve.service");
        assert_eq!(Dev.windows_value(), "KithServe");
        assert_eq!(Dev.supervisor(), "kith-servew.exe");
    }
}
