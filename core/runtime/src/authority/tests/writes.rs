//! The owner's writes (M2-2) against core.db. First the verbs by name, with the words `kith-auth` gives and the refusals
//! it makes; then sequences of writes on generated tables, where core.db must hold exactly what the same verbs leave in
//! the auth.json shape, and the readers of M2-1 must answer the same from it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]

use std::cell::RefCell;

use kith_kernel::secrets::{self, Ctx, Rules};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};

use super::{AGENTS, FUTURE, NOW, auth_strategy, got, ids, machine, pick, said, same};
use crate::authority::call::{call, call_with, write_op};
use crate::authority::load::load;
use crate::authority::write::{self, Settings};
use crate::authority::{Authority, store};
use crate::db::Core;
use crate::error::Error;
use crate::layout::Layout;

/// A core.db on a temp folder and the machine of agents the tests use (`house`, `garden`, `sound`, and the kernel's
/// built-in `scratch`).
struct Rig {
    _core_dir: tempfile::TempDir,
    _machine: tempfile::TempDir,
    core: Core,
    c: Ctx,
}

fn plain(e: &Error) -> String {
    e.to_string().trim_start_matches("authority: ").to_owned()
}

impl Rig {
    fn new(auth: &Value) -> Rig {
        let (core_dir, m) = crate::tests::machine();
        let core = crate::tests::open(&Layout::new(&m));
        load(core.conn(), auth);
        let (machine_dir, c) = machine();
        Rig {
            _core_dir: core_dir,
            _machine: machine_dir,
            core,
            c,
        }
    }

    /// Empties every table of authority and loads `auth`.
    fn reset(&self, auth: &Value) {
        for t in [
            "people",
            "memberships",
            "presets",
            "authority_settings",
            "invites",
            "retired",
        ] {
            self.core
                .conn()
                .execute(&format!("DELETE FROM {t}"), [])
                .unwrap();
        }
        load(self.core.conn(), auth);
    }

    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        call_with(&mut self.core, &self.c, method, params).map_err(|e| plain(&e))
    }

    fn lines(&mut self, method: &str, params: &Value) -> Result<Vec<String>, String> {
        self.call(method, params).map(|v| {
            v["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap().to_owned())
                .collect()
        })
    }

    fn tables(&mut self) -> Value {
        store::read(&mut self.core, Value::clone).unwrap()
    }
}

fn base() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam", "was": ["samuel"]},
            "amy": {},
            "bot-a": {"bot_of": "sam"},
        },
        "presets": {"pro": {"mode": "acceptEdits", "deny": ["Bash"]}},
    })
}

fn grant(person: &str, agent: &str, more: &Value) -> Value {
    let mut p = json!({"person": person, "agent": agent});
    for (k, v) in more.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

// --- grant and revoke ---------------------------------------------------------------------------------------------

#[test]
fn a_grant_says_what_the_person_gets() {
    let mut rig = Rig::new(&base());
    assert_eq!(
        rig.lines(
            "people.grant",
            &grant("sam", "house", &json!({"preset": "member"}))
        )
        .unwrap(),
        ["sam may use house: preset member, mode acceptEdits"]
    );
    assert_eq!(
        rig.lines(
            "people.grant",
            &grant(
                "sam",
                "house,garden",
                &json!({"preset": "pro", "model": "haiku", "deny": "Edit", "helpers": "garden", "budget": "20"})
            )
        )
        .unwrap(),
        [
            "sam may use house: preset pro, mode acceptEdits, deny Bash,Edit, helpers garden, budget $20/month",
            "sam may use garden: preset pro, mode acceptEdits, deny Bash,Edit, helpers garden, budget $20/month",
        ]
    );
    let t = rig.tables();
    let row = &t["members"]["house"]["sam"];
    assert_eq!(row["preset"], "pro");
    assert_eq!(row["model"], "haiku");
    assert_eq!(row["deny"], json!(["Edit"]));
    assert_eq!(row["helpers"], json!(["garden"]));
    assert_eq!(row["budget"], "20");
    assert_eq!(t["members"]["garden"]["sam"], *row);
    // and the readers of M2-1 see it
    let core = Authority::new(rig.core.conn(), &rig.c);
    assert!(core.may("sam", "house", None, false).unwrap().0);
    assert_eq!(
        core.resolve("house", "sam", None, false).unwrap()["budget"],
        "20"
    );
}

#[test]
fn a_grant_changes_only_what_it_names_and_an_empty_value_clears() {
    let mut rig = Rig::new(&base());
    rig.lines(
        "people.grant",
        &grant(
            "sam",
            "house",
            &json!({"preset": "pro", "model": "haiku", "budget": "5"}),
        ),
    )
    .unwrap();
    let said = rig
        .lines(
            "people.grant",
            &grant("sam", "house", &json!({"model": "", "deny": "Edit"})),
        )
        .unwrap();
    assert_eq!(
        said,
        ["sam may use house: preset pro, mode acceptEdits, deny Bash,Edit, budget $5/month"]
    );
    let t = rig.tables();
    let row = &t["members"]["house"]["sam"];
    assert!(row.get("model").is_none(), "{row}");
    assert_eq!(row["preset"], "pro");
    assert_eq!(row["budget"], "5");
    // a budget of "0" is text, so it is kept
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"budget": "0"})),
    )
    .unwrap();
    assert_eq!(rig.tables()["members"]["house"]["sam"]["budget"], "0");
}

