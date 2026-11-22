//! The trash: `issue delete` is not permanent, `issue restore` undoes it,
//! and `issue purge` is the one way an issue leaves the workspace for good.
//!
//! # States
//!
//! An issue that exists is in one of two states, told apart by one column:
//!
//! - **Live**: `deleted_at` is NULL.
//! - **Trashed**: `deleted_at` is the time `issue delete` ran.
//!
//! A **purged** issue has no row.
//!
//! ```text
//! Live --issue delete--> Trashed --issue restore--> Live
//!                           |
//!                           +--issue purge--> Purged
//! ```
//!
//! Nothing else writes `deleted_at`, and nothing else removes an issue row.
//!
//! # Invariants
//!
//! - **One field.** Delete and restore change `deleted_at` and nothing
//!   else: not `updated_at`, not a comment, a label link, a cycle
//!   membership or a relation, and not the search index, whose trigger
//!   fires only when a title or description is written (migration 12).
//!   Delete then restore leaves every table as it was, row for row, and so
//!   gives back the issue to the byte of `issue show --json`.
//! - **Frozen.** While an issue is trashed no command reads or changes it
//!   or its comments; `issue restore` and `issue purge` are the two that
//!   take one. Every other command reaches an issue through
//!   `db::resolve_issue`, which refuses a trashed one and names `issue
//!   restore`; `comment edit` and `comment delete` refuse a trashed issue's
//!   comment the same way. Deleting a project, a milestone or a cycle is
//!   refused while any issue uses it, trashed ones included, so a restored
//!   issue finds them still there. (Deleting a label removes it from every
//!   issue, trashed ones included: that is what deleting a label means.)
//! - **Out of sight.** A trashed issue is in no list, search result, count,
//!   child list or relation list, and it does not block: a query that
//!   returns issues, counts them or follows a relation to one has
//!   `deleted_at IS NULL` on it. `issue list --trashed` is the one list
//!   that shows trashed issues, and shows only them. A relation to a
//!   trashed issue stays stored, hidden from the other issue until restore.
//! - **A live issue's parent is live.** Delete is refused while the issue
//!   has live sub-issues; restore is refused while the issue's parent is
//!   trashed; and no issue can be put under a trashed one, which is frozen.
//! - **Still taken.** A trashed issue keeps its number and its ref: the
//!   number is never handed out again, and no other issue can take the ref.
//! - **Not exported.** `lcl export` leaves out trashed issues, their
//!   comments, and relations with a trashed end, so `lcl-export/1` is
//!   unchanged and an imported workspace starts with an empty trash. By the
//!   parent rule, no exported issue names a parent that was left out.

//!
//! # Purging
//!
//! - **Only from the trash.** There is no step from Live to Purged: a purge
//!   of a live issue is refused and names `issue delete`. Two commands, at
//!   least, stand between a live issue and its loss.
//! - **Whole.** One transaction removes the issue's row, its comments, its
//!   relations in both directions, its label links and its cycle links, or
//!   none of them. The search indexes follow through their triggers. No
//!   other issue's row changes.
//! - **Sub-issues first.** An issue's sub-issues are in the trash with it
//!   (a live issue's parent is live). One issue is not purged while it has
//!   any: they would be left under nothing. Emptying the trash purges them
//!   all together.
//! - **The number is spent.** The counter is never lowered, so a purged
//!   issue's number is not handed out again. Its ref is free again: no
//!   issue has it.
//! - **Only when asked.** Nothing purges by itself: no expiry, no limit on
//!   the trash, no clock. `purge_issue` and `purge_trash` are the only
//!   callers of the deletion, and only the two commands call them.

use crate::db::{Store, read_key, reread, resolve_issue, resolve_issue_or_trashed};
use crate::error::{LclError, Result};
use crate::models::{Issue, IssueRef, identifier};
use rusqlite::{Connection, OptionalExtension, params};

/// Fails with `InTrash` if the issue with UUID `issue_id` is trashed. For
/// the commands that reach an issue by something other than a reference to
/// it, as a comment's id.
pub(crate) fn refuse_trashed(conn: &Connection, issue_id: &str) -> Result<()> {
    let trashed: Option<i64> = conn
        .query_row(
            "SELECT number FROM issues WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![issue_id],
            |r| r.get(0),
        )
        .optional()?;
    match trashed {
        Some(number) => Err(LclError::InTrash(identifier(&read_key(conn)?, number))),
        None => Ok(()),
    }
}

/// What a purge removed.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Purged {
    /// The `KEY-N` of each issue removed, lowest number first.
    pub purged: Vec<String>,
    pub issues: i64,
    pub comments: i64,
    pub relations: i64,
}

