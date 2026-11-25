//! The launch on Windows: the owner's settings against the Mac's (WI7), the file rules in a drive's spellings, and the refusal of
//! every run that is not the owner's (WI6). The rule and refusal tests run on every OS; the ones that need drive paths or
//! a Windows process run on the `windows` CI job only.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Map, Value, json};

use super::assemble::Env;
use super::plan::windows_guest_refusal;
use super::settings::{self, Input, Member, drive_spellings, windows_rules};
use crate::paths::{self, Paths};

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "kith-win-launch-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        for d in ["home", "kernel/bin", "kith/state", "work"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let t = Tree { root };
        t.put(
            "kith/agents/echo/role/agent.md",
            "---\nname: echo\ndescription: t\n---\nYou are echo.\n",
        );
        // A role's own rule names a folder by a placeholder, after the one `/` the rule syntax puts before an absolute path.
        t.put(
            "kith/agents/echo/role/settings.json",
            r#"{"permissions": {"allow": ["Bash(python3:*)"], "deny": ["Edit(/{{FILES_DIR}}/keep/**)"]}}"#,
        );
        t.put("kith/agents/echo/memory/MEMORY.md", "owner\n");
        t.put("kith/state/user.md", "owner\n");
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

    /// The owner's settings for agent `echo`, as the launch on this OS (`windows`) or on a Mac builds them.
    fn settings(&self, windows: bool) -> Value {
        let p = self.paths();
        let member = Member::owner();
        let kernel = self.root.join("kernel");
        let src_pkg = self.root.join("kith/agents/echo/role");
        let home = self.root.join("home");
        let folders = settings::Folders::new(&p, &src_pkg, "echo", &member);
        settings::prepare_folders(&p, &folders, &member).unwrap();
        let env = |_: &str| None;
        let cwd = self.root.join("work").to_string_lossy().into_owned();
        let input = Input {
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
            kith_bin: None,
            fence_bin: None,
            ledger_bin: None,
            cost_bin: None,
            permit_bin: None,
            threads_bin: None,
            plugins_json: "[]",
            owner_terminal: false,
            api_helper: "",
            once_id: "0123456789abcdef0123456789abcdef",
            developer_dir: "",
            ctmp: "",
            claude_version: None,
            guest_domains: Ok(&[]),
            windows,
            env: &env,
        };
        settings::build(&input).unwrap().settings.unwrap()
    }

    /// How a path under the tree can read in a rule or a command: as the OS spells it, as Git Bash spells it (`/c/...`), and
    /// with a lower-case drive.
    fn spellings(&self) -> Vec<String> {
        let native = self.root.to_string_lossy().replace('\\', "/");
        let mut all = vec![native.clone()];
        let b = native.as_bytes();
        if b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            let drive = native[..1].to_ascii_lowercase();
            all.push(format!("/{drive}{}", &native[2..]));
            all.push(format!("{drive}{}", &native[1..]));
        }
        all
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect()
}

