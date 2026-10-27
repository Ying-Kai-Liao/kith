//! Escape cards (F11 step 2b): `requestAccess` and `consumeAccess` on a real hub with the repo's own kith-auth.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::access::{self, consume, request};
use super::tests::{guest_run, joined, owner_run};
use super::*;
use crate::api::rig::Rig;
use crate::api::roles;
use std::path::{Path, PathBuf};

/// The strings of `NAME = (...)` in `kernel/lib/kith_guard.py`.
fn guard_list(name: &str) -> Vec<String> {
    let src =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/lib/kith_guard.py")).unwrap();
    let start = src.find(&format!("\n{name} = (")).unwrap_or_else(|| panic!("{name} not in kith_guard.py")) + 1;
    let body = &src[start..];
    let body = &body[..body.find(')').unwrap()];
    body.split('"').skip(1).step_by(2).map(str::to_owned).collect()
}

fn sorted(v: impl IntoIterator<Item = impl ToString>) -> Vec<String> {
    let mut v: Vec<String> = v.into_iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

#[test]
fn kith_guard_lists_match_the_kernels() {
    use resource as r;
    // The kernel's one list of secrets is the host's never-carded list, apart from the two files that exist only in the older
    // layout (secret_paths names them by hand).
    let host: Vec<_> = r::SENSITIVE.iter().filter(|p| !["auth.json", "env"].contains(p)).collect();
    assert_eq!(sorted(host), sorted(guard_list("SECRET_RULES")), "SENSITIVE and kith_guard.SECRET_RULES drifted apart");
    assert_eq!(sorted(r::SECRET_DIRS), sorted(guard_list("SECRET_DIRS")));
    assert_eq!(sorted(r::RULE_W_IN_FOLDER_TREES), sorted(guard_list("IN_FOLDER_TREES")));
    assert_eq!(sorted(r::RULE_W_IN_FOLDER_FILES), sorted(guard_list("IN_FOLDER_FILES")));
    assert_eq!(sorted(r::RULE_W_HOME_TREES), sorted(guard_list("HOME_TREES")));
    assert_eq!(sorted(r::RULE_W_HOME_FILES), sorted(guard_list("HOME_FILES")));
    assert_eq!(sorted(r::RULE_W_ABS_TREES), sorted(guard_list("ABS_TREES")));
}

#[test]
fn rule_w_and_secret_dirs_refuse_what_the_kernel_would() {
    let home = Path::new("/Users/kai");
    let w = |p: &str| resource::is_rule_w(Path::new(p), home, &[PathBuf::from("/opt/kith/kernel")]);
    for p in [
        "/Users/kai/proj/.claude/settings.json",
        "/Users/kai/proj/deep/er/.git/hooks/pre-commit",
        "/Users/kai/proj/CLAUDE.md",
        "/Users/kai/.zshrc",
        "/Users/kai/.local/bin/tool",
        "/Users/kai/Library/LaunchAgents/x.plist",
        "/opt/homebrew/bin/brew",
        "/opt/kith/kernel/bin/kith",
    ] {
        assert!(w(p), "{p} should be rule W");
    }
    for p in [
        "/Users/kai/proj/src/main.rs",
        "/Users/kai/Documents/x",
        "/Users/kai/proj/notCLAUDE.md",
        "/Users/kai/.zshrc.d/x",
    ] {
        assert!(!w(p), "{p} is not rule W");
    }
    assert!(w("/Users/KAI/PROJ/.CLAUDE/x") == cfg!(target_os = "macos"), "case is folded on macOS only");
}

#[test]
fn domains_are_plain_host_names() {
    for ok in ["pypi.org", "files.pythonhosted.org", "a-b.example.co.uk"] {
        assert!(access::valid_domain(ok), "{ok}");
    }
    for bad in [
        "",
        "localhost",
        "*.pypi.org",
        "1.2.3.4",
        "pypi.org:443",
        "pypi.org/x",
        "-a.org",
        "a..org",
        "PyPI.org",
        "a b.org",
    ] {
        assert!(!access::valid_domain(bad), "{bad}");
    }
}

fn open_cards(r: &Rig) -> Vec<Card> {
    r.hub.store.cards_open(None)
}

/// A folder of the rig the test can name, made so that its real path is itself.
fn place(r: &Rig, name: &str) -> String {
    let p = std::fs::canonicalize(&r.dir).unwrap().join(name);
    std::fs::create_dir_all(&p).unwrap();
    p.to_string_lossy().into_owned()
}

fn ask_for(target: &str) -> Value {
    json!({"kind": "path", "target": target, "reason": "to read the notes", "write": false})
}

/// The answer an approver gives to the entry of `card`.
fn reply(r: &Rig, card: &Card, option: &str) {
    let out = answer(&r.hub, card.entry_id.as_ref().unwrap(), Some(option), "owner").unwrap();
    assert!(matches!(out, Outcome::Recorded { .. }), "{out:?}");
}

fn sam(room: &str, run: &str) -> RunIdentity {
    RunIdentity { run_id: run.into(), ..guest_run("sam", room) }
}

#[tokio::test]
async fn a_secret_is_refused_with_no_card_and_a_folder_outside_the_box_goes_to_the_owner() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let who = sam("1:1:house:sam", "r1");
    let machine = resource::roots().kith[0].clone();
    for secret in [
        machine.join("secrets/env"),
        machine.join("serve/token"),
        machine.join("serve"),
        machine.clone(),
        PathBuf::from("/Users/nobody/proj/.claude/settings.json"),
        dirs::home_dir().unwrap().join(".zshrc"),
    ] {
        let reply = request(&r.hub, &who, &ask_for(&secret.to_string_lossy())).await;
        assert_eq!(reply["ok"], false, "{secret:?}: {reply}");
    }
    assert!(open_cards(&r).is_empty(), "a refusal makes no card");
    assert_eq!(request(&r.hub, &who, &ask_for("relative/x")).await["ok"], false);
    assert_eq!(
        request(&r.hub, &who, &json!({"kind": "path", "target": place(&r, "docs"), "reason": " "})).await["ok"],
        false
    );
    assert_eq!(
        request(&r.hub, &owner_run("s"), &ask_for(&place(&r, "docs"))).await["ok"],
        false,
        "an owner run isn't boxed"
    );

    let docs = place(&r, "Documents/x");
    let a = request(&r.hub, &who, &ask_for(&docs)).await;
    assert_eq!(a["carded"], true, "{a}");
    assert!(a["message"].as_str().unwrap().contains("End your turn"));
    let cards = open_cards(&r);
    assert_eq!(cards.len(), 1);
    let c = &cards[0];
    assert_eq!((c.approver.as_str(), c.tool.as_str(), c.person.as_str()), ("owner", "request_access", "sam"));
    assert_eq!(c.resource, format!("access:read:{docs}"));
    let entry = r.hub.store.entry(c.entry_id.as_ref().unwrap()).unwrap();
    assert!(entry.data["detail"].as_str().unwrap().contains(&docs), "the resolved path is shown");
    assert!(
        roles::offered(&entry.data, "allow")
            && roles::offered(&entry.data, "always")
            && roles::offered(&entry.data, "deny")
    );
    // A6: the same ask joins the card, a write ask is its own.
    request(&r.hub, &who, &ask_for(&docs)).await;
    assert_eq!(open_cards(&r).len(), 1);
    request(&r.hub, &who, &json!({"kind": "path", "target": docs, "reason": "edit", "write": true})).await;
    assert_eq!(open_cards(&r).len(), 2);
    // A link is resolved: a path through it shows where it really goes.
    let link = std::fs::canonicalize(&r.dir).unwrap().join("lnk");
    std::os::unix::fs::symlink(machine.join("secrets"), &link).unwrap();
    assert_eq!(request(&r.hub, &who, &ask_for(&link.to_string_lossy())).await["ok"], false, "a link into secrets");
}

