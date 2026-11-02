//! "New bot": every bot is a kith agent. `newKithAgent` makes one through the CLI (`kith new … --json`, then
//! `kith <agent> set harness|model|effort`), syncs the roster so its bot is there when the answer is, and gives that
//! bot the look the person picked: the host keeps only how a bot looks (avatar, pin, notifications); name,
//! description, engine, model and effort are the agent's. `kernelStatus` says whether the kernel can run at all
//! (kith missing, its Python or uv broken), so the app can tell the person what to do instead of failing silently.
//! Both are the owner's alone: on no member list and no bot list.

use super::str_arg;
use crate::agent::hq;
use crate::hub::Hub;
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a good answer of `kernelStatus` is reused, and a bad one (the person is fixing it, and looks again).
const STATUS_FRESH: Duration = Duration::from_secs(30);
const STATUS_FAILED_FRESH: Duration = Duration::from_secs(3);

/// Two makes at once (a double click): one at a time, so the second finds the name taken and says so.
static MAKES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The last `kernelStatus` answer, with the kernel it was for and when.
static STATUS: std::sync::Mutex<Option<(Instant, Option<PathBuf>, Value)>> = std::sync::Mutex::new(None);

/// The bot-row fields a client may give the new agent's bot: how it looks and whether it is pinned or notifies.
const APPEARANCE: [&str; 5] = ["avatarColor", "avatarShape", "avatarSource", "pinned", "notify"];

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "newKithAgent" => make(hub, &b).await?,
        "kernelStatus" => status(b["refresh"] == true).await,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

const NO_KITH: &str = "kith isn't installed on this computer";

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name != "kith"
        && (2..=31).contains(&name.len())
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A model, effort or harness name: it becomes an argument of `kith`, so nothing that could read as a flag.
fn word(v: &Value, what: &str) -> Result<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(w) if w.trim().is_empty() => Ok(None),
        Value::String(w) if !w.trim().starts_with('-') && !w.trim().contains(char::is_whitespace) => {
            Ok(Some(w.trim().to_owned()))
        }
        _ => bail!("{what} must be a name"),
    }
}

async fn make(hub: &Arc<Hub>, b: &Value) -> Result<Value> {
    let name = str_arg(b, "name")?.trim();
    let description = str_arg(b, "description")?.trim();
    if !valid_name(name) {
        bail!(
            "An agent's name is 2 to 31 lowercase letters, digits or dashes, starting with a letter (and not \"kith\")."
        );
    }
    if description.is_empty() {
        bail!("Say what the agent is for.");
    }
    // The kernel keeps the description in a line of agent.md; `: ` and a leading dash are what it can't hold.
    if description.contains(": ") || description.starts_with('-') || description.chars().any(char::is_control) {
        bail!("The description can't contain \": \", start with a dash or run over several lines.");
    }
    let settings: Vec<(&str, String)> = [("harness", "harness"), ("model", "model"), ("effort", "effort")]
        .into_iter()
        .filter_map(|(key, what)| word(&b[key], what).transpose().map(|w| w.map(|w| (what, w))))
        .collect::<Result<_>>()?;
    let mut look = serde_json::Map::new();
    for key in APPEARANCE {
        match (&b[key], key) {
            (Value::Null, _) | (Value::Bool(_), "pinned" | "notify") => {}
            (Value::String(s), "avatarColor" | "avatarShape" | "avatarSource") if s.len() <= 64 => {}
            _ => bail!("{key} isn't a valid value"),
        }
        if !b[key].is_null() {
            look.insert(key.to_owned(), b[key].clone());
        }
    }

    let bin = tokio::task::spawn_blocking(hq::bin)
        .await?
        .ok_or_else(|| anyhow!("{NO_KITH}; install it, then run `kith doctor` in Terminal"))?;
    let _one = MAKES.lock().await;
    let made = hq::new_agent(&bin, name, description).await?;
    let agent = made.agent;
    // The agent exists from here on; a failure after this says so, and leaves it.
    for (setting, value) in &settings {
        hq::set_setting(&agent, setting, value, &[])
            .await
            .map_err(|e| anyhow!("{agent} was made, but its {setting} couldn't be set: {e:#}"))?;
    }
    // A failed look for the bot leaves the next sync (every minute) to find it.
    if let Err(error) = hub.sync_hq_bots_with(&bin).await {
        tracing::warn!(error = format!("{error:#}"), "made an agent; the roster will catch up");
    }
    let row = hub
        .store
        .bots()?
        .into_iter()
        .find(|r| !r.deleted && r.config.person.is_none() && r.config.hq_agent.as_deref() == Some(agent.as_str()));
    let Some(row) = row else {
        return Ok(json!({"agent": agent, "botId": null, "bot": null}));
    };
    let id = row.config.id.clone();
    if !look.is_empty() {
        look.insert("id".into(), id.clone().into());
        let (patch, hub) = (Value::Object(look), hub.clone());
        tokio::task::spawn_blocking(move || hub.update_bot(&patch))
            .await?
            .map_err(|e| anyhow!("{agent} was made, but its look couldn't be saved: {e:#}"))?;
    }
    let bot = hub.store.bot(&id)?.map(|r| hub.bot_json(&r));
    Ok(json!({"agent": agent, "botId": id, "bot": bot}))
}

