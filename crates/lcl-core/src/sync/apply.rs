//! Applying what a sync pulls: docs/sync-protocol.md, "Applying what it
//! pulls".
//!
//! A pull is applied whole, in one transaction, by its net effect: for each
//! field of each record, the change with the latest clock, unless this file
//! has a later one. That is what applying the changes one by one in `seq`
//! order with the rules gives, without the states between them, where two
//! fields of one write (an issue's project and milestone) or a correction
//! and what it corrects can break a rule of the file for a moment. Creates
//! go first, then each record's fields in one update, then memberships,
//! then purges, then what a rule of the file refused, again until nothing
//! more goes. Foreign keys are checked at the commit.
//!
//! The writes run with the log's triggers on. The rows they log are deleted
//! after, and the changes go in the log as they came, each with its `seq`:
//! so the file's history holds what was done where it was done, and no
//! change goes back to the service as this machine's.

use super::{Change, SyncConflict, conflict};
use crate::changes::{clock_seconds, last_seq};
use crate::db::{put_meta, read_meta};
use crate::error::{LclError, Result};
use crate::migrations::logged_record;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params, params_from_iter};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// What applying a pull did.
#[derive(Debug, Default)]
pub(crate) struct Applied {
    /// The changes received from elsewhere: all but this machine's own.
    pub received: usize,
    /// Fields where this machine's change gave way to a later one.
    pub overwritten: Vec<SyncConflict>,
}

/// The tables with an `updated_at`, which a change moves forward.
const TOUCHED: &[&str] = &["issues", "projects", "milestones", "docs"];

/// Where a change lands.
#[derive(Clone, Copy)]
enum Target<'a> {
    Create {
        table: &'static str,
        fields: &'static [&'static str],
    },
    Purge {
        table: &'static str,
    },
    Field {
        table: &'static str,
        column: &'static str,
    },
    /// An issue's label, cycle or relation: the field names it.
    Member(Member<'a>),
}

#[derive(Clone, Copy)]
enum Member<'a> {
    Label(&'a str),
    Cycle(&'a str),
    Relation { kind: &'a str, other: &'a str },
}

impl Member<'_> {
    /// The record at the membership's other end.
    fn other(&self) -> &str {
        match self {
            Member::Label(o) | Member::Cycle(o) | Member::Relation { other: o, .. } => o,
        }
    }
}

fn unknown(c: &Change) -> LclError {
    LclError::SyncUnknownChange {
        kind: c.kind.clone(),
        field: c.field.clone(),
    }
}

fn target(c: &Change) -> Result<Target<'_>> {
    let (table, fields, _) = logged_record(&c.kind).ok_or_else(|| unknown(c))?;
    if c.field == "*" {
        return match c.new {
            Value::Null => Ok(Target::Purge { table }),
            Value::Object(_) => Ok(Target::Create { table, fields }),
            _ => Err(unknown(c)),
        };
    }
    if let Some(column) = fields.iter().find(|f| **f == c.field) {
        return Ok(Target::Field { table, column });
    }
    let member = match (c.kind.as_str(), c.field.split_once(':')) {
        ("issue", Some(("label", other))) => Member::Label(other),
        ("issue", Some(("cycle", other))) => Member::Cycle(other),
        ("issue", Some((kind @ ("blocks" | "related" | "duplicate"), other))) => {
            Member::Relation { kind, other }
        }
        _ => return Err(unknown(c)),
    };
    Ok(Target::Member(member))
}

/// A JSON value as SQLite stores it; None for an array or an object.
fn sql(v: &Value) -> Option<Sql> {
    Some(match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Sql::Integer(i),
            None => Sql::Real(n.as_f64()?),
        },
        Value::String(s) => Sql::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => return None,
    })
}

/// Whether the error is a rule of the file refusing the write.
fn refused(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation)
}

/// A write to try: Ok(true) when done, Ok(false) when a rule of the file
/// refused it, or its record isn't here (yet).
enum Step<'a> {
    Create {
        change: &'a Change,
        table: &'static str,
        fields: &'static [&'static str],
    },
    Update {
        /// The latest of the changes it applies, to name it.
        change: &'a Change,
        table: &'static str,
        sets: Vec<(&'static str, Sql)>,
        /// The latest change's time, for `updated_at`.
        seconds: i64,
    },
    Member {
        change: &'a Change,
        member: Member<'a>,
    },
}

