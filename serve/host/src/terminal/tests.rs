//! Terminal threads against synthetic transcripts in a temp folder. No real transcript is ever copied here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::jsonl::{self, Item};
use super::registry::{self, Live};
use super::{Env, HeldByWriter, Terminal, sync};
use crate::LockExt as _;
use crate::hub::Hub;
use crate::store::{BotConfig, Entry, Lane, Store, lane_key};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn line(kind: &str, ts: &str, extra: &Value, message: &Value) -> String {
    let mut v = json!({"type": kind, "timestamp": ts, "isSidechain": false, "message": message});
    for (k, x) in extra.as_object().into_iter().flatten() {
        v[k] = x.clone();
    }
    format!("{v}\n")
}

fn user(ts: &str, text: &str) -> String {
    line("user", ts, &json!({}), &json!({"role": "user", "content": text}))
}

fn agent(ts: &str, text: &str) -> String {
    line("assistant", ts, &json!({}), &json!({"role": "assistant", "content": [{"type": "text", "text": text}]}))
}

fn tool_use(ts: &str, id: &str, command: &str) -> String {
    let block = json!({"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}});
    line("assistant", ts, &json!({}), &json!({"role": "assistant", "content": [block]}))
}

fn tool_result(ts: &str, id: &str, out: &str) -> String {
    let block = json!({"type": "tool_result", "tool_use_id": id, "content": out});
    line("user", ts, &json!({}), &json!({"role": "user", "content": [block]}))
}

struct Rig {
    children: std::cell::RefCell<Vec<std::process::Child>>,
    dir: PathBuf,
    hub: Arc<Hub>,
    bot: String,
}

impl Rig {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kith-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::create_dir_all(registry::dir(&dir.join("state"))).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        let identity = crate::remote::identity::Identity::load_or_create(&dir).unwrap();
        let hub = Hub::new(store, "host".into(), identity, "token".into(), 0);
        hub.terminal.set_env(Env { state: dir.join("state"), sessions: Some(dir.join("sessions")) });
        let cfg: BotConfig = serde_json::from_value(json!({
            "id": "", "name": "builder", "backend": "claude", "cwd": dir.to_string_lossy(),
            "hqAgent": "builder", "permission": "default", "tasks": true, "connectors": [], "skills": [],
        }))
        .unwrap();
        let bot = hub.create_bot(cfg).unwrap()["id"].as_str().unwrap().to_owned();
        Self { children: std::cell::RefCell::default(), dir, hub, bot }
    }

    /// The pid of a process that lives for the test: a registry can't name the test's own.
    fn sleeper(&self) -> u32 {
        let child = std::process::Command::new("sleep").arg("300").spawn().unwrap();
        let pid = child.id();
        self.children.borrow_mut().push(child);
        pid
    }

    fn transcript(&self, sid: &str) -> PathBuf {
        self.dir.join(format!("{sid}.jsonl"))
    }

    fn append(&self, sid: &str, text: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.transcript(sid)).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// A registry file for `sid` whose process is this test's (alive) or not.
    fn register(&self, sid: &str, agent: &str, pid: u32, resumed_from: Option<&str>) {
        let reg = json!({
            "v": 1, "sessionId": sid, "agent": agent, "cwd": self.dir.to_string_lossy(),
            "transcriptPath": self.transcript(sid).to_string_lossy(), "pid": pid, "project": null,
            "source": "startup", "resumedFrom": resumed_from, "startedAt": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(registry::dir(&self.dir.join("state")).join(format!("{sid}.json")), reg.to_string()).unwrap();
    }

    fn session_file(&self, pid: u32, sid: &str, status: &str) {
        let v = json!({"pid": pid, "sessionId": sid, "status": status});
        std::fs::write(self.dir.join("sessions").join(format!("{pid}.json")), v.to_string()).unwrap();
    }

    fn roots(&self) -> Vec<Entry> {
        self.hub
            .store
            .history(&self.bot, i64::MAX, 100)
            .unwrap()
            .into_iter()
            .filter(|e| e.data["task"] == true)
            .collect()
    }

    fn thread(&self, root: &str) -> Vec<Entry> {
        self.hub.store.thread(&self.bot, root, 500).unwrap()
    }

    fn texts(&self, root: &str) -> Vec<String> {
        self.thread(root).iter().filter_map(|e| e.data["text"].as_str().map(str::to_owned)).collect()
    }

    fn summary(&self, root: &str) -> Value {
        self.hub.store.entry(root).unwrap().data["thread"].clone()
    }

    fn scan(&self) {
        sync::scan(&self.hub);
    }
}

const DEAD: u32 = 2_000_000_000;

#[test]
fn the_importer_keeps_the_conversation_and_leaves_the_rest() {
    let mut text = String::new();
    text += &user("2026-10-23T01:00:00Z", "fix the build");
    text += &line("user", "2026-10-23T01:00:01Z", &json!({"isMeta": true}), &json!({"content": "meta line"}));
    text += &line("user", "2026-10-23T01:00:02Z", &json!({"isSidechain": true}), &json!({"content": "subagent talk"}));
    text += &user("2026-10-23T01:00:03Z", "<command-name>/clear</command-name>");
    text += &user("2026-10-23T01:00:04Z", "<local-command-stdout>ok</local-command-stdout>");
    text += &user("2026-10-23T01:00:05Z", "<system-reminder>be nice</system-reminder>");
    text += &user("2026-10-23T01:00:06Z", "ship it<system-reminder>be nice</system-reminder>");
    text += &line("user", "2026-10-23T01:00:07Z", &json!({"isCompactSummary": true}), &json!({"content": "summary"}));
    text += &line("queue-operation", "2026-10-23T01:00:08Z", &json!({}), &json!({}));
    text += &agent("2026-10-23T01:00:09Z", "On it.");
    text += &tool_use("2026-10-23T01:00:10Z", "t1", "cargo build\nsecond line");
    text += &tool_result("2026-10-23T01:00:11Z", "t1", "built");
    let think = json!({"role": "assistant", "content": [{"type": "thinking", "thinking": "hmm"}]});
    text += &line("assistant", "2026-10-23T01:00:12Z", &json!({}), &think);
    let synthetic = json!({"model": "<synthetic>", "content": [{"type": "text", "text": "No response requested."}]});
    text += &line("assistant", "2026-10-23T01:00:13Z", &json!({}), &synthetic);
    text += "{\"type\":\"user\",\"half a line";

    let parsed = jsonl::parse(text.as_bytes(), 0);
    let kinds: Vec<String> = parsed
        .items
        .iter()
        .map(|i| match i {
            Item::User { text, .. } => format!("user:{text}"),
            Item::Agent { text, .. } => format!("agent:{text}"),
            Item::Thought { text, .. } => format!("thought:{text}"),
            Item::ToolUse { title, kind, .. } => format!("tool:{kind}:{title}"),
            Item::ToolResult { id, output, failed } => format!("result:{id}:{output}:{failed}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user:fix the build",
            "user:ship it",
            "agent:On it.",
            "tool:execute:Bash cargo build",
            "result:t1:built:false",
            "thought:hmm",
        ]
    );
    assert!(!text[parsed.consumed..].contains('\n'), "the half line waits for the rest of it");
    assert!(text[parsed.consumed..].starts_with("{\"type\":\"user\",\"half"));

    // Lines at or before `since` are skipped.
    let later = jsonl::parse(
        text.as_bytes(),
        chrono::DateTime::parse_from_rfc3339("2026-10-23T01:00:09Z").unwrap().timestamp_millis(),
    );
    assert_eq!(later.items.len(), 3, "after the reply: the tool, its result and the thought");
}

#[test]
fn a_registry_file_is_only_read_when_it_is_a_listable_sessions() {
    let ok = json!({
        "v": 1, "sessionId": "abc-1", "agent": "builder", "cwd": "/w", "transcriptPath": "/p/abc-1.jsonl",
        "pid": 7, "project": null, "source": "startup", "resumedFrom": null, "startedAt": "2026-10-23T01:00:00Z",
    });
    assert!(registry::parse(&ok.to_string()).is_some());
    let with = |k: &str, v: Value| {
        let mut x = ok.clone();
        x[k] = v;
        x.to_string()
    };
    assert!(registry::parse("not json").is_none());
    assert!(registry::parse(&with("person", json!("sam"))).is_none(), "a person's session is never listed");
    assert!(registry::parse(&with("personId", json!("sam"))).is_none());
    assert!(registry::parse(&with("v", json!(2))).is_none());
    assert!(registry::parse(&with("sessionId", json!("../x"))).is_none());
    assert!(registry::parse(&with("transcriptPath", json!("/p/other.jsonl"))).is_none());
    assert!(registry::parse(&with("cwd", json!("relative"))).is_none());
}

#[tokio::test]
async fn live_needs_the_process_and_a_session_file_that_names_the_session() {
    let r = Rig::new();
    let me = r.sleeper();
    r.register("sess-1", "builder", me, None);
    let reg = registry::read_all(&r.dir.join("state")).pop().unwrap();
    let sessions = Some(r.dir.join("sessions"));
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Closed, "no session file");
    r.session_file(me, "sess-1", "idle");
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Idle);
    r.session_file(me, "sess-1", "busy");
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Busy);
    r.session_file(me, "sess-1", "something-new");
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Busy, "an unknown state counts as working");
    r.session_file(me, "other-session", "idle");
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Closed, "a reused pid names another session");
    r.session_file(DEAD, "sess-1", "idle");
    r.register("sess-1", "builder", DEAD, None);
    let dead = registry::read_all(&r.dir.join("state")).pop().unwrap();
    assert_eq!(registry::liveness(&dead, sessions.as_deref()), Live::Closed, "the process is gone");
}

