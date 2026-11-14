use super::*;
use serde_json::json;

fn guest(
    id: &str,
    mode: &str,
    deny: &[&str],
    helpers: &[&str],
    read: &[&str],
    write: &[&str],
    domains: &[&str],
) -> Value {
    json!({"person": id, "owner": false, "allowed": true, "held": false, "why": "", "mode": mode, "trusted": false,
           "deny": deny, "helpers": helpers, "model": "x", "env": {"A": "1"}, "jobs": ["j"], "doors": ["d@1"],
           "access": {"read": read, "write": write, "domains": domains}})
}

fn held(have: &[String], write: &[String], path: &str) -> bool {
    have.iter()
        .chain(write)
        .any(|r| path == r || path.starts_with(&format!("{}/", r.trim_end_matches('/'))))
}

#[test]
fn two_guests_meet() {
    let a = guest(
        "a",
        "acceptEdits",
        &["Bash(rm:*)"],
        &["h1", "h2"],
        &["/p/x"],
        &["/p/w"],
        &["a.com", "b.com"],
    );
    let b = guest(
        "b",
        "default",
        &["WebFetch"],
        &["h2"],
        &["/p"],
        &[],
        &["b.com"],
    );
    let g = room_grant("scout", &[a, b]).unwrap();
    assert_eq!(g.mode, "default");
    assert_eq!(g.deny, ["Bash(rm:*)", "WebFetch"]);
    assert_eq!(g.helpers, ["h2"]);
    assert_eq!(g.domains, ["b.com"]);
    assert_eq!(g.read, ["/p/w", "/p/x"]);
    assert!(g.write.is_empty() && g.jobs.is_empty() && g.doors.is_empty() && g.env.is_empty());
    assert!(g.model.is_empty() && !g.trusted);
}

#[test]
fn empty_basis_refuses() {
    assert_eq!(room_grant("scout", &[]), Err(Refusal::NoBasis));
}

#[test]
fn one_bad_answer_refuses_all() {
    let ok = guest("a", "default", &[], &[], &[], &[], &[]);
    for bad in [
        json!({"person": "b", "allowed": false, "why": "revoked"}),
        json!({"person": "b", "allowed": true, "held": true, "mode": "default"}),
        json!({"person": "b", "mode": "default"}),
        json!("not an object"),
        json!(null),
        guest("b", "plan", &[], &[], &[], &[], &[]),
    ] {
        assert!(
            room_grant("scout", &[ok.clone(), bad.clone()]).is_err(),
            "{bad}"
        );
        assert!(room_grant("scout", &[bad, ok.clone()]).is_err());
    }
}

#[test]
fn free_shell_is_never_the_meet() {
    for m in ["bypassPermissions", "auto"] {
        let mut a = guest("a", m, &[], &[], &[], &[], &[]);
        a["trusted"] = json!(true);
        let g = room_grant("scout", &[a.clone()]).unwrap();
        assert_eq!(g.mode, "acceptEdits");
        assert!(g.trusted);
    }
}

#[test]
fn trusted_is_and() {
    let mut a = guest("a", "default", &[], &["h"], &["/p"], &[], &["a.com"]);
    a["trusted"] = json!(true);
    let b = guest("b", "default", &[], &["h"], &["/p"], &[], &["a.com"]);
    assert!(room_grant("s", &[a.clone()]).unwrap().trusted);
    assert!(!room_grant("s", &[a, b]).unwrap().trusted);
}

#[test]
fn the_owner_is_not_a_room_person() {
    let a = guest("a", "default", &[], &["h"], &["/p"], &[], &["a.com"]);
    let owner = json!({"person": "", "owner": true, "allowed": true, "held": false});
    let named = json!({"person": "kai", "owner": true, "allowed": true, "held": false});
    let mut empty_id = a.clone();
    empty_id["person"] = json!("");
    for bad in [owner, named, empty_id] {
        assert!(room_grant("s", &[a.clone(), bad.clone()]).is_err(), "{bad}");
        assert!(room_grant("s", &[bad]).is_err());
    }
}

#[test]
fn held_must_be_exactly_false() {
    for held in [json!(true), json!(null), json!("false"), json!(0)] {
        let mut a = guest("a", "default", &[], &[], &[], &[], &[]);
        a["held"] = held;
        assert!(room_grant("s", &[a]).is_err());
    }
    let mut a = guest("a", "default", &[], &[], &[], &[], &[]);
    a.as_object_mut().unwrap().remove("held");
    assert!(room_grant("s", &[a]).is_err());
}

#[test]
fn a_missing_or_mistyped_field_refuses() {
    let ok = || guest("a", "default", &["D"], &["h"], &["/p"], &["/w"], &["a.com"]);
    assert!(room_grant("s", &[ok()]).is_ok());
    for k in ["mode", "deny", "helpers", "access", "trusted"] {
        let mut a = ok();
        a.as_object_mut().unwrap().remove(k);
        assert!(room_grant("s", &[a]).is_err(), "missing {k}");
    }
    for k in ["read", "write", "domains"] {
        let mut a = ok();
        a["access"].as_object_mut().unwrap().remove(k);
        assert!(room_grant("s", &[a]).is_err(), "missing access.{k}");
    }
    let wrong = [
        ("mode", json!(1)),
        ("mode", json!("")),
        ("deny", json!("D")),
        ("deny", json!(null)),
        ("helpers", json!({})),
        ("access", json!([])),
        ("trusted", json!("true")),
        ("trusted", json!(null)),
    ];
    for (k, v) in wrong {
        let mut a = ok();
        a[k] = v;
        assert!(room_grant("s", &[a]).is_err(), "wrong {k}");
    }
    for k in ["read", "write", "domains"] {
        let mut a = ok();
        a["access"][k] = json!("x");
        assert!(room_grant("s", &[a]).is_err(), "wrong access.{k}");
    }
}