/// The settings with every spelling of the tree's folder as `<ROOT>`, every `\` as `/`, the lists of rules without repeats,
/// and without the parts WI7 allows to differ: the hook commands (their form is WI3's) and the status line.
fn normalised(v: &Value, roots: &[String]) -> Value {
    fn walk(v: &Value, roots: &[String]) -> Value {
        match v {
            Value::String(s) => {
                let mut t = s.replace('\\', "/");
                for r in roots {
                    t = t.replace(r.as_str(), "<ROOT>");
                }
                Value::String(t)
            }
            Value::Array(a) => {
                let mut out: Vec<Value> = vec![];
                for x in a {
                    let x = walk(x, roots);
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
                Value::Array(out)
            }
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, x) in o {
                    out.insert(k.clone(), walk(x, roots));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    let mut out = walk(v, roots);
    if let Some(o) = out.as_object_mut() {
        o.remove("hooks");
        o.remove("statusLine");
    }
    out
}

#[test]
fn a_windows_owner_run_gets_the_macs_settings_but_for_spelling_and_powershell() {
    let t = Tree::new();
    let mac = t.settings(false);
    let win = t.settings(true);
    let roots = t.spellings();
    let mut want = normalised(&mac, &roots);
    let mut got = normalised(&win, &roots);
    // The one rule a Windows run has that a Mac's does not: Claude Code's PowerShell tool stays denied.
    let deny = got["permissions"]["deny"].as_array_mut().unwrap();
    assert!(
        deny.contains(&json!("PowerShell")),
        "the Windows settings must deny the PowerShell tool"
    );
    deny.retain(|r| r != "PowerShell");
    assert!(!strs(&want["permissions"]["deny"]).contains(&"PowerShell".to_owned()));
    // The same rules, in the same order, whatever the OS spelled them as.
    assert_eq!(
        strs(&got["permissions"]["deny"]),
        strs(&want["permissions"]["deny"])
    );
    assert_eq!(
        strs(&got["permissions"]["allow"]),
        strs(&want["permissions"]["allow"])
    );
    // Everything else, key by key.
    want.as_object_mut().unwrap().remove("permissions");
    got.as_object_mut().unwrap().remove("permissions");
    assert_eq!(got, want);
}

#[test]
fn the_owner_denies_what_the_macs_owner_denies() {
    // A floor the comparison above cannot give: rules both builds must have, so one dropped from both is still caught.
    let t = Tree::new();
    let s = normalised(&t.settings(true), &t.spellings());
    let deny = strs(&s["permissions"]["deny"]);
    for want in [
        "Read(/<ROOT>/kith/state/secrets/**)",
        "Edit(/<ROOT>/kith/state/secrets/**)",
        "Read(/<ROOT>/kith/state/core/**)",
        "Edit(/<ROOT>/kith/state/core/**)",
        // The role's own rule, with its placeholder filled.
        "Edit(/<ROOT>/kith/agents/echo/files/keep/**)",
    ] {
        assert!(
            deny.contains(&want.to_owned()),
            "missing {want} in {deny:#?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_windows_rule_is_spelled_for_git_bash_and_for_the_drive() {
    let t = Tree::new();
    let s = t.settings(true);
    let deny = strs(&s["permissions"]["deny"]);
    let native = t.root.to_string_lossy().replace('\\', "/");
    let (drive, rest) = native.split_at(1);
    let (git, drv) = (
        format!(
            "Read(//{}{}/kith/state/secrets/**)",
            drive.to_ascii_lowercase(),
            &rest[1..]
        ),
        format!(
            "Read(/{}{}/kith/state/secrets/**)",
            drive.to_ascii_uppercase(),
            rest
        ),
    );
    assert!(deny.contains(&git), "missing {git}");
    assert!(deny.contains(&drv), "missing {drv}");
    // No rule keeps a backslash: in a pattern it is an escape.
    assert!(
        deny.iter()
            .filter(|r| r.starts_with("Read(") || r.starts_with("Edit("))
            .all(|r| !r.contains('\\')),
        "{deny:#?}"
    );
}

#[test]
fn a_rule_on_a_drive_gets_both_spellings_and_any_other_rule_none() {
    assert_eq!(
        drive_spellings(r"Read(/C:\Users\k\.kith/secrets/**)").unwrap(),
        [
            "Read(//c/Users/k/.kith/secrets/**)",
            "Read(/C:/Users/k/.kith/secrets/**)"
        ]
    );
    assert_eq!(
        drive_spellings("Edit(/c:/x)").unwrap(),
        ["Edit(//c/x)", "Edit(/C:/x)"]
    );
    for not in [
        "Bash(rm:*)",
        "Read(//Users/k/x)",
        "Read(/Users/k/x)",
        "Read(//c/Users/x)",
        "Read(/Cx/y)",
        "WebFetch(domain:example.com)",
        "PowerShell",
    ] {
        assert!(drive_spellings(not).is_none(), "{not}");
    }
}

#[test]
fn windows_rules_spell_each_file_rule_and_deny_powershell_and_touch_nothing_else() {
    let mut sm = json!({"permissions": {
        "deny": [r"Read(/C:\a\b/**)", "Bash(rm:*)"],
        "allow": [r"Edit(/D:/w/**)", "Bash(git status:*)"],
    }, "model": "m"})
    .as_object()
    .unwrap()
    .clone();
    windows_rules(&mut sm).unwrap();
    let perms = &sm["permissions"];
    assert_eq!(
        strs(&perms["deny"]),
        [
            "Read(//c/a/b/**)",
            "Read(/C:/a/b/**)",
            "Bash(rm:*)",
            "PowerShell"
        ]
    );
    assert_eq!(
        strs(&perms["allow"]),
        ["Edit(//d/w/**)", "Edit(/D:/w/**)", "Bash(git status:*)"]
    );
    // No list is made that was not there.
    assert!(perms.get("ask").is_none());
    assert_eq!(sm["model"], "m");
    // Run again, nothing changes: the rules are already spelled.
    let before = sm.clone();
    windows_rules(&mut sm).unwrap();
    assert_eq!(sm, before);
}

fn env_of(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn every_run_that_is_not_the_owners_is_refused_on_windows_by_name() {
    for pairs in [
        // an invited person, a bot guest, a room's people
        &[("KITH_PERSON", "sam")][..],
        &[("KITH_PERSON", "room-1")][..],
        // the host's mark of a guest run, with no person named
        &[("KITH_RUN_TOKEN", "t")][..],
        &[("KITH_RUN_SOCKET", "/s")][..],
        &[("KITH_ROOM_MEMBERS", "sam,ann")][..],
    ] {
        let why = windows_guest_refusal(&env_of(pairs)).unwrap_or_else(|| panic!("{pairs:?}"));
        assert!(why.contains("Windows guest sandbox"), "{why}");
        assert!(why.contains("never started without a sandbox"), "{why}");
    }
    assert!(
        windows_guest_refusal(&env_of(&[("KITH_PERSON", ""), ("KITH_AGENT", "echo")])).is_none()
    );
    assert!(windows_guest_refusal(&env_of(&[])).is_none());
}

#[cfg(windows)]
mod on_windows {
    use super::*;
    use crate::launch::{assemble, plan, winenv};

    fn env(t: &Tree) -> Env {
        let mut env = winenv::process_env();
        for k in [
            "KITH_PERSON",
            "HQ_PERSON",
            "KITH_RUN_TOKEN",
            "KITH_RUN_SOCKET",
            "KITH_ROOM_MEMBERS",
            "KITH_AGENT",
            "HQ_AGENT",
            "KITH_VIA",
            "HQ_VIA",
            "PWD",
        ] {
            env.remove(k);
        }
        for (k, v) in [
            ("HOME", t.root.join("home")),
            ("KITH_HOME", t.root.join("kith")),
            ("KITH_STATE", t.root.join("kith/state")),
        ] {
            env.insert(k.to_owned(), v.to_string_lossy().into_owned());
        }
        env
    }

    #[test]
    fn a_guest_launch_exits_3_and_starts_nothing() {
        let t = Tree::new();
        let argv = ["--launch-json".to_owned()];
        for (k, v) in [("KITH_PERSON", "sam"), ("KITH_RUN_TOKEN", "tok")] {
            let mut e = env(&t);
            e.insert(k.to_owned(), v.to_owned());
            let got = assemble::launch_in(
                "echo",
                &argv,
                &t.root.join("kernel"),
                e,
                &t.root.join("work"),
            );
            assert_eq!(got.unwrap_err(), 3, "{k}");
        }
        // Nothing of a run was made for them: no folder for the person.
        assert!(!t.root.join("kith/agents/echo/memory/people/sam").exists());
    }

    #[test]
    fn an_owner_launch_in_process_is_the_owners_with_powershell_denied() {
        let t = Tree::new();
        let argv = ["--launch-json".to_owned()];
        let kernel = t.root.join("kernel");
        let work = t.root.join("work");
        let text = assemble::launch_in("echo", &argv, &kernel, env(&t), &work).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["harness"], "claude");
        assert_eq!(v["env"]["KITH_AGENT"], "echo");
        assert!(v["env"].get("KITH_PERSON").is_none());
        let roots = t.spellings();
        let norm = |s: &str| {
            let mut s = s.replace('\\', "/");
            for r in &roots {
                s = s.replace(r.as_str(), "<ROOT>");
            }
            s
        };
        // The folder the run works in is the one named, not this process's.
        assert_eq!(norm(v["cwd"].as_str().unwrap()), "<ROOT>/work");
        let deny = strs(&v["settings"]["permissions"]["deny"]);
        assert!(deny.contains(&"PowerShell".to_owned()), "{deny:#?}");
        // The plan alone is the same answer, through the same entry.
        let again = plan::plan_in(
            "echo",
            &argv,
            &kernel,
            env(&t),
            &plan::Mode::LaunchJson,
            Some(&work),
        )
        .unwrap();
        assert!(again.owner);
    }
}
