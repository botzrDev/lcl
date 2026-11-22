//! The trash: `issue delete` is not permanent, `issue restore` undoes it,
//! and `issue purge` is the one way an issue leaves the workspace for good.
//! Docs, comments and labels have the same trash, by the same mechanism;
//! what differs for them is under "Docs, comments and labels" below.
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
//!   issue finds them still there. The exceptions, each a command of its
//!   own that says what it removes: `comment purge` removes a comment
//!   that is itself in the trash, whatever state its issue is in, and
//!   `label purge` removes a trashed label's links to every issue.
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
//!
//! # Docs, comments and labels
//!
//! Each has the same column, the same two states and the same three
//! commands (`delete`, `restore`, `purge`), and the rules above hold for
//! each with the name changed: one field, frozen, out of sight, not
//! exported, purged only from the trash and only when asked. What is
//! particular to each:
//!
//! - **A doc** is to its parent doc as an issue is to its parent issue: a
//!   live doc's parent is live. Delete is refused while it has live docs
//!   under it, restore while its parent is trashed, and one doc is not
//!   purged while docs (trashed, as they must be) are under it.
//! - **A comment** belongs to an issue. While the issue is trashed its
//!   comments are frozen with it, trashed or not. Purging the issue purges
//!   all its comments. A trashed comment is not in `issue show`, `issue
//!   comments` or search; `issue comments --trashed` lists an issue's.
//! - **A label** keeps its name and its links. The name stays taken: a
//!   label of that name can't be created, and the refusal names `label
//!   restore`. Its rows in `issue_labels` stay, and no issue shows the
//!   label. Nothing removes one of those rows while the label is trashed
//!   except purging the issue at its other end: rewriting an issue's labels
//!   replaces only its links to live labels (`db::set_labels`). So `label
//!   restore` puts the label back on every issue that had it. Purging the
//!   label removes the links with it.

use crate::db::{
    COMMENT_COLS, DOC_COLS, LABEL_COLS, Store, comment_row, doc_row, find_comment, label_by_name,
    label_row, read_doc, read_key, reread, resolve_issue, resolve_issue_or_trashed,
};
use crate::error::{LclError, Result};
use crate::models::{Comment, Doc, Issue, IssueRef, Label, identifier, now};
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

// ----- labels -----

/// What a purge of labels removed.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct PurgedLabels {
    /// The name of each label removed, by name.
    pub purged: Vec<String>,
    pub labels: i64,
    /// How many links to issues went with them.
    pub removed_from_issues: i64,
}

/// What a purge of comments removed.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct PurgedComments {
    /// The id of each comment removed, oldest first.
    pub purged: Vec<String>,
    pub comments: i64,
}

/// What a purge of docs removed.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct PurgedDocs {
    /// The id of each doc removed, by id.
    pub purged: Vec<String>,
    pub docs: i64,
}

/// The label with id `name_or_id`, or else named it (ignoring case), live or
/// in the trash.
fn find_label(conn: &Connection, name_or_id: &str) -> Result<Label> {
    let by_id = conn
        .query_row(
            &format!("SELECT {LABEL_COLS} FROM labels WHERE id=?1"),
            params![name_or_id],
            label_row,
        )
        .optional()?;
    match by_id {
        Some(label) => Ok(label),
        None => label_by_name(conn, name_or_id)?
            .ok_or_else(|| LclError::LabelNotFound(name_or_id.to_string())),
    }
}

fn purge_label_rows(tx: &Connection, labels: &[Label]) -> Result<PurgedLabels> {
    let mut out = PurgedLabels::default();
    for label in labels {
        out.removed_from_issues += tx.execute(
            "DELETE FROM issue_labels WHERE label_id = ?1
             AND label_id IN (SELECT id FROM labels WHERE deleted_at IS NOT NULL)",
            params![label.id],
        )? as i64;
        out.labels += tx.execute(
            "DELETE FROM labels WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![label.id],
        )? as i64;
        out.purged.push(label.name.clone());
    }
    Ok(out)
}

impl Store {
    /// Moves the label named (ignoring case) or with id `name_or_id` to the
    /// trash. Its links to issues stay, hidden. Returns the label as it was
    /// and how many live issues showed it.
    pub fn delete_label(&mut self, name_or_id: &str) -> Result<(Label, i64)> {
        self.write(|tx| {
            let label = find_label(tx, name_or_id)?;
            if label.deleted_at.is_some() {
                return Err(LclError::LabelInTrash(label.name));
            }
            let issues: i64 = tx.query_row(
                "SELECT count(*) FROM issue_labels il JOIN issues i ON i.id = il.issue_id
                 WHERE il.label_id = ?1 AND i.deleted_at IS NULL",
                params![label.id],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE labels SET deleted_at = ?1 WHERE id = ?2",
                params![now(), label.id],
            )?;
            Ok((label, issues))
        })
    }

