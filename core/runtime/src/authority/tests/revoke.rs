//! The person-level writes (M2-4) against core.db: adding, renaming and removing a person, suspending one (revoke) and
//! undoing it, the ids a removal retires. The named cases first, then the property the rest rests on: after a
//! suspension M2-1's readers refuse the person and every bot of theirs, for every agent, and after undo they answer
//! exactly as they did before it. The oracle is not the code under test: it is the readers' own answers from before
//! (a snapshot taken through the same readers, compared after), the people the suspension must reach found by walking
//! the generated `bot_of` links here, and the tables as raw values.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};

use super::writes::{Rig, base};
use super::{AGENTS, auth_strategy, got};
use crate::authority::Authority;
use crate::authority::call::call_with;
use crate::error::Error;

/// A world with a person who has bots, rows on two agents, a `helper_once`, an invite and the `*` row.
fn world() -> Value {
    json!({
        "people": {
            "kai": {"owner": true},
            "sam": {"name": "Sam", "was": ["samuel"]},
            "amy": {},
            "bot-a": {"bot_of": "sam"},
            "bot-b": {"bot_of": "amy"},
        },
        "presets": {"pro": {"mode": "acceptEdits", "deny": ["Bash"]}},
        "members": {
            "house": {
                "*": {"preset": "guest"},
                "sam": {"preset": "pro", "helpers": ["garden"], "helper_once": {"garden": super::FUTURE}},
                "amy": {"preset": "member"},
                "bot-a": {"preset": "guest"},
                "bot-b": {"preset": "guest"},
            },
            "garden": {
                "sam": {"preset": "member"},
                "bot-a": {"preset": "guest"},
            },
        },
        "invites": {"ABCDEF": {"person": "sam", "expires": 4_000_000_000.0}},
    })
}

fn id(person: &str) -> Value {
    json!({ "id": person })
}

fn may(rig: &Rig, person: &str, agent: &str) -> bool {
    Authority::new(rig.core.conn(), &rig.c)
        .may(person, agent, None, false)
        .is_ok_and(|(ok, _)| ok)
}

fn refused(rig: &mut Rig, method: &str, params: &Value) -> String {
    rig.call(method, params).unwrap_err()
}

// --- suspend and undo ------------------------------------------------------------------------------------------------

#[test]
fn a_suspension_is_one_fact_and_the_person_and_their_bots_reach_nothing() {
    let mut rig = Rig::new(&world());
    assert!(may(&rig, "sam", "house") && may(&rig, "bot-a", "garden"));
    let before = rig.tables();
    assert_eq!(
        rig.lines("people.suspend", &id("sam")).unwrap(),
        [
            "sam: access removed; their rows, accounts and bots (bot-a, dormant) are kept (undo: kith-auth person sam --unrevoke)"
        ]
    );
    let t = rig.tables();
    assert_eq!(t["people"]["sam"]["revoked"], json!({"at": 1_800_000_000}));
    // nothing but that one fact changed: rows, presets, invites and the other people are as they were
    for k in ["members", "presets", "invites", "retired"] {
        assert_eq!(t[k], before[k], "{k}");
    }
    assert_eq!(t["people"]["amy"], before["people"]["amy"]);
    assert_eq!(t["people"]["bot-a"], before["people"]["bot-a"]);
    // sam, their bot and the `*` row's reach through them: nothing, on any agent
    for agent in ["house", "garden", "sound", "scratch", "nope"] {
        assert!(!may(&rig, "sam", agent), "sam on {agent}");
        assert!(!may(&rig, "bot-a", agent), "bot-a on {agent}");
        assert!(!may(&rig, "samuel", agent), "the old id on {agent}");
    }
    let core = Authority::new(rig.core.conn(), &rig.c);
    assert!(core.cut_off("sam").unwrap() && core.cut_off("bot-a").unwrap());
    assert!(
        core.resolve("house", "sam", None, false)
            .is_ok_and(|r| r["allowed"] != json!(true))
    );
    assert!(
        core.resolve_bot("house", "bot-a", None, false)
            .is_ok_and(|r| r.is_some_and(|r| r["allowed"] != json!(true)))
    );
    // the others are not touched
    assert!(may(&rig, "amy", "house") && may(&rig, "bot-b", "house"));
    assert!(!core.cut_off("amy").unwrap() && !core.cut_off("bot-b").unwrap());

    assert_eq!(
        rig.lines("people.undo", &id("sam")).unwrap(),
        ["sam: access back as it was, bots too (bot-a)"]
    );
    assert_eq!(rig.tables(), before, "undo leaves the tables as they were");
    assert!(may(&rig, "sam", "house") && may(&rig, "bot-a", "garden"));
}

