//! `lcl insights`: lead time, cycle time and throughput, from the times lcl
//! stamps on each issue as its status changes (see `Issue::apply_lifecycle`).
//! Nothing is stored. `report` is a pure function of the issues, the days
//! asked for, and the time zone those days are in.
//!
//! - An issue is completed on the day of its `completed_at`: it is Done and
//!   has one. Those completed in the range make the report.
//! - Lead time is `created_at` to `completed_at`; cycle time is `started_at`
//!   (the first move to InProgress or InReview since it was last in Backlog
//!   or Todo) to `completed_at`. An issue with no start time counts toward
//!   lead time only.
//! - Canceled and Duplicate issues, and those in the trash, are left out
//!   altogether. Of the rest, an issue that isn't Done, or is Done with no
//!   completion time (from before lcl stamped one), counts toward nothing;
//!   the report says how many of those there were by the range's end.

use crate::models::{Issue, Status};
use chrono::{Datelike, Days, NaiveDate, TimeZone};
use serde::Serialize;

/// A report over the days `since` to `until`, both included.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Insights {
    pub since: NaiveDate,
    pub until: NaiveDate,
    /// Issues completed in the range.
    pub completed: usize,
    /// `completed` over the range's days, times 7, to two decimals.
    pub per_week: f64,
    /// Each week (Monday to Sunday) the range touches, oldest first, with
    /// the issues completed in it and in the range.
    pub weeks: Vec<Week>,
    pub lead_time: Spread,
    pub cycle_time: Spread,
    pub left_out: LeftOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Week {
    /// Its Monday.
    pub week: NaiveDate,
    pub completed: usize,
}

/// How long the issues it counts took, in whole seconds: the median and
/// the 90th percentile, as a spreadsheet's PERCENTILE.INC finds them (the
/// median of 1, 2, 3 and 4 is 2.5, rounded to whole seconds). Both are null
/// when it counts no issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spread {
    pub issues: usize,
    pub median: Option<i64>,
    pub p90: Option<i64>,
}

/// What the report couldn't count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeftOut {
    /// Issues made by the range's end that aren't Done, or are Done with no
    /// completion time: in neither lead time nor cycle time.
    pub not_completed: usize,
    /// Issues completed in the range with no start time: in lead time, not
    /// in cycle time.
    pub not_started: usize,
}

/// The report on `issues` for the days `since` to `until`, both included,
/// in time zone `tz`. A range that ends before it starts has no days, and
/// no issue is completed in it.
pub fn report<Tz: TimeZone>(
    issues: &[Issue],
    since: NaiveDate,
    until: NaiveDate,
    tz: &Tz,
) -> Insights {
    let day = |at: i64| tz.timestamp_opt(at, 0).earliest().map(|t| t.date_naive());
    let in_range = |d: NaiveDate| since <= d && d <= until;
    let mut leads = Vec::new();
    let mut cycles = Vec::new();
    let mut done_days = Vec::new();
    let mut left_out = LeftOut {
        not_completed: 0,
        not_started: 0,
    };
    for issue in issues {
        if issue.deleted_at.is_some()
            || matches!(issue.status, Status::Canceled | Status::Duplicate)
        {
            continue;
        }
        let completed = match issue.completed_at {
            Some(at) if issue.status == Status::Done => Some(at),
            _ => None,
        };
        let Some(done) = completed else {
            if day(issue.created_at).is_some_and(|d| d <= until) {
                left_out.not_completed += 1;
            }
            continue;
        };
        let Some(done_day) = day(done).filter(|d| in_range(*d)) else {
            continue;
        };
        done_days.push(done_day);
        leads.push((done - issue.created_at).max(0));
        match issue.started_at {
            Some(started) => cycles.push((done - started).max(0)),
            None => left_out.not_started += 1,
        }
    }
    let days = (until - since).num_days() + 1;
    let per_week = if days > 0 {
        (done_days.len() as f64 * 7.0 / days as f64 * 100.0).round() / 100.0
    } else {
        0.0
    };
    Insights {
        since,
        until,
        completed: done_days.len(),
        per_week,
        weeks: weeks(since, until, &done_days),
        lead_time: spread(leads),
        cycle_time: spread(cycles),
        left_out,
    }
}