    /// Takes the label out of the trash: it is on every issue it was on.
    /// Returns it, live, and how many live issues show it again.
    pub fn restore_label(&mut self, name_or_id: &str) -> Result<(Label, i64)> {
        self.write(|tx| {
            let mut label = find_label(tx, name_or_id)?;
            if label.deleted_at.is_none() {
                return Err(LclError::NotInTrash(format!("label {:?}", label.name)));
            }
            tx.execute(
                "UPDATE labels SET deleted_at = NULL WHERE id = ?1",
                params![label.id],
            )?;
            label.deleted_at = None;
            let issues: i64 = tx.query_row(
                "SELECT count(*) FROM issue_labels il JOIN issues i ON i.id = il.issue_id
                 WHERE il.label_id = ?1 AND i.deleted_at IS NULL",
                params![label.id],
                |r| r.get(0),
            )?;
            Ok((label, issues))
        })
    }

    /// Removes one trashed label for good, with its links to issues.
    pub fn purge_label(&mut self, name_or_id: &str) -> Result<PurgedLabels> {
        self.write(|tx| {
            let label = find_label(tx, name_or_id)?;
            if label.deleted_at.is_none() {
                return Err(LclError::PurgeLiveItem {
                    what: format!("label {:?}", label.name),
                    delete: "lcl label delete".into(),
                });
            }
            purge_label_rows(tx, &[label])
        })
    }

