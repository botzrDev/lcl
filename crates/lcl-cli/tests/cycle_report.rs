//! `lcl cycle report`: a cycle's scope (planned at its start, added and
//! removed after) from each issue's history, what it completed, and the
//! velocity of the cycles before it. The counting is tested in
//! `lcl_core::cycle_report`; these tests run the command.
//!
//! What the log records happens now, so to have issues in a cycle before it
//! started, a test moves the whole log back in time, in order, as if those
//! writes were made a week ago.

mod common;

use chrono::{Days, Local, NaiveDate};
use common::{Env, exec};
use serde_json::{Value, json};

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn ago(days: u64) -> String {
    (today() - Days::new(days)).to_string()
}

fn ahead(days: u64) -> String {
    (today() + Days::new(days)).to_string()
}

/// Every change so far, moved `days` back, in the same order.
fn back_in_time(env: &Env, days: i64) {
    exec(
        &env.db_path(),
        &format!(
            "UPDATE changes SET clock = printf('%013d', CAST(substr(clock, 1, 13) AS INTEGER) - {}) || substr(clock, 14);
             UPDATE meta SET value = (SELECT max(clock) FROM changes) WHERE key = 'clock';",
            days * 86_400_000
        ),
    );
}

fn ok(env: &Env, args: &[&str]) {
    let out = env.lcl(args);
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (0, ""),
        "{args:?}\n{out:#?}"
    );
}

