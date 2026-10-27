//! Promotion over an agent's files: a retag that keeps the readers is applied and undoable, one that adds readers
//! waits for the person, and the approval is the only way a wider audience is read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::tidy::{self, Answered};
use super::*;
use kith_mem::rusqlite::params;
use kith_mem::tidy::{self as kd, Audience, NoteFacets, Plan};

struct Home {
    root: PathBuf,
    layout: Layout,
}

impl Home {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kith-hqmem-promote-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("agents/builder/memory/people/sam")).unwrap();
        let root = root.canonicalize().unwrap();
        let layout = Layout::new(root.join("state"), root.join("agents"));
        let home = Self { root, layout };
        home.set("MEMORY.md", "# Memory\n\n- Likes tea\n");
        home.set("plans.md", "---\ndescription: Plans\n---\n- Osaka in October\n");
        home.set("people/sam/trip.md", "---\ndescription: Sam's trip\n---\n- Sam flies to Osaka\n");
        home
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join("agents/builder/memory").join(name)
    }

    fn set(&self, name: &str, text: &str) {
        std::fs::write(self.path(name), text).unwrap();
    }

    fn text(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap()
    }

    fn conn(&self) -> Connection {
        open(&self.layout).unwrap()
    }

    fn load(&self) -> tidy::Material {
        let conn = self.conn();
        tidy::load(&self.layout, "builder", "2026-10-23", None).unwrap().unwrap().with_promotions(&conn)
    }

    /// The facets the kernel would read for a note.
    fn facets(&self, name: &str) -> NoteFacets {
        let conn = self.conn();
        let text = self.text(name);
        NoteFacets::read("builder", name, &text, &|id, a| tidy::granted(&conn, "builder", name, id, a))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn plan(m: &tidy::Material, retags: &serde_json::Value) -> Plan {
    let plan = kd::parse(&serde_json::json!({"promote": retags}).to_string()).unwrap();
    let (kept, rejected) = kd::audit(&m.snapshot, &[], plan);
    assert_eq!(rejected, Vec::<String>::new());
    kept
}

#[test]
fn a_retag_that_keeps_the_readers_is_applied_logged_and_undone() {
    let home = Home::new();
    let m = home.load();
    assert!(m.snapshot.facets.iter().any(|f| f.file == "people/sam/trip.md" && f.audience.to_string() == "person:sam"));
    let kept = plan(
        &m,
        &serde_json::json!([
            {"file": "people/sam/trip.md", "audience": "owner", "projects": ["japan"], "reason": "private trip"},
            {"file": "plans.md", "projects": ["japan"]},
        ]),
    );
    let conn = home.conn();
    let applied = tidy::apply(&conn, &m, &kept).unwrap();
    assert!(applied.asked.is_empty() && applied.dropped.is_empty(), "{applied:?}");
    let text = home.text("people/sam/trip.md");
    assert!(text.contains("audience: owner") && text.contains("projects: [japan]"), "{text}");
    assert!(text.contains("approved_by: tidy") && text.contains("- Sam flies to Osaka"), "{text}");
    assert_eq!(home.facets("people/sam/trip.md").audience, Audience::Owner);
    assert_eq!(home.facets("plans.md").projects, ["japan"]);

    let runs = tidy::runs(&conn, "builder", 5).unwrap();
    let changes = runs[0]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|c| c["kind"] == "retagged"), "{changes:?}");
    let report = tidy::undo(&conn, &home.layout, "builder", applied.tidy.unwrap()).unwrap();
    assert!(report.undone);
    assert_eq!(home.facets("people/sam/trip.md").audience.to_string(), "person:sam");
    assert!(!home.text("people/sam/trip.md").contains("audience"));
}

