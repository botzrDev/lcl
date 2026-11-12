use crate::error::{LclError, Result};
use crate::migrations::{MIGRATIONS, SCHEMA_VERSION};
use crate::models::*;
use rusqlite::{params, Connection, OptionalExtension, ToSql, Transaction, TransactionBehavior};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// How long a connection waits for a lock held by another lcl process before
/// failing with SQLITE_BUSY. Concurrent first opens queue on the migration lock.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// The `meta` key holding the next issue number to hand out (seeded by
/// migration 2; the name must match it). It only ever grows: deleting an issue
/// never lowers it, so no number is handed out twice.
const NEXT_ISSUE_NUMBER: &str = "next_issue_number";

pub struct Store {
    pub conn: Connection,
}

/// Which issues `Store::list_issues` returns.
#[derive(Debug, Clone)]
pub struct IssueFilter {
    pub status: Option<Status>,
    pub assignee: Option<String>,
    /// Only the direct sub-issues of the issue with this UUID.
    pub parent_id: Option<String>,
    /// At most this many, newest first. Negative means no limit.
    pub limit: i64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Connection setup shared by every open. The order matters: the busy
    /// timeout must be in place before anything can contend for a lock, and
    /// the version check must come before anything that can write, so a
    /// database from a newer lcl is refused untouched.
    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // Read-only. Must precede journal_mode: switching a rollback-journal
        // file to WAL rewrites its header.
        check_supported(user_version(&conn)?)?;
        // journal_mode cannot change inside a transaction. It answers with the
        // resulting mode ("memory" for an in-memory db), which we don't need.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut conn)?;
        Ok(Store { conn })
    }

    /// The schema version recorded in the file (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64> {
        user_version(&self.conn)
    }

    /// Runs `f` in one `BEGIN IMMEDIATE` transaction and commits if it returns
    /// Ok. Every write goes through here, together with every read the write
    /// depends on: IMMEDIATE takes the write lock before `f` runs, so nothing
    /// can change between its reads and its writes. Concurrent writers queue on
    /// the lock for up to BUSY_TIMEOUT instead of failing. An error drops the
    /// transaction, which rolls all of it back.
    fn write<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ----- meta -----
    /// Sets every `(key, value)` pair in one transaction.
    pub fn set_meta(&mut self, entries: &[(&str, &str)]) -> Result<()> {
        self.write(|tx| {
            for (key, value) in entries { put_meta(tx, key, value)?; }
            Ok(())
        })
    }
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        read_meta(&self.conn, key)
    }

    // ----- labels -----
    pub fn create_label(&mut self, l: &Label) -> Result<()> {
        self.write(|tx| {
            tx.execute("INSERT INTO labels(id,name,color) VALUES(?1,?2,?3)", params![l.id, l.name, l.color])?;
            Ok(())
        })
    }
    pub fn list_labels(&self) -> Result<Vec<Label>> {
        let mut stmt = self.conn.prepare("SELECT id,name,color FROM labels ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn delete_label(&mut self, id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM labels WHERE id=?1", params![id])?;
            Ok(())
        })
    }

    // ----- issues -----
    /// Inserts `issue` and returns it as stored. A zero `number` is allocated
    /// from the high-water mark; either way the mark ends up past the number
    /// used. A `parent_id` must be the UUID of an existing issue.
    pub fn create_issue(&mut self, mut issue: Issue) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            if let Some(parent) = &issue.parent_id { check_parent(tx, &key, None, parent)?; }
            if issue.id.is_empty() { issue.id = uuid::Uuid::new_v4().to_string(); }
            let next = next_issue_number(tx)?;
            if issue.number == 0 { issue.number = next; }
            let now = crate::models::now();
            if issue.created_at == 0 { issue.created_at = now; }
            if issue.updated_at == 0 { issue.updated_at = now; }
            issue.apply_lifecycle(None, issue.created_at);
            let status = issue.status.as_str();
            let priority = issue.priority.as_i64();
            tx.execute(
                "INSERT INTO issues(id,number,title,description,status,priority,assignee,parent_id,estimate,due_date,created_at,updated_at,completed_at,started_at,canceled_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![issue.id, issue.number, issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.created_at, issue.updated_at, issue.completed_at, issue.started_at, issue.canceled_at],
            )?;
            for lid in &issue.labels {
                tx.execute("INSERT OR IGNORE INTO issue_labels(issue_id,label_id) VALUES(?1,?2)", params![issue.id, lid])?;
            }
            put_meta(tx, NEXT_ISSUE_NUMBER, &next.max(issue.number + 1).to_string())?;
            reread(tx, &key, &issue.id)
        })
    }
    /// The issue `r` names. This is the one resolver: every command that
    /// takes an issue goes through it (writes through `resolve_issue`, inside
    /// their transaction).
    pub fn resolve(&self, r: &IssueRef) -> Result<Issue> {
        resolve_issue(&self.conn, r)
    }
    /// The issues directly under the issue with UUID `parent_id`, by number.
    pub fn children(&self, parent_id: &str) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare("SELECT id FROM issues WHERE parent_id=?1 ORDER BY number")?;
        let ids = stmt.query_map(params![parent_id], |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        fetch_issues(&self.conn, &ids)
    }
    pub fn list_issues(&self, filter: &IssueFilter) -> Result<Vec<Issue>> {
        let mut conds: Vec<&str> = vec![];
        let mut args: Vec<String> = vec![];
        if let Some(st) = filter.status { conds.push("status=?"); args.push(st.as_str().to_string()); }
        if let Some(p) = &filter.parent_id { conds.push("parent_id=?"); args.push(p.clone()); }
        let mut sql = String::from("SELECT id FROM issues");
        if !conds.is_empty() { sql.push_str(" WHERE "); sql.push_str(&conds.join(" AND ")); }
        sql.push_str(&format!(" ORDER BY number DESC LIMIT {}", filter.limit));
        let mut stmt = self.conn.prepare(&sql)?;
        let ids = stmt.query_map(rusqlite::params_from_iter(&args), |r| r.get(0))?.collect::<std::result::Result<Vec<String>, _>>()?;
        let mut out = fetch_issues(&self.conn, &ids)?;
        if let Some(a) = &filter.assignee { out.retain(|i| i.assignee.as_ref() == Some(a)); }
        Ok(out)
    }
    /// Reads the issue, applies `f` and writes it back, all under the write
    /// lock, so a concurrent update can't be overwritten with a stale copy.
    /// A changed `parent_id` must be the UUID of an existing issue that is
    /// neither this one nor below it.
    pub fn update_issue(&mut self, r: &IssueRef, f: impl FnOnce(&mut Issue)) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let mut issue = resolve_issue(tx, r)?;
            let (from, old_parent) = (issue.status, issue.parent_id.clone());
            f(&mut issue);
            if issue.parent_id != old_parent && let Some(parent) = &issue.parent_id {
                check_parent(tx, &key, Some(&issue), parent)?;
            }
            issue.updated_at = crate::models::now();
            issue.apply_lifecycle(Some(from), issue.updated_at);
            let status = issue.status.as_str();
            let priority = issue.priority.as_i64();
            tx.execute(
                "UPDATE issues SET title=?1,description=?2,status=?3,priority=?4,assignee=?5,parent_id=?6,estimate=?7,due_date=?8,updated_at=?9,completed_at=?10,started_at=?11,canceled_at=?12 WHERE id=?13",
                params![issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.updated_at, issue.completed_at, issue.started_at, issue.canceled_at, issue.id],
            )?;
            // sync labels: replace
            tx.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue.id])?;
            for lid in &issue.labels {
                tx.execute("INSERT OR IGNORE INTO issue_labels(issue_id,label_id) VALUES(?1,?2)", params![issue.id, lid])?;
            }
            reread(tx, &key, &issue.id)
        })
    }
    /// Deletes the issue and its comments, labels and cycle links, and returns
    /// it. Refused while it has sub-issues, which would be left pointing at
    /// nothing. The number-allocation mark is deliberately left alone, so the
    /// number is never reused.
    pub fn delete_issue(&mut self, r: &IssueRef) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let issue = resolve_issue(tx, r)?;
            let mut stmt = tx.prepare("SELECT number FROM issues WHERE parent_id=?1 ORDER BY number")?;
            let children = stmt.query_map(params![issue.id], |r| r.get(0))?.collect::<std::result::Result<Vec<i64>, _>>()?;
            if !children.is_empty() {
                let children: Vec<String> = children.into_iter().map(|n| identifier(&key, n)).collect();
                return Err(LclError::HasChildren { issue: issue.identifier, children: children.join(", ") });
            }
            tx.execute("DELETE FROM comments WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issue_cycles WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issues WHERE id=?1", params![issue.id])?;
            Ok(issue)
        })
    }

    // ----- comments -----
    /// Resolves the issue and adds the comment in one transaction, so the
    /// issue can't be deleted in between.
    pub fn add_comment(&mut self, r: &IssueRef, body: String, author: Option<String>) -> Result<Comment> {
        self.write(|tx| {
            let target = resolve_issue(tx, r)?;
            let c = Comment { id: uuid::Uuid::new_v4().to_string(), issue_id: target.id, body, author, created_at: crate::models::now() };
            tx.execute("INSERT INTO comments(id,issue_id,body,author,created_at) VALUES(?1,?2,?3,?4,?5)", params![c.id, c.issue_id, c.body, c.author, c.created_at])?;
            Ok(c)
        })
    }
    pub fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let mut stmt = self.conn.prepare("SELECT id,issue_id,body,author,created_at FROM comments WHERE issue_id=?1 ORDER BY created_at")?;
        let rows = stmt.query_map(params![issue_id], |r| Ok(Comment { id: r.get(0)?, issue_id: r.get(1)?, body: r.get(2)?, author: r.get(3)?, created_at: r.get(4)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }

    // ----- docs -----
    pub fn create_doc(&mut self, mut d: Doc) -> Result<Doc> {
        if d.id.is_empty() { d.id = uuid::Uuid::new_v4().to_string(); }
        let now = crate::models::now();
        if d.created_at == 0 { d.created_at = now; }
        if d.updated_at == 0 { d.updated_at = now; }
        self.write(|tx| {
            tx.execute("INSERT INTO docs(id,title,content,parent_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6)", params![d.id, d.title, d.content, d.parent_id, d.created_at, d.updated_at])?;
            Ok(d)
        })
    }
    pub fn get_doc(&self, id: &str) -> Result<Option<Doc>> {
        read_doc(&self.conn, id)
    }
    pub fn list_docs(&self) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare("SELECT id,title,content,parent_id,created_at,updated_at FROM docs ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(Doc { id: r.get(0)?, title: r.get(1)?, content: r.get(2)?, parent_id: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }
    pub fn update_doc(&mut self, id: &str, title: Option<String>, content: Option<String>) -> Result<Doc> {
        self.write(|tx| {
            let mut d = read_doc(tx, id)?.ok_or_else(|| LclError::NotFound(id.to_string()))?;
            if let Some(t) = title { d.title = t; }
            if let Some(c) = content { d.content = c; }
            d.updated_at = crate::models::now();
            tx.execute("UPDATE docs SET title=?1,content=?2,updated_at=?3 WHERE id=?4", params![d.title, d.content, d.updated_at, d.id])?;
            Ok(d)
        })
    }
    pub fn delete_doc(&mut self, id: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM docs WHERE id=?1", params![id])?;
            Ok(())
        })
    }

    // ----- cycles -----
    pub fn create_cycle(&mut self, mut c: Cycle) -> Result<Cycle> {
        if c.id.is_empty() { c.id = uuid::Uuid::new_v4().to_string(); }
        if c.created_at == 0 { c.created_at = crate::models::now(); }
        self.write(|tx| {
            tx.execute("INSERT INTO cycles(id,name,starts_at,ends_at,created_at) VALUES(?1,?2,?3,?4,?5)", params![c.id, c.name, c.starts_at, c.ends_at, c.created_at])?;
            Ok(c)
        })
    }
    pub fn list_cycles(&self) -> Result<Vec<Cycle>> {
        let mut stmt = self.conn.prepare("SELECT id,name,starts_at,ends_at,created_at FROM cycles ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(Cycle { id: r.get(0)?, name: r.get(1)?, starts_at: r.get(2)?, ends_at: r.get(3)?, created_at: r.get(4)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }

    // ----- search -----
    pub fn search_issues(&self, q: &str) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare("SELECT i.id FROM issues i JOIN issues_fts f ON f.rowid=i.rowid WHERE issues_fts MATCH ?1")?;
        let ids: Vec<String> = stmt.query_map(params![q], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        fetch_issues(&self.conn, &ids)
    }
    pub fn search_docs(&self, q: &str) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare("SELECT d.id FROM docs d JOIN docs_fts f ON f.rowid=d.rowid WHERE docs_fts MATCH ?1")?;
        let ids: Vec<String> = stmt.query_map(params![q], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut out = vec![];
        for id in ids { if let Some(d) = self.get_doc(&id)? { out.push(d); } }
        Ok(out)
    }
}

// ----- reads shared by Store's read methods and its write transactions -----
// They take &Connection; a &Transaction derefs to one, so a write can make its
// reads inside its own transaction.

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key=?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(r) = rows.next()? { Ok(Some(r.get(0)?)) } else { Ok(None) }
}

fn put_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
    Ok(())
}