#[test]
fn a_free_shell_needs_trusted_and_stays_trusted_only_while_the_mode_is() {
    let mut rig = Rig::new(&base());
    let e = rig
        .lines(
            "people.grant",
            &grant("sam", "house", &json!({"mode": "auto"})),
        )
        .unwrap_err();
    assert_eq!(
        e,
        "sam on house: mode=auto runs any command on this Mac as you, unasked. Add --trusted if you mean it"
    );
    assert!(rig.tables()["members"].get("house").is_none());
    let said = rig
        .lines(
            "people.grant",
            &grant("sam", "house", &json!({"mode": "auto", "trusted": true})),
        )
        .unwrap();
    assert_eq!(
        said,
        ["sam may use house: preset guest, mode auto (a free shell on this Mac: trusted)"]
    );
    assert_eq!(rig.tables()["members"]["house"]["sam"]["trusted"], true);
    // a later mode drops the trust the earlier one had
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"mode": "acceptEdits"})),
    )
    .unwrap();
    assert!(
        rig.tables()["members"]["house"]["sam"]
            .get("trusted")
            .is_none()
    );
}

#[test]
fn a_grant_is_refused_for_who_and_what_kith_auth_refuses() {
    let mut rig = Rig::new(&base());
    let mut refused = |params: Value| rig.lines("people.grant", &params).unwrap_err();
    assert_eq!(
        refused(grant("zed", "house", &json!({}))),
        "no person 'zed' (kith-auth person zed name=..)"
    );
    assert_eq!(
        refused(grant("sam", "nope", &json!({}))),
        "no agent 'nope' (kith ls)"
    );
    assert_eq!(
        refused(grant("sam", "scratch", &json!({}))),
        "scratch is a built-in agent: only the owner runs it, it can't be given to sam"
    );
    assert_eq!(
        refused(grant("*", "scratch", &json!({}))),
        "scratch is a built-in agent: only the owner runs it, it can't be given to everyone"
    );
    assert_eq!(
        refused(grant("sam", "house", &json!({"budget": "-1"}))),
        "budget is dollars a month, e.g. budget=20"
    );
    assert_eq!(
        refused(grant("sam", "house", &json!({"budget": "lots"}))),
        "budget is dollars a month, e.g. budget=20"
    );
    assert_eq!(
        refused(grant("sam", "house", &json!({"preset": "nope"}))),
        "no preset 'nope' (kith-auth preset nope mode=..)"
    );
    // a bot is a guest of its owner's budget
    assert_eq!(
        refused(grant("bot-a", "house", &json!({"preset": "member"}))),
        "bot-a is a bot: its preset is guest"
    );
    assert_eq!(
        refused(grant("bot-a", "house", &json!({"budget": "5"}))),
        "bot-a is a bot: a bot spends from its owner's budget"
    );
    assert_eq!(
        refused(grant("bot-a", "house", &json!({"mode": "acceptEdits"}))),
        "bot-a is a bot: it runs in the guest preset's mode"
    );
    // none of those wrote anything
    assert!(rig.tables()["members"].as_object().unwrap().is_empty());
}

#[test]
fn a_grant_to_several_agents_is_all_or_nothing() {
    let mut rig = Rig::new(&base());
    let e = rig
        .lines("people.grant", &grant("sam", "house,nope", &json!({})))
        .unwrap_err();
    assert_eq!(e, "no agent 'nope' (kith ls)");
    assert!(rig.tables()["members"].as_object().unwrap().is_empty());
    // a usage refusal is a bad request, not a refused state
    let e = call_with(
        &mut rig.core,
        &rig.c,
        "people.grant",
        &grant("sam", "house", &json!({"mode": "auto"})),
    )
    .unwrap_err();
    assert!(matches!(e, Error::BadParams(_)), "{e}");
    let e = call_with(
        &mut rig.core,
        &rig.c,
        "people.grant",
        &grant("zed", "house", &json!({})),
    )
    .unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
}

#[test]
fn a_grant_to_the_owner_a_revoked_person_or_a_bot_without_its_owner_waits() {
    let mut a = base();
    a["people"]["amy"] = json!({"revoked": {"at": 7}});
    let mut rig = Rig::new(&a);
    assert_eq!(
        rig.lines("people.grant", &grant("kai", "house", &json!({})))
            .unwrap(),
        ["kai is the owner: always may use house"]
    );
    // the row is written all the same
    assert_eq!(rig.tables()["members"]["house"]["kai"], json!({}));
    assert_eq!(
        rig.lines("people.grant", &grant("amy", "house", &json!({})))
            .unwrap(),
        ["amy has a row for house, but may not use it now: amy's access was removed"]
    );
    assert_eq!(
        rig.lines("people.grant", &grant("bot-a", "house", &json!({})))
            .unwrap(),
        [
            "bot-a has a row for house, but may not use it now: bot-a may not use house: its owner sam may not"
        ]
    );
    // the owner gets the agent: the waiting row is live
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"preset": "member"})),
    )
    .unwrap();
    let core = Authority::new(rig.core.conn(), &rig.c);
    assert_eq!(
        core.may("bot-a", "house", None, false).unwrap(),
        (true, "bot of sam".to_owned())
    );
    // and granting the bot again now says what it gets
    assert_eq!(
        rig.lines("people.grant", &grant("bot-a", "house", &json!({})))
            .unwrap(),
        ["bot-a may use house: preset guest, mode default"]
    );
}

