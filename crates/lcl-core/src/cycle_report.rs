//! `lcl cycle report`: what a cycle held when it started, what was added to
//! it and removed from it after, what was completed in it, and the same
//! completion (velocity) for the cycles before it.
//!
//! Membership comes from the change log (docs/sync.md), not from what the
//! cycle holds now: each `cycle:UUID` row says the issue entered (`true`) or
//! left the cycle at its clock.
//!
//! - A cycle runs from the first moment of its first day to the first
//!   moment after its last, in a given time zone; one not over yet runs to
//!   now. Planned is what it held at its start, added what entered after
//!   (up to its end), removed what it held at or after its start but not at
//!   its end, and completed what it held at its end, Done before the end.
//! - The log holds no time for what was there before it began: an upgrade
//!   to schema 14 and an import log each membership as it stood, at the
//!   log's first clock. (A real first write can't hold one: an issue joins
//!   a cycle or a label that exists already.) A cycle that held issues then
//!   and started before then has no known start: its scope change is
//!   unavailable. So is a cycle with no start date. Its completion is still
//!   reported.
//! - Points are estimates. An issue without one counts as an issue, and in
//!   no point total; `unestimated` says how many.
//! - Issues in the trash are left out; purged ones are gone from the log.

use crate::db::{Store, all_cycles};
use crate::error::Result;
use crate::models::{Cycle, Status};
use chrono::{NaiveDate, TimeZone};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::BTreeMap;

/// How many cycles before the one reported its velocity is compared with.
pub const EARLIER: usize = 5;

/// Issues, and the sum of their estimates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Count {
    pub issues: usize,
    pub points: i64,
}

impl Count {
    fn add(&mut self, estimate: Option<i64>) {
        self.issues += 1;
        self.points += estimate.unwrap_or(0);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Scope {
    pub planned: Count,
    pub added: Count,
    pub removed: Count,
}

/// What the log says of one issue that has been in the cycle.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub estimate: Option<i64>,
    /// When it was completed, in seconds, if it is Done.
    pub completed_at: Option<i64>,
    /// Its entries to (`true`) and exits from the cycle, oldest first, at
    /// their clocks' milliseconds.
    pub changes: Vec<(i64, bool)>,
}

impl Facts {
    /// Whether it was in the cycle at `ms`: what its last change at or
    /// before then left.
    fn member_at(&self, ms: i64) -> bool {
        self.changes
            .iter()
            .take_while(|(at, _)| *at <= ms)
            .last()
            .is_some_and(|(_, member)| *member)
    }
}

/// A cycle's times, in milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// The first moment of its first day; `None` with no start date.
    pub start: Option<i64>,
    /// The first moment after its last day; `None` with no end date.
    pub end: Option<i64>,
    pub now: i64,
    /// The log's first clock, when memberships were logged at it as they
    /// stood (an upgrade or an import).
    pub log_began: Option<i64>,
}

/// Why a cycle's scope change can't be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unknown {
    NoStart,
    /// The cycle held issues when the log began, at this millisecond,
    /// after the cycle started.
    LogBegan(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    pub scope: std::result::Result<Scope, Unknown>,
    pub completed: Count,
    /// Issues counted anywhere above with no estimate.
    pub unestimated: usize,
}

