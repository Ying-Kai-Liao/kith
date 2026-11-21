//! Saved sign-ins for websites and apps. A bot asks for one with `request_login` (a
//! secure card the user fills in), then `type_login` types it into the focused field;
//! the password reaches the screen, never the bot. Stored encrypted in the vault.
use crate::store::Store;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const SLOT: &str = "logins";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Login {
    pub id: String,
    /// A domain (`github.com`) or an app name (`Slack`).
    pub site: String,
    #[serde(default)]
    pub username: String,
    /// The password, or an `op://` reference resolved when it's typed.
    pub password: String,
}

impl Login {
    /// Everything but the password.
    pub fn public(&self) -> Value {
        json!({"id": self.id, "site": self.site, "username": self.username})
    }
}

pub fn all(store: &Store) -> Result<Vec<Login>> {
    Ok(match super::vault::read(store, SLOT)? {
        Some(s) if !s.is_empty() => serde_json::from_str(&s)?,
        _ => vec![],
    })
}

fn save_all(store: &Store, items: &[Login]) -> Result<()> {
    super::vault::write(store, SLOT, &serde_json::to_string(items)?)
}

pub fn list(store: &Store) -> Result<Value> {
    Ok(json!({"items": all(store)?.iter().map(Login::public).collect::<Vec<_>>()}))
}

/// `https://www.GitHub.com/login` → `github.com`; an app name stays as typed.
pub fn normalize_site(site: &str) -> String {
    let s = site.trim();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let s = s.split(['/', '?', '#']).next().unwrap_or(s);
    let s = s.rsplit_once('@').map_or(s, |(_, host)| host);
    s.strip_prefix("www.").unwrap_or(s).to_lowercase()
}

/// Saves a login, replacing the one for the same site and username.
pub fn save(store: &Store, site: &str, username: &str, password: &str) -> Result<Login> {
    let site = normalize_site(site);
    if site.is_empty() || site.len() > 200 {
        bail!("Give the website or app this login is for");
    }
    if password.is_empty() || password.len() > 4096 || username.len() > 512 {
        bail!("Invalid password");
    }
    let mut items = all(store)?;
    let username = username.trim().to_owned();
    let login = if let Some(l) = items.iter_mut().find(|l| l.site == site && l.username == username) {
        password.clone_into(&mut l.password);
        l.clone()
    } else {
        let taken: Vec<String> = items.iter().map(|l| l.id.clone()).collect();
        let l = Login { id: super::unique_id(&site, &taken), site, username, password: password.to_owned() };
        items.push(l.clone());
        l
    };
    save_all(store, &items)?;
    Ok(login)
}

pub fn remove(store: &Store, id: &str) -> Result<()> {
    let mut items = all(store)?;
    items.retain(|l| l.id != id);
    save_all(store, &items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_normalize() {
        assert_eq!(normalize_site("https://www.GitHub.com/login?x=1"), "github.com");
        assert_eq!(normalize_site("Slack"), "slack");
        assert_eq!(normalize_site("https://user@accounts.google.com"), "accounts.google.com");
    }
}