#[test]
fn everyone_is_a_row_and_needs_no_person() {
    let mut rig = Rig::new(&base());
    assert_eq!(
        rig.lines(
            "people.grant",
            &grant("*", "house", &json!({"preset": "member", "deny": "Bash"}))
        )
        .unwrap(),
        ["everyone kith knows may use house: preset member, mode acceptEdits, deny Bash"]
    );
    let core = Authority::new(rig.core.conn(), &rig.c);
    assert_eq!(
        core.may("amy", "house", None, false).unwrap(),
        (true, "member (everyone)".to_owned())
    );
    // never a bot's
    assert!(!core.may("bot-a", "house", None, false).unwrap().0);
    assert_eq!(
        rig.lines("people.revoke", &grant("*", "house", &json!({})))
            .unwrap(),
        ["* may no longer use house"]
    );
}

#[test]
fn a_revoke_repeats_and_needs_no_person_or_folder() {
    let mut rig = Rig::new(&base());
    rig.lines("people.grant", &grant("sam", "house,garden", &json!({})))
        .unwrap();
    let said = |rig: &mut Rig, person: &str, agent: &str| {
        rig.lines("people.revoke", &grant(person, agent, &json!({})))
            .unwrap()
    };
    assert_eq!(
        said(&mut rig, "sam", "house"),
        ["sam may no longer use house"]
    );
    // again: nothing to take, nothing refused
    assert_eq!(
        said(&mut rig, "sam", "house"),
        ["sam may no longer use house"]
    );
    // a person who is gone, an agent whose folder is gone
    assert_eq!(
        said(&mut rig, "ghost", "nope"),
        ["ghost may no longer use nope"]
    );
    assert_eq!(
        said(&mut rig, "sam", "garden,house"),
        [
            "sam may no longer use garden",
            "sam may no longer use house"
        ]
    );
    // the last row of an agent takes the agent's entry with it
    assert!(rig.tables()["members"].as_object().unwrap().is_empty());
    let core = Authority::new(rig.core.conn(), &rig.c);
    assert!(!core.may("sam", "house", None, false).unwrap().0);
}

#[test]
fn a_revoke_takes_the_always_entries_the_person_approved_with_it() {
    let mut a = base();
    a["members"] = json!({"house": {
        "sam": {"preset": "member",
                "access": {"read": ["/a", "/b"], "write": [], "domains": []},
                "access_by": {"read:/a": "sam", "read:/b": "kai"},
                "access_since": {"read:/a": 5, "read:/b": 6}},
        "bot-a": {"access": {"read": [], "write": [], "domains": ["x.com", "y.com"]},
                  "access_by": {"domain:x.com": "sam", "domain:y.com": "kai"}},
    }});
    let mut rig = Rig::new(&a);
    assert_eq!(
        rig.lines("people.revoke", &grant("sam", "house", &json!({})))
            .unwrap(),
        ["sam may no longer use house (2 always entries they approved went)"]
    );
    let t = rig.tables();
    // the bot keeps what the owner approved; the entry sam approved is gone with its record
    assert_eq!(
        t["members"]["house"]["bot-a"]["access"]["domains"],
        json!(["y.com"])
    );
    assert_eq!(
        t["members"]["house"]["bot-a"]["access_by"],
        json!({"domain:y.com": "kai"})
    );
    assert!(t["members"]["house"].get("sam").is_none());
}

#[test]
fn a_demotion_from_member_takes_the_approved_entries_too() {
    let mut a = base();
    a["members"] = json!({"house": {"sam": {
        "preset": "member",
        "access": {"read": ["/a"], "write": [], "domains": []},
        "access_by": {"read:/a": "sam"},
        "access_since": {"read:/a": 5},
    }}});
    let mut rig = Rig::new(&a);
    assert_eq!(
        rig.lines(
            "people.grant",
            &grant("sam", "house", &json!({"preset": "guest"}))
        )
        .unwrap(),
        [
            "sam is no longer a member of house: 1 always entry they approved went",
            "sam may use house: preset guest, mode default"
        ]
    );
    let t = rig.tables();
    let row = &t["members"]["house"]["sam"];
    assert_eq!(row["access"]["read"], json!([]));
    assert_eq!(row["access_by"], json!({}));
    assert_eq!(row["access_since"], json!({}));
}

