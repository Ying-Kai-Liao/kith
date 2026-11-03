//! The schema steps of core.db, one file per step, collected into the ordered list `db.rs` opens a database with.
//!
//! Version numbers are reserved per lane, so parallel work never picks the same one and never edits another lane's
//! file (the M2 lanes of docs/plans/one-bot-model.md; M2-0b made the split):
//!
//! | versions | lane                                   | where                      |
//! |----------|----------------------------------------|----------------------------|
//! | 1-9      | core (M0)                              | `m0001_core.rs`            |
//! | 10-19    | A authority                            | `authority/`               |
//! | 20-29    | B secrets                              | `secrets/`                 |
//! | 30-39    | D gate and sync lease                  | `gate/`                    |
//! | 40-49    | E cards                                | `cards/`                   |
//! | 50-59    | F run tokens                           | `run_tokens/`              |
//! | 60-69    | G doors                                | `doors/`                   |
//! | 70-79    | C import bookkeeping                   | `import/`                  |
//! | 80+      | spare (a new lane takes the next 10)   |                            |
//!
//! A lane's versions start at its first number and run without a gap; the list is strictly increasing and no number
//! appears twice ([`check`], run by a test on the real list and on bad ones). A step that has shipped is never edited.
//!
//! A database that applied a later lane's step before an earlier lane's step was merged would skip the earlier one
//! (`db::apply` runs the steps above the highest version it has). That cannot happen inside one release, because every
//! lane is merged before the release migrates anything; a machine running a build in between rebuilds its core.db.

#[derive(Clone, Copy, Debug)]
pub(crate) struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

mod authority;
mod cards;
mod doors;
mod gate;
mod import;
mod m0001_core;
mod run_tokens;
mod secrets;

/// A lane's reserved range of versions.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lane {
    pub name: &'static str,
    pub first: i64,
    pub last: i64,
}

#[cfg(test)]
pub(crate) const LANES: &[Lane] = &[
    Lane {
        name: "core",
        first: 1,
        last: 9,
    },
    Lane {
        name: "authority",
        first: 10,
        last: 19,
    },
    Lane {
        name: "secrets",
        first: 20,
        last: 29,
    },
    Lane {
        name: "gate",
        first: 30,
        last: 39,
    },
    Lane {
        name: "cards",
        first: 40,
        last: 49,
    },
    Lane {
        name: "run_tokens",
        first: 50,
        last: 59,
    },
    Lane {
        name: "doors",
        first: 60,
        last: 69,
    },
    Lane {
        name: "import",
        first: 70,
        last: 79,
    },
    Lane {
        name: "spare",
        first: 80,
        last: i64::MAX,
    },
];

const CORE: &[Migration] = &[m0001_core::MIGRATION];

/// The lanes' steps, in version order.
const PARTS: &[&[Migration]] = &[
    CORE,
    authority::STEPS,
    secrets::STEPS,
    gate::STEPS,
    cards::STEPS,
    run_tokens::STEPS,
    doors::STEPS,
    import::STEPS,
];

const LEN: usize = {
    let mut n = 0;
    let mut p = 0;
    while p < PARTS.len() {
        n += PARTS[p].len();
        p += 1;
    }
    n
};

const fn flatten() -> [Migration; LEN] {
    // Any step fills the array; every slot is overwritten below.
    let mut out = [CORE[0]; LEN];
    let mut n = 0;
    let mut p = 0;
    while p < PARTS.len() {
        let mut i = 0;
        while i < PARTS[p].len() {
            out[n] = PARTS[p][i];
            n += 1;
            i += 1;
        }
        p += 1;
    }
    out
}

const ALL: [Migration; LEN] = flatten();

/// Every schema this binary knows, oldest first. Add a step as a file in your lane; never edit a step that has shipped.
pub(crate) const MIGRATIONS: &[Migration] = &ALL;

/// Why a list of steps is not a valid schema history, if it is not: a name or version twice, a version out of order or
/// outside every lane, or a gap inside a lane (a lane's first step is its first number).
#[cfg(test)]
pub(crate) fn check(steps: &[Migration]) -> Result<(), String> {
    let in_lane = |l: &Lane, v: i64| (l.first..=l.last).contains(&v);
    let mut names = std::collections::BTreeSet::new();
    for (i, m) in steps.iter().enumerate() {
        if !names.insert(m.name) {
            return Err(format!("name {:?} is used twice", m.name));
        }
        if i > 0 {
            let before = steps[i - 1].version;
            if m.version == before {
                return Err(format!("version {} is claimed twice", m.version));
            }
            if m.version < before {
                return Err(format!("version {} comes after {before}", m.version));
            }
        }
        let lane = LANES
            .iter()
            .find(|l| in_lane(l, m.version))
            .ok_or_else(|| format!("version {} is in no lane", m.version))?;
        let want = steps[..i]
            .iter()
            .rev()
            .find(|p| in_lane(lane, p.version))
            .map_or(lane.first, |p| p.version + 1);
        if m.version != want {
            return Err(format!(
                "lane {}: version {} where {want} is next (a gap, or a lane's first step not at its first number)",
                lane.name, m.version
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