#[tokio::test]
async fn a_session_becomes_one_thread_and_a_rescan_changes_nothing() {
    let r = Rig::new();
    // Nothing the user said yet: no thread.
    r.append("s1", &user("2026-10-23T01:00:00Z", "<command-name>/model</command-name>"));
    r.register("s1", "builder", DEAD, None);
    r.scan();
    assert!(r.roots().is_empty());
    r.append("s1", &user("2026-10-23T01:00:01Z", "fix the build"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "Looking."));
    r.append("s1", &tool_use("2026-10-23T01:00:03Z", "t1", "ls"));
    r.append("s1", &tool_result("2026-10-23T01:00:04Z", "t1", "a b"));
    r.append("s1", &agent("2026-10-23T01:00:05Z", "Done."));
    r.scan();
    let roots = r.roots();
    assert_eq!(roots.len(), 1);
    let root = roots[0].id.clone();
    assert_eq!(roots[0].data["text"], "fix the build");
    assert_eq!(
        roots[0].created_at,
        chrono::DateTime::parse_from_rfc3339("2026-10-23T01:00:01Z").unwrap().timestamp_millis()
    );
    let thread = r.thread(&root);
    let kinds: Vec<&str> = thread.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["notice", "agent", "tool", "agent"], "the first message is the root; the rest is the thread");
    assert_eq!(thread[0].data["writerChange"], "startedInTerminal");
    assert_eq!(thread[2].data["status"], "completed");
    assert_eq!(thread[2].data["output"], "a b");
    assert_eq!(thread[1].data["final"], false, "only the last reply of the turn is final");
    assert_eq!(thread[3].data["final"], true);
    let summary = r.summary(&root);
    assert_eq!(summary["origin"], "terminal");
    assert_eq!(summary["liveWriter"], Value::Null);
    assert_eq!(summary["liveBusy"], false);
    assert_eq!(summary["terminal"]["sessionId"], "s1");
    assert_eq!(summary["unread"], 0);
    // The thread continues the terminal's session, in its folder.
    let lane = Lane::in_thread(&r.bot, &root);
    assert_eq!(r.hub.store.kv_get(&lane_key("session", &r.bot, &lane)).as_deref(), Some("s1"));

    let before = (r.hub.store.current_rev(), r.texts(&root));
    r.scan();
    r.scan();
    assert_eq!((r.hub.store.current_rev(), r.texts(&root)), before, "rescans are idempotent");
    assert_eq!(r.roots().len(), 1);
}

#[tokio::test]
async fn a_session_of_an_agent_without_an_owner_bot_is_skipped() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:01Z", "hi"));
    r.register("s1", "stranger", DEAD, None);
    r.scan();
    assert!(r.roots().is_empty());
}

#[tokio::test]
async fn new_lines_are_imported_once_and_the_terminal_writes_while_it_lives() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "one"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "busy");
    r.scan();
    let root = r.roots()[0].id.clone();
    let s = r.summary(&root);
    assert_eq!((s["liveWriter"].as_str(), s["liveBusy"].as_bool()), (Some("terminal"), Some(true)));

    r.append("s1", &user("2026-10-23T01:00:03Z", "second"));
    r.append("s1", &agent("2026-10-23T01:00:04Z", "two"));
    // A half-written line waits.
    r.append("s1", "{\"type\":\"assistant\",\"time");
    r.scan();
    r.scan();
    assert_eq!(r.texts(&root).iter().filter(|t| *t == "two").count(), 1);
    assert_eq!(r.texts(&root).iter().filter(|t| *t == "second").count(), 1);
    let finals: Vec<bool> =
        r.thread(&root).iter().filter(|e| e.kind == "agent").map(|e| e.data["final"] == true).collect();
    assert_eq!(finals, [true, true], "each turn keeps its last reply final");

    // The terminal goes idle, then closes: a notice for the close, once.
    r.session_file(me, "s1", "idle");
    r.scan();
    assert_eq!(r.summary(&root)["liveBusy"], false);
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    r.scan();
    r.scan();
    let changes: Vec<String> =
        r.thread(&root).iter().filter_map(|e| e.data["writerChange"].as_str().map(str::to_owned)).collect();
    assert_eq!(changes, ["startedInTerminal", "terminalClosed"]);
    assert_eq!(r.summary(&root)["liveWriter"], Value::Null);
}

#[tokio::test]
async fn a_host_turn_moves_the_cursor_so_its_lines_are_not_imported_twice() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    // The host continues the session: its turn is in the entries already and lands in the transcript.
    r.append("s1", &user("2026-10-23T02:00:00Z", "from the app"));
    r.append("s1", &agent("2026-10-23T02:00:01Z", "app reply"));
    sync::host_turn_ended(&r.hub, &Lane::in_thread(&r.bot, &root));
    r.scan();
    assert!(!r.texts(&root).contains(&"app reply".to_owned()), "the host's turn is not imported");
    // The terminal later adds a line: that one is.
    r.append("s1", &agent("2026-10-23T03:00:00Z", "terminal again"));
    r.scan();
    assert_eq!(r.texts(&root).iter().filter(|t| *t == "terminal again").count(), 1);
}

#[tokio::test]
async fn a_huge_first_import_keeps_the_newest_entries_and_says_so() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:00Z", "the very first"));
    for i in 0..450 {
        r.append("s1", &agent(&format!("2026-10-23T01:{:02}:{:02}Z", i / 60, i % 60), &format!("reply {i}")));
    }
    r.register("s1", "builder", DEAD, None);
    r.scan();
    let root = r.roots()[0].clone();
    assert_eq!(root.data["text"], "the very first");
    let thread = r.thread(&root.id);
    assert!(
        thread.iter().any(|e| e.kind == "notice" && e.data["text"].as_str().unwrap().contains("terminal transcript"))
    );
    let replies: Vec<&Entry> = thread.iter().filter(|e| e.kind == "agent").collect();
    assert!(replies.len() <= 400 && replies.last().unwrap().data["text"] == "reply 449");
}

