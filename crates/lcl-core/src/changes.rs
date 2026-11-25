//! The change log (docs/sync.md): its clock, the bookkeeping each write does
//! around it, and an issue's history read from it.
//!
//! Triggers write the rows (see migration v14 in `crate::migrations`), so
//! every write to a record is logged in its own transaction. What they need
//! from lcl is the write's clock and author, which `begin` puts in `meta`
//! for them and `finish` takes back out.

use crate::db::{put_meta, read_key, read_meta, resolve_issue_or_trashed};
use crate::error::Result;
use crate::models::{IssueRef, Priority, identifier};
use crate::store::Store;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// The machine part of a clock until sync gives this machine an id of its
/// own (LOCL-30). A copied file would copy any id kept in it, so none is.
pub const LOCAL_MACHINE: &str = "local";

/// The largest counter a clock carries; one more moves to the next
/// millisecond instead.
const MAX_COUNTER: i64 = 9999;

/// Milliseconds since the Unix epoch, by this machine's clock.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The clock that follows `prev`, at wall time `now_ms`: a hybrid logical
/// clock, `MS-COUNTER-MACHINE` (`1795523737123-0000-local`), the
/// milliseconds 13 digits and the counter 4, so that comparing two clocks
/// as text orders them. It never goes back: when the wall clock is not past
/// `prev`, the counter goes up instead, and past `MAX_COUNTER` the
/// milliseconds do.
pub fn next_clock(prev: Option<&str>, now_ms: i64, machine: &str) -> String {
    let (ms, counter) = prev.and_then(parse_clock).unwrap_or((0, 0));
    let (ms, counter) = if now_ms > ms {
        (now_ms, 0)
    } else if counter < MAX_COUNTER {
        (ms, counter + 1)
    } else {
        (ms + 1, 0)
    };
    format!("{ms:013}-{counter:04}-{machine}")
}

/// A clock's milliseconds and counter.
pub(crate) fn parse_clock(clock: &str) -> Option<(i64, i64)> {
    let mut parts = clock.splitn(3, '-');
    let ms = parts.next()?.parse().ok()?;
    let counter = parts.next()?.parse().ok()?;
    Some((ms, counter))
}

/// The epoch seconds a clock was read at.
pub(crate) fn clock_seconds(clock: &str) -> i64 {
    parse_clock(clock).map_or(0, |(ms, _)| ms.div_euclid(1000))
}

/// What `finish` needs from `begin`.
pub(crate) struct Pending {
    clock_before: Option<String>,
    last_seq: i64,
}

/// Starts a write's log: the next clock and the author go in `meta`, where
/// the triggers read them. Runs first in every write transaction.
pub(crate) fn begin(conn: &Connection, author: Option<&str>) -> Result<Pending> {
    let clock_before = read_meta(conn, "clock")?;
    let machine = crate::sync::machine(conn)?;
    put_meta(
        conn,
        "clock",
        &next_clock(clock_before.as_deref(), now_ms(), &machine),
    )?;
    match author {
        Some(author) => put_meta(conn, "author", author)?,
        None => {
            conn.execute("DELETE FROM meta WHERE key = 'author'", [])?;
        }
    }
    Ok(Pending {
        clock_before,
        last_seq: last_seq(conn)?,
    })
}

/// Ends a write's log, last before the commit. The author leaves `meta`.
/// A write that logged nothing (it changed nothing) puts the clock back as
/// well, so it leaves the file as it found it. `seq` is AUTOINCREMENT, so
/// a write that logged anything leaves a larger one, even after a purge.
pub(crate) fn finish(conn: &Connection, pending: Pending) -> Result<()> {
    conn.execute("DELETE FROM meta WHERE key = 'author'", [])?;
    if last_seq(conn)? <= pending.last_seq {
        match pending.clock_before {
            Some(clock) => put_meta(conn, "clock", &clock)?,
            None => {
                conn.execute("DELETE FROM meta WHERE key = 'clock'", [])?;
            }
        }
    }
    Ok(())
}

