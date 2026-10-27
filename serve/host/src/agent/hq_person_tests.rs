//! An invited person's run, started through the real kernel launcher (`kernel/bin/kith --launch-json`) in a temp
//! `KITH_HOME` / `KITH_STATE` / `KITH_AUTH`. The folders reach the child commands through `TEST_ENV`; nothing is set on the
//! process, which every other test shares.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::hq::{self, Refused};
use crate::channels::hq::TEST_ENV;
use crate::remote::account::ONE_AT_A_TIME;
use crate::runauth::{PeerInfo, RunGrant, RunIdentity, TokenScope, TokenTable};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

struct Kernel {
    dir: PathBuf,
    kith: PathBuf,
    auth: PathBuf,
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Kernel {
    async fn new() -> Self {
        let guard = ONE_AT_A_TIME.lock().await;
        let dir = std::env::temp_dir().join(format!("kith-runs-{}", uuid::Uuid::new_v4()));
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("agents/house")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        std::fs::create_dir_all(dir.join("cwd")).unwrap();
        std::fs::write(home.join("agents/house/agent.md"), "---\nname: house\ndescription: d\n---\n# house\n").unwrap();
        // An empty HOME: the cost ledger reads the user's Claude transcripts from it, which takes a minute on a real one.
        std::fs::create_dir_all(dir.join("user")).unwrap();
        // A stand-in `claude`: the kernel asks it for its version (a person's run needs 2.1.257 or newer) and its plugins.
        let fake = dir.join("fakebin");
        std::fs::create_dir_all(&fake).unwrap();
        let claude = fake.join("claude");
        std::fs::write(&claude, "#!/bin/sh\n[ \"$1\" = --version ] && echo \"2.1.309 (Claude Code)\"\n[ \"$1 $2\" = \"plugin list\" ] && echo \"[]\"\nexit 0\n").unwrap();
        std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(fake).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .unwrap();
        *TEST_ENV.lock().unwrap() = vec![
            ("PATH", PathBuf::from(path)),
            ("KITH_AUTH", dir.join("auth.json")),
            ("KITH_HOME", home),
            ("KITH_STATE", dir.join("state")),
            ("HOME", dir.join("user")),
        ];
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin");
        let k = Self { kith: bin.join("kith"), auth: bin.join("kith-auth"), dir, _guard: guard };
        k.auth(&["person", "kai", "owner"]);
        k.auth(&["person", "sam"]);
        k
    }

    fn command(&self, bin: &PathBuf) -> std::process::Command {
        let mut c = std::process::Command::new(bin);
        c.envs(TEST_ENV.lock().unwrap().iter().map(|(k, v)| (*k, v))).current_dir(self.dir.join("cwd"));
        c
    }

    fn auth(&self, args: &[&str]) {
        let out = self.command(&self.auth).args(args).output().unwrap();
        assert!(out.status.success(), "kith-auth {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    async fn launch(&self, person: Option<&str>) -> anyhow::Result<hq::Launch> {
        self.launch_with(person, None).await
    }

    async fn launch_with(
        &self,
        person: Option<&str>,
        access: Option<&(RunGrant, [(String, String); 2])>,
    ) -> anyhow::Result<hq::Launch> {
        hq::launch(
            &self.kith,
            "house",
            self.dir.join("cwd").to_str().unwrap(),
            None,
            None,
            person,
            "1:1:house:owner",
            &hq::Members::Unknown,
            access,
        )
        .await
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn a_members_run_is_theirs_and_the_owners_is_not() {
    let k = Kernel::new().await;
    k.auth(&["member", "house", "sam", "preset=member"]);

    let mine = k.launch(Some("sam")).await.unwrap();
    assert_eq!(mine.env.get("KITH_PERSON").map(String::as_str), Some("sam"), "the agent process runs as sam");
    assert_eq!(
        mine.permission_mode,
        Some(hq::PermissionMode::AcceptEdits),
        "the member preset's mode, not the template's"
    );
    assert!(mine.system_prompt.contains("memory/people/sam"), "sam's memory folder, not the owner's");
    assert!(mine.system_prompt.contains("files/people/sam") || mine.add_dirs.iter().any(|d| d.contains("people/sam")));

    // The cost line: the launch's tag and the agent's env both name the person (the cost hooks read KITH_PERSON).
    let raw = k
        .command(&k.kith)
        .args(kith_wire::kernel::command::launch("house", None, None))
        .env("KITH_PERSON", "sam")
        .output()
        .unwrap();
    let raw: Value = serde_json::from_slice(&raw.stdout).unwrap();
    assert_eq!(raw["cost_tag"]["person"], "sam");

    let owner = k.launch(None).await.unwrap();
    assert!(!owner.env.contains_key("KITH_PERSON"), "the owner's run has no person");
    assert!(!owner.system_prompt.contains("memory/people/"));
}

#[tokio::test]
async fn a_guest_gets_the_default_mode_and_never_a_free_shell() {
    let k = Kernel::new().await;
    k.auth(&["member", "house", "sam", "preset=guest"]);
    assert_eq!(k.launch(Some("sam")).await.unwrap().permission_mode, Some(hq::PermissionMode::Default));

    // Even a membership an owner marked trusted can't hand an invitee a free shell through the host.
    k.auth(&["member", "house", "sam", "mode=bypassPermissions", "--trusted"]);
    let l = k.launch(Some("sam")).await.unwrap();
    assert_eq!(l.permission_mode, Some(hq::PermissionMode::Default), "bypassPermissions is the owner's alone");
    assert_ne!(l.settings.as_ref().unwrap()["permissions"]["defaultMode"], "bypassPermissions");
}

#[tokio::test]
async fn the_kernel_refusing_reaches_the_person_and_never_runs_as_the_owner() {
    let k = Kernel::new().await;
    // Not a member of this agent.
    let e = k.launch(Some("sam")).await.unwrap_err();
    let refused = e.downcast_ref::<Refused>().unwrap_or_else(|| panic!("a refusal, not a crash: {e:#}"));
    assert!(refused.0.contains("sam"), "{}", refused.0);

    // A member over their budget: the kernel's message, without the owner's command line.
    k.auth(&["member", "house", "sam", "preset=member", "budget=0"]);
    let e = k.launch(Some("sam")).await.unwrap_err();
    let refused = e.downcast_ref::<Refused>().unwrap_or_else(|| panic!("a refusal: {e:#}"));
    assert!(refused.0.contains("this month") && refused.0.contains("ask the owner"), "{}", refused.0);
    assert!(!refused.0.contains("kith auth member"), "{}", refused.0);

    // Membership taken away: the next launch refuses; the owner's still works.
    k.auth(&["member", "house", "sam", "--rm"]);
    assert!(k.launch(Some("sam")).await.unwrap_err().is::<Refused>());
    k.launch(None).await.unwrap();
}

/// A run socket that answers `consumeAccess` with `grants`, once, and only for a token of `table` asked from the
/// process it was bound to or below it (the host's own check, with real process ancestry). Every method it was asked
/// is logged.
fn access_socket(table: Arc<TokenTable>, grants: Value) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
    // Short: a unix socket path has about a hundred bytes.
    let path = PathBuf::from(format!("/tmp/kf2-{}.sock", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        let mut grants = Some(grants);
        while let Ok((stream, _)) = listener.accept().await {
            let peer =
                PeerInfo { pid: stream.peer_cred().ok().and_then(|c| c.pid()).and_then(|p| u32::try_from(p).ok()) };
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            let known = table.resolve(req["token"].as_str().unwrap_or_default(), peer).is_some();
            seen.lock().unwrap().push(format!(
                "{}{}",
                req["method"].as_str().unwrap_or_default(),
                if known { "" } else { " (unknown)" }
            ));
            let out = match grants.take().filter(|_| known && req["method"] == "consumeAccess") {
                Some(g) => serde_json::json!({"ok": true, "grants": g}),
                None => serde_json::json!({"ok": false, "error": "not authorized"}),
            };
            stream.get_mut().write_all(format!("{out}\n").as_bytes()).await.unwrap();
        }
    });
    (path, log)
}

#[tokio::test]
async fn a_persons_launch_with_access_carries_what_their_conversation_was_given() {
    let k = Kernel::new().await;
    k.auth(&["member", "house", "sam", "preset=guest"]);
    let docs = std::fs::canonicalize(&k.dir).unwrap().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let docs = docs.to_string_lossy().into_owned();
    // The host's real check: the token resolves only for the kernel process the launch started, or below it.
    let table = Arc::new(TokenTable::new());
    let (sock, log) =
        access_socket(table.clone(), serde_json::json!({"read": [docs], "write": [], "domains": ["pypi.org"]}));
    let issue = || {
        let identity = RunIdentity {
            person: "sam".into(),
            agent: "house".into(),
            room: None,
            run_id: uuid::Uuid::new_v4().simple().to_string(),
            scope: TokenScope::Guest,
            session: "run:g:3:sam:acp:chat:".into(),
            project: None,
        };
        let grant = RunGrant::issue(&table, identity).unwrap();
        let env = grant.env(&sock);
        (grant, env)
    };

    let plain = k.launch(Some("sam")).await.unwrap();
    assert!(log.lock().unwrap().is_empty(), "no access token, nothing asked");
    assert!(!plain.settings.as_ref().unwrap()["sandbox"]["filesystem"]["allowRead"].to_string().contains(&docs));

    let given = k.launch_with(Some("sam"), Some(&issue())).await.unwrap();
    assert_eq!(*log.lock().unwrap(), ["consumeAccess"], "asked once, by the bound kernel process");
    let read = &given.settings.as_ref().unwrap()["sandbox"]["filesystem"]["allowRead"];
    assert!(read.as_array().unwrap().iter().any(|d| d == docs.as_str()), "{read}");
    assert!(given.settings.as_ref().unwrap()["permissions"]["additionalDirectories"].to_string().contains(&docs));
    assert_eq!(
        given.settings.as_ref().unwrap()["sandbox"]["network"]["allowedDomains"],
        serde_json::json!(["pypi.org"])
    );
    assert!(!given.env.contains_key(crate::runauth::TOKEN_ENV), "the launch's token never reaches the agent process");

    // The owner's launch is never asked for access, whatever it is handed.
    log.lock().unwrap().clear();
    let owner = k.launch_with(None, Some(&issue())).await.unwrap();
    assert!(log.lock().unwrap().is_empty(), "{:?}", log.lock().unwrap());
    assert!(!owner.settings.as_ref().unwrap().to_string().contains(&docs));
    let _ = std::fs::remove_file(&sock);
}