#[test]
fn a_helper_once_and_a_temp_grant_come_back_with_the_person() {
    let mut rig = Rig::new(&world());
    let once = |rig: &Rig| {
        let core = Authority::new(rig.core.conn(), &rig.c);
        got(core.may("sam", "garden", Some("house"), true))
    };
    let before = once(&rig);
    rig.lines("people.suspend", &id("sam")).unwrap();
    assert!(once(&rig).is_ok_and(|(ok, _)| !ok), "cut off beats a once");
    rig.lines("people.undo", &id("sam")).unwrap();
    assert_eq!(once(&rig), before);
    let row = rig.tables()["members"]["house"]["sam"].clone();
    assert_eq!(row["helper_once"], json!({"garden": super::FUTURE}));
}

#[test]
fn a_second_suspension_and_a_second_undo_change_nothing() {
    let mut rig = Rig::new(&world());
    rig.lines("people.suspend", &id("sam")).unwrap();
    // the first moment stays
    rig.core
        .conn()
        .execute("UPDATE people SET revoked_at = 5 WHERE id = 'sam'", [])
        .unwrap();
    let once = rig.tables();
    rig.lines("people.suspend", &id("sam")).unwrap();
    assert_eq!(rig.tables(), once);
    rig.lines("people.undo", &id("sam")).unwrap();
    let undone = rig.tables();
    assert!(undone["people"]["sam"].get("revoked").is_none());
    // an undo with nothing to undo says so the way kith-auth does and writes nothing
    assert_eq!(
        rig.lines("people.undo", &id("sam")).unwrap(),
        ["sam: access back as it was, bots too (bot-a)"]
    );
    assert_eq!(rig.tables(), undone);
    assert_eq!(
        rig.lines("people.undo", &id("amy")).unwrap(),
        ["amy: access back as it was, bots too (bot-b)"]
    );
    assert_eq!(rig.tables(), undone);
}

#[test]
fn a_suspension_by_an_old_id_reaches_the_person() {
    let mut rig = Rig::new(&world());
    let said = rig.lines("people.suspend", &id("samuel")).unwrap();
    assert!(said[0].starts_with("sam: access removed"), "{said:?}");
    assert!(!may(&rig, "sam", "house"));
    rig.lines("people.undo", &id("samuel")).unwrap();
    assert!(may(&rig, "sam", "house"));
}

#[test]
fn the_owner_a_bot_and_nobody_cannot_be_suspended() {
    let mut rig = Rig::new(&world());
    let before = rig.tables();
    assert_eq!(
        refused(&mut rig, "people.suspend", &id("kai")),
        "kai is the owner: the owner can not be revoked"
    );
    assert_eq!(
        refused(&mut rig, "people.suspend", &id("bot-a")),
        "bot-a is a bot: revoke its owner sam (its bots go dormant), or remove it (kith-auth bot bot-a --rm)"
    );
    assert_eq!(
        refused(&mut rig, "people.suspend", &id("ghost")),
        "no person 'ghost' (kith-auth person ghost name=..)"
    );
    assert_eq!(
        refused(&mut rig, "people.undo", &id("ghost")),
        "no person 'ghost' (kith-auth person ghost name=..)"
    );
    assert!(refused(&mut rig, "people.suspend", &id("Bad Id")).contains("lowercase letters"));
    assert!(matches!(
        call_with(&mut rig.core, &rig.c, "people.suspend", &id("Bad Id")),
        Err(Error::BadParams(_))
    ));
    assert!(matches!(
        call_with(&mut rig.core, &rig.c, "people.suspend", &id("kai")),
        Err(Error::Refused(_))
    ));
    assert_eq!(rig.tables(), before);
}

