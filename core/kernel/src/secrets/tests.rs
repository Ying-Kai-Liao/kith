#![allow(clippy::unwrap_used, clippy::many_single_char_names)]

use super::*;
use serde_json::json;

/// A throwaway folder that goes away with the test.
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kith-secrets-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const NOW: f64 = 1_800_000_000_000.0;
const FUTURE: f64 = 4_102_444_800_000.0;

fn ctx(t: &Tmp) -> Ctx {
    Ctx {
        auth: t.0.join("secrets/auth.json"),
        agents: t.0.join("agents"),
        kernel: t.0.join("kernel"),
        person: None,
        now_ms: NOW,
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(t.0.join("serve")),
    }
}

fn with_tables(mut v: Value) -> Value {
    let o = v.as_object_mut().unwrap();
    for t in TABLES {
        o.entry(t).or_insert_with(|| json!({}));
    }
    v
}

fn people() -> Value {
    with_tables(json!({
        "people": {
            "kai": {"owner": true, "name": "Kai"},
            "sam": {"name": "Sam", "lang": "en", "was": ["samuel"]},
            "ola": {"name": "Ola", "revoked": true},
            "bot-s": {"bot_of": "sam", "was": ["bot-old"]},
            "bot-k": {"bot_of": "kai"},
            "lone": {},
        },
        "presets": {"pro": {"mode": "acceptEdits", "deny": ["Bash(rm *)"]}},
        "members": {
            "house": {
                "sam": {"preset": "pro", "model": "haiku", "deny": ["Edit"], "helpers": ["sound"], "budget": "20",
                        "env": {"A": "1"}, "access": {"read": ["/a"], "write": ["/w"], "domains": ["x.com", 3]}},
                "ola": {"preset": "pro"},
                "bot-s": {"env": {"B": "2"}, "access": {"read": ["/a/b", "/zzz"], "write": [], "domains": ["x.com", "y.com"]}},
                "bot-k": {},
            },
            "garden": {"*": {"preset": "member"}},
            "sound": {},
        },
    }))
}

fn rules<'a>(c: &'a Ctx, a: &'a Value) -> Rules<'a> {
    Rules::new(c, a)
}

#[test]
fn a_missing_file_is_empty_with_every_table() {
    let t = Tmp::new("missing");
    let a = load_auth(&ctx(&t)).unwrap();
    for table in TABLES {
        assert_eq!(a[table], json!({}), "{table}");
    }
}

#[test]
fn a_file_missing_tables_gets_the_defaults_and_keeps_the_rest() {
    let t = Tmp::new("old");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(
        &c.auth,
        r#"{"profiles": {"p": {}}, "extra": 1, "people": {"kai": {"owner": true}}}"#,
    )
    .unwrap();
    let a = load_auth(&c).unwrap();
    assert_eq!(a["extra"], json!(1));
    assert_eq!(a["members"], json!({}));
    assert_eq!(a["once"], json!({}));
    assert_eq!(owner(&a).as_deref(), Some("kai"));
}

#[test]
fn a_damaged_file_dies_with_pythons_words() {
    let t = Tmp::new("damaged");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    let path = c.auth.display().to_string();
    for (text, why) in [
        ("", "Expecting value: line 1 column 1 (char 0)"),
        (
            "{",
            "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
        ),
        (
            "{\"a\": 1,}",
            "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)",
        ),
        (
            "{\"a\" 1}",
            "Expecting ':' delimiter: line 1 column 6 (char 5)",
        ),
        ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
        ("{} x", "Extra data: line 1 column 4 (char 3)"),
        (
            "\n\n {\"a\": [1,\n 2 3]}",
            "Expecting ',' delimiter: line 4 column 4 (char 16)",
        ),
        ("\"a\\x\"", "Invalid \\escape: line 1 column 3 (char 2)"),
        (
            "\"abc",
            "Unterminated string starting at: line 1 column 1 (char 0)",
        ),
    ] {
        std::fs::write(&c.auth, text).unwrap();
        let d = load_auth(&c).unwrap_err();
        assert_eq!(
            d,
            Die {
                msg: format!("{path}: invalid JSON ({why})"),
                code: 1
            },
            "{text:?}"
        );
    }
    std::fs::write(&c.auth, "[]").unwrap();
    assert!(load_auth(&c).is_err());
}

#[test]
fn canonical_follows_an_old_id() {
    let a = people();
    assert_eq!(canonical(&a, "samuel"), "sam");
    assert_eq!(canonical(&a, "sam"), "sam");
    assert_eq!(canonical(&a, "nobody"), "nobody");
    assert_eq!(canonical(&a, "bot-old"), "bot-s");
}

