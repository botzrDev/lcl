//! Ordered schema migrations.
//!
//! `MIGRATIONS[i]` takes a database from schema v(i) to v(i+1), and
//! `PRAGMA user_version` records which version a file is at. `Store::open`
//! applies the pending tail of this list inside one `BEGIN IMMEDIATE`
//! transaction.
//!
//! Append only: never edit or reorder a migration once it has shipped. A
//! database that already ran it will never run it again, so an edit silently
//! forks the schema.

use crate::error::Result;
use crate::parse;
use rusqlite::{params, Row, Transaction};
use std::collections::{HashMap, HashSet};

/// The schema version this build reads and writes. Derived from the list, so
/// adding a migration bumps it and the two can't drift apart.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// One step of `MIGRATIONS`.
pub(crate) enum Migration {
    /// SQL, run as one batch.
    Sql(&'static str),
    /// Rust, for a step that must read values the way lcl's own parsers do.
    /// It returns what it changed, as phrases for the one line `db::migrate`
    /// hands back to whoever opened the file; empty when it changed nothing.
    Code(fn(&Transaction<'_>) -> Result<Vec<String>>),
}

use Migration::{Code, Sql};

pub(crate) const MIGRATIONS: &[Migration] = &[Sql(V1), Sql(V2), Sql(V3), Sql(V4), Sql(V5), Sql(V6), Sql(V7), Sql(V8), Sql(V9), Code(v10)];

/// v0 -> v1: the lcl 0.1.0 schema, verbatim.
///
/// 0.1.0 ran this DDL on every open and never set `user_version`, so a 0.1.0
/// database is at v0 and already has every object here. `IF NOT EXISTS` makes
/// this a no-op for it, and the upgrade just records v1. No detection needed.
// The FTS tables here (issues_fts, docs_fts) index by implicit rowid. lcl
// never runs VACUUM, which may renumber those rowids: see V6.
const V1: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS labels (id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS issues (
    id TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'Backlog',
    priority INTEGER NOT NULL DEFAULT 0,
    assignee TEXT,
    parent_id TEXT,
    estimate INTEGER,
    due_date TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER
);
CREATE TABLE IF NOT EXISTS issue_labels (issue_id TEXT NOT NULL, label_id TEXT NOT NULL, PRIMARY KEY(issue_id,label_id));
CREATE TABLE IF NOT EXISTS comments (
    id TEXT PRIMARY KEY,
    issue_id TEXT NOT NULL,
    body TEXT NOT NULL,
    author TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS docs (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    parent_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS cycles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    starts_at TEXT,
    ends_at TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS issue_cycles (issue_id TEXT NOT NULL, cycle_id TEXT NOT NULL, PRIMARY KEY(issue_id,cycle_id));
CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(title, description, content='issues', content_rowid='rowid');
CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(title, content, content='docs', content_rowid='rowid');

CREATE TRIGGER IF NOT EXISTS issues_ai AFTER INSERT ON issues BEGIN
    INSERT INTO issues_fts(rowid,title,description) VALUES (new.rowid,new.title,new.description);
END;
CREATE TRIGGER IF NOT EXISTS issues_ad AFTER DELETE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts,rowid,title,description) VALUES ('delete',old.rowid,old.title,old.description);
END;
CREATE TRIGGER IF NOT EXISTS issues_au AFTER UPDATE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts,rowid,title,description) VALUES ('delete',old.rowid,old.title,old.description);
    INSERT INTO issues_fts(rowid,title,description) VALUES (new.rowid,new.title,new.description);
END;
CREATE TRIGGER IF NOT EXISTS docs_ai AFTER INSERT ON docs BEGIN
    INSERT INTO docs_fts(rowid,title,content) VALUES (new.rowid,new.title,new.content);
END;
CREATE TRIGGER IF NOT EXISTS docs_ad AFTER DELETE ON docs BEGIN
    INSERT INTO docs_fts(docs_fts,rowid,title,content) VALUES ('delete',old.rowid,old.title,old.content);
END;
CREATE TRIGGER IF NOT EXISTS docs_au AFTER UPDATE ON docs BEGIN
    INSERT INTO docs_fts(docs_fts,rowid,title,content) VALUES ('delete',old.rowid,old.title,old.content);
    INSERT INTO docs_fts(rowid,title,content) VALUES (new.rowid,new.title,new.content);
END;
"#;

/// v1 -> v2: seed the issue-number high-water mark.
///
/// `issue create` allocates from `meta.next_issue_number` and deletes never
/// lower it, so a number is never handed out twice. The seed is one past the
/// highest existing number. A number deleted under 0.1.0 before the upgrade
/// can't be recovered.
const V2: &str = r#"
INSERT OR IGNORE INTO meta(key,value) VALUES('next_issue_number', COALESCE((SELECT MAX(number) FROM issues),0)+1);
"#;

/// v2 -> v3: lifecycle timestamps.
///
/// Adds `started_at` and `canceled_at`, then backfills from `updated_at`:
/// `canceled_at` for Canceled issues, and `completed_at` for Done issues that
/// lack one (0.1.0's `create --status Done` left it null). `updated_at` is the
/// best time available, and only an approximation: any later edit moved it.
/// `started_at` is not backfilled, because when work started is unknown.
///
/// Not idempotent (ADD COLUMN fails the second time), so it relies on
/// `migrate` re-reading the version under the write lock.
const V3: &str = r#"
ALTER TABLE issues ADD COLUMN started_at INTEGER;
ALTER TABLE issues ADD COLUMN canceled_at INTEGER;
UPDATE issues SET canceled_at = updated_at WHERE status = 'Canceled';
UPDATE issues SET completed_at = updated_at WHERE status = 'Done' AND completed_at IS NULL;
"#;

/// v3 -> v4: repair `issues.parent_id`, which 0.1.0 stored as typed.
///
/// Every value becomes the UUID of the issue it names, or NULL:
/// - a UUID of an existing issue stays (lowercased, the stored form);
/// - `N`, `#N` or `KEY-N` (this workspace's key, in any ASCII case) naming an
///   existing issue becomes that issue's UUID;
/// - anything else becomes NULL: another key, a number with no issue, text,
///   or "". These were dangling, and nothing could follow them.
///
/// Then every link on a cycle (an issue that is its own ancestor, itself
/// included) becomes NULL. No link in a loop is more wrong than another, so
/// all of them go; issues hanging below the loop keep their parents.
///
/// Rows whose value is already right are not rewritten (so their FTS entries
/// are untouched). Idempotent.
const V4: &str = r#"
CREATE TEMP TABLE v4_parent AS
WITH k(key) AS (SELECT COALESCE((SELECT value FROM meta WHERE key = 'key'), '')),
c AS (
    SELECT i.id, i.parent_id AS raw,
        CASE
            WHEN i.parent_id GLOB '#*' THEN substr(i.parent_id, 2)
            WHEN k.key <> '' AND lower(substr(i.parent_id, 1, length(k.key) + 1)) = lower(k.key) || '-'
                THEN substr(i.parent_id, length(k.key) + 2)
            ELSE i.parent_id
        END AS n
    FROM issues i, k
    WHERE i.parent_id IS NOT NULL
)
SELECT c.id, c.raw,
    COALESCE(
        (SELECT p.id FROM issues p WHERE p.id = lower(c.raw)),
        CASE WHEN c.n <> '' AND c.n NOT GLOB '*[^0-9]*'
            THEN (SELECT p.id FROM issues p WHERE p.number = CAST(c.n AS INTEGER)) END
    ) AS parent
FROM c;

UPDATE issues SET parent_id = (SELECT v.parent FROM v4_parent v WHERE v.id = issues.id)
WHERE id IN (SELECT id FROM v4_parent WHERE parent IS NOT raw);

DROP TABLE v4_parent;

WITH RECURSIVE up(start, cur) AS (
    SELECT id, parent_id FROM issues WHERE parent_id IS NOT NULL
    UNION
    SELECT up.start, i.parent_id FROM up JOIN issues i ON i.id = up.cur
    WHERE i.parent_id IS NOT NULL AND up.cur <> up.start
)
UPDATE issues SET parent_id = NULL WHERE id IN (SELECT start FROM up WHERE cur = start);
"#;

/// v4 -> v5: labels become records, and assignees are trimmed (blank ones
/// become unset).
///
/// 0.1.0 stored whatever followed `--label` in issue_labels.label_id: usually
/// a name, sometimes a label id, sometimes "". And it allowed two labels with
/// the same name. This migration:
/// - trims label names, and merges labels whose trimmed names match ignoring
///   case into the one with the lowest rowid (its id, trimmed name and
///   colour survive);
/// - maps every issue_labels value, trimmed of whitespace, to a label: a
///   label id (any case) becomes that label or its survivor; else a name
///   (ignoring case) becomes that label; else a new label is made with that
///   name and colour #888888, one per name ignoring case, spelled as its first
///   use. Dropped: blank values, rows whose issue no longer exists, and
///   UUIDs that name no label (rows 0.1.0's `label delete` left behind; the
///   user deleted that label);
/// - rebuilds issue_labels with foreign keys to issues and labels, each
///   ON DELETE CASCADE, so deleting a label takes it off its issues;
/// - adds a unique index on labels(name COLLATE NOCASE);
/// - trims assignees, and sets them to NULL where blank (0.1.0's
///   `--assignee ""`).
///
/// "Whitespace" is Unicode White_Space, the set Rust's `str::trim` uses, so
/// the CLI's trimming and this one agree. "Ignoring case" is SQLite's
/// NOCASE: ASCII letters only. Not idempotent
/// (CREATE TABLE/INDEX), so it relies on `migrate` re-reading the version
/// under the write lock.
const V5: &str = r#"
CREATE TEMP TABLE v5_label AS
SELECT l.id AS id,
    (SELECT s.id FROM labels s WHERE trim(s.name, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)) = trim(l.name, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)) COLLATE NOCASE
     ORDER BY s.rowid LIMIT 1) AS survivor
FROM labels l;
-- From here on names are trimmed, so a name match's lowest rowid is its survivor.
UPDATE labels SET name = trim(name, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)) WHERE name <> trim(name, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288));

