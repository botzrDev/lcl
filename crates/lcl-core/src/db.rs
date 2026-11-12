use crate::error::{LclError, Result};
use crate::migrations::{MIGRATIONS, SCHEMA_VERSION};
use crate::models::*;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
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
    /// Inserts `issue`. A zero `number` is allocated from the high-water mark;
    /// either way the mark ends up past the number used.
    pub fn create_issue(&mut self, mut issue: Issue) -> Result<Issue> {
        self.write(|tx| {
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
            Ok(issue)
        })
    }
    pub fn get_issue(&self, id_or_number: &str) -> Result<Option<Issue>> {
        read_issue(&self.conn, id_or_number)
    }
    pub fn list_issues(&self, status: Option<Status>, assignee: Option<&str>, limit: i64) -> Result<Vec<Issue>> {
        let mut sql = String::from("SELECT id FROM issues");
        if status.is_some() { sql.push_str(" WHERE status=?1"); }
        sql.push_str(" ORDER BY number DESC LIMIT ");
        sql.push_str(&limit.to_string());
        let mut stmt = self.conn.prepare(&sql)?;
        let ids: Vec<String> = if let Some(st) = status {
            stmt.query_map(params![st.as_str()], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?
        } else {
            stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(iss) = self.get_issue(&id)? {
                if let Some(a) = assignee && iss.assignee.as_deref() != Some(a) { continue; }
                out.push(iss);
            }
        }
        Ok(out)
    }
    /// Reads the issue, applies `f` and writes it back, all under the write
    /// lock, so a concurrent update can't be overwritten with a stale copy.
    pub fn update_issue(&mut self, id_or_number: &str, f: impl FnOnce(&mut Issue)) -> Result<Issue> {
        self.write(|tx| {
            let mut issue = read_issue(tx, id_or_number)?.ok_or_else(|| LclError::NotFound(id_or_number.to_string()))?;
            let from = issue.status;
            f(&mut issue);
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
            Ok(issue)
        })
    }
    /// Deletes the issue and its child rows. The number-allocation mark is
    /// deliberately left alone, so the number is never reused.
    pub fn delete_issue(&mut self, id_or_number: &str) -> Result<()> {
        self.write(|tx| {
            let issue = read_issue(tx, id_or_number)?.ok_or_else(|| LclError::NotFound(id_or_number.to_string()))?;
            tx.execute("DELETE FROM comments WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issue_cycles WHERE issue_id=?1", params![issue.id])?;
            tx.execute("DELETE FROM issues WHERE id=?1", params![issue.id])?;
            Ok(())
        })
    }

    // ----- comments -----
    /// Looks up the issue (number or id) and adds the comment in one
    /// transaction. `Ok(None)` means there is no such issue.
    pub fn add_comment(&mut self, issue: &str, body: String, author: Option<String>) -> Result<Option<Comment>> {
        self.write(|tx| {
            let Some(target) = read_issue(tx, issue)? else { return Ok(None) };
            let c = Comment { id: uuid::Uuid::new_v4().to_string(), issue_id: target.id, body, author, created_at: crate::models::now() };
            tx.execute("INSERT INTO comments(id,issue_id,body,author,created_at) VALUES(?1,?2,?3,?4,?5)", params![c.id, c.issue_id, c.body, c.author, c.created_at])?;
            Ok(Some(c))
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
        let mut out = vec![];
        for id in ids { if let Some(iss) = self.get_issue(&id)? { out.push(iss); } }
        Ok(out)
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

fn read_issue(conn: &Connection, id_or_number: &str) -> Result<Option<Issue>> {
    // try number first
    if let Ok(n) = id_or_number.parse::<i64>() {
        fetch_issue_by(conn, "number", n.to_string().as_str())
    } else {
        fetch_issue_by(conn, "id", id_or_number)
    }
}

fn fetch_issue_by(conn: &Connection, col: &str, val: &str) -> Result<Option<Issue>> {
    let sql = format!("SELECT id,number,title,description,status,priority,assignee,parent_id,estimate,due_date,created_at,updated_at,completed_at,started_at,canceled_at FROM issues WHERE {}=?1", col);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![val])?;
    if let Some(r) = rows.next()? {
        let issue = row_to_issue(r)?;
        // labels
        let mut li = conn.prepare("SELECT label_id FROM issue_labels WHERE issue_id=?1")?;
        let ls: Vec<String> = li.query_map(params![issue.id], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let mut issue = issue;
        issue.labels = ls;
        Ok(Some(issue))
    } else { Ok(None) }
}

fn row_to_issue(r: &rusqlite::Row) -> Result<Issue> {
    let number: i64 = r.get(1)?;
    let status_str: String = r.get(4)?;
    // Read leniently ("done" is Done), but never guess: a status parse rejects
    // (a hand edit, or a newer lcl) must not be read as something else and
    // then written back.
    let status = Status::parse(&status_str).ok_or(LclError::UnknownStatus { number, value: status_str })?;
    let prio: i64 = r.get(5)?;
    let priority = Priority::from_stored(prio).ok_or(LclError::UnknownPriority { number, value: prio })?;
    Ok(Issue {
        id: r.get(0)?,
        number,
        title: r.get(2)?,
        description: r.get(3)?,
        status,
        priority,
        assignee: r.get(6)?,
        parent_id: r.get(7)?,
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