#[test]
fn cut_off_covers_a_revoked_person_their_bots_and_a_loop() {
    let mut a = people();
    assert!(cut_off(&a, "ola"));
    assert!(!cut_off(&a, "sam"));
    assert!(!cut_off(&a, "bot-s"));
    a["people"]["sam"]["revoked"] = json!(true);
    assert!(cut_off(&a, "sam"));
    assert!(cut_off(&a, "bot-s"), "a bot answers to its owner");
    assert!(!cut_off(&a, ""));
    assert!(!cut_off(&a, "ghost"));
    // two bots of each other: the walk stops at the repeat
    a["people"]["x"] = json!({"bot_of": "y"});
    a["people"]["y"] = json!({"bot_of": "x"});
    assert!(!cut_off(&a, "x"));
}

#[test]
fn may_owner_unknown_revoked_member_everyone() {
    let t = Tmp::new("may");
    let c = ctx(&t);
    let a = people();
    let r = rules(&c, &a);
    assert_eq!(
        r.may("kai", "house", None, false).unwrap(),
        (true, "owner".into())
    );
    assert_eq!(
        r.may("", "house", None, false).unwrap(),
        (true, "owner".into())
    );
    assert_eq!(
        r.may("ghost", "house", None, false).unwrap(),
        (
            false,
            "'ghost' is not a person in kith (kith-auth people)".into()
        )
    );
    assert_eq!(
        r.may("ola", "house", None, false).unwrap(),
        (false, "ola's access was removed".into())
    );
    assert_eq!(
        r.may("sam", "house", None, false).unwrap(),
        (true, "member".into())
    );
    assert_eq!(
        r.may("samuel", "house", None, false).unwrap(),
        (true, "member".into())
    );
    assert_eq!(
        r.may("sam", "garden", None, false).unwrap(),
        (true, "member (everyone)".into())
    );
    assert_eq!(
        r.may("lone", "garden", None, false).unwrap(),
        (true, "member (everyone)".into())
    );
    assert_eq!(
        r.may("lone", "house", None, false).unwrap(),
        (false, "lone may not use house".into())
    );
}

#[test]
fn revoke_beats_the_everyone_row() {
    let t = Tmp::new("revoke");
    let c = ctx(&t);
    let mut a = people();
    a["people"]["lone"]["revoked"] = json!(true);
    let r = rules(&c, &a);
    assert_eq!(
        r.may("lone", "garden", None, false).unwrap(),
        (false, "lone's access was removed".into())
    );
}

#[test]
fn a_helper_is_allowed_through_the_callers_helpers_list() {
    let t = Tmp::new("helper");
    let c = ctx(&t);
    let a = people();
    let r = rules(&c, &a);
    assert_eq!(
        r.may("sam", "sound", Some("house"), false).unwrap(),
        (true, "helper of house".into())
    );
    assert_eq!(
        r.may("sam", "video", Some("house"), false).unwrap(),
        (
            false,
            "sam may not use video (not in their house helpers)".into()
        )
    );
    assert_eq!(
        r.may("sam", "video", None, false).unwrap(),
        (false, "sam may not use video".into())
    );
}

#[test]
fn a_once_grant_counts_only_with_once_and_while_it_lasts() {
    let t = Tmp::new("once");
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["sam"]["helper_once"] = json!({"video": FUTURE, "late": 1});
    let r = rules(&c, &a);
    assert!(!r.may("sam", "video", Some("house"), false).unwrap().0);
    assert_eq!(
        r.may("sam", "video", Some("house"), true).unwrap(),
        (true, "helper of house".into())
    );
    assert!(
        !r.may("sam", "late", Some("house"), true).unwrap().0,
        "it has run out"
    );
}

#[test]
fn a_bot_needs_its_own_row_and_its_owners_access() {
    let t = Tmp::new("bot");
    let c = ctx(&t);
    let mut a = people();
    {
        let r = rules(&c, &a);
        assert_eq!(
            r.may("bot-s", "house", None, false).unwrap(),
            (true, "bot of sam".into())
        );
        assert_eq!(
            r.may("bot-old", "house", None, false).unwrap(),
            (true, "bot of sam".into())
        );
        assert_eq!(
            r.may("bot-s", "garden", None, false).unwrap(),
            (false, "bot-s may not use garden".into()),
            "the * row never reaches a bot"
        );
        assert_eq!(
            r.may("bot-k", "house", None, false).unwrap(),
            (true, "bot of kai".into())
        );
    }
    // the owner loses the agent: the bot loses it at once
    a["members"]["house"].as_object_mut().unwrap().remove("sam");
    let r = rules(&c, &a);
    assert_eq!(
        r.may("bot-s", "house", None, false).unwrap(),
        (
            false,
            "bot-s may not use house: its owner sam may not".into()
        )
    );
    // revoked owner
    let mut b = people();
    b["people"]["sam"]["revoked"] = json!(true);
    assert_eq!(
        rules(&c, &b).may("bot-s", "house", None, false).unwrap(),
        (
            false,
            "bot-s may not use house: its owner sam may not".into()
        )
    );
    // owner gone
    let mut g = people();
    g["people"].as_object_mut().unwrap().remove("sam");
    assert_eq!(
        rules(&c, &g).may("bot-s", "house", None, false).unwrap(),
        (false, "bot-s's owner 'sam' is gone".into())
    );
}