/// One cycle's tally from the facts of every issue that has been in it.
pub fn tally(issues: &[Facts], w: &Window) -> Tally {
    let end = w.end.map_or(w.now, |e| e.min(w.now));
    // Memberships logged as they stood when the log began.
    let as_of = w.log_began.filter(|b| {
        issues
            .iter()
            .any(|f| f.changes.first().is_some_and(|c| c.0 == *b))
    });
    // Before then, what the cycle held is known only as of then.
    let known = |ms: i64| as_of.map_or(ms, |b| ms.max(b));
    let mut counted = vec![false; issues.len()];

    let mut completed = Count::default();
    for (i, f) in issues.iter().enumerate() {
        let done = f
            .completed_at
            .is_some_and(|c| w.end.is_none_or(|e| c.saturating_mul(1000) < e));
        if done && f.member_at(known(end)) {
            completed.add(f.estimate);
            counted[i] = true;
        }
    }

    let scope = match w.start {
        None => Err(Unknown::NoStart),
        Some(start) => {
            // Not started yet: what it holds so far is planned.
            let start = start.min(w.now);
            match as_of {
                Some(b) if start < b => Err(Unknown::LogBegan(b)),
                _ => {
                    let mut s = Scope::default();
                    for (i, f) in issues.iter().enumerate() {
                        let planned = f.member_at(start);
                        let added = !planned
                            && f.changes
                                .iter()
                                .any(|(at, m)| *m && start < *at && *at <= end);
                        if planned {
                            s.planned.add(f.estimate);
                        }
                        if added {
                            s.added.add(f.estimate);
                        }
                        if (planned || added) && !f.member_at(end) {
                            s.removed.add(f.estimate);
                        }
                        counted[i] |= planned || added;
                    }
                    Ok(s)
                }
            }
        }
    };
    let unestimated = issues
        .iter()
        .zip(&counted)
        .filter(|(f, c)| **c && f.estimate.is_none())
        .count();
    Tally {
        scope,
        completed,
        unestimated,
    }
}

/// A cycle and what it completed, for comparison.
#[derive(Debug, Clone, Serialize)]
pub struct Velocity {
    #[serde(flatten)]
    pub cycle: Cycle,
    pub completed: Count,
}

/// What `lcl cycle report` prints.
#[derive(Debug, Clone, Serialize)]
pub struct CycleReport {
    pub cycle: Cycle,
    /// Null when it can't be told; `scope_unavailable` says why.
    pub scope: Option<Scope>,
    pub scope_unavailable: Option<String>,
    pub completed: Count,
    pub unestimated: usize,
    /// The cycles before it (by start date; those with none after, by
    /// creation), at most `EARLIER`, then this one, oldest first.
    pub velocity: Vec<Velocity>,
}

/// The first moment of `day` in `tz`, in milliseconds.
fn midnight<Tz: TimeZone>(day: NaiveDate, tz: &Tz) -> i64 {
    let local = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    match tz.from_local_datetime(&local).earliest() {
        Some(t) => t.timestamp_millis(),
        // Midnight skipped by a clock change: an hour on is safe.
        None => (local + chrono::Duration::hours(1))
            .and_utc()
            .timestamp_millis(),
    }
}

/// A clock's milliseconds (`MS-COUNTER-MACHINE`).
fn clock_ms(clock: &str) -> Option<i64> {
    crate::changes::parse_clock(clock).map(|(ms, _)| ms)
}

