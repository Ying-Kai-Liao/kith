#![allow(clippy::unwrap_used)]
//! The build gate of the `schedules` step: each of its four conditions refuses on its own and says which, and the plist
//! is read with a temporary home in the three shapes the installer writes.

use std::path::Path;

use super::schedules::{RUNS_FIRED_TURNS, Schedules};
use super::schedules_gate::{
    Facts, MESSAGE, launchd_runs, launchd_sched_locks, refusal, serves_rows,
};
use crate::cutover::Module;

fn all() -> Facts {
    Facts {
        runs_fired_turns: true,
        depends_on_claims: true,
        serves_rows: true,
        launchd: Ok(()),
        sched_locks: Ok(()),
    }
}

#[test]
fn all_four_conditions_holding_lets_the_step_through() {
    assert_eq!(refusal(&all()), None);
}

#[test]
fn each_condition_refuses_on_its_own_and_names_itself() {
    let said = |f: Facts| refusal(&f).unwrap();
    let a = said(Facts {
        runs_fired_turns: false,
        ..all()
    });
    assert!(
        a.starts_with(MESSAGE) && a.contains("1:") && !a.contains("2:"),
        "{a}"
    );
    let b = said(Facts {
        depends_on_claims: false,
        ..all()
    });
    assert!(
        b.starts_with(MESSAGE) && b.contains("2:") && !b.contains("1:"),
        "{b}"
    );
    let c = said(Facts {
        serves_rows: false,
        ..all()
    });
    assert!(
        c.starts_with(MESSAGE) && c.contains("3:") && c.contains("schedule.fire"),
        "{c}"
    );
    let d = said(Facts {
        launchd: Err("launchd runs /x/old, this binary is /x/new".into()),
        ..all()
    });
    assert!(
        d.starts_with(MESSAGE) && d.contains("4: launchd runs /x/old, this binary is /x/new"),
        "{d}"
    );
    let both = said(Facts {
        runs_fired_turns: false,
        serves_rows: false,
        ..all()
    });
    assert!(both.contains("1:") && both.contains("3:"), "{both}");
    assert_eq!(
        MESSAGE,
        "not stepped: this build can't run the turns its schedules fire; kith-sched keeps running your jobs."
    );
}

#[test]
fn the_runner_flag_is_on_and_the_module_depends_on_claims() {
    // On since the temp-home end-to-end test (`serve/host/tests/schedules_cutover_e2e.rs`); the checklist is on the flag.
    const { assert!(RUNS_FIRED_TURNS) };
    assert_eq!(Schedules::new().depends_on(), ["claims"]);
}

#[test]
fn the_method_table_serves_the_three_rows_in_the_core() {
    assert!(serves_rows());
}

/// The words of `ProgramArguments`, as an XML plist.
fn write_plist(home: &Path, words: &[&str]) {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    let strings = words.join("</string><string>");
    let strings = format!("<string>{strings}</string>");
    std::fs::write(
        dir.join("dev.kith.serve.plist"),
        format!(
            "<plist><dict><key>Label</key><string>dev.kith.serve</string><key>ProgramArguments</key><array>{strings}</array></dict></plist>"
        ),
    )
    .unwrap();
}

/// The three shapes the installer writes around `<kith> serve --port N`: launcher first, holder first (ending in `--`),
/// and both together.
fn shapes(program: &str) -> Vec<Vec<String>> {
    let tail = [program, "serve", "--port", "7001"].map(str::to_owned);
    let launcher =
        ["kith-uv", "run", "--quiet", "--script", "kith-serve-launch"].map(str::to_owned);
    let holder = ["/h/kith-holder", "supervise", "--"].map(str::to_owned);
    let with = |heads: &[&[String]]| -> Vec<String> {
        heads
            .iter()
            .flat_map(|h| h.iter().cloned())
            .chain(tail.clone())
            .collect()
    };
    vec![
        with(&[&launcher]),
        with(&[&holder]),
        with(&[&launcher, &holder]),
    ]
}

fn plist_shape(home: &Path, program: &str, shape: usize) {
    let words = shapes(program).swap_remove(shape);
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    write_plist(home, &refs);
}