#[test]
fn a_widening_retag_waits_for_the_person_and_is_asked_once() {
    let home = Home::new();
    home.set("people/sam/mood.md", "---\ndescription: Mood\n---\n- Sam is shy\n");
    let m = home.load();
    let wide = serde_json::json!([
        {"file": "plans.md", "audience": "shared", "reason": "the team needs it"},
        {"file": "people/sam/mood.md", "audience": "shared"},
    ]);
    let kept = plan(&m, &wide);
    assert_eq!(kept.promote.len(), 2, "the audit keeps both: the host asks");
    let conn = home.conn();
    let applied = tidy::apply(&conn, &m, &kept).unwrap();
    assert_eq!(applied.asked.len(), 2);
    assert!(applied.tidy.is_none(), "nothing was written, so there is nothing to undo");
    assert!(!home.text("plans.md").contains("audience"));
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);

    let waiting = tidy::promotions(&conn, "builder", "pending", 10).unwrap();
    let asks: Vec<_> = waiting.iter().map(|p| (p["file"].as_str().unwrap(), p["asks"].as_str().unwrap())).collect();
    assert!(asks.contains(&("plans.md", "")) && asks.contains(&("people/sam/mood.md", "sam")), "{asks:?}");
    // The same proposal again is not asked again, and nor is one the person answered "keep private" to.
    let again = tidy::apply(&conn, &m, &plan(&m, &wide)).unwrap();
    assert_eq!(again.asked, Vec::<i64>::new());
    let id = waiting.iter().find(|p| p["file"] == "plans.md").unwrap()["id"].as_i64().unwrap();
    let kept_private = tidy::answer(&conn, &home.layout, "builder", id, false, "").unwrap();
    assert_eq!(kept_private, Answered { status: "declined", run: None });
    assert_eq!(tidy::apply(&conn, &m, &plan(&m, &wide)).unwrap().asked, Vec::<i64>::new());
}

#[test]
fn only_the_person_asked_can_approve_and_the_approval_is_what_makes_it_wider() {
    let home = Home::new();
    home.set("people/sam/mood.md", "---\ndescription: Mood\n---\n- Sam is shy\n");
    let m = home.load();
    let kept = plan(
        &m,
        &serde_json::json!([
            {"file": "plans.md", "audience": "shared"},
            {"file": "people/sam/mood.md", "audience": "shared"},
        ]),
    );
    let conn = home.conn();
    tidy::apply(&conn, &m, &kept).unwrap();
    let waiting = tidy::promotions(&conn, "builder", "pending", 10).unwrap();
    let id = |file: &str| waiting.iter().find(|p| p["file"] == file).unwrap()["id"].as_i64().unwrap();

    // Sam's note is Sam's to share: the owner can't approve it, and neither can another person.
    assert!(tidy::answer(&conn, &home.layout, "builder", id("people/sam/mood.md"), true, "").is_err());
    assert!(tidy::answer(&conn, &home.layout, "builder", id("people/sam/mood.md"), true, "eve").is_err());
    assert!(!home.text("people/sam/mood.md").contains("shared"));

    // A hand-edited wider audience, or one that names an unapproved change, is not read.
    home.set("plans.md", "---\naudience: shared\napproved_change: 1\n---\n- Osaka\n");
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
    home.set("plans.md", "---\ndescription: Plans\n---\n- Osaka in October\n");

    let done = tidy::answer(&conn, &home.layout, "builder", id("plans.md"), true, "").unwrap();
    assert_eq!(done.status, "approved");
    let text = home.text("plans.md");
    assert!(
        text.contains("audience: shared") && text.contains(&format!("approved_change: {}", id("plans.md"))),
        "{text}"
    );
    assert!(text.contains("approved_by: owner"), "{text}");
    assert_eq!(home.facets("plans.md").audience, Audience::Shared);
    // The change reads as a tidy-up in the log, and undoing it takes the wider audience away again.
    let runs = tidy::runs(&conn, "builder", 5).unwrap();
    assert_eq!(runs[0]["changes"][0]["kind"], "retagged");
    tidy::undo(&conn, &home.layout, "builder", done.run.unwrap()).unwrap();
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
    // Answered once: a second answer finds nothing waiting.
    assert!(tidy::answer(&conn, &home.layout, "builder", id("plans.md"), true, "").is_err());

    // Sam approves Sam's note.
    let done = tidy::answer(&conn, &home.layout, "builder", id("people/sam/mood.md"), true, "sam").unwrap();
    assert_eq!(done.status, "approved");
    assert_eq!(home.facets("people/sam/mood.md").audience, Audience::Shared);
}

