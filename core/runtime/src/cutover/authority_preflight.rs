//! The authority step's preflight: what "zero" counts before `kith migrate step authority` writes anything.
//!
//! Six read-only items, each printed with its count. Items 1 to 5 stop the step (the dry run reports not-ready) and name
//! the ids; item 6 is a listing.
//!
//! 1. the import (dry run) refuses nothing;
//! 2. `check authority` finds 0 differences (the compare of core.db with auth.json; skipped while the core holds no people
//!    yet, because the step's import is what fills it and the step runs this compare itself right after it);
//! 3. `open_removals` is empty (a removal that has not finished its revoke order);
//! 4. no credential record in serve.db is waiting to move into the vault;
//! 5. no live claim and no open ask is held by an id that is not a current `people` id (cores from before ids were swept,
//!    a suspend or rename the import did not see);
//! 6. every live claim or open ask of a non-owner id (a bot never retired, a guest), listed.
//!
//! "Live" is a claim whose state is not `ended` and an ask whose state is `open`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rusqlite::Connection;

use super::{Env, Module, authority};
use crate::error::{Error, Result};

/// One line of the preflight.
pub(crate) struct Item {
    pub n: u8,
    pub what: &'static str,
    pub ids: Vec<String>,
    /// A non-zero count stops the step.
    pub stops: bool,
    /// Why the item was not counted. A skipped item never reads as a pass.
    pub skipped: Option<String>,
}

pub(crate) struct Preflight {
    pub items: Vec<Item>,
    /// Lines that are not items (the module is switched, no core.db yet).
    pub notes: Vec<String>,
}

impl Preflight {
    pub(crate) fn ready(&self) -> bool {
        self.items.iter().all(|i| !i.stops || i.ids.is_empty())
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut out = self.notes.clone();
        for i in &self.items {
            let mut l = if let Some(why) = &i.skipped {
                format!("preflight {}. {}: skipped: {why}", i.n, i.what)
            } else {
                format!("preflight {}. {}: {}", i.n, i.what, i.ids.len())
            };
            if i.skipped.is_none() && !i.ids.is_empty() {
                let _ = write!(l, " ({})", i.ids.join(", "));
            }
            out.push(l);
        }
        let skipped2 = self.items.iter().any(|i| i.n == 2 && i.skipped.is_some());
        out.push(if self.ready() && skipped2 {
            "preflight: ready; item 2 was skipped, so the step will compare after its own import (not 0 differences yet)"
                .to_owned()
        } else if self.ready() {
            "preflight: ready (items 1 to 5 are zero)".to_owned()
        } else {
            "preflight: NOT ready; the items above with a count must be zero first".to_owned()
        });
        out
    }
}

fn has_table(db: &Connection, name: &str) -> bool {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

fn strings(db: &Connection, sql: &str) -> Result<Vec<String>> {
    let io = |e: rusqlite::Error| Error::Io(format!("authority preflight: {e}"));
    db.prepare(sql)
        .map_err(io)?
        .query_map([], |r| r.get(0))
        .map_err(io)?
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(io)
}

/// Item 3: the people whose removal is still recorded as open.
pub(crate) fn open_removals(db: &Connection) -> Result<Vec<String>> {
    if !has_table(db, "open_removals") {
        return Ok(Vec::new());
    }
    strings(db, "SELECT person FROM open_removals ORDER BY person")
}

/// The ids that hold a live claim or an open ask, each with how many: `(id, claims, asks)`. An empty id (a claim with no
/// person) is nobody's and not listed.
fn holders(db: &Connection) -> Result<BTreeMap<String, (u32, u32)>> {
    let io = |e: rusqlite::Error| Error::Io(format!("authority preflight: {e}"));
    let mut out: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (table, sql, claim) in [
        (
            "claims",
            "SELECT person, COUNT(*) FROM claims WHERE state <> 'ended' AND person <> '' GROUP BY person",
            true,
        ),
        (
            "asks",
            "SELECT caller_person, COUNT(*) FROM asks WHERE state = 'open' AND caller_person IS NOT NULL AND caller_person <> '' GROUP BY caller_person",
            false,
        ),
    ] {
        if !has_table(db, table) {
            continue;
        }
        let rows = db
            .prepare(sql)
            .map_err(io)?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))
            .map_err(io)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(io)?;
        for (id, n) in rows {
            let e = out.entry(id).or_default();
            if claim {
                e.0 = n;
            } else {
                e.1 = n;
            }
        }
    }
    Ok(out)
}