#[test]
fn env_settings_are_set_cleared_and_checked() {
    let (_t, c) = machine();
    let mut auth = base();
    let kv = |pairs: &[(&str, &str)]| -> Settings {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    write::member(
        &mut auth,
        &c,
        "house",
        "sam",
        &kv(&[("env.FOO", "1"), ("env.BAR", "2")]),
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        auth["members"]["house"]["sam"]["env"],
        json!({"FOO": "1", "BAR": "2"})
    );
    write::member(
        &mut auth,
        &c,
        "house",
        "sam",
        &kv(&[("env.FOO", "")]),
        false,
        false,
    )
    .unwrap();
    assert_eq!(auth["members"]["house"]["sam"]["env"], json!({"BAR": "2"}));
    write::member(
        &mut auth,
        &c,
        "house",
        "sam",
        &kv(&[("env.BAR", "")]),
        false,
        false,
    )
    .unwrap();
    assert!(auth["members"]["house"]["sam"].get("env").is_none());
    for bad in [
        "PATH", "HOME", "SHELL", "DYLD_X", "LD_X", "KITH_X", "HQ_X", "lower", "1A",
    ] {
        let key = format!("env.{bad}");
        let e = write::member(
            &mut auth,
            &c,
            "house",
            "sam",
            &kv(&[(key.as_str(), "x")]),
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(e.code, 2, "{bad}");
        assert!(
            e.msg
                .starts_with(&format!("env.{bad}: not a variable a membership may set")),
            "{}",
            e.msg
        );
    }
}

// --- bots and the owner's cap -------------------------------------------------------------------------------------

#[test]
fn a_bot_is_added_to_a_person_and_removed_with_its_rows() {
    let mut rig = Rig::new(&base());
    let add = |rig: &mut Rig, p: Value| rig.lines("people.addBot", &p);
    assert_eq!(
        add(
            &mut rig,
            json!({"id": "bot-b", "of": "sam", "name": "Scout", "provider": "telegram"})
        )
        .unwrap(),
        ["bot-b: bot of sam, name Scout, provider telegram"]
    );
    let t = rig.tables();
    assert_eq!(
        t["people"]["bot-b"],
        json!({"bot_of": "sam", "name": "Scout", "provider": "telegram"})
    );
    // again: the same owner is a repeat, and an empty value clears
    assert_eq!(
        add(&mut rig, json!({"id": "bot-b", "of": "samuel", "name": ""})).unwrap(),
        ["bot-b: bot of sam, provider telegram"]
    );
    assert!(rig.tables()["people"]["bot-b"].get("name").is_none());
    // another owner is refused
    assert_eq!(
        add(&mut rig, json!({"id": "bot-b", "of": "amy"})).unwrap_err(),
        "bot-b belongs to sam; remove it (--rm) and add it again to hand it over"
    );
    // the order of people is the order they were added in
    let order: Vec<String> = rig.tables()["people"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(order, ["kai", "sam", "amy", "bot-a", "bot-b"]);
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"preset": "member"})),
    )
    .unwrap();
    rig.lines("people.grant", &grant("bot-b", "house", &json!({})))
        .unwrap();
    assert_eq!(
        rig.lines("people.removeBot", &json!({"id": "bot-b"}))
            .unwrap(),
        ["bot-b removed, with its memberships"]
    );
    let t = rig.tables();
    assert!(t["people"].get("bot-b").is_none());
    assert!(t["members"]["house"].get("bot-b").is_none());
    assert!(t["members"]["house"].get("sam").is_some());
    let order: Vec<String> = t["people"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(order, ["kai", "sam", "amy", "bot-a"]);
    // removing it again, or a person, is refused
    assert_eq!(
        rig.lines("people.removeBot", &json!({"id": "bot-b"}))
            .unwrap_err(),
        "no bot 'bot-b'"
    );
    assert_eq!(
        rig.lines("people.removeBot", &json!({"id": "amy"}))
            .unwrap_err(),
        "no bot 'amy'"
    );
}

#[test]
fn a_bot_is_refused_for_who_and_what_kith_auth_refuses() {
    let mut a = base();
    a["retired"] = json!({"old-bot": {"at": 5}});
    let mut rig = Rig::new(&a);
    let mut refused = |p: Value| rig.lines("people.addBot", &p).unwrap_err();
    assert_eq!(
        refused(json!({"id": "Bot_1", "of": "sam"})),
        "a bot id is lowercase letters, digits and -, e.g. bot-3f9a1c2e"
    );
    assert_eq!(
        refused(json!({"id": "bot-c"})),
        "bot <id> --of <person>: whose bot it is (the owner, or a person kith knows)"
    );
    assert_eq!(
        refused(json!({"id": "bot-c", "of": "zed"})),
        "no person 'zed' (kith-auth people)"
    );
    assert_eq!(
        refused(json!({"id": "bot-c", "of": "bot-a"})),
        "bot-a is a bot: a bot belongs to a person, not to another bot"
    );
    assert_eq!(
        refused(json!({"id": "amy", "of": "sam"})),
        "'amy' is already a person, not a bot"
    );
    assert_eq!(
        refused(json!({"id": "old-bot", "of": "sam"})),
        "'old-bot' was a person who was removed: give the new person another id (e.g. old-bot-2)"
    );
    assert_eq!(
        refused(json!({"id": "samuel", "of": "amy"})),
        "'samuel' is an old id of sam: give the new person another id (e.g. samuel-2)"
    );
}

#[test]
fn a_new_bot_starts_with_nothing_whatever_an_id_left_behind() {
    let mut a = base();
    a["members"] = json!({"house": {"bot-z": {"preset": "guest"}}});
    a["invites"] = json!({"ABCDEF": {"person": "bot-z", "expires": NOW / 1000.0 + 600.0}});
    let mut rig = Rig::new(&a);
    rig.lines("people.addBot", &json!({"id": "bot-z", "of": "sam"}))
        .unwrap();
    let t = rig.tables();
    assert!(t["members"].as_object().unwrap().is_empty(), "{t}");
    assert!(t["invites"].as_object().unwrap().is_empty(), "{t}");
}