#[test]
fn a_bot_may_use_a_helper_its_owner_may() {
    let t = Tmp::new("bothelper");
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["bot-s"]["helpers"] = json!(["sound"]);
    a["members"]["sound"]["sam"] = json!({});
    let r = rules(&c, &a);
    assert_eq!(
        r.may("bot-s", "sound", Some("house"), false).unwrap(),
        (true, "bot of sam".into())
    );
    // not in its caller row's helpers: refused
    a["members"]["house"]["bot-s"]["helpers"] = json!([]);
    assert!(
        !rules(&c, &a)
            .may("bot-s", "sound", Some("house"), false)
            .unwrap()
            .0
    );
}

#[test]
fn bots_that_follow_each_other_end_in_an_error_not_a_hang() {
    let t = Tmp::new("loop");
    let c = ctx(&t);
    let mut a = people();
    a["people"]["x"] = json!({"bot_of": "y"});
    a["people"]["y"] = json!({"bot_of": "x"});
    a["members"]["house"]["x"] = json!({});
    a["members"]["house"]["y"] = json!({});
    assert!(rules(&c, &a).may("x", "house", None, false).is_err());
}

fn keys(m: &Map<String, Value>) -> Vec<&str> {
    m.keys().map(String::as_str).collect()
}

#[test]
fn resolve_owner_is_short() {
    let t = Tmp::new("resolve-owner");
    let c = ctx(&t);
    let a = people();
    let out = rules(&c, &a).resolve("house", "kai", None, false).unwrap();
    assert_eq!(keys(&out), ["person", "owner", "allowed", "why", "name"]);
    assert_eq!(out["owner"], json!(true));
}

#[test]
fn resolve_a_member_gets_their_settings_over_the_agents() {
    let t = Tmp::new("resolve-member");
    let c = ctx(&t);
    let a = people();
    let out = rules(&c, &a)
        .resolve("house", "samuel", None, false)
        .unwrap();
    assert_eq!(
        keys(&out),
        [
            "person",
            "owner",
            "allowed",
            "why",
            "name",
            "lang",
            "preset",
            "mode",
            "deny",
            "model",
            "env",
            "helpers",
            "budget",
            "budget_agent",
            "was",
            "trusted",
            "access",
            "doors",
            "jobs"
        ]
    );
    let v = Value::Object(out);
    assert_eq!(v["person"], "sam");
    assert_eq!(v["preset"], "pro");
    assert_eq!(v["mode"], "acceptEdits");
    assert_eq!(v["deny"], json!(["Bash(rm *)", "Edit"]));
    assert_eq!(v["model"], "haiku");
    assert_eq!(v["env"], json!({"A": "1"}));
    assert_eq!(v["helpers"], json!(["sound"]));
    assert_eq!(v["budget"], "20");
    assert_eq!(v["budget_agent"], "house");
    assert_eq!(v["was"], json!(["samuel"]));
    assert_eq!(v["trusted"], json!(false));
    assert_eq!(
        v["access"],
        json!({"read": ["/a"], "write": ["/w"], "domains": ["x.com"]}),
        "strings only"
    );
    assert_eq!(v["doors"], json!([]));
    assert_eq!(v["jobs"], json!([]));
}

#[test]
fn resolve_a_row_with_no_preset_is_a_guest_and_a_helper_runs_with_the_callers_row() {
    let t = Tmp::new("resolve-guest");
    let c = ctx(&t);
    let a = people();
    let guest = rules(&c, &a)
        .resolve("garden", "lone", None, false)
        .unwrap();
    assert_eq!(guest["preset"], "member");
    assert_eq!(guest["mode"], "acceptEdits");
    assert_eq!(
        guest["budget_agent"], "garden",
        "the everyone row is still garden's"
    );
    let via = rules(&c, &a)
        .resolve("sound", "sam", Some("house"), false)
        .unwrap();
    assert_eq!(via["allowed"], json!(true));
    assert_eq!(via["why"], "helper of house");
    assert_eq!(via["model"], "haiku", "the caller's row");
    assert_eq!(via["budget_agent"], "house");
}

#[test]
fn a_free_shell_needs_trusted_or_it_falls_back_to_default() {
    let t = Tmp::new("free-shell");
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["sam"]["mode"] = json!("bypassPermissions");
    let r = rules(&c, &a).resolve("house", "sam", None, false).unwrap();
    assert_eq!(r["mode"], "default");
    assert_eq!(r["downgraded"], "bypassPermissions");
    assert_eq!(r["trusted"], json!(false));
    a["members"]["house"]["sam"]["trusted"] = json!(true);
    let r = rules(&c, &a).resolve("house", "sam", None, false).unwrap();
    assert_eq!(r["mode"], "bypassPermissions");
    assert_eq!(r["trusted"], json!(true));
    assert!(r.get("downgraded").is_none());
    // a preset's trust belongs to the preset's mode, a row's own mode needs the row's trust
    a["presets"]["free"] = json!({"mode": "auto", "trusted": true});
    a["members"]["house"]["sam"] = json!({"preset": "free"});
    let r = rules(&c, &a).resolve("house", "sam", None, false).unwrap();
    assert_eq!(r["mode"], "auto");
    a["members"]["house"]["sam"] = json!({"preset": "free", "mode": "auto"});
    let r = rules(&c, &a).resolve("house", "sam", None, false).unwrap();
    assert_eq!(
        r["mode"], "default",
        "the row's own mode is not covered by the preset's trust"
    );
}

