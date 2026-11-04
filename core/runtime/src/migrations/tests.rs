use super::{LANES, Lane, MIGRATIONS, Migration, check, check_files, check_lane};

const fn step(version: i64, name: &'static str) -> Migration {
    Migration {
        version,
        name,
        sql: "",
    }
}

#[test]
fn the_real_list_is_a_valid_history() {
    check(MIGRATIONS).expect("MIGRATIONS");
    check_files().expect("each file in its lane");
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[0].name, "core");
}

#[test]
fn version_one_is_unchanged_byte_for_byte() {
    // Real databases hold this step; the text is pinned here so an edit shows up as a failing test.
    let old = "
        CREATE TABLE migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at INTEGER NOT NULL
        );
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
        INSERT INTO counters (name, value) VALUES ('generation', 0), ('claim_id', 0);
    ";
    assert_eq!(MIGRATIONS[0].sql, old);
}

#[test]
fn lanes_do_not_overlap() {
    for (i, a) in LANES.iter().enumerate() {
        assert!(a.first <= a.last, "{}", a.name);
        for b in &LANES[i + 1..] {
            assert!(a.last < b.first, "{} and {} share a number", a.name, b.name);
        }
    }
}

#[test]
fn a_gap_is_refused() {
    let e = check(&[step(1, "core"), step(3, "x")]).expect_err("refused");
    assert!(e.contains("gap"), "{e}");
    // A lane's first number is fine; a lane that starts above its first number is not.
    check(&[step(1, "core"), step(10, "a")]).expect("lane start");
    assert!(check(&[step(1, "core"), step(11, "a")]).is_err());
    check(&[step(1, "core"), step(10, "a"), step(11, "b"), step(20, "c")]).expect("two lanes");
}

#[test]
fn a_duplicate_is_refused() {
    // Two files claiming one number.
    let e = check(&[step(1, "core"), step(10, "a"), step(10, "b")]).expect_err("refused");
    assert!(e.contains("twice"), "{e}");
    assert!(
        check(&[step(1, "core"), step(10, "a"), step(11, "a")]).is_err(),
        "a name twice"
    );
}

#[test]
fn an_out_of_order_version_is_refused() {
    let e = check(&[step(1, "core"), step(20, "b"), step(10, "a")]).expect_err("refused");
    assert!(e.contains("after"), "{e}");
}

#[test]
fn a_version_outside_every_lane_is_refused() {
    assert!(check(&[step(0, "zero")]).is_err());
}

#[test]
fn a_step_outside_its_files_lane_is_refused() {
    let lane = Lane {
        name: "authority",
        first: 10,
        last: 19,
    };
    check_lane(&lane, &[step(10, "a"), step(11, "b")]).expect("inside");
    let e = check_lane(&lane, &[step(10, "a"), step(20, "b")]).expect_err("outside");
    assert!(e.contains("outside its lane"), "{e}");
}

#[test]
fn the_m3_lanes_are_reserved_in_order() {
    let want = [
        ("conversations", 80, 89),
        ("wake", 90, 99),
        ("children", 100, 104),
        ("accounts", 105, 109),
    ];
    for (name, first, last) in want {
        let l = LANES.iter().find(|l| l.name == name).expect(name);
        assert_eq!((l.first, l.last), (first, last), "{name}");
    }
    // Only conversations has steps so far; the others stay empty until their lane's PR.
    let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
    assert!(versions.contains(&80));
    assert!(
        versions.iter().all(|v| !(90..=109).contains(v)),
        "{versions:?}"
    );
    let spare = LANES.last().expect("lanes");
    assert_eq!((spare.name, spare.first), ("spare", 110));
}