#[test]
fn the_check_runs_again_at_apply_and_at_the_answer() {
    let home = Home::new();
    let mut m = home.load();
    // The audit saw the note as shared, where a move to Sam alone narrows it; on disk it is the owner's, where the
    // same move adds a reader. Apply judges the note as it is now.
    for f in &mut m.snapshot.facets {
        if f.file == "plans.md" {
            f.audience = Audience::Shared;
        }
    }
    let kept = plan(&m, &serde_json::json!([{"file": "plans.md", "audience": "person:sam"}]));
    let conn = home.conn();
    let applied = tidy::apply(&conn, &m, &kept).unwrap();
    assert_eq!(applied.asked.len(), 1);
    assert!(!home.text("plans.md").contains("audience"));

    // The note's audience is not what it was when the question was put: the approval counts for nothing.
    let id = tidy::promotions(&conn, "builder", "pending", 10).unwrap()[0]["id"].as_i64().unwrap();
    conn.execute("UPDATE hq_promotions SET from_audience = 'shared' WHERE id = ?1", [id]).unwrap();
    let done = tidy::answer(&conn, &home.layout, "builder", id, true, "").unwrap();
    assert_eq!(done, Answered { status: "stale", run: None });
    assert!(!home.text("plans.md").contains("person:sam"));
}

#[test]
fn a_note_with_no_facets_or_an_unknown_room_is_owner_only() {
    let home = Home::new();
    let m = home.load();
    assert!(
        m.snapshot
            .facets
            .iter()
            .filter(|f| f.file == "plans.md" || f.file == "MEMORY.md")
            .all(|f| f.audience == Audience::Owner)
    );
    // Moving it into a room whose members nobody can tell is neither applied nor put to the person.
    let kept = plan(&m, &serde_json::json!([{"file": "plans.md", "audience": "room:grp1"}]));
    let conn = home.conn();
    let applied = tidy::apply(&conn, &m, &kept).unwrap();
    assert_eq!(applied.asked, Vec::<i64>::new());
    assert_eq!(tidy::promotions(&conn, "builder", "pending", 10).unwrap().len(), 0);
    assert!(!home.text("plans.md").contains("audience"));
}

#[test]
fn an_older_approval_does_not_come_back_after_a_newer_retag() {
    let home = Home::new();
    let conn = home.conn();
    let m = home.load();
    tidy::apply(&conn, &m, &plan(&m, &serde_json::json!([{"file": "plans.md", "audience": "shared"}]))).unwrap();
    let id = tidy::promotions(&conn, "builder", "pending", 10).unwrap()[0]["id"].as_i64().unwrap();
    tidy::answer(&conn, &home.layout, "builder", id, true, "").unwrap();
    let wide = home.text("plans.md");
    assert_eq!(home.facets("plans.md").audience, Audience::Shared);
    // Narrowed to the owner: a newer change exists.
    let m = home.load();
    tidy::apply(&conn, &m, &plan(&m, &serde_json::json!([{"file": "plans.md", "audience": "owner"}]))).unwrap();
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
    // Putting the old text back (the old approved_change) does not widen it again.
    home.set("plans.md", &wide);
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
}

/// Approves `plans.md` as shared through the host and returns the change id.
fn approve_plans(home: &Home, conn: &Connection) -> i64 {
    let m = home.load();
    tidy::apply(conn, &m, &plan(&m, &serde_json::json!([{"file": "plans.md", "audience": "shared"}]))).unwrap();
    let id = tidy::promotions(conn, "builder", "pending", 10).unwrap()[0]["id"].as_i64().unwrap();
    tidy::answer(conn, &home.layout, "builder", id, true, "").unwrap();
    assert_eq!(home.facets("plans.md").audience, Audience::Shared);
    id
}