#[test]
fn while_suspended_the_owner_can_take_a_row_away_but_not_give_one_and_undo_keeps_the_change() {
    let mut rig = Rig::new(&world());
    rig.lines("people.suspend", &id("sam")).unwrap();
    // a grant waits: the row is written and says so, and they still reach nothing
    let said = rig
        .lines(
            "people.grant",
            &json!({"person": "sam", "agent": "sound", "preset": "member"}),
        )
        .unwrap();
    assert!(said[0].contains("may not use it now"), "{said:?}");
    assert!(!may(&rig, "sam", "sound"));
    // and taking one away works
    rig.lines(
        "people.revoke",
        &json!({"person": "sam", "agent": "garden"}),
    )
    .unwrap();
    rig.lines("people.undo", &id("sam")).unwrap();
    // undo restores what the rows say now: house as it was, garden gone, the waiting row live
    assert!(may(&rig, "sam", "house"));
    assert!(!may(&rig, "sam", "garden"));
    assert!(may(&rig, "sam", "sound"));
    // the bot is below its owner again: it keeps its own garden row, which its owner no longer backs
    assert!(!may(&rig, "bot-a", "garden"));
    assert!(may(&rig, "bot-a", "house"));
}

#[test]
fn a_preset_deleted_while_suspended_is_the_same_wait_it_would_have_been() {
    // the oracle is a twin world where the same owner steps happen with no suspension at all
    let mut sus = Rig::new(&world());
    let mut twin = Rig::new(&world());
    sus.lines("people.suspend", &id("sam")).unwrap();
    for rig in [&mut sus, &mut twin] {
        rig.lines("people.removePreset", &json!({"name": "pro"}))
            .unwrap();
    }
    sus.lines("people.undo", &id("sam")).unwrap();
    assert_eq!(sus.tables(), twin.tables());
    for person in ["sam", "bot-a", "amy", "bot-b", "*"] {
        for agent in ["house", "garden"] {
            let a = got(Authority::new(sus.core.conn(), &sus.c).may(person, agent, None, false));
            let b = got(Authority::new(twin.core.conn(), &twin.c).may(person, agent, None, false));
            assert_eq!(a, b, "{person} on {agent}");
        }
    }
}

#[test]
fn a_suspended_person_cannot_be_given_a_link_code_and_keeps_the_one_they_had() {
    let mut rig = Rig::new(&world());
    rig.lines("people.suspend", &id("sam")).unwrap();
    let e = refused(&mut rig, "people.invite", &json!({"person": "sam"}));
    assert!(e.contains("access was removed"), "{e}");
    // the open code stays for undo to give back
    assert!(rig.tables()["invites"].get("ABCDEF").is_some());
}

// --- add, rename, remove -----------------------------------------------------------------------------------------------

#[test]
fn a_person_is_added_changed_and_made_the_owner() {
    let mut rig = Rig::new(&base());
    assert_eq!(
        rig.lines("people.addPerson", &id("zed")).unwrap(),
        ["zed: added"]
    );
    assert_eq!(
        rig.lines(
            "people.addPerson",
            &json!({"id": "zed", "name": "Zed", "lang": "en", "tz": "Australia/Sydney"})
        )
        .unwrap(),
        ["zed: name Zed, lang en, tz Australia/Sydney"]
    );
    // an empty value clears
    assert_eq!(
        rig.lines("people.addPerson", &json!({"id": "zed", "lang": ""}))
            .unwrap(),
        ["zed: name Zed, tz Australia/Sydney"]
    );
    // the owner moves: one owner, the old one is told what stays
    let said = rig
        .lines("people.addPerson", &json!({"id": "zed", "owner": true}))
        .unwrap();
    assert_eq!(
        said,
        [
            "note: anyone using this Mac account, and kai's KITH_DISCORD_OWNER_ID, still run as the owner",
            "zed: name Zed, tz Australia/Sydney, owner True",
        ]
    );
    let t = rig.tables();
    assert!(t["people"]["kai"].get("owner").is_none());
    assert_eq!(t["people"]["zed"]["owner"], json!(true));
    // a bot cannot be the owner; a mistyped id is a usage error
    assert_eq!(
        refused(
            &mut rig,
            "people.addPerson",
            &json!({"id": "bot-a", "owner": true})
        ),
        "bot-a is a bot: it can not be the owner"
    );
    assert!(refused(&mut rig, "people.addPerson", &id("Zed")).contains("lowercase letters"));
    // an old id is not a new person
    assert_eq!(
        refused(&mut rig, "people.addPerson", &id("samuel")),
        "'samuel' was renamed to sam"
    );
}

