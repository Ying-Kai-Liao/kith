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
    json!({"person": id, "owner": false, "allowed": true, "why": "", "mode": mode, "trusted": false,
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
        let owner = json!({"person": "", "owner": true, "allowed": true});
        assert_eq!(
            room_grant("scout", &[owner, a]).unwrap().mode,
            "acceptEdits"
        );
    }
}

#[test]
fn trusted_is_and_and_owner_limits_nothing() {
    let mut a = guest("a", "default", &[], &["h"], &["/p"], &[], &["a.com"]);
    a["trusted"] = json!(true);
    let b = guest("b", "default", &[], &["h"], &["/p"], &[], &["a.com"]);
    assert!(!room_grant("s", &[a.clone(), b]).unwrap().trusted);
    let owner = json!({"person": "", "owner": true, "allowed": true});
    let g = room_grant("s", &[a, owner]).unwrap();
    assert!(g.trusted);
    assert_eq!(
        (g.helpers, g.read, g.domains),
        (
            vec!["h".to_owned()],
            vec!["/p".to_owned()],
            vec!["a.com".to_owned()]
        )
    );
}

#[test]
fn missing_fields_read_as_narrowest() {
    let a = guest("a", "default", &["D"], &["h"], &["/p"], &[], &["a.com"]);
    let bare = json!({"person": "b", "allowed": true, "mode": "acceptEdits"});
    let g = room_grant("s", &[a, bare]).unwrap();
    assert!(g.helpers.is_empty() && g.read.is_empty() && g.domains.is_empty());
    assert_eq!(g.deny, ["D"]);
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
    let mut v = json!({"person": format!("p{i}"), "owner": false, "allowed": true,
        "mode": modes[r.next(4)], "trusted": r.next(2) == 0,
        "deny": r.pick(&["d1", "d2", "d3"], 3), "helpers": r.pick(&["h1", "h2", "h3"], 3),
        "access": {"read": r.pick(&PATHS, 3), "write": r.pick(&PATHS, 2), "domains": r.pick(&["a.com", "b.com", "c.com"], 3)}});
    if r.next(8) == 0 {
        v.as_object_mut().unwrap().remove("helpers");
    }
    if r.next(8) == 0 {
        v.as_object_mut().unwrap().remove("access");
    }
    v
}

fn list(v: &Value, k: &str) -> Vec<String> {
    strings(v.get(k))
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