#[test]
fn a_row_written_into_the_database_by_hand_grants_nothing() {
    let home = Home::new();
    let conn = home.conn();
    let real = approve_plans(&home, &conn);
    let (dream, sig): (i64, String) = conn
        .query_row("SELECT dream, sig FROM hq_promotions WHERE id = ?1", [real], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let key = home.root.join("state/secrets/memory-approvals.key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // A forged approval of Sam's note, unsigned, then carrying the real row's signature: neither widens it.
    conn.execute(
        "INSERT INTO hq_promotions (agent, file, from_audience, audience, status, created_at, dream)
         VALUES ('builder', 'people/sam/trip.md', 'person:sam', 'shared', 'approved', 0, ?1)",
        [dream],
    )
    .unwrap();
    let forged = conn.last_insert_rowid();
    home.set(
        "people/sam/trip.md",
        &format!("---\naudience: shared\napproved_change: {forged}\n---\n- Sam flies to Osaka\n"),
    );
    assert_eq!(home.facets("people/sam/trip.md").audience.to_string(), "person:sam");
    conn.execute("UPDATE hq_promotions SET sig = ?1 WHERE id = ?2", params![sig, forged]).unwrap();
    assert_eq!(home.facets("people/sam/trip.md").audience.to_string(), "person:sam");

    // The real row moved to another audience no longer matches its signature.
    conn.execute("UPDATE hq_promotions SET audience = 'person:eve' WHERE id = ?1", [real]).unwrap();
    home.set("plans.md", &format!("---\naudience: person:eve\napproved_change: {real}\n---\n- Osaka\n"));
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
    conn.execute("UPDATE hq_promotions SET audience = 'shared' WHERE id = ?1", [real]).unwrap();
    home.set("plans.md", &format!("---\naudience: shared\napproved_change: {real}\n---\n- Osaka\n"));
    assert_eq!(home.facets("plans.md").audience, Audience::Shared);

    // With the key gone nothing is granted: the most private reading.
    std::fs::remove_file(&key).unwrap();
    assert_eq!(home.facets("plans.md").audience, Audience::Owner);
}

#[test]
fn a_waiting_or_declined_question_flipped_to_approved_grants_nothing() {
    let home = Home::new();
    home.set("people/sam/mood.md", "---\ndescription: Mood\n---\n- Sam is shy\n");
    let conn = home.conn();
    approve_plans(&home, &conn);
    let m = home.load();
    tidy::apply(&conn, &m, &plan(&m, &serde_json::json!([{"file": "people/sam/mood.md", "audience": "shared"}])))
        .unwrap();
    let id = tidy::promotions(&conn, "builder", "pending", 10).unwrap()[0]["id"].as_i64().unwrap();
    let dream: i64 = conn.query_row("SELECT max(id) FROM memory_dreams", [], |r| r.get(0)).unwrap();
    conn.execute("UPDATE hq_promotions SET status = 'approved', dream = ?1 WHERE id = ?2", params![dream, id]).unwrap();
    home.set("people/sam/mood.md", &format!("---\naudience: shared\napproved_change: {id}\n---\n- Sam is shy\n"));
    assert_eq!(home.facets("people/sam/mood.md").audience.to_string(), "person:sam");
}

#[test]
fn approvals_from_before_signing_are_signed_once_at_the_upgrade() {
    let home = Home::new();
    let conn = home.conn();
    let id = approve_plans(&home, &conn);
    // The database as an older host left it: no signatures, no key.
    conn.execute_batch("ALTER TABLE hq_promotions DROP COLUMN sig").unwrap();
    std::fs::remove_file(home.root.join("state/secrets/memory-approvals.key")).unwrap();
    drop(conn);
    let conn = home.conn();
    let sig: Option<String> =
        conn.query_row("SELECT sig FROM hq_promotions WHERE id = ?1", [id], |r| r.get(0)).unwrap();
    assert!(sig.is_some());
    assert_eq!(home.facets("plans.md").audience, Audience::Shared);
}

#[test]
fn the_signature_is_the_kernels() {
    // The same answer as kernel/lib/kith_facets.py's `signature` (kernel/tests/kith-recall-check).
    let key: Vec<u8> = (0..32).collect();
    assert_eq!(
        tidy::signature(&key, 7, "echo", "people/sam/ü.md", "room:grp1").unwrap(),
        "8f50a51b1e41ae8cb8f5488e7dff393bba6b281b275075cf6c122932aa9c9c7f"
    );
}