#[tokio::test]
async fn a_yes_once_widens_the_next_activation_only_and_a_parallel_consume_has_one_winner() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let docs = place(&r, "docs");
    let who = sam("1:1:house:sam", "r1");
    request(&r.hub, &who, &ask_for(&docs)).await;
    let card = open_cards(&r).pop().unwrap();
    // Nothing is granted while the card is open, and the run that asked gets nothing mid-run.
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "early")).await["grants"]["read"], json!([]));
    reply(&r, &card, "allow");
    let granted = r.hub.store.card(&card.id).unwrap();
    assert_eq!((granted.state, granted.scope.as_str()), (CardState::Approved, "once"));
    assert!(granted.answered_parked, "owed to the session at its next activation");

    // C8: another thread of the same person can't take it.
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam-other", "other")).await["grants"]["read"], json!([]));
    // L3: two launches race for one grant: exactly one gets it.
    let (ia, ib) = (sam("1:1:house:sam", "next-a"), sam("1:1:house:sam", "next-b"));
    let (a, b) = tokio::join!(consume(&r.hub, &ia), consume(&r.hub, &ib));
    let wins = [&a, &b].iter().filter(|v| v["grants"]["read"] == json!([docs])).count();
    assert_eq!(wins, 1, "{a} {b}");
    // The activation after that has nothing, and the card is spent but still owed its resume (C7).
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "after")).await["grants"]["read"], json!([]));
    let used = r.hub.store.card(&card.id).unwrap();
    assert_eq!(used.state, CardState::Used);
    assert_eq!(r.hub.store.cards_owed_in("run:1:1:house:sam", now_ms()).len(), 1);
    // A run takes its access once: a second call of the same token (a helper) gets nothing.
    assert_eq!(consume(&r.hub, &ia).await["ok"], false);
}