#[test]
fn an_unknown_preset_dies() {
    let t = Tmp::new("preset");
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["sam"]["preset"] = json!("nope");
    let d = rules(&c, &a)
        .resolve("house", "sam", None, false)
        .unwrap_err();
    assert_eq!(d.msg, "no preset 'nope' (kith-auth preset nope mode=..)");
}

#[test]
fn a_refused_person_gets_the_short_answer() {
    let t = Tmp::new("refused");
    let c = ctx(&t);
    let a = people();
    let out = rules(&c, &a).resolve("house", "ola", None, false).unwrap();
    assert_eq!(keys(&out), ["person", "owner", "allowed", "why", "name"]);
    assert_eq!(out["allowed"], json!(false));
    assert_eq!(out["why"], "ola's access was removed");
}

#[test]
fn a_bot_is_a_guest_inside_what_its_owner_gets() {
    let t = Tmp::new("resolve-bot");
    let c = ctx(&t);
    let a = people();
    let out = rules(&c, &a)
        .resolve("house", "bot-s", None, false)
        .unwrap();
    assert_eq!(
        keys(&out),
        [
            "person",
            "owner",
            "allowed",
            "why",
            "preset",
            "mode",
            "deny",
            "model",
            "env",
            "helpers",
            "budget",
            "budget_agent",
            "was",
            "access",
            "doors",
            "jobs"
        ]
    );
    let v = Value::Object(out);
    assert_eq!(v["preset"], "guest");
    assert_eq!(v["mode"], "default");
    assert_eq!(
        v["deny"],
        json!(["Bash(rm *)", "Edit"]),
        "the owner's denies are the bot's too"
    );
    assert_eq!(
        v["model"], "haiku",
        "the owner's model when the bot's row names none"
    );
    assert_eq!(v["env"], json!({"B": "2"}));
    assert_eq!(v["helpers"], json!([]));
    assert_eq!(v["budget"], "20", "spends from its owner's budget");
    assert_eq!(v["budget_agent"], "house");
    assert_eq!(v["was"], json!(["sam", "samuel"]));
    // never more than the owner holds: /a/b is inside /a, /zzz is not; y.com is not the owner's
    assert_eq!(
        v["access"],
        json!({"read": ["/a/b"], "write": [], "domains": ["x.com"]})
    );
}

#[test]
fn the_owners_bots_are_capped_by_nothing_but_the_optional_cap() {
    let t = Tmp::new("resolve-bot-owner");
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["bot-k"] =
        json!({"access": {"read": ["/anything"], "domains": ["z.org"]}, "model": "own"});
    let r = rules(&c, &a)
        .resolve("house", "bot-k", None, false)
        .unwrap();
    assert_eq!(r["budget"], "");
    assert_eq!(r["budget_agent"], "");
    assert_eq!(r["model"], "own");
    assert_eq!(
        r["access"],
        json!({"read": ["/anything"], "write": [], "domains": ["z.org"]})
    );
    a["owner_bots_budget"] = json!("50");
    let r = rules(&c, &a)
        .resolve("house", "bot-k", None, false)
        .unwrap();
    assert_eq!(r["budget"], "50");
}

#[test]
fn a_sibling_bots_line_is_counted_in_was() {
    let t = Tmp::new("siblings");
    let c = ctx(&t);
    let mut a = people();
    a["people"]["bot-t"] = json!({"bot_of": "sam", "was": ["bt"]});
    let r = rules(&c, &a)
        .resolve("house", "bot-s", None, false)
        .unwrap();
    assert_eq!(r["was"], json!(["sam", "samuel", "bot-t", "bt"]));
}

#[test]
fn held_covers_paths_and_domains() {
    let have = json!({"read": ["/r/"], "write": ["/w"], "domains": ["a.com"]});
    assert!(held(&have, "read", &json!("/r/")));
    assert!(
        !held(&have, "read", &json!("/r")),
        "the folder's own name without its slash is not under it"
    );
    assert!(held(&have, "read", &json!("/r/x/y")));
    assert!(held(&have, "read", &json!("/w/x")), "write covers read");
    assert!(!held(&have, "write", &json!("/r/x")));
    assert!(!held(&have, "read", &json!("/rx")));
    assert!(held(&have, "domains", &json!("a.com")));
    assert!(!held(&have, "domains", &json!("b.com")));
}

// --- jobs ---------------------------------------------------------------------------------------------------------------