fn report(env: &Env, cycle: &str) -> Value {
    let out = env.lcl(&["--json", "cycle", "report", cycle]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    out.json()
}

/// Sprint, from three days ago to four days on. A week ago DEMO-1 (3
/// points), DEMO-2 (2) and DEMO-3 (none) went into it. Since its start
/// DEMO-4 (5) and DEMO-5 (none) went in, DEMO-2 came out, and DEMO-1 and
/// DEMO-4 were completed.
fn sprint() -> Env {
    let env = Env::new();
    ok(&env, &["init", "demo", "--key", "DEMO"]);
    ok(
        &env,
        &[
            "cycle",
            "create",
            "Sprint",
            "--start",
            &ago(3),
            "--end",
            &ahead(4),
        ],
    );
    for (title, estimate) in [("One", Some("3")), ("Two", Some("2")), ("Three", None)] {
        let mut args = vec!["issue", "create", "--title", title, "--cycle", "Sprint"];
        if let Some(e) = estimate {
            args.extend(["--estimate", e]);
        }
        ok(&env, &args);
    }
    back_in_time(&env, 7);
    ok(
        &env,
        &[
            "issue",
            "create",
            "--title",
            "Four",
            "--cycle",
            "Sprint",
            "--estimate",
            "5",
        ],
    );
    ok(
        &env,
        &["issue", "create", "--title", "Five", "--cycle", "Sprint"],
    );
    ok(&env, &["issue", "update", "DEMO-2", "--no-cycle"]);
    ok(&env, &["issue", "update", "DEMO-1", "--status", "done"]);
    ok(&env, &["issue", "update", "DEMO-4", "--status", "done"]);
    env
}

#[test]
fn the_report_tells_planned_added_removed_and_completed_from_history() {
    let env = sprint();
    let out = env.lcl(&["cycle", "report", "sprint"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    let dates = format!("{} to {}", ago(3), ahead(4));
    assert_eq!(
        out.stdout,
        format!(
            "Sprint: {dates}\n\
             \x20          Issues  Points\n\
             Planned         3       5\n\
             Added           2       5\n\
             Removed         1       2\n\
             Completed       2       8\n\
             2 issues have no estimate: counted as issues, in no point total.\n\
             \n\
             Velocity  Issues  Points\n\
             Sprint         2       8  {dates}\n"
        )
    );
    let r = report(&env, "Sprint");
    let count = |issues: i64, points: i64| json!({"issues": issues, "points": points});
    assert_eq!(
        (
            &r["scope"],
            &r["scope_unavailable"],
            &r["completed"],
            &r["unestimated"]
        ),
        (
            &json!({"planned": count(3, 5), "added": count(2, 5), "removed": count(1, 2)}),
            &Value::Null,
            &count(2, 8),
            &json!(2)
        )
    );
    // The cycle now holds four issues: what it holds is not what it planned.
    assert_eq!(
        env.lcl(&["--json", "issue", "list", "--cycle", "Sprint"])
            .json()["total"],
        4
    );
}

#[test]
fn trashed_issues_are_left_out() {
    let env = sprint();
    ok(&env, &["issue", "delete", "DEMO-4"]);
    let r = report(&env, "Sprint");
    assert_eq!(
        (&r["scope"]["added"], &r["completed"]),
        (
            &json!({"issues": 1, "points": 0}),
            &json!({"issues": 1, "points": 3})
        )
    );
}

#[test]
fn a_cycle_with_no_start_date_reports_completion_only() {
    let env = Env::new();
    ok(&env, &["init", "demo", "--key", "DEMO"]);
    ok(&env, &["cycle", "create", "Someday"]);
    ok(
        &env,
        &[
            "issue",
            "create",
            "--title",
            "One",
            "--cycle",
            "Someday",
            "--estimate",
            "2",
            "--status",
            "done",
        ],
    );
    ok(
        &env,
        &[
            "issue", "create", "--title", "Two", "--cycle", "Someday", "--status", "done",
        ],
    );
    ok(
        &env,
        &["issue", "create", "--title", "Three", "--cycle", "Someday"],
    );
    let out = env.lcl(&["cycle", "report", "Someday"]);
    assert_eq!(
        out.stdout,
        "Someday: no dates\n\
         Scope change unavailable: the cycle has no start date.\n\
         \x20          Issues  Points\n\
         Completed       2       2\n\
         1 issue has no estimate: counted as an issue, in no point total.\n\
         \n\
         Velocity  Issues  Points\n\
         Someday        2       2  no dates\n"
    );
    let r = report(&env, "someday");
    assert_eq!(
        (&r["scope"], &r["scope_unavailable"]),
        (&Value::Null, &json!("the cycle has no start date"))
    );
}

/// Seven weekly cycles from 2026-01-05, each with issue N completed in it
/// (N points) and, from the fourth, one issue completed after its end.
fn imported() -> Env {
    let env = Env::new();
    ok(&env, &["init", "demo", "--key", "DEMO"]);
    let mut lines =
        vec![r#"{"type":"workspace","format":"lcl-export/1","key":"DEMO"}"#.to_string()];
    let monday = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let noon = |d: NaiveDate| d.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp();
    let mut number = 0;
    for k in 0..7u64 {
        let start = monday + Days::new(7 * k);
        let id = format!("00000000-0000-4000-8000-{:012}", k + 1);
        lines.push(
            json!({"type": "cycle", "id": id, "name": format!("Week {}", k + 1),
                "starts_at": start.to_string(), "ends_at": (start + Days::new(6)).to_string()})
            .to_string(),
        );
        let mut done = vec![(start + Days::new(2), Some(k as i64 + 1))];
        if k >= 3 {
            done.push((start + Days::new(9), Some(1)));
        }
        for (when, estimate) in done {
            number += 1;
            lines.push(
                json!({"type": "issue", "number": number, "title": format!("Issue {number}"),
                    "status": "Done", "created_at": noon(start) - 86_400, "completed_at": noon(when),
                    "estimate": estimate, "cycles": [id]})
                .to_string(),
            );
        }
    }
    let out = env.lcl_stdin(&["import", "-"], (lines.join("\n") + "\n").as_bytes());
    assert_eq!(out.code, 0, "{out:#?}");
    env
}

#[test]
fn velocity_compares_the_five_cycles_before() {
    let env = imported();
    let r = report(&env, "Week 7");
    let velocity: Vec<(&str, i64, i64)> = r["velocity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap(),
                v["completed"]["issues"].as_i64().unwrap(),
                v["completed"]["points"].as_i64().unwrap(),
            )
        })
        .collect();
    // Each completed its own issue; the ones completed after their cycle
    // ended don't count for it.
    assert_eq!(
        velocity,
        [
            ("Week 2", 1, 2),
            ("Week 3", 1, 3),
            ("Week 4", 1, 4),
            ("Week 5", 1, 5),
            ("Week 6", 1, 6),
            ("Week 7", 1, 7),
        ]
    );
    // These cycles held their issues when the import began the history:
    // when they went in is unknown.
    let why = r["scope_unavailable"].as_str().unwrap();
    assert!(
        why.starts_with(&format!(
            "lcl began keeping history on {}, after the cycle started",
            today()
        )) || why.starts_with(&format!(
            "lcl began keeping history on {}, after the cycle started",
            ago(1)
        )),
        "{why}"
    );
    assert_eq!(r["scope"], Value::Null);
    // The first cycle has none before it.
    assert_eq!(
        report(&env, "Week 1")["velocity"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn an_unknown_cycle_is_not_found() {
    let env = Env::new();
    ok(&env, &["init", "demo", "--key", "DEMO"]);
    let out = env.lcl(&["--json", "cycle", "report", "Nope"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("\"kind\":\"not_found\""), "{out:#?}");
}