impl Step<'_> {
    fn change(&self) -> &Change {
        match self {
            Step::Create { change, .. }
            | Step::Update { change, .. }
            | Step::Member { change, .. } => change,
        }
    }

    fn run(&self, tx: &Connection) -> Result<bool> {
        let done = |r: rusqlite::Result<usize>| match r {
            Ok(n) => Ok(n > 0),
            Err(e) if refused(&e) => Ok(false),
            Err(e) => Err(LclError::from(e)),
        };
        match self {
            Step::Create {
                change,
                table,
                fields,
            } => {
                let there: Option<i64> = tx
                    .query_row(
                        &format!("SELECT 1 FROM {table} WHERE id = ?1"),
                        [&change.record],
                        |r| r.get(0),
                    )
                    .optional()?;
                if there.is_some() {
                    return Ok(true);
                }
                let mut columns = vec!["id"];
                let mut values = vec![Sql::Text(change.record.clone())];
                for (key, value) in change.new.as_object().into_iter().flatten() {
                    let column = fields
                        .iter()
                        .find(|f| **f == key)
                        .ok_or_else(|| unknown(change))?;
                    columns.push(column);
                    values.push(sql(value).ok_or_else(|| unknown(change))?);
                }
                if TOUCHED.contains(table) {
                    columns.push("updated_at");
                    values.push(Sql::Integer(clock_seconds(&change.clock)));
                }
                let marks: Vec<String> = (1..=values.len()).map(|i| format!("?{i}")).collect();
                done(tx.execute(
                    &format!(
                        "INSERT INTO {table} ({}) VALUES ({})",
                        columns.join(", "),
                        marks.join(", ")
                    ),
                    params_from_iter(values),
                ))
            }
            Step::Update {
                change,
                table,
                sets,
                seconds,
            } => {
                let mut assigns: Vec<String> = sets
                    .iter()
                    .enumerate()
                    .map(|(i, (column, _))| format!("{column} = ?{}", i + 2))
                    .collect();
                if TOUCHED.contains(table) {
                    assigns.push(format!("updated_at = MAX(updated_at, {seconds})"));
                }
                let mut values = vec![Sql::Text(change.record.clone())];
                values.extend(sets.iter().map(|(_, v)| v.clone()));
                done(tx.execute(
                    &format!("UPDATE {table} SET {} WHERE id = ?1", assigns.join(", ")),
                    params_from_iter(values),
                ))
            }
            Step::Member { change, member } => {
                let issue = change.record.as_str();
                let (table, column, kind) = match member {
                    Member::Label(_) => ("issue_labels", "label_id", None),
                    Member::Cycle(_) => ("issue_cycles", "cycle_id", None),
                    Member::Relation { kind, .. } => ("issue_relations", "related_id", Some(*kind)),
                };
                let other = member.other();
                let which = format!(
                    "issue_id = ?1 AND {column} = ?2{}",
                    if kind.is_some() { " AND kind = ?3" } else { "" }
                );
                let mut keys = vec![Sql::Text(issue.into()), Sql::Text(other.into())];
                keys.extend(kind.map(|k| Sql::Text(k.into())));
                if change.new.is_null() {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE {which}"),
                        params_from_iter(keys),
                    )?;
                    return Ok(true);
                }
                let there: Option<i64> = tx
                    .query_row(
                        &format!("SELECT 1 FROM {table} WHERE {which}"),
                        params_from_iter(keys),
                        |r| r.get(0),
                    )
                    .optional()?;
                if there.is_some() {
                    return Ok(true);
                }
                match kind {
                    None => done(tx.execute(
                        &format!("INSERT INTO {table} (issue_id, {column}) VALUES (?1, ?2)"),
                        params![issue, other],
                    )),
                    Some(kind) => {
                        let at = change.new.as_i64().ok_or_else(|| unknown(change))?;
                        done(tx.execute(
                            "INSERT INTO issue_relations (issue_id, related_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
                            params![issue, other, kind, at],
                        ))
                    }
                }
            }
        }
    }
}

fn cant_apply(c: &Change, detail: String) -> LclError {
    LclError::SyncCantApply {
        seq: c.seq.unwrap_or(0),
        kind: c.kind.clone(),
        record: c.record.clone(),
        field: c.field.clone(),
        detail,
    }
}

