//! The load filter and the prompt in a throwaway tree. The guest-privacy tests are the point: they say what a run for
//! someone other than the owner must never be handed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::facets::{self, Kind};
use super::prompt::{self, Registry, Who};
use crate::paths;

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "kith-launch-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        for d in ["home", "kith/agents/echo/role", "kith/state"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let t = Tree { root };
        t.put(
            "kith/agents/echo/role/agent.md",
            "---\nname: echo\ndescription: t\n---\nYou are echo.\n",
        );
        t
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn mem(&self, rel: &str, text: &str) {
        self.put(&format!("kith/agents/echo/memory/{rel}"), text);
    }

    fn paths(&self) -> paths::Paths {
        let root = self.root.clone();
        let env = move |k: &str| match k {
            "KITH_HOME" => Some(root.join("kith").to_string_lossy().into_owned()),
            "KITH_STATE" => Some(root.join("kith/state").to_string_lossy().into_owned()),
            _ => None,
        };
        paths::paths(&self.root.join("home"), &self.root.join("kernel"), &env)
    }

    /// The prompt for `person` (None: the owner) in the room the environment names.
    fn prompt(&self, person: Option<&str>, project: &str, env: &[(&str, &str)]) -> String {
        let auth = json!({"people": {
            "kai": {"owner": true, "name": "Kai"},
            "sam": {"name": "Sam"}, "ann": {"name": "Ann"},
            "bot-ab12cd34": {"bot_of": "sam"}
        }});
        let who = Who::resolve(Some(&auth), person);
        let p = self.paths();
        let src = prompt::agent_dir(&p, &self.root.join("kernel"), "echo").unwrap();
        let reg = Registry::load(&p.state).unwrap();
        let vars: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let getenv = move |k: &str| vars.get(k).cloned();
        let home = self.root.join("home");
        prompt::build(&prompt::Input {
            agent: "echo",
            paths: &p,
            kernel: &self.root.join("kernel"),
            src_pkg: &src,
            pkg: &src,
            who: &who,
            project,
            registry: &reg,
            cwd: "/work",
            date: "2026-11-03",
            ctmp_note: "",
            user_home: &home,
            env: &getenv,
        })
        .unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn secrets_tree() -> Tree {
    let t = Tree::new();
    t.put("kith/state/user.md", "OWNER-ABOUT\n");
    t.put("kith/state/people/sam.md", "SAM-ABOUT\n");
    t.put("kith/state/people/ann.md", "ANN-ABOUT\n");
    t.mem("MEMORY.md", "OWNER-MEMORY\n");
    t.mem("shared/MEMORY.md", "SHARED-CRAFT\n");
    t.mem("people/sam/MEMORY.md", "SAM-MEMORY\n");
    t.mem("people/ann/MEMORY.md", "ANN-MEMORY\n");
    t.mem("people/ann/sessions/s.md", "ANN-SESSION\n");
    t.mem("rooms/grp1/a.md", "ROOM-NOTE\n");
    t.mem("notes.md", "OWNER-LOOSE-NOTE\n");
    t.mem("widen.md", "---\naudience: shared\n---\nWIDEN-ATTEMPT\n");
    t.mem("projects/alpha/README.md", "ALPHA-NOTE\n");
    t
}

#[test]
fn a_guest_never_sees_the_owners_or_another_persons_memory() {
    let t = secrets_tree();
    for person in ["sam", "bot-ab12cd34"] {
        for env in [
            &[][..],
            &[("KITH_ROOM", "grp1")][..],
            &[("KITH_ROOM", "grp1"), ("KITH_ROOM_MEMBERS", "sam,ann")][..],
        ] {
            let p = t.prompt(Some(person), "", env);
            for secret in [
                "OWNER-ABOUT",
                "OWNER-MEMORY",
                "ANN-ABOUT",
                "ANN-MEMORY",
                "ANN-SESSION",
                "OWNER-LOOSE-NOTE",
                "WIDEN-ATTEMPT",
            ] {
                assert!(!p.contains(secret), "{person} {env:?} was handed {secret}");
            }
            assert!(
                p.contains("SHARED-CRAFT"),
                "{person} {env:?} lost the shared craft"
            );
        }
    }
}

#[test]
fn a_person_sees_their_own_and_the_shared_part_only() {
    let t = secrets_tree();
    let p = t.prompt(Some("sam"), "", &[]);
    assert!(p.contains("SAM-ABOUT") && p.contains("SAM-MEMORY") && p.contains("SHARED-CRAFT"));
    assert!(p.contains("<limits>This run is in a sandbox for sam."));
    assert!(p.contains(" person=\"sam\" name=\"Sam\"/>"));
    // a bot guest is shared craft only, never even its own person-folder memory
    t.mem("people/bot-ab12cd34/MEMORY.md", "BOT-OWN\n");
    let b = t.prompt(Some("bot-ab12cd34"), "", &[]);
    assert!(!b.contains("BOT-OWN") && !b.contains("SAM-MEMORY") && b.contains("SHARED-CRAFT"));
}

#[test]
fn a_group_room_gets_what_everyone_there_may_see() {
    let t = secrets_tree();
    // the owner in a room with sam: the intersection of their sets, so shared only; the withheld line says nothing more
    let p = t.prompt(
        None,
        "",
        &[("KITH_ROOM", "grp1"), ("KITH_ROOM_MEMBERS", "sam")],
    );
    assert!(p.contains("SHARED-CRAFT") && p.contains("room=\"group\""));
    assert!(!p.contains("OWNER-MEMORY") && !p.contains("SAM-MEMORY") && !p.contains("OWNER-ABOUT"));
    assert!(p.contains("ask me in our 1:1"));
    // members unknown: shared only, whoever asks
    let u = t.prompt(None, "", &[("KITH_ROOM", "grp1")]);
    assert!(!u.contains("OWNER-MEMORY") && u.contains("SHARED-CRAFT"));
    // the owner alone (the host's marker) reads the owner's part
    let o = t.prompt(
        None,
        "",
        &[("KITH_ROOM", "grp1"), ("KITH_ROOM_OWNER_ONLY", "1")],
    );
    assert!(o.contains("OWNER-MEMORY"));
    // ... but not in a channel run
    let c = t.prompt(
        None,
        "",
        &[
            ("KITH_ROOM", "grp1"),
            ("KITH_ROOM_OWNER_ONLY", "1"),
            ("KITH_VIA", "telegram"),
        ],
    );
    assert!(!c.contains("OWNER-MEMORY"));
}

#[test]
fn a_room_note_loads_only_where_everyone_was_present_when_it_was_written() {
    let t = secrets_tree();
    t.mem(
        "rooms/grp1/a.md",
        "---\naudience: room:grp1\n---\nROOM-NOTE\n",
    );
    let env = [("KITH_ROOM", "grp1"), ("KITH_ROOM_MEMBERS", "sam")];
    // no ledger: the note was the owner's alone
    assert!(!t.prompt(Some("sam"), "", &env).contains("ROOM-NOTE"));
    t.put(
        "kith/state/room-ledger/echo/grp1.json",
        r#"{"rooms/grp1/a.md": ["sam"]}"#,
    );
    assert!(t.prompt(Some("sam"), "", &env).contains("ROOM-NOTE"));
    // someone the note was not shared with joins: it drops out again
    assert!(
        !t.prompt(
            Some("sam"),
            "",
            &[("KITH_ROOM", "grp1"), ("KITH_ROOM_MEMBERS", "sam,ann")]
        )
        .contains("ROOM-NOTE")
    );
}

#[test]
fn a_note_cannot_widen_itself_but_can_narrow() {
    let t = secrets_tree();
    t.mem("shared/narrow.md", "---\naudience: owner\n---\nNARROWED\n");
    let f = facets::facets_for("widen.md", "echo", "---\naudience: shared\n---\nx");
    assert_eq!(f.audience, "owner");
    let g = facets::facets_for("shared/narrow.md", "echo", "---\naudience: owner\n---\nx");
    assert_eq!(g.audience, "owner");
    // a promotion record in the text grants nothing without the host's signed row
    let h = facets::facets_for(
        "widen.md",
        "echo",
        "---\naudience: shared\napproved_change: 7\n---\nx",
    );
    assert_eq!(h.audience, "owner");
}

#[test]
fn folder_defaults_follow_the_layout() {
    let d = |r| facets::folder_defaults(r, "echo");
    assert_eq!(d("MEMORY.md").audience, "owner");
    assert_eq!(d("shared/MEMORY.md").audience, "shared");
    let p = d("people/Sam/MEMORY.md");
    assert_eq!(
        (p.audience.as_str(), p.room.as_str()),
        ("person:sam", "1:1:echo:sam")
    );
    let s = d("people/sam/sessions/t1.md");
    assert_eq!(
        (s.kind.as_str(), s.thread.as_str()),
        ("project-detail", "t1")
    );
    assert_eq!(d("rooms/g/x.md").audience, "room:g");
    assert_eq!(d("projects/a/x.md").projects, vec!["a"]);
    assert_eq!(d("projects/b.md").projects, vec!["b"]);
    assert_eq!(d("sessions/z.md").kind, "project-detail");
}

#[test]
fn frontmatter_is_read_like_the_python_twin() {
    let (fm, rest) = facets::split_frontmatter(
        "---\nAudience: \"shared\"\nbad key: x\nprojects: [a, 'b', c d]\n---\nbody\n",
    );
    assert_eq!(fm.get("audience").map(String::as_str), Some("shared"));
    assert!(!fm.contains_key("bad key"));
    assert_eq!(rest, "body\n");
    let f = facets::apply_frontmatter(&facets::Facets::owner_only(), &fm);
    assert_eq!(f.projects, vec!["a", "b"]);
    assert_eq!(facets::split_frontmatter("no front\n").1, "no front\n");
}

#[test]
fn over_the_cap_the_oldest_project_notes_go_first_and_the_person_stays() {
    let t = secrets_tree();
    t.mem("projects/alpha/README.md", &"i".repeat(100));
    fs::write(
        t.root.join("kith/agents/echo/memory/projects/alpha/old.md"),
        "o".repeat(100),
    )
    .unwrap();
    t.put(
        "kith/agents/echo/role/agent.md",
        "---\nname: echo\nmemory_cap: 150\n---\nYou.\n",
    );
    let p = t.prompt(None, "", &[]);
    assert!(p.contains("OWNER-MEMORY"));
    let q = {
        let reg = serde_json::json!({"projects": {"alpha": {}}});
        t.put("kith/state/projects.json", &reg.to_string());
        t.prompt(None, "alpha", &[])
    };
    assert!(
        q.contains("OWNER-MEMORY") && q.contains("more=\"1\""),
        "{q}"
    );
}

#[test]
fn a_projects_start_files_are_the_tail_and_the_agents_own_open_lines() {
    let t = secrets_tree();
    t.put(
        "kith/state/projects.json",
        r#"{"projects": {"alpha": {"parent": "acme"}, "acme": {}, "beta": {"parent": "alpha"}}}"#,
    );
    let mut dec = String::new();
    for i in 1..=12 {
        dec.push_str(&format!(
            "2026-10-01 \u{b7} echo \u{b7} open: q{i} \u{b7} c\n"
        ));
    }
    dec.push_str("2026-10-01 \u{b7} other \u{b7} open: theirs \u{b7} c\n");
    for i in 1..=40 {
        dec.push_str(&format!(
            "2026-10-02 \u{b7} other \u{b7} decision {i} \u{b7} w\n"
        ));
    }
    t.put("kith/state/projects/alpha/decisions.md", &dec);
    t.put("kith/state/projects/alpha/codebase.md", "MAP\n");
    t.mem("sessions/alpha.md", "OWNER-SESSION\n");
    t.mem("people/sam/sessions/alpha.md", "SAM-SESSION\n");
    let o = t.prompt(None, "alpha", &[]);
    assert!(o.contains("OWNER-SESSION") && o.contains("MAP") && !o.contains("SAM-SESSION"));
    assert!(
        o.contains("decision 40 ") && o.contains("decision 11 ") && !o.contains("decision 10 ")
    );
    assert!(
        o.contains("(12, the newest 10 here")
            && o.contains(" q12 ")
            && o.contains(" q3 ")
            && !o.contains(" q2 ")
            && !o.contains("theirs")
    );
    assert!(o.contains("parents=\"acme\" holds=\"beta\""));
    let s = t.prompt(Some("sam"), "alpha", &[]);
    assert!(s.contains("SAM-SESSION") && !s.contains("OWNER-SESSION") && s.contains("MAP"));
}

#[test]
fn an_unknown_project_is_refused() {
    let t = Tree::new();
    let auth = json!({});
    let who = Who::resolve(Some(&auth), None);
    let p = t.paths();
    let src = prompt::agent_dir(&p, &t.root.join("kernel"), "echo").unwrap();
    let reg = Registry::load(&p.state).unwrap();
    let r = prompt::build(&prompt::Input {
        agent: "echo",
        paths: &p,
        kernel: Path::new("/k"),
        src_pkg: &src,
        pkg: &src,
        who: &who,
        project: "nope",
        registry: &reg,
        cwd: "/",
        date: "d",
        ctmp_note: "",
        user_home: Path::new("/h"),
        env: &|_| None,
    });
    assert!(r.unwrap_err().contains("unknown project 'nope'"));
}

#[test]
fn who_cleans_what_goes_into_the_tag() {
    let auth = json!({"people": {"kai": {"owner": true, "name": "K\"ai"}, "sam": {"name": "<S>\"am\n", "was": ["old"]}}});
    let sam = Who::resolve(Some(&auth), Some("old"));
    assert_eq!(
        (sam.id.as_str(), sam.owner, sam.kind()),
        ("sam", false, Kind::Person)
    );
    assert_eq!(sam.line, " person=\"sam\" name=\"Sam\"");
    let kai = Who::resolve(Some(&auth), Some("kai"));
    assert!(kai.owner && kai.line.contains("name=\"Kai\""));
    assert_eq!(Who::resolve(Some(&auth), None), Who::owner());
    assert!(!Who::resolve(None, Some("kai")).owner);
}

#[test]
fn the_template_is_the_body_then_the_protocol() {
    let t = Tree::new();
    t.put("kernel/PROTOCOL.md", "PROTO {{AGENT}}\n");
    let p = t.prompt(None, "", &[]);
    assert!(p.starts_with("You are echo.\n\nPROTO echo\n"), "{p}");
}

#[test]
fn guest_tmp_prefers_the_id_then_a_hash_then_nothing() {
    let h = |s: &str| format!("{s:0<8}-hash");
    let base = Path::new("/m");
    assert_eq!(
        prompt::guest_tmp(base, "sam", 501, &h),
        Some(base.join("run/t/sam"))
    );
    let long = "x".repeat(40);
    assert_eq!(
        prompt::guest_tmp(base, &long, 501, &|_| "abcdef0123".into()),
        Some(base.join("run/t/abcdef01"))
    );
    assert_eq!(
        prompt::guest_tmp(Path::new(&"/m".repeat(20)), "sam", 501, &h),
        None
    );
}
