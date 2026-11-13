//! Quick chats: a thread of the built-in scratch agent kept as a real agent. `keepScratchThread` is the owner's alone
//! (it makes an agent on this computer); it is on no member or bot list. The kernel does the work
//! (`kith scratch keep`); the host names the thread, adds what only it knows (the thread's engine, its harness
//! session, a summary for the draft) and syncs the roster so the new agent's bot is there when the answer is.

use super::str_arg;
use crate::agent::hq::{self, ThreadEngine};
use crate::hub::Hub;
use crate::store::{Lane, lane_key};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

const KEEP_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a missing summary is written for, before the agent is made without one.
const SUMMARY_WAIT: Duration = Duration::from_secs(60);

/// Two keeps at once (a double click): one at a time, so the second finds the name taken and says so.
static KEEPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) async fn call(hub: &Arc<Hub>, method: &str, b: Value) -> Result<ControlFlow<Value, Value>> {
    Ok(ControlFlow::Break(match method {
        "keepScratchThread" => keep(hub, &b).await?,
        _ => return Ok(ControlFlow::Continue(b)),
    }))
}

/// What `kith scratch keep` failed with, as one line for the person: the kernel's own words without its prefix.
fn said(error: &anyhow::Error) -> anyhow::Error {
    let text = format!("{error:#}");
    let line = text.rsplit_once("kith scratch: ").map_or(text.as_str(), |(_, said)| said);
    // `kith-new` and `kith-pkg` speak as themselves.
    let line = line.rsplit_once("kith: ").map_or(line, |(_, said)| said);
    anyhow!("{}", line.trim())
}

