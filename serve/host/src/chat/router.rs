//! What a message sent while the bot works in the same lane means for that work, asked of Jev
//! (Typesafe's System One): stop it, restart it with a correction, give it its own turn, or join
//! the queue. Without a key, or when Jev fails, the message joins the queue as before.

use crate::market::vault;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const SLOT: &str = "router";
const TIMEOUT: Duration = Duration::from_secs(5);

/// Narrow yes/no questions (Jev ignores negation inside a question); thresholds from probes on 2026-10-16.
const QUESTIONS: [(&str, &str); 4] = [
    ("stop", "Does `latest` ask the assistant to stop, cancel or abort the work in `task`?"),
    ("change", "Does `latest` replace or correct an instruction given in `task`?"),
    ("add", "Does `latest` add a detail or an extra request to the work in `task`?"),
    ("other", "Is `latest` a request about a different subject from `task`?"),
];

/// Told at the start of a turn that redoes work the user corrected mid-run.
const INTERRUPTED: &str = "(Your previous attempt at this was stopped part-way because the user corrected it; files or other things it touched may be half-changed, so check before you go on.)\n\n";

/// The prompt that starts corrected work again: what the stopped turn was asked, then the correction.
pub fn redo(task: &str, correction: &str) -> String {
    format!(
        "{INTERRUPTED}The user sent several messages in a row; answer them together, in one reply:\n\n[1] {task}\n\n[2] {correction}"
    )
}

/// Only asked by the chat channels (`crate::channels`): the app chat lanes have no use for it.
const PARTIAL: (&str, &str) =
    ("partial", "Does `latest` end mid-thought, as if the user will send the rest in a next message?");

/// A partial score this high on a message that would merge makes the channel wait for the rest.
const PARTIAL_HOLD: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// End the running work and drop what waits.
    Stop,
    /// The running work is off course: stop it and start it again with this correction.
    Interrupt,
    /// A different subject: its own turn after the running one.
    Separate,
    /// More for the running work: joins the next turn with what else waits.
    Merge,
}

/// Jev's scores (0–1) for each question.
#[derive(Clone, Copy, Debug, Default)]
struct Scores {
    stop: f64,
    change: f64,
    add: f64,
    other: f64,
    /// 0 unless asked for.
    partial: f64,
}

impl Scores {
    fn decide(self) -> Route {
        self.decide_with(true)
    }

    /// `interrupt` off: a correction waits for the next turn like any other message.
    fn decide_with(self, interrupt: bool) -> Route {
        // "wait, do August instead" scores stop 0.80, change 0.96
        if self.stop >= 0.9 && self.stop > self.change {
            Route::Stop
        } else if self.change >= 0.9 && interrupt {
            Route::Interrupt
        } else if self.other >= 0.9 && self.add < 0.7 && self.change < 0.5 {
            // "make it sushi": other 0.93 but change 0.95
            Route::Separate
        } else {
            Route::Merge
        }
    }
}

/// What a channel does with a message sent while its agent works.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub route: Route,
    /// The message looks unfinished and would merge: wait a moment for the rest.
    pub hold: bool,
    /// The scores, for the log.
    pub path: String,
}

pub struct Jev {
    key: String,
    url: String,
    model: String,
}

impl Jev {
    /// `JEV_API_KEY` (and `JEV_BASE_URL`, `JEV_MODEL`) from the environment, else the key saved on this computer.
    pub async fn configured(hub: &Arc<crate::hub::Hub>) -> Option<Self> {
        let key = if let Some(key) = std::env::var("JEV_API_KEY").ok().filter(|k| !k.is_empty()) {
            key
        } else {
            // No saved key: don't touch the keychain for every message.
            hub.store.kv_read(SLOT).ok().flatten()?;
            vault::unlock(hub.clone()).await.ok()?;
            vault::read(&hub.store, SLOT).ok().flatten().filter(|k| !k.is_empty())?
        };
        Some(Self::from_key(key))
    }

    /// `key` with `JEV_BASE_URL` and `JEV_MODEL` from the environment.
    pub fn from_key(key: String) -> Self {
        let base = std::env::var("JEV_BASE_URL").unwrap_or_else(|_| "https://api.typesafe.ai".into());
        let model = std::env::var("JEV_MODEL").unwrap_or_else(|_| "jev-latest".into());
        Self::at(key, &base, model)
    }

    pub fn at(key: String, base: &str, model: String) -> Self {
        let base = base.trim_end_matches('/');
        let url = if base.ends_with("/systemone") {
            base.to_owned()
        } else if base.ends_with("/v1") {
            format!("{base}/systemone")
        } else {
            format!("{base}/v1/systemone")
        };
        Self { key, url, model }
    }

    /// How a channel handles `latest`, sent while `task` runs. Never fails: anything wrong merges.
    pub async fn judge(&self, task: &str, recent: &[String], latest: &str, interrupt: bool) -> Verdict {
        match self.scores(task, recent, latest, true).await {
            Ok(s) => {
                let route = s.decide_with(interrupt);
                let path = format!(
                    "stop:{:.2} change:{:.2} add:{:.2} other:{:.2} partial:{:.2}",
                    s.stop, s.change, s.add, s.other, s.partial
                );
                Verdict { route, hold: route == Route::Merge && s.partial >= PARTIAL_HOLD, path }
            }
            Err(e) => Verdict { route: Route::Merge, hold: false, path: format!("jev failed ({})", e.root_cause()) },
        }
    }