#[test]
fn a_new_person_starts_with_nothing_whatever_a_removal_left() {
    let mut a = base();
    a["members"] = json!({"house": {"zed": {"preset": "member"}}});
    a["invites"] = json!({"AAAAAA": {"person": "zed", "expires": 4_000_000_000.0}});
    let mut rig = Rig::new(&a);
    rig.lines("people.addPerson", &id("zed")).unwrap();
    let t = rig.tables();
    assert!(t["members"].as_object().unwrap().is_empty(), "{t}");
    assert!(t["invites"].as_object().unwrap().is_empty(), "{t}");
}

#[test]
fn a_rename_carries_every_table_and_the_folders_follow() {
    let mut a = world();
    a["members"]["house"]["amy"]["access_by"] = json!({"domain:a.example": "sam"});
    let mut rig = Rig::new(&a);
    let home = rig.machine_dir.path().to_path_buf();
    let mem = home.join("agents/house/memory/people/sam");
    std::fs::create_dir_all(&mem).unwrap();
    std::fs::write(mem.join("note.md"), "x").unwrap();
    std::fs::create_dir_all(home.join("state/people")).unwrap();
    std::fs::write(home.join("state/people/sam.md"), "y").unwrap();
    let said = rig
        .lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    assert_eq!(
        said,
        [
            "sam is now anna; moved their memory and files in house, people (profile names stay as they are: they name accounts, not people)"
        ]
    );
    let t = rig.tables();
    assert!(t["people"].get("sam").is_none());
    assert_eq!(t["people"]["anna"]["was"], json!(["samuel", "sam"]));
    assert_eq!(t["people"]["bot-a"]["bot_of"], "anna");
    assert!(t["members"]["house"].get("sam").is_none());
    assert_eq!(t["members"]["house"]["anna"]["preset"], "pro");
    assert_eq!(t["members"]["garden"]["anna"]["preset"], "member");
    assert_eq!(
        t["members"]["house"]["amy"]["access_by"],
        json!({"domain:a.example": "anna"})
    );
    assert_eq!(t["invites"]["ABCDEF"]["person"], "anna");
    assert!(
        home.join("agents/house/memory/people/anna/note.md")
            .exists()
    );
    assert!(home.join("state/people/anna.md").exists());
    assert!(!mem.exists());
    // the old id still means them, to a reader and to a suspension
    assert!(may(&rig, "samuel", "house") && may(&rig, "sam", "house"));
    rig.lines("people.suspend", &id("sam")).unwrap();
    assert!(!may(&rig, "anna", "house") && !may(&rig, "bot-a", "house"));
}

#[test]
fn a_rename_keeps_a_suspension() {
    let mut rig = Rig::new(&world());
    rig.lines("people.suspend", &id("sam")).unwrap();
    rig.lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    assert!(!may(&rig, "anna", "house") && !may(&rig, "bot-a", "house"));
    rig.lines("people.undo", &id("anna")).unwrap();
    assert!(may(&rig, "anna", "house"));
}

#[test]
fn a_rename_is_refused_for_the_ids_kith_auth_refuses() {
    let mut a = world();
    a["retired"] = json!({"old": {"at": 3}});
    let mut rig = Rig::new(&a);
    std::fs::create_dir_all(
        rig.machine_dir
            .path()
            .join("agents/house/files/people/left"),
    )
    .unwrap();
    let before = rig.tables();
    let rename = |rig: &mut Rig, from: &str, to: &str| {
        refused(rig, "people.renamePerson", &json!({"id": from, "to": to}))
    };
    assert_eq!(
        rename(&mut rig, "ghost", "zed"),
        "no person 'ghost' (kith-auth person ghost name=..)"
    );
    assert_eq!(
        rename(&mut rig, "sam", "amy"),
        "'amy' is already a person (or one's old id)"
    );
    assert_eq!(
        rename(&mut rig, "amy", "samuel"),
        "'samuel' is already a person (or one's old id)"
    );
    assert_eq!(
        rename(&mut rig, "sam", "old"),
        "'old' was a person who was removed: give the new person another id (e.g. old-2)"
    );
    assert_eq!(
        rename(&mut rig, "sam", "left"),
        "memory and files of a removed 'left' are still in house (move them away to use the id): give the new person another id (e.g. left-2)"
    );
    assert!(rename(&mut rig, "sam", "No").contains("lowercase letters"));
    assert_eq!(rig.tables(), before);
}