/// The number the next created issue gets: the high-water mark, or one past
/// the highest existing number if that is larger. The second term covers a
/// 0.1.0 binary, which knows nothing of the mark, writing to an upgraded file.
/// Must run inside the create's write transaction.
fn next_issue_number(conn: &Connection) -> Result<i64> {
    let mark = match read_meta(conn, NEXT_ISSUE_NUMBER)? {
        Some(v) => v.parse::<i64>().map_err(|_| LclError::Msg(format!("meta {NEXT_ISSUE_NUMBER} is not a number: {v:?}")))?,
        None => 1,
    };
    let past_max: i64 = conn.query_row("SELECT COALESCE(MAX(number),0)+1 FROM issues", [], |r| r.get(0))?;
    Ok(mark.max(past_max))
}

/// This workspace's key, as used in `KEY-N` ("" if it has none).
fn read_key(conn: &Connection) -> Result<String> {
    Ok(read_meta(conn, "key")?.unwrap_or_default())
}

/// The issue `r` names, or why there is none. `KEY-N` must carry this
/// workspace's key, compared ignoring case. A miss names the issue as
/// `KEY-N` whichever number form was given.
fn resolve_issue(conn: &Connection, r: &IssueRef) -> Result<Issue> {
    let key = read_key(conn)?;
    match r {
        IssueRef::Number { key: typed, number } => {
            if let Some(typed) = typed && typed.to_lowercase() != key.to_lowercase() {
                return Err(LclError::WrongWorkspace { given: r.to_string(), key });
            }
            fetch_issue_by(conn, &key, "number", number)?.ok_or_else(|| LclError::IssueNotFound(identifier(&key, *number)))
        }
        IssueRef::Id(id) => fetch_issue_by(conn, &key, "id", id)?.ok_or_else(|| LclError::IssueNotFound(id.clone())),
    }
}

