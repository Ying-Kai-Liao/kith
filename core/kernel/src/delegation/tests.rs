#![allow(clippy::unwrap_used)]

use super::*;
use std::fs;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let dir =
            std::env::temp_dir().join(format!("kith-delegation-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(box_dir(&dir)).unwrap();
        Tmp(dir)
    }

    fn reg(&self, id: &str, text: &str) {
        fs::write(box_dir(&self.0).join(format!("{id}.reg")), text).unwrap();
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn place(hop: i64, cross_hops: i64) -> Place {
    Place {
        chain: None,
        hop,
        cross_hops,
        ask: None,
        cost_base: 0.0,
        crossed: cross_hops > 0,
    }
}

fn why(r: Result<(), Refusal>) -> String {
    r.unwrap_err().0
}

fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |k: &str| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    }
}

#[test]
fn local_hops_stop_after_eight() {
    let none = HashMap::new();
    assert!(decide(&place(0, 0), &none, None).is_ok());
    assert!(decide(&place(8, 0), &none, None).is_ok());
    let w = why(decide(&place(9, 0), &none, None));
    assert!(
        w.contains("already 8 asks deep on this Mac (at most 8)"),
        "{w}"
    );
    // hops that crossed Macs do not count against the local depth
    assert!(decide(&place(10, 2), &none, None).is_ok());
}

#[test]
fn cross_hops_stop_after_two() {
    let none = HashMap::new();
    assert!(decide(&place(2, 2), &none, None).is_ok());
    let w = why(decide(&place(3, 3), &none, None));
    assert!(
        w.contains("crossed to another Mac 2 times (at most 2)"),
        "{w}"
    );
}

#[test]
fn cost_cap_stops_at_the_cap() {
    let mut p = place(1, 0);
    p.chain = Some("c1".into());
    let costs = HashMap::from([("c1".to_owned(), 4.99)]);
    assert!(decide(&p, &costs, Some(5.0)).is_ok());
    let costs = HashMap::from([("c1".to_owned(), 5.0)]);
    let w = why(decide(&p, &costs, Some(5.0)));
    assert_eq!(
        w,
        "delegation cost cap: chain c1 has spent US$5.00 of its US$5 cap; ask the user before starting another ask"
    );
    assert!(decide(&p, &costs, None).is_ok());
    // a chain handed over from another Mac carries its cost: the larger, not the sum
    p.cost_base = 6.0;
    assert!(decide(&p, &HashMap::new(), Some(5.0)).is_err());
    // a root ask has no chain to cost
    assert!(decide(&place(0, 0), &costs, Some(0.5)).is_ok());
}