fn describe(id: &str, (claims, asks): (u32, u32)) -> String {
    format!("{id}: {claims} claim(s), {asks} ask(s)")
}

/// Items 5 and 6: `(held by an id that is not a current person, held by a non-owner)`.
pub(crate) fn live_holders(db: &Connection) -> Result<(Vec<String>, Vec<String>)> {
    let people: BTreeMap<String, bool> = if has_table(db, "people") {
        let io = |e: rusqlite::Error| Error::Io(format!("authority preflight: {e}"));
        db.prepare("SELECT id, owner FROM people")
            .map_err(io)?
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? == 1))
            })
            .map_err(io)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(io)?
            .into_iter()
            .collect()
    } else {
        BTreeMap::new()
    };
    let (mut stray, mut non_owner) = (Vec::new(), Vec::new());
    for (id, n) in holders(db)? {
        match people.get(&id) {
            None => {
                stray.push(describe(&id, n));
                non_owner.push(describe(&id, n));
            }
            Some(false) => non_owner.push(describe(&id, n)),
            Some(true) => {}
        }
    }
    Ok((stray, non_owner))
}

/// The preflight over what the dry-run import said (`import_refusal`: the error text when it refused) and the core, if
/// there is one. `compare` is false for the real step. Reads only.
pub(crate) fn run(
    m: &dyn Module,
    env: &Env,
    import_refusal: Option<String>,
    db: Option<&Connection>,
    compare: bool,
) -> Result<Preflight> {
    let mut items = vec![Item {
        n: 1,
        what: "the import refuses",
        ids: import_refusal.into_iter().collect(),
        stops: true,
        skipped: None,
    }];
    let mut notes = Vec::new();
    let Some(db) = db else {
        notes.push(
            "preflight: there is no core.db yet; items 2 to 6 have nothing to count (the step makes it)"
                .to_owned(),
        );
        return Ok(Preflight { items, notes });
    };
    let filled = has_table(db, "people")
        && db
            .query_row("SELECT COUNT(*) FROM people", [], |r| r.get::<_, i64>(0))
            .is_ok_and(|n| n > 0);
    let ctx = env
        .ctx
        .as_ref()
        .ok_or_else(|| Error::Refused("authority: no folders to look in".into()))?;
    let unmoved = authority::unmoved_vault_rows(ctx, db)?.unwrap_or_default();
    let (diffs, skipped) = if !compare {
        // The step compares right after its own import; before it, the core may be part-way through a re-run.
        (
            Vec::new(),
            Some("the step compares after its own import".to_owned()),
        )
    } else if !unmoved.is_empty() {
        // `same_answer` refuses while credential records have not moved; item 4 names them.
        (
            Vec::new(),
            Some("item 4 is not zero; the step will compare after its own import".to_owned()),
        )
    } else if filled {
        (m.same_answer(env, db)?, None)
    } else {
        (
            Vec::new(),
            Some(
                "the core holds no people yet; the step will compare after its own import"
                    .to_owned(),
            ),
        )
    };
    items.push(Item {
        n: 2,
        what: "differences between the core and auth.json (check authority)",
        ids: diffs,
        stops: true,
        skipped,
    });
    items.push(Item {
        n: 3,
        what: "open removals",
        ids: open_removals(db)?,
        stops: true,
        skipped: None,
    });
    items.push(Item {
        n: 4,
        what: "credential records in serve.db that have not moved into the vault",
        ids: unmoved,
        stops: true,
        skipped: None,
    });
    let (stray, non_owner) = live_holders(db)?;
    items.push(Item {
        n: 5,
        what: "live claims or open asks held by an id that is not a current person",
        ids: stray,
        stops: true,
        skipped: None,
    });
    items.push(Item {
        n: 6,
        what: "live claims or open asks of a non-owner (listing only)",
        ids: non_owner,
        stops: false,
        skipped: None,
    });
    Ok(Preflight { items, notes })
}
