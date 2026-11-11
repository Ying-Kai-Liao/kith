//! The card writes `kith-auth` routes here on the tombstone (C-a, C-b): `people.access`, `people.helper`, `people.job`
//! and `people.revision`. The byte-for-byte parity with the file is `kernel/tests/kith-auth-router-check`; these hold the
//! core's side: the refusals in the one locked step that writes, the removals that take the entry as written, the fence of
//! `viaSwitch` and of a damaged record, and the revision the host's cards watch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::writes::Rig;
use crate::cutover::state::{self, Phase, Switch};
use crate::layout::Layout;

fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam"},
            "ann": {"name": "Ann"},
            "pat": {"name": "Pat", "revoked": {"at": 5}},
            "gus": {"name": "Gus"},
            "bot-s": {"bot_of": "sam"},
            "bot-k": {"bot_of": "kai"},
            "bot-p": {"bot_of": "pat"},
        },
        "members": {
            "house": {
                "sam": {"preset": "member", "helpers": ["garden"],
                        "access": {"read": ["/srv/a"], "write": ["/srv/w"], "domains": ["x.example.com"]},
                        "jobs": {"work": {"snapshot": {"domains": ["a.example"], "helpers": ["garden"]}}}},
                "ann": {"preset": "member"},
                "pat": {"preset": "member", "access": {"read": ["/srv/old"], "write": [], "domains": []}},
                "gus": {"preset": "guest"},
                "bot-s": {},
                "bot-k": {},
                "bot-p": {},
                "*": {"preset": "guest"},
            },
        },
    })
}

fn switched(rig: &Rig, phase: Phase) {
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    let s = Switch {
        phase,
        snapshot: None,
        at: 1,
    };
    state::write(&layout, "authority", &s).unwrap();
}

fn rig() -> Rig {
    let rig = Rig::new(&world());
    switched(&rig, Phase::Switched);
    rig
}

/// The caller's home: a folder that need not exist (rule W reads its name).
const HOME: &str = "/home/kith-test";

