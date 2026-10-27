//! An invited person's run, started through the real kernel launcher (`kernel/bin/kith --launch-json`) in a temp
//! `KITH_HOME` / `KITH_STATE` / `KITH_AUTH`. The folders reach the child commands through `TEST_ENV`; nothing is set on the
//! process, which every other test shares.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::hq::{self, Refused};
use crate::channels::hq::TEST_ENV;
use crate::remote::account::ONE_AT_A_TIME;
use serde_json::Value;
use std::path::PathBuf;

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
        hq::launch(
            &self.kith,
            "house",
            self.dir.join("cwd").to_str().unwrap(),
            None,
            None,
            person,
            "1:1:house:owner",
            &hq::Members::Unknown,
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