#[test]
fn a_removal_takes_the_bots_rows_and_codes_and_retires_the_ids() {
    let mut rig = Rig::new(&world());
    let home = rig.machine_dir.path().to_path_buf();
    std::fs::create_dir_all(home.join("agents/garden/files/people/sam")).unwrap();
    let said = rig.lines("people.removePerson", &id("sam")).unwrap();
    assert_eq!(
        said,
        [
            "sam removed, with their memberships and grants and bots (bot-a) (profiles they own are kept); the id is retired, a new person gets another; their memory and files in garden are kept (delete them by hand)"
        ]
    );
    let t = rig.tables();
    assert!(t["people"].get("sam").is_none() && t["people"].get("bot-a").is_none());
    assert!(t["members"]["house"].get("sam").is_none());
    assert!(t["members"]["house"].get("bot-a").is_none());
    assert!(
        t["members"].get("garden").is_none(),
        "an agent without rows leaves the table"
    );
    assert!(t["invites"].as_object().unwrap().is_empty());
    // the person's ids are retired (their old one too), the bot's is not: the same bot may join again
    assert_eq!(t["retired"]["sam"], json!({"at": 1_800_000_000}));
    assert!(t["retired"].get("samuel").is_some());
    assert!(t["retired"].get("bot-a").is_none());
    // nobody else takes the id, and it is no person to grant
    let e = refused(&mut rig, "people.addPerson", &id("sam"));
    assert_eq!(
        e,
        "'sam' was a person who was removed: give the new person another id (e.g. sam-2)"
    );
    assert_eq!(
        rig.lines("people.retired", &id("samuel")).unwrap(),
        ["'samuel' was a person who was removed"]
    );
    assert_eq!(
        rig.lines("people.retired", &id("amy")).unwrap(),
        Vec::<String>::new()
    );
    assert!(
        rig.lines("people.addBot", &json!({"id": "bot-a", "of": "amy"}))
            .is_ok()
    );
    // the others are untouched
    assert!(may(&rig, "amy", "house") && may(&rig, "bot-b", "house"));
}

#[test]
fn an_unused_person_is_removed_and_the_id_stays_free() {
    let mut rig = Rig::new(&world());
    rig.lines("people.addPerson", &id("zed")).unwrap();
    assert_eq!(
        rig.lines("people.removePerson", &json!({"id": "zed", "unused": true}))
            .unwrap(),
        ["zed removed (unused: the id is free again)"]
    );
    let t = rig.tables();
    assert!(t["people"].get("zed").is_none() && t["retired"].get("zed").is_none());
    rig.lines("people.addPerson", &id("zed")).unwrap();
    // anything they have refuses it
    let e = refused(
        &mut rig,
        "people.removePerson",
        &json!({"id": "sam", "unused": true}),
    );
    assert_eq!(
        e,
        "sam is not unused (was, bots, memberships, invites): kith-auth person sam --rm removes them and retires the id"
    );
    rig.lines("people.suspend", &id("amy")).unwrap();
    let e = refused(
        &mut rig,
        "people.removePerson",
        &json!({"id": "amy", "unused": true}),
    );
    assert!(
        e.starts_with("amy is not unused (revoked, bots, memberships)"),
        "{e}"
    );
    assert!(
        refused(
            &mut rig,
            "people.removePerson",
            &json!({"id": "ghost", "unused": true})
        )
        .starts_with("no person 'ghost'")
    );
}

#[test]
fn removing_a_suspended_person_removes_them() {
    let mut rig = Rig::new(&world());
    rig.lines("people.suspend", &id("sam")).unwrap();
    rig.lines("people.removePerson", &id("sam")).unwrap();
    let t = rig.tables();
    assert!(t["people"].get("sam").is_none());
    assert_eq!(
        refused(&mut rig, "people.undo", &id("sam")),
        "no person 'sam' (kith-auth person sam name=..)"
    );
}