#[tokio::test]
async fn an_expired_or_denied_card_grants_nothing() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let (a, b) = (place(&r, "a"), place(&r, "b"));
    let who = sam("1:1:house:sam", "r1");
    request(&r.hub, &who, &ask_for(&a)).await;
    request(&r.hub, &who, &ask_for(&b)).await;
    let cards = open_cards(&r);
    let (ca, cb) = (
        cards.iter().find(|c| c.resource.ends_with("/a")).unwrap(),
        cards.iter().find(|c| c.resource.ends_with("/b")).unwrap(),
    );
    reply(&r, ca, "deny");
    reply(&r, cb, "allow");
    r.hub.store.cards_sweep(now_ms() + 25 * 3_600_000).unwrap();
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "n")).await["grants"]["read"], json!([]));
}

#[tokio::test]
async fn an_always_persists_in_kith_auth_and_survives_every_activation_and_a_revoke_voids_it() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let (docs, site) = (place(&r, "docs"), "pypi.org");
    let who = sam("1:1:house:sam", "r1");
    request(&r.hub, &who, &ask_for(&docs)).await;
    request(&r.hub, &who, &json!({"kind": "domain", "target": site, "reason": "pip"})).await;
    for card in open_cards(&r) {
        reply(&r, &card, "always");
    }
    // The kith-auth write runs in the background.
    for _ in 0..200 {
        if r.hub.store.cards_open(None).is_empty()
            && roles::resolve("sam", "house")
                .await
                .is_some_and(|v| v["access"]["read"] == json!([docs]) && v["access"]["domains"] == json!([site]))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let held = roles::resolve("sam", "house").await.unwrap();
    assert_eq!((held["access"]["read"].clone(), held["access"]["domains"].clone()), (json!([docs]), json!([site])));
    for run in ["a", "b", "c"] {
        let g = consume(&r.hub, &sam("1:1:house:sam", run)).await;
        assert_eq!(
            (g["grants"]["read"].clone(), g["grants"]["domains"].clone()),
            (json!([docs]), json!([site])),
            "run {run}"
        );
    }
    // Another room of the same person holds it too: it is the membership's, not the session's.
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam-2", "d")).await["grants"]["read"], json!([docs]));
    // A7: taken away, void at the next consume.
    let bin = crate::remote::account::bin().await.unwrap();
    assert!(crate::channels::hq::output(&bin, &["person", "sam", "--rm"]).await.unwrap().status.success());
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "e")).await["ok"], false, "a removed person's grants are void");
}