/// The Monday of `d`'s week.
fn monday(d: NaiveDate) -> NaiveDate {
    d - Days::new(u64::from(d.weekday().num_days_from_monday()))
}

/// The weeks from `since`'s to `until`'s, each with how many of `done` are
/// in it; none when `until` is before `since`.
fn weeks(since: NaiveDate, until: NaiveDate, done: &[NaiveDate]) -> Vec<Week> {
    let mut counts = std::collections::HashMap::new();
    for d in done {
        *counts.entry(monday(*d)).or_insert(0) += 1;
    }
    let mut out = Vec::new();
    let mut week = monday(since);
    while week <= until {
        out.push(Week {
            week,
            completed: counts.get(&week).copied().unwrap_or(0),
        });
        // NaiveDate ends at 262143-12-31, past any date parse::date takes.
        week = week + Days::new(7);
    }
    out
}

fn spread(mut seconds: Vec<i64>) -> Spread {
    seconds.sort_unstable();
    Spread {
        issues: seconds.len(),
        median: percentile(&seconds, 0.5),
        p90: percentile(&seconds, 0.9),
    }
}

/// The `p` percentile (0 to 1) of `sorted`, interpolated between the two
/// nearest values as PERCENTILE.INC does, rounded to a whole second.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    let last = sorted.len().checked_sub(1)?;
    let at = last as f64 * p;
    let below = at.floor() as usize;
    let above = (below + 1).min(last);
    let (a, b) = (sorted[below] as f64, sorted[above] as f64);
    Some((a + (b - a) * (at - below as f64)).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{IssueId, Priority};
    use chrono::{FixedOffset, Utc};

    const DAY: i64 = 86_400;

    fn date(s: &str) -> NaiveDate {
        crate::parse::date(s).unwrap()
    }

    /// Midnight UTC on `s`, in epoch seconds.
    fn at(s: &str) -> i64 {
        date(s).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
    }

    /// Issue `n`, made at `created`: Backlog, or with `done`, Done then,
    /// and with `started`, started then.
    fn issue(n: i64, created: i64, started: Option<i64>, done: Option<i64>) -> Issue {
        Issue {
            id: IssueId::from_stored(format!("id-{n}")),
            number: n,
            identifier: format!("D-{n}"),
            title: String::new(),
            description: String::new(),
            status: if done.is_some() {
                Status::Done
            } else {
                Status::Backlog
            },
            priority: Priority::None,
            assignee: None,
            parent_id: None,
            parent_identifier: None,
            project_id: None,
            project: None,
            milestone_id: None,
            milestone: None,
            r#ref: None,
            creator: None,
            estimate: None,
            due_date: None,
            labels: vec![],
            cycles: vec![],
            created_at: created,
            updated_at: created,
            completed_at: done,
            started_at: started,
            canceled_at: None,
            deleted_at: None,
        }
    }

    #[test]
    fn an_empty_range_has_zero_counts_and_its_weeks() {
        // 2026-11-02 is a Monday; the range touches two weeks.
        let r = report(&[], date("2026-11-04"), date("2026-11-10"), &Utc);
        assert_eq!(
            r,
            Insights {
                since: date("2026-11-04"),
                until: date("2026-11-10"),
                completed: 0,
                per_week: 0.0,
                weeks: vec![
                    Week {
                        week: date("2026-11-02"),
                        completed: 0
                    },
                    Week {
                        week: date("2026-11-09"),
                        completed: 0
                    },
                ],
                lead_time: Spread {
                    issues: 0,
                    median: None,
                    p90: None
                },
                cycle_time: Spread {
                    issues: 0,
                    median: None,
                    p90: None
                },
                left_out: LeftOut {
                    not_completed: 0,
                    not_started: 0
                },
            }
        );
        // A range that ends before it starts: no days, no weeks.
        let r = report(&[], date("2026-11-10"), date("2026-11-04"), &Utc);
        assert_eq!((r.weeks.len(), r.per_week), (0, 0.0));
    }

    #[test]
    fn one_issue_is_its_own_median_and_90th_percentile() {
        let made = at("2026-11-02");
        let one = issue(1, made, Some(made + DAY), Some(made + 3 * DAY));
        let r = report(&[one], date("2026-11-02"), date("2026-11-15"), &Utc);
        assert_eq!(r.completed, 1);
        assert_eq!(r.per_week, 0.5);
        assert_eq!(
            (r.lead_time.median, r.lead_time.p90),
            (Some(3 * DAY), Some(3 * DAY))
        );
        assert_eq!(
            (r.cycle_time.issues, r.cycle_time.median),
            (1, Some(2 * DAY))
        );
        let counts: Vec<usize> = r.weeks.iter().map(|w| w.completed).collect();
        assert_eq!(counts, [1, 0]);
    }

    #[test]
    fn percentiles_interpolate_as_a_spreadsheet_does() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[7], 0.9), Some(7));
        assert_eq!(percentile(&[1, 2, 3, 4], 0.5), Some(3)); // 2.5, rounded
        assert_eq!(percentile(&[10, 20, 30, 40], 0.5), Some(25));
        let ten: Vec<i64> = (1..=10).map(|n| n * 100).collect();
        assert_eq!(percentile(&ten, 0.5), Some(550));
        assert_eq!(percentile(&ten, 0.9), Some(910));
        assert_eq!(percentile(&[0, 1000], 0.9), Some(900));
    }

    #[test]
    fn what_counts_toward_what() {
        let made = at("2026-11-02");
        let done = Some(made + 2 * DAY);
        let mut canceled = issue(4, made, Some(made), done);
        canceled.status = Status::Canceled;
        let mut duplicate = issue(5, made, None, None);
        duplicate.status = Status::Duplicate;
        let mut trashed = issue(6, made, Some(made), done);
        trashed.deleted_at = Some(made + 3 * DAY);
        // Done with no completion time: from before lcl stamped one.
        let mut unstamped = issue(7, made, None, None);
        unstamped.status = Status::Done;
        let issues = [
            issue(1, made, Some(made + DAY), done),
            // No start time: lead time only.
            issue(2, made, None, done),
            // Not completed.
            issue(3, made, Some(made), None),
            canceled,
            duplicate,
            trashed,
            unstamped,
            // Completed before the range: in nothing.
            issue(8, made - 30 * DAY, None, Some(made - 20 * DAY)),
            // Made after the range: not counted as left out.
            issue(9, made + 30 * DAY, None, None),
        ];
        let r = report(&issues, date("2026-11-02"), date("2026-11-08"), &Utc);
        assert_eq!(
            (r.completed, r.lead_time.issues, r.cycle_time.issues),
            (2, 2, 1)
        );
        assert_eq!(
            r.left_out,
            LeftOut {
                not_completed: 2,
                not_started: 1
            }
        );
        assert_eq!(r.lead_time.median, Some(2 * DAY));
        assert_eq!(r.cycle_time.median, Some(DAY));
    }

    #[test]
    fn days_and_weeks_are_in_the_time_zone_given() {
        // Completed at 23:30 UTC on Sunday 2026-11-08: still Sunday in UTC,
        // Monday 2026-11-09 an hour east.
        let done = at("2026-11-08") + 23 * 3600 + 1800;
        let one = [issue(1, done - DAY, None, Some(done))];
        let east = FixedOffset::east_opt(3600).unwrap();
        let (since, until) = (date("2026-11-02"), date("2026-11-15"));
        let week_of = |r: Insights| r.weeks.iter().position(|w| w.completed == 1);
        assert_eq!(
            (
                week_of(report(&one, since, until, &Utc)),
                week_of(report(&one, since, until, &east))
            ),
            (Some(0), Some(1))
        );
        // The range's last day is the 8th: in it in UTC, not an hour east.
        let until = date("2026-11-08");
        assert_eq!(
            (
                report(&one, since, until, &Utc).completed,
                report(&one, since, until, &east).completed
            ),
            (1, 0)
        );
    }

    #[test]
    fn a_start_or_creation_after_completion_counts_as_no_time() {
        let made = at("2026-11-03");
        // Clocks disagree: started after it was completed.
        let odd = issue(1, made, Some(made + 5 * DAY), Some(made + DAY));
        let r = report(&[odd], date("2026-11-02"), date("2026-11-08"), &Utc);
        assert_eq!(
            (r.lead_time.median, r.cycle_time.median),
            (Some(DAY), Some(0))
        );
    }
}
