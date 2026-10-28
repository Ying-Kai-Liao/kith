//! The `channels` object in `<data dir>/config.json`, read once at start like the gate's settings.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// `enabled`: channels off entirely when false.
    pub enabled: bool,
    /// `dm_default`: the agent the owner's new DMs go to.
    pub dm_default: String,
    /// `debounce_s`: how long a run waits for more messages before it starts.
    pub debounce: Duration,
    /// `router`: `"jev"` asks Jev about messages sent while an agent works; `"off"` merges them all.
    pub router: bool,
    /// `interrupt`: false makes corrections wait for the next turn instead of restarting the run.
    pub interrupt: bool,
    /// `context_show`: a reply ends with a "context N%" line (in the apps: `contextPercent` on the turn's last agent entry) once the session is at least this full; `"off"` never.
    pub context_show: Option<f64>,
    /// `permission_mode`: the owner's runs; unset leaves it to the agent's settings, then hq's default.
    pub permission_mode: Option<String>,
    /// `discord_guild`: the server (name or id) the Discord bot uses when it is in several; its owner is taken as
    /// kith's owner until the owner's Discord account is linked (unset: no server owner is).
    pub discord_guild: Option<String>,
    /// How long a message that looks unfinished waits for the rest (not configurable).
    pub hold: Duration,
    /// A run is killed after this long (not configurable).
    pub run_timeout: Duration,
    /// After the agent process ends, how long its output may stay open before the rest of its group is killed.
    pub reader_grace: Duration,
    /// How often the outbox and live outboxes are posted (not configurable).
    pub poll: Duration,
    /// While this computer is out of sync with the Kith cloud, how long a message or a turn of anyone but the owner
    /// waits for the sync before it is refused (not configurable).
    pub sync_hold: Duration,
    /// How often a held message or turn asks whether the sync is back (not configurable).
    pub sync_poll: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            dm_default: "personal".into(),
            debounce: Duration::from_secs(2),
            router: true,
            interrupt: true,
            context_show: Some(30.0),
            permission_mode: None,
            discord_guild: None,
            hold: Duration::from_secs(8),
            run_timeout: Duration::from_secs(3600),
            reader_grace: Duration::from_secs(3),
            poll: Duration::from_secs(1),
            sync_hold: Duration::from_secs(120),
            sync_poll: Duration::from_secs(2),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let file = std::fs::read(crate::service::data_dir().join("config.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let (settings, warnings) = Self::from_file(file.as_ref());
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        settings
    }

    /// `file`: the parsed config.json, if any. Every value that is present but unusable is reported.
    pub fn from_file(file: Option<&Value>) -> (Self, Vec<String>) {
        let mut s = Self::default();
        let mut warnings = Vec::new();
        let Some(section) = file.and_then(|f| f.get("channels")) else { return (s, warnings) };
        let Some(section) = section.as_object() else {
            warnings.push("config.json: channels isn't an object; using the defaults".into());
            return (s, warnings);
        };
        let mut bad = |key: &str, v: &Value| {
            warnings.push(format!("config.json: channels.{key} = {v} isn't valid; using the default"));
        };
        for (key, v) in section {
            let text = v.as_str().map(str::trim).filter(|t| !t.is_empty());
            match key.as_str() {
                "enabled" => match v.as_bool() {
                    Some(b) => s.enabled = b,
                    None => bad(key, v),
                },
                "interrupt" => match v.as_bool() {
                    Some(b) => s.interrupt = b,
                    None => bad(key, v),
                },
                "dm_default" => match text {
                    Some(t) => t.clone_into(&mut s.dm_default),
                    None => bad(key, v),
                },
                "debounce_s" => match v.as_f64().filter(|n| n.is_finite() && (0.0..=600.0).contains(n)) {
                    Some(n) => s.debounce = Duration::from_secs_f64(n),
                    None => bad(key, v),
                },
                "router" => match text.map(str::to_lowercase).as_deref() {
                    Some("jev") => s.router = true,
                    Some("off") => s.router = false,
                    _ => bad(key, v),
                },
                "context_show" => {
                    let percent = v.as_f64().or_else(|| text?.trim_end_matches('%').parse().ok());
                    match (percent, text.map(str::to_lowercase).as_deref()) {
                        (Some(n), _) if n.is_finite() => s.context_show = Some(n),
                        (_, Some("off")) => s.context_show = None,
                        _ => bad(key, v),
                    }
                }
                "permission_mode" => match text {
                    Some(t) => s.permission_mode = Some(t.to_owned()),
                    None => bad(key, v),
                },
                "discord_guild" => match text.map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())) {
                    Some(t) => s.discord_guild = Some(t),
                    None => bad(key, v),
                },
                _ => {}
            }
        }
        (s, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_without_a_section() {
        let (s, w) = Settings::from_file(Some(&json!({"relay_url": "x"})));
        assert_eq!(s, Settings::default());
        assert!(w.is_empty(), "no warnings for defaults");
    }

    #[test]
    fn the_section_overrides_and_bad_values_are_reported() {
        let file = json!({"channels": {
            "enabled": false, "dm_default": "house", "debounce_s": 0.5, "router": "OFF", "interrupt": false,
            "context_show": "off", "permission_mode": "plan", "discord_guild": "Home", "interrupt_typo": 1}});
        let (s, w) = Settings::from_file(Some(&file));
        assert!(!s.enabled && !s.router && !s.interrupt);
        assert_eq!((s.dm_default.as_str(), s.debounce, s.context_show), ("house", Duration::from_millis(500), None));
        assert_eq!((s.permission_mode.as_deref(), s.discord_guild.as_deref()), (Some("plan"), Some("Home")));
        assert!(w.is_empty(), "no warnings for valid values");

        let (s, w) =
            Settings::from_file(Some(&json!({"channels": {"debounce_s": -1, "router": "x", "context_show": "45%"}})));
        assert_eq!((s.debounce, s.router, s.context_show), (Duration::from_secs(2), true, Some(45.0)));
        assert_eq!(w.len(), 2, "{w:?}");
    }
}