pub(crate) fn last_seq(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM changes", [], |r| {
            r.get(0)
        })?,
    )
}

/// One write's changes to an issue, as `lcl issue history` lists them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEntry {
    /// When, in epoch seconds.
    pub at: i64,
    pub author: Option<String>,
    pub changes: Vec<FieldChange>,
}

/// One field's change. Values are as `Issue` shows them: `priority` by
/// name, `parent` as `KEY-N`, `project`, `milestone`, `labels` and `cycles`
/// by name, a relation by the other issue's `KEY-N`. For a set (`labels`,
/// `cycles`, the relations) each change is one member: added when `old` is
/// null, removed when `new` is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub field: String,
    /// The comment changed, for a change to one of the issue's comments
    /// (`body` or `deleted_at`); else None.
    pub comment_id: Option<String>,
    pub old: Value,
    pub new: Value,
}

/// The issue's fields in the order a creation lists them. `number` is not
/// among them: it never changes, and the identifier names it.
const ISSUE_FIELDS: &[&str] = &[
    "title",
    "description",
    "status",
    "priority",
    "assignee",
    "parent_id",
    "project_id",
    "milestone_id",
    "ref",
    "creator",
    "estimate",
    "due_date",
    "created_at",
    "started_at",
    "completed_at",
    "canceled_at",
    "deleted_at",
];

/// A relation's kind as the issue named first in it sees it, and as the
/// other one does.
const RELATIONS: &[(&str, &str, &str)] = &[
    ("blocks", "blocks", "blocked_by"),
    ("related", "related", "related"),
    ("duplicate", "duplicate_of", "duplicates"),
];

struct Row {
    clock: String,
    kind: String,
    record: String,
    field: String,
    old: Option<String>,
    new: Option<String>,
    author: Option<String>,
}

impl Store {
    /// Everything recorded about one issue (live or in the trash), oldest
    /// first, one entry per write: its fields, labels, cycles and relations,
    /// and its comments. The first entry sets every field that isn't null,
    /// from null: the issue's creation, or for an issue made before lcl
    /// kept history, or brought in by `lcl import`, its fields as they were
    /// when lcl started to record it (its `created_at` says when it was
    /// made). A write made without lcl (a hand edit) is an entry of its
    /// own, at the clock of lcl's last write, with no author. A purge leaves
    /// no history: a purged comment's changes go with it, and so do a purged
    /// label's and a deleted cycle's memberships.
    pub fn issue_history(&self, r: &IssueRef) -> Result<Vec<HistoryEntry>> {
        self.read(|conn| {
            let issue = resolve_issue_or_trashed(conn, r)?;
            let rows = history_rows(conn, issue.id.as_str())?;
            let mut names = Names::new(conn)?;
            let mut out: Vec<HistoryEntry> = Vec::new();
            // An entry is one clock and one author. A write made without
            // lcl (a hand edit) is logged with the clock of lcl's last write
            // but no author, so it is an entry of its own, not part of
            // that write's.
            let mut last: Option<(String, Option<String>)> = None;
            for row in rows {
                let changes = names.field_changes(issue.id.as_str(), &row)?;
                if changes.is_empty() {
                    continue;
                }
                let same_write = last
                    .as_ref()
                    .is_some_and(|(clock, author)| *clock == row.clock && *author == row.author);
                match out.last_mut() {
                    Some(entry) if same_write => entry.changes.extend(changes),
                    _ => out.push(HistoryEntry {
                        at: clock_seconds(&row.clock),
                        author: row.author.clone(),
                        changes,
                    }),
                }
                last = Some((row.clock, row.author));
            }
            Ok(out)
        })
    }
}