#[test]
fn the_cap_on_the_owners_bots_is_set_shown_and_removed() {
    let mut rig = Rig::new(&base());
    assert_eq!(
        rig.lines("people.botsCap", &json!({})).unwrap(),
        ["your bots have no monthly cap"]
    );
    assert_eq!(
        rig.lines("people.botsCap", &json!({"amount": "50"}))
            .unwrap(),
        ["your bots together: at most $50 a month"]
    );
    assert_eq!(rig.tables()["owner_bots_budget"], "50");
    assert_eq!(
        rig.lines("people.botsCap", &json!({})).unwrap(),
        ["your bots together: at most $50 a month"]
    );
    rig.lines("people.grant", &grant("kai", "house", &json!({})))
        .unwrap();
    rig.lines("people.addBot", &json!({"id": "bot-k", "of": "kai"}))
        .unwrap();
    rig.lines("people.grant", &grant("bot-k", "house", &json!({})))
        .unwrap();
    let r = Authority::new(rig.core.conn(), &rig.c)
        .resolve("house", "bot-k", None, false)
        .unwrap();
    assert_eq!(r["budget"], "50", "{r:?}");
    for bad in ["0", "-5", "lots", ""] {
        assert_eq!(
            rig.lines("people.botsCap", &json!({"amount": bad}))
                .unwrap_err(),
            "bots-cap is dollars a month for all your bots together, e.g. bots-cap 50 (or off)",
            "{bad:?}"
        );
    }
    assert_eq!(
        rig.lines("people.botsCap", &json!({"amount": "off"}))
            .unwrap(),
        ["your bots have no monthly cap"]
    );
    assert!(rig.tables().get("owner_bots_budget").is_none());
}

// --- invites ------------------------------------------------------------------------------------------------------

#[test]
fn an_invite_is_made_listed_and_taken_back() {
    let mut rig = Rig::new(&base());
    let made = rig
        .call("people.invite", &json!({"person": "sam", "minutes": 30}))
        .unwrap();
    let code = made["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 6);
    assert!(
        code.chars()
            .all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)),
        "{code}"
    );
    assert_eq!(
        made["lines"],
        json!([
            code,
            format!("sam sends \"/link {code}\" to the kith bot on any platform within 30 minutes")
        ])
    );
    let t = rig.tables();
    assert_eq!(
        t["invites"][&code],
        json!({"person": "sam", "expires": NOW / 1000.0 + 1800.0})
    );
    let rows = rig.call("people.invites", &json!({})).unwrap();
    assert_eq!(
        rows["rows"],
        json!([{"code": code, "person": "sam", "expires": NOW / 1000.0 + 1800.0}])
    );
    // the code is read as `link` reads it: trimmed, any case
    assert_eq!(
        rig.lines(
            "people.removeInvite",
            &json!({"code": format!(" {} ", code.to_lowercase())})
        )
        .unwrap(),
        [format!("{code} removed")]
    );
    assert!(rig.tables()["invites"].as_object().unwrap().is_empty());
    assert_eq!(
        rig.lines("people.removeInvite", &json!({"code": code}))
            .unwrap_err(),
        "that code is unknown or expired; ask for a new one (kith-auth invite <id>)"
    );
}

#[test]
fn an_invite_is_refused_for_who_kith_auth_refuses() {
    let mut a = base();
    a["people"]["amy"] = json!({"revoked": true});
    let mut rig = Rig::new(&a);
    assert_eq!(
        rig.call("people.invite", &json!({"person": "zed"}))
            .unwrap_err(),
        "no person 'zed' (kith-auth person zed name=..)"
    );
    assert_eq!(
        rig.call("people.invite", &json!({"person": "amy"}))
            .unwrap_err(),
        "amy's access was removed: kith-auth person amy --unrevoke first"
    );
    // a bot of a revoked person is cut off with them
    a["people"]["bot-q"] = json!({"bot_of": "amy"});
    let mut rig = Rig::new(&a);
    assert!(
        rig.call("people.invite", &json!({"person": "bot-q"}))
            .is_err()
    );
    // the default is fifteen minutes
    let made = rig
        .call("people.invite", &json!({"person": "sam"}))
        .unwrap();
    let code = made["code"].as_str().unwrap();
    assert_eq!(
        rig.tables()["invites"][code]["expires"],
        NOW / 1000.0 + 900.0
    );
}