#[tokio::test]
async fn kith_auth_refuses_secrets_rule_w_the_owner_and_non_members_for_an_always() {
    let r = Rig::new().await;
    let _sam = joined(&r, "guest").await;
    let bin = crate::remote::account::bin().await.unwrap();
    let run = |args: Vec<String>| {
        let bin = bin.clone();
        async move {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            crate::channels::hq::output(&bin, &a).await.unwrap().status.success()
        }
    };
    let docs = place(&r, "docs");
    let w = |p: &str| vec!["access".into(), "house".into(), "sam".into(), "read".into(), p.into()];
    assert!(run(w(&docs)).await, "a plain folder goes in");
    // kith-auth judges by the folders of its own environment, which the rig points at its temp state folder
    let secrets = std::fs::canonicalize(&r.dir).unwrap().join("state/secrets");
    for bad in [
        secrets.to_string_lossy().into_owned(),
        "/Users/x/p/.claude/settings.json".into(),
        dirs::home_dir().unwrap().join(".zshrc").display().to_string(),
        "rel".into(),
    ] {
        assert!(!run(w(&bad)).await, "{bad} must be refused");
    }
    assert!(!run(vec!["access".into(), "house".into(), "sam".into(), "domain".into(), "*.org".into()]).await);
    assert!(
        !run(vec!["access".into(), "house".into(), "nobody".into(), "read".into(), docs.clone()]).await,
        "not a person"
    );
    assert!(
        !run(vec!["access".into(), "money".into(), "sam".into(), "read".into(), docs.clone()]).await,
        "no row for that agent"
    );
    let held = roles::resolve("sam", "house").await.unwrap();
    assert_eq!(held["access"]["read"], json!([docs]), "only the plain folder was written");
}

/// F4: someone who uses the agent through the everyone row has no membership of their own, so kith-auth can't keep an
/// always for them. The yes stays a once grant, and the card says so instead of reading "Always allowed".
#[tokio::test]
async fn an_always_kith_auth_refuses_stays_once_and_the_card_tells_the_owner() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    assert!(r.auth_ok(&["member", "house", "*", "preset=guest"]));
    assert!(r.auth_ok(&["member", "house", "sam", "--rm"]));
    assert!(r.allowed("sam", "house"), "sam still uses house, through the everyone row");
    let docs = place(&r, "docs");
    request(&r.hub, &sam("1:1:house:sam", "r1"), &ask_for(&docs)).await;
    let card = open_cards(&r).pop().expect("a card for the owner");
    reply(&r, &card, "always");
    let entry = || r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data;
    for _ in 0..200 {
        if entry()["outcome"].is_string() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let data = entry();
    let said = data["outcome"].as_str().expect("the card says what became of the always");
    assert!(said.starts_with("Allowed once only"), "{said}");
    assert!(said.contains("no membership of their own"), "and why: {said}");
    assert!(!said.contains("kith-auth:"), "{said}");
    assert_eq!((data["status"].as_str(), data["selected"].as_str()), (Some("answered"), Some("allow")));
    assert_eq!(r.hub.store.card(&card.id).unwrap().scope, "once");
    // It is a real once grant: the next activation gets it, the one after doesn't, and nothing was kept.
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "n1")).await["grants"]["read"], json!([docs]));
    assert_eq!(consume(&r.hub, &sam("1:1:house:sam", "n2")).await["grants"]["read"], json!([]));
    assert_eq!(roles::resolve("sam", "house").await.unwrap()["access"]["read"], json!([]));
}