/// The rows about issue `id`: its own, the relations that name it from
/// another issue, and its comments'. In clock order; a write's rows share
/// its clock, in the order the write made them.
fn history_rows(conn: &Connection, id: &str) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT clock, kind, record, field, old, new, author FROM changes
         WHERE record = ?1
            OR field IN ('blocks:' || ?1, 'related:' || ?1, 'duplicate:' || ?1)
            OR (kind = 'comment' AND record IN (SELECT id FROM comments WHERE issue_id = ?1))
         ORDER BY clock, seq",
    )?;
    let rows = stmt
        .query_map(params![id], |r| {
            Ok(Row {
                clock: r.get(0)?,
                kind: r.get(1)?,
                record: r.get(2)?,
                field: r.get(3)?,
                old: r.get(4)?,
                new: r.get(5)?,
                author: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Turns stored values into the names people know, remembering each one.
struct Names<'c> {
    conn: &'c Connection,
    key: String,
    seen: HashMap<(&'static str, String), Value>,
}

impl<'c> Names<'c> {
    fn new(conn: &'c Connection) -> Result<Self> {
        Ok(Names {
            conn,
            key: read_key(conn)?,
            seen: HashMap::new(),
        })
    }

    /// What one row says about issue `id`, as field changes.
    fn field_changes(&mut self, id: &str, row: &Row) -> Result<Vec<FieldChange>> {
        let old = parse(row.old.as_deref());
        let new = parse(row.new.as_deref());
        if row.kind == "comment" {
            let (field, old, new) = if row.field == "*" {
                ("body".to_string(), Value::Null, new["body"].clone())
            } else {
                (row.field.clone(), old, new)
            };
            if row.field == "*" && row.new.is_none() {
                return Ok(Vec::new()); // a purge: the comment is gone
            }
            return Ok(vec![FieldChange {
                field,
                comment_id: Some(row.record.clone()),
                old,
                new,
            }]);
        }
        if row.kind != "issue" {
            return Ok(Vec::new());
        }
        let change = |field: &str, old: Value, new: Value| FieldChange {
            field: field.to_string(),
            comment_id: None,
            old,
            new,
        };
        if row.record != id {
            // A relation another issue holds, naming this one.
            let (kind, _) = row.field.split_once(':').unwrap_or_default();
            let Some((_, _, theirs)) = RELATIONS.iter().find(|(k, _, _)| *k == kind) else {
                return Ok(Vec::new());
            };
            let other = self.issue(&row.record)?;
            let member = |v: &Value| {
                if v.is_null() {
                    Value::Null
                } else {
                    other.clone()
                }
            };
            return Ok(vec![change(theirs, member(&old), member(&new))]);
        }
        if row.field == "*" {
            if row.new.is_none() {
                return Ok(Vec::new());
            }
            let mut out = Vec::new();
            for f in ISSUE_FIELDS {
                let v = new.get(*f).cloned().unwrap_or(Value::Null);
                if !v.is_null() {
                    let (name, v) = self.issue_value(f, v)?;
                    out.push(change(name, Value::Null, v));
                }
            }
            return Ok(out);
        }
        if let Some((kind, other)) = row.field.split_once(':') {
            let (name, shown) = match kind {
                "label" => ("labels", self.named("labels", other)?),
                "cycle" => ("cycles", self.named("cycles", other)?),
                kind => match RELATIONS.iter().find(|(k, _, _)| *k == kind) {
                    Some((_, ours, _)) => (*ours, self.issue(other)?),
                    None => return Ok(Vec::new()),
                },
            };
            let member = |v: &Value| {
                if v.is_null() {
                    Value::Null
                } else {
                    shown.clone()
                }
            };
            return Ok(vec![change(name, member(&old), member(&new))]);
        }
        if !ISSUE_FIELDS.contains(&row.field.as_str()) {
            return Ok(Vec::new());
        }
        let (name, old) = self.issue_value(&row.field, old)?;
        let (_, new) = self.issue_value(&row.field, new)?;
        Ok(vec![change(name, old, new)])
    }

    /// An issue column's name and value as `Issue` shows them.
    fn issue_value(&mut self, field: &str, v: Value) -> Result<(&'static str, Value)> {
        let field: &'static str = ISSUE_FIELDS
            .iter()
            .find(|f| **f == field)
            .copied()
            .unwrap_or("?");
        if v.is_null() {
            let name = match field {
                "parent_id" => "parent",
                "project_id" => "project",
                "milestone_id" => "milestone",
                f => f,
            };
            return Ok((name, v));
        }
        Ok(match field {
            "priority" => (
                "priority",
                match v.as_i64().and_then(Priority::from_stored) {
                    Some(p) => Value::from(p.label()),
                    None => v,
                },
            ),
            "parent_id" => ("parent", self.issue(v.as_str().unwrap_or_default())?),
            "project_id" => (
                "project",
                self.named("projects", v.as_str().unwrap_or_default())?,
            ),
            "milestone_id" => (
                "milestone",
                self.named("milestones", v.as_str().unwrap_or_default())?,
            ),
            f => (f, v),
        })
    }

    /// An issue's `KEY-N`, from the issues there are or the purged ones;
    /// its UUID if neither has it.
    fn issue(&mut self, id: &str) -> Result<Value> {
        let k = ("issue", id.to_string());
        if let Some(v) = self.seen.get(&k) {
            return Ok(v.clone());
        }
        let number: Option<i64> = self
            .conn
            .query_row(
                "SELECT number FROM issues WHERE id = ?1
                 UNION ALL SELECT number FROM purged WHERE record = ?1 AND number IS NOT NULL
                 LIMIT 1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let v = Value::from(match number {
            Some(n) => identifier(&self.key, n),
            None => id.to_string(),
        });
        self.seen.insert(k, v.clone());
        Ok(v)
    }

    /// The name of the label, cycle, project or milestone `id`; its UUID
    /// once it is gone.
    fn named(&mut self, table: &'static str, id: &str) -> Result<Value> {
        let k = (table, id.to_string());
        if let Some(v) = self.seen.get(&k) {
            return Ok(v.clone());
        }
        let name: Option<String> = self
            .conn
            .query_row(
                &format!("SELECT name FROM {table} WHERE id = ?1"),
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let v = Value::from(name.unwrap_or_else(|| id.to_string()));
        self.seen.insert(k, v.clone());
        Ok(v)
    }
}

/// A stored JSON value; null for SQL NULL, or for text that isn't JSON (a
/// hand edit's).
fn parse(text: Option<&str>) -> Value {
    text.and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_follows_the_wall_clock_and_never_goes_back() {
        assert_eq!(
            next_clock(None, 1_795_523_737_123, "local"),
            "1795523737123-0000-local"
        );
        let a = next_clock(None, 5, "local");
        assert_eq!(a, "0000000000005-0000-local");
        // The wall clock moves on: the counter starts again.
        assert_eq!(next_clock(Some(&a), 6, "local"), "0000000000006-0000-local");
        // It stands still or goes back: the counter goes up.
        let b = next_clock(Some(&a), 5, "local");
        assert_eq!(b, "0000000000005-0001-local");
        assert_eq!(next_clock(Some(&b), 1, "local"), "0000000000005-0002-local");
        // Past the counter's last value, the milliseconds go up.
        assert_eq!(
            next_clock(Some("0000000000005-9999-local"), 1, "local"),
            "0000000000006-0000-local"
        );
        // Text order is clock order.
        let mut clocks = vec![a.clone(), b.clone()];
        clocks.push(next_clock(Some(&b), 5, "local"));
        clocks.push(next_clock(Some(&b), 1_000_000, "local"));
        let mut sorted = clocks.clone();
        sorted.sort();
        assert_eq!(sorted, clocks);
        assert_eq!(clock_seconds("1795523737123-0000-local"), 1_795_523_737);
    }
}