/// Checks a parent link before it is written. The parent must exist, and for
/// an existing issue (`child`) it must be neither the issue itself nor below
/// it: walking up from the new parent must never reach the issue.
fn check_parent(conn: &Connection, key: &str, child: Option<&Issue>, parent_id: &str) -> Result<()> {
    let up = |id: &str| -> Result<Option<(i64, Option<String>)>> {
        Ok(conn.query_row("SELECT number, parent_id FROM issues WHERE id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    };
    let (parent_number, mut next) = up(parent_id)?.ok_or_else(|| LclError::IssueNotFound(parent_id.to_string()))?;
    let Some(child) = child else { return Ok(()) };
    if parent_id == child.id {
        return Err(LclError::SelfParent(identifier(key, child.number)));
    }
    let mut path = vec![child.number, parent_number];
    let mut seen = HashSet::from([parent_id.to_string()]);
    while let Some(id) = next {
        if id == child.id {
            path.push(child.number);
            let path: Vec<String> = path.into_iter().map(|n| identifier(key, n)).collect();
            return Err(LclError::ParentCycle { issue: identifier(key, child.number), parent: identifier(key, parent_number), path: path.join(" -> ") });
        }
        // A dangling link, or a loop above that doesn't pass through this
        // issue (only a 0.1.0 binary could write one now), ends the walk.
        if !seen.insert(id.clone()) { break; }
        let Some((number, above)) = up(&id)? else { break };
        path.push(number);
        next = above;
    }
    Ok(())
}

/// The issue as just written, read back inside the same transaction so the
/// caller gets the derived fields (identifiers) too.
fn reread(conn: &Connection, key: &str, id: &str) -> Result<Issue> {
    fetch_issue_by(conn, key, "id", &id)?.ok_or_else(|| LclError::Msg(format!("issue {id} vanished inside its own transaction")))
}

/// The issues with these UUIDs, in this order, skipping any that are gone.
fn fetch_issues(conn: &Connection, ids: &[String]) -> Result<Vec<Issue>> {
    let key = read_key(conn)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(issue) = fetch_issue_by(conn, &key, "id", id)? { out.push(issue); }
    }
    Ok(out)
}

/// The issue whose `col` (`id` or `number`) equals `val`, with its labels and
/// its identifiers under workspace key `key`.
fn fetch_issue_by(conn: &Connection, key: &str, col: &str, val: &dyn ToSql) -> Result<Option<Issue>> {
    let sql = format!("SELECT i.id,i.number,i.title,i.description,i.status,i.priority,i.assignee,i.parent_id,i.estimate,i.due_date,i.created_at,i.updated_at,i.completed_at,i.started_at,i.canceled_at,p.number FROM issues i LEFT JOIN issues p ON p.id = i.parent_id WHERE i.{col}=?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![val])?;
    if let Some(r) = rows.next()? {
        let issue = row_to_issue(r, key)?;
        // labels
        let mut li = conn.prepare("SELECT label_id FROM issue_labels WHERE issue_id=?1")?;
        let ls: Vec<String> = li.query_map(params![issue.id], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut issue = issue;
        issue.labels = ls;
        Ok(Some(issue))
    } else { Ok(None) }
}

fn row_to_issue(r: &rusqlite::Row, key: &str) -> Result<Issue> {
    let number: i64 = r.get(1)?;
    let status_str: String = r.get(4)?;
    // Read leniently ("done" is Done), but never guess: a status parse rejects
    // (a hand edit, or a newer lcl) must not be read as something else and
    // then written back.
    let status = Status::parse(&status_str).ok_or(LclError::UnknownStatus { number, value: status_str })?;
    let prio: i64 = r.get(5)?;
    let priority = Priority::from_stored(prio).ok_or(LclError::UnknownPriority { number, value: prio })?;
    let parent_number: Option<i64> = r.get(15)?;
    Ok(Issue {
        id: r.get(0)?,
        number,
        identifier: identifier(key, number),
        title: r.get(2)?,
        description: r.get(3)?,
        status,
        priority,
        assignee: r.get(6)?,
        parent_id: r.get(7)?,
        parent_identifier: parent_number.map(|n| identifier(key, n)),
        estimate: r.get(8)?,
        due_date: r.get(9)?,
        labels: vec![],
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        completed_at: r.get(12)?,
        started_at: r.get(13)?,
        canceled_at: r.get(14)?,
    })
}

fn read_doc(conn: &Connection, id: &str) -> Result<Option<Doc>> {
    let mut stmt = conn.prepare("SELECT id,title,content,parent_id,created_at,updated_at FROM docs WHERE id=?1")?;
    let mut rows = stmt.query(params![id])?;
    if let Some(r) = rows.next()? {
        Ok(Some(Doc { id: r.get(0)?, title: r.get(1)?, content: r.get(2)?, parent_id: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)? }))
    } else { Ok(None) }
}

fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Fails unless this build understands schema `found`. Nothing may write to a
/// database from a newer lcl: we don't know what its invariants are.
fn check_supported(found: i64) -> Result<usize> {
    if found > SCHEMA_VERSION {
        return Err(LclError::SchemaTooNew { found, supported: SCHEMA_VERSION });
    }
    usize::try_from(found).map_err(|_| LclError::Msg(format!("database schema version {found} is invalid")))
}

/// Brings the database up to `SCHEMA_VERSION`, all or nothing.
fn migrate(conn: &mut Connection) -> Result<()> {
    // Unlocked fast path: an up-to-date database takes no write lock.
    if check_supported(user_version(conn)?)? == MIGRATIONS.len() {
        return Ok(());
    }
    // Take the write lock, then read again: another process may have migrated
    // between our read and acquiring the lock.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let from = check_supported(user_version(&tx)?)?;
    let pending = &MIGRATIONS[from..];
    if pending.is_empty() {
        return Ok(()); // someone else migrated while we waited
    }
    for sql in pending {
        tx.execute_batch(sql)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    // An early return above drops `tx`, which rolls back every migration.
    tx.commit()?;
    Ok(())
}