CREATE TEMP TABLE v5_raw AS
SELECT rid, issue_id, raw FROM (
    SELECT il.rowid AS rid, il.issue_id AS issue_id, trim(il.label_id, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)) AS raw
    FROM issue_labels il
    WHERE il.issue_id IN (SELECT id FROM issues)
)
WHERE raw <> ''
  AND NOT (lower(raw) GLOB replace('xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx', 'x', '[0-9a-f]')
           AND lower(raw) NOT IN (SELECT id FROM labels)
           AND NOT EXISTS (SELECT 1 FROM labels l WHERE l.name = raw COLLATE NOCASE));

INSERT INTO labels(id, name, color)
SELECT lower(printf('%s-%s-4%s-%s%s-%s',
        hex(randomblob(4)), hex(randomblob(2)), substr(hex(randomblob(2)), 2),
        substr('89ab', 1 + abs(random() % 4), 1), substr(hex(randomblob(2)), 2), hex(randomblob(6)))),
    r.raw, '#888888'
FROM v5_raw r
WHERE lower(r.raw) NOT IN (SELECT id FROM labels)
  AND NOT EXISTS (SELECT 1 FROM labels l WHERE l.name = r.raw COLLATE NOCASE)
  AND r.rid = (SELECT min(o.rid) FROM v5_raw o
               WHERE o.raw = r.raw COLLATE NOCASE AND lower(o.raw) NOT IN (SELECT id FROM labels));