fn access(person: &str, kind: &str, value: &str, more: &Value) -> Value {
    let mut p = json!({"agent": "house", "person": person, "kind": kind, "value": value,
                       "viaSwitch": true, "home": HOME, "dies": true});
    for (k, v) in more.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

/// What the router prints: the lines, or the refusal and its code.
fn said(rig: &mut Rig, method: &str, params: &Value) -> Result<Vec<String>, (String, i64)> {
    let ans = rig.call(method, params).unwrap();
    if let Some(code) = ans["died"].as_i64() {
        return Err((ans["why"].as_str().unwrap().to_owned(), code));
    }
    Ok(ans["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_owned())
        .collect())
}

fn row(rig: &mut Rig, person: &str) -> Value {
    rig.tables()["members"]["house"][person].clone()
}

#[test]
fn an_always_lands_on_the_persons_row_once() {
    let mut rig = rig();
    let p = access("ann", "domain", "docs.example.com", &json!({}));
    assert_eq!(
        said(&mut rig, "people.access", &p).unwrap(),
        ["ann may domain docs.example.com beyond the box for house"]
    );
    let first = row(&mut rig, "ann");
    assert_eq!(first["access"]["domains"], json!(["docs.example.com"]));
    assert_eq!(first["access_by"]["domain:docs.example.com"], Value::Null);
    // the same always twice is one entry
    said(&mut rig, "people.access", &p).unwrap();
    assert_eq!(
        row(&mut rig, "ann")["access"]["domains"],
        json!(["docs.example.com"])
    );
}

#[test]
fn the_owner_and_the_everyone_row_are_never_given_an_always() {
    let mut rig = rig();
    let e = said(
        &mut rig,
        "people.access",
        &access("kai", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(
        e,
        ("kai is the owner: nothing is boxed for them".to_owned(), 1)
    );
    let e = said(
        &mut rig,
        "people.access",
        &access("*", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.1, 1);
    assert!(e.0.contains("no person '*'"), "{e:?}");
    // a person with no row of their own, and one who may not use the agent now
    let e = said(
        &mut rig,
        "people.access",
        &access("bot-p", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert!(e.0.contains("may not use house now"), "{e:?}");
    let e = said(
        &mut rig,
        "people.access",
        &access("pat", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e, ("pat may not use house now".to_owned(), 1));
    assert_eq!(rig.tables()["members"]["house"]["kai"], Value::Null);
}

#[test]
fn a_secret_or_a_rule_w_place_is_never_granted() {
    let mut rig = rig();
    let home = HOME;
    let secrets = rig.c.auth.parent().unwrap().to_string_lossy().into_owned();
    for (value, why) in [
        (format!("{secrets}/auth.json"), "is a secret"),
        (secrets.clone(), "is a secret"),
        (format!("{home}/.cargo/bin/tool"), "rule W"),
        ("/opt/homebrew/bin/x".to_owned(), "rule W"),
    ] {
        for kind in ["read", "write"] {
            let e = said(
                &mut rig,
                "people.access",
                &access("ann", kind, &value, &json!({})),
            )
            .unwrap_err();
            assert!(e.0.contains(why) && e.1 == 2, "{value}: {e:?}");
        }
    }
    let e = said(
        &mut rig,
        "people.access",
        &access("ann", "read", "relative/path", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e, ("a path must be absolute".to_owned(), 2));
    let e = said(
        &mut rig,
        "people.access",
        &access("ann", "domain", "*.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.1, 2);
    assert_eq!(row(&mut rig, "ann").get("access"), None);
}

#[test]
fn a_bot_is_never_given_more_than_its_owner_holds() {
    let mut rig = rig();
    // sam holds /srv/a for reading and writes /srv/w; the bot gets a part of either
    for (kind, value) in [
        ("read", "/srv/a/deep"),
        ("read", "/srv/w/x"),
        ("write", "/srv/w/y"),
        ("domain", "x.example.com"),
    ] {
        said(
            &mut rig,
            "people.access",
            &access("bot-s", kind, value, &json!({})),
        )
        .unwrap();
    }
    for (kind, value) in [
        ("write", "/srv/a/deep"),
        ("read", "/srv/other"),
        ("domain", "y.example.com"),
    ] {
        let e = said(
            &mut rig,
            "people.access",
            &access("bot-s", kind, value, &json!({})),
        )
        .unwrap_err();
        assert_eq!(
            e.0, "bot-s is a bot: it can't be given more than its owner sam holds",
            "{kind} {value}"
        );
    }
    // the owner's bots are capped by nothing
    said(
        &mut rig,
        "people.access",
        &access("bot-k", "domain", "any.example.com", &json!({})),
    )
    .unwrap();
}

#[test]
fn by_names_only_the_person_or_their_bots_owner_and_only_a_member() {
    let mut rig = rig();
    let p = access("ann", "domain", "a.example.com", &json!({"by": "sam"}));
    let e = said(&mut rig, "people.access", &p).unwrap_err();
    assert_eq!(
        e.0,
        "sam approves only for themselves or their own bot, not for ann"
    );
    // themselves, as a member
    let p = access("ann", "domain", "a.example.com", &json!({"by": "ann"}));
    said(&mut rig, "people.access", &p).unwrap();
    assert_eq!(
        row(&mut rig, "ann")["access_by"]["domain:a.example.com"],
        json!("ann")
    );
    // the owner's yes outlasts a person's; a person's never replaces the owner's
    let p = access("ann", "domain", "a.example.com", &json!({"by": "kai"}));
    said(&mut rig, "people.access", &p).unwrap();
    assert_eq!(
        row(&mut rig, "ann")["access_by"]["domain:a.example.com"],
        Value::Null
    );
    let p = access("ann", "domain", "a.example.com", &json!({"by": "ann"}));
    said(&mut rig, "people.access", &p).unwrap();
    assert_eq!(
        row(&mut rig, "ann")["access_by"]["domain:a.example.com"],
        Value::Null
    );
    // a guest is not a member now: refused in the same step that would write
    let p = access("gus", "domain", "a.example.com", &json!({"by": "gus"}));
    let e = said(&mut rig, "people.access", &p).unwrap_err();
    assert_eq!(e.0, "gus is not a member of house now");
    // a bot's owner approves for it, if a member
    let p = access("bot-s", "domain", "x.example.com", &json!({"by": "sam"}));
    said(&mut rig, "people.access", &p).unwrap();
    assert_eq!(
        row(&mut rig, "bot-s")["access_by"]["domain:x.example.com"],
        json!("sam")
    );
}

#[test]
fn a_removal_takes_the_entry_as_written_from_anyone_a_revoked_person_included() {
    let mut rig = rig();
    let rm = |person: &str, kind: &str, value: &str| {
        access(person, kind, value, &json!({"remove": true}))
    };
    let p = rm("pat", "read", "/srv/old");
    assert_eq!(
        said(&mut rig, "people.access", &p).unwrap(),
        ["pat no longer has read /srv/old beyond the box for house"]
    );
    assert_eq!(row(&mut rig, "pat")["access"]["read"], json!([]));
    // an entry that is not there, a person who is not there: quiet, as the file
    said(&mut rig, "people.access", &rm("nobody", "read", "/srv/old")).unwrap();
    said(&mut rig, "people.access", &rm("pat", "read", "/srv/old")).unwrap();
}

#[test]
fn a_helper_goes_on_the_row_and_a_bot_never_has_one_its_owner_lacks() {
    let mut rig = rig();
    let helper = |person: &str, name: &str, more: &Value| {
        let mut p = json!({"agent": "house", "person": person, "helper": name, "viaSwitch": true, "dies": true});
        for (k, v) in more.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    assert_eq!(
        said(
            &mut rig,
            "people.helper",
            &helper("ann", "garden", &json!({}))
        )
        .unwrap(),
        ["ann's runs of house may call garden"]
    );
    said(
        &mut rig,
        "people.helper",
        &helper("ann", "garden", &json!({})),
    )
    .unwrap();
    assert_eq!(row(&mut rig, "ann")["helpers"], json!(["garden"]));
    assert_eq!(
        said(
            &mut rig,
            "people.helper",
            &helper("ann", "sound", &json!({"once": true}))
        )
        .unwrap(),
        ["ann's run of house may call sound once"]
    );
    assert!(
        row(&mut rig, "ann")["helper_once"]["sound"]
            .as_i64()
            .unwrap()
            > 0
    );
    let e = said(
        &mut rig,
        "people.helper",
        &helper("ann", "nowhere", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e, ("no agent 'nowhere' to call from house".to_owned(), 2));
    let e = said(
        &mut rig,
        "people.helper",
        &helper("ann", "house", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.1, 2);
    let e = said(
        &mut rig,
        "people.helper",
        &helper("kai", "garden", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.0, "kai is the owner: nothing is boxed for them");
    let e = said(
        &mut rig,
        "people.helper",
        &helper("pat", "garden", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.0, "pat may not use house now");
    // sam lists garden, not sound: the bot gets the first only
    said(
        &mut rig,
        "people.helper",
        &helper("bot-s", "garden", &json!({})),
    )
    .unwrap();
    let e = said(
        &mut rig,
        "people.helper",
        &helper("bot-s", "sound", &json!({})),
    )
    .unwrap_err();
    assert_eq!(
        e.0,
        "bot-s is a bot: it can't be given more than its owner sam holds"
    );
    // removal takes it as written, from a revoked person too
    said(
        &mut rig,
        "people.helper",
        &helper("ann", "garden", &json!({"remove": true})),
    )
    .unwrap();
    assert_eq!(row(&mut rig, "ann")["helpers"], json!([]));
    said(
        &mut rig,
        "people.helper",
        &helper("pat", "garden", &json!({"remove": true})),
    )
    .unwrap();
}

#[test]
fn a_job_is_declined_by_its_hash_and_a_tick_goes_as_written() {
    let mut rig = rig();
    let job = |person: &str, name: &str, more: &Value| {
        let mut p = json!({"agent": "house", "person": person, "job": name, "viaSwitch": true, "dies": true});
        for (k, v) in more.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    let decline = |h: &str| json!({"decline": h});
    assert_eq!(
        said(&mut rig, "people.job", &job("sam", "work", &decline("h1"))).unwrap(),
        ["sam's work on house stays as it was ticked"]
    );
    said(&mut rig, "people.job", &job("sam", "work", &decline("h1"))).unwrap();
    said(&mut rig, "people.job", &job("sam", "work", &decline("h2"))).unwrap();
    assert_eq!(
        row(&mut rig, "sam")["jobs"]["work"]["declined"],
        json!(["h1", "h2"])
    );
    let e = said(&mut rig, "people.job", &job("sam", "nope", &decline("h1"))).unwrap_err();
    assert_eq!(
        e.0,
        "house declares no job 'nope' (capabilities.yaml jobs:)"
    );
    assert_eq!(e.1, 2);
    let e = said(&mut rig, "people.job", &job("ann", "work", &decline("h1"))).unwrap_err();
    assert_eq!(e.0, "ann has no tick of work on house");
    let e = said(&mut rig, "people.job", &job("kai", "work", &decline("h1"))).unwrap_err();
    assert_eq!(e.0, "kai is the owner: nothing is boxed for them");
    assert_eq!(
        said(
            &mut rig,
            "people.job",
            &job("sam", "work", &json!({"remove": true}))
        )
        .unwrap(),
        ["sam no longer has the job work for house"]
    );
    assert_eq!(row(&mut rig, "sam")["jobs"], json!({}));
}

#[test]
fn a_job_is_ticked_as_the_owner_saw_it_and_a_second_tick_replaces_the_first() {
    let mut rig = rig();
    let tick = |person: &str, name: &str, shown: Option<&str>| {
        let mut p = json!({"agent": "house", "person": person, "job": name, "viaSwitch": true, "dies": true, "tick": true});
        if let Some(s) = shown {
            p["shown"] = json!(s);
        }
        p
    };
    // no --shown: the job as the package declares it now, the pieces in the order kith_jobs lists them
    assert_eq!(
        said(&mut rig, "people.job", &tick("ann", "work", None)).unwrap(),
        ["ann has the job work for house"]
    );
    let ann = row(&mut rig, "ann");
    assert_eq!(
        ann["jobs"]["work"]["snapshot"],
        json!({"domains": ["a.example"], "helpers": ["garden"], "doors": []})
    );
    assert!(ann["jobs"]["work"]["since"].as_i64().unwrap() > 0);
    // --shown: what the sheet listed, cut to what the package declares now (b.example is not declared)
    said(
        &mut rig,
        "people.job",
        &tick(
            "ann",
            "work",
            Some(r#"{"domains": ["a.example", "b.example"], "helpers": []}"#),
        ),
    )
    .unwrap();
    assert_eq!(
        row(&mut rig, "ann")["jobs"]["work"]["snapshot"],
        json!({"domains": ["a.example"], "helpers": [], "doors": []})
    );
    // a tick again is the same tick: the decline of the one before is gone, as in the file
    let decline = json!({"agent": "house", "person": "sam", "job": "work", "decline": "h1", "viaSwitch": true, "dies": true});
    said(&mut rig, "people.job", &decline).unwrap();
    said(&mut rig, "people.job", &tick("sam", "work", None)).unwrap();
    assert!(
        row(&mut rig, "sam")["jobs"]["work"]
            .get("declined")
            .is_none()
    );
    // the refusals are kith-auth's, in its order: the job, then the person, then the JSON
    let e = said(&mut rig, "people.job", &tick("ann", "nope", None)).unwrap_err();
    assert_eq!(
        e,
        (
            "house declares no job 'nope' (capabilities.yaml jobs:)".to_owned(),
            2
        )
    );
    let e = said(&mut rig, "people.job", &tick("kai", "work", None)).unwrap_err();
    assert_eq!(e.0, "kai is the owner: nothing is boxed for them");
    let e = said(&mut rig, "people.job", &tick("pat", "work", None)).unwrap_err();
    assert_eq!(e.0, "pat may not use house now");
    let before = rig.tables();
    for bad in ["not json", "[1]", "5"] {
        let e = said(&mut rig, "people.job", &tick("ann", "work", Some(bad))).unwrap_err();
        assert_eq!(
            e,
            (
                r#"--shown is JSON: {"domains": [..], "helpers": [..], "doors": [..]}"#.to_owned(),
                2
            )
        );
    }
    assert_eq!(rig.tables(), before, "a refusal writes nothing");
}

#[test]
fn a_refusal_leaves_the_tables_as_they_were_and_an_unmarked_call_is_refused() {
    let mut rig = rig();
    let before = rig.tables();
    // a refusal with `dies` is an answer, and writes nothing
    let e = said(
        &mut rig,
        "people.access",
        &access("kai", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.1, 1);
    assert_eq!(rig.tables(), before);
    // without `dies` it is the refusal of the call
    let mut p = access("kai", "domain", "a.example.com", &json!({}));
    p.as_object_mut().unwrap().remove("dies");
    assert!(
        rig.call("people.access", &p)
            .unwrap_err()
            .contains("is the owner")
    );
    // and none of the three runs without viaSwitch, whatever the phase
    for (method, params) in [
        (
            "people.access",
            json!({"agent": "house", "person": "ann", "kind": "domain", "value": "a.example.com"}),
        ),
        (
            "people.helper",
            json!({"agent": "house", "person": "ann", "helper": "garden"}),
        ),
        (
            "people.job",
            json!({"agent": "house", "person": "sam", "job": "work", "decline": "h"}),
        ),
    ] {
        let e = rig.call(method, &params).unwrap_err();
        assert!(e.contains("needs viaSwitch"), "{method}: {e}");
    }
    assert_eq!(rig.tables(), before);
}

#[test]
fn only_a_removal_goes_through_on_a_damaged_record_and_nothing_when_not_switched() {
    let mut rig = rig();
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    let grant = access("ann", "domain", "a.example.com", &json!({}));
    let rm = access("pat", "read", "/srv/old", &json!({"remove": true}));
    let helper_rm = json!({"agent": "house", "person": "sam", "helper": "garden", "remove": true, "viaSwitch": true});
    // rolled back (no record): everything is refused, removals too
    state::clear(&layout, "authority").unwrap();
    for (m, p) in [
        ("people.access", &grant),
        ("people.access", &rm),
        ("people.helper", &helper_rm),
    ] {
        let e = rig.call(m, p).unwrap_err();
        assert!(e.contains("not switched"), "{m}: {e}");
    }
    // damaged: a grant is refused, a removal goes through
    std::fs::write(state::path(&layout, "authority").unwrap(), "garbage").unwrap();
    let e = rig.call("people.access", &grant).unwrap_err();
    assert!(e.contains("not switched"), "{e}");
    assert_eq!(row(&mut rig, "ann").get("access"), None);
    rig.call("people.access", &rm).unwrap();
    rig.call("people.helper", &helper_rm).unwrap();
    assert_eq!(row(&mut rig, "pat")["access"]["read"], json!([]));
    assert_eq!(row(&mut rig, "sam")["helpers"], json!([]));
}

#[test]
fn the_revision_moves_with_any_write_and_is_a_read_through_the_switch() {
    let mut rig = rig();
    let ask = json!({"viaSwitch": true});
    let digest = |rig: &mut Rig| {
        let r = rig.call("people.revision", &ask).unwrap();
        r["digest"].as_str().unwrap().to_owned()
    };
    let d0 = digest(&mut rig);
    assert_eq!(d0.len(), 16);
    assert_eq!(digest(&mut rig), d0, "a read changes nothing");
    said(
        &mut rig,
        "people.access",
        &access("ann", "domain", "a.example.com", &json!({})),
    )
    .unwrap();
    let d1 = digest(&mut rig);
    assert_ne!(d1, d0);
    said(
        &mut rig,
        "people.access",
        &access("ann", "domain", "a.example.com", &json!({"remove": true})),
    )
    .unwrap();
    assert_ne!(digest(&mut rig), d1, "a removal is a write too");
    // the same fence as every routed read: not switched, no answer
    let layout = Layout::new(&crate::authority::revoke::Life::beside(&rig.c).machine);
    state::clear(&layout, "authority").unwrap();
    let e = rig.call("people.revision", &ask).unwrap_err();
    assert!(e.contains("not switched"), "{e}");
}

#[test]
fn a_refused_card_write_does_not_drop_the_invites_that_ran_out() {
    let mut w = world();
    w["invites"] = json!({"OLD123": {"person": "sam", "expires": 1}});
    let mut rig = Rig::new(&w);
    switched(&rig, Phase::Switched);
    // refused: the owner has no row of their own; kith-auth saves nothing then, so nothing is pruned either
    let e = said(
        &mut rig,
        "people.access",
        &access("kai", "domain", "a.example.com", &json!({})),
    )
    .unwrap_err();
    assert_eq!(e.1, 1);
    assert!(
        rig.tables()["invites"].get("OLD123").is_some(),
        "the refusal pruned"
    );
    // a write that lands saves the file, and with it the prune
    said(
        &mut rig,
        "people.access",
        &access("ann", "domain", "a.example.com", &json!({})),
    )
    .unwrap();
    assert!(rig.tables()["invites"].get("OLD123").is_none());
}
