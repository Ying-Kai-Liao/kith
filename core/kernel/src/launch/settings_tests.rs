//! The settings of a launch in a throwaway tree. The guest tests are the point: a run for someone other than the owner
//! must have every other agent's folder, the owner's user.md and everyone else's files denied, and its sandbox closed.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use super::settings::{self, Input, Member};
use super::{machine, mcp};
use crate::paths::{self, Paths};

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "kith-settings-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
        let _ = fs::remove_dir_all(&root);
        for d in ["home", "kernel/bin", "kith/state"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let t = Tree { root };
        t.put(
            "kith/agents/echo/role/agent.md",
            "---\nname: echo\ndescription: t\n---\nYou are echo.\n",
        );
        t.put(
            "kith/agents/echo/role/settings.json",
            r#"{"permissions": {"allow": ["Bash(python3:*)", "Bash(git status:*)"]}}"#,
        );
        t.put(
            "kith/agents/other/role/agent.md",
            "---\nname: other\ndescription: t\n---\nOther.\n",
        );
        t.put("kith/agents/other/memory/MEMORY.md", "other\n");
        t.put("kith/agents/echo/memory/MEMORY.md", "owner\n");
        t.put("kith/agents/echo/memory/shared/MEMORY.md", "shared\n");
        t.put("kith/agents/echo/memory/people/sam/MEMORY.md", "sam\n");
        t.put("kith/agents/echo/memory/people/ann/MEMORY.md", "ann\n");
        t.put("kith/agents/echo/files/people/ann/note.md", "ann\n");
        t.put("kith/agents/echo/files/report.md", "owner output\n");
        t.put("kith/state/user.md", "owner\n");
        t.put("kith/state/people/sam.md", "sam\n");
        t.put("kith/state/people/ann.md", "ann\n");
        t
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn paths(&self) -> Paths {
        let root = self.root.clone();
        let env = move |k: &str| match k {
            "KITH_HOME" => Some(root.join("kith").to_string_lossy().into_owned()),
            "KITH_STATE" => Some(root.join("kith/state").to_string_lossy().into_owned()),
            _ => None,
        };
        paths::paths(&self.root.join("home"), &self.root.join("kernel"), &env)
    }

    fn settings(
        &self,
        member: &Member,
        caller_mode: &str,
    ) -> Result<settings::Output, settings::Refusal> {
        self.settings_with(member, caller_mode, Some("2.1.309"))
    }

    fn settings_with(
        &self,
        member: &Member,
        caller_mode: &str,
        claude_version: Option<&str>,
    ) -> Result<settings::Output, settings::Refusal> {
        let p = self.paths();
        let kernel = self.root.join("kernel");
        let src_pkg = self.root.join("kith/agents/echo/role");
        let home = self.root.join("home");
        let folders = settings::Folders::new(&p, &src_pkg, "echo", member);
        settings::prepare_folders(&p, &folders, member).unwrap();
        let env = |_: &str| None;
        let cwd = self
            .root
            .join("kith/agents/echo/files/people/sam")
            .to_string_lossy()
            .into_owned();
        let input = Input {
            agent: "echo",
            paths: &p,
            kernel: &kernel,
            src_pkg: &src_pkg,
            pkg: &src_pkg,
            member,
            claude: true,
            caller_mode,
            project: "",
            project_dirs: &[],
            cwd: &cwd,
            user_home: &home,
            kith_bin: Some("/usr/local/bin/kith"),
            plugins_json: "[]",
            owner_terminal: false,
            api_helper: "",
            once_id: "0123456789abcdef0123456789abcdef",
            developer_dir: "",
            ctmp: "",
            claude_version,
            env: &env,
        };
        settings::build(&input)
    }

    fn guest(&self, id: &str, mode: &str) -> Member {
        Member {
            owner: false,
            id: id.to_owned(),
            trusted: false,
            mode: mode.to_owned(),
            deny: vec!["Bash(curl:*)".to_owned()],
        }
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn guest_denies_every_other_agent_and_the_owner_files() {
    let t = Tree::new();
    let out = t.settings(&t.guest("sam", "acceptEdits"), "plan").unwrap();
    let s = out.settings.unwrap();
    let deny = strs(&s["permissions"]["deny"]);
    let root = t.root.to_string_lossy().into_owned();
    let has = |rule: String| assert!(deny.contains(&rule), "missing {rule}");
    for tool in ["Read", "Edit"] {
        has(format!("{tool}(/{root}/kith/agents/other/**)"));
        // the owner's memory and outputs, and everyone else's, by name
        has(format!("{tool}(/{root}/kith/agents/echo/memory/MEMORY.md)"));
        has(format!(
            "{tool}(/{root}/kith/agents/echo/memory/people/ann/**)"
        ));
        has(format!("{tool}(/{root}/kith/agents/echo/files/report.md)"));
        has(format!(
            "{tool}(/{root}/kith/agents/echo/files/people/ann/**)"
        ));
        has(format!("{tool}(/{root}/kith/state/people/ann.md)"));
    }
    has(format!("Read(/{root}/kith/state/user.md)"));
    // their own memory and their own person file are not denied
    assert!(
        !deny
            .iter()
            .any(|d| d.contains("memory/people/sam") || d.ends_with("people/sam.md)"))
    );
    // the membership's own deny comes first of the guest rules, and the secrets stay denied
    assert!(deny.contains(&"Bash(curl:*)".to_owned()));
    assert!(
        deny.iter()
            .any(|d| d.starts_with("Read(/") && d.contains("/state/secrets/**"))
    );
    // the membership's mode stands, whatever the caller asked
    assert_eq!(out.permission_mode.as_deref(), Some("acceptEdits"));
}

#[test]
fn guest_sandbox_is_closed_and_fails_closed() {
    let t = Tree::new();
    let s = t
        .settings(&t.guest("sam", "default"), "")
        .unwrap()
        .settings
        .unwrap();
    let sb = &s["sandbox"];
    assert_eq!(sb["enabled"], json!(true));
    assert_eq!(sb["failIfUnavailable"], json!(true));
    assert_eq!(sb["allowUnsandboxedCommands"], json!(false));
    assert_eq!(strs(&sb["filesystem"]["denyRead"])[0], "/");
    let root = t.root.to_string_lossy().into_owned();
    // the other agent's folder is unreadable by Bash too, the allowlist opens only what the run was given
    assert!(strs(&sb["filesystem"]["denyRead"]).contains(&format!("{root}/kith/agents/other")));
    assert!(
        strs(&sb["filesystem"]["allowRead"])
            .contains(&format!("{root}/kith/agents/echo/memory/people/sam"))
    );
    assert!(
        !strs(&sb["filesystem"]["allowRead"])
            .iter()
            .any(|p| p.contains("agents/other"))
    );
    assert_eq!(
        s["permissions"]["blockReadsOutsideWorkingDirectories"],
        json!(true)
    );
    // no broad Bash rule for an untrusted guest
    let allow = strs(&s["permissions"]["allow"]);
    assert!(!allow.contains(&"Bash(python3:*)".to_owned()));
    assert!(allow.contains(&"Bash(git status:*)".to_owned()));
    // a guest's memory write that cannot be checked is refused
    let hook = s["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(hook.starts_with("out=$(") && hook.contains("permissionDecision"));
}

#[test]
fn guest_needs_a_new_enough_claude_code() {
    let t = Tree::new();
    let m = t.guest("sam", "default");
    for v in [None, Some("2.1.256"), Some("1.9.999")] {
        let e = t.settings_with(&m, "", v).err().unwrap();
        assert_eq!(e.code, 3);
        assert!(e.message.contains("2.1.257"));
    }
    assert!(t.settings_with(&m, "", Some("2.1.257")).is_ok());
    assert!(t.settings_with(&m, "", Some("2.10.0")).is_ok());
}

#[test]
fn guest_work_dir_is_never_the_home_folder() {
    let t = Tree::new();
    let p = t.paths();
    let home = t.root.join("home");
    let src_pkg = t.root.join("kith/agents/echo/role");
    let kernel = t.root.join("kernel");
    let member = t.guest("sam", "default");
    let env = |_: &str| None;
    let cwd = home.to_string_lossy().into_owned();
    let files = settings::Folders::new(&p, &src_pkg, "echo", &member);
    settings::prepare_folders(&p, &files, &member).unwrap();
    let out = settings::build(&Input {
        agent: "echo",
        paths: &p,
        kernel: &kernel,
        src_pkg: &src_pkg,
        pkg: &src_pkg,
        member: &member,
        claude: true,
        caller_mode: "",
        project: "",
        project_dirs: &[],
        cwd: &cwd,
        user_home: &home,
        kith_bin: Some("/usr/local/bin/kith"),
        plugins_json: "[]",
        owner_terminal: false,
        api_helper: "",
        once_id: "x",
        developer_dir: "",
        ctmp: "",
        claude_version: Some("2.1.309"),
        env: &env,
    })
    .unwrap();
    assert_eq!(out.cwd, files.files);
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("wider than a guest may read"))
    );
}

#[test]
fn guest_without_the_memory_check_does_not_run() {
    let t = Tree::new();
    let p = t.paths();
    let home = t.root.join("home");
    let src_pkg = t.root.join("kith/agents/echo/role");
    let kernel = t.root.join("kernel");
    let member = t.guest("sam", "default");
    let env = |_: &str| None;
    let e = settings::build(&Input {
        agent: "echo",
        paths: &p,
        kernel: &kernel,
        src_pkg: &src_pkg,
        pkg: &src_pkg,
        member: &member,
        claude: true,
        caller_mode: "",
        project: "",
        project_dirs: &[],
        cwd: "/",
        user_home: &home,
        kith_bin: None,
        plugins_json: "[]",
        owner_terminal: false,
        api_helper: "",
        once_id: "x",
        developer_dir: "",
        ctmp: "",
        claude_version: Some("2.1.309"),
        env: &env,
    })
    .err()
    .unwrap();
    assert_eq!(e.code, 3);
}

#[test]
fn owner_is_held_to_auto_when_the_agent_reads_other_peoples_text() {
    let t = Tree::new();
    let out = t.settings(&Member::owner(), "bypassPermissions").unwrap();
    assert_eq!(out.permission_mode.as_deref(), Some("auto"));
    // an agent on the owner's own files may keep it
    t.put(
        "kith/agents/echo/role/capabilities.yaml",
        "reads_others: false\n",
    );
    let out = t.settings(&Member::owner(), "bypassPermissions").unwrap();
    assert_eq!(out.permission_mode.as_deref(), Some("bypassPermissions"));
}

#[test]
fn this_macs_mode_wins_over_the_agent_and_the_caller_wins_over_both() {
    let t = Tree::new();
    t.put(
        "kith/agents/echo/role/settings.json",
        r#"{"permissions": {"defaultMode": "plan"}}"#,
    );
    t.put(
        "kith/state/settings.json",
        r#"{"agents": {"echo": {"permission_mode": "acceptEdits"}}}"#,
    );
    let out = t.settings(&Member::owner(), "").unwrap();
    assert_eq!(out.permission_mode.as_deref(), Some("acceptEdits"));
    let out = t.settings(&Member::owner(), "default").unwrap();
    assert_eq!(out.permission_mode.as_deref(), Some("default"));
    // the agent's own suggestion applies when this Mac has said nothing; a retired one reads as its replacement
    fs::remove_file(t.root.join("kith/state/settings.json")).unwrap();
    t.put(
        "kith/agents/echo/role/settings.json",
        r#"{"permissions": {"defaultMode": "dontAsk"}}"#,
    );
    let s = t.settings(&Member::owner(), "").unwrap().settings.unwrap();
    assert_eq!(s["permissions"]["defaultMode"], json!("auto"));
}

#[test]
fn owner_keeps_the_hooks_command_lines() {
    let t = Tree::new();
    let s = t.settings(&Member::owner(), "").unwrap().settings.unwrap();
    let pre = &s["hooks"]["PreToolUse"];
    let first = pre[0]["hooks"][0]["command"].as_str().unwrap();
    assert!(first.starts_with("/usr/local/bin/kith memory hook pre --agent echo --state "));
    assert!(first.ends_with(" || true"));
    let post = s["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(post.contains(" memory hook post --agent echo "));
    // auto mode (this Mac's default): a refused call turns into a card, and one retry goes through
    assert!(
        s["hooks"]["PermissionDenied"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("kith-permit' denied")
            || s["hooks"]["PermissionDenied"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("kith-permit denied")
    );
    assert_eq!(s["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
}

#[test]
fn bash_q_quotes_like_printf() {
    let t = Tree::new();
    t.put("kith/agents/echo/role/settings.json", "{}");
    let s = t.settings(&Member::owner(), "").unwrap().settings.unwrap();
    let ledger = s["hooks"]["PreToolUse"][1]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(ledger.contains("/bin/kith-room-ledger pre --agent echo --home "));
}

#[test]
fn machine_settings_ignore_a_broken_file_and_read_old_modes() {
    let t = Tree::new();
    let m = t.paths().machine;
    assert_eq!(machine::agent_permission_mode(&m, "echo"), "");
    t.put(
        "kith/state/settings.json",
        r#"{"agents": {"echo": {"permission_mode": "manual"}}}"#,
    );
    // an old file still reads back: manual as default
    assert_eq!(machine::agent_permission_mode(&m, "echo"), "default");
    t.put(
        "kith/state/settings.json",
        r#"{"agents": {"echo": {"permission_mode": "yolo"}}}"#,
    );
    assert_eq!(machine::agent_permission_mode(&m, "echo"), "");
    t.put(
        "kith/state/settings.json",
        r#"{"agents": {"echo": {"permission_mode": "plan"}}, "plugins": ["a@b"]}"#,
    );
    assert_eq!(machine::agent_permission_mode(&m, "echo"), "plan");
    assert_eq!(machine::plugins(&m), vec!["a@b".to_owned()]);
    t.put("kith/state/settings.json", r#"{"agents": []}"#);
    assert_eq!(machine::agent_permission_mode(&m, "echo"), "");
}

#[test]
fn broad_bash_rules_are_stripped_for_untrusted_guests_only() {
    let rules: Vec<Value> = [
        "Bash",
        "Bash(*)",
        "Bash(python3:*)",
        "Bash(/usr/bin/python3 -c x)",
        "Bash(uv run:*)",
        "Bash(git status:*)",
        "Read(/x)",
    ]
    .iter()
    .map(|r| json!(r))
    .collect();
    let kept = machine::strip_broad_bash(rules.clone(), false);
    assert_eq!(kept, vec![json!("Bash(git status:*)"), json!("Read(/x)")]);
    assert_eq!(machine::strip_broad_bash(rules.clone(), true), rules);
}

#[test]
fn mcp_merges_granted_with_the_agents_own_and_gives_a_guest_the_permit_server_only() {
    let t = Tree::new();
    let kernel = t.root.join("kernel");
    let pkg = t.root.join("kith/agents/echo/role");
    let granted = json!({"mcpServers": {"svc": {"command": "a"}, "both": {"command": "granted"}}});
    t.put(
        "kith/agents/echo/role/mcp.json",
        r#"{"mcpServers": {"both": {"command": "own"}, "local": {"command": "b"}}}"#,
    );
    let m = mcp::merged(true, &kernel, Some(&granted), &pkg).unwrap();
    assert_eq!(m["mcpServers"]["svc"]["command"], json!("a"));
    assert_eq!(m["mcpServers"]["both"]["command"], json!("own"));
    assert_eq!(m["mcpServers"]["local"]["command"], json!("b"));
    let g = mcp::merged(false, &kernel, Some(&granted), &pkg).unwrap();
    assert_eq!(g["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(
        g["mcpServers"]["kithpermit"]["command"],
        json!(format!("{}/bin/kith-permit", kernel.display()))
    );
    // kith-auth failed: no shared services, the agent's own file still stands
    let none = mcp::merged(true, &kernel, None, &pkg).unwrap();
    assert_eq!(none["mcpServers"].as_object().unwrap().len(), 2);
}