#[tokio::test]
async fn a_thread_resumed_in_the_terminal_is_that_thread_not_a_new_one() {
    let r = Rig::new();
    // An app thread: its root, a reply, and the lane's session.
    let lane = Lane::main(&r.bot);
    let root = r
        .hub
        .add_entry(&lane, crate::store::EntryKind::User, 1, &json!({"text": "from the app", "task": true}))
        .unwrap();
    let tl = Lane::in_thread(&r.bot, &root.id);
    r.hub.add_entry(&tl, crate::store::EntryKind::Agent, 1, &json!({"text": "app reply", "final": true})).unwrap();
    r.hub.store.kv_set(&lane_key("session", &r.bot, &tl), "s9").unwrap();
    // The session's transcript holds the app's turn (old lines) and what the terminal added since.
    r.append("s9", &user("2020-01-01T00:00:00Z", "from the app"));
    r.append("s9", &agent("2020-01-01T00:00:01Z", "app reply"));
    r.append("s9", &user("2099-01-01T00:00:00Z", "typed in the terminal"));
    let me = r.sleeper();
    r.register("s9", "builder", me, Some("s9"));
    r.session_file(me, "s9", "idle");
    r.scan();
    assert_eq!(r.roots().len(), 1, "no new thread");
    let texts = r.texts(&root.id);
    assert_eq!(texts.iter().filter(|t| *t == "app reply").count(), 1, "the app's turn is not imported again");
    assert!(texts.contains(&"typed in the terminal".to_owned()));
    let s = r.summary(&root.id);
    assert_eq!(s["liveWriter"], "terminal");
    assert!(s["origin"].is_null(), "still an app thread");
    let changes: Vec<String> =
        r.thread(&root.id).iter().filter_map(|e| e.data["writerChange"].as_str().map(str::to_owned)).collect();
    assert_eq!(changes, ["openedInTerminal"]);
    // And the kernel can resolve it by thread or by session.
    for which in [root.id.as_str(), "s9"] {
        let v = sync::resolve(&r.hub, which).await;
        assert_eq!(
            (v["threadId"].as_str(), v["sessionId"].as_str(), v["botId"].as_str()),
            (Some(root.id.as_str()), Some("s9"), Some(r.bot.as_str()))
        );
        assert_eq!(v["hostBusy"], false);
    }
    assert_eq!(sync::resolve(&r.hub, "nope").await["error"], "unknown thread");
}

#[tokio::test]
async fn a_forked_resume_maps_to_the_thread_it_came_from() {
    let r = Rig::new();
    let root = r
        .hub
        .add_entry(&Lane::main(&r.bot), crate::store::EntryKind::User, 1, &json!({"text": "hi", "task": true}))
        .unwrap();
    let tl = Lane::in_thread(&r.bot, &root.id);
    r.hub.store.kv_set(&lane_key("session", &r.bot, &tl), "old").unwrap();
    r.append("new", &user("2020-01-01T00:00:00Z", "hi"));
    r.append("new", &agent("2099-01-01T00:00:00Z", "after the fork"));
    r.register("new", "builder", DEAD, Some("old"));
    r.scan();
    assert_eq!(r.roots().len(), 1);
    assert!(r.texts(&root.id).contains(&"after the fork".to_owned()));
    assert_eq!(
        r.hub.store.kv_get(&lane_key("session", &r.bot, &tl)).as_deref(),
        Some("new"),
        "the thread follows the fork"
    );
}

#[tokio::test]
async fn nothing_is_sent_while_the_terminal_lives() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    let e = sync::before_send(&r.hub, &r.bot, &root).await.unwrap_err();
    assert_eq!(e.downcast_ref::<HeldByWriter>().unwrap().code, "liveInTerminal");
    assert!(e.to_string().starts_with("liveInTerminal"));
    // An ordinary thread is not held.
    let other = r
        .hub
        .add_entry(&Lane::main(&r.bot), crate::store::EntryKind::User, 2, &json!({"text": "x", "task": true}))
        .unwrap();
    sync::before_send(&r.hub, &r.bot, &other.id).await.unwrap();
    // Closed: the terminal's last lines are caught up and the send goes on.
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    r.append("s1", &agent("2026-10-23T01:00:09Z", "last words"));
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
    assert!(r.texts(&root).contains(&"last words".to_owned()));
}

#[tokio::test]
async fn take_over_is_refused_while_busy_and_a_no_op_once_closed() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "busy");
    r.scan();
    let root = r.roots()[0].id.clone();
    let e = sync::take_over(&r.hub, &r.bot, &root).await.unwrap_err();
    assert!(e.to_string().starts_with("terminalBusy"), "{e:#}");
    assert!(!registry::dir(&r.dir.join("state")).join("s1.taken").exists(), "a busy terminal is left alone");
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    let v = sync::take_over(&r.hub, &r.bot, &root).await.unwrap();
    assert_eq!(v["tookOver"], false);
    assert!(sync::take_over(&r.hub, &r.bot, "missing").await.is_err());
}

#[tokio::test]
async fn take_over_ends_an_idle_terminal_and_the_host_writes_next() {
    let r = Rig::new();
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    let pid = child.id();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", pid, None);
    r.session_file(pid, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    assert_eq!(r.summary(&root)["liveWriter"], "terminal");
    r.append("s1", &agent("2026-10-23T01:00:05Z", "its last line"));
    // The session file stays after the process ends, as a crashed terminal's would: the pid decides.
    let reaper = std::thread::spawn(move || child.wait().unwrap());
    let v = sync::take_over(&r.hub, &r.bot, &root).await.unwrap();
    reaper.join().unwrap();
    assert_eq!(v["tookOver"], true);
    assert_eq!(v["liveWriter"], Value::Null);
    assert!(registry::dir(&r.dir.join("state")).join("s1.taken").exists());
    assert!(r.texts(&root).contains(&"its last line".to_owned()), "its last lines are imported first");
    let changes: Vec<String> =
        r.thread(&root).iter().filter_map(|e| e.data["writerChange"].as_str().map(str::to_owned)).collect();
    assert_eq!(changes, ["startedInTerminal", "tookOver"]);
    r.scan();
    assert_eq!(r.summary(&root)["liveWriter"], Value::Null);
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
}

#[test]
fn unknown_writer_values_pass_through_the_summary() {
    let mut summary = json!({"count": 1});
    let old = json!({"origin": "discord", "liveWriter": "bot", "originLabel": "Sam's server", "originBot": {"provider": "x", "extra": 1}});
    sync::carry_over(&old, &mut summary);
    assert_eq!(summary["origin"], "discord");
    assert_eq!(summary["originBot"]["extra"], 1);
    let _ = (Terminal::default(), Path::new(""));
}

impl Drop for Rig {
    fn drop(&mut self) {
        for child in self.children.borrow_mut().iter_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[tokio::test]
async fn a_pid_started_at_another_time_is_not_the_session() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "hi"));
    r.register("s1", "builder", me, None);
    let reg = registry::read_all(&r.dir.join("state")).pop().unwrap();
    let sessions = Some(r.dir.join("sessions"));
    let write = |proc_start: &str| {
        let v = json!({"pid": me, "sessionId": "s1", "status": "idle", "procStart": proc_start});
        std::fs::write(r.dir.join("sessions").join(format!("{me}.json")), v.to_string()).unwrap();
    };
    let actual = registry::proc_start(me).expect("ps knows the process");
    write(&actual);
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Idle);
    write("Mon Jan  1 00:00:00 2001");
    assert_eq!(registry::liveness(&reg, sessions.as_deref()), Live::Closed, "a reused pid started at another time");
}