CREATE TABLE issue_labels_v5 (
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);
-- No OR IGNORE: it would also swallow a NULL mapping. GROUP BY dedupes.
INSERT INTO issue_labels_v5(issue_id, label_id)
SELECT issue_id, label_id FROM (
    SELECT r.rid, r.issue_id, COALESCE(
        (SELECT v.survivor FROM v5_label v WHERE v.id = lower(r.raw)),
        (SELECT l.id FROM labels l WHERE l.name = r.raw COLLATE NOCASE ORDER BY l.rowid LIMIT 1)
    ) AS label_id
    FROM v5_raw r
) GROUP BY issue_id, label_id ORDER BY min(rid);
DROP TABLE issue_labels;
ALTER TABLE issue_labels_v5 RENAME TO issue_labels;

DELETE FROM labels WHERE id IN (SELECT id FROM v5_label WHERE survivor <> id);
CREATE UNIQUE INDEX labels_name ON labels(name COLLATE NOCASE);

UPDATE issues SET assignee = NULLIF(trim(assignee, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)), '')
WHERE assignee <> trim(assignee, char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)) OR assignee = '';

DROP TABLE v5_label;
DROP TABLE v5_raw;
"#;

/// v5 -> v6: comments become searchable.
///
/// comments_fts indexes comment bodies, kept in step by triggers the same
/// way as issues_fts and docs_fts (V1). 'rebuild' then indexes the comments
/// that already exist.
///
/// All three FTS tables use external content keyed by the content table's
/// implicit rowid. SQLite documents that VACUUM may renumber implicit
/// rowids (3.45 was tested and didn't), which would point the indexes at
/// the wrong rows. So lcl never runs VACUUM; anything that ever does must
/// run 'rebuild' on issues_fts, docs_fts and comments_fts afterwards.
const V6: &str = r#"
-- External content keyed by comments' implicit rowid: never VACUUM without
-- rebuilding every fts table afterwards (see the doc comment on V6).
CREATE VIRTUAL TABLE comments_fts USING fts5(body, content='comments', content_rowid='rowid');
CREATE TRIGGER comments_ai AFTER INSERT ON comments BEGIN
    INSERT INTO comments_fts(rowid, body) VALUES (new.rowid, new.body);
