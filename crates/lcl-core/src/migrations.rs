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

/// The schema version this build reads and writes. Derived from the list, so
/// adding a migration bumps it and the two can't drift apart.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

pub(crate) const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5];

/// v0 -> v1: the lcl 0.1.0 schema, verbatim.
///
/// 0.1.0 ran this DDL on every open and never set `user_version`, so a 0.1.0
/// database is at v0 and already has every object here. `IF NOT EXISTS` makes
/// this a no-op for it, and the upgrade just records v1. No detection needed.
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
