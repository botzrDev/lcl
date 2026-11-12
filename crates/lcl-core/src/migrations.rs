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

pub(crate) const MIGRATIONS: &[&str] = &[V1, V2, V3];

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