impl Store {
    /// The report on cycle `r` (name or id), with days in `tz`, as of
    /// `now_ms`.
    pub fn cycle_report<Tz: TimeZone>(&self, r: &str, tz: &Tz, now_ms: i64) -> Result<CycleReport> {
        self.read(|conn| {
            let cycle = crate::db::resolve_cycle(conn, r)?;
            let first: Option<String> =
                conn.query_row("SELECT min(clock) FROM changes", [], |r| r.get(0))?;
            let log_began = first.as_deref().and_then(clock_ms);
            let day = |d: &Option<String>| d.as_deref().and_then(|d| crate::parse::date(d).ok());
            let window = |c: &Cycle| Window {
                start: day(&c.starts_at).map(|d| midnight(d, tz)),
                end: day(&c.ends_at).and_then(|d| d.succ_opt()).map(|d| midnight(d, tz)),
                now: now_ms,
                log_began,
            };
            let facts = |c: &Cycle| -> Result<Vec<Facts>> {
                let mut by_issue: BTreeMap<String, Facts> = BTreeMap::new();
                let mut stmt = conn.prepare(
                    "SELECT record, clock, new FROM changes
                     WHERE kind = 'issue' AND field = 'cycle:' || ?1 ORDER BY clock, seq",
                )?;
                let rows = stmt.query_map(params![c.id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
                })?;
                for row in rows {
                    let (issue, clock, new) = row?;
                    // As history reads one: a clock it can't read is 0.
                    let at = clock_ms(&clock).unwrap_or(0);
                    by_issue.entry(issue).or_default().changes.push((at, new.as_deref() == Some("true")));
                }
                let mut stmt = conn.prepare(
                    "SELECT estimate, status, completed_at, deleted_at FROM issues WHERE id = ?1",
                )?;
                let mut out = Vec::new();
                for (id, mut f) in by_issue {
                    let row = stmt
                        .query_row(params![id], |r| {
                            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))
                        })
                        .optional()?;
                    // Purged, or in the trash: left out.
                    let Some((estimate, status, completed_at, None)) = row else {
                        continue;
                    };
                    f.estimate = estimate;
                    f.completed_at = completed_at.filter(|_| status == Status::Done.as_str());
                    out.push(f);
                }
                Ok(out)
            };

            let this = tally(&facts(&cycle)?, &window(&cycle));
            let (scope, scope_unavailable) = match this.scope {
                Ok(s) => (Some(s), None),
                Err(Unknown::NoStart) => (None, Some("the cycle has no start date".to_string())),
                Err(Unknown::LogBegan(ms)) => {
                    let date = tz
                        .timestamp_millis_opt(ms)
                        .earliest()
                        .map(|t| t.date_naive().to_string())
                        .unwrap_or_default();
                    (
                        None,
                        Some(format!(
                            "lcl began keeping history on {date}, after the cycle started, and the cycle already had issues then"
                        )),
                    )
                }
            };

            // Started cycles by start date, then the others by creation.
            let mut cycles = all_cycles(conn)?;
            cycles.sort_by_key(|c| (day(&c.starts_at).is_none(), day(&c.starts_at), c.created_at));
            let at = cycles.iter().position(|c| c.id == cycle.id).unwrap_or(0);
            let mut velocity = Vec::new();
            for c in &cycles[at.saturating_sub(EARLIER)..at] {
                velocity.push(Velocity {
                    cycle: c.clone(),
                    completed: tally(&facts(c)?, &window(c)).completed,
                });
            }
            velocity.push(Velocity {
                cycle: cycle.clone(),
                completed: this.completed,
            });
            Ok(CycleReport {
                cycle,
                scope,
                scope_unavailable,
                completed: this.completed,
                unestimated: this.unestimated,
                velocity,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;
    const DAY: i64 = 24 * H;

    /// An issue with this estimate, completed at `done` hours (seconds
    /// stored), and these changes at hours.
    fn issue(estimate: Option<i64>, done: Option<i64>, changes: &[(i64, bool)]) -> Facts {
        Facts {
            estimate,
            completed_at: done.map(|h| h * H / 1000),
            changes: changes.iter().map(|(h, m)| (h * H, *m)).collect(),
        }
    }

    /// Days 10 to 13 (hours 240 to 336), now at hour 400.
    fn window() -> Window {
        Window {
            start: Some(10 * DAY),
            end: Some(14 * DAY),
            now: 400 * H,
            log_began: None,
        }
    }

    fn count(issues: usize, points: i64) -> Count {
        Count { issues, points }
    }

    #[test]
    fn planned_added_removed_and_completed() {
        let issues = [
            // In before the start, completed in the cycle.
            issue(Some(3), Some(300), &[(100, true)]),
            // In before the start, removed during it.
            issue(Some(2), None, &[(100, true), (260, false)]),
            // Added during it, completed.
            issue(Some(5), Some(320), &[(250, true)]),
            // Added during it, no estimate, still open.
            issue(None, None, &[(250, true)]),
            // In and out before the start: in nothing.
            issue(Some(8), None, &[(100, true), (200, false)]),
            // Added after the end: in nothing.
            issue(Some(1), Some(390), &[(350, true)]),
            // Completed after the end: not completed in it.
            issue(Some(13), Some(340), &[(100, true)]),
        ];
        let t = tally(&issues, &window());
        assert_eq!(
            t.scope,
            Ok(Scope {
                planned: count(3, 18),
                added: count(2, 5),
                removed: count(1, 2),
            })
        );
        assert_eq!(t.completed, count(2, 8));
        assert_eq!(t.unestimated, 1);
    }

    #[test]
    fn added_then_removed_counts_in_both() {
        let issues = [issue(Some(2), None, &[(250, true), (270, false)])];
        let t = tally(&issues, &window());
        let s = t.scope.unwrap();
        assert_eq!(
            (s.planned, s.added, s.removed),
            (count(0, 0), count(1, 2), count(1, 2))
        );
        // Added again before the end: not removed.
        let issues = [issue(
            Some(2),
            None,
            &[(250, true), (270, false), (280, true)],
        )];
        let s = tally(&issues, &window()).scope.unwrap();
        assert_eq!((s.added, s.removed), (count(1, 2), count(0, 0)));
    }

    #[test]
    fn a_cycle_still_running_ends_now() {
        let mut w = window();
        w.now = 300 * H;
        let issues = [
            issue(Some(1), Some(280), &[(100, true)]),
            // Added after now: the log can't hold it yet, but a clock may
            // run ahead; it isn't counted.
            issue(Some(1), None, &[(310, true)]),
        ];
        let t = tally(&issues, &w);
        assert_eq!(t.completed, count(1, 1));
        assert_eq!(t.scope.unwrap().added, count(0, 0));
    }

    #[test]
    fn a_cycle_not_started_has_planned_what_it_holds_so_far() {
        let mut w = window();
        w.now = 200 * H;
        let issues = [
            issue(Some(3), None, &[(150, true)]),
            issue(Some(1), None, &[(190, true), (195, false)]),
        ];
        let s = tally(&issues, &w).scope.unwrap();
        assert_eq!(
            (s.planned, s.added, s.removed),
            (count(1, 3), count(0, 0), count(0, 0))
        );
    }

    #[test]
    fn no_start_date_reports_completion_only() {
        let mut w = window();
        w.start = None;
        let issues = [
            issue(Some(3), Some(300), &[(100, true)]),
            issue(None, Some(300), &[(100, true)]),
        ];
        let t = tally(&issues, &w);
        assert_eq!(t.scope, Err(Unknown::NoStart));
        assert_eq!(t.completed, count(2, 3));
        assert_eq!(t.unestimated, 1);
        // No end date either: completed whenever, in it now.
        w.end = None;
        let issues = [issue(Some(3), Some(390), &[(100, true)])];
        assert_eq!(tally(&issues, &w).completed, count(1, 3));
    }

    #[test]
    fn what_the_log_began_with_has_no_known_start() {
        // The log began at hour 260, during the cycle, holding issue 0.
        let mut w = window();
        w.log_began = Some(260 * H);
        let issues = [
            issue(Some(3), Some(300), &[(260, true)]),
            issue(Some(2), None, &[(270, true)]),
        ];
        let t = tally(&issues, &w);
        assert_eq!(t.scope, Err(Unknown::LogBegan(260 * H)));
        assert_eq!(t.completed, count(1, 3));
        // A cycle that held nothing when the log began is known whole.
        let issues = [issue(Some(2), None, &[(270, true)])];
        let s = tally(&issues, &w).scope.unwrap();
        assert_eq!(s.added, count(1, 2));
        // One that started after the log began is known whole too.
        w.log_began = Some(100 * H);
        let issues = [
            issue(Some(3), None, &[(100, true)]),
            issue(Some(2), None, &[(270, true)]),
        ];
        let s = tally(&issues, &w).scope.unwrap();
        assert_eq!((s.planned, s.added), (count(1, 3), count(1, 2)));
    }

    #[test]
    fn a_cycle_that_ended_before_the_log_began_completes_what_it_held_then() {
        // Days 10 to 13; the log began on day 20, holding issue 0 (Done on
        // day 12) and not issue 1 (moved out before then).
        let mut w = window();
        w.now = 30 * DAY;
        w.log_began = Some(20 * DAY);
        let issues = [issue(Some(5), Some(12 * 24), &[(20 * 24, true)])];
        let t = tally(&issues, &w);
        assert_eq!(t.completed, count(1, 5));
        assert_eq!(t.scope, Err(Unknown::LogBegan(20 * DAY)));
    }

    #[test]
    fn an_empty_cycle() {
        let t = tally(&[], &window());
        assert_eq!(t.scope, Ok(Scope::default()));
        assert_eq!((t.completed, t.unestimated), (Count::default(), 0));
    }
}