async fn keep(hub: &Arc<Hub>, b: &Value) -> Result<Value> {
    let (bot, thread, name) = (str_arg(b, "botId")?, str_arg(b, "threadId")?, str_arg(b, "name")?.trim());
    if name.is_empty() {
        bail!("Name the agent.");
    }
    // The name is an argument of `kith`: no flag, no second word.
    if name.starts_with('-') || name.len() > 64 || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("'{name}' isn't a name an agent can have.");
    }
    let row = hub.store.bot(bot)?.filter(|r| !r.deleted).ok_or_else(|| anyhow!("unknown bot"))?;
    let cfg = &row.config;
    let agent = cfg.hq_agent.as_deref().filter(|_| cfg.builtin && cfg.person.is_none());
    let agent = agent.ok_or_else(|| anyhow!("Only a quick chat can be kept as an agent."))?;
    let root = hub.store.entry(thread).filter(|e| e.bot_id == bot && e.thread_id.is_none());
    let root = root.ok_or_else(|| anyhow!("unknown thread"))?;

    // The engine and model this chat started with are the host's record; the kernel's ledger only has the session.
    let engine = ThreadEngine::of_thread(&hub.store, bot, &root.id);
    let session = hub
        .store
        .kv_get(&lane_key("session", bot, &Lane::in_thread(bot, &root.id)))
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.starts_with('-'));
    // The draft is made from the chat's summary; one that hasn't been written yet is written now. A chat too short
    // for one becomes an agent with an empty draft.
    if !hub.store.with_db(|c| kith_mem::summary::get(c, &root.id).ok().flatten().is_some()) {
        match tokio::time::timeout(SUMMARY_WAIT, crate::hqmem::summary::summarise_now(hub, &root.id, false)).await {
            Ok(Err(error)) => tracing::info!(error = format!("{error:#}"), "no summary for the kept chat"),
            Err(_) => tracing::info!("the summary of the kept chat took too long"),
            Ok(Ok(_)) => {}
        }
    }

    let bin = tokio::task::spawn_blocking(hq::bin)
        .await?
        .ok_or_else(|| anyhow!("kith isn't installed on this computer (set KITH_KERNEL_BIN, or put `kith` on PATH)"))?;
    let mut args = vec![agent, "keep", root.id.as_str(), name, "--json"];
    if let Some(engine) = &engine {
        args.extend(["-H", engine.harness.as_str()]);
        args.extend(engine.model.iter().flat_map(|m| ["-m", m.as_str()]));
    }
    if let Some(session) = &session {
        args.extend(["--session", session.as_str()]);
    }
    let _one = KEEPS.lock().await;
    let out = hq::run(&bin, &args, None, KEEP_TIMEOUT).await.map_err(|e| said(&e))?;
    let kept: Value = serde_json::from_str(out.trim()).context("`kith scratch keep --json` didn't print JSON")?;
    // The agent exists now; a failed look for it leaves the next sync (every minute) to find it.
    if let Err(error) = hub.sync_hq_bots_with(&bin).await {
        tracing::warn!(error = format!("{error:#}"), "kept a chat as an agent; the roster will catch up");
    }
    let kept_name = kept["agent"].as_str().unwrap_or(name);
    let bot_id = hub
        .store
        .bots()?
        .into_iter()
        .find(|r| !r.deleted && r.config.person.is_none() && r.config.hq_agent.as_deref() == Some(kept_name))
        .map(|r| r.config.id);
    Ok(json!({"agent": kept_name, "botId": bot_id, "kept": kept}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::account::ONE_AT_A_TIME;
    use crate::remote::identity::Identity;
    use crate::store::{EntryKind, Store};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    struct Rig {
        dir: PathBuf,
        hub: Arc<Hub>,
        _one: tokio::sync::MutexGuard<'static, ()>,
    }

    /// A stand-in `kith`: `ls --json` lists scratch and whatever was kept; `scratch keep` records its arguments and
    /// makes the agent's folder, or refuses a name that exists.
    fn fake_kith(dir: &Path) -> PathBuf {
        let bin = dir.join("kith");
        let script = format!(
            r#"#!/bin/sh
D="{d}"
echo "$@" >> "$D/calls"
if [ "$1 $2" = "ls --json" ]; then
  printf '[{{"name":"scratch","description":"quick","home":"%s/scratch","harness":"claude","builtin":true}}' "$D"
  for a in "$D"/kept/*; do [ -d "$a" ] && printf ',{{"name":"%s","description":"kept","home":"%s","harness":"claude","builtin":false}}' "$(basename "$a")" "$a"; done
  echo ']'
  exit 0
fi
if [ "$2" = --launch-json ]; then
  H=claude; M=null
  while [ $# -gt 0 ]; do case "$1" in -H) H=$2; shift;; -m) M="\"$2\"";; esac; shift; done
  printf '{{"version":1,"agent":"scratch","agent_version":"1","harness":"%s","model":%s,"cwd":"","env":{{}},"system_prompt":"","mcp_servers":{{}},"add_dirs":[],"allowed_tools":[],"disable_slash_commands":false,"cost_tag":{{"agent":"scratch","version":"1","via":"serve"}}}}\n' "$H" "$M"
  exit 0
fi
if [ "$2" = keep ]; then
  [ -e "$D/kept/$4" ] && {{ echo "kith scratch: there is already an agent called \"$4\"" >&2; exit 1; }}
  mkdir -p "$D/kept/$4"
  printf '{{"agent":"%s","dir":"%s/kept/%s/role","home":"%s/kept/%s","thread":"%s","description":"d","harness":"claude","model":null,"from_summary":false}}\n' "$4" "$D" "$4" "$D" "$4" "$3"
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
            let dir = std::env::temp_dir().join(format!("kith-scratch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("scratch")).unwrap();
            std::fs::create_dir_all(dir.join("kept")).unwrap();
            let store = Store::open(Path::new(":memory:")).unwrap();
            let hub = Hub::new(store, "scratch-test".into(), Identity::load_or_create(&dir).unwrap(), "t".into(), 0);
            *hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kith(&dir));
            hub.sync_hq_bots().await.unwrap();
            Self { dir, hub, _one: one }
        }

        fn scratch(&self) -> crate::store::BotConfig {
            self.hub.store.bots().unwrap().into_iter().map(|r| r.config).find(|c| c.builtin).unwrap()
        }

        /// A quick chat's root message.
        fn chat(&self) -> String {
            let bot = self.scratch().id;
            self.hub.add_entry(&Lane::main(&bot), EntryKind::User, 1, &json!({"text": "hi"})).unwrap().id
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default().lines().map(str::to_owned).collect()
        }

        async fn keep(&self, thread: &str, name: &str) -> Result<Value> {
            let bot = self.scratch().id;
            keep(&self.hub, &json!({"botId": bot, "threadId": thread, "name": name})).await
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            *hq::TEST_KERNEL_BIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn the_built_in_agent_gets_one_flagged_bot_whatever_the_syncs() {
        let rig = Rig::new().await;
        rig.hub.sync_hq_bots().await.unwrap();
        let all = rig.hub.store.bots().unwrap();
        assert_eq!(all.len(), 1, "one scratch bot, not one per sync");
        assert!(all[0].config.builtin && all[0].config.hq_agent.as_deref() == Some("scratch"));
        // A client can't mark or unmark a bot.
        let patched =
            super::super::bots::call(&rig.hub, "updateBot", json!({"id": all[0].config.id, "builtin": false}))
                .await
                .unwrap();
        assert!(matches!(patched, ControlFlow::Break(_)));
        assert!(rig.scratch().builtin);
    }

    #[tokio::test]
    async fn keeping_a_chat_runs_the_kernel_with_its_engine_and_session_then_the_roster_has_the_agent() {
        let rig = Rig::new().await;
        let (bot, root) = (rig.scratch().id, rig.chat());
        ThreadEngine::ask(Some("codex"), Some("gpt-5")).unwrap().unwrap().save(&rig.hub.store, &bot, &root).unwrap();
        rig.hub.store.kv_set(&lane_key("session", &bot, &Lane::in_thread(&bot, &root)), "sess-9").unwrap();
        rig.hub.store.with_db(|c| {
            kith_mem::summary::put(
                c,
                &kith_mem::summary::Summary {
                    thread_id: root.clone(),
                    room: String::new(),
                    agent: "scratch".into(),
                    audience: "owner".into(),
                    projects: Vec::new(),
                    text: "Planning.".into(),
                    links: Vec::new(),
                    covered_seq: 1,
                    reason: kith_mem::summary::Reason::End,
                    written_at: 1,
                    cost_via: String::new(),
                },
            )
            .unwrap();
        });

        let got = rig.keep(&root, "trip-pal").await.unwrap();
        assert_eq!(got["agent"], "trip-pal");
        let keep_call = rig.calls().into_iter().find(|c| c.contains("keep")).unwrap();
        assert_eq!(keep_call, format!("scratch keep {root} trip-pal --json -H codex -m gpt-5 --session sess-9"));
        let kept_bot = rig.hub.store.bots().unwrap().into_iter().find(|r| r.config.name == "trip-pal").unwrap();
        assert_eq!(got["botId"], kept_bot.config.id.as_str(), "the agent's bot is there when the answer is");
        assert!(!kept_bot.config.builtin, "a kept chat is an ordinary bot");
    }

    #[tokio::test]
    async fn a_name_taken_or_bad_or_a_chat_that_is_not_scratchs_fails_cleanly() {
        let rig = Rig::new().await;
        let root = rig.chat();
        rig.keep(&root, "trip-pal").await.unwrap();
        // The double click: the second says why, in the kernel's words and no more.
        let again = rig.keep(&root, "trip-pal").await.unwrap_err();
        assert_eq!(again.to_string(), "there is already an agent called \"trip-pal\"");
        assert_eq!(rig.hub.store.bots().unwrap().len(), 2, "no second bot");

        for bad in ["", "  ", "-H", "two words", "x\ny"] {
            assert!(rig.keep(&root, bad).await.is_err(), "{bad:?}");
        }
        assert_eq!(rig.calls().iter().filter(|c| c.contains("keep")).count(), 2, "the host refuses before asking");
        assert!(rig.keep("no-such-thread", "other").await.unwrap_err().to_string().contains("unknown thread"));

        // Not a quick chat: an ordinary bot's thread.
        let kept = rig.hub.store.bots().unwrap().into_iter().find(|r| r.config.name == "trip-pal").unwrap();
        let theirs =
            rig.hub.add_entry(&Lane::main(&kept.config.id), EntryKind::User, 1, &json!({"text": "x"})).unwrap();
        let err = keep(&rig.hub, &json!({"botId": kept.config.id, "threadId": theirs.id, "name": "other"}))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Only a quick chat can be kept as an agent.");
    }

    #[tokio::test]
    async fn a_threads_launch_carries_its_engine_and_model_and_other_launches_do_not() {
        let rig = Rig::new().await;
        let go = |engine: Option<ThreadEngine>| {
            let bin = hq::bin().unwrap();
            async move {
                hq::launch(&bin, "scratch", "/", None, None, None, "r", &hq::Members::Unknown, None, engine.as_ref())
                    .await
            }
        };
        let plain = go(None).await.unwrap();
        assert_eq!((plain.harness, plain.model.as_deref()), (Some(hq::Harness::Claude), None));
        let model = go(ThreadEngine::ask(None, Some("opus")).unwrap()).await.unwrap();
        assert_eq!(model.model.as_deref(), Some("opus"));
        let codex = go(ThreadEngine::ask(Some("codex"), Some("gpt-5")).unwrap()).await.unwrap();
        assert_eq!((codex.harness, codex.model.as_deref()), (Some(hq::Harness::Codex), Some("gpt-5")));
        let calls: Vec<String> = rig.calls().into_iter().filter(|c| c.contains("--launch-json")).collect();
        assert_eq!(
            calls,
            [
                "scratch --launch-json",
                "scratch --launch-json -H claude -m opus",
                "scratch --launch-json -H codex -m gpt-5"
            ]
        );
        // Every engine of the kernel's table starts, whoever asked (A2: all through `--launch-json`).
        *hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kith_harness(&rig.dir, "codex"));
        assert_eq!(go(None).await.unwrap().harness, Some(hq::Harness::Codex));
        // An engine this host doesn't know is never started.
        *hq::TEST_KERNEL_BIN.lock().unwrap() = Some(fake_kith_harness(&rig.dir, "no-such-engine"));
        assert!(go(None).await.is_err());
    }

    /// A kith whose launch says `harness` whatever was asked.
    fn fake_kith_harness(dir: &Path, harness: &str) -> PathBuf {
        let bin = dir.join(format!("kith-{harness}"));
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '{"version":1,"agent":"scratch","agent_version":"1","harness":"HARNESS","cwd":"","env":{},"system_prompt":"","mcp_servers":{},"add_dirs":[],"allowed_tools":[],"disable_slash_commands":false,"cost_tag":{"agent":"scratch","version":"1","via":"serve"}}\n'
"#
            .replace("HARNESS", harness),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[tokio::test]
    async fn a_new_quick_chat_keeps_its_engine_for_that_thread_and_a_bad_one_sends_nothing() {
        let rig = Rig::new().await;
        let bot = rig.scratch().id;
        let send = |extra: Value| {
            let mut b = json!({"botId": bot, "text": "hello", "clientNonce": uuid::Uuid::new_v4().to_string()});
            b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let hub = rig.hub.clone();
            async move { super::super::bots::call(&hub, "send", b).await }
        };
        let err = send(json!({"engine": "no-such-engine"})).await.unwrap_err();
        assert!(err.to_string().contains("no-such-engine"), "{err}");
        assert!(rig.hub.store.history(&bot, i64::MAX, 10).unwrap().is_empty(), "nothing saved");

        let ControlFlow::Break(first) = send(json!({"engine": "codex", "engineModel": "gpt-5"})).await.unwrap() else {
            panic!("answered")
        };
        let root = first["entry"]["id"].as_str().unwrap();
        let kept = ThreadEngine::of_thread(&rig.hub.store, &bot, root).unwrap();
        assert_eq!((kept.harness.as_str(), kept.model.as_deref()), ("codex", Some("gpt-5")));
        // Two quick chats are two threads, each with its own.
        let ControlFlow::Break(other) = send(json!({"newTask": true})).await.unwrap() else { panic!("answered") };
        let other = other["entry"]["id"].as_str().unwrap();
        assert_ne!(other, root);
        assert!(ThreadEngine::of_thread(&rig.hub.store, &bot, other).is_none(), "scratch's own");
    }

    #[test]
    fn a_thread_engine_is_a_known_engine_and_a_plain_model_name() {
        assert_eq!(ThreadEngine::ask(None, None).unwrap(), None);
        assert_eq!(ThreadEngine::ask(Some(" "), Some("")).unwrap(), None);
        let e = ThreadEngine::ask(None, Some("opus")).unwrap().unwrap();
        assert_eq!((e.harness.as_str(), e.model.as_deref()), ("claude", Some("opus")));
        // Any engine of the kernel's table that speaks ACP, not only Claude and Codex.
        for h in crate::agent::backends::harnesses() {
            assert_eq!(ThreadEngine::ask(Some(h.id), None).unwrap().unwrap().harness, h.id);
        }
        assert!(ThreadEngine::ask(Some("gemini"), None).is_ok());
        assert!(ThreadEngine::ask(Some("no-such-engine"), None).is_err());
        for bad in ["--model", "-x", "a b", "a;b", &"m".repeat(65)] {
            assert!(ThreadEngine::ask(Some("claude"), Some(bad)).is_err(), "{bad}");
        }
        assert!(ThreadEngine::ask(Some("claude"), Some("claude-opus-4-1[1m]")).is_ok());
    }
}