fn package(t: &Tmp, agent: &str, caps: &str) {
    let dir = t.0.join("agents").join(agent).join("role");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.md"), "---\nname: x\n---\n").unwrap();
    std::fs::write(dir.join("capabilities.yaml"), caps).unwrap();
}

const CAPS: &str = "needs: [a]\nhelpers: [sound, mix]\njobs:\n  - name: make-videos   # a job\n    label: Make videos\n    domains: [registry.npmjs.org, github.com, github.com]\n    helpers: [sound]\n  - name: bad\n    domains: [*.evil.com]\n  - name: outside\n    helpers: [other]\n  - name: second\n    domains: [example.org]\n";

#[test]
fn jobs_are_read_from_capabilities_and_a_bad_job_is_left_out_whole() {
    let t = Tmp::new("jobs-read");
    package(&t, "house", CAPS);
    let jobs = jobs::read(&t.0.join("agents/house/role/capabilities.yaml"));
    assert_eq!(
        jobs,
        vec![
            Job {
                name: "make-videos".into(),
                domains: vec!["registry.npmjs.org".into(), "github.com".into()],
                helpers: vec!["sound".into()],
                doors: vec![]
            },
            Job {
                name: "second".into(),
                domains: vec!["example.org".into()],
                ..Job::default()
            },
        ]
    );
}

#[test]
fn a_door_is_named_by_the_digest_of_its_declaration_and_lockfile() {
    let t = Tmp::new("jobs-door");
    let caps = "helpers: []\ndoors:\n  - name: render\n    npm: [hyperframes@0.8.159]\n    lockfile: doors/render.lock.json\n    tools: [node, ffmpeg]\n    bin: hyperframes/bin/hyperframes.mjs\n    args: [render, --quality, draft]\n    writes: [renders]\n    env: [HYPERFRAMES_NO_UPDATE_CHECK=1, FF=$ffmpeg]\njobs:\n  - name: render-videos\n    doors: [render]\n  - name: ghost\n    doors: [missing]\n";
    package(&t, "house", caps);
    let dir = t.0.join("agents/house/role/doors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("render.lock.json"), "{}").unwrap();
    let jobs = jobs::read(&t.0.join("agents/house/role/capabilities.yaml"));
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "render-videos");
    // sha256 of the sorted JSON of the declaration, a NUL and the lockfile's bytes: the digest kith_jobs.py computes
    assert_eq!(jobs[0].doors, vec!["render@03d245db65bb".to_owned()]);
}

#[test]
fn ticked_jobs_add_their_pieces_to_a_run_and_a_bot_gets_no_more_than_its_owner() {
    let t = Tmp::new("jobs-resolve");
    package(&t, "house", CAPS);
    let c = ctx(&t);
    let mut a = people();
    a["members"]["house"]["sam"]["jobs"] = json!({
        "make-videos": {"snapshot": {"domains": ["github.com", "gone.com"], "helpers": ["sound"], "doors": []}},
        "removed": {"snapshot": {"domains": ["x.org"]}},
    });
    a["members"]["house"]["bot-s"]["jobs"] = json!({
        "make-videos": {"snapshot": {"domains": ["github.com", "registry.npmjs.org"], "helpers": ["sound"]}},
        "second": {"snapshot": {"domains": ["example.org"]}},
    });
    let sam = Value::Object(rules(&c, &a).resolve("house", "sam", None, false).unwrap());
    assert_eq!(sam["access"]["domains"], json!(["x.com", "github.com"]));
    assert_eq!(sam["helpers"], json!(["sound"]));
    assert_eq!(
        sam["jobs"],
        json!(["make-videos"]),
        "only the jobs the package still has"
    );
    let bot = Value::Object(
        rules(&c, &a)
            .resolve("house", "bot-s", None, false)
            .unwrap(),
    );
    // the owner holds x.com and github.com; the bot's other domains are cut
    assert_eq!(bot["access"]["domains"], json!(["x.com", "github.com"]));
    assert_eq!(bot["jobs"], json!(["make-videos"]));
    assert_eq!(bot["helpers"], json!(["sound"]));
}

// --- take_once and the file protocol ---------------------------------------------------------------------------------

fn write_auth(c: &Ctx, a: &Value) {
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, serde_json::to_string(a).unwrap()).unwrap();
}