/// Claude Code writes its session file's `procStart` in UTC; `ps` prints the Mac's local time. Away from UTC the two
/// differ by hours, and every live terminal read as closed. (A host running in UTC, as CI does, passes either way.)
#[tokio::test]
async fn a_session_file_with_the_start_in_utc_is_live() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "hi"));
    r.register("s1", "builder", me, None);
    let reg = registry::read_all(&r.dir.join("state")).pop().unwrap();
    let out = std::process::Command::new("ps")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p", &me.to_string()])
        .output()
        .unwrap();
    let utc = String::from_utf8(out.stdout).unwrap().split_whitespace().collect::<Vec<_>>().join(" ");
    let v = json!({"pid": me, "sessionId": "s1", "status": "idle", "procStart": utc});
    std::fs::write(r.dir.join("sessions").join(format!("{me}.json")), v.to_string()).unwrap();
    assert_eq!(registry::liveness(&reg, Some(&r.dir.join("sessions"))), Live::Idle);
}

/// The owner's Mac runs in Chinese (zh-Hant): `ps` there prints `二 10月/27 22:19:44 2026`, which never equals the
/// C-locale start Claude and the kernel write, and every live one-shot read as closed. The start is read in the C
/// locale whatever the caller's is, local time and UTC alike.
#[test]
fn the_start_time_is_read_in_the_c_locale_under_any_caller_locale() {
    let me = std::process::id();
    let zh = || {
        let mut ps = std::process::Command::new("ps");
        ps.env("LANG", "zh_TW.UTF-8").env("LC_ALL", "zh_TW.UTF-8").env("LC_TIME", "zh_TW.UTF-8");
        ps
    };
    for utc in [false, true] {
        let start = registry::lstart_via(zh(), me, utc).expect("ps knows the process");
        assert!(
            chrono::NaiveDateTime::parse_from_str(&start, "%a %b %d %H:%M:%S %Y").is_ok(),
            "C-locale lstart, got {start:?}"
        );
    }
    assert_eq!(registry::lstart_via(zh(), me, false), registry::proc_start(me));
}

#[test]
fn pids_that_cannot_be_a_terminal_are_refused() {
    let base = |pid: u32| {
        json!({
            "v": 1, "sessionId": "abc-1", "agent": "builder", "cwd": "/w", "transcriptPath": "/p/abc-1.jsonl",
            "pid": pid, "startedAt": "2026-10-23T01:00:00Z",
        })
        .to_string()
    };
    assert!(registry::parse(&base(4242)).is_some());
    assert!(registry::parse(&base(0)).is_none());
    assert!(registry::parse(&base(1)).is_none());
    assert!(registry::parse(&base(std::process::id())).is_none(), "never the host itself");
}

#[tokio::test]
async fn racing_passes_import_once_and_say_a_close_once() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    for i in 0..5 {
        r.append("s1", &agent(&format!("2026-10-23T02:00:0{i}Z"), &format!("line {i}")));
    }
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let hub = r.hub.clone();
            std::thread::spawn(move || sync::scan(&hub))
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    for i in 0..5 {
        assert_eq!(r.texts(&root).iter().filter(|t| **t == format!("line {i}")).count(), 1, "line {i}");
    }
    let changes: Vec<String> =
        r.thread(&root).iter().filter_map(|e| e.data["writerChange"].as_str().map(str::to_owned)).collect();
    assert_eq!(changes, ["startedInTerminal", "terminalClosed"]);
}

#[tokio::test]
async fn a_replaced_or_shortened_transcript_does_not_repeat_what_was_imported() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "kept once"));
    r.register("s1", "builder", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    // The file is written again from the start (shorter, new inode) with a new line at the end.
    std::fs::remove_file(r.transcript("s1")).unwrap();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "kept once"));
    r.append("s1", &agent("2099-01-01T00:00:00Z", "new after"));
    r.scan();
    assert_eq!(r.texts(&root).iter().filter(|t| *t == "kept once").count(), 1);
    assert_eq!(r.texts(&root).iter().filter(|t| *t == "new after").count(), 1);
}

#[tokio::test]
async fn a_terminal_that_wrote_makes_the_actor_load_the_session_again_once() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    let stale = format!("terminal.stale.{}/{root}", r.bot);
    assert_eq!(r.hub.store.kv_get(&stale).as_deref(), Some("1"), "live: the actor's copy of the session is stale");
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
    assert!(r.hub.store.kv_get(&stale).is_none(), "the send told the actor to forget it, once");
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
    assert!(r.hub.store.kv_get(&stale).is_none(), "a send with nothing new in the terminal loads nothing again");
}

// --- the writer claim and the checks that read it ---------------------------------------------------------------------

use super::claim::{self, Kind};

impl Rig {
    fn state(&self) -> PathBuf {
        self.dir.join("state")
    }

    /// An app thread (root + a reply) whose lane runs session `sid`, with no terminal known to the host.
    fn app_thread(&self, sid: &str) -> String {
        let root = self
            .hub
            .add_entry(
                &Lane::main(&self.bot),
                crate::store::EntryKind::User,
                1,
                &json!({"text": "from the app", "task": true}),
            )
            .unwrap();
        let tl = Lane::in_thread(&self.bot, &root.id);
        self.hub
            .add_entry(&tl, crate::store::EntryKind::Agent, 1, &json!({"text": "app reply", "final": true}))
            .unwrap();
        self.hub.store.kv_set(&lane_key("session", &self.bot, &tl), sid).unwrap();
        root.id
    }

    fn forgets(&self) -> usize {
        use crate::LockExt as _;
        self.hub.terminal.forgets.locked().len()
    }
}

#[tokio::test]
async fn a_second_claim_on_a_session_is_refused_and_the_first_is_kept() {
    let r = Rig::new();
    let (a, b) = (r.sleeper(), r.sleeper());
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, a, false).unwrap().is_ok());
    let held = claim::take(&r.state(), "s1", Kind::Bot, b, false).unwrap().unwrap_err();
    assert_eq!((held.0.pid, held.0.kind), (a, Kind::Terminal));
    assert_eq!(claim::holder(&r.state(), "s1").unwrap().pid, a);
    // Only the claimer gives it back.
    claim::release(&r.state(), "s1", b, false);
    assert!(claim::holder(&r.state(), "s1").is_some());
    claim::release(&r.state(), "s1", a, false);
    assert!(claim::holder(&r.state(), "s1").is_none());
    assert!(claim::take(&r.state(), "s1", Kind::Bot, b, false).unwrap().is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(claim::path(&r.state(), "s1", false)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn a_claim_of_a_dead_or_reused_pid_is_taken_over() {
    let r = Rig::new();
    let me = r.sleeper();
    let path = claim::path(&r.state(), "s1", false);
    std::fs::write(&path, json!({"pid": DEAD, "procStart": "x", "at": "t"}).to_string()).unwrap();
    assert!(claim::holder(&r.state(), "s1").is_none(), "a dead claimer holds nothing");
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, me, false).unwrap().is_ok());
    assert_eq!(claim::read(&path).unwrap().pid, me);
    // A live pid that started at another time is a reused pid.
    let other = r.sleeper();
    std::fs::write(&path, json!({"pid": other, "procStart": "Mon Jan  1 00:00:00 2001", "at": "t"}).to_string())
        .unwrap();
    assert!(claim::holder(&r.state(), "s1").is_none());
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, me, false).unwrap().is_ok());
    // No temp files are left behind (the lock file stays, empty).
    let left: Vec<String> = std::fs::read_dir(registry::dir(&r.state()))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| Path::new(n).extension().is_none_or(|x| x != "lock"))
        .collect();
    assert_eq!(left, ["s1.claim"]);
}