#[test]
fn a_write_drops_the_invites_that_ran_out_as_a_save_does_and_a_read_does_not() {
    let mut a = base();
    a["invites"] = json!({
        "OLDONE": {"person": "sam", "expires": NOW / 1000.0 - 1.0},
        "LIVE22": {"person": "sam", "expires": NOW / 1000.0 + 60.0},
    });
    let mut rig = Rig::new(&a);
    // listed: only the live one; stored: both, until something is written
    let rows = rig.call("people.invites", &json!({})).unwrap();
    assert_eq!(rows["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rig.tables()["invites"].as_object().unwrap().len(), 2);
    assert_eq!(
        rig.lines("people.removeInvite", &json!({"code": "OLDONE"}))
            .unwrap_err(),
        "that code is unknown or expired; ask for a new one (kith-auth invite <id>)"
    );
    rig.lines("people.botsCap", &json!({"amount": "10"}))
        .unwrap();
    assert_eq!(
        rig.tables()["invites"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["LIVE22"]
    );
}

// --- presets and the list -----------------------------------------------------------------------------------------

#[test]
fn a_preset_is_defined_changed_and_removed_and_rows_naming_it_wait() {
    let mut rig = Rig::new(&base());
    let preset = |rig: &mut Rig, p: Value| rig.lines("people.preset", &p);
    let json_of = |line: &str, name: &str| -> Value {
        serde_json::from_str(line.strip_prefix(&format!("{name}: ")).unwrap()).unwrap()
    };
    let l = preset(&mut rig, json!({"name": "ro", "deny": "Edit,Write"})).unwrap();
    assert_eq!(json_of(&l[0], "ro"), json!({"deny": ["Edit", "Write"]}));
    let e = preset(&mut rig, json!({"name": "ro", "mode": "bypassPermissions"})).unwrap_err();
    assert_eq!(
        e,
        "preset ro: mode=bypassPermissions runs any command on this Mac as you, unasked. Add --trusted if you mean it"
    );
    let l = preset(
        &mut rig,
        json!({"name": "ro", "mode": "auto", "trusted": true}),
    )
    .unwrap();
    assert_eq!(
        json_of(&l[0], "ro"),
        json!({"deny": ["Edit", "Write"], "mode": "auto", "trusted": true})
    );
    let l = preset(&mut rig, json!({"name": "ro", "mode": "default"})).unwrap();
    assert_eq!(
        json_of(&l[0], "ro"),
        json!({"deny": ["Edit", "Write"], "mode": "default"})
    );
    // the builtins start from their own mode
    let l = preset(&mut rig, json!({"name": "member", "deny": "Bash"})).unwrap();
    assert_eq!(
        json_of(&l[0], "member"),
        json!({"mode": "acceptEdits", "deny": ["Bash"]})
    );
    // a preset with nothing in it is no preset: kith-auth refuses to show it
    assert_eq!(
        preset(&mut rig, json!({"name": "blank"})).unwrap_err(),
        "no preset 'blank' (kith-auth preset blank mode=..)"
    );
    assert!(rig.tables()["presets"].get("blank").is_none());

    // a row naming a preset that is removed waits, refused, until it is defined again
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"preset": "pro"})),
    )
    .unwrap();
    assert_eq!(
        rig.lines("people.removePreset", &json!({"name": "pro"}))
            .unwrap(),
        ["pro: removed"]
    );
    let core = Authority::new(rig.core.conn(), &rig.c);
    let e = core.resolve("house", "sam", None, false).unwrap_err();
    assert!(matches!(e, Error::Refused(_)), "{e}");
    assert!(core.may("sam", "house", None, false).unwrap().0);
    let rows = rig.call("people.list", &json!({"person": "sam"})).unwrap();
    assert_eq!(rows["rows"][0]["allowed"], false);
    assert_eq!(
        rows["rows"][0]["why"],
        "no preset 'pro' (kith-auth preset pro mode=..)"
    );
    // removing a preset that is not there is not a refusal; a builtin shows itself
    assert_eq!(
        rig.lines("people.removePreset", &json!({"name": "pro"}))
            .unwrap(),
        ["pro: removed"]
    );
    let l = rig
        .lines("people.removePreset", &json!({"name": "guest"}))
        .unwrap();
    assert_eq!(json_of(&l[0], "guest"), json!({"mode": "default"}));
}

#[test]
fn the_list_answers_what_the_rules_answer_and_changes_nothing() {
    let mut a = base();
    a["invites"] = json!({"OLDONE": {"person": "sam", "expires": NOW / 1000.0 - 1.0}});
    let mut rig = Rig::new(&a);
    rig.lines(
        "people.grant",
        &grant(
            "sam",
            "house,garden",
            &json!({"preset": "member", "budget": "9"}),
        ),
    )
    .unwrap();
    rig.lines("people.grant", &grant("*", "house", &json!({})))
        .unwrap();
    rig.lines("people.grant", &grant("bot-a", "house", &json!({})))
        .unwrap();
    let before = rig.tables();
    let all = rig.call("people.list", &json!({})).unwrap();
    let rows = all["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    let find = |agent: &str, person: &str| {
        rows.iter()
            .find(|r| r["agent"] == agent && r["person"] == person)
            .unwrap_or_else(|| panic!("{agent}/{person} in {rows:?}"))
            .clone()
    };
    let sam = find("house", "sam");
    assert_eq!(sam["via"], "own");
    assert_eq!(sam["allowed"], true);
    assert_eq!(sam["preset"], "member");
    assert_eq!(sam["mode"], "acceptEdits");
    assert_eq!(sam["budget"], "9");
    let everyone = find("house", "*");
    assert_eq!(everyone["via"], "everyone");
    assert_eq!(everyone["preset"], "guest");
    assert_eq!(find("house", "bot-a")["allowed"], true);
    // filters
    let only = rig
        .call("people.list", &json!({"agent": "garden"}))
        .unwrap();
    assert_eq!(only["rows"].as_array().unwrap().len(), 1);
    let only = rig.call("people.list", &json!({"person": "amy"})).unwrap();
    assert!(only["rows"].as_array().unwrap().is_empty());
    assert_eq!(rig.tables(), before);
}

