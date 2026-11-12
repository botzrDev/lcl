use crate::error::{LclError, Result};
use crate::models::*;
use rusqlite::{params, Connection};
use std::path::Path;

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
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
            "#,
        )?;
        // triggers for fts sync
        self.conn.execute_batch(
            r#"
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
            "#,
        )?;
        Ok(())
    }

    // ----- meta -----
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key=?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(r) = rows.next()? { Ok(Some(r.get(0)?)) } else { Ok(None) }
    }

    // ----- labels -----
    pub fn create_label(&self, l: &Label) -> Result<()> {
        self.conn.execute("INSERT INTO labels(id,name,color) VALUES(?1,?2,?3)", params![l.id, l.name, l.color])?;
        Ok(())
    }
    pub fn list_labels(&self) -> Result<Vec<Label>> {
        let mut stmt = self.conn.prepare("SELECT id,name,color FROM labels ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn delete_label(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM labels WHERE id=?1", params![id])?;
        Ok(())
    }

    // ----- issues -----
    pub fn next_number(&self) -> Result<i64> {
        let n: i64 = self.conn.query_row("SELECT COALESCE(MAX(number),0)+1 FROM issues", [], |r| r.get(0))?;
        Ok(n)
    }
    pub fn create_issue(&self, mut issue: Issue) -> Result<Issue> {
        if issue.id.is_empty() { issue.id = uuid::Uuid::new_v4().to_string(); }
        if issue.number == 0 { issue.number = self.next_number()?; }
        let now = crate::models::now();
        if issue.created_at == 0 { issue.created_at = now; }
        if issue.updated_at == 0 { issue.updated_at = now; }
        let status = issue.status.as_str();
        let priority = issue.priority.as_i64();
        self.conn.execute(
            "INSERT INTO issues(id,number,title,description,status,priority,assignee,parent_id,estimate,due_date,created_at,updated_at,completed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![issue.id, issue.number, issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.created_at, issue.updated_at, issue.completed_at],
        )?;
        for lid in &issue.labels {
            self.conn.execute("INSERT OR IGNORE INTO issue_labels(issue_id,label_id) VALUES(?1,?2)", params![issue.id, lid])?;
        }
        Ok(issue)
    }
    pub fn get_issue(&self, id_or_number: &str) -> Result<Option<Issue>> {
        // try number first
        let row = if let Ok(n) = id_or_number.parse::<i64>() {
            self.fetch_issue_by("number", n.to_string().as_str())
        } else {
            self.fetch_issue_by("id", id_or_number)
        };
        row
    }
    fn fetch_issue_by(&self, col: &str, val: &str) -> Result<Option<Issue>> {
        let sql = format!("SELECT id,number,title,description,status,priority,assignee,parent_id,estimate,due_date,created_at,updated_at,completed_at FROM issues WHERE {}=?1", col);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![val])?;
        if let Some(r) = rows.next()? {
            let issue = self.row_to_issue(r)?;
            // labels
            let mut li = self.conn.prepare("SELECT label_id FROM issue_labels WHERE issue_id=?1")?;
            let ls: Vec<String> = li.query_map(params![issue.id], |r| r.get(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            let mut issue = issue;
            issue.labels = ls;
            Ok(Some(issue))
        } else { Ok(None) }
    }
    fn row_to_issue(&self, r: &rusqlite::Row) -> Result<Issue> {
        let status_str: String = r.get(4)?;
        let prio: i64 = r.get(5)?;
        Ok(Issue {
            id: r.get(0)?,
            number: r.get(1)?,
            title: r.get(2)?,
            description: r.get(3)?,
            status: Status::from_str(&status_str).unwrap_or(Status::Backlog),
            priority: Priority::from_i64(prio),
            assignee: r.get(6)?,
            parent_id: r.get(7)?,
            estimate: r.get(8)?,
            due_date: r.get(9)?,
            labels: vec![],
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
            completed_at: r.get(12)?,
        })
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
                if let Some(a) = assignee { if iss.assignee.as_deref() != Some(a) { continue; } }
                out.push(iss);
            }
        }
        Ok(out)
    }
    pub fn update_issue(&self, id_or_number: &str, f: impl FnOnce(&mut Issue)) -> Result<Issue> {
        let mut issue = self.get_issue(id_or_number)?.ok_or_else(|| LclError::NotFound(id_or_number.to_string()))?;
        f(&mut issue);
        issue.updated_at = crate::models::now();
        if issue.status == Status::Done && issue.completed_at.is_none() { issue.completed_at = Some(issue.updated_at); }
        if issue.status != Status::Done { issue.completed_at = None; }
        let status = issue.status.as_str();
        let priority = issue.priority.as_i64();
        self.conn.execute(
            "UPDATE issues SET title=?1,description=?2,status=?3,priority=?4,assignee=?5,parent_id=?6,estimate=?7,due_date=?8,updated_at=?9,completed_at=?10 WHERE id=?11",
            params![issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.updated_at, issue.completed_at, issue.id],
        )?;
        // sync labels: replace
        self.conn.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue.id])?;
        for lid in &issue.labels {
            self.conn.execute("INSERT OR IGNORE INTO issue_labels(issue_id,label_id) VALUES(?1,?2)", params![issue.id, lid])?;
        }
        Ok(issue)
    }
    pub fn delete_issue(&self, id_or_number: &str) -> Result<()> {
        let issue = self.get_issue(id_or_number)?.ok_or_else(|| LclError::NotFound(id_or_number.to_string()))?;
        self.conn.execute("DELETE FROM comments WHERE issue_id=?1", params![issue.id])?;
        self.conn.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue.id])?;
        self.conn.execute("DELETE FROM issue_cycles WHERE issue_id=?1", params![issue.id])?;
        self.conn.execute("DELETE FROM issues WHERE id=?1", params![issue.id])?;
        Ok(())
    }

    // ----- comments -----
    pub fn add_comment(&self, c: Comment) -> Result<Comment> {
        let mut c = c;
        if c.id.is_empty() { c.id = uuid::Uuid::new_v4().to_string(); }
        if c.created_at == 0 { c.created_at = crate::models::now(); }
        self.conn.execute("INSERT INTO comments(id,issue_id,body,author,created_at) VALUES(?1,?2,?3,?4,?5)", params![c.id, c.issue_id, c.body, c.author, c.created_at])?;
        Ok(c)
    }
    pub fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>> {
        let mut stmt = self.conn.prepare("SELECT id,issue_id,body,author,created_at FROM comments WHERE issue_id=?1 ORDER BY created_at")?;
        let rows = stmt.query_map(params![issue_id], |r| Ok(Comment { id: r.get(0)?, issue_id: r.get(1)?, body: r.get(2)?, author: r.get(3)?, created_at: r.get(4)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }

    // ----- docs -----
    pub fn create_doc(&self, mut d: Doc) -> Result<Doc> {
        if d.id.is_empty() { d.id = uuid::Uuid::new_v4().to_string(); }
        let now = crate::models::now();
        if d.created_at == 0 { d.created_at = now; }
        if d.updated_at == 0 { d.updated_at = now; }
        self.conn.execute("INSERT INTO docs(id,title,content,parent_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6)", params![d.id, d.title, d.content, d.parent_id, d.created_at, d.updated_at])?;
        Ok(d)
    }
    pub fn get_doc(&self, id: &str) -> Result<Option<Doc>> {
        let mut stmt = self.conn.prepare("SELECT id,title,content,parent_id,created_at,updated_at FROM docs WHERE id=?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(r) = rows.next()? {
            Ok(Some(Doc { id: r.get(0)?, title: r.get(1)?, content: r.get(2)?, parent_id: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)? }))
        } else { Ok(None) }
    }
    pub fn list_docs(&self) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare("SELECT id,title,content,parent_id,created_at,updated_at FROM docs ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| Ok(Doc { id: r.get(0)?, title: r.get(1)?, content: r.get(2)?, parent_id: r.get(3)?, created_at: r.get(4)?, updated_at: r.get(5)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }
    pub fn update_doc(&self, id: &str, title: Option<String>, content: Option<String>) -> Result<Doc> {
        let mut d = self.get_doc(id)?.ok_or_else(|| LclError::NotFound(id.to_string()))?;
        if let Some(t) = title { d.title = t; }
        if let Some(c) = content { d.content = c; }
        d.updated_at = crate::models::now();
        self.conn.execute("UPDATE docs SET title=?1,content=?2,updated_at=?3 WHERE id=?4", params![d.title, d.content, d.updated_at, d.id])?;
        Ok(d)
    }
    pub fn delete_doc(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE id=?1", params![id])?;
        Ok(())
    }

    // ----- cycles -----
    pub fn create_cycle(&self, mut c: Cycle) -> Result<Cycle> {
        if c.id.is_empty() { c.id = uuid::Uuid::new_v4().to_string(); }
        if c.created_at == 0 { c.created_at = crate::models::now(); }
        self.conn.execute("INSERT INTO cycles(id,name,starts_at,ends_at,created_at) VALUES(?1,?2,?3,?4,?5)", params![c.id, c.name, c.starts_at, c.ends_at, c.created_at])?;
        Ok(c)
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