#[tokio::test]
async fn racing_takers_of_a_stale_claim_leave_exactly_one_owner() {
    let r = Rig::new();
    let path = claim::path(&r.state(), "s1", false);
    std::fs::write(&path, json!({"pid": DEAD, "procStart": "x", "at": "t"}).to_string()).unwrap();
    let pids: Vec<u32> = (0..6).map(|_| r.sleeper()).collect();
    let state = r.state();
    let wins: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = pids
            .iter()
            .map(|&pid| {
                let state = state.clone();
                s.spawn(move || claim::take(&state, "s1", Kind::Terminal, pid, false).unwrap().is_ok())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(wins.iter().filter(|w| **w).count(), 1, "{wins:?}");
    let owner = claim::read(&path).unwrap().pid;
    assert_eq!(pids.iter().zip(&wins).find(|(_, w)| **w).map(|(p, _)| *p), Some(owner));
}

#[cfg(unix)]
#[tokio::test]
async fn a_taker_waiting_on_a_claim_lock_the_kernel_prunes_takes_the_new_lock_not_the_removed_one() {
    use std::os::fd::AsRawFd as _;
    let flock = |p: &Path| {
        let f = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(p).unwrap();
        // SAFETY: the descriptor is open for the life of `f`.
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
        f
    };
    let r = Rig::new();
    let (state, live) = (r.state(), r.sleeper());
    let path = claim::path(&state, "s1", false);
    let lock = path.with_file_name("s1.claim.lock");
    std::fs::write(&path, json!({"pid": DEAD, "procStart": "x", "at": "t"}).to_string()).unwrap();
    let pruner = flock(&lock);
    let taker = std::thread::spawn({
        let state = state.clone();
        move || claim::take(&state, "s1", Kind::Terminal, std::process::id(), false).unwrap()
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    // What kith-sessions' prune_stale does under the lock: the stale claim and its lock file go. A kernel taker then makes
    // the lock anew and holds it while it writes its claim.
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_file(&lock).unwrap();
    let kernel = flock(&lock);
    drop(pruner);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!taker.is_finished(), "the waiting taker went ahead under the removed lock");
    std::fs::write(&path, json!({"pid": live, "procStart": null, "at": "t"}).to_string()).unwrap();
    drop(kernel);
    assert!(taker.join().unwrap().is_err(), "the kernel taker's live claim holds");
    assert_eq!(claim::read(&path).map(|c| c.pid), Some(live));
}

#[tokio::test]
async fn a_host_turns_own_claim_does_not_stop_the_hosts_sends() {
    let r = Rig::new();
    let me = r.sleeper();
    assert!(claim::take(&r.state(), "s1", Kind::App, me, false).unwrap().is_ok());
    assert!(claim::holder(&r.state(), "s1").is_none(), "only keeps a terminal out");
    claim::release(&r.state(), "s1", me, false);
    assert!(claim::take(&r.state(), "s1", Kind::App, me, true).unwrap().is_ok());
    assert!(claim::holder(&r.state(), "s1").is_some(), "a take-over's claim does");
}

#[tokio::test]
async fn resolve_counts_a_queued_send_as_busy() {
    let r = Rig::new();
    let root = r.app_thread("s9");
    assert_eq!(sync::resolve(&r.hub, &root).await["hostBusy"], false);
    let tl = Lane::in_thread(&r.bot, &root);
    let queued = r
        .hub
        .add_entry(&tl, crate::store::EntryKind::User, 2, &json!({"text": "hi", "status": "queued", "attachments": []}))
        .unwrap();
    assert_eq!(sync::resolve(&r.hub, &root).await["hostBusy"], true, "queued, not running yet");
    let mut sent = queued.data.clone();
    sent["status"] = "cancelled".into();
    r.hub.set_entry(&queued.id, &sent);
    assert_eq!(sync::resolve(&r.hub, &root).await["hostBusy"], false);
    assert_eq!(sync::resolve(&r.hub, &root).await["claims"], 1, "the kernel tells a host with claims by this");
}

#[tokio::test]
async fn a_take_over_under_way_makes_the_host_busy_for_a_resume() {
    let r = Rig::new();
    let root = r.app_thread("s9");
    r.hub.terminal.take_for_test(&format!("{}/{root}", r.bot));
    assert_eq!(sync::resolve(&r.hub, &root).await["hostBusy"], true);
}

#[tokio::test]
async fn a_send_is_refused_for_a_live_registry_or_claim_the_host_has_not_scanned_yet() {
    let r = Rig::new();
    let root = r.app_thread("s9");
    // The host knows no terminal (no scan): the registry file of the lane's session is read from disk.
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
    let me = r.sleeper();
    r.append("s9", &user("2026-10-23T01:00:01Z", "x"));
    r.register("s9", "builder", me, Some("s9"));
    r.session_file(me, "s9", "idle");
    let e = sync::before_send(&r.hub, &r.bot, &root).await.unwrap_err();
    assert_eq!(e.downcast_ref::<HeldByWriter>().unwrap().code, "liveInTerminal");
    // The terminal closed; only a claim (a resume under way, no registry yet) is left.
    std::fs::remove_file(r.dir.join("sessions").join(format!("{me}.json"))).unwrap();
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
    assert!(claim::take(&r.state(), "s9", Kind::Terminal, me, false).unwrap().is_ok());
    let e = sync::before_send(&r.hub, &r.bot, &root).await.unwrap_err();
    assert_eq!(e.downcast_ref::<HeldByWriter>().unwrap().code, "liveInTerminal");
    claim::release(&r.state(), "s9", me, false);
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
}

#[tokio::test]
async fn a_queued_turn_does_not_start_while_a_claim_holds_the_thread() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let r = Rig::new();
    let root = r.app_thread("s9");
    let lane = Lane::in_thread(&r.bot, &root);
    let me = r.sleeper();
    assert!(claim::take(&r.state(), "s9", Kind::Terminal, me, false).unwrap().is_ok());
    let began = Arc::new(AtomicBool::new(false));
    let mark = |flag: &Arc<AtomicBool>| {
        let flag = flag.clone();
        move |_: &Hub| flag.store(true, SeqCst)
    };
    let refused = sync::start_gate(&r.hub, &lane, mark(&began)).await.unwrap_err();
    assert_eq!(refused.code, "liveInTerminal");
    assert!(!began.load(SeqCst), "the turn is not marked running");
    claim::release(&r.state(), "s9", me, false);
    sync::start_gate(&r.hub, &lane, mark(&began)).await.unwrap();
    assert!(began.load(SeqCst));
    // A turn outside any thread is not looked at.
    let main = Arc::new(AtomicBool::new(false));
    sync::start_gate(&r.hub, &Lane::main(&r.bot), mark(&main)).await.unwrap();
    assert!(main.load(SeqCst));
}

#[tokio::test]
async fn lines_a_closed_terminal_wrote_make_the_actor_forget_the_session() {
    let r = Rig::new();
    let root = r.app_thread("s9");
    r.append("s9", &user("2020-01-01T00:00:00Z", "from the app"));
    r.append("s9", &user("2099-01-01T00:00:00Z", "typed in the terminal, then closed"));
    r.register("s9", "builder", DEAD, Some("s9"));
    let thread = sync::Thread { bot: r.bot.clone(), root: root.clone() };
    let reg = registry::read_one(&r.state(), "s9").unwrap();
    // Never seen live (no scan saw it): the import alone must make the actor forget.
    let before = r.forgets();
    sync::import(&r.hub, &thread, &reg).unwrap();
    assert!(r.forgets() > before);
    let again = r.forgets();
    sync::import(&r.hub, &thread, &reg).unwrap();
    assert_eq!(r.forgets(), again, "nothing new: nothing forgotten");
}

#[tokio::test]
async fn a_second_take_over_while_one_holds_its_claim_does_nothing() {
    let r = Rig::new();
    let me = r.sleeper();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", me, None);
    r.session_file(me, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    assert!(claim::take(&r.state(), "s1", Kind::App, me, true).unwrap().is_ok());
    let v = sync::take_over(&r.hub, &r.bot, &root).await.unwrap();
    assert_eq!(v["tookOver"], false);
    assert!(!registry::dir(&r.state()).join("s1.taken").exists(), "the terminal was not signalled");
    assert!(registry::pid_runs(me));
}

#[tokio::test]
async fn a_bot_holding_the_thread_refuses_an_app_send_as_another_writer() {
    let r = Rig::new();
    let root = r.app_thread("s5");
    let me = r.sleeper();
    assert!(claim::take(&r.state(), "s5", Kind::Bot, me, false).unwrap().is_ok());
    let e = sync::before_send(&r.hub, &r.bot, &root).await.unwrap_err();
    assert_eq!(e.downcast_ref::<HeldByWriter>().unwrap().code, "heldByOtherWriter");
    assert!(e.to_string().starts_with("heldByOtherWriter"));
    // The bot's turn is over: the claim is released, the thread is free.
    claim::release(&r.state(), "s5", me, false);
    sync::before_send(&r.hub, &r.bot, &root).await.unwrap();
}

#[tokio::test]
async fn a_bots_own_claim_does_not_stop_the_next_turn_of_its_lane_but_a_terminal_does() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let r = Rig::new();
    let root = r.app_thread("s5");
    let lane = Lane::in_thread(&r.bot, &root);
    let (bot, term) = (r.sleeper(), r.sleeper());
    let began = Arc::new(AtomicBool::new(false));
    let mark = |flag: &Arc<AtomicBool>| {
        let flag = flag.clone();
        move |_: &Hub| flag.store(true, SeqCst)
    };
    // The lane's queue orders the bot's turns with the app's; the claim only keeps other processes out.
    assert!(claim::take(&r.state(), "s5", Kind::Bot, bot, false).unwrap().is_ok());
    sync::start_gate(&r.hub, &lane, mark(&began)).await.unwrap();
    assert!(began.load(SeqCst));
    claim::release(&r.state(), "s5", bot, false);
    assert!(claim::take(&r.state(), "s5", Kind::Terminal, term, false).unwrap().is_ok());
    began.store(false, SeqCst);
    sync::start_gate(&r.hub, &lane, mark(&began)).await.unwrap_err();
    assert!(!began.load(SeqCst));
}

#[tokio::test]
async fn a_bot_ask_is_refused_while_another_writer_holds_and_waits_behind_its_own_turn() {
    let r = Rig::new();
    let root = r.app_thread("s6");
    let (bot, term, app) = (r.sleeper(), r.sleeper(), r.sleeper());
    sync::bot_gate(&r.hub, &r.bot, &root).await.unwrap();
    // A terminal holds: refused, not queued.
    assert!(claim::take(&r.state(), "s6", Kind::Terminal, term, false).unwrap().is_ok());
    assert_eq!(sync::bot_gate(&r.hub, &r.bot, &root).await.unwrap_err().code, "heldByOtherWriter");
    claim::release(&r.state(), "s6", term, false);
    // A take-over of the app holds too.
    assert!(claim::take(&r.state(), "s6", Kind::App, app, true).unwrap().is_ok());
    assert_eq!(sync::bot_gate(&r.hub, &r.bot, &root).await.unwrap_err().code, "heldByOtherWriter");
    claim::release(&r.state(), "s6", app, true);
    // Its own running turn: the ask goes on to the queue.
    assert!(claim::take(&r.state(), "s6", Kind::Bot, bot, false).unwrap().is_ok());
    sync::bot_gate(&r.hub, &r.bot, &root).await.unwrap();
}

fn agent_finals(r: &Rig, root: &str) -> Vec<(String, bool)> {
    r.thread(root)
        .iter()
        .filter(|e| e.kind == "agent")
        .map(|e| (e.data["text"].as_str().unwrap_or("").to_owned(), e.data["final"] == true))
        .collect()
}

#[tokio::test]
async fn a_reply_stays_final_after_a_tool_call_and_only_a_later_reply_moves_it() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:01Z", "go"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "first"));
    r.append("s1", &tool_use("2026-10-23T01:00:03Z", "t1", "ls"));
    r.register("s1", "builder", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    assert_eq!(agent_finals(&r, &root), [("first".to_owned(), true)], "a tool call after the reply keeps it final");
    r.append("s1", &agent("2026-10-23T01:00:04Z", "second"));
    r.scan();
    r.scan();
    assert_eq!(agent_finals(&r, &root), [("first".to_owned(), false), ("second".to_owned(), true)]);
    // A new turn leaves the last turn's reply alone.
    r.append("s1", &user("2026-10-23T01:00:05Z", "more"));
    r.append("s1", &tool_use("2026-10-23T01:00:06Z", "t2", "pwd"));
    r.scan();
    assert_eq!(agent_finals(&r, &root), [("first".to_owned(), false), ("second".to_owned(), true)]);
}

#[tokio::test]
async fn the_repair_marks_each_turns_last_reply_final_once() {
    let r = Rig::new();
    r.append("s1", &user("2026-10-23T01:00:01Z", "go"));
    r.append("s1", &agent("2026-10-23T01:00:02Z", "a1"));
    r.append("s1", &user("2026-10-23T01:00:03Z", "again"));
    r.append("s1", &agent("2026-10-23T01:00:04Z", "b1"));
    r.append("s1", &agent("2026-10-23T01:00:05Z", "b2"));
    r.register("s1", "builder", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    // Put the data back the way the old importer left it.
    for e in r.thread(&root).iter().filter(|e| e.kind == "agent") {
        let mut data = e.data.clone();
        data["final"] = false.into();
        r.hub.set_entry(&e.id, &data);
    }
    sync::backfill_finals(&r.hub);
    let fin = |r: &Rig| agent_finals(r, &root).into_iter().map(|(_, f)| f).collect::<Vec<_>>();
    assert_eq!(fin(&r), [true, false, true]);
    let rev = r.hub.store.current_rev();
    sync::backfill_finals(&r.hub);
    assert_eq!(r.hub.store.current_rev(), rev, "a second run changes nothing");
}

#[tokio::test]
async fn a_main_lane_session_resolves_with_its_busy_state() {
    let r = Rig::new();
    r.hub.store.kv_set(&lane_key("session", &r.bot, &Lane::main(&r.bot)), "m1").unwrap();
    let v = sync::resolve(&r.hub, "m1").await;
    assert_eq!(v["sessionId"], "m1", "the kernel claims it like a thread's, not as a raw session id");
    assert_eq!(v["agent"], "builder");
    assert_eq!(v["hostBusy"], false);
    assert_eq!(v["claims"], 1);
}

#[tokio::test]
async fn a_dropped_take_over_does_not_jam_the_thread() {
    let r = Rig::new();
    let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
    let pid = child.id();
    r.append("s1", &user("2026-10-23T01:00:01Z", "first"));
    r.register("s1", "builder", pid, None);
    r.session_file(pid, "s1", "idle");
    r.scan();
    let root = r.roots()[0].id.clone();
    let (hub, bot, r2) = (r.hub.clone(), r.bot.clone(), root.clone());
    let task = tokio::spawn(async move { sync::take_over(&hub, &bot, &r2).await });
    tokio::task::yield_now().await;
    task.abort();
    let _ = task.await;
    assert!(!r.hub.terminal.taking.locked().contains(&format!("{}/{root}", r.bot)), "the key goes with the future");
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn releasing_a_claim_leaves_a_foreign_claim_alone() {
    let r = Rig::new();
    let (a, b) = (r.sleeper(), r.sleeper());
    assert!(claim::take(&r.state(), "s1", Kind::Terminal, b, false).unwrap().is_ok());
    claim::release(&r.state(), "s1", a, false);
    assert_eq!(claim::holder(&r.state(), "s1").map(|c| c.pid), Some(b));
    claim::release(&r.state(), "s1", b, false);
    assert!(claim::holder(&r.state(), "s1").is_none());
}

#[tokio::test]
async fn a_oneshot_claim_written_by_the_kernel_is_parsed_and_holds_the_session() {
    let r = Rig::new();
    let pid = r.sleeper();
    // The shape kernel/bin/kith-sessions writes for a `claude -p --resume` one-shot. An unparsed claim would be ignored.
    let p = claim::path(&r.state(), "s1", false);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let start = registry::proc_start(pid).expect("ps knows the process");
    std::fs::write(
        &p,
        json!({"pid": pid, "procStart": start, "at": "2026-10-24T00:00:00+00:00", "kind": "oneshot"}).to_string(),
    )
    .unwrap();
    assert_eq!(claim::read(&p).expect("the host parses a oneshot claim").kind, Kind::Oneshot);
    let held = claim::holder(&r.state(), "s1").expect("a live one-shot holds the session for a host turn");
    assert_eq!((held.pid, held.kind), (pid, Kind::Oneshot));
    assert!(claim::take(&r.state(), "s1", Kind::App, std::process::id(), false).unwrap().is_err());
}

// --- the revoke sweep: claims name the person, the sweep ends their holders -----------------------------------------

/// A process with a child of its own (`sh` waiting on a `sleep`), so the sweep's reach to children can be seen. The child's pid
/// is the first line `sh` prints.
fn shell_with_child() -> (std::process::Child, u32) {
    use std::io::BufRead as _;
    let mut sh = std::process::Command::new("sh")
        .args(["-c", "sleep 300 & echo $!; wait"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(sh.stdout.take().unwrap()).read_line(&mut line).unwrap();
    (sh, line.trim().parse().unwrap())
}

fn gone(pid: u32) -> bool {
    for _ in 0..40 {
        if !registry::pid_runs(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

const GRACE: std::time::Duration = std::time::Duration::from_millis(300);

#[tokio::test]
async fn the_sweep_ends_a_left_over_guest_holder_and_its_child_and_keeps_the_owners() {
    let r = Rig::new();
    let (mut guest, guest_child) = shell_with_child();
    let owner = r.sleeper();
    let other_guest = r.sleeper();
    let gpid = guest.id();
    claim::take_as(&r.state(), "g1", Kind::Oneshot, gpid, false, Some("bot-sam"), None).unwrap().unwrap();
    claim::take(&r.state(), "o1", Kind::Terminal, owner, false).unwrap().unwrap();
    claim::take_as(&r.state(), "x1", Kind::Oneshot, other_guest, false, Some("ann"), None).unwrap().unwrap();
    let ended = claim::sweep(&r.state(), &["bot-sam".into(), "sam".into()], &|_| false, GRACE);
    assert_eq!(ended, [gpid]);
    guest.wait().unwrap();
    assert!(gone(guest_child), "what the holder started goes with it");
    assert!(registry::pid_runs(owner), "a claim without a person is the owner's and is never touched");
    assert!(registry::pid_runs(other_guest), "another person's holder is left alone");
    assert!(claim::read(&claim::path(&r.state(), "g1", false)).is_none(), "the ended holder's claim is removed");
    assert!(claim::read(&claim::path(&r.state(), "o1", false)).is_some());
    // Again: nothing left to end.
    assert_eq!(claim::sweep(&r.state(), &["bot-sam".into()], &|_| false, GRACE).len(), 0);
}

#[tokio::test]
async fn the_sweep_leaves_a_reused_pid_alone() {
    let r = Rig::new();
    let stranger = r.sleeper();
    // A live pid that started at another time than the claim says: not the process that took the claim.
    let path = claim::path(&r.state(), "g2", false);
    std::fs::write(
        &path,
        json!({"pid": stranger, "procStart": "Mon Jan  1 00:00:00 2001", "at": "t", "kind": "oneshot", "person": "sam"})
            .to_string(),
    )
    .unwrap();
    assert_eq!(claim::sweep(&r.state(), &["sam".into()], &|_| false, GRACE).len(), 0);
    assert!(registry::pid_runs(stranger));
    // A dead pid is stale: nothing to kill either.
    std::fs::write(&path, json!({"pid": DEAD, "at": "t", "kind": "oneshot", "person": "sam"}).to_string()).unwrap();
    assert_eq!(claim::sweep(&r.state(), &["sam".into()], &|_| false, GRACE).len(), 0);
}

#[tokio::test]
async fn claim_files_from_before_the_person_field_parse_as_the_owners() {
    let r = Rig::new();
    let me = r.sleeper();
    let path = claim::path(&r.state(), "old1", false);
    for body in [
        json!({"pid": me, "at": "t"}),
        json!({"pid": me, "at": "t", "kind": "oneshot", "takeover": false}),
        json!({"pid": me, "at": "t", "kind": "terminal", "person": null}),
    ] {
        std::fs::write(&path, body.to_string()).unwrap();
        let c = claim::read(&path).expect("parses");
        assert_eq!(c.person, None);
    }
    std::fs::write(&path, json!({"pid": me, "kind": "oneshot"}).to_string()).unwrap();
    assert_eq!(claim::sweep(&r.state(), &["sam".into()], &|_| false, GRACE).len(), 0);
    assert!(registry::pid_runs(me), "an old claim is the owner's: kept");
    // The owner's claim is written without the field.
    claim::release(&r.state(), "old1", me, false);
    claim::take(&r.state(), "old1", Kind::Terminal, me, false).unwrap().unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("person"));
}

#[tokio::test]
async fn a_persons_warm_process_stays_findable_between_turns() {
    let r = Rig::new();
    let (mut warm, _child) = shell_with_child();
    // The turn claim is given back at the end of the turn; the process claim is not.
    claim::take_proc(&r.state(), warm.id(), "sam", None).unwrap();
    claim::take_as(&r.state(), "t1", Kind::App, warm.id(), false, Some("sam"), None).unwrap().unwrap();
    claim::release(&r.state(), "t1", warm.id(), false);
    assert!(claim::holder(&r.state(), "proc-1").is_none(), "it keeps no writer out");
    let ended = claim::sweep(&r.state(), &["sam".into()], &|_| false, GRACE);
    assert_eq!(ended, [warm.id()]);
    warm.wait().unwrap();
}

#[tokio::test]
async fn a_narrowing_sweep_ends_only_the_holders_on_agents_the_person_lost() {
    let r = Rig::new();
    let (mut lost, _c1) = shell_with_child();
    let (mut kept, _c2) = shell_with_child();
    claim::take_as(&r.state(), "n1", Kind::Oneshot, lost.id(), false, Some("sam"), Some("money")).unwrap().unwrap();
    claim::take_as(&r.state(), "n2", Kind::Oneshot, kept.id(), false, Some("sam"), Some("house")).unwrap().unwrap();
    assert_eq!(claim::holders_of(&r.state(), &["sam".into()]).len(), 2);
    let spare = |c: &claim::Claim| c.agent.as_deref() == Some("house");
    let ended = claim::sweep(&r.state(), &["sam".into()], &spare, GRACE);
    assert_eq!(ended, [lost.id()]);
    lost.wait().unwrap();
    assert!(kept.try_wait().unwrap().is_none(), "the agent they still hold keeps running");
    kept.kill().unwrap();
    kept.wait().unwrap();
}

// --- one-shots: `kith ask`, scheduled and helper runs show up as cards in the agent's room ---------------------

impl Rig {
    /// A registry file the kernel wrote for a one-shot (`kind` ask | schedule | helper) of the process `pid`.
    fn register_one_shot(&self, sid: &str, kind: &str, label: &str, pid: u32, resumed_from: Option<&str>) {
        let reg = json!({
            "v": 1, "sessionId": sid, "agent": "builder", "cwd": self.dir.to_string_lossy(),
            "transcriptPath": self.transcript(sid).to_string_lossy(), "pid": pid, "project": null,
            "source": "startup", "resumedFrom": resumed_from, "startedAt": chrono::Utc::now().to_rfc3339(),
            "kind": kind, "label": label, "procStart": registry::proc_start(pid),
        });
        std::fs::write(registry::dir(&self.dir.join("state")).join(format!("{sid}.json")), reg.to_string()).unwrap();
    }
}

#[tokio::test]
async fn an_ask_run_that_already_ended_is_a_card_labelled_with_who_asked() {
    let r = Rig::new();
    r.append("a1", &(user("2026-10-23T01:00:01Z", "is the build green?") + &agent("2026-10-23T01:00:02Z", "Yes.")));
    // The process exited before the host looked: the registry file and the transcript are all it has.
    r.register_one_shot("a1", "ask", "asked by architect", DEAD, None);
    r.scan();
    let roots = r.roots();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].data["text"], "is the build green?");
    let root = roots[0].id.clone();
    assert_eq!(r.texts(&root), ["Yes."], "no writer-change notices for a run nobody sat in");
    let s = r.summary(&root);
    assert_eq!((s["origin"].as_str(), s["originLabel"].as_str()), (Some("terminal"), Some("asked by architect")));
    assert_eq!((s["liveWriter"].clone(), s["liveBusy"].clone()), (Value::Null, json!(false)), "a finished ask is done");
    assert_eq!(s["unread"], 1, "the owner has not seen it");
    assert_eq!(r.thread(&root)[0].data["final"], true);
    r.scan();
    r.scan();
    assert_eq!(r.roots().len(), 1, "rescans (a host restart) add no second card");
    assert_eq!(r.texts(&root), ["Yes."]);
}

#[tokio::test]
async fn a_running_ask_is_working_until_its_process_ends() {
    let r = Rig::new();
    let pid = r.sleeper();
    r.append("a2", &user("2026-10-23T01:00:01Z", "dig into the logs"));
    // `claude -p` keeps no session file here: the process alone says it works.
    r.register_one_shot("a2", "schedule", "scheduled: nightly", pid, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    let s = r.summary(&root);
    assert_eq!((s["liveWriter"].as_str(), s["liveBusy"].clone()), (Some("terminal"), json!(true)));
    assert_eq!(s["originLabel"], "scheduled: nightly");
    // The run crashes: no final reply, yet the card ends.
    for mut c in r.children.borrow_mut().drain(..) {
        c.kill().unwrap();
        c.wait().unwrap();
    }
    r.scan();
    let s = r.summary(&root);
    assert_eq!((s["liveWriter"].clone(), s["liveBusy"].clone()), (Value::Null, json!(false)));
    assert!(r.thread(&root).iter().all(|e| e.kind != "notice"), "no 'terminal closed' for a one-shot");
}

#[tokio::test]
async fn a_pid_reused_after_a_crash_does_not_keep_a_one_shot_working() {
    let r = Rig::new();
    let pid = r.sleeper();
    r.append("a3", &user("2026-10-23T01:00:01Z", "hello"));
    r.register_one_shot("a3", "ask", "asked from a terminal", pid, None);
    let path = registry::dir(&r.dir.join("state")).join("a3.json");
    let mut reg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    reg["procStart"] = "Mon Jan  1 00:00:00 2001".into();
    std::fs::write(&path, reg.to_string()).unwrap();
    let parsed = registry::read_one(&r.dir.join("state"), "a3").unwrap();
    assert_eq!(registry::liveness(&parsed, Some(&r.dir.join("sessions"))), Live::Closed);
}

#[tokio::test]
async fn a_continued_ask_lands_in_the_thread_it_continues() {
    let r = Rig::new();
    r.append("c1", &(user("2026-10-23T01:00:01Z", "first question") + &agent("2026-10-23T01:00:02Z", "First answer.")));
    r.register_one_shot("c1", "ask", "asked by architect", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    // `kith ask -c`: a new session that resumes the first (claude copies the history, then adds the new turn).
    r.append(
        "c2",
        &(user("2026-10-23T01:00:01Z", "first question")
            + &agent("2026-10-23T01:00:02Z", "First answer.")
            + &user("2026-10-23T02:00:01Z", "and then?")
            + &agent("2026-10-23T02:00:02Z", "Second answer.")),
    );
    r.register_one_shot("c2", "ask", "asked by architect", DEAD, Some("c1"));
    r.scan();
    assert_eq!(r.roots().len(), 1, "no second card for a continued ask");
    let texts = r.texts(&root);
    assert!(texts.contains(&"Second answer.".to_owned()) && texts.contains(&"and then?".to_owned()), "{texts:?}");
    assert_eq!(texts.iter().filter(|t| *t == "First answer.").count(), 1, "history is not imported twice");
}

#[tokio::test]
async fn a_person_s_run_is_never_mirrored_even_with_a_kind() {
    let r = Rig::new();
    r.append("p1", &user("2026-10-23T01:00:01Z", "private"));
    r.register_one_shot("p1", "ask", "asked by architect", DEAD, None);
    let path = registry::dir(&r.dir.join("state")).join("p1.json");
    let mut reg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    reg["person"] = "anna".into();
    std::fs::write(&path, reg.to_string()).unwrap();
    r.scan();
    assert!(r.roots().is_empty());
    // And a file naming a kind the host doesn't know is skipped.
    r.register_one_shot("p2", "serve", "x", DEAD, None);
    r.append("p2", &user("2026-10-23T01:00:01Z", "maintenance"));
    r.scan();
    assert!(r.roots().is_empty());
}

#[tokio::test]
async fn a_mirrored_ask_card_reaches_the_owner_and_no_invited_person_or_bot_guest() {
    use crate::api::roles;
    let r = Rig::new();
    r.append(
        "g1",
        &(user("2026-10-23T01:00:01Z", "private question") + &agent("2026-10-23T01:00:02Z", "private answer")),
    );
    r.register_one_shot("g1", "ask", "asked by architect", DEAD, None);
    r.scan();
    let root = r.roots()[0].id.clone();
    let mut entries = vec![r.hub.store.entry(&root).unwrap()];
    entries.extend(r.thread(&root));
    for e in entries {
        let ev =
            json!({"type": "entry", "rev": 1, "entry": {"botId": r.bot, "id": e.id, "kind": e.kind, "data": e.data}});
        assert!(roles::event_for(&r.hub.store, None, ev.clone()).is_some(), "the owner sees the card");
        for person in ["p1", "p2"] {
            assert!(
                roles::event_for(&r.hub.store, Some(person), ev.clone()).is_none(),
                "{person} saw an owner-only run"
            );
        }
        assert!(roles::event_for_bot(&r.hub.store, "some-bot", ev).is_none(), "a bot guest saw an owner-only run");
    }
}
