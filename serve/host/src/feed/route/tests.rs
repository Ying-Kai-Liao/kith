use super::*;

fn out(agents: &[&str]) -> Vec<u8> {
    let rows: Vec<Value> = agents
        .iter()
        .map(|a| json!({"agent": a, "score": 1.0, "matched": [{"kind": "skill", "text": format!("{a} does it")}]}))
        .collect();
    serde_json::to_vec(&rows).unwrap()
}

#[test]
fn the_best_agent_the_owner_has_wins_and_the_rest_follow() {
    let got = pick(Some(&out(&["ghost", "video", "sound"])), |a| a != "ghost");
    assert_eq!(got, json!({"agent": "video", "why": "video does it", "others": ["sound"]}));
}

#[test]
fn no_answer_no_match_or_a_broken_answer_goes_to_main() {
    let home = json!({"agent": "main", "why": "", "others": []});
    assert_eq!(pick(None, |_| true), home);
    assert_eq!(pick(Some(b"[]"), |_| true), home);
    assert_eq!(pick(Some(b"not json"), |_| true), home);
    assert_eq!(pick(Some(&out(&["ghost"])), |_| false), home);
}

#[tokio::test]
async fn with_no_kith_to_ask_the_route_is_main() {
    assert_eq!(route("anything").await["agent"], "main");
}