// --- the handler and the daemon's door ----------------------------------------------------------------------------

#[test]
fn the_handler_takes_the_callers_view_of_where_the_agents_are() {
    let mut rig = Rig::new(&base());
    let root = rig._machine.path().to_path_buf();
    let view = |mut p: Value| {
        p["home"] = root.to_string_lossy().as_ref().into();
        p["kernel"] = root.join("kernel").to_string_lossy().as_ref().into();
        p["env"] = json!({"KITH_AGENTS": root.join("agents").to_string_lossy()});
        p
    };
    let ok = call(
        &mut rig.core,
        "people.grant",
        &view(grant("sam", "house", &json!({"preset": "member"}))),
    )
    .unwrap();
    assert_eq!(
        ok["lines"],
        json!(["sam may use house: preset member, mode acceptEdits"])
    );
    // the kernel's agent is built in only where that kernel is
    let e = call(
        &mut rig.core,
        "people.grant",
        &view(grant("sam", "scratch", &json!({}))),
    )
    .unwrap_err();
    assert_eq!(
        plain(&e),
        "scratch is a built-in agent: only the owner runs it, it can't be given to sam"
    );
    // a method this module does not answer is not served
    let e = call(&mut rig.core, "people.approve", &view(json!({"id": "r1"}))).unwrap_err();
    assert!(matches!(e, Error::NotServed(_)), "{e}");
}

// --- sequences of writes, against the auth.json shape --------------------------------------------------------------

const PERSONS: [&str; 8] = [
    "kai", "sam", "amy", "bot-a", "bot-b", "ghost", "*", "samuel",
];
const AGENT_LISTS: [&str; 7] = [
    "house",
    "garden",
    "house,garden",
    "scratch",
    "nope",
    "sound,nope",
    "sound",
];

#[derive(Clone, Debug)]
struct Op {
    method: &'static str,
    params: Value,
    code: &'static str,
}

fn maybe<T: std::fmt::Debug>(s: impl Strategy<Value = T>) -> impl Strategy<Value = Option<T>> {
    prop::option::weighted(0.5, s)
}

fn put(p: &mut Value, pairs: &[(&str, Option<&str>)]) {
    for (k, v) in pairs {
        if let Some(v) = v {
            p[*k] = (*v).into();
        }
    }
}

fn grant_op() -> impl Strategy<Value = Op> {
    (
        pick(&PERSONS),
        pick(&AGENT_LISTS),
        maybe(pick(&["guest", "member", "pro", "empty", "nope", ""])),
        maybe(pick(&[
            "default",
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "",
        ])),
        maybe(pick(&["7", "20", "0", "-1", "x", ""])),
        maybe(pick(&["Bash", "Edit,Bash", ""])),
        maybe(pick(&["haiku", ""])),
        maybe(pick(&["garden", "house,sound", ""])),
        any::<bool>(),
    )
        .prop_map(
            |(person, agent, preset, mode, budget, deny, model, helpers, trusted)| {
                let mut p = json!({"person": person, "agent": agent, "trusted": trusted});
                put(
                    &mut p,
                    &[
                        ("preset", preset),
                        ("mode", mode),
                        ("budget", budget),
                        ("deny", deny),
                        ("model", model),
                        ("helpers", helpers),
                    ],
                );
                Op {
                    method: "people.grant",
                    params: p,
                    code: "",
                }
            },
        )
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => grant_op(),
        3 => (pick(&PERSONS), pick(&AGENT_LISTS))
            .prop_map(|(person, agent)| Op {
                method: "people.revoke",
                params: json!({"person": person, "agent": agent}),
                code: "",
            }),
        2 => (
            pick(&["bot-a", "bot-b", "bot-c", "sam", "amy", "Bad", "old-bot", "samuel"]),
            maybe(pick(&["sam", "amy", "kai", "bot-a", "samuel", "zed", ""])),
            maybe(pick(&["Scout", ""])),
        )
            .prop_map(|(id, of, name)| {
                let mut p = json!({"id": id});
                put(&mut p, &[("of", of), ("name", name)]);
                Op { method: "people.addBot", params: p, code: "" }
            }),
        1 => pick(&["bot-a", "bot-b", "sam", "ghost"])
            .prop_map(|id| Op { method: "people.removeBot", params: json!({"id": id}), code: "" }),
        1 => pick(&["50", "0", "off", "x", "12.5"])
            .prop_map(|amount| Op { method: "people.botsCap", params: json!({"amount": amount}), code: "" }),
        1 => (pick(&PERSONS), maybe(pick(&[0i64, 15, -5, 30])), pick(&["AAAAAA", "BBBBBB"]))
            .prop_map(|(person, minutes, code)| {
                let mut p = json!({"person": person});
                if let Some(m) = minutes {
                    p["minutes"] = m.into();
                }
                Op { method: "people.invite", params: p, code }
            }),
        1 => pick(&["aaaaaa", "BBBBBB", "CCCCCC"])
            .prop_map(|code| Op { method: "people.removeInvite", params: json!({"code": code}), code: "" }),
        1 => (
            pick(&["pro", "guest", "member", "new", "empty"]),
            maybe(pick(&["default", "auto", "acceptEdits", ""])),
            maybe(pick(&["Bash", "Edit,Bash", ""])),
            any::<bool>(),
        )
            .prop_map(|(name, mode, deny, trusted)| {
                let mut p = json!({"name": name, "trusted": trusted});
                put(&mut p, &[("mode", mode), ("deny", deny)]);
                Op { method: "people.preset", params: p, code: "" }
            }),
        1 => pick(&["pro", "guest", "new"])
            .prop_map(|name| Op { method: "people.removePreset", params: json!({"name": name}), code: "" }),
    ]
}