END;
CREATE TRIGGER comments_ad AFTER DELETE ON comments BEGIN
    INSERT INTO comments_fts(comments_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
END;
CREATE TRIGGER comments_au AFTER UPDATE ON comments BEGIN
    INSERT INTO comments_fts(comments_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
    INSERT INTO comments_fts(rowid, body) VALUES (new.rowid, new.body);
END;
INSERT INTO comments_fts(comments_fts) VALUES ('rebuild');
"#;

/// v6 -> v7: relations between issues.
///
/// One row per relation, in one stored form:
/// - `blocks`: issue_id blocks related_id (`A blocked-by B` is stored as
///   `B blocks A`);
/// - `duplicate`: issue_id is a duplicate of related_id;
/// - `related`: symmetric, so the smaller id is stored first.
///
/// Deleting an issue removes its relations in both directions. The first
/// index serves the reverse direction: what blocks an issue, and its
/// duplicates. The second holds an issue to at most one duplicate-of target
/// for every writer, `lcl issue relate` (which checks first and names the
/// current target) and rows inserted directly alike.
const V7: &str = r#"
CREATE TABLE issue_relations (
    issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    related_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('blocks', 'related', 'duplicate')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (issue_id, related_id, kind),
    CHECK (issue_id <> related_id)
);
CREATE INDEX issue_relations_related ON issue_relations(related_id, kind);
CREATE UNIQUE INDEX issue_relations_one_duplicate ON issue_relations(issue_id) WHERE kind = 'duplicate';
"#;

/// v7 -> v8: Linear projects and milestones inside a workspace, and an
/// issue's place in them. Existing issues are in no project.
///
/// - A project's name is unique ignoring case; so is a milestone's within
///   its project.
/// - The foreign keys have no ON DELETE action: a project with milestones
///   or issues, or a milestone with issues, can't be deleted out from under
///   them (lcl refuses first, with counts).
/// - The triggers hold every writer to the rules lcl checks itself (an
///   import that inserts rows directly included): an issue's milestone is
///   in the issue's project (so no milestone without a project), and a
///   milestone never moves to another project.
const V8: &str = r#"
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'Backlog' CHECK (status IN ('Backlog', 'Planned', 'Started', 'Paused', 'Completed', 'Canceled')),
    target_date TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX projects_name ON projects(name COLLATE NOCASE);
CREATE TABLE milestones (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    target_date TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (project_id, name COLLATE NOCASE)
);
ALTER TABLE issues ADD COLUMN project_id TEXT REFERENCES projects(id);
ALTER TABLE issues ADD COLUMN milestone_id TEXT REFERENCES milestones(id);
CREATE INDEX issues_project ON issues(project_id);
CREATE INDEX issues_milestone ON issues(milestone_id);
CREATE TRIGGER issues_milestone_in_project_insert BEFORE INSERT ON issues
WHEN NEW.milestone_id IS NOT NULL AND (SELECT project_id FROM milestones WHERE id = NEW.milestone_id) IS NOT NEW.project_id
BEGIN
    SELECT RAISE(ABORT, 'milestone is not in the issue''s project');
END;
CREATE TRIGGER issues_milestone_in_project_update BEFORE UPDATE OF project_id, milestone_id ON issues
WHEN NEW.milestone_id IS NOT NULL AND (SELECT project_id FROM milestones WHERE id = NEW.milestone_id) IS NOT NEW.project_id
BEGIN
    SELECT RAISE(ABORT, 'milestone is not in the issue''s project');
END;
CREATE TRIGGER milestones_stay_in_project BEFORE UPDATE OF project_id ON milestones
WHEN NEW.project_id IS NOT OLD.project_id
BEGIN
    SELECT RAISE(ABORT, 'a milestone cannot move to another project');
END;
"#;

/// v8 -> v9: an issue's ref, the id it had elsewhere (its Linear id, as
/// BZR-1532, which commits and specs cite), and its creator. A ref is on at
/// most one issue, ignoring case, for every writer. Existing issues have
/// neither.
const V9: &str = r#"
ALTER TABLE issues ADD COLUMN ref TEXT;
CREATE UNIQUE INDEX issues_ref ON issues(ref COLLATE NOCASE) WHERE ref IS NOT NULL;
ALTER TABLE issues ADD COLUMN creator TEXT;
"#;

/// v9 -> v10: the values lcl 0.1.0 accepted but lcl can't read, made
/// readable, so a workspace from 0.1.0 can be exported. Text lcl can't read
/// is kept where it can be seen, or dropped only when it said nothing.
/// Values that are already valid are left alone, and no `updated_at` moves.
///
/// - A blank label name becomes "unnamed", or the first of "unnamed-2",
///   "unnamed-3", … that no other label has, ignoring case. (V5 trimmed the
///   names, so from 0.1.0 at most one is blank.)
/// - A label colour that isn't `#rgb` or `#rrggbb` becomes #888888.
/// - An issue's due date that isn't a real date written as YYYY-MM-DD (so
///   2026-02-30 too) is cleared, and its text becomes the last line of the
///   description, "Due (from 0.1.0): <text>", after a blank line when the
///   description isn't empty.
/// - A cycle's start or end date that isn't one is cleared, and its text is
///   added to the cycle's name: "Sprint 1 (start: soon) (end: 2026-13-01)".
/// - A blank date (empty, or only whitespace) said nothing, so it is just
///   cleared: no line, no addition to the name.
/// - A negative estimate is cleared.
/// - A doc's parent follows V4's rule for issues: a value naming an existing
///   doc's id, in any ASCII case, becomes that id as lcl stores it
///   (lowercase); anything else is cleared. Then every link on a loop (a doc
///   that is its own ancestor, itself included) is cleared too. 0.1.0 can't
///   make a loop (its `doc update` has no `--parent`), but a direct writer
///   can.
/// - A comment's author is trimmed, and cleared when blank.
///
/// "Whitespace" is Unicode White_Space, as Rust's `str::trim` reads it.
/// Changes no schema. Idempotent: a second run finds nothing to change.
fn v10(tx: &Transaction<'_>) -> Result<Vec<String>> {
    let mut n = V10::default();

    for (rowid, name, color) in rows(tx, "SELECT rowid, name, color FROM labels ORDER BY rowid", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        if name.trim().is_empty() {
            let mut i = 1;
            let name = loop {
                let candidate = if i == 1 { "unnamed".to_string() } else { format!("unnamed-{i}") };
                let taken: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM labels WHERE name = ?1 COLLATE NOCASE)", [&candidate], |r| r.get(0))?;
                if !taken {
                    break candidate;
                }
                i += 1;
            };
            tx.execute("UPDATE labels SET name = ?1 WHERE rowid = ?2", params![name, rowid])?;
            n.blank_labels += 1;
        }
        if parse::color(&color).is_err() {
            tx.execute("UPDATE labels SET color = '#888888' WHERE rowid = ?1", [rowid])?;
            n.colours += 1;
        }
    }

    for (rowid, description, due) in rows(tx, "SELECT rowid, description, due_date FROM issues WHERE due_date IS NOT NULL ORDER BY rowid", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        match date_text(&due) {
            DateText::Readable => {}
            DateText::Blank => {
                tx.execute("UPDATE issues SET due_date = NULL WHERE rowid = ?1", [rowid])?;
                n.due_blank += 1;
            }
            DateText::Unreadable => {
                let line = format!("Due (from 0.1.0): {due}");
                let description = if description.is_empty() { line } else { format!("{description}\n\n{line}") };
                tx.execute("UPDATE issues SET due_date = NULL, description = ?1 WHERE rowid = ?2", params![description, rowid])?;
                n.due_moved += 1;
            }
        }
    }
    n.estimates = tx.execute("UPDATE issues SET estimate = NULL WHERE estimate < 0", [])?;

    for (rowid, name, starts, ends) in rows(tx, "SELECT rowid, name, starts_at, ends_at FROM cycles ORDER BY rowid", |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
    })? {
        let mut suffix = String::new();
        let new_starts = cycle_date(&starts, "start", &mut suffix, &mut n.cycle_blank);
        let new_ends = cycle_date(&ends, "end", &mut suffix, &mut n.cycle_blank);
        if (&new_starts, &new_ends) != (&starts, &ends) {
            tx.execute("UPDATE cycles SET name = ?1, starts_at = ?2, ends_at = ?3 WHERE rowid = ?4", params![format!("{name}{suffix}"), new_starts, new_ends, rowid])?;
            n.cycles_renamed += usize::from(!suffix.is_empty());
        }
    }

    let docs = rows(tx, "SELECT id, parent_id FROM docs ORDER BY rowid", |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    let ids: HashSet<&str> = docs.iter().map(|(id, _)| id.as_str()).collect();
    // Each doc's parent under the naming rule, before loops are broken.
    let parent: HashMap<&str, String> = docs
        .iter()
        .filter_map(|(id, raw)| raw.as_ref().map(|raw| (id.as_str(), raw.to_ascii_lowercase())))
        .filter(|(_, p)| ids.contains(p.as_str()))
        .collect();
    let on_loop = |start: &str| {
        let mut seen = HashSet::new();
        let mut cur = parent.get(start);
        while let Some(p) = cur {
            if p == start {
                return true;
            }
            if !seen.insert(p) {
                return false; // a loop above this doc, not through it
            }
            cur = parent.get(p.as_str());
        }
        false
    };
    for (id, raw) in &docs {
        let Some(raw) = raw else { continue };
        let kept = parent.get(id.as_str()).filter(|_| !on_loop(id));
        match kept {
            Some(p) if p == raw => {}
            Some(p) => {
                tx.execute("UPDATE docs SET parent_id = ?1 WHERE id = ?2", params![p, id])?;
                n.doc_parents_lowercased += 1;
            }
            None => {
                tx.execute("UPDATE docs SET parent_id = NULL WHERE id = ?1", [id])?;
                n.doc_parents_cleared += 1;
            }
        }
    }

    for (rowid, author) in rows(tx, "SELECT rowid, author FROM comments WHERE author IS NOT NULL ORDER BY rowid", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let trimmed = Some(author.trim()).filter(|a| !a.is_empty());
        if trimmed != Some(author.as_str()) {
            tx.execute("UPDATE comments SET author = ?1 WHERE rowid = ?2", params![trimmed, rowid])?;
            n.authors += 1;
        }
    }

    Ok(n.phrases())
}

/// How many values of each kind v10 changed.
#[derive(Default)]
struct V10 {
    blank_labels: usize,
    colours: usize,
    due_moved: usize,
    due_blank: usize,
    cycles_renamed: usize,
    cycle_blank: usize,
    estimates: usize,
    doc_parents_cleared: usize,
    doc_parents_lowercased: usize,
    authors: usize,
}

impl V10 {
    /// "named 1 blank label", …: one phrase per kind with a count, in this
    /// order.
    fn phrases(&self) -> Vec<String> {
        let phrase = |count: usize, verb: &str, one: &str, many: &str| match count {
            0 => None,
            1 => Some(format!("{verb} 1 {one}")),
            _ => Some(format!("{verb} {count} {many}")),
        };
        [
            phrase(self.blank_labels, "named", "blank label", "blank labels"),
            phrase(self.colours, "reset", "label colour", "label colours"),
            phrase(self.due_moved, "moved", "unreadable due date into a description", "unreadable due dates into descriptions"),
            phrase(self.due_blank, "cleared", "blank due date", "blank due dates"),
            phrase(self.cycles_renamed, "renamed", "cycle with unreadable dates", "cycles with unreadable dates"),
            phrase(self.cycle_blank, "cleared", "blank cycle date", "blank cycle dates"),
            phrase(self.estimates, "cleared", "negative estimate", "negative estimates"),
            phrase(self.doc_parents_cleared, "cleared", "doc parent", "doc parents"),
            phrase(self.doc_parents_lowercased, "lowercased", "doc parent", "doc parents"),
            phrase(self.authors, "cleaned", "comment author", "comment authors"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

enum DateText {
    Readable,
    Blank,
    Unreadable,
}

fn date_text(s: &str) -> DateText {
    if parse::date(s).is_ok() {
        DateText::Readable
    } else if s.trim().is_empty() {
        DateText::Blank
    } else {
        DateText::Unreadable
    }
}

/// A cycle date as v10 leaves it: kept when unset or readable, else
/// cleared. Unreadable text is added to `suffix` as " (start: <text>)";
/// blank text is counted in `blank`.
fn cycle_date(date: &Option<String>, which: &str, suffix: &mut String, blank: &mut usize) -> Option<String> {
    let text = date.as_deref()?;
    match date_text(text) {
        DateText::Readable => return date.clone(),
        DateText::Blank => *blank += 1,
        DateText::Unreadable => suffix.push_str(&format!(" ({which}: {text})")),
    }
    None
}

fn rows<T>(tx: &Transaction<'_>, sql: &str, f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>) -> Result<Vec<T>> {
    let mut stmt = tx.prepare(sql)?;
    let all = stmt.query_map([], f)?.collect::<rusqlite::Result<Vec<T>>>()?;
    Ok(all)
}