/// An always kept as asked has no outcome note: the apps say "Always allowed" from the option.
#[tokio::test]
async fn a_kept_always_has_no_outcome_note() {
    let r = Rig::new().await;
    r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let docs = place(&r, "docs");
    request(&r.hub, &sam("1:1:house:sam", "r1"), &ask_for(&docs)).await;
    let card = open_cards(&r).pop().unwrap();
    reply(&r, &card, "always");
    for _ in 0..200 {
        if r.hub.store.card(&card.id).is_some_and(|c| c.state == CardState::Used) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let data = r.hub.store.entry(card.entry_id.as_ref().unwrap()).unwrap().data;
    assert_eq!((data["selected"].as_str(), data["outcome"].is_null()), (Some("always"), true), "{data}");
}

/// The owner's list of an agent's always grants (`kithAccessGrants`), and taking one away (`kithRemoveAccessGrant`):
/// the next run of that person no longer gets it, and the others stay.
#[tokio::test]
async fn the_owner_lists_an_agents_always_grants_and_a_removed_one_is_gone_from_the_next_run() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let _sam = joined(&r, "guest").await;
    let (docs, notes) = (place(&r, "docs"), place(&r, "notes"));
    let before = now_ms();
    assert!(r.auth_ok(&["access", "house", "sam", "read", &docs]));
    assert!(r.auth_ok(&["access", "house", "sam", "write", &notes]));
    assert!(r.auth_ok(&["access", "house", "sam", "domain", "pypi.org"]));
    let call = |method: &'static str, body: Value| {
        let hub = r.hub.clone();
        async move { crate::api::dispatch(&hub, &crate::api::devices::Caller::Local, method, body).await }
    };
    let listed = call("kithAccessGrants", json!({"botId": bot})).await.unwrap();
    let grants = listed["grants"].as_array().unwrap();
    let mut seen: Vec<_> = grants
        .iter()
        .map(|g| (g["person"].as_str().unwrap(), g["kind"].as_str().unwrap(), g["target"].as_str().unwrap()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        sorted_tuples(vec![
            ("sam", "domain", "pypi.org"),
            ("sam", "read", docs.as_str()),
            ("sam", "write", notes.as_str())
        ])
    );
    assert!(grants.iter().all(|g| g["name"] == "Sam"), "{listed}");
    assert!(grants.iter().all(|g| g["since"].as_i64().is_some_and(|t| t >= before)), "{listed}");

    let left = call("kithRemoveAccessGrant", json!({"botId": bot, "person": "sam", "kind": "read", "target": docs}))
        .await
        .unwrap();
    assert_eq!(left["grants"].as_array().unwrap().len(), 2, "{left}");
    let next = consume(&r.hub, &sam("1:1:house:sam", "after")).await;
    assert_eq!(next["grants"]["read"], json!([]), "{next}");
    assert_eq!(
        (next["grants"]["write"].clone(), next["grants"]["domains"].clone()),
        (json!([notes]), json!(["pypi.org"]))
    );

    for bad in [
        json!({"botId": bot, "person": "sam", "kind": "exec", "target": notes}),
        json!({"botId": bot, "person": "--rm", "kind": "write", "target": notes}),
        json!({"botId": bot, "person": "sam", "kind": "write"}),
    ] {
        assert!(call("kithRemoveAccessGrant", bad.clone()).await.is_err(), "{bad}");
    }
    assert_eq!(call("kithAccessGrants", json!({"botId": bot})).await.unwrap()["grants"].as_array().unwrap().len(), 2);
}

/// A member can't see or take back anyone's always grants (both are owner-only).
#[tokio::test]
async fn a_member_cant_list_or_remove_always_grants() {
    let r = Rig::new().await;
    let bot = r.owner_bot("house");
    let sam_phone = joined(&r, "guest").await;
    for (method, body) in [
        ("kithAccessGrants", json!({"botId": bot})),
        ("kithRemoveAccessGrant", json!({"botId": bot, "person": "sam", "kind": "domain", "target": "pypi.org"})),
    ] {
        assert!(crate::api::dispatch(&r.hub, &sam_phone, method, body).await.is_err(), "{method}");
    }
}

fn sorted_tuples<'a>(mut v: Vec<(&'a str, &'a str, &'a str)>) -> Vec<(&'a str, &'a str, &'a str)> {
    v.sort();
    v
}

/// The shared layout (no `~/.kith`): the secrets folder is the config folder itself. The kernel (`whole_secret_paths`) and the
/// host (`holds_secrets_folder`) must refuse the same grants: the folder, below it, and any folder above it.
#[test]
fn the_shared_config_folder_is_refused_whole_by_both_sides() {
    let cfg = "/home/u/.config/kith";
    let roots = resource::Roots {
        kith: vec![PathBuf::from(cfg)],
        secrets_folder: vec![PathBuf::from(cfg)],
        ..Default::default()
    };
    let probes = [
        (cfg, true),
        ("/home/u/.config/kith/anything", true),
        ("/home/u/.config", true),
        ("/home/u", true),
        ("/", true),
        ("/home/u/.config/kithx", false),
        ("/home/u/.config/other", false),
        ("/home/u/docs", false),
    ];
    let script = format!(
        "import sys, json; sys.path.insert(0, 'kernel/lib'); import kith_guard as g\n\
         s = g.whole_secret_paths(['{cfg}'], '{cfg}', '{cfg}')\n\
         print(json.dumps([any(g.covers(x, p) or g.holds(p, x) for x in s) for p in json.loads(sys.argv[1])]))"
    );
    let paths: Vec<&str> = probes.iter().map(|p| p.0).collect();
    let out = std::process::Command::new("python3")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["-I", "-c", &script, &serde_json::to_string(&paths).unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let kernel: Vec<bool> = serde_json::from_slice(&out.stdout).unwrap();
    for ((p, want), k) in probes.iter().zip(kernel) {
        assert_eq!(resource::holds_secrets_folder(Path::new(p), &roots), *want, "host: {p}");
        assert_eq!(k, *want, "kernel: {p}");
    }
}