/// Removes the trashed issues with these UUIDs and everything that hangs
/// on them, and counts it. Must run inside the write transaction that
/// chose them, with foreign keys deferred when a parent and its
/// sub-issues go together.
fn purge(tx: &Connection, key: &str, issues: &[(String, i64)]) -> Result<Purged> {
    let mut out = Purged::default();
    for (id, number) in issues {
        // Trashed, still: the only rows this may delete.
        let trashed: bool = tx.query_row(
            "SELECT deleted_at IS NOT NULL FROM issues WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if !trashed {
            return Err(LclError::NotInTrash(identifier(key, *number)));
        }
        out.comments += tx.execute("DELETE FROM comments WHERE issue_id = ?1", params![id])? as i64;
        out.relations += tx.execute(
            "DELETE FROM issue_relations WHERE issue_id = ?1 OR related_id = ?1",
            params![id],
        )? as i64;
        tx.execute("DELETE FROM issue_labels WHERE issue_id = ?1", params![id])?;
        tx.execute("DELETE FROM issue_cycles WHERE issue_id = ?1", params![id])?;
        out.issues += tx.execute(
            "DELETE FROM issues WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![id],
        )? as i64;
        out.purged.push(identifier(key, *number));
    }
    Ok(out)
}

impl Store {
    /// Removes one trashed issue for good, with its comments, relations,
    /// label links and cycle links. Refused for a live issue, which must
    /// be deleted first, and while the issue has sub-issues (in the trash,
    /// as they must be), which must be purged first.
    pub fn purge_issue(&mut self, r: &IssueRef) -> Result<Purged> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let issue = resolve_issue_or_trashed(tx, r)?;
            if issue.deleted_at.is_none() {
                return Err(LclError::PurgeLive(issue.identifier));
            }
            let mut stmt =
                tx.prepare("SELECT number FROM issues WHERE parent_id = ?1 ORDER BY number")?;
            let children = stmt
                .query_map(params![issue.id], |r| r.get(0))?
                .collect::<std::result::Result<Vec<i64>, _>>()?;
            if !children.is_empty() {
                let children: Vec<String> =
                    children.into_iter().map(|n| identifier(&key, n)).collect();
                return Err(LclError::PurgeHasChildren {
                    issue: issue.identifier,
                    children: children.join(", "),
                });
            }
            purge(tx, &key, &[(issue.id.as_str().to_string(), issue.number)])
        })
    }

    /// Empties the trash: removes every trashed issue for good, as
    /// `purge_issue` removes one. Parents and their sub-issues go together.
    pub fn purge_trash(&mut self) -> Result<Purged> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let mut stmt = tx.prepare(
                "SELECT id, number FROM issues WHERE deleted_at IS NOT NULL ORDER BY number",
            )?;
            let issues = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<Vec<(String, i64)>, _>>()?;
            drop(stmt);
            // A trashed parent may go before its trashed sub-issue; the
            // check runs at commit, when both are gone.
            tx.pragma_update(None, "defer_foreign_keys", true)?;
            purge(tx, &key, &issues)
        })
    }

    /// Moves the issue to the trash and returns it as it was. Refused while
    /// it has live sub-issues, which would be left under a trashed parent,
    /// and for an issue already in the trash.
    pub fn delete_issue(&mut self, r: &IssueRef) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let issue = resolve_issue(tx, r)?;
            let mut stmt = tx.prepare(
                "SELECT number FROM issues WHERE parent_id=?1 AND deleted_at IS NULL ORDER BY number",
            )?;
            let children = stmt
                .query_map(params![issue.id], |r| r.get(0))?
                .collect::<std::result::Result<Vec<i64>, _>>()?;
            if !children.is_empty() {
                let children: Vec<String> =
                    children.into_iter().map(|n| identifier(&key, n)).collect();
                return Err(LclError::HasChildren {
                    issue: issue.identifier,
                    children: children.join(", "),
                });
            }
            tx.execute(
                "UPDATE issues SET deleted_at = ?1 WHERE id = ?2",
                params![crate::models::now(), issue.id],
            )?;
            Ok(issue)
        })
    }

    /// Takes the issue out of the trash and returns it, live. Refused for
    /// an issue that is not in the trash, and while its parent is.
    pub fn restore_issue(&mut self, r: &IssueRef) -> Result<Issue> {
        self.write(|tx| {
            let key = read_key(tx)?;
            let issue = resolve_issue_or_trashed(tx, r)?;
            if issue.deleted_at.is_none() {
                return Err(LclError::NotInTrash(issue.identifier));
            }
            if let Some(parent) = &issue.parent_id {
                let trashed_parent: Option<i64> = tx
                    .query_row(
                        "SELECT number FROM issues WHERE id = ?1 AND deleted_at IS NOT NULL",
                        params![parent],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(number) = trashed_parent {
                    return Err(LclError::ParentInTrash {
                        issue: issue.identifier,
                        parent: identifier(&key, number),
                    });
                }
            }
            tx.execute(
                "UPDATE issues SET deleted_at = NULL WHERE id = ?1",
                params![issue.id],
            )?;
            reread(tx, &key, issue.id.as_str())
        })
    }
}