// --- the temp folder -----------------------------------------------------------------------------------------------------

fn temp_of(rig: &Rig, who: &str, owner: Option<&str>) -> std::path::PathBuf {
    let run = rig.machine_dir.path().join("machine/run");
    let d = run.join("t").join(who);
    std::fs::create_dir_all(d.join("claude-1")).unwrap();
    std::fs::create_dir_all(run.join("t-who")).unwrap();
    if let Some(o) = owner {
        std::fs::write(run.join("t-who").join(who), o).unwrap();
    }
    d
}

#[test]
fn a_suspension_and_a_removal_clear_the_temp_folders_of_the_person_and_their_bots() {
    let mut rig = Rig::new(&world());
    let (sam, bot, amy) = (
        temp_of(&rig, "sam", Some("sam")),
        temp_of(&rig, "bot-a", Some("bot-a")),
        temp_of(&rig, "amy", Some("amy")),
    );
    rig.lines("people.suspend", &id("sam")).unwrap();
    assert!(
        !sam.exists() && !bot.exists(),
        "their runs' temp folders go"
    );
    assert!(amy.exists());
    rig.lines("people.removePerson", &id("amy")).unwrap();
    assert!(!amy.exists());
}

#[test]
fn a_temp_folder_recorded_as_someone_elses_stays_and_clear_temp_is_a_verb() {
    let mut rig = Rig::new(&world());
    let theirs = temp_of(&rig, "sam", Some("someone-else"));
    let said = rig.call("people.clearTemp", &id("sam")).unwrap();
    assert_eq!(said["cleared"], json!(["sam", "bot-a"]));
    assert!(theirs.exists());
    // a record that names them, or none at all for the plain name, and it goes
    std::fs::write(rig.machine_dir.path().join("machine/run/t-who/sam"), "sam").unwrap();
    rig.call("people.clearTemp", &id("sam")).unwrap();
    assert!(!theirs.exists());
    // the temp folder of someone who is not a person is cleared too: kith-auth asks nothing of the tables
    assert!(rig.call("people.clearTemp", &id("ghost")).is_ok());
    assert!(refused(&mut rig, "people.clearTemp", &id("Bad Id")).contains("lowercase letters"));
}

// --- a revoke racing a grant -----------------------------------------------------------------------------------------------

#[test]
fn a_revoke_racing_a_grant_leaves_the_person_cut_off_either_way() {
    for suspend_first in [true, false] {
        let mut rig = Rig::new(&world());
        let c = rig.c.clone();
        {
            let core = Mutex::new(&mut rig.core);
            std::thread::scope(|s| {
                let grant = s.spawn(|| {
                    let mut guard = core.lock().unwrap();
                    let params = json!({"person": "sam", "agent": "sound", "preset": "member"});
                    call_with(&mut guard, &c, "people.grant", &params)
                });
                let suspend = s.spawn(|| {
                    let mut guard = core.lock().unwrap();
                    call_with(&mut guard, &c, "people.suspend", &id("sam")).unwrap();
                });
                if suspend_first {
                    suspend.join().unwrap();
                    grant.join().unwrap().unwrap();
                } else {
                    grant.join().unwrap().unwrap();
                    suspend.join().unwrap();
                }
            });
        }
        // whichever won: the row may be there, and they reach nothing
        assert!(
            !may(&rig, "sam", "sound")
                && !may(&rig, "sam", "house")
                && !may(&rig, "bot-a", "house")
        );
        rig.lines("people.undo", &id("sam")).unwrap();
        assert!(may(&rig, "sam", "house"));
        let has_row = rig.tables()["members"]["sound"].get("sam").is_some();
        assert_eq!(may(&rig, "sam", "sound"), has_row);
    }
}

// --- the property --------------------------------------------------------------------------------------------------------

