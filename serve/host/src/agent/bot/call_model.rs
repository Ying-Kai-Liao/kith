//! Voice-call turns: a turn spoken in a call runs on the call model (a quick one by default), a typed turn on the
//! agent's own. Same Claude session either way: the session's model option is switched with `session/set_config_option`
//! (the adapter has no `session/set_model`) before the prompt, so the conversation, its cache and its tools carry over and
//! nothing relaunches. The adapter keeps the effort level consistent with the model it switches to.

use super::{Actor, Conn, NoticeStyle};
use serde_json::{Value, json};

/// What hq's launch said about models: the agent's own, and the call model it named.
#[derive(Clone)]
pub(super) struct CallModels {
    pub own: Option<String>,
    pub launch_call: Option<String>,
}

impl Conn {
    /// A session was just created or loaded: it is on its launch model, whatever a call switched an earlier one to.
    /// Remembers which model that is, so a typed turn can go back to it.
    pub(super) fn session_started(&mut self, sid: &str, answer: &Value) {
        self.call_sessions.remove(sid);
        self.call_refused.remove(sid);
        match crate::agent::auth::model_config(answer) {
            Some(option) => {
                let current = option["currentValue"].as_str().unwrap_or_default();
                if let (Some(id), false) = (option["id"].as_str(), current.is_empty()) {
                    self.model_options.insert(sid.to_owned(), (id.to_owned(), current.to_owned()));
                    return;
                }
                self.model_options.remove(sid);
            }
            None => {
                self.model_options.remove(sid);
            }
        }
    }
}

impl Actor {
    /// The call model that applies now, without waiting for kith: hq's last answer, else what the launch said.
    /// `None` keeps the agent's own model.
    fn call_model_now(&self, models: &CallModels) -> Option<String> {
        let named = self
            .cfg
            .hq_agent
            .as_deref()
            .and_then(crate::agent::hq::cached_call_model)
            .or_else(|| models.launch_call.clone());
        named.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty())
    }

    /// Puts `sid` on the model this turn wants. A refusal leaves the session on the model it has (the call just
    /// answers with the agent's own), says so once, and isn't asked again for that session.
    pub(super) async fn sync_call_model(&mut self, sid: &str) {
        let Some(models) = self.call_models.clone() else { return };
        let own = models.own.as_deref().filter(|m| !m.is_empty());
        let wanted = if self.turn_call { self.call_model_now(&models) } else { None };
        // The same model as the agent's own: nothing to switch.
        let wanted = wanted.filter(|m| Some(m.as_str()) != own);
        let Some(conn) = self.conn.as_ref() else { return };
        if conn.call_sessions.get(sid).cloned() == wanted || conn.call_refused.contains(sid) {
            return;
        }
        // A session with no model option can't be switched.
        let Some((config_id, started_on)) = conn.model_options.get(sid).cloned() else { return };
        let target = wanted.clone().unwrap_or(started_on);
        let params = json!({"sessionId": sid, "configId": config_id, "value": target});
        match self.session_request("session/set_config_option", params).await {
            Ok(_) => {
                if let Some(conn) = self.conn.as_mut() {
                    match wanted {
                        Some(model) => conn.call_sessions.insert(sid.to_owned(), model),
                        None => conn.call_sessions.remove(sid),
                    };
                }
            }
            Err(error) => {
                let bot = &self.cfg.id;
                tracing::warn!(bot = %bot, model = %target, error = format!("{error:#}"), "couldn't switch the call model");
                if let Some(conn) = self.conn.as_mut() {
                    conn.call_refused.insert(sid.to_owned());
                }
                if wanted.is_some() {
                    self.notice(&format!("Couldn't switch to the call model {target}: {error:#}"), NoticeStyle::Info);
                }
            }
        }
    }
}