/// Whether the kernel can run: `{ok, bin, problem, hint}`. Asked of `kith ls --json` (the call every bot's roster
/// depends on), reused for [`STATUS_FRESH`] so a screen that asks on every open costs one run in half a minute.
/// `refresh` asks again now (the app's "check again" after the person ran `kith doctor`).
async fn status(refresh: bool) -> Value {
    let bin = tokio::task::spawn_blocking(hq::bin).await.ok().flatten();
    if !refresh
        && let Some((at, was, answer)) = STATUS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
        && *was == bin
        && at.elapsed() < if answer["ok"] == true { STATUS_FRESH } else { STATUS_FAILED_FRESH }
    {
        return answer.clone();
    }
    let answer = match &bin {
        None => json!({
            "ok": false, "bin": null, "problem": "kith isn't installed on this computer",
            "hint": "Install kith, then run `kith doctor` in Terminal",
        }),
        Some(path) => match hq::probe(path).await {
            Ok(()) => json!({"ok": true, "bin": path, "problem": null, "hint": null}),
            Err(problem) => json!({
                "ok": false, "bin": path, "problem": problem,
                "hint": "Run `kith doctor` in Terminal to see what's wrong",
            }),
        },
    };
    *STATUS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((Instant::now(), bin, answer.clone()));
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::account::ONE_AT_A_TIME;
    use crate::remote::identity::Identity;
    use crate::store::Store;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    struct Rig {
        dir: PathBuf,
        hub: Arc<Hub>,
        _one: tokio::sync::MutexGuard<'static, ()>,
    }

    /// A stand-in `kith`: `ls --json` lists what was made, `new … --json` makes a folder (or says the name is taken),
    /// `<agent> set … --json` is recorded (and refused for a setting named in `refuse`). Every call is logged.
    fn fake_kith(dir: &Path) -> PathBuf {
        let bin = dir.join("kith");
        let script = format!(
            r#"#!/bin/sh
D="{d}"
echo "$@" >> "$D/calls"
if [ "$1 $2" = "ls --json" ]; then
  [ -e "$D/broken" ] && {{ cat "$D/broken" >&2; exit 1; }}
  [ -e "$D/garbage" ] && {{ echo "not json"; exit 0; }}
  printf '['
  sep=''
  for a in "$D"/made/*; do [ -d "$a" ] && printf '%s{{"name":"%s","description":"d","home":"%s","harness":"%s","builtin":false}}' "$sep" "$(basename "$a")" "$a" "$(cat "$a/harness" 2>/dev/null || echo claude)" && sep=','; done
  echo ']'
  exit 0
fi
if [ "$1" = new ]; then
  [ -e "$D/old" ] && {{ echo "created $2"; exit 0; }}
  [ -e "$D/made/$2" ] && {{ echo '{{"ok": false, "error": "kith: there is already an agent called \"'"$2"'\""}}'; exit 1; }}
  mkdir -p "$D/made/$2/role"
  printf '{{"ok":true,"agent":"%s","home":"%s/made/%s","dir":"%s/made/%s/role","description":"%s"}}\n' "$2" "$D" "$2" "$D" "$2" "$3"
  exit 0
fi
if [ "$2" = set ]; then
  [ -e "$D/refuse-$3" ] && {{ echo '{{"ok": false, "error": "kith: no such '"$3"'"}}'; exit 2; }}
  echo "$4" > "$D/made/$1/$3"
  echo '{{"agent":"'"$1"'","setting":"'"$3"'","value":"'"$4"'","previous":null,"changed":true}}'
  exit 0
fi
exit 1
"#,
            d = dir.display()
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    impl Rig {
        async fn new() -> Self {
            let one = ONE_AT_A_TIME.lock().await;
            let dir = std::env::temp_dir().join(format!("kith-new-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("made")).unwrap();
            let store = Store::open(Path::new(":memory:")).unwrap();
            let hub = Hub::new(store, "new-test".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
            *hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kith(&dir));
            *STATUS.lock().unwrap() = None;
            Self { dir, hub, _one: one }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default().lines().map(str::to_owned).collect()
        }

        async fn make(&self, extra: Value) -> Result<Value> {
            let mut b = json!({"name": "trip-pal", "description": "Plans trips"});
            b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            make(&self.hub, &b).await
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            *hq::TEST_KERNEL_BIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn a_new_agent_is_made_then_set_in_order_then_its_bot_gets_the_look() {
        let rig = Rig::new().await;
        let got = rig
            .make(json!({"harness": "claude", "model": "opus", "effort": "high", "pinned": true, "notify": false,
                         "avatarSource": "shape", "avatarShape": "star", "avatarColor": "#112233"}))
            .await
            .unwrap();
        assert_eq!(got["agent"], "trip-pal");
        let calls = rig.calls();
        let order = [
            "new trip-pal Plans trips --json",
            "trip-pal set harness claude --json",
            "trip-pal set model opus --json",
            "trip-pal set effort high --json",
        ];
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("new") || c.contains(" set "))
                .take(4)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            order
        );
        let row = rig.hub.store.bots().unwrap().into_iter().find(|r| r.config.hq_agent.as_deref() == Some("trip-pal"));
        let cfg = row.unwrap().config;
        assert_eq!(got["botId"], cfg.id.as_str());
        assert_eq!(got["bot"]["id"], cfg.id.as_str());
        assert!(cfg.pinned && cfg.notify == Some(false));
        assert_eq!(
            (cfg.avatar_source.as_str(), cfg.avatar_shape.as_str(), cfg.avatar_color.as_str()),
            ("shape", "star", "#112233")
        );
        assert!(cfg.person.is_none() && !cfg.builtin);
    }

    #[tokio::test]
    async fn only_the_given_settings_are_set_and_none_is_fine() {
        let rig = Rig::new().await;
        rig.make(json!({"model": "sonnet"})).await.unwrap();
        let sets: Vec<String> = rig.calls().into_iter().filter(|c| c.contains(" set ")).collect();
        assert_eq!(sets, ["trip-pal set model sonnet --json"]);
    }

    #[tokio::test]
    async fn a_name_taken_shows_kiths_words_and_a_bad_one_never_reaches_kith() {
        let rig = Rig::new().await;
        rig.make(json!({})).await.unwrap();
        let again = rig.make(json!({})).await.unwrap_err();
        assert_eq!(again.to_string(), "there is already an agent called \"trip-pal\"");
        let before = rig.calls().len();
        for bad in [
            json!({"name": "Trip"}),
            json!({"name": "a"}),
            json!({"name": "kith"}),
            json!({"name": "-x"}),
            json!({"description": " "}),
            json!({"description": "a: b"}),
            json!({"description": "-x"}),
            json!({"model": "--bad"}),
            json!({"effort": "two words"}),
            json!({"pinned": "yes"}),
            json!({"avatarColor": 3}),
        ] {
            assert!(rig.make(bad.clone()).await.is_err(), "{bad}");
        }
        assert_eq!(rig.calls().len(), before, "the host refuses before asking");
    }

    #[tokio::test]
    async fn an_old_kith_that_prints_no_json_is_told_to_update() {
        let rig = Rig::new().await;
        std::fs::write(rig.dir.join("old"), "").unwrap();
        let e = rig.make(json!({})).await.unwrap_err().to_string();
        assert!(e.contains("update kith"), "{e}");
    }

    #[tokio::test]
    async fn a_setting_that_fails_after_the_agent_was_made_says_so_and_keeps_the_agent() {
        let rig = Rig::new().await;
        std::fs::write(rig.dir.join("refuse-effort"), "").unwrap();
        let e = rig.make(json!({"model": "opus", "effort": "high"})).await.unwrap_err().to_string();
        assert!(e.contains("trip-pal was made") && e.contains("effort") && e.contains("no such effort"), "{e}");
        assert!(rig.dir.join("made/trip-pal").is_dir(), "the agent stays");
        assert_eq!(std::fs::read_to_string(rig.dir.join("made/trip-pal/model")).unwrap().trim(), "opus");
    }

    #[tokio::test]
    async fn a_missing_kith_names_kith_doctor() {
        let rig = Rig::new().await;
        *hq::TEST_KERNEL_BIN.lock().unwrap() = None;
        let e = rig.make(json!({})).await.unwrap_err().to_string();
        assert!(e.contains("isn't installed") && e.contains("kith doctor"), "{e}");
    }

    #[tokio::test]
    async fn a_non_claude_agent_is_made_but_has_no_bot_yet() {
        let rig = Rig::new().await;
        let got = rig.make(json!({"harness": "codex"})).await.unwrap();
        assert_eq!(got["agent"], "trip-pal");
        assert!(got["botId"].is_null() && got["bot"].is_null());
    }

    #[tokio::test]
    async fn the_kernel_status_is_ok_missing_or_failing_and_is_reused_until_asked_again() {
        let rig = Rig::new().await;
        let ok = status(false).await;
        assert_eq!(ok["ok"], true);
        assert!(ok["problem"].is_null() && ok["hint"].is_null());
        // Reused: a break after the answer isn't seen until the person asks again.
        std::fs::write(
            rig.dir.join("broken"),
            "Traceback (most recent call last):\n  File \"x\"\nModuleNotFoundError: no module named 'yaml'\n",
        )
        .unwrap();
        assert_eq!(status(false).await["ok"], true);
        let bad = status(true).await;
        assert_eq!(bad["ok"], false);
        assert_eq!(bad["problem"], "ModuleNotFoundError: no module named 'yaml'");
        assert_eq!(bad["hint"], "Run `kith doctor` in Terminal to see what's wrong");

        std::fs::remove_file(rig.dir.join("broken")).unwrap();
        std::fs::write(rig.dir.join("garbage"), "").unwrap();
        let junk = status(true).await;
        assert_eq!(junk["ok"], false);
        assert!(junk["problem"].as_str().unwrap().contains("didn't print a list"), "{junk}");

        *hq::TEST_KERNEL_BIN.lock().unwrap() = None;
        let none = status(true).await;
        assert_eq!(none["ok"], false);
        assert!(none["bin"].is_null());
        assert_eq!(none["problem"], "kith isn't installed on this computer");
        assert_eq!(none["hint"], "Install kith, then run `kith doctor` in Terminal");
    }
}