    /// One System One call, `state` and `questions` in, the response's JSON out. Fails on anything but a 2xx within
    /// [`TIMEOUT`]. Other users of Jev (the reply cards) read the answers themselves.
    pub async fn ask(&self, state: Value, questions: serde_json::Map<String, Value>) -> Result<Value> {
        let body = json!({"model": self.model, "state": state, "questions": questions});
        let res = reqwest::Client::new()
            .post(&self.url)
            .bearer_auth(&self.key)
            .header("User-Agent", "kith-serve")
            .json(&body)
            .timeout(TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        Ok(res.json().await?)
    }

    async fn scores(&self, task: &str, recent: &[String], latest: &str, partial: bool) -> Result<Scores> {
        let asked = QUESTIONS.iter().chain(partial.then_some(&PARTIAL));
        let questions: serde_json::Map<String, Value> =
            asked.map(|(k, q)| ((*k).to_owned(), json!({"type": "noul", "instructions": q}))).collect();
        let recent: Vec<Value> = recent.iter().map(|t| json!({"role": "user", "text": tail(t, 1000)})).collect();
        let v = self
            .ask(json!({"task": tail(task, 4000), "recent": recent, "latest": tail(latest, 2000)}), questions)
            .await?;
        let score = |k: &str| -> Result<f64> {
            let s = v["answers"][k]["noul"].as_f64().with_context(|| format!("no `{k}` score"))?;
            Ok(s.clamp(0.0, 1.0))
        };
        Ok(Scores {
            stop: score("stop")?,
            change: score("change")?,
            add: score("add")?,
            other: score("other")?,
            partial: if partial { score("partial")? } else { 0.0 },
        })
    }
}

/// How `latest`, sent while `task` runs, should be handled. Never fails: anything wrong merges.
pub async fn route(hub: &Arc<crate::hub::Hub>, task: &str, recent: &[String], latest: &str) -> Route {
    let Some(jev) = Jev::configured(hub).await else {
        return Route::Merge;
    };
    match jev.scores(task, recent, latest, false).await {
        Ok(s) => {
            let route = s.decide();
            tracing::info!(?route, stop = s.stop, change = s.change, add = s.add, other = s.other, "routed a message");
            route
        }
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), "message router failed; queueing the message");
            Route::Merge
        }
    }
}

/// Saves the Jev key on this computer (empty removes it).
pub async fn set_key(hub: &Arc<crate::hub::Hub>, key: &str) -> Result<Value> {
    let key = key.trim();
    if key.is_empty() {
        hub.store.kv_delete(SLOT)?;
    } else {
        vault::unlock(hub.clone()).await?;
        vault::write(&hub.store, SLOT, key)?;
    }
    status(&hub.store)
}

pub fn status(store: &Store) -> Result<Value> {
    let saved = store.kv_read(SLOT)?.is_some_and(|v| !v.is_empty());
    Ok(json!({"configured": saved || std::env::var("JEV_API_KEY").is_ok_and(|k| !k.is_empty())}))
}

/// The last `max` characters.
fn tail(s: &str, max: usize) -> &str {
    let n = s.chars().count();
    if n <= max {
        return s;
    }
    let (i, _) = s.char_indices().nth(n - max).expect("n > max, so index n - max exists");
    &s[i..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(stop: f64, change: f64, add: f64, other: f64) -> Scores {
        Scores { stop, change, add, other, partial: 0.0 }
    }

    #[test]
    fn decisions_follow_the_bridge_thresholds() {
        assert_eq!(s(0.97, 0.10, 0.05, 0.10).decide(), Route::Stop);
        // "wait, do August instead": a correction, not a stop
        assert_eq!(s(0.80, 0.96, 0.20, 0.30).decide(), Route::Interrupt);
        assert_eq!(s(0.95, 0.96, 0.10, 0.10).decide(), Route::Interrupt);
        assert_eq!(s(0.05, 0.10, 0.10, 0.95).decide(), Route::Separate);
        // "make it sushi": other subject but also a change
        assert_eq!(s(0.05, 0.95, 0.10, 0.93).decide(), Route::Interrupt);
        assert_eq!(s(0.05, 0.40, 0.80, 0.95).decide(), Route::Merge);
        assert_eq!(s(0.10, 0.20, 0.90, 0.10).decide(), Route::Merge);
    }

    #[test]
    fn interrupt_off_lets_a_correction_fall_through() {
        // A correction that reads as another subject becomes its own turn; otherwise it merges.
        assert_eq!(s(0.05, 0.95, 0.10, 0.10).decide_with(false), Route::Merge);
        assert_eq!(s(0.05, 0.40, 0.10, 0.10).decide_with(false), Route::Merge);
        assert_eq!(s(0.97, 0.10, 0.05, 0.10).decide_with(false), Route::Stop);
    }

    #[tokio::test]
    async fn an_empty_key_removes_the_saved_one() {
        let dir = std::env::temp_dir().join(format!("kith-router-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        store.kv_set(SLOT, "vault:v1:sealed").unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = crate::hub::Hub::new(store, "router-test".into(), identity, "test".into(), 0);

        let status = set_key(&hub, "  ").await.unwrap();
        assert_eq!(status["configured"], json!(false));
        assert!(Jev::configured(&hub).await.is_none());
    }

    #[test]
    fn tail_keeps_the_end_on_char_boundaries() {
        assert_eq!(tail("abc", 5), "abc");
        assert_eq!(tail("停一下好嗎", 2), "好嗎");
    }
}
