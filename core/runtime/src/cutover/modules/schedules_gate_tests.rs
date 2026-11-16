#![allow(clippy::unwrap_used)]
//! The build gate of the `schedules` step: each of its four conditions refuses on its own, and the plist is read with a
//! temporary home.

use std::path::Path;

use super::schedules::{RUNS_FIRED_TURNS, Schedules};
use super::schedules_gate::{Facts, MESSAGE, launchd_runs, refusal, serves_rows};
use crate::cutover::Module;

const ALL: Facts = Facts {
    runs_fired_turns: true,
    depends_on_claims: true,
    serves_rows: true,
    launchd_runs_this_build: true,
};

#[test]
fn all_four_conditions_holding_lets_the_step_through() {
    assert_eq!(refusal(ALL), None);
}

#[test]
fn each_condition_refuses_on_its_own_with_the_same_words() {
    let said = Some(MESSAGE.to_owned());
    assert_eq!(
        refusal(Facts {
            runs_fired_turns: false,
            ..ALL
        }),
        said
    );
    assert_eq!(
        refusal(Facts {
            depends_on_claims: false,
            ..ALL
        }),
        said
    );
    assert_eq!(
        refusal(Facts {
            serves_rows: false,
            ..ALL
        }),
        said
    );
    assert_eq!(
        refusal(Facts {
            launchd_runs_this_build: false,
            ..ALL
        }),
        said
    );
    assert_eq!(
        MESSAGE,
        "not stepped: this build can't run the turns its schedules fire; kith-sched keeps running your jobs."
    );
}

#[test]
fn the_runner_flag_is_off_and_the_module_depends_on_claims() {
    // Flips only in the runner's PR, with its own test that a fired job and routine run and end done.
    const { assert!(!RUNS_FIRED_TURNS) };
    assert_eq!(Schedules::new().depends_on(), ["claims"]);
}

#[test]
fn the_method_table_serves_the_three_rows_in_the_core() {
    assert!(serves_rows());
}

fn plist(home: &Path, program: &str) {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dev.kith.serve.plist"),
        format!(
            "<plist><dict><key>Label</key><string>dev.kith.serve</string><key>ProgramArguments</key><array>\n<string>{program}</string>\n<string>serve</string></array></dict></plist>"
        ),
    )
    .unwrap();
}

#[test]
fn launchd_must_start_this_binary_or_one_with_its_build_id() {
    let t = tempfile::tempdir().unwrap();
    let mine = t.path().join("kith-a");
    std::fs::write(&mine, b"build one").unwrap();

    // No plist: no.
    assert!(!launchd_runs(t.path(), &mine));
    // The same path.
    plist(t.path(), &mine.to_string_lossy());
    assert!(launchd_runs(t.path(), &mine));
    // Another path, same bytes: the same build id.
    let copy = t.path().join("kith-b");
    std::fs::write(&copy, b"build one").unwrap();
    plist(t.path(), &copy.to_string_lossy());
    assert!(launchd_runs(t.path(), &mine));
    // Another build.
    let other = t.path().join("kith-c");
    std::fs::write(&other, b"build two").unwrap();
    plist(t.path(), &other.to_string_lossy());
    assert!(!launchd_runs(t.path(), &mine));
    // A program that is not there.
    plist(t.path(), "/no/such/kith-serve");
    assert!(!launchd_runs(t.path(), &mine));
}

#[test]
fn under_the_holder_the_program_after_the_dashes_counts() {
    let t = tempfile::tempdir().unwrap();
    let mine = t.path().join("kith-a");
    std::fs::write(&mine, b"x").unwrap();
    let dir = t.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dev.kith.serve.plist"),
        format!(
            "<key>ProgramArguments</key><array><string>/h/kith-holder</string><string>supervise</string><string>--</string><string>{}</string></array>",
            mine.display()
        ),
    )
    .unwrap();
    assert!(launchd_runs(t.path(), &mine));
}