/// Applies `pulled`, the changes after this machine's last `seq`, in `seq`
/// order, inside the caller's transaction. Changes it has already (its own,
/// and any it applied before) are skipped. `machine` is this machine's id.
pub(crate) fn apply(tx: &Connection, pulled: &[Change], machine: &str) -> Result<Applied> {
    let known: HashSet<i64> = {
        let mut stmt = tx.prepare("SELECT synced FROM changes WHERE synced IS NOT NULL")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let incoming: Vec<&Change> = pulled
        .iter()
        .filter(|c| !c.seq.is_some_and(|s| known.contains(&s)))
        .collect();
    if incoming.is_empty() {
        return Ok(Applied::default());
    }
    // A change this lcl can't read stops the sync before anything is written.
    let targets: Vec<Target> = incoming.iter().map(|c| target(c)).collect::<Result<_>>()?;
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    let before = last_seq(tx)?;
    let clock_before = read_meta(tx, "clock")?;
    let applies = |c: &Change| c.applied == Some(true);

    let purges: Vec<(&Change, &'static str)> = incoming
        .iter()
        .zip(&targets)
        .filter_map(|(c, t)| match t {
            Target::Purge { table } if applies(c) => Some((*c, *table)),
            _ => None,
        })
        .collect();
    let purging: HashSet<&str> = purges.iter().map(|(c, _)| c.record.as_str()).collect();
    let dead = |record: &str| -> Result<bool> {
        if purging.contains(record) {
            return Ok(true);
        }
        Ok(tx
            .query_row("SELECT 1 FROM purged WHERE record = ?1", [record], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .is_some())
    };

    let mut steps: Vec<Step> = Vec::new();
    // For each field, the latest change.
    let mut latest: BTreeMap<(&str, &str), (&Change, Target)> = BTreeMap::new();
    for (c, t) in incoming.iter().zip(&targets) {
        if !applies(c) || dead(&c.record)? {
            continue;
        }
        match t {
            Target::Create { table, fields } => steps.push(Step::Create {
                change: c,
                table,
                fields,
            }),
            Target::Purge { .. } => {}
            Target::Member(m) if dead(m.other())? => {}
            Target::Field { .. } | Target::Member(_) => {
                let key = (c.record.as_str(), c.field.as_str());
                if latest.get(&key).is_none_or(|(l, _)| l.clock < c.clock) {
                    latest.insert(key, (c, *t));
                }
            }
        }
    }
    let mut overwritten = Vec::new();
    let mut updates: BTreeMap<&str, Step> = BTreeMap::new();
    let mine = format!("-{machine}");
    for ((record, field), (c, t)) in latest {
        let here: Option<String> = tx.query_row(
            "SELECT MAX(clock) FROM changes WHERE record = ?1 AND field = ?2 AND seq <= ?3",
            params![record, field, before],
            |r| r.get(0),
        )?;
        if here.as_deref().is_some_and(|h| h > c.clock.as_str()) {
            continue;
        }
        if here.as_deref().is_some_and(|h| h.ends_with(&mine)) {
            overwritten.push(conflict(tx, c)?);
        }
        match t {
            Target::Field { table, column } => {
                let value = sql(&c.new).ok_or_else(|| unknown(c))?;
                let step = updates.entry(record).or_insert(Step::Update {
                    change: c,
                    table,
                    sets: Vec::new(),
                    seconds: 0,
                });
                if let Step::Update {
                    change,
                    sets,
                    seconds,
                    ..
                } = step
                {
                    sets.push((column, value));
                    *seconds = (*seconds).max(clock_seconds(&c.clock));
                    if change.clock < c.clock {
                        *change = c;
                    }
                }
            }
            Target::Member(member) => steps.push(Step::Member { change: c, member }),
            Target::Create { .. } | Target::Purge { .. } => {}
        }
    }
    // Creates, then fields, then memberships.
    let members = steps.split_off(
        steps
            .iter()
            .position(|s| matches!(s, Step::Member { .. }))
            .unwrap_or(steps.len()),
    );
    steps.extend(updates.into_values());
    steps.extend(members);

    let mut left = Vec::new();
    for step in steps {
        if !step.run(tx)? {
            left.push(step);
        }
    }
    for (c, table) in &purges {
        // The tombstone takes the purge's clock.
        put_meta(tx, "clock", &c.clock)?;
        if let Err(e) = tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [&c.record]) {
            if refused(&e) {
                return Err(cant_apply(
                    c,
                    "something in this file still names the record".into(),
                ));
            }
            return Err(e.into());
        }
    }
    // Again, until all went or a round moved nothing.
    while !left.is_empty() {
        let tried = left.len();
        let mut still = Vec::new();
        for step in left.drain(..) {
            if !step.run(tx)? {
                still.push(step);
            }
        }
        if still.len() == tried {
            return Err(cant_apply(
                still[0].change(),
                "a rule of this file refuses it, or its record isn't here".into(),
            ));
        }
        left = still;
    }

    // What the writes logged goes, and the changes go in as they came.
    tx.execute("DELETE FROM changes WHERE seq > ?1", [before])?;
    let mut newest = clock_before.unwrap_or_default();
    for (c, t) in incoming.iter().zip(&targets) {
        if c.clock > newest {
            newest = c.clock.clone();
        }
        // A purge took the history of its record with it.
        let kept = match t {
            Target::Purge { .. } => true,
            Target::Member(m) => !dead(&c.record)? && !dead(m.other())?,
            _ => !dead(&c.record)?,
        };
        if !kept {
            continue;
        }
        let text =
            |v: &Value| serde_json::to_string(v).map_err(|e| LclError::Encode(e.to_string()));
        let (old, new) = if c.field == "*" {
            (None, (!c.new.is_null()).then(|| text(&c.new)).transpose()?)
        } else {
            (Some(text(&c.old)?), Some(text(&c.new)?))
        };
        tx.execute(
            "INSERT INTO changes (clock, kind, record, field, old, new, author, synced)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                c.clock, c.kind, c.record, c.field, old, new, c.author, c.seq
            ],
        )?;
    }
    // This machine's next clock comes after every clock it has seen.
    put_meta(tx, "clock", &newest)?;
    Ok(Applied {
        received: incoming.len(),
        overwritten,
    })
}