fn once_auth() -> Value {
    let mut a = people();
    a["members"]["house"]["sam"]["helper_once"] = json!({"video": FUTURE, "late": 1});
    a
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn json_of(o: &Outcome) -> Value {
    serde_json::from_str(&o.stdout).unwrap()
}

#[test]
fn take_once_uses_the_grant_up() {
    let mut a = once_auth();
    assert!(take_once(&mut a, "sam", "video", Some("house")));
    assert!(!take_once(&mut a, "sam", "video", Some("house")), "used up");
    assert!(!take_once(&mut a, "sam", "video", None));
    assert!(!take_once(&mut a, "ola", "video", Some("house")));
    assert_eq!(
        a["members"]["house"]["sam"]["helper_once"],
        json!({"late": 1})
    );
}

#[test]
fn resolve_take_once_lets_one_launch_through_and_the_second_has_no_grant() {
    let t = Tmp::new("take-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let first = run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    );
    assert_eq!(first.code, 0);
    let v = json_of(&first);
    assert_eq!(v["allowed"], json!(true));
    assert_eq!(v["why"], "helper of house (just this once)");
    assert_eq!(v["held"], json!(false));
    let second = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(second["allowed"], json!(false));
    assert_eq!(
        second["why"],
        "sam may not use video (not in their house helpers)"
    );
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"],
        json!({"late": 1})
    );
}

#[test]
fn resolve_without_take_once_leaves_the_file_alone() {
    let t = Tmp::new("no-take");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let before = std::fs::read(&c.auth).unwrap();
    let v = json_of(&run(&c, "resolve", &args(&["video", "--via", "house"])));
    assert_eq!(v["allowed"], json!(false));
    assert_eq!(std::fs::read(&c.auth).unwrap(), before);
    assert!(
        !c.auth.with_file_name("auth.json.lock").exists()
            || std::fs::metadata(c.auth.with_file_name("auth.json.lock")).is_ok()
    );
}

#[test]
fn a_removal_between_the_card_and_the_launch_still_wins() {
    let t = Tmp::new("revoked-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    let mut a = once_auth();
    a["people"]["sam"]["revoked"] = json!(true);
    write_auth(&c, &a);
    let v = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(v["allowed"], json!(false));
    assert_eq!(v["why"], "sam's access was removed");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"]["video"],
        json!(FUTURE),
        "not used up"
    );
}

#[test]
fn a_held_person_does_not_use_the_grant() {
    let t = Tmp::new("held-once");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    std::fs::create_dir_all(t.0.join("serve")).unwrap();
    std::fs::write(
        t.0.join("serve/cloud-sync.json"),
        r#"{"linked": true, "until": 1}"#,
    )
    .unwrap();
    let v = json_of(&run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    ));
    assert_eq!(v["held"], json!(true));
    assert_eq!(v["allowed"], json!(false));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&c.auth).unwrap()).unwrap();
    assert_eq!(
        saved["members"]["house"]["sam"]["helper_once"]["video"],
        json!(FUTURE)
    );
}

