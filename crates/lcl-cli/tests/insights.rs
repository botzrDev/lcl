//! `lcl insights`: lead time, cycle time and throughput over a range of
//! days, from the times lcl stamps on issues. The arithmetic is tested in
//! `lcl_core::insights`; these tests run the command on issues imported
//! with known times, each at noon UTC so that every runner's time zone puts
//! it on the same day.

mod common;

use common::Env;
use serde_json::{Value, json};

/// Noon UTC on 2026-11-`day`, in epoch seconds.
fn noon(day: i64) -> i64 {
    1_793_534_400 + (day - 1) * 86_400
}

const SPRINT: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// DEMO-1 to DEMO-6: three completed in 2026-11-02..15 (one with no start
/// time), one in progress, one canceled, and one completed after.
fn demo() -> Env {
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    let issue = |n: i64, status: &str, created: i64, extra: Value| {
        let mut r = json!({"type": "issue", "number": n, "title": format!("issue {n}"),
            "status": status, "created_at": noon(created)});
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        r.to_string()
    };
    let lines = [
        r#"{"type":"workspace","format":"lcl-export/1","key":"DEMO"}"#.to_string(),
        r#"{"type":"label","name":"bug"}"#.to_string(),
        r#"{"type":"project","name":"alpha"}"#.to_string(),
        format!(r#"{{"type":"cycle","id":"{SPRINT}","name":"Sprint"}}"#),
        issue(
            1,
            "Done",
            2,
            json!({"started_at": noon(3), "completed_at": noon(4), "labels": ["bug"],
                "project": "alpha", "cycles": [SPRINT]}),
        ),
        issue(
            2,
            "Done",
            2,
            json!({"completed_at": noon(9), "cycles": [SPRINT]}),
        ),
        issue(
            3,
            "Done",
            3,
            json!({"started_at": noon(5), "completed_at": noon(12)}),
        ),
        issue(4, "InProgress", 4, json!({"started_at": noon(6)})),
        issue(5, "Canceled", 4, json!({"canceled_at": noon(6)})),
        issue(
            6,
            "Done",
            1,
            json!({"started_at": noon(1), "completed_at": noon(20), "project": "alpha"}),
        ),
    ];
    env.lcl_stdin(&["import", "-"], (lines.join("\n") + "\n").as_bytes())
        .assert_ok();
    env
}

const RANGE: [&str; 4] = ["--since", "2026-11-02", "--until", "2026-11-15"];

fn report(env: &Env, filters: &[&str]) -> Value {
    let mut args = vec!["--json", "insights"];
    args.extend(RANGE);
    args.extend(filters);
    let out = env.lcl(&args);
    out.assert_ok();
    out.json()
}

const DAY: i64 = 86_400;

#[test]
fn the_report_counts_what_was_completed_in_the_range() {
    let env = demo();
    let mut args = vec!["insights"];
    args.extend(RANGE);
    let out = env.lcl(&args);
    out.assert_ok();
    assert_eq!(
        out.stdout,
        "2026-11-02 to 2026-11-15, 14 days\n\
         Completed   3, 1.50 a week\n\
         Lead time   median 7d, 90th percentile 8d 14h, of 3 issues\n\
         Cycle time  median 4d, 90th percentile 6d 9h, of 2 issues\n\
         Left out    1 not completed; 1 completed with no start time, in lead time only\n\
         \n\
         Week of     Completed\n\
         2026-11-02  1\n\
         2026-11-09  2\n"
    );
    // The same in seconds: lead times 2, 7 and 9 days; cycle times 1 and 7.
    assert_eq!(
        report(&env, &[]),
        json!({
            "since": "2026-11-02",
            "until": "2026-11-15",
            "completed": 3,
            "per_week": 1.5,
            "weeks": [
                {"week": "2026-11-02", "completed": 1},
                {"week": "2026-11-09", "completed": 2},
            ],
            "lead_time": {"issues": 3, "median": 7 * DAY, "p90": 86 * DAY / 10},
            "cycle_time": {"issues": 2, "median": 4 * DAY, "p90": 64 * DAY / 10},
            "left_out": {"not_completed": 1, "not_started": 1},
        })
    );
}

#[test]
fn project_cycle_and_label_narrow_the_report() {
    let env = demo();
    let counts = |filters: &[&str]| {
        let r = report(&env, filters);
        (
            r["completed"].as_i64().unwrap(),
            r["cycle_time"]["issues"].as_i64().unwrap(),
            r["left_out"]["not_completed"].as_i64().unwrap(),
        )
    };
    // alpha: DEMO-1 (DEMO-6 is completed after the range).
    assert_eq!(counts(&["--project", "ALPHA"]), (1, 1, 0));
    // No project: DEMO-2 and DEMO-3, and DEMO-4 left out.
    assert_eq!(counts(&["--project", "none"]), (2, 1, 1));
    assert_eq!(counts(&["--cycle", "sprint"]), (2, 1, 0));
    assert_eq!(counts(&["--label", "bug"]), (1, 1, 0));
    assert_eq!(counts(&["--label", "bug", "--cycle", "Sprint"]), (1, 1, 0));
    // The range narrows it too: the first week only.
    let out = env.lcl(&[
        "--json",
        "insights",
        "--since",
        "2026-11-02",
        "--until",
        "2026-11-08",
    ]);
    assert_eq!(out.json()["completed"], 1);

    for (flag, value, message) in [
        ("--project", "nope", "project \"nope\" not found"),
        ("--cycle", "nope", "cycle \"nope\" not found"),
        ("--label", "nope", "unknown label \"nope\""),
    ] {
        let out = env.lcl(&["insights", flag, value]);
        assert_eq!(out.code, 1, "{out:#?}");
        assert!(out.stderr.contains(message), "{out:#?}");
    }
}

#[test]
fn canceled_duplicate_and_trashed_issues_are_left_out() {
    let env = demo();
    env.lcl(&["issue", "delete", "DEMO-1"]).assert_ok();
    env.lcl(&["issue", "update", "DEMO-4", "--status", "Duplicate"])
        .assert_ok();
    let r = report(&env, &[]);
    assert_eq!(
        (&r["completed"], &r["left_out"]["not_completed"]),
        (&json!(2), &json!(0))
    );
    env.lcl(&["issue", "restore", "DEMO-1"]).assert_ok();
    assert_eq!(report(&env, &[])["completed"], 3);
}

#[test]
fn an_empty_range_prints_zero_counts_and_exits_0() {
    let env = demo();
    let out = env.lcl(&["insights", "--since", "2026-12-01", "--until", "2026-12-01"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    assert_eq!(
        out.stdout,
        "2026-12-01 to 2026-12-01, 1 day\n\
         Completed   0, 0.00 a week\n\
         Lead time   no issue to measure\n\
         Cycle time  no issue to measure\n\
         Left out    1 not completed; 0 completed with no start time, in lead time only\n\
         \n\
         Week of     Completed\n\
         2026-11-30  0\n"
    );
    let r = env
        .lcl(&[
            "--json",
            "insights",
            "--since",
            "2026-12-01",
            "--until",
            "2026-12-01",
        ])
        .json();
    assert_eq!(
        (&r["completed"], &r["lead_time"], &r["per_week"]),
        (
            &json!(0),
            &json!({"issues": 0, "median": null, "p90": null}),
            &json!(0.0)
        )
    );
    // A workspace with no issues at all.
    let empty = Env::new();
    empty.lcl(&["init", "empty", "--key", "E"]).assert_ok();
    let r = empty.lcl(&["--json", "insights"]).json();
    assert_eq!(
        (&r["completed"], r["weeks"].as_array().unwrap().len() >= 4),
        (&json!(0), true)
    );
}

#[test]
fn a_range_that_ends_before_it_starts_is_refused() {
    let env = demo();
    let out = env.lcl(&["insights", "--since", "2026-11-10", "--until", "2026-11-09"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .starts_with("error: --until 2026-11-09 is before --since 2026-11-10\n"),
        "{out:#?}"
    );
    let out = env.lcl(&["insights", "--since", "9999-12-31"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .starts_with("error: --since 9999-12-31 is after today, "),
        "{out:#?}"
    );
    let out = env.lcl(&["insights", "--since", "2026-02-30"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("expected a real calendar date as YYYY-MM-DD"),
        "{out:#?}"
    );
}

#[test]
fn by_default_the_range_is_the_four_weeks_to_today() {
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    env.lcl(&["issue", "create", "--title", "One"]).assert_ok();
    env.lcl(&["issue", "create", "--title", "Two"]).assert_ok();
    // Stamped by the status changes, as they happen.
    env.lcl(&["issue", "update", "DEMO-1", "--status", "InProgress"])
        .assert_ok();
    env.lcl(&["issue", "update", "DEMO-1", "--status", "Done"])
        .assert_ok();
    env.lcl(&["issue", "update", "DEMO-2", "--status", "Done"])
        .assert_ok();
    let r = env.lcl(&["--json", "insights"]).json();
    let date =
        |v: &Value| chrono::NaiveDate::parse_from_str(v.as_str().unwrap(), "%Y-%m-%d").unwrap();
    let (since, until) = (date(&r["since"]), date(&r["until"]));
    assert_eq!((until - since).num_days(), 27);
    let today = chrono::Local::now().date_naive();
    // The run may straddle midnight.
    assert!((today - until).num_days().abs() <= 1, "{until} {today}");
    assert_eq!(
        (
            &r["completed"],
            &r["cycle_time"]["issues"],
            &r["left_out"]["not_started"]
        ),
        (&json!(2), &json!(1), &json!(1))
    );
    // Only --until: the four weeks to it.
    let r = env
        .lcl(&["--json", "insights", "--until", "2026-11-28"])
        .json();
    assert_eq!(
        (&r["since"], &r["until"]),
        (&json!("2026-11-01"), &json!("2026-11-28"))
    );
    // Reopened: no longer completed, and its completion time is gone.
    env.lcl(&["issue", "update", "DEMO-2", "--status", "Todo"])
        .assert_ok();
    let r = env.lcl(&["--json", "insights"]).json();
    assert_eq!(
        (&r["completed"], &r["left_out"]["not_completed"]),
        (&json!(1), &json!(1))
    );
}