/// A value as both sides keep it: no agent without rows, every table there. A line that is `name: {json}` compares as
/// JSON (the key order of a preset follows its table, not the file).
fn norm(v: &Value) -> Value {
    let mut v = v.clone();
    write::tables(&mut v);
    if let Some(m) = v["members"].as_object_mut() {
        m.retain(|_, rows| rows.as_object().is_some_and(|r| !r.is_empty()));
    }
    v
}

fn canon(a: &Value) -> Value {
    match a {
        Value::Array(xs) => Value::Array(xs.iter().map(canon).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), canon(x))).collect()),
        Value::String(line) => match line.split_once(": ") {
            Some((name, rest)) => match serde_json::from_str::<Value>(rest) {
                Ok(j) if j.is_object() => json!([name, j]),
                _ => a.clone(),
            },
            None => a.clone(),
        },
        _ => a.clone(),
    }
}

/// Runs the op on the plain value and on core.db: the same answer or the same refusal, then the same tables.
fn step(rig: &mut Rig, model: &mut Value, op: &Op) -> Result<(), String> {
    let mut next = model.clone();
    let want = write_op(op.method, &op.params, &rig.c, &mut next, op.code);
    if want.is_ok() {
        write::tables(&mut next);
        write::prune_invites(&mut next, &rig.c);
        *model = next;
    }
    let have = store::run(&mut rig.core, &rig.c, |a| {
        write_op(op.method, &op.params, &rig.c, a, op.code)
    });
    let (want, have) = (want.map_err(|d| d.msg), have.map_err(|e| plain(&e)));
    if want.as_ref().map(canon) != have.as_ref().map(canon) {
        return Err(format!(
            "{} {}: model {want:?}, core {have:?}",
            op.method, op.params
        ));
    }
    let (m, c) = (norm(model), norm(&rig.tables()));
    if m != c {
        return Err(format!(
            "{} {}: tables differ\nmodel {m}\ncore  {c}",
            op.method, op.params
        ));
    }
    Ok(())
}

#[test]
fn sequences_of_writes_leave_core_db_as_the_same_verbs_leave_the_file() {
    let rig = RefCell::new(Rig::new(&json!({})));
    let mut config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 48;
    }
    let mut runner = TestRunner::new(config);
    runner
        .run(
            &(auth_strategy(), prop::collection::vec(op(), 1..8)),
            |(auth, ops)| {
                let mut rig = rig.borrow_mut();
                rig.reset(&auth);
                let mut model = auth;
                for op in &ops {
                    step(&mut rig, &mut model, op).map_err(TestCaseError::fail)?;
                }
                readers_agree(&rig, &model).map_err(TestCaseError::fail)?;
                same(&model, &rig.c).map_err(TestCaseError::fail)
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
}

/// M2-1's readers over the real core.db, against the rules over the plain value.
fn readers_agree(rig: &Rig, model: &Value) -> Result<(), String> {
    let core = Authority::new(rig.core.conn(), &rig.c);
    for p in ids(model) {
        let (a, b) = (secrets::cut_off(model, &p), core.cut_off(&p).unwrap());
        if a != b {
            return Err(format!("cut_off({p:?}): file {a}, core {b}"));
        }
        for agent in AGENTS {
            for once in [false, true] {
                let (a, b) = (
                    said(Rules::new(&rig.c, model).may(&p, agent, None, once)),
                    got(core.may(&p, agent, None, once)),
                );
                if a != b {
                    return Err(format!(
                        "may({p:?}, {agent:?}, once {once}): file {a:?}, core {b:?}"
                    ));
                }
                let (a, b) = (
                    said(Rules::new(&rig.c, model).resolve(agent, &p, None, once)),
                    got(core.resolve(agent, &p, None, once)),
                );
                if a != b {
                    return Err(format!(
                        "resolve({p:?}, {agent:?}, once {once}): file {a:?}, core {b:?}"
                    ));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn a_helper_once_grant_and_the_far_future_survive_a_write_beside_them() {
    // `helper_once` and `jobs` are not the owner's to write here; a write to the same row must leave them as they were
    let mut a = base();
    a["members"] = json!({"house": {"sam": {"preset": "member", "helper_once": {"garden": FUTURE},
                                            "jobs": {"work": {"snapshot": {"domains": ["a.example"]}}}}}});
    let mut rig = Rig::new(&a);
    rig.lines(
        "people.grant",
        &grant("sam", "house", &json!({"model": "opus"})),
    )
    .unwrap();
    let t = rig.tables();
    let row = &t["members"]["house"]["sam"];
    assert_eq!(row["helper_once"], json!({"garden": FUTURE}));
    assert_eq!(
        row["jobs"],
        json!({"work": {"snapshot": {"domains": ["a.example"]}}})
    );
    assert_eq!(row["model"], "opus");
}