#[test]
fn launchd_must_start_this_binary_or_one_with_its_build_id_in_every_installer_shape() {
    for shape in 0..3 {
        let t = tempfile::tempdir().unwrap();
        let mine = t.path().join("kith-a");
        std::fs::write(&mine, b"build one").unwrap();

        // No plist: no.
        assert!(launchd_runs(t.path(), &mine).is_err());
        // The same path.
        plist_shape(t.path(), &mine.to_string_lossy(), shape);
        assert_eq!(launchd_runs(t.path(), &mine), Ok(()), "shape {shape}");
        // Another path, same bytes: the same build id.
        let copy = t.path().join("kith-b");
        std::fs::write(&copy, b"build one").unwrap();
        plist_shape(t.path(), &copy.to_string_lossy(), shape);
        assert_eq!(launchd_runs(t.path(), &mine), Ok(()), "shape {shape}");
        // Another build: refused, showing both programs.
        let other = t.path().join("kith-c");
        std::fs::write(&other, b"build two").unwrap();
        plist_shape(t.path(), &other.to_string_lossy(), shape);
        let why = launchd_runs(t.path(), &mine).unwrap_err();
        assert!(
            why.contains(&other.to_string_lossy().into_owned())
                && why.contains(&mine.to_string_lossy().into_owned()),
            "{why}"
        );
        // A program that is not there.
        plist_shape(t.path(), "/no/such/kith", shape);
        assert!(launchd_runs(t.path(), &mine).is_err());
    }
}

#[test]
fn a_launcher_or_holder_word_is_not_taken_for_the_program() {
    let t = tempfile::tempdir().unwrap();
    // The launcher is this binary's bytes; the real program is another build. Reading the first word would say yes.
    let mine = t.path().join("kith-a");
    std::fs::write(&mine, b"build one").unwrap();
    let other = t.path().join("kith-c");
    std::fs::write(&other, b"build two").unwrap();
    let words = [
        mine.to_str().unwrap(),
        "supervise",
        "--",
        other.to_str().unwrap(),
        "serve",
        "--port",
        "7001",
    ];
    write_plist(t.path(), &words);
    assert!(launchd_runs(t.path(), &mine).is_err());
    // No `serve --port`: no program found.
    write_plist(t.path(), &[mine.to_str().unwrap(), "serve"]);
    assert!(launchd_runs(t.path(), &mine).is_err());
}

fn write_sched_plist(home: &Path, words: &[&str]) {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).unwrap();
    let strings = format!("<string>{}</string>", words.join("</string><string>"));
    std::fs::write(
        dir.join("dev.kith.sched.plist"),
        format!("<plist><dict><key>ProgramArguments</key><array>{strings}</array></dict></plist>"),
    )
    .unwrap();
}

const MARKED: &str =
    "#!/usr/bin/env python3\nKITH_SCHED_TICK_LOCK = 1  # honours sched/tick.lock\n";

#[test]
fn launchds_kith_sched_must_carry_the_tick_lock_marker() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    // Missing plist.
    let missing = launchd_sched_locks(home).unwrap_err();
    assert!(
        missing.contains("no kith-sched program found in")
            && missing.ends_with("; run `kith-sched --install`"),
        "{missing}"
    );
    // Through a `current` symlink, with the launcher words in front.
    let v = home.join("cli/v2/bin");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(v.join("kith-sched"), MARKED).unwrap();
    std::os::unix::fs::symlink(home.join("cli/v2"), home.join("cli/current")).unwrap();
    let via = home.join("cli/current/bin/kith-sched");
    let via = via.to_str().unwrap();
    write_sched_plist(
        home,
        &["kith-uv", "run", "--quiet", "--script", via, "tick"],
    );
    assert_eq!(launchd_sched_locks(home), Ok(()));
    // Direct program.
    write_sched_plist(home, &[via, "tick"]);
    assert_eq!(launchd_sched_locks(home), Ok(()));
    // No marker: refused, naming the program.
    std::fs::write(v.join("kith-sched"), "#!/usr/bin/env python3\n").unwrap();
    let why = launchd_sched_locks(home).unwrap_err();
    assert!(why.contains(via) && why.contains("tick.lock"), "{why}");
    // Not utf-8, and a program that is not there.
    std::fs::write(v.join("kith-sched"), [0xff, 0xfe]).unwrap();
    assert!(launchd_sched_locks(home).is_err());
    write_sched_plist(home, &["/no/such/kith-sched", "tick"]);
    assert!(launchd_sched_locks(home).is_err());
}

#[test]
fn a_marked_kith_sched_beside_this_binary_does_not_stand_in_for_launchds() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let beside = home.join("beside");
    std::fs::create_dir_all(&beside).unwrap();
    std::fs::write(beside.join("kith-sched"), MARKED).unwrap();
    let old = home.join("old-kith-sched");
    std::fs::write(&old, "#!/usr/bin/env python3\n").unwrap();
    write_sched_plist(home, &[old.to_str().unwrap(), "tick"]);
    assert!(launchd_sched_locks(home).is_err());
}

#[test]
fn condition_five_refuses_on_its_own() {
    let r = refusal(&Facts {
        sched_locks: Err(
            "launchd's kith-sched (/x) does not take sched/tick.lock; update kith first".into(),
        ),
        ..all()
    })
    .unwrap();
    assert!(
        r.starts_with(MESSAGE) && r.contains("5: launchd's kith-sched") && !r.contains("4:"),
        "{r}"
    );
}
