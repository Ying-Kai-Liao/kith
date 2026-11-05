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
fn undo_on_an_owner_who_was_revoked_by_old_data_clears_it_and_the_revoke_stays_refused() {
    let mut auth = world();
    auth["people"]["kai"]["revoked"] = json!({"at": 1});
    let mut rig = Rig::new(&auth);
    assert_eq!(
        refused(&mut rig, "people.suspend", &id("kai")),
        "kai is the owner: the owner can not be revoked"
    );
    let said = rig.lines("people.undo", &id("kai")).unwrap();
    assert!(
        said[0].starts_with("kai: access back as it was"),
        "{said:?}"
    );
    let t = rig.tables();
    assert!(t["people"]["kai"].get("revoked").is_none());
    assert_eq!(t["people"]["kai"]["owner"], json!(true));
    // nothing else moved: undo widens nothing
    let mut want = world();
    want["people"]["kai"]["revoked"] = json!(null);
    want["people"]["kai"]
        .as_object_mut()
        .unwrap()
        .remove("revoked");
    assert_eq!(t["people"], want["people"]);
    // an owner who is not revoked: undo changes nothing
    let before = rig.tables();
    rig.lines("people.undo", &id("kai")).unwrap();
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
fn a_suspension_and_a_removal_leave_the_temp_folders_to_the_caller_who_stops_the_runs_first() {
    let mut rig = Rig::new(&world());
    let (sam, bot, amy) = (
        temp_of(&rig, "sam", Some("sam")),
        temp_of(&rig, "bot-a", Some("bot-a")),
        temp_of(&rig, "amy", Some("amy")),
    );
    // as `kith-auth --local`: runs are not stopped here, so nothing under them is pulled away
    let said = rig.call("people.suspend", &id("sam")).unwrap();
    assert_eq!(said["ids"], json!(["sam", "bot-a"]));
    assert!(sam.exists() && bot.exists());
    let said = rig.call("people.removePerson", &id("amy")).unwrap();
    assert_eq!(said["ids"], json!(["amy", "bot-b"]));
    assert!(amy.exists());
    // the caller then clears them
    rig.call("people.clearTemp", &id("sam")).unwrap();
    assert!(!sam.exists() && !bot.exists());
    assert!(amy.exists());
}

#[test]
fn a_folder_that_cannot_move_after_a_rename_is_a_note_and_not_an_error() {
    let rig = Rig::new(&world());
    let home = rig.machine_dir.path().to_path_buf();
    let life = crate::authority::revoke::Life::beside(&rig.c);
    std::fs::create_dir_all(home.join("agents/house/memory/people/sam")).unwrap();
    std::fs::create_dir_all(home.join("agents/garden/files/people/sam")).unwrap();
    // a destination that appeared after the check: left as it is, said, and the other folder still moves
    std::fs::create_dir_all(home.join("agents/house/memory/people/anna")).unwrap();
    let (moved, notes) = crate::authority::revoke::move_life(&life, "sam", "anna");
    assert_eq!(moved, ["garden"]);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("note: ") && notes[0].contains("is already there"),
        "{notes:?}"
    );
    assert!(home.join("agents/house/memory/people/sam").exists());
    assert!(home.join("agents/garden/files/people/anna").exists());
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
fn a_revoke_and_a_grant_in_either_order_leave_the_person_cut_off_and_the_row_there() {
    for suspend_first in [true, false] {
        let mut rig = Rig::new(&world());
        let grant = json!({"person": "sam", "agent": "sound", "preset": "member"});
        if suspend_first {
            rig.lines("people.suspend", &id("sam")).unwrap();
            rig.lines("people.grant", &grant).unwrap();
        } else {
            rig.lines("people.grant", &grant).unwrap();
            rig.lines("people.suspend", &id("sam")).unwrap();
        }
        // the row is written either way (kith-auth writes it and says it waits), and they reach nothing
        assert!(rig.tables()["members"]["sound"].get("sam").is_some());
        assert!(
            !may(&rig, "sam", "sound")
                && !may(&rig, "sam", "house")
                && !may(&rig, "bot-a", "house")
        );
        rig.lines("people.undo", &id("sam")).unwrap();
        assert!(may(&rig, "sam", "house") && may(&rig, "sam", "sound"));
    }
}

#[test]
fn a_revoke_racing_a_grant_on_one_core_behind_a_mutex_leaves_the_person_cut_off() {
    // the daemon holds one Core behind a Mutex; both calls start together and the claim serialises them
    let mut rig = Rig::new(&world());
    let c = rig.c.clone();
    let start = std::sync::Barrier::new(2);
    {
        let core = Mutex::new(&mut rig.core);
        std::thread::scope(|s| {
            let grant = s.spawn(|| {
                start.wait();
                let mut guard = core.lock().unwrap();
                let params = json!({"person": "sam", "agent": "sound", "preset": "member"});
                call_with(&mut guard, &c, "people.grant", &params)
            });
            let suspend = s.spawn(|| {
                start.wait();
                let mut guard = core.lock().unwrap();
                call_with(&mut guard, &c, "people.suspend", &id("sam"))
            });
            grant.join().unwrap().unwrap();
            suspend.join().unwrap().unwrap();
        });
    }
    assert!(
        !may(&rig, "sam", "sound") && !may(&rig, "sam", "house") && !may(&rig, "bot-a", "house")
    );
    rig.lines("people.undo", &id("sam")).unwrap();
    assert!(may(&rig, "sam", "house") && may(&rig, "sam", "sound"));
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

/// Whether an answer of a reader (its Debug text) reaches nothing: not allowed, not resolved as allowed, cut off.
fn is_denied(kind: &str, v: &str) -> bool {
    match kind {
        "may" => v.contains("Ok((false,") || v.starts_with("Err("),
        "resolve" | "bot" => !v.contains("\"allowed\": Bool(true)"),
        "cut_off" => v == "Ok(true)",
        _ => true,
    }
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
    let twin = std::cell::RefCell::new(Rig::new(&json!({})));
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
            &(auth_strategy(), 0usize..2, any::<bool>(), 0u8..3),
            |(auth, which, twice, edit)| {
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
                    // `cut_off` takes a current id (its callers canonicalise first, as kith-auth's do): an old id is no key
                    if kind == "cut_off" && auth["people"].get(who_id).is_none() {
                        continue;
                    }
                    if under(&auth, who_id, target) && !who_id.is_empty() {
                        prop_assert!(is_denied(kind, v), "{k} after a suspension: {v}");
                    } else {
                        // everyone else answers as they did
                        prop_assert_eq!(v, &before[k], "{} (not theirs)", k);
                    }
                }
                // the owner edits the person's rows while they are suspended: taking one away, or giving one
                let edit_call = match edit {
                    1 => Some(("people.revoke", json!({"person": target, "agent": "house"}))),
                    2 => Some((
                        "people.grant",
                        json!({"person": target, "agent": "garden", "preset": "member"}),
                    )),
                    _ => None,
                };
                if let Some((method, params)) = &edit_call {
                    let _ = rig.call(method, params);
                    // an edit never lets them through
                    for (k, v) in &answers(&rig, &ids) {
                        let mut it = k.split(' ');
                        let (kind, who_id) = (it.next().unwrap(), it.next().unwrap_or(""));
                        if kind == "cut_off" && auth["people"].get(who_id).is_none() {
                            continue;
                        }
                        if under(&auth, who_id, target) && !who_id.is_empty() {
                            prop_assert!(is_denied(kind, v), "{k} after an edit: {v}");
                        }
                    }
                }

                rig.call("people.undo", &id(target))
                    .map_err(TestCaseError::fail)?;
                if twice {
                    rig.call("people.undo", &id(target))
                        .map_err(TestCaseError::fail)?;
                }
                if let Some((method, params)) = &edit_call {
                    // undo shows the owner's edit and nothing else: the twin world had the edit and no suspension
                    let mut twin = twin.borrow_mut();
                    twin.reset(&auth);
                    // (A grant to a suspended person skips the `resolve` check a live person's grant goes through,
                    // as `kith-auth member` does, so a grant the twin refuses can still be written here. That row
                    // answers "no" when read, so it is only compared where the twin took the same edit.)
                    if twin.call(method, params).is_ok() {
                        prop_assert_eq!(rig.tables(), twin.tables());
                        prop_assert_eq!(answers(&rig, &ids), answers(&twin, &ids));
                    }
                } else {
                    prop_assert_eq!(rig.tables(), tables);
                    prop_assert_eq!(answers(&rig, &ids), before);
                }
                Ok(())
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn an_unused_person_with_handles_grants_or_profiles_is_not_unused() {
    let mut rig = Rig::new(&world());
    let unused = |rig: &mut Rig, who: &str| {
        refused(
            rig,
            "people.removePerson",
            &json!({"id": who, "unused": true}),
        )
    };
    // handles come from the people table (m0015), the same way the import fills them
    rig.lines("people.addPerson", &id("hal")).unwrap();
    rig.core
        .conn()
        .execute(
            "UPDATE people SET handles = '{\"telegram\":\"9\"}' WHERE id = 'hal'",
            [],
        )
        .unwrap();
    assert_eq!(
        unused(&mut rig, "hal"),
        "hal is not unused (handles): kith-auth person hal --rm removes them and retires the id"
    );
    // an `agent@<id>` grant
    rig.lines("people.addPerson", &id("ivy")).unwrap();
    rig.core
        .conn()
        .execute(
            "INSERT INTO secret_grants (agent, service, profile, seq) VALUES ('house@ivy', 'github', 'github/ivy', 0)",
            [],
        )
        .unwrap();
    assert_eq!(
        unused(&mut rig, "ivy"),
        "ivy is not unused (grants): kith-auth person ivy --rm removes them and retires the id"
    );
    // a profile they own, and one lent to them
    rig.lines("people.addPerson", &id("jon")).unwrap();
    rig.lines("people.addPerson", &id("kim")).unwrap();
    rig.core
        .conn()
        .execute_batch(
            "INSERT INTO secret_profiles (profile, person, lent) VALUES ('github/jon', 'jon', NULL), \
             ('github/kai', 'kai', '[\"kim\"]')",
        )
        .unwrap();
    assert_eq!(
        unused(&mut rig, "jon"),
        "jon is not unused (profiles): kith-auth person jon --rm removes them and retires the id"
    );
    assert_eq!(
        unused(&mut rig, "kim"),
        "kim is not unused (profiles): kith-auth person kim --rm removes them and retires the id"
    );
}

#[test]
fn a_removed_persons_id_is_not_free_while_they_still_own_a_profile() {
    let mut rig = Rig::new(&world());
    rig.core
        .conn()
        .execute(
            "INSERT INTO secret_profiles (profile, person, lent) VALUES ('github/lee', 'lee', NULL)",
            [],
        )
        .unwrap();
    assert_eq!(
        rig.lines("people.retired", &id("lee")).unwrap(),
        ["a removed 'lee' still owns github/lee"]
    );
    assert_eq!(
        refused(&mut rig, "people.addPerson", &id("lee")),
        "a removed 'lee' still owns github/lee: give the new person another id (e.g. lee-2)"
    );
}

#[test]
fn a_database_imported_before_handles_cannot_say_a_person_is_unused() {
    let mut rig = Rig::new(&world());
    rig.lines("people.addPerson", &id("hal")).unwrap();
    // what m0015 leaves on a database that already had people
    rig.core
        .conn()
        .execute(
            "INSERT INTO authority_settings (key, value) VALUES ('handles_stale', '1')",
            [],
        )
        .unwrap();
    let said = refused(
        &mut rig,
        "people.removePerson",
        &json!({"id": "hal", "unused": true}),
    );
    assert!(said.contains("import again first"), "{said}");
    rig.core
        .conn()
        .execute(
            "DELETE FROM authority_settings WHERE key = 'handles_stale'",
            [],
        )
        .unwrap();
    rig.lines("people.removePerson", &json!({"id": "hal", "unused": true}))
        .unwrap();
}

// --- lane B filled by the import ---------------------------------------------------------------------------------------------

/// A world the real importer fills: a person with an `agent@<id>` grant, one who owns a profile, one it is lent to, one with
/// nothing, and an owned profile of an id that is no longer a person.
fn imported_world() -> Rig {
    let rig = Rig::new(&json!({}));
    crate::import::insert_all_for_tests(
        rig.core.conn(),
        &json!({
            "people": {"kai": {"owner": true}, "ivy": {}, "jon": {}, "kim": {}, "zed": {}},
            "profiles": {
                "github/jon": {"GITHUB_TOKEN": "keychain:kith/github/jon#GITHUB_TOKEN", "person": "jon"},
                "github/kai": {"person": "kai", "with": ["kim", "kai"]},
                "github/lee": {"person": "lee"},
                "github/shared": {}
            },
            "grants": {"house": {"github": "github/shared"}, "house@ivy": {"github": "github/shared"}}
        }),
    )
    .unwrap();
    rig
}

#[test]
fn after_an_import_remove_unused_refuses_a_person_with_a_grant_a_profile_or_one_lent_to_them() {
    let mut rig = imported_world();
    for (who, what) in [("ivy", "grants"), ("jon", "profiles"), ("kim", "profiles")] {
        assert_eq!(
            refused(
                &mut rig,
                "people.removePerson",
                &json!({"id": who, "unused": true})
            ),
            format!(
                "{who} is not unused ({what}): kith-auth person {who} --rm removes them and retires the id"
            )
        );
    }
    rig.lines("people.removePerson", &json!({"id": "zed", "unused": true}))
        .unwrap();
}

#[test]
fn after_an_import_a_removed_persons_id_is_kept_while_they_own_a_profile() {
    let mut rig = imported_world();
    assert_eq!(
        rig.lines("people.retired", &id("lee")).unwrap(),
        ["a removed 'lee' still owns github/lee"]
    );
    assert_eq!(
        refused(&mut rig, "people.addPerson", &id("lee")),
        "a removed 'lee' still owns github/lee: give the new person another id (e.g. lee-2)"
    );
    // an id nothing is left of is free
    assert_eq!(
        rig.lines("people.retired", &id("nobody")).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_database_imported_before_the_tables_were_filled_cannot_say_an_id_is_free_or_a_person_unused() {
    let mut rig = imported_world();
    // what m0022 leaves on a database that already had people
    rig.core
        .conn()
        .execute_batch(
            "DELETE FROM secret_profiles; DELETE FROM secret_vars; DELETE FROM secret_grants; \
             INSERT INTO secret_stale VALUES ('imported before secret_grants');",
        )
        .unwrap();
    let said = rig.lines("people.retired", &id("lee")).unwrap();
    assert!(said[0].contains("import again first"), "{said:?}");
    let said = refused(
        &mut rig,
        "people.removePerson",
        &json!({"id": "zed", "unused": true}),
    );
    assert!(said.contains("import again first"), "{said}");
    // a re-import (what `--force` does) empties the mark with the tables it refills
    rig.core
        .conn()
        .execute_batch("DELETE FROM secret_stale")
        .unwrap();
    rig.lines("people.removePerson", &json!({"id": "zed", "unused": true}))
        .unwrap();
}

// --- a rename and the owner's change carry the secrets tables (lane B) ---------------------------------------------------

/// Fills `secret_grants` and `secret_profiles` from auth.json's `grants` and `profiles` (person and `with` only), the
/// way the import does.
fn seed_secrets(rig: &Rig, auth: &Value) {
    let conn = rig.core.conn();
    for (agent, by) in auth["grants"].as_object().into_iter().flatten() {
        for (service, p) in by.as_object().unwrap() {
            let list = p.as_array().cloned().unwrap_or_else(|| vec![p.clone()]);
            for (seq, profile) in list.iter().enumerate() {
                conn.execute(
                    "INSERT INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![agent, service, profile.as_str().unwrap(), i64::try_from(seq).unwrap()],
                )
                .unwrap();
            }
        }
    }
    for (name, p) in auth["profiles"].as_object().into_iter().flatten() {
        conn.execute(
            "INSERT INTO secret_profiles (profile, person, lent) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                name,
                p.get("person").and_then(Value::as_str),
                p.get("with").map(Value::to_string)
            ],
        )
        .unwrap();
    }
}

fn secrets_world() -> Value {
    let mut a = world();
    a["grants"] = json!({
        "house": {"github": "github/kai"},
        "house@amy": {"github": "github/amy"},
        "house@sam": {"github": ["github/sam", "github/kai"]},
        "bot@sam": {"web": "web/sam"},
        "garden@sam": {"github": "github/sam"},
        "zed@samuel": {"github": "github/sam"},
    });
    a["profiles"] = json!({
        "github/amy": {"person": "amy", "with": ["sam"]},
        "github/kai": {"person": "kai", "with": ["amy", "sam", "bot-a"]},
        "github/sam": {"person": "sam"},
        "web/sam": {"person": "sam", "with": ["sam"]},
    });
    a
}

/// The grants (with their key order) and profiles as the core reads them back.
fn secrets_of(rig: &mut Rig) -> (Vec<String>, Value, Value) {
    let t = rig.tables();
    let keys = t["grants"]
        .as_object()
        .map(|g| g.keys().cloned().collect())
        .unwrap_or_default();
    (keys, t["grants"].clone(), t["profiles"].clone())
}

#[test]
fn a_rename_moves_the_grants_and_profiles_of_the_person() {
    let a = secrets_world();
    let mut rig = Rig::new(&a);
    seed_secrets(&rig, &a);
    let said = rig
        .lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    assert_eq!(
        said,
        [
            "sam is now anna; owns github/sam, web/sam (profile names stay as they are: they name accounts, not people)"
        ]
    );
    let (keys, grants, profiles) = secrets_of(&mut rig);
    assert_eq!(keys.len(), 6, "{grants}");
    assert_eq!(
        grants["house@anna"]["github"],
        json!(["github/sam", "github/kai"])
    );
    assert_eq!(grants["bot@anna"], json!({"web": "web/sam"}));
    assert_eq!(grants["garden@anna"], json!({"github": "github/sam"}));
    assert!(grants.get("house@sam").is_none() && grants.get("garden@sam").is_none());
    // another person's, the agent's own and an older id's stay
    assert_eq!(grants["house@amy"], json!({"github": "github/amy"}));
    assert_eq!(grants["house"], json!({"github": "github/kai"}));
    assert_eq!(grants["zed@samuel"], json!({"github": "github/sam"}));
    assert_eq!(profiles["github/sam"]["person"], "anna");
    assert_eq!(
        profiles["web/sam"],
        json!({"person": "anna", "with": ["anna"]})
    );
    assert_eq!(profiles["github/amy"]["with"], json!(["anna"]));
    assert_eq!(
        profiles["github/kai"]["with"],
        json!(["amy", "anna", "bot-a"])
    );
    assert_eq!(profiles["github/kai"]["person"], "kai");
    // the rows, not only the read-back
    let rows = |rig: &Rig, agent: &str| -> i64 {
        rig.core
            .conn()
            .query_row(
                "SELECT count(*) FROM secret_grants WHERE agent = ?1",
                [agent],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!((rows(&rig, "house@sam"), rows(&rig, "house@anna")), (0, 2));
}

#[test]
fn renaming_twice_carries_the_secrets_on_and_the_first_id_cannot_come_back() {
    let a = secrets_world();
    let mut rig = Rig::new(&a);
    seed_secrets(&rig, &a);
    rig.lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    let after_first = secrets_of(&mut rig);
    rig.lines("people.renamePerson", &json!({"id": "anna", "to": "zoe"}))
        .unwrap();
    let (_, grants, profiles) = secrets_of(&mut rig);
    assert_eq!(grants["house@zoe"], after_first.1["house@anna"]);
    assert!(grants.get("house@anna").is_none());
    assert_eq!(profiles["github/sam"]["person"], "zoe");
    assert_eq!(
        profiles["github/kai"]["with"],
        json!(["amy", "zoe", "bot-a"])
    );
    // an old id is not free again, and the refusal leaves the tables as they were
    let before = secrets_of(&mut rig);
    assert_eq!(
        refused(
            &mut rig,
            "people.renamePerson",
            &json!({"id": "zoe", "to": "sam"})
        ),
        "'sam' is already a person (or one's old id)"
    );
    assert_eq!(secrets_of(&mut rig), before);
}

#[test]
fn a_rename_without_secrets_and_with_the_new_id_already_lent_changes_nothing_else() {
    // lane B empty (a database imported before it, or a Mac with no logins): the rename is the same as ever
    let mut rig = Rig::new(&world());
    assert_eq!(
        rig.lines("people.renamePerson", &json!({"id": "amy", "to": "bea"}))
            .unwrap(),
        ["amy is now bea (profile names stay as they are: they name accounts, not people)"]
    );
    let t = rig.tables();
    assert!(
        t.get("grants").is_none() && t.get("profiles").is_none(),
        "{t}"
    );
    // an id lent a login by a removed person is not lent it again under a rename to that id: `forget` ran first
    let mut a = secrets_world();
    a["profiles"]["github/kai"]["with"] = json!(["bea", "amy", "sam", "bot-a"]);
    rig.reset(&a);
    seed_secrets(&rig, &a);
    rig.lines("people.renamePerson", &json!({"id": "sam", "to": "bea"}))
        .unwrap();
    let (_, _, profiles) = secrets_of(&mut rig);
    assert_eq!(
        profiles["github/kai"]["with"],
        json!(["amy", "bea", "bot-a"])
    );
}

#[test]
fn a_renamed_owner_or_bot_takes_its_grants_and_logins_along() {
    let mut a = secrets_world();
    a["grants"]["house@bot-a"] = json!({"github": "github/kai"});
    a["profiles"]["github/kai"]["with"] = json!(["bot-a"]);
    let mut rig = Rig::new(&a);
    seed_secrets(&rig, &a);
    rig.lines("people.renamePerson", &json!({"id": "kai", "to": "ken"}))
        .unwrap();
    rig.lines(
        "people.renamePerson",
        &json!({"id": "bot-a", "to": "bot-z"}),
    )
    .unwrap();
    let (_, grants, profiles) = secrets_of(&mut rig);
    assert!(grants.get("house@bot-a").is_none());
    assert_eq!(grants["house@bot-z"], json!({"github": "github/kai"}));
    assert_eq!(profiles["github/kai"]["person"], "ken");
    assert_eq!(profiles["github/kai"]["with"], json!(["bot-z"]));
}

#[test]
fn a_new_owner_keeps_the_old_owners_default_logins() {
    // The import stores a default login as its owner's id; one stored while no owner was named is `the owner`.
    let mut a = world();
    a["profiles"] = json!({
        "chrome-agent/kai": {"person": "kai"},
        "chrome-agent/default": {"person": "the owner"},
        "github/shared": {},
    });
    let mut rig = Rig::new(&a);
    seed_secrets(&rig, &a);
    rig.lines("people.addPerson", &json!({"id": "amy", "owner": true}))
        .unwrap();
    let t = rig.tables();
    assert_eq!(t["profiles"]["chrome-agent/kai"]["person"], "kai");
    assert_eq!(t["profiles"]["chrome-agent/default"]["person"], "kai");
    assert!(t["profiles"]["github/shared"].get("person").is_none());
    // with no owner named yet, "the owner" is whoever is named
    let mut b = world();
    b["people"]["kai"] = json!({});
    b["profiles"] = json!({"chrome-agent/default": {"person": "the owner"}});
    rig.reset(&b);
    seed_secrets(&rig, &b);
    rig.lines("people.addPerson", &json!({"id": "amy", "owner": true}))
        .unwrap();
    assert_eq!(
        rig.tables()["profiles"]["chrome-agent/default"]["person"],
        "amy"
    );
}

/// `kith-auth person <old> id=<new>` over a file holding `auth`, or `None` where there is no python3.
fn python_rename(auth: &Value, old: &str, new: &str) -> Option<Value> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("auth.json");
    let mut auth = auth.clone();
    for t in [
        "people", "members", "presets", "invites", "grants", "profiles",
    ] {
        if auth.get(t).is_none() {
            auth[t] = json!({});
        }
    }
    std::fs::write(&file, auth.to_string()).unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
    let out = std::process::Command::new("python3")
        .arg(&script)
        .args(["person", old, &format!("id={new}")])
        .env_clear()
        .env("PATH", std::env::var_os("PATH")?)
        .env("HOME", dir.path())
        .env("KITH_AUTH", &file)
        .env("KITH_HOME", dir.path().join("kith"))
        .env("KITH_AGENTS", dir.path().join("agents"))
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "kith-auth person failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap())
}

#[test]
fn the_secrets_after_a_rename_are_what_kith_auth_leaves() {
    let mut a = secrets_world();
    // a name listed twice, a name only in `with`, an owner-less profile and a trailing `@` look-alike
    a["profiles"]["github/kai"]["with"] = json!(["sam", "amy", "sam"]);
    a["grants"]["a@b@sam"] = json!({"github": "github/sam"});
    a["grants"]["sam@x"] = json!({"github": "github/sam"});
    a["profiles"]["github/none"] = json!({});
    let Some(py) = python_rename(&a, "sam", "anna") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "no python3 in CI: the rename was not compared with kith-auth"
        );
        eprintln!("no python3: rename not compared with kith-auth");
        return;
    };
    let mut rig = Rig::new(&a);
    seed_secrets(&rig, &a);
    rig.lines("people.renamePerson", &json!({"id": "sam", "to": "anna"}))
        .unwrap();
    let (keys, grants, profiles) = secrets_of(&mut rig);
    let py_keys: Vec<String> = py["grants"].as_object().unwrap().keys().cloned().collect();
    // the core reads grants back sorted by agent, so compare the keys as sets and the contents as values
    let mut sorted = py_keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(grants, py["grants"]);
    let narrow = |p: &Value| -> Value {
        let mut o = serde_json::Map::new();
        for (n, prof) in p.as_object().unwrap() {
            let mut q = serde_json::Map::new();
            for k in ["person", "with"] {
                if let Some(v) = prof.get(k) {
                    q.insert(k.to_owned(), v.clone());
                }
            }
            o.insert(n.clone(), Value::Object(q));
        }
        Value::Object(o)
    };
    assert_eq!(narrow(&profiles), narrow(&py["profiles"]));
}
