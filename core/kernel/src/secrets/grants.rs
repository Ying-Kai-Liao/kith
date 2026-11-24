//! Which profile an agent uses for a service, and what the profile's secrets resolve to (`granted`, `grant_for`, `secrets`,
//! `resolve_ref`, `browser_profile` of `kernel/bin/kith-auth`). A value is read from where its profile says (Keychain, a
//! variable, a file) when it is asked for and goes to the caller only: never to argv, a file or a log.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use super::session::Session;
use super::{Die, R, arr, canonical, die, get, is_owner, owner, run_command_with_timeout, truthy};

/// A locked Keychain or an access prompt must not hang a headless run.
const KEYCHAIN_LIMIT: Duration = Duration::from_secs(10);
/// The profile's person that means "shared on purpose", even for a service whose profiles are personal.
const SHARED: &str = "*";

/// `(profile, why not, refused)` of `granted`.
pub(super) type Granted = (Option<String>, String, bool);

impl Session<'_> {
    /// Whether a service's profiles belong to someone unless marked shared: logins (a Chrome profile, a `ChatGPT` login) and
    /// browser profiles are; API keys and bots aren't. A service json can say `"personal": true` or `false`.
    fn personal(&self, svc: &str) -> R<bool> {
        let spec = match self.service_file(svc, None) {
            Some(f) => super::session::read_spec(&f)?,
            None => return Ok(false),
        };
        Ok(match spec.get("personal") {
            Some(p) => truthy(p),
            None => matches!(get(&spec, "kind").as_str(), Some("login" | "browser")),
        })
    }

    /// `owner_of(auth, profile)`: whose a profile is: its `person`, else the owner's when its service is personal, else no
    /// one's (shared). With no owner named yet "the owner" still owns it, so other people's runs are refused all the same.
    fn owner_of(&self, profile: &str) -> R<Option<String>> {
        let belongs = get(get(get(self.auth, "profiles"), profile), "person");
        if belongs.as_str() == Some(SHARED) {
            return Ok(None);
        }
        if truthy(belongs) {
            return Ok(Some(
                belongs.as_str().map_or_else(String::new, str::to_owned),
            ));
        }
        let svc = profile.split('/').next().unwrap_or_default();
        Ok(if self.personal(svc)? {
            Some(owner(self.auth).unwrap_or_else(|| "the owner".to_owned()))
        } else {
            None
        })
    }

    /// `granted(auth, agent, svc, person)`: the person's own grant (`<agent>@<person>`) first, then the agent's. A profile that
    /// belongs to someone else is never handed over, so a missing personal grant can't fall back to another person's account.
    pub fn granted(&self, agent: &str, svc: &str, person: Option<&str>) -> R<Granted> {
        let person = person.map_or_else(|| self.acting(), str::to_owned);
        let grants = get(self.auth, "grants");
        let pick = |v: &Value| -> Option<Value> { truthy(v).then(|| v.clone()) };
        let mut found = if person.is_empty() {
            None
        } else {
            pick(get(get(grants, &format!("{agent}@{person}")), svc))
        }
        .or_else(|| pick(get(get(grants, agent), svc)));
        // several (the channels' bots): the first, unless the caller picks one (KITH_AUTH_PROFILE)
        if let Some(Value::Array(list)) = &found {
            let want = self.env.get("KITH_AUTH_PROFILE").filter(|p| !p.is_empty());
            found = match want {
                Some(w) => list.iter().find(|p| p.as_str() == Some(w)).cloned(),
                None => list.first().cloned(),
            };
        }
        let Some(profile) = found
            .filter(truthy)
            .map(|p| p.as_str().map_or_else(|| p.to_string(), str::to_owned))
        else {
            return Ok((
                None,
                format!("{agent} has no grant for {svc} (kith-auth grant {agent} {svc} <profile>)"),
                false,
            ));
        };
        let belongs = self.owner_of(&profile)?;
        let lent = arr(get(get(get(self.auth, "profiles"), &profile), "with"));
        // owner runs: never refused
        if let Some(belongs) =
            belongs.filter(|b| !b.is_empty() && !person.is_empty() && *b != person)
            && !lent
                .iter()
                .any(|l| l.as_str() == Some(canonical(self.auth, &person).as_str()))
        {
            return Ok((
                None,
                format!(
                    "{profile} is {belongs}'s and this run is for {person} \
                     (kith-auth grant {agent}@{person} {svc} <their profile>)"
                ),
                true,
            ));
        }
        Ok((Some(profile), String::new(), false))
    }

    /// `grant_for`: the profile name this agent may use for svc, or the refusal.
    pub fn grant_for(&self, agent: &str, svc: &str) -> R<String> {
        if agent.is_empty() {
            return die(format!(
                "who is asking for {svc}? (KITH_AGENT is not set; tools pass --for <agent>)"
            ));
        }
        match self.granted(agent, svc, None)? {
            (Some(p), _, _) => Ok(p),
            (None, why, _) => die(why),
        }
    }

    /// `resolve_ref(ref)`: the value a profile's reference names, or `None` when it does not resolve.
    pub fn resolve_ref(&self, reference: &str) -> Option<String> {
        let (kind, rest) = reference.split_once(':').unwrap_or((reference, ""));
        match kind {
            "env" => self.env.get(rest).cloned(),
            "env-file" => {
                let (path, var) = rest.split_once('#').unwrap_or((rest, ""));
                let file = super::session::expanduser(&self.env, path);
                let text = std::fs::read_to_string(file).ok()?;
                text.lines().find_map(|line| {
                    let line = line.trim();
                    let line = match line.strip_prefix("export") {
                        Some(r) if r.starts_with(char::is_whitespace) => r.trim_start(),
                        _ => line,
                    };
                    let (k, v) = line.split_once('=')?;
                    (k.trim() == var).then(|| v.trim().trim_matches(['"', '\'']).to_owned())
                })
            }
            "keychain" => {
                let (svc, account) = rest.split_once('#').unwrap_or((rest, ""));
                // `security` writes the item and so trusts only itself: never a read through the Keychain API, which would ask
                // the owner to allow it on every server start
                let mut cmd = std::process::Command::new(self.security_bin());
                cmd.args(["find-generic-password", "-s", svc, "-a", account, "-w"]);
                run_command_with_timeout(cmd, KEYCHAIN_LIMIT)
                    .map(|out| out.trim_end_matches('\n').to_owned())
            }
            _ => None,
        }
    }

    /// The one tool that reads and writes Keychain items.
    fn security_bin(&self) -> String {
        self.env
            .get("KITH_SECURITY_BIN")
            .filter(|b| !b.is_empty())
            .cloned()
            .unwrap_or_else(|| "/usr/bin/security".to_owned())
    }

    /// `secrets(auth, svc, profile, agent)`: `{VAR: value}` for every secret the service needs, from the profile, in the order
    /// the service names them. A missing one is a refusal.
    pub fn secrets(&self, svc: &str, profile: &str, agent: &str) -> R<Vec<(String, String)>> {
        let refs = get(get(self.auth, "profiles"), profile);
        if refs.is_null() {
            return die(format!(
                "no profile '{profile}' in {}",
                self.c.auth.display()
            ));
        }
        let spec = self.service(svc, Some(agent))?;
        let mut out = Vec::new();
        for var in arr(get(&spec, "secrets")) {
            let var = var.as_str().unwrap_or_default();
            let reference = get(refs, var).as_str().filter(|r| !r.is_empty());
            let value = reference
                .and_then(|r| self.resolve_ref(r))
                .filter(|v| !v.is_empty());
            let Some(value) = value else {
                return Err(Die {
                    msg: format!(
                        "{profile}: {var} does not resolve ({}; kith-auth set {profile} {var})",
                        reference.unwrap_or("not set")
                    ),
                    code: 1,
                });
            };
            out.push((var.to_owned(), value));
        }
        Ok(out)
    }

    /// `browser_profile(auth, agent)`: `(profile name, its browser, its folder)` of the browser profile this agent's browser
    /// tools open. Without a grant, the agent's own; for someone other than the owner, one of their own (the agent's own
    /// holds the owner's logins). A profile that is someone else's is a refusal.
    pub fn browser_profile(&self, agent: &str) -> R<(String, Option<String>, PathBuf)> {
        let person = self.acting();
        let (granted, why, refused) = self.granted(agent, "browser", None)?;
        if refused {
            return die(why);
        }
        let name = granted.unwrap_or_else(|| {
            if is_owner(self.auth, &person) {
                format!("browser/{agent}")
            } else {
                format!("browser/{agent}-{person}")
            }
        });
        let browser = get(get(get(self.auth, "profiles"), &name), "browser")
            .as_str()
            .filter(|b| !b.is_empty())
            .map(str::to_owned);
        let leaf = name.split_once('/').map_or(name.as_str(), |(_, r)| r);
        let folder = self.paths.machine.join("browsers").join(leaf);
        Ok((name, browser, folder))
    }
}
