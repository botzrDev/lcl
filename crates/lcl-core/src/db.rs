use crate::error::{LclError, Result};
use crate::migrations::{MIGRATIONS, SCHEMA_VERSION};
use crate::models::*;
use crate::search::{Field, IssueHit, SearchQuery, SearchResults};
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params, params_from_iter, Connection, ErrorCode, OptionalExtension, ToSql, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// How long a connection waits for a lock held by another lcl process before
/// failing with SQLITE_BUSY. Concurrent first opens queue on the migration lock.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Puts a database (a workspace, or the registry's index) in WAL mode. The
/// switch needs an exclusive lock, and when waiting for it could deadlock
/// (several processes opening a brand-new file at once, each holding a lock
/// another needs), SQLite returns BUSY at once instead of calling the busy
/// handler. A failed attempt releases the locks it held, so retrying makes
/// progress; give up when the busy timeout would have. The mode it answers
/// with ("memory" for an in-memory database) isn't needed. Can't run inside
/// a transaction.
pub(crate) fn use_wal(conn: &Connection) -> Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(())) {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::DatabaseBusy && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            done => return Ok(done?),
        }
    }
}

/// The `meta` key holding the next issue number to hand out (seeded by
/// migration 2; the name must match it). It only ever grows: deleting an issue
/// never lowers it, so no number is handed out twice.
const NEXT_ISSUE_NUMBER: &str = "next_issue_number";

pub struct Store {
    pub conn: Connection,
}

/// Which issues `Store::list_issues` returns. Each part narrows the list
/// (they AND); an empty or `None` part doesn't narrow it.
#[derive(Debug, Clone, Default)]
pub struct IssueFilter {
    /// Any of these statuses (they OR). A stored status is read by the same
    /// rule as everywhere else (`Status::parse`), so a hand-edited "done" is
    /// Done here too.
    pub statuses: Vec<Status>,
    pub assignee: Option<AssigneeFilter>,
    /// Every one of these labels (they AND), each named ignoring case. An
    /// unknown name is an error.
    pub labels: Vec<String>,
    /// Only the direct sub-issues of this issue.
    pub parent: Option<IssueRef>,
    /// `Some(true)`: only blocked issues, those with at least one blocker
    /// that isn't Done, Canceled or Duplicate. `Some(false)`: only issues
    /// with no such blocker.
    pub blocked: Option<bool>,
    /// Only issues in this project, or in none. An unknown project is an
    /// error, not an empty list.
    pub project: Option<ProjectFilter>,
    /// Only issues with this milestone (name or id), looked up in the
    /// `project` filter's project when there is one, else in the workspace.
    pub milestone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFilter {
    /// Issues in no project.
    NoProject,
    /// Issues in the project with this name (ignoring ASCII case) or id.
    Is(String),
}

/// What `update_issue` does to one field: leave it, clear it, or set it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Change<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

/// Where `create_issue` and `update_issue` put an issue. The project and the
/// milestone are given by name (ignoring ASCII case) or id, and resolved
/// inside the write transaction. A milestone is looked up in the issue's
/// resulting project: the one given, or the one it has.
#[derive(Debug, Clone, Default)]
pub struct Placement {
    pub project: Change<String>,
    pub milestone: Change<String>,
}

/// A milestone `update_issue` took off an issue because the issue left the
/// milestone's project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearedMilestone {
    pub milestone: String,
    pub project: String,
}

/// `update_project`'s changes; None leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct ProjectUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    pub target_date: Change<String>,
}

/// `update_milestone`'s changes; None leaves a field alone. There is no way
/// to move a milestone to another project.
#[derive(Debug, Clone, Default)]
pub struct MilestoneUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub target_date: Change<String>,
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssigneeFilter {
    Unassigned,
    /// Assigned to this name, ignoring ASCII case.
    Named(String),
}

/// The order of `Store::list_issues`. Ties go to the higher number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueSort {
    /// Newest number first.
    #[default]
    Number,
    /// Newest first.
    Created,
    /// Most recently updated first.
    Updated,
    /// Urgent, High, Medium, Low, then None, as Linear sorts.
    Priority,
}

impl IssueSort {
    fn order_by(self) -> &'static str {
        match self {
            IssueSort::Number => "i.number DESC",
            IssueSort::Created => "i.created_at DESC, i.number DESC",
            IssueSort::Updated => "i.updated_at DESC, i.number DESC",
            // Stored: None 0, Urgent 1 .. Low 4. None goes after Low.
            IssueSort::Priority => "CASE i.priority WHEN 0 THEN 5 ELSE i.priority END, i.number DESC",
        }
    }
}

impl FromStr for IssueSort {
    type Err = InvalidValue;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "number" => Ok(IssueSort::Number),
            "created" => Ok(IssueSort::Created),
            "updated" => Ok(IssueSort::Updated),
            "priority" => Ok(IssueSort::Priority),
            _ => Err(InvalidValue(format!("invalid sort \"{s}\"; expected one of: number, created, updated, priority"))),
        }
    }
}

/// Which stretch of the sorted list to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// At most this many; `None` for all of them.
    pub limit: Option<u32>,
    /// Skip this many first.
    pub offset: u32,
}

impl Page {
    pub const ALL: Page = Page { limit: None, offset: 0 };
}

/// One page of `issue list`, and where it sits in the whole list.
#[derive(Debug, Clone, Serialize)]
pub struct IssueList {
    pub issues: Vec<Issue>,
    /// How many issues match the filter, whatever the page.
    pub total: usize,
    /// `None` (null) for all of them.
    pub limit: Option<u32>,
    pub offset: u32,
    /// Whether matching issues come after this page: offset + returned < total.
    pub truncated: bool,
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
        use_wal(&conn)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        register_lcl_status(&conn)?;
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