#[test]
fn old_registrations_read_as_roots() {
    let t = Tmp::new("old");
    t.reg("ask-1", r#"{"session":"s1","done":true}"#);
    t.reg("ask-2", r#"{"asked":"echo"}"#);
    let one = read_reg(&box_dir(&t.0), "ask-1").unwrap();
    assert_eq!(one["chain"], "ask-1");
    assert_eq!(one["hop"], 0);
    assert_eq!(one["cost"], Value::Null);
    assert_eq!(one["state"], "answered");
    assert_eq!(one["reply_to"], json!({"session": "s1", "ask": null}));
    assert!(!one.contains_key("session"));
    assert_eq!(read_reg(&box_dir(&t.0), "ask-2").unwrap()["state"], "open");
}

#[test]
fn damaged_records_are_left_out() {
    let t = Tmp::new("damaged");
    t.reg("a", "{not json");
    t.reg("b", "[1]");
    t.reg("c", r#"{"chain":"c","hop":1,"cost":1.5}"#);
    assert!(read_reg(&box_dir(&t.0), "a").is_none());
    assert!(read_reg(&box_dir(&t.0), "b").is_none());
    assert!(read_reg(&box_dir(&t.0), "missing").is_none());
    let recs = records(&box_dir(&t.0));
    assert_eq!(recs.keys().collect::<Vec<_>>(), ["c"]);
    assert_eq!(chain_costs(&recs)["c"], 1.5);
}

#[test]
fn chain_cost_is_base_plus_runs() {
    let t = Tmp::new("costs");
    t.reg("a", r#"{"chain":"x","cost":1.0,"cost_base":2.0}"#);
    t.reg("b", r#"{"chain":"x","cost":0.5,"cost_base":3.0}"#);
    t.reg("c", r#"{"chain":"y"}"#);
    let costs = chain_costs(&records(&box_dir(&t.0)));
    assert_eq!(costs["x"], 4.5);
    assert_eq!(costs["y"], 0.0);
}

#[test]
fn lineage_follows_the_parent_or_the_handed_chain() {
    let t = Tmp::new("lineage");
    t.reg("p", r#"{"chain":"root","hop":3,"cross_hops":1}"#);
    let dir = box_dir(&t.0);
    let got = lineage(&dir, &env_of(&[("KITH_ASK_ID", "p")]), true).unwrap();
    assert_eq!(
        (
            got.chain.as_deref(),
            got.hop,
            got.cross_hops,
            got.ask.as_deref()
        ),
        (Some("root"), 4, 2, Some("p"))
    );

    let handed = env_of(&[
        ("KITH_ASK_CHAIN", "c9"),
        ("KITH_ASK_HOP", "2"),
        ("KITH_ASK_COST", "1.25"),
        ("KITH_ASK_FROM", "z"),
    ]);
    let got = lineage(&dir, &handed, false).unwrap();
    assert_eq!(
        (got.hop, got.cross_hops, got.cost_base, got.crossed),
        (2, 1, 1.25, true)
    );

    let bad = env_of(&[("KITH_ASK_CHAIN", "c9"), ("KITH_ASK_HOP", "x")]);
    assert!(
        lineage(&dir, &bad, false)
            .unwrap_err()
            .0
            .ends_with("is not numbers")
    );
    for hop in [
        "-1",
        "65",
        "99999999999999999999999999999999999999999999999",
    ] {
        let get = move |k: &str| match k {
            "KITH_ASK_CHAIN" => Some("c9".to_owned()),
            "KITH_ASK_HOP" => Some(hop.to_owned()),
            _ => None,
        };
        assert!(
            lineage(&dir, &get, false)
                .unwrap_err()
                .0
                .contains("impossible"),
            "{hop}"
        );
    }
    for cost in ["nan", "inf", "-1", "1e9"] {
        let get = move |k: &str| match k {
            "KITH_ASK_CHAIN" => Some("c9".to_owned()),
            "KITH_ASK_COST" => Some(cost.to_owned()),
            _ => None,
        };
        assert!(
            lineage(&dir, &get, false)
                .unwrap_err()
                .0
                .contains("impossible"),
            "{cost}"
        );
    }

    let root = lineage(&dir, &env_of(&[]), true).unwrap();
    assert_eq!(
        (root.chain, root.hop, root.cross_hops, root.crossed),
        (None, 0, 1, true)
    );
}

#[test]
fn the_cap_is_five_unless_settings_say() {
    let t = Tmp::new("cap");
    assert_eq!(chain_cost_cap(&t.0), Some(5.0));
    let set = |text: &str| fs::write(t.0.join("settings.json"), text).unwrap();
    set(r#"{"delegation":{"chain_cost_cap_usd":12.5}}"#);
    assert_eq!(chain_cost_cap(&t.0), Some(12.5));
    set(r#"{"delegation":{"chain_cost_cap_usd":"none"}}"#);
    assert_eq!(chain_cost_cap(&t.0), None);
    // a file that can't be used never lifts the cap
    set(r#"{"delegation":{"chain_cost_cap_usd":0}}"#);
    assert_eq!(chain_cost_cap(&t.0), Some(5.0));
    set("{broken");
    assert_eq!(chain_cost_cap(&t.0), Some(5.0));
}

#[test]
fn pct_g_prints_like_python() {
    for (x, want) in [
        (5.0, "5"),
        (2.5, "2.5"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (123_456.0, "123456"),
        (1_234_567.0, "1.23457e+06"),
        (0.1, "0.1"),
    ] {
        assert_eq!(pct_g(x), want, "{x}");
    }
}

#[test]
fn parked_is_the_marker_file() {
    let t = Tmp::new("parked");
    assert!(!parked(&t.0, "ask-1"));
    fs::write(box_dir(&t.0).join("ask-1.parked"), "").unwrap();
    assert!(parked(&t.0, "ask-1"));
    assert!(!parked(&t.0, "ask-2"));
}

#[test]
fn a_check_answers_with_the_record_it_would_get() {
    let t = Tmp::new("check");
    t.reg("p", r#"{"chain":"root","hop":8,"cross_hops":0,"cost":1.0}"#);
    let err = check(&t.0, &env_of(&[("KITH_ASK_ID", "p")]), "new", false).unwrap_err();
    assert!(err.0.contains("already 8 asks deep"), "{}", err.0);
    let ok = check(&t.0, &env_of(&[]), "new", false).unwrap();
    assert_eq!(ok["chain"], "new");
    assert_eq!(ok["hop"], 0);
}

#[test]
fn a_box_lock_that_cannot_be_taken_refuses_the_ask() {
    let t = Tmp::new("nolock");
    // "live" is a file, so the box folder cannot be made and the lock cannot be taken.
    fs::remove_dir_all(t.0.join("live")).unwrap();
    fs::write(t.0.join("live"), "not a folder").unwrap();
    let err = check(&t.0, &env_of(&[]), "new", false).unwrap_err();
    assert!(err.0.contains("can't lock the ask box"), "{}", err.0);
}

#[test]
fn check_and_write_writes_only_an_admitted_ask_and_inside_the_check() {
    let t = Tmp::new("cw");
    t.reg("p", r#"{"chain":"root","hop":8,"cross_hops":0,"cost":1.0}"#);
    let mut wrote = false;
    let refused = check_and_write(&t.0, &env_of(&[("KITH_ASK_ID", "p")]), "new", false, |_| {
        wrote = true;
        Ok(())
    });
    assert!(refused.is_err() && !wrote, "a refused ask writes nothing");
    let ok = check_and_write(&t.0, &env_of(&[]), "new", false, |d| {
        wrote = d["chain"] == "new";
        Ok(())
    })
    .unwrap();
    assert!(wrote);
    assert_eq!(ok["hop"], 0);
    let failed = check_and_write(&t.0, &env_of(&[]), "new", false, |_| {
        Err(std::io::Error::other("disk full"))
    });
    assert!(failed.unwrap_err().0.contains("disk full"));
}