#[test]
fn a_non_string_entry_refuses_instead_of_being_dropped() {
    let mut a = guest("a", "default", &["D"], &["h"], &["/p"], &[], &["a.com"]);
    a["deny"] = json!(["D", 7]);
    assert!(room_grant("s", &[a]).is_err());
    for k in ["read", "write", "domains"] {
        let mut a = guest("a", "default", &[], &[], &[], &[], &[]);
        a["access"][k] = json!([null]);
        assert!(room_grant("s", &[a]).is_err(), "access.{k}");
    }
    let mut a = guest("a", "default", &[], &[], &[], &[], &[]);
    a["helpers"] = json!([["h"]]);
    assert!(room_grant("s", &[a]).is_err());
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n
    }
    fn pick(&mut self, from: &[&str], max: usize) -> Vec<String> {
        let mut v: Vec<String> = (0..self.next(max + 1))
            .map(|_| from[self.next(from.len())].to_owned())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

const PATHS: [&str; 7] = ["/p", "/p/a", "/p/a/b", "/p/c", "/q", "/q/r", "/pp"];

fn random_answer(r: &mut Rng, i: usize) -> Value {
    let modes = ["default", "acceptEdits", "bypassPermissions", "auto"];
    let v = json!({"person": format!("p{i}"), "owner": false, "allowed": true, "held": false,
        "mode": modes[r.next(4)], "trusted": r.next(2) == 0,
        "deny": r.pick(&["d1", "d2", "d3"], 3), "helpers": r.pick(&["h1", "h2", "h3"], 3),
        "access": {"read": r.pick(&PATHS, 3), "write": r.pick(&PATHS, 2), "domains": r.pick(&["a.com", "b.com", "c.com"], 3)}});
    v
}

fn list(v: &Value, k: &str) -> Vec<String> {
    strings(v.get(k)).unwrap_or_default()
}

#[test]
fn never_wider_than_any_person_and_order_free() {
    let mut r = Rng(7);
    for _ in 0..2000 {
        let n = 1 + r.next(4);
        let basis: Vec<Value> = (0..n).map(|i| random_answer(&mut r, i)).collect();
        let g = room_grant("s", &basis).unwrap();
        assert!(!FREE_SHELL.contains(&g.mode.as_str()));
        let mut rev = basis.clone();
        rev.reverse();
        assert_eq!(room_grant("s", &rev).unwrap(), g);
        rev.rotate_left(1);
        assert_eq!(room_grant("s", &rev).unwrap(), g);
        for p in &basis {
            assert!(rank(&g.mode).unwrap() <= rank(p["mode"].as_str().unwrap()).unwrap());
            for d in list(p, "deny") {
                assert!(g.deny.contains(&d));
            }
            let helpers = list(p, "helpers");
            assert!(g.helpers.iter().all(|h| helpers.contains(h)));
            assert!(!g.trusted || p["trusted"] == json!(true));
            let access = p.get("access").cloned().unwrap_or(json!({}));
            let domains = list(&access, "domains");
            assert!(g.domains.iter().all(|d| domains.contains(d)));
            let (pw, pr) = (list(&access, "write"), list(&access, "read"));
            for path in &g.write {
                assert!(held(&pw, &[], path), "write {path} of {basis:?}");
            }
            for path in &g.read {
                assert!(held(&pr, &pw, path), "read {path} of {basis:?}");
            }
        }
        // the meet of one person is that person's own
        if n == 1 {
            assert_eq!(g.helpers, list(&basis[0], "helpers"));
        }
    }
}

#[test]
fn only_a_room_ids_are_room_persons() {
    assert!(is_room_person("room-grp1-9"));
    assert!(!is_room_person("sam") && !is_room_person("") && !is_room_person("a-room-x"));
}

#[test]
fn the_launch_of_a_room_person_refuses_before_asking_anyone_when_it_has_no_room_or_no_basis_or_a_stranger_asked()
 {
    let ctx = secrets::Ctx::from_process(std::path::Path::new("/nonexistent"));
    // No room named, no people named, or an author who is not one of the people: no launch, and nobody is resolved.
    let no_room = launch_answer(&ctx, "scout", "room-g-1", "", "ann", "");
    assert_eq!(no_room, Err(Refusal::NoBasis));
    let no_people = launch_answer(&ctx, "scout", "room-g-1", "g", " , ", "");
    assert_eq!(no_people, Err(Refusal::NoBasis));
    let stranger = launch_answer(&ctx, "scout", "room-g-1", "g", "ann,ben", "cy");
    assert!(
        matches!(stranger, Err(Refusal::Person { ref person, .. }) if person == "cy"),
        "{stranger:?}"
    );
}