#[test]
fn bridge_held_reads_the_cloud_sync_file() {
    let t = Tmp::new("held");
    let c = ctx(&t);
    let a = people();
    let r = rules(&c, &a);
    let serve = t.0.join("serve");
    assert!(!r.bridge_held("sam"), "no file: never linked");
    std::fs::create_dir_all(&serve).unwrap();
    for (text, want) in [
        (r#"{"linked": true, "until": 1}"#, true),
        (r#"{"linked": true, "until": 4102444800000}"#, false),
        (r#"{"linked": false, "until": 1}"#, false),
        (r#"{"linked": true}"#, true),
        (r#"{"linked": true, "until": "abc"}"#, false),
        ("[1]", false),
        ("not json", false),
    ] {
        std::fs::write(serve.join("cloud-sync.json"), text).unwrap();
        assert_eq!(r.bridge_held("sam"), want, "{text}");
    }
    std::fs::write(
        serve.join("cloud-sync.json"),
        r#"{"linked": true, "until": 1}"#,
    )
    .unwrap();
    assert!(!r.bridge_held("kai"), "the owner is never held");
    assert!(!r.bridge_held(""));
}

#[test]
fn may_prints_why_and_exits_by_the_answer() {
    let t = Tmp::new("may-cli");
    let c = ctx(&t);
    write_auth(&c, &once_auth());
    assert_eq!(
        run(&c, "may", &args(&["sam", "house"])),
        Outcome {
            stdout: "member\n".into(),
            stderr: String::new(),
            code: 0
        }
    );
    let no = run(&c, "may", &args(&["sam", "video", "--via", "house"]));
    assert_eq!(
        (no.stdout.as_str(), no.code),
        ("sam may not use video (not in their house helpers)\n", 1)
    );
    let yes = run(
        &c,
        "may",
        &args(&["sam", "video", "--via", "house", "--once"]),
    );
    assert_eq!((yes.stdout.as_str(), yes.code), ("helper of house\n", 0));
    let usage = run(&c, "may", &args(&["sam"]));
    assert_eq!(
        (usage.stderr.as_str(), usage.code),
        ("kith-auth: may: see kith-auth --help\n", 2)
    );
    let unknown = run(&c, "frobnicate", &args(&[]));
    assert_eq!(unknown.code, 2);
}

#[test]
fn resolve_accepts_json_as_a_no_op_and_may_does_not() {
    let t = Tmp::new("json-flag");
    let c = ctx(&t);
    write_auth(&c, &people());
    let a = run(&c, "resolve", &args(&["house", "--json"]));
    let b = run(&c, "resolve", &args(&["house"]));
    assert_eq!(a, b);
    assert_eq!(run(&c, "may", &args(&["sam", "house", "--json"])).code, 2);
}

#[test]
fn resolve_acts_for_kith_person_and_the_owner_in_a_sandbox_is_no_one() {
    let t = Tmp::new("acting");
    write_auth(&ctx(&t), &people());
    let owner_run = run(&ctx(&t), "resolve", &args(&["house"]));
    assert_eq!(json_of(&owner_run)["person"], "kai");
    let boxed = Ctx {
        sandbox: Sandbox::Fixed(true),
        ..ctx(&t)
    };
    let v = json_of(&run(&boxed, "resolve", &args(&["house"])));
    assert_eq!(v["person"], "-");
    assert_eq!(v["allowed"], json!(false));
    let sam = Ctx {
        person: Some("samuel".into()),
        sandbox: Sandbox::Fixed(true),
        ..ctx(&t)
    };
    assert_eq!(
        json_of(&run(&sam, "resolve", &args(&["house"])))["person"],
        "sam"
    );
    let named = json_of(&run(
        &boxed,
        "resolve",
        &args(&["house", "--person", "sam"]),
    ));
    assert_eq!(named["person"], "sam");
}

#[test]
fn a_damaged_file_stops_resolve_with_exit_1_and_kith_auths_prefix() {
    let t = Tmp::new("resolve-damaged");
    let c = ctx(&t);
    std::fs::create_dir_all(c.auth.parent().unwrap()).unwrap();
    std::fs::write(&c.auth, "").unwrap();
    let o = run(&c, "resolve", &args(&["house"]));
    assert_eq!(o.code, 1);
    assert_eq!(
        o.stderr,
        format!(
            "kith-auth: {}: invalid JSON (Expecting value: line 1 column 1 (char 0))\n",
            c.auth.display()
        )
    );
    assert_eq!(o.stdout, "");
}

#[test]
fn save_auth_filters_like_kith_auth_and_writes_atomically_with_mode_0600() {
    use std::os::unix::fs::PermissionsExt as _;
    let t = Tmp::new("save");
    let c = ctx(&t);
    let mut a = once_auth();
    a["invites"] =
        json!({"live": {"expires": NOW / 1000.0 + 60.0}, "dead": {"expires": 5}, "none": {}});
    a["retired"] = json!({});
    write_auth(&c, &a);
    let mut locked = load_auth_locked(&c).unwrap();
    locked.auth["members"]["house"]["sam"]["note"] = json!("é");
    save_auth(&c, &locked).unwrap();
    drop(locked);
    let text = std::fs::read_to_string(&c.auth).unwrap();
    let saved: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        saved["invites"],
        json!({"live": {"expires": NOW / 1000.0 + 60.0}})
    );
    for gone in ["retired", "once"] {
        assert!(saved.get(gone).is_none(), "{gone}");
    }
    for kept in ["profiles", "grants", "env"] {
        assert_eq!(saved[kept], json!({}), "{kept}");
    }
    assert!(text.ends_with("}\n"));
    assert!(text.contains("\n  \"people\": {\n"), "indent=2");
    assert!(text.contains("\\u00e9"), "ASCII only");
    assert_eq!(
        std::fs::metadata(&c.auth).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let leftovers: Vec<_> = std::fs::read_dir(c.auth.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn the_save_matches_pythons_bytes() {
    // the file Python's save_auth would write for the same data (checked against kith-auth by hand and by the difftest)
    let t = Tmp::new("bytes");
    let c = ctx(&t);
    write_auth(
        &c,
        &json!({"people": {"kai": {"owner": true}}, "grants": {"a": {"s": ["p", "q"]}}}),
    );
    let locked = load_auth_locked(&c).unwrap();
    save_auth(&c, &locked).unwrap();
    assert_eq!(
        std::fs::read_to_string(&c.auth).unwrap(),
        "{\n  \"people\": {\n    \"kai\": {\n      \"owner\": true\n    }\n  },\n  \"grants\": {\n    \"a\": {\n      \"s\": [\n        \"p\",\n        \"q\"\n      ]\n    }\n  },\n  \"profiles\": {},\n  \"env\": {}\n}\n"
    );
}

#[test]
fn a_python_writer_holding_the_lock_makes_take_once_wait_and_see_its_write() {
    // Python's lock_auth/save_auth and ours must be one lock: hold it from Python, change the file there, and the
    // transaction here only reads once Python lets go.
    if Command::new("python3").arg("-V").output().is_err() {
        return;
    }
    let t = Tmp::new("py-lock");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let script = r"
import fcntl, json, sys, time
auth = sys.argv[1]
f = open(auth + '.lock', 'a')
fcntl.flock(f, fcntl.LOCK_EX)
print('locked', flush=True)
time.sleep(1.0)
d = json.load(open(auth))
d['members']['house']['sam']['helper_once'] = {}   # Python used it up
json.dump(d, open(auth, 'w'))
";
    let mut py = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(&c.auth)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(py.stdout.as_mut().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line.trim(), "locked");
    let start = Instant::now();
    let out = run(
        &c,
        "resolve",
        &args(&["video", "--via", "house", "--take-once"]),
    );
    let waited = start.elapsed();
    py.wait().unwrap();
    assert!(waited >= Duration::from_millis(700), "waited {waited:?}");
    // it saw Python's write: no grant left, so refused
    assert_eq!(json_of(&out)["allowed"], json!(false));
}

#[test]
fn two_launches_racing_for_one_grant_exactly_one_wins() {
    let t = Tmp::new("race");
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let wins = std::thread::scope(|s| {
        let runs: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    json_of(&run(
                        &c,
                        "resolve",
                        &args(&["video", "--via", "house", "--take-once"]),
                    ))
                })
            })
            .collect();
        runs.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|v| v["allowed"] == json!(true))
            .count()
    });
    assert_eq!(wins, 1);
}

#[test]
fn python_and_rust_racing_for_one_grant_exactly_one_wins() {
    let kith_auth = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
    if Command::new("python3").arg("-V").output().is_err() || !kith_auth.is_file() {
        return;
    }
    let t = Tmp::new("py-race");
    let home = t.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let c = Ctx {
        person: Some("sam".into()),
        ..ctx(&t)
    };
    write_auth(&c, &once_auth());
    let python = |t: &Tmp| {
        Command::new("python3")
            .arg(&kith_auth)
            .args(["resolve", "video", "--via", "house", "--take-once"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("KITH_HOME", t.0.join("kith"))
            .env("KITH_AUTH", &c.auth)
            .env("KITH_PERSON", "sam")
            .env("KITH_SERVE_DIR", t.0.join("serve"))
            .env("KITH_SERVE_PORT", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let procs: Vec<_> = (0..4).map(|_| python(&t)).collect();
    let rust: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    json_of(&run(
                        &c,
                        "resolve",
                        &args(&["video", "--via", "house", "--take-once"]),
                    ))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut wins = rust.iter().filter(|v| v["allowed"] == json!(true)).count();
    for p in procs {
        let o = p.wait_with_output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|_| panic!("python: {}", String::from_utf8_lossy(&o.stderr)));
        wins += usize::from(v["allowed"] == json!(true));
    }
    assert_eq!(wins, 1);
}

#[test]
fn json_errors_match_python() {
    for (text, want) in [
        ("", "Expecting value: line 1 column 1 (char 0)"),
        ("  ", "Expecting value: line 1 column 3 (char 2)"),
        (
            "{\"a\"",
            "Expecting ':' delimiter: line 1 column 5 (char 4)",
        ),
        ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
        (
            "{\"a\":1",
            "Expecting ',' delimiter: line 1 column 7 (char 6)",
        ),
        ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
        ("nul", "Expecting value: line 1 column 1 (char 0)"),
        (
            "\"a\nb\"",
            "Invalid control character at: line 1 column 3 (char 2)",
        ),
        (
            "\"\\u12\"",
            "Invalid \\uXXXX escape: line 1 column 3 (char 2)",
        ),
        ("01", "Extra data: line 1 column 2 (char 1)"),
        ("-", "Expecting value: line 1 column 1 (char 0)"),
        ("{\"a\":tru}", "Expecting value: line 1 column 6 (char 5)"),
        (
            "{\"a\":\"\\",
            "Unterminated string starting at: line 1 column 6 (char 5)",
        ),
        ("é", "Expecting value: line 1 column 1 (char 0)"),
        ("{\"a\":1} {", "Extra data: line 1 column 9 (char 8)"),
    ] {
        assert_eq!(pyjson::error_of(text).as_deref(), Some(want), "{text:?}");
    }
    assert_eq!(
        pyjson::error_of("{\"a\": [1, 2.5e3, null, true, \"x\"]}"),
        None
    );
    assert_eq!(pyjson::error_of("NaN"), None, "Python reads it");
}

#[test]
fn dumps_prints_like_python() {
    let v = json!({"a": [1, 2.5, "é\n\u{7f}"], "b": {}, "c": [], "d": null, "e": 1e16, "f": 1e-5, "g": true});
    assert_eq!(
        pyjson::dumps(&v),
        "{\"a\": [1, 2.5, \"\\u00e9\\n\\u007f\"], \"b\": {}, \"c\": [], \"d\": null, \"e\": 1e+16, \"f\": 1e-05, \"g\": true}"
    );
    assert_eq!(
        pyjson::dumps_indent(&json!({"a": [], "b": {"c": 1}})),
        "{\n  \"a\": [],\n  \"b\": {\n    \"c\": 1\n  }\n}"
    );
}

#[test]
fn domains_follow_the_guard_rule() {
    for ok in ["a.com", "registry.npmjs.org", "a-b.c0.example", "x.y"] {
        assert!(jobs::valid_domain(ok), "{ok}");
    }
    for bad in [
        "", "com", "*.a.com", "a.com:80", "1.2.3.4", "A.com", "-a.com", "a-.com", "a..com",
        "a.1com", ".a.com",
    ] {
        assert!(!jobs::valid_domain(bad), "{bad}");
    }
}