/// What every reader says for every id, agent, helper door and `once`: the oracle's "before".
fn answers(rig: &Rig, ids: &[String]) -> BTreeMap<String, String> {
    let core = Authority::new(rig.core.conn(), &rig.c);
    let mut out = BTreeMap::new();
    for p in ids {
        out.insert(
            format!("cut_off {p}"),
            format!("{:?}", got(core.cut_off(p))),
        );
        for agent in AGENTS {
            for via in [None, Some("house")] {
                for once in [false, true] {
                    let k = format!("{p} {agent} {via:?} {once}");
                    out.insert(
                        format!("may {k}"),
                        format!("{:?}", got(core.may(p, agent, via, once))),
                    );
                    out.insert(
                        format!("resolve {k}"),
                        format!("{:?}", got(core.resolve(agent, p, via, once))),
                    );
                    out.insert(
                        format!("bot {k}"),
                        format!("{:?}", got(core.resolve_bot(agent, p, via, once))),
                    );
                }
            }
        }
    }
    out
}

/// The person an id means, by the generated `people` alone (not the rules under test).
fn who(auth: &Value, id: &str) -> String {
    let people = auth["people"].as_object().cloned().unwrap_or_default();
    if people.contains_key(id) {
        return id.to_owned();
    }
    people
        .iter()
        .find(|(_, v)| {
            v["was"]
                .as_array()
                .is_some_and(|w| w.iter().any(|x| x == id))
        })
        .map_or_else(|| id.to_owned(), |(k, _)| k.clone())
}

/// Whether `id` is `top` or a bot, or a bot of a bot, of theirs, by following the generated `bot_of` links.
fn under(auth: &Value, id: &str, top: &str) -> bool {
    let mut at = who(auth, id);
    for _ in 0..8 {
        if at == top {
            return true;
        }
        match auth["people"][&at]["bot_of"]
            .as_str()
            .filter(|b| !b.is_empty())
        {
            Some(parent) => at = who(auth, parent),
            None => return false,
        }
    }
    false
}

fn ids_of(auth: &Value) -> Vec<String> {
    let mut v: Vec<String> = auth["people"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    v.extend(["ghost", "*", "", "old-sam"].map(str::to_owned));
    v
}

#[test]
fn a_suspension_cuts_off_the_person_and_their_bots_and_undo_gives_back_exactly_what_they_had() {
    let rig = std::cell::RefCell::new(Rig::new(&json!({})));
    let mut config = Config {
        failure_persistence: None,
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
    }
    let mut runner = TestRunner::new(config);
    runner
        .run(
            &(auth_strategy(), 0usize..2, any::<bool>()),
            |(auth, which, twice)| {
                let mut rig = rig.borrow_mut();
                rig.reset(&auth);
                // a person who has nothing revoked yet (undo clears the fact, so a person revoked before is a different story)
                let target = ["sam", "amy"][which];
                let Some(p) = auth["people"].get(target) else {
                    return Ok(());
                };
                if p.get("revoked").is_some() {
                    return Ok(());
                }
                let ids = ids_of(&auth);
                let (tables, before) = (rig.tables(), answers(&rig, &ids));

                rig.call("people.suspend", &id(target))
                    .map_err(TestCaseError::fail)?;
                if twice {
                    rig.call("people.suspend", &id(target))
                        .map_err(TestCaseError::fail)?;
                }
                let during = answers(&rig, &ids);
                for (k, v) in &during {
                    let mut it = k.split(' ');
                    let (kind, who_id) = (it.next().unwrap(), it.next().unwrap_or(""));
                    if under(&auth, who_id, target) && !who_id.is_empty() {
                        // reaches nothing: not allowed, not resolved as allowed, cut off
                        let denied = match kind {
                            "may" => v.contains("Ok((false,") || v.starts_with("Err("),
                            "resolve" | "bot" => !v.contains("\"allowed\": Bool(true)"),
                            "cut_off" => v == "Ok(true)",
                            _ => true,
                        };
                        prop_assert!(denied, "{k} after a suspension: {v}");
                    } else {
                        // everyone else answers as they did
                        prop_assert_eq!(v, &before[k], "{} (not theirs)", k);
                    }
                }

                rig.call("people.undo", &id(target))
                    .map_err(TestCaseError::fail)?;
                if twice {
                    rig.call("people.undo", &id(target))
                        .map_err(TestCaseError::fail)?;
                }
                prop_assert_eq!(rig.tables(), tables);
                prop_assert_eq!(answers(&rig, &ids), before);
                Ok(())
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
}