    /// Removes every trashed label for good.
    pub fn purge_labels(&mut self) -> Result<PurgedLabels> {
        self.write(|tx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {LABEL_COLS} FROM labels WHERE deleted_at IS NOT NULL ORDER BY name COLLATE NOCASE, name"
            ))?;
            let labels = stmt
                .query_map([], label_row)?
                .collect::<std::result::Result<Vec<Label>, _>>()?;
            drop(stmt);
            purge_label_rows(tx, &labels)
        })
    }

    // ----- comments -----

    /// Moves the comment to the trash, leaving the issue's `updated_at`
    /// alone, and returns it as it was with its issue's `KEY-N` (None if
    /// that issue is gone). Refused for a comment of a trashed issue.
    pub fn delete_comment(&mut self, id: &str) -> Result<(Comment, Option<String>)> {
        self.write(|tx| {
            let c = find_comment(tx, id)?;
            if c.deleted_at.is_some() {
                return Err(LclError::CommentInTrash(c.id));
            }
            refuse_trashed(tx, &c.issue_id)?;
            tx.execute(
                "UPDATE comments SET deleted_at = ?1 WHERE id = ?2",
                params![now(), c.id],
            )?;
            let issue = issue_identifier(tx, &c.issue_id)?;
            Ok((c, issue))
        })
    }

    /// Takes the comment out of the trash and returns it, live, with its
    /// issue's `KEY-N`. Refused while its issue is in the trash.
    pub fn restore_comment(&mut self, id: &str) -> Result<(Comment, Option<String>)> {
        self.write(|tx| {
            let mut c = find_comment(tx, id)?;
            if c.deleted_at.is_none() {
                return Err(LclError::NotInTrash(format!("comment {}", c.id)));
            }
            refuse_trashed(tx, &c.issue_id)?;
            tx.execute(
                "UPDATE comments SET deleted_at = NULL WHERE id = ?1",
                params![c.id],
            )?;
            c.deleted_at = None;
            let issue = issue_identifier(tx, &c.issue_id)?;
            Ok((c, issue))
        })
    }

    /// Removes one trashed comment for good.
    pub fn purge_comment(&mut self, id: &str) -> Result<PurgedComments> {
        self.write(|tx| {
            let c = find_comment(tx, id)?;
            if c.deleted_at.is_none() {
                return Err(LclError::PurgeLiveItem {
                    what: format!("comment {}", c.id),
                    delete: "lcl comment delete".into(),
                });
            }
            purge_comment_rows(tx, &[c])
        })
    }

    /// Removes every trashed comment for good, those of trashed issues
    /// included.
    pub fn purge_comments(&mut self) -> Result<PurgedComments> {
        self.write(|tx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {COMMENT_COLS} FROM comments WHERE deleted_at IS NOT NULL ORDER BY created_at, rowid"
            ))?;
            let comments = stmt
                .query_map([], comment_row)?
                .collect::<std::result::Result<Vec<Comment>, _>>()?;
            drop(stmt);
            purge_comment_rows(tx, &comments)
        })
    }

    // ----- docs -----

    /// Moves the doc to the trash. Refused while it has live docs under it.
    pub fn delete_doc(&mut self, id: &str) -> Result<Doc> {
        self.write(|tx| {
            let doc = read_doc(tx, id)?.ok_or_else(|| LclError::DocNotFound(id.to_string()))?;
            if doc.deleted_at.is_some() {
                return Err(LclError::DocInTrash(doc.id));
            }
            let children = doc_children(tx, &doc.id, true)?;
            if !children.is_empty() {
                return Err(LclError::DocHasChildren {
                    doc: doc.id,
                    children: children.join(", "),
                });
            }
            tx.execute(
                "UPDATE docs SET deleted_at = ?1 WHERE id = ?2",
                params![now(), doc.id],
            )?;
            Ok(doc)
        })
    }

    /// Takes the doc out of the trash and returns it, live. Refused while
    /// its parent is in the trash.
    pub fn restore_doc(&mut self, id: &str) -> Result<Doc> {
        self.write(|tx| {
            let mut doc = read_doc(tx, id)?.ok_or_else(|| LclError::DocNotFound(id.to_string()))?;
            if doc.deleted_at.is_none() {
                return Err(LclError::NotInTrash(format!("doc {}", doc.id)));
            }
            if let Some(parent) = &doc.parent_id
                && read_doc(tx, parent)?.is_some_and(|p| p.deleted_at.is_some())
            {
                return Err(LclError::DocParentInTrash {
                    doc: doc.id,
                    parent: parent.clone(),
                });
            }
            tx.execute(
                "UPDATE docs SET deleted_at = NULL WHERE id = ?1",
                params![doc.id],
            )?;
            doc.deleted_at = None;
            Ok(doc)
        })
    }

    /// Removes one trashed doc for good. Refused while docs are under it.
    pub fn purge_doc(&mut self, id: &str) -> Result<PurgedDocs> {
        self.write(|tx| {
            let doc = read_doc(tx, id)?.ok_or_else(|| LclError::DocNotFound(id.to_string()))?;
            if doc.deleted_at.is_none() {
                return Err(LclError::PurgeLiveItem {
                    what: format!("doc {}", doc.id),
                    delete: "lcl doc delete".into(),
                });
            }
            let children = doc_children(tx, &doc.id, false)?;
            if !children.is_empty() {
                return Err(LclError::DocPurgeHasChildren {
                    doc: doc.id,
                    children: children.join(", "),
                });
            }
            purge_doc_rows(tx, &[doc.id])
        })
    }

    /// Removes every trashed doc for good, parents and the docs under them
    /// together.
    pub fn purge_docs(&mut self) -> Result<PurgedDocs> {
        self.write(|tx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {DOC_COLS} FROM docs WHERE deleted_at IS NOT NULL ORDER BY id"
            ))?;
            let ids = stmt
                .query_map([], doc_row)?
                .map(|d| d.map(|d| d.id))
                .collect::<std::result::Result<Vec<String>, _>>()?;
            drop(stmt);
            // A trashed parent may go before the doc under it; the check
            // runs at commit, when both are gone.
            tx.pragma_update(None, "defer_foreign_keys", true)?;
            purge_doc_rows(tx, &ids)
        })
    }
}

/// The `KEY-N` of the issue with this UUID, if there is one.
fn issue_identifier(conn: &Connection, issue_id: &str) -> Result<Option<String>> {
    let number: Option<i64> = conn
        .query_row(
            "SELECT number FROM issues WHERE id=?1",
            params![issue_id],
            |r| r.get(0),
        )
        .optional()?;
    let key = read_key(conn)?;
    Ok(number.map(|n| identifier(&key, n)))
}

fn purge_comment_rows(tx: &Connection, comments: &[Comment]) -> Result<PurgedComments> {
    let mut out = PurgedComments::default();
    for c in comments {
        out.comments += tx.execute(
            "DELETE FROM comments WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![c.id],
        )? as i64;
        out.purged.push(c.id.clone());
    }
    Ok(out)
}

/// The ids of the docs directly under `id`, by id: the live ones, or all.
fn doc_children(conn: &Connection, id: &str, live_only: bool) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM docs WHERE parent_id = ?1 {} ORDER BY id",
        if live_only {
            "AND deleted_at IS NULL"
        } else {
            ""
        }
    ))?;
    let ids = stmt
        .query_map(params![id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(ids)
}

fn purge_doc_rows(tx: &Connection, ids: &[String]) -> Result<PurgedDocs> {
    let mut out = PurgedDocs::default();
    for id in ids {
        out.docs += tx.execute(
            "DELETE FROM docs WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![id],
        )? as i64;
        out.purged.push(id.clone());
    }
    Ok(out)
}