    /// Runs `f` in one read transaction (`BEGIN DEFERRED` … `COMMIT`). In WAL
    /// mode every query in it reads the same snapshot, so a count and the
    /// page it counts agree even while another process writes. Takes no
    /// write lock.
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
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
    /// Adds the label. A name that exists, ignoring case, is refused: names
    /// are how labels are given on the command line.
    pub fn create_label(&mut self, l: &Label) -> Result<()> {
        self.write(|tx| {
            if let Some(existing) = label_by_name(tx, &l.name)? {
                return Err(LclError::LabelExists { name: l.name.clone(), existing: existing.name });
            }
            tx.execute("INSERT INTO labels(id,name,color) VALUES(?1,?2,?3)", params![l.id, l.name, l.color])?;
            Ok(())
        })
    }
    pub fn list_labels(&self) -> Result<Vec<Label>> {
        let mut stmt = self.conn.prepare("SELECT id,name,color FROM labels ORDER BY name COLLATE NOCASE, name")?;
        let rows = stmt.query_map([], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
    /// Fails with `UnknownLabel` unless every name names a label.
    pub fn require_labels(&self, names: &[String]) -> Result<()> {
        for name in names {
            label_by_name(&self.conn, name)?.ok_or_else(|| LclError::UnknownLabel(name.clone()))?;
        }
        Ok(())
    }
    /// Deletes the label named (ignoring case) or with id `name_or_id`. The
    /// foreign key cascades it off every issue. Returns the label and how
    /// many issues it was removed from.
    pub fn delete_label(&mut self, name_or_id: &str) -> Result<(Label, i64)> {
        self.write(|tx| {
            let by_id = tx
                .query_row("SELECT id,name,color FROM labels WHERE id=?1", params![name_or_id], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))
                .optional()?;
            let label = match by_id {
                Some(l) => l,
                None => label_by_name(tx, name_or_id)?.ok_or_else(|| LclError::LabelNotFound(name_or_id.to_string()))?,
            };
            let issues: i64 = tx.query_row("SELECT count(*) FROM issue_labels WHERE label_id=?1", params![label.id], |r| r.get(0))?;
            tx.execute("DELETE FROM labels WHERE id=?1", params![label.id])?;
            Ok((label, issues))
        })
    }

    // ----- issues -----
    /// Inserts `issue` and returns it as stored. A zero `number` is allocated
    /// from the high-water mark; either way the mark ends up past the number
    /// used. A `parent_id` must be the UUID of an existing issue.
    pub fn create_issue(&mut self, mut issue: Issue, placement: &Placement) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            if let Some(parent) = &issue.parent_id { check_parent(tx, &key, None, parent)?; }
            (issue.project_id, issue.milestone_id) = (None, None);
            place(tx, &mut issue, placement)?;
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
                "INSERT INTO issues(id,number,title,description,status,priority,assignee,parent_id,estimate,due_date,created_at,updated_at,completed_at,started_at,canceled_at,project_id,milestone_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![issue.id, issue.number, issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.created_at, issue.updated_at, issue.completed_at, issue.started_at, issue.canceled_at, issue.project_id, issue.milestone_id],
            )?;
            set_labels(tx, &issue.id, &issue.labels)?;
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
    /// The issues `filter` selects, in `sort` order, cut to `page`. Every
    /// filter is in SQL, before LIMIT, and the total and the page come from
    /// one read transaction, so they agree.
    pub fn list_issues(&self, filter: &IssueFilter, sort: IssueSort, page: Page) -> Result<IssueList> {
        self.read(|conn| {
            let key = read_key(conn)?;
            let mut conds: Vec<String> = vec![];
            let mut args: Vec<Value> = vec![];
            if !filter.statuses.is_empty() {
                conds.push(format!("lcl_status(i.status) IN ({})", vec!["?"; filter.statuses.len()].join(",")));
                args.extend(filter.statuses.iter().map(|s| Value::Text(s.as_str().to_string())));
            }
            match &filter.assignee {
                Some(AssigneeFilter::Unassigned) => conds.push("i.assignee IS NULL".into()),
                Some(AssigneeFilter::Named(name)) => {
                    conds.push("i.assignee = ? COLLATE NOCASE".into());
                    args.push(Value::Text(name.clone()));
                }
                None => {}
            }
            for name in &filter.labels {
                let label = label_by_name(conn, name)?.ok_or_else(|| LclError::UnknownLabel(name.clone()))?;
                conds.push("EXISTS (SELECT 1 FROM issue_labels il WHERE il.issue_id = i.id AND il.label_id = ?)".into());
                args.push(Value::Text(label.id));
            }
            if let Some(parent) = &filter.parent {
                conds.push("i.parent_id = ?".into());
                args.push(Value::Text(resolve_issue(conn, parent)?.id));
            }
            if let Some(blocked) = filter.blocked {
                // A blocker whose status can't be read isn't known to be
                // resolved, so it still blocks.
                let open_blocker = "EXISTS (SELECT 1 FROM issue_relations r JOIN issues b ON b.id = r.issue_id
                    WHERE r.related_id = i.id AND r.kind = 'blocks' AND coalesce(lcl_status(b.status), '') NOT IN ('Done', 'Canceled', 'Duplicate'))";
                conds.push(if blocked { open_blocker.to_string() } else { format!("NOT {open_blocker}") });
            }
            let project = match &filter.project {
                Some(ProjectFilter::NoProject) => {
                    conds.push("i.project_id IS NULL".into());
                    None
                }
                Some(ProjectFilter::Is(r)) => {
                    let project = resolve_project(conn, r)?;
                    conds.push("i.project_id = ?".into());
                    args.push(Value::Text(project.id.clone()));
                    Some(project)
                }
                None => None,
            };
            if let Some(m) = &filter.milestone {
                if filter.project == Some(ProjectFilter::NoProject) {
                    return Err(LclError::MilestoneInNoProject(m.clone()));
                }
                let milestone = resolve_milestone(conn, m, project.as_ref())?;
                conds.push("i.milestone_id = ?".into());
                args.push(Value::Text(milestone.id));
            }
            let where_sql = if conds.is_empty() { String::new() } else { format!(" WHERE {}", conds.join(" AND ")) };

            let total: i64 = conn.query_row(&format!("SELECT count(*) FROM issues i{where_sql}"), params_from_iter(&args), |r| r.get(0))?;
            let limit = page.limit.map_or(-1, i64::from);
            let sql = format!("{ISSUE_SELECT}{where_sql} ORDER BY {} LIMIT {limit} OFFSET {}", sort.order_by(), page.offset);
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(&args))?;
            let mut issues = vec![];
            while let Some(r) = rows.next()? {
                issues.push(row_to_issue(r, &key)?);
            }
            attach_labels(conn, &mut issues)?;

            let total = usize::try_from(total).map_err(|_| LclError::Msg(format!("count(*) returned {total}")))?;
            let truncated = page.offset as usize + issues.len() < total;
            Ok(IssueList { issues, total, limit: page.limit, offset: page.offset, truncated })
        })
    }
    /// Reads the issue, applies `f` and writes it back, all under the write
    /// lock, so a concurrent update can't be overwritten with a stale copy.
    /// A changed `parent_id` must be the UUID of an existing issue that is
    /// neither this one nor below it. `placement` moves it between projects
    /// and milestones (see `place`); a milestone it loses because it left the
    /// milestone's project is returned, for the caller to report.
    pub fn update_issue(&mut self, r: &IssueRef, placement: &Placement, f: impl FnOnce(&mut Issue)) -> Result<(Issue, Option<ClearedMilestone>)> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let mut issue = resolve_issue(tx, r)?;
            let (from, old_parent) = (issue.status, issue.parent_id.clone());
            f(&mut issue);
            if issue.parent_id != old_parent && let Some(parent) = &issue.parent_id {
                check_parent(tx, &key, Some(&issue), parent)?;
            }
            let cleared = place(tx, &mut issue, placement)?;
            issue.updated_at = crate::models::now();
            issue.apply_lifecycle(Some(from), issue.updated_at);
            let status = issue.status.as_str();
            let priority = issue.priority.as_i64();
            tx.execute(
                "UPDATE issues SET title=?1,description=?2,status=?3,priority=?4,assignee=?5,parent_id=?6,estimate=?7,due_date=?8,updated_at=?9,completed_at=?10,started_at=?11,canceled_at=?12,project_id=?13,milestone_id=?14 WHERE id=?15",
                params![issue.title, issue.description, status, priority, issue.assignee, issue.parent_id, issue.estimate, issue.due_date, issue.updated_at, issue.completed_at, issue.started_at, issue.canceled_at, issue.project_id, issue.milestone_id, issue.id],
            )?;
            set_labels(tx, &issue.id, &issue.labels)?;
            Ok((reread(tx, &key, &issue.id)?, cleared))
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

    // ----- relations -----
    /// Records `a <kind> b` in its stored form (see migration V7). Returns the
    /// relation as typed, and false when it was already there (a no-op).
    /// Refused, inside the transaction: relating an issue to itself, a
    /// `blocks` row that would close a cycle, a second duplicate-of target,
    /// and a duplicate-of cycle. duplicate-of also sets `a`'s status to
    /// Duplicate, with the lifecycle that goes with it.
    pub fn relate(&mut self, a: &IssueRef, kind: RelationKind, b: &IssueRef) -> Result<(Relation, bool)> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let edge = resolve_edge(tx, a, kind, b)?;
            if edge_exists(tx, &edge)? {
                return Ok((edge.typed, false));
            }
            check_edge(tx, &key, &edge)?;
            tx.execute(
                "INSERT INTO issue_relations(issue_id, related_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![edge.from.id, edge.to.id, edge.stored, crate::models::now()],
            )?;
            if edge.stored == "duplicate" && edge.from.status != Status::Duplicate {
                let mut dup = edge.from.clone();
                let from = dup.status;
                dup.status = Status::Duplicate;
                dup.updated_at = crate::models::now();
                dup.apply_lifecycle(Some(from), dup.updated_at);
                tx.execute(
                    "UPDATE issues SET status=?1, updated_at=?2, completed_at=?3, started_at=?4, canceled_at=?5 WHERE id=?6",
                    params![dup.status.as_str(), dup.updated_at, dup.completed_at, dup.started_at, dup.canceled_at, dup.id],
                )?;
            }
            Ok((edge.typed, true))
        })
    }
    /// Removes `a <kind> b`, given as it was made or in its other form
    /// (`b blocks a` for `a blocked-by b`). Statuses are left alone. A
    /// relation that isn't there is an error.
    pub fn unrelate(&mut self, a: &IssueRef, kind: RelationKind, b: &IssueRef) -> Result<Relation> {
        self.write(|tx| {
            let edge = resolve_edge(tx, a, kind, b)?;
            let removed = tx.execute(
                "DELETE FROM issue_relations WHERE issue_id=?1 AND related_id=?2 AND kind=?3",
                params![edge.from.id, edge.to.id, edge.stored],
            )?;
            if removed == 0 {
                return Err(LclError::RelationNotFound(edge.typed.to_string()));
            }
            Ok(edge.typed)
        })
    }
    /// Every relation of the issue with UUID `issue_id`, seen from it.
    pub fn relations(&self, issue_id: &str) -> Result<Relations> {
        self.read(|conn| {
            let key = read_key(conn)?;
            let mut stmt = conn.prepare(
                "SELECT x.side, i.number, i.title, i.status FROM (
                    SELECT 'blocks' AS side, related_id AS other FROM issue_relations WHERE issue_id = ?1 AND kind = 'blocks'
                    UNION ALL SELECT 'blocked_by', issue_id FROM issue_relations WHERE related_id = ?1 AND kind = 'blocks'
                    UNION ALL SELECT 'related', related_id FROM issue_relations WHERE issue_id = ?1 AND kind = 'related'
                    UNION ALL SELECT 'related', issue_id FROM issue_relations WHERE related_id = ?1 AND kind = 'related'
                    UNION ALL SELECT 'duplicate_of', related_id FROM issue_relations WHERE issue_id = ?1 AND kind = 'duplicate'
                    UNION ALL SELECT 'duplicates', issue_id FROM issue_relations WHERE related_id = ?1 AND kind = 'duplicate'
                ) x JOIN issues i ON i.id = x.other ORDER BY i.number",
            )?;
            let rows = stmt.query_map(params![issue_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
            let mut out = Relations::default();
            for row in rows {
                let (side, number, title, status) = row?;
                let status = Status::parse(&status).ok_or(LclError::UnknownStatus { number, value: status })?;
                let other = RelatedIssue { identifier: identifier(&key, number), title, status };
                match side.as_str() {
                    "blocks" => out.blocks.push(other),
                    "blocked_by" => out.blocked_by.push(other),
                    "related" => out.related.push(other),
                    "duplicate_of" => {
                        out.duplicate_of.get_or_insert(other);
                    }
                    _ => out.duplicates.push(other),
                }
            }
            Ok(out)
        })
    }

    // ----- projects -----
    /// Creates a project. Its name must be free, ignoring ASCII case.
    pub fn create_project(&mut self, name: &str, description: &str, status: ProjectStatus, target_date: Option<&str>) -> Result<Project> {
        self.write(|tx| {
            if let Some(existing) = project_named(tx, name, None)? {
                return Err(LclError::ProjectExists { name: name.to_string(), existing: existing.name });
            }
            let id = uuid::Uuid::new_v4().to_string();
            let now = crate::models::now();
            tx.execute(
                "INSERT INTO projects(id, name, description, status, target_date, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![id, name, description, status.as_str(), target_date, now],
            )?;
            project_by_id(tx, &id)
        })
    }
    /// Projects with any of these statuses (all when empty), by name
    /// ignoring case.
    pub fn list_projects(&self, statuses: &[ProjectStatus]) -> Result<Vec<Project>> {
        self.read(|conn| {
            let wanted: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
            let mut stmt = conn.prepare(&format!(
                "{PROJECT_SELECT} WHERE ?1 = '[]' OR status IN (SELECT value FROM json_each(?1)) ORDER BY name COLLATE NOCASE, name"
            ))?;
            let rows = stmt.query_and_then(params![json_array(&wanted)?], row_to_project)?;
            rows.collect()
        })
    }
    /// The project `r` names (name ignoring ASCII case, or id), with its
    /// issue counts and its milestones, from one snapshot.
    pub fn project_detail(&self, r: &str) -> Result<ProjectDetail> {
        self.read(|conn| {
            let project = resolve_project(conn, r)?;
            let issues = issue_counts(conn, "project_id", &project.id)?;
            let mut stmt = conn.prepare(
                "SELECT m.id, m.name, m.target_date, m.sort_order, (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id)
                 FROM milestones m WHERE m.project_id = ?1 ORDER BY m.sort_order, m.name COLLATE NOCASE, m.name",
            )?;
            let milestones = stmt
                .query_map(params![project.id], |r| {
                    Ok(MilestoneSummary { id: r.get(0)?, name: r.get(1)?, target_date: r.get(2)?, sort_order: r.get(3)?, issues: r.get(4)? })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ProjectDetail { project, issues, milestones })
        })
    }
    /// Applies `u` to the project `r` names. A new name must be free,
    /// ignoring ASCII case, except for this project's own.
    pub fn update_project(&mut self, r: &str, u: &ProjectUpdate) -> Result<Project> {
        self.write(|tx| {
            let mut project = resolve_project(tx, r)?;
            if let Some(name) = &u.name {
                if let Some(existing) = project_named(tx, name, Some(&project.id))? {
                    return Err(LclError::ProjectExists { name: name.clone(), existing: existing.name });
                }
                project.name = name.clone();
            }
            if let Some(d) = &u.description { project.description = d.clone(); }
            if let Some(st) = u.status { project.status = st; }
            match &u.target_date {
                Change::Keep => {}
                Change::Clear => project.target_date = None,
                Change::Set(d) => project.target_date = Some(d.clone()),
            }
            project.updated_at = crate::models::now();
            tx.execute(
                "UPDATE projects SET name=?1, description=?2, status=?3, target_date=?4, updated_at=?5 WHERE id=?6",
                params![project.name, project.description, project.status.as_str(), project.target_date, project.updated_at, project.id],
            )?;
            Ok(project)
        })
    }
    /// Deletes the project `r` names. Refused while issues or milestones
    /// reference it; nothing changes then.
    pub fn delete_project(&mut self, r: &str) -> Result<Project> {
        self.write(|tx| {
            let project = resolve_project(tx, r)?;
            let issues: i64 = tx.query_row("SELECT count(*) FROM issues WHERE project_id = ?1", params![project.id], |r| r.get(0))?;
            let milestones: i64 = tx.query_row("SELECT count(*) FROM milestones WHERE project_id = ?1", params![project.id], |r| r.get(0))?;
            if issues + milestones > 0 {
                return Err(LclError::ProjectInUse { name: project.name, issues, milestones });
            }
            tx.execute("DELETE FROM projects WHERE id = ?1", params![project.id])?;
            Ok(project)
        })
    }

    // ----- milestones -----
    /// Creates a milestone in the project `project` names. Its name must be
    /// free in that project, ignoring ASCII case.
    pub fn create_milestone(&mut self, name: &str, project: &str, description: &str, target_date: Option<&str>, sort_order: i64) -> Result<Milestone> {
        self.write(|tx| {
            let project = resolve_project(tx, project)?;
            if let Some(existing) = milestone_named(tx, &project, name, None)? {
                return Err(LclError::MilestoneExists { name: name.to_string(), existing: existing.name, project: project.name });
            }
            let id = uuid::Uuid::new_v4().to_string();
            let now = crate::models::now();
            tx.execute(
                "INSERT INTO milestones(id, project_id, name, description, target_date, sort_order, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![id, project.id, name, description, target_date, sort_order, now],
            )?;
            milestone_by_id(tx, &id)
        })
    }
    /// Milestones, of one project or all, by project name, sort_order, then
    /// name (names ignoring case).
    pub fn list_milestones(&self, project: Option<&str>) -> Result<Vec<Milestone>> {
        self.read(|conn| {
            let project_id = project.map(|p| resolve_project(conn, p)).transpose()?.map(|p| p.id);
            let mut stmt = conn.prepare(&format!(
                "{MILESTONE_SELECT} WHERE ?1 IS NULL OR m.project_id = ?1 ORDER BY p.name COLLATE NOCASE, p.name, m.sort_order, m.name COLLATE NOCASE, m.name"
            ))?;
            let rows = stmt.query_and_then(params![project_id], row_to_milestone)?;
            rows.collect()
        })
    }
    /// The milestone `r` names (see `resolve_milestone`), with its issue counts.
    pub fn milestone_detail(&self, r: &str, project: Option<&str>) -> Result<MilestoneDetail> {
        self.read(|conn| {
            let project = project.map(|p| resolve_project(conn, p)).transpose()?;
            let milestone = resolve_milestone(conn, r, project.as_ref())?;
            let issues = issue_counts(conn, "milestone_id", &milestone.id)?;
            Ok(MilestoneDetail { milestone, issues })
        })
    }
    /// Applies `u` to the milestone `r` names. A new name must be free in its
    /// project, ignoring ASCII case, except for its own.
    pub fn update_milestone(&mut self, r: &str, project: Option<&str>, u: &MilestoneUpdate) -> Result<Milestone> {
        self.write(|tx| {
            let project = project.map(|p| resolve_project(tx, p)).transpose()?;
            let mut m = resolve_milestone(tx, r, project.as_ref())?;
            if let Some(name) = &u.name {
                let own = project_by_id(tx, &m.project_id)?;
                if let Some(existing) = milestone_named(tx, &own, name, Some(&m.id))? {
                    return Err(LclError::MilestoneExists { name: name.clone(), existing: existing.name, project: own.name });
                }
                m.name = name.clone();
            }
            if let Some(d) = &u.description { m.description = d.clone(); }
            match &u.target_date {
                Change::Keep => {}
                Change::Clear => m.target_date = None,
                Change::Set(d) => m.target_date = Some(d.clone()),
            }
            if let Some(o) = u.sort_order { m.sort_order = o; }
            m.updated_at = crate::models::now();
            tx.execute(
                "UPDATE milestones SET name=?1, description=?2, target_date=?3, sort_order=?4, updated_at=?5 WHERE id=?6",
                params![m.name, m.description, m.target_date, m.sort_order, m.updated_at, m.id],
            )?;
            Ok(m)
        })
    }
    /// Deletes the milestone `r` names. Refused while issues reference it.
    pub fn delete_milestone(&mut self, r: &str, project: Option<&str>) -> Result<Milestone> {
        self.write(|tx| {
            let project = project.map(|p| resolve_project(tx, p)).transpose()?;
            let m = resolve_milestone(tx, r, project.as_ref())?;
            let issues: i64 = tx.query_row("SELECT count(*) FROM issues WHERE milestone_id = ?1", params![m.id], |r| r.get(0))?;
            if issues > 0 {
                return Err(LclError::MilestoneInUse { name: m.name, issues });
            }
            tx.execute("DELETE FROM milestones WHERE id = ?1", params![m.id])?;
            Ok(m)
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
            if tx.execute("DELETE FROM docs WHERE id=?1", params![id])? == 0 {
                return Err(LclError::DocNotFound(id.to_string()));
            }
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
    /// Issues and docs matching `q`, at most `limit` of each.
    ///
    /// An issue matches when every word is in its title and description
    /// together, or every word is in one of its comments; it is listed once
    /// either way, ranked by the best (lowest) bm25 of those matches. If the
    /// query is exactly this workspace's `KEY-N` or `#N` for an issue that
    /// exists, that issue comes first. `matched_in` comes from per-column
    /// MATCHes (`{title} : (any word)`), counted only for issues whose own
    /// text matched. Every query runs in one read transaction.
    pub fn search(&self, q: &SearchQuery, limit: usize) -> Result<SearchResults> {
        self.read(|conn| {
            struct Hit {
                rank: f64,
                number: i64,
                fields: Vec<Field>,
            }
            let mut hits: HashMap<String, Hit> = HashMap::new();
            let all = q.all_words();

            let mut stmt = conn.prepare(
                "SELECT i.id, i.number, bm25(issues_fts) FROM issues_fts JOIN issues i ON i.rowid = issues_fts.rowid WHERE issues_fts MATCH ?1",
            )?;
            for row in stmt.query_map(params![all], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)))? {
                let (id, number, rank) = row?;
                hits.insert(id, Hit { rank, number, fields: vec![] });
            }
            for (field, col) in [(Field::Title, "title"), (Field::Description, "description")] {
                let mut stmt = conn.prepare("SELECT i.id FROM issues_fts JOIN issues i ON i.rowid = issues_fts.rowid WHERE issues_fts MATCH ?1")?;
                for id in stmt.query_map(params![q.any_word_in(col)], |r| r.get::<_, String>(0))? {
                    if let Some(hit) = hits.get_mut(&id?) {
                        hit.fields.push(field);
                    }
                }
            }

            let mut stmt = conn.prepare(
                "SELECT c.issue_id, i.number, bm25(comments_fts) FROM comments_fts
                 JOIN comments c ON c.rowid = comments_fts.rowid JOIN issues i ON i.id = c.issue_id
                 WHERE comments_fts MATCH ?1",
            )?;
            for row in stmt.query_map(params![all], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)))? {
                let (id, number, rank) = row?;
                let hit = hits.entry(id).or_insert(Hit { rank, number, fields: vec![] });
                hit.rank = hit.rank.min(rank);
                if !hit.fields.contains(&Field::Comments) {
                    hit.fields.push(Field::Comments);
                }
            }

            // The issue the query names, if it's ours and exists. Another
            // workspace's key, or a missing number, is just text.
            let mut named = None;
            if let Some(IssueRef::Number { key: typed, number }) = q.identifier() {
                let key = read_key(conn)?;
                let ours = match typed {
                    Some(typed) => !key.is_empty() && typed.to_lowercase() == key.to_lowercase(),
                    None => true,
                };
                if ours && let Some(id) = conn.query_row("SELECT id FROM issues WHERE number=?1", params![number], |r| r.get::<_, String>(0)).optional()? {
                    let hit = hits.entry(id.clone()).or_insert(Hit { rank: 0.0, number: *number, fields: vec![] });
                    hit.fields.insert(0, Field::Identifier);
                    named = Some(id);
                }
            }

            let mut ranked: Vec<(String, Hit)> = hits.into_iter().collect();
            ranked.sort_by(|(a_id, a), (b_id, b)| {
                let (a_named, b_named) = (named.as_ref() == Some(a_id), named.as_ref() == Some(b_id));
                b_named.cmp(&a_named).then(a.rank.total_cmp(&b.rank)).then(b.number.cmp(&a.number))
            });
            let issue_total = ranked.len();
            // The page's issues, and all their labels, in one query each.
            let page: Vec<(String, Hit)> = ranked.into_iter().take(limit).collect();
            let ids: Vec<String> = page.iter().map(|(id, _)| id.clone()).collect();
            let mut fields: HashMap<String, Vec<Field>> = page.into_iter().map(|(id, hit)| (id, hit.fields)).collect();
            let issues: Vec<IssueHit> = fetch_issues(conn, &ids)?
                .into_iter()
                .map(|issue| IssueHit { matched_in: fields.remove(&issue.id).unwrap_or_default(), issue })
                .collect();

            let mut stmt = conn.prepare(
                "SELECT d.id FROM docs_fts JOIN docs d ON d.rowid = docs_fts.rowid WHERE docs_fts MATCH ?1 ORDER BY bm25(docs_fts), d.updated_at DESC",
            )?;
            let doc_ids: Vec<String> = stmt.query_map(params![all], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            let doc_total = doc_ids.len();
            let mut docs = Vec::with_capacity(limit.min(doc_total));
            for id in doc_ids.iter().take(limit) {
                if let Some(d) = read_doc(conn, id)? {
                    docs.push(d);
                }
            }

            Ok(SearchResults {
                query: q.raw().to_string(),
                truncated: issue_total > limit || doc_total > limit,
                issues,
                docs,
                issue_total,
                doc_total,
                limit,
            })
        })
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

/// The label named `name`, ignoring (ASCII) case: the same rule as the
/// unique index on labels(name COLLATE NOCASE), so at most one matches.
fn label_by_name(conn: &Connection, name: &str) -> Result<Option<Label>> {
    Ok(conn
        .query_row("SELECT id,name,color FROM labels WHERE name=?1 COLLATE NOCASE", params![name], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))
        .optional()?)
}

/// Makes the issue's labels exactly `names` (each matched ignoring case;
/// repeats collapse). Must run inside the write transaction that reads the
/// issue, so a concurrent add can't be lost.
fn set_labels(conn: &Connection, issue_id: &str, names: &[String]) -> Result<()> {
    conn.execute("DELETE FROM issue_labels WHERE issue_id=?1", params![issue_id])?;
    for name in names {
        let label = label_by_name(conn, name)?.ok_or_else(|| LclError::UnknownLabel(name.clone()))?;
        conn.execute("INSERT OR IGNORE INTO issue_labels(issue_id,label_id) VALUES(?1,?2)", params![issue_id, label.id])?;
    }
    Ok(())
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

/// A relation as typed, resolved, and in its stored form: `from <stored> to`.
struct Edge {
    typed: Relation,
    from: Issue,
    to: Issue,
    stored: &'static str,
}

/// Resolves both issues and works out the stored row. Relating an issue to
/// itself is refused here, for unrelate too: no such row can exist.
fn resolve_edge(conn: &Connection, a: &IssueRef, kind: RelationKind, b: &IssueRef) -> Result<Edge> {
    let a = resolve_issue(conn, a)?;
    let b = resolve_issue(conn, b)?;
    if a.id == b.id {
        return Err(LclError::SelfRelation(a.identifier));
    }
    let typed = Relation { issue: a.identifier.clone(), kind, related: b.identifier.clone() };
    let (from, to, stored) = match kind {
        RelationKind::Blocks => (a, b, "blocks"),
        RelationKind::BlockedBy => (b, a, "blocks"),
        // Symmetric: the smaller id first, so either order is the same row.
        RelationKind::Related if a.id < b.id => (a, b, "related"),
        RelationKind::Related => (b, a, "related"),
        RelationKind::DuplicateOf => (a, b, "duplicate"),
    };
    Ok(Edge { typed, from, to, stored })
}

fn edge_exists(conn: &Connection, e: &Edge) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM issue_relations WHERE issue_id=?1 AND related_id=?2 AND kind=?3", params![e.from.id, e.to.id, e.stored], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Refuses a new row that would close a `blocks` cycle (of any length), a
/// second duplicate-of target, or a duplicate-of cycle. Must run inside the
/// write transaction that inserts the row, so two opposite relates can't
/// both pass it.
fn check_edge(conn: &Connection, key: &str, e: &Edge) -> Result<()> {
    match e.stored {
        "blocks" => {
            // from -> to closes a cycle if `to` already reaches `from`.
            if let Some(path) = blocks_path(conn, &e.to, &e.from.id)? {
                let names: Vec<String> = std::iter::once(e.from.number).chain(path).map(|n| identifier(key, n)).collect();
                return Err(LclError::BlocksCycle { blocker: e.from.identifier.clone(), blocked: e.to.identifier.clone(), path: names.join(" blocks ") });
            }
        }
        "duplicate" => {
            let target = |id: &str| -> Result<Option<(String, i64)>> {
                Ok(conn
                    .query_row(
                        "SELECT r.related_id, i.number FROM issue_relations r JOIN issues i ON i.id = r.related_id WHERE r.issue_id = ?1 AND r.kind = 'duplicate' ORDER BY i.number LIMIT 1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?)
            };
            if let Some((_, current)) = target(&e.from.id)? {
                return Err(LclError::AlreadyDuplicate { issue: e.from.identifier.clone(), current: identifier(key, current) });
            }
            // Each issue has at most one target, so the chain from `to` is a
            // line; reaching `from` closes a cycle.
            let mut path = vec![e.from.number, e.to.number];
            let mut seen = HashSet::from([e.to.id.clone()]);
            let mut next = target(&e.to.id)?;
            while let Some((id, number)) = next {
                path.push(number);
                if id == e.from.id {
                    let path: Vec<String> = path.into_iter().map(|n| identifier(key, n)).collect();
                    return Err(LclError::DuplicateCycle { issue: e.from.identifier.clone(), target: e.to.identifier.clone(), path: path.join(" -> ") });
                }
                // A loop that doesn't pass through `from` (only a hand edit
                // could make one) ends the walk.
                if !seen.insert(id.clone()) {
                    break;
                }
                next = target(&id)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The shortest chain of `blocks` rows from `start` to the issue `goal`, as
/// issue numbers from start to goal, or None if `start` doesn't reach it.
fn blocks_path(conn: &Connection, start: &Issue, goal: &str) -> Result<Option<Vec<i64>>> {
    let mut stmt = conn.prepare("SELECT r.related_id, i.number FROM issue_relations r JOIN issues i ON i.id = r.related_id WHERE r.issue_id = ?1 AND r.kind = 'blocks'")?;
    // Each issue reached, with the issue it was reached from and its number.
    let mut came_from: HashMap<String, (Option<String>, i64)> = HashMap::from([(start.id.clone(), (None, start.number))]);
    let mut queue = VecDeque::from([start.id.clone()]);
    while let Some(id) = queue.pop_front() {
        let blocked = stmt.query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        for (next, number) in blocked {
            if came_from.contains_key(&next) {
                continue;
            }
            came_from.insert(next.clone(), (Some(id.clone()), number));
            if next == goal {
                let mut path = vec![];
                let mut at = Some(next);
                while let Some(id) = at {
                    let (prev, number) = came_from[&id].clone();
                    path.push(number);
                    at = prev;
                }
                path.reverse();
                return Ok(Some(path));
            }
            queue.push_back(next);
        }
    }
    Ok(None)
}

const PROJECT_SELECT: &str = "SELECT id, name, description, status, target_date, created_at, updated_at FROM projects";

fn row_to_project(r: &rusqlite::Row) -> Result<Project> {
    let status: String = r.get(3)?;
    // The schema's CHECK admits only the canonical six.
    let status = ProjectStatus::parse(&status).ok_or_else(|| LclError::Msg(format!("project status {status:?} is unknown")))?;
    Ok(Project { id: r.get(0)?, name: r.get(1)?, description: r.get(2)?, status, target_date: r.get(4)?, created_at: r.get(5)?, updated_at: r.get(6)? })
}

/// A milestone with its project's name: `milestones m` joined to `projects p`.
const MILESTONE_SELECT: &str = "SELECT m.id, m.name, p.name, m.project_id, m.description, m.target_date, m.sort_order, m.created_at, m.updated_at FROM milestones m JOIN projects p ON p.id = m.project_id";

fn row_to_milestone(r: &rusqlite::Row) -> Result<Milestone> {
    Ok(Milestone {
        id: r.get(0)?,
        name: r.get(1)?,
        project: r.get(2)?,
        project_id: r.get(3)?,
        description: r.get(4)?,
        target_date: r.get(5)?,
        sort_order: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn project_by_id(conn: &Connection, id: &str) -> Result<Project> {
    conn.query_row(&format!("{PROJECT_SELECT} WHERE id = ?1"), params![id], |r| Ok(row_to_project(r)))
        .optional()?
        .ok_or_else(|| LclError::Msg(format!("project {id} vanished inside its own transaction")))?
}

fn milestone_by_id(conn: &Connection, id: &str) -> Result<Milestone> {
    conn.query_row(&format!("{MILESTONE_SELECT} WHERE m.id = ?1"), params![id], |r| Ok(row_to_milestone(r)))
        .optional()?
        .ok_or_else(|| LclError::Msg(format!("milestone {id} vanished inside its own transaction")))?
}

/// The project named `name` ignoring ASCII case, other than `except`.
fn project_named(conn: &Connection, name: &str, except: Option<&str>) -> Result<Option<Project>> {
    conn.query_row(&format!("{PROJECT_SELECT} WHERE name = ?1 COLLATE NOCASE AND id IS NOT ?2"), params![name, except], |r| Ok(row_to_project(r)))
        .optional()?
        .transpose()
}

/// The milestone of `project` named `name` ignoring ASCII case, other than
/// `except`.
fn milestone_named(conn: &Connection, project: &Project, name: &str, except: Option<&str>) -> Result<Option<Milestone>> {
    conn.query_row(
        &format!("{MILESTONE_SELECT} WHERE m.project_id = ?1 AND m.name = ?2 COLLATE NOCASE AND m.id IS NOT ?3"),
        params![project.id, name, except],
        |r| Ok(row_to_milestone(r)),
    )
    .optional()?
    .transpose()
}

/// The project `r` names: its id, or its name ignoring ASCII case.
fn resolve_project(conn: &Connection, r: &str) -> Result<Project> {
    conn.query_row(
        &format!("{PROJECT_SELECT} WHERE id = ?1 OR name = ?1 COLLATE NOCASE ORDER BY id = ?1 DESC LIMIT 1"),
        params![r],
        |row| Ok(row_to_project(row)),
    )
    .optional()?
    .transpose()?
    .ok_or_else(|| LclError::ProjectNotFound { name: r.to_string(), workspace: false })
}

/// Every milestone `r` could name (its id, or its name ignoring ASCII
/// case), in any project, by project name.
fn milestones_matching(conn: &Connection, r: &str) -> Result<Vec<Milestone>> {
    let mut stmt = conn.prepare(&format!(
        "{MILESTONE_SELECT} WHERE m.id = ?1 OR m.name = ?1 COLLATE NOCASE ORDER BY m.id = ?1 DESC, p.name COLLATE NOCASE, p.name"
    ))?;
    let rows = stmt.query_and_then(params![r], row_to_milestone)?;
    rows.collect()
}

/// The milestone `r` names, for milestone commands and `issue list
/// --milestone`: in `project` when given; else in the whole workspace,
/// where an id wins and a name more than one project has is ambiguous.
fn resolve_milestone(conn: &Connection, r: &str, project: Option<&Project>) -> Result<Milestone> {
    let all = milestones_matching(conn, r)?;
    if let Some(project) = project {
        return all
            .into_iter()
            .find(|m| m.project_id == project.id)
            .ok_or_else(|| LclError::MilestoneNotFound { name: r.to_string(), project: Some(project.name.clone()) });
    }
    if let Some(m) = all.iter().find(|m| m.id == r) {
        return Ok(m.clone());
    }
    match all.as_slice() {
        [] => Err(LclError::MilestoneNotFound { name: r.to_string(), project: None }),
        [one] => Ok(one.clone()),
        several => Err(LclError::MilestoneAmbiguous { name: r.to_string(), projects: several.iter().map(|m| m.project.clone()).collect() }),
    }
}

/// The milestone `r` names for an issue in `project`. One that exists only
/// in other projects is a usage error naming them; one that exists nowhere
/// is not found.
fn milestone_for_issue(conn: &Connection, r: &str, project: &Project) -> Result<Milestone> {
    let all = milestones_matching(conn, r)?;
    if let Some(m) = all.iter().find(|m| m.project_id == project.id) {
        return Ok(m.clone());
    }
    if all.is_empty() {
        return Err(LclError::MilestoneNotFound { name: r.to_string(), project: Some(project.name.clone()) });
    }
    let mut elsewhere: Vec<String> = all.into_iter().map(|m| m.project).collect();
    elsewhere.dedup();
    Err(LclError::MilestoneElsewhere { name: r.to_string(), project: project.name.clone(), elsewhere })
}

/// Sets `issue`'s project and milestone from `p`, inside the caller's write
/// transaction:
/// - the project is the one given, none, or (Keep) the one it has;
/// - a milestone given must be in that project;
/// - a milestone kept that isn't in it (the issue changed project, or left
///   all projects) is cleared and returned.
fn place(conn: &Connection, issue: &mut Issue, p: &Placement) -> Result<Option<ClearedMilestone>> {
    let project = match &p.project {
        Change::Keep => issue.project_id.as_deref().map(|id| project_by_id(conn, id)).transpose()?,
        Change::Clear => None,
        Change::Set(r) => Some(resolve_project(conn, r)?),
    };
    let mut cleared = None;
    let milestone_id = match &p.milestone {
        Change::Clear => None,
        Change::Set(r) => {
            let Some(project) = &project else { return Err(LclError::MilestoneWithoutProject(r.clone())) };
            Some(milestone_for_issue(conn, r, project)?.id)
        }
        Change::Keep => match issue.milestone_id.as_deref() {
            None => None,
            Some(id) => {
                let m = milestone_by_id(conn, id)?;
                if project.as_ref().is_some_and(|p| p.id == m.project_id) {
                    Some(m.id)
                } else {
                    cleared = Some(ClearedMilestone { milestone: m.name, project: m.project });
                    None
                }
            }
        },
    };
    issue.project_id = project.map(|p| p.id);
    issue.milestone_id = milestone_id;
    Ok(cleared)
}

/// How many issues have `col` (project_id or milestone_id) = `id`, by
/// status. lcl_status() reads each stored status as every read does; what
/// it can't read is counted as unreadable.
fn issue_counts(conn: &Connection, col: &str, id: &str) -> Result<IssueCounts> {
    let mut stmt = conn.prepare(&format!("SELECT lcl_status(status), count(*) FROM issues WHERE {col} = ?1 GROUP BY 1"))?;
    let mut counts = IssueCounts::default();
    for row in stmt.query_map(params![id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)))? {
        let (status, n) = row?;
        // lcl_status answers with a canonical spelling, or NULL.
        let status = status.and_then(|s| Status::all().iter().copied().find(|st| st.as_str() == s));
        counts.by_status.add(status, n);
        counts.total += n;
    }
    Ok(counts)
}

/// The issue as just written, read back inside the same transaction so the
/// caller gets the derived fields (identifiers) too.
fn reread(conn: &Connection, key: &str, id: &str) -> Result<Issue> {
    fetch_issue_by(conn, key, "id", &id)?.ok_or_else(|| LclError::Msg(format!("issue {id} vanished inside its own transaction")))
}

/// An issue row as `row_to_issue` reads it: `issues i`, with its parent's
/// number from `issues p` and its project's and milestone's names. Callers append WHERE, ORDER BY and LIMIT.
const ISSUE_SELECT: &str = "SELECT i.id,i.number,i.title,i.description,i.status,i.priority,i.assignee,i.parent_id,i.estimate,i.due_date,i.created_at,i.updated_at,i.completed_at,i.started_at,i.canceled_at,p.number,i.project_id,pr.name,i.milestone_id,ms.name FROM issues i LEFT JOIN issues p ON p.id = i.parent_id LEFT JOIN projects pr ON pr.id = i.project_id LEFT JOIN milestones ms ON ms.id = i.milestone_id";

/// The issues with these UUIDs, in this order, skipping any that are gone:
/// one query for the rows and one for all their labels.
fn fetch_issues(conn: &Connection, ids: &[String]) -> Result<Vec<Issue>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let key = read_key(conn)?;
    let mut stmt = conn.prepare(&format!("{ISSUE_SELECT} WHERE i.id IN (SELECT value FROM json_each(?1))"))?;
    let mut rows = stmt.query(params![json_array(ids)?])?;
    let mut by_id: HashMap<String, Issue> = HashMap::with_capacity(ids.len());
    while let Some(r) = rows.next()? {
        let issue = row_to_issue(r, &key)?;
        by_id.insert(issue.id.clone(), issue);
    }
    let mut out: Vec<Issue> = ids.iter().filter_map(|id| by_id.remove(id)).collect();
    attach_labels(conn, &mut out)?;
    Ok(out)
}

/// The issue whose `col` (`id` or `number`) equals `val`, with its labels and
/// its identifiers under workspace key `key`.
fn fetch_issue_by(conn: &Connection, key: &str, col: &str, val: &dyn ToSql) -> Result<Option<Issue>> {
    let mut stmt = conn.prepare(&format!("{ISSUE_SELECT} WHERE i.{col}=?1"))?;
    let mut rows = stmt.query(params![val])?;
    let Some(r) = rows.next()? else { return Ok(None) };
    let mut issue = [row_to_issue(r, key)?];
    attach_labels(conn, &mut issue)?;
    let [issue] = issue;
    Ok(Some(issue))
}

/// Fills in every issue's label names, sorted ignoring case, with one query
/// however many issues there are.
fn attach_labels(conn: &Connection, issues: &mut [Issue]) -> Result<()> {
    if issues.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    let mut stmt = conn.prepare(
        "SELECT il.issue_id, l.name FROM issue_labels il JOIN labels l ON l.id = il.label_id
         WHERE il.issue_id IN (SELECT value FROM json_each(?1)) ORDER BY l.name COLLATE NOCASE, l.name",
    )?;
    let mut by_issue: HashMap<String, Vec<String>> = HashMap::new();
    for row in stmt.query_map(params![json_array(&ids)?], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (id, name) = row?;
        by_issue.entry(id).or_default().push(name);
    }
    for issue in issues {
        issue.labels = by_issue.get(&issue.id).cloned().unwrap_or_default();
    }
    Ok(())
}

/// `items` as a JSON array, for `json_each(?)`: one parameter however many
/// items, where `IN (?, ?, …)` would hit SQLite's parameter limit.
fn json_array<T: Serialize>(items: &[T]) -> Result<String> {
    serde_json::to_string(items).map_err(|e| LclError::Msg(format!("can't encode ids: {e}")))
}

/// Registers `lcl_status(text)`: the stored status read by `Status::parse`,
/// the rule every read uses, as its canonical spelling, or NULL for a value
/// that names no status (that matches no status filter, and reading the
/// issue reports it). `issue list --status` filters with it in SQL, so a
/// hand-edited "done" is Done there as it is in `issue show`.
fn register_lcl_status(conn: &Connection) -> Result<()> {
    conn.create_scalar_function("lcl_status", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        Ok(match ctx.get_raw(0) {
            ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(Status::parse).map(|s| s.as_str()),
            _ => None,
        })
    })?;
    Ok(())
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
        project_id: r.get(16)?,
        project: r.get(17)?,
        milestone_id: r.get(18)?,
        milestone: r.get(19)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A write that lands between `list_issues`' count and its page doesn't
    /// split them: both read the snapshot their read transaction started
    /// with. The write comes from a second connection, made from inside the
    /// status filter while the count is running.
    #[test]
    fn a_list_reads_one_snapshot() {
        let path = std::env::temp_dir().join(format!("lcl-snapshot-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(&path).expect("open");
        let insert = |c: &Connection, n: i64| {
            c.execute(
                "INSERT INTO issues(id, number, title, description, status, priority, created_at, updated_at) VALUES (?1, ?2, 't', '', 'Done', 0, 1, 1)",
                params![format!("issue-{n}"), n],
            )
        };
        for n in 1..=3 {
            insert(&store.conn, n).expect("seed");
        }
        let writer = Connection::open(&path).expect("second connection");
        let mut wrote = false;
        store
            .conn
            .create_scalar_function("lcl_status", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
                if !wrote {
                    wrote = true;
                    insert(&writer, 4)?;
                }
                ctx.get::<String>(0)
            })
            .expect("replace lcl_status");

        let filter = IssueFilter { statuses: vec![Status::Done], ..Default::default() };
        let list = store.list_issues(&filter, IssueSort::Number, Page::ALL).expect("list");
        assert_eq!((list.total, list.issues.len()), (3, 3), "the count and the page read different snapshots");
        let after = store.list_issues(&filter, IssueSort::Number, Page::ALL).expect("list again");
        assert_eq!((after.total, after.issues.len()), (4, 4), "the concurrent write did land");

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
