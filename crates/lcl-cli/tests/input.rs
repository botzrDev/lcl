//! Strict input. Statuses, priorities, dates, estimates and colors are checked
//! by clap's value parsers before any database is opened: a bad value exits 2
//! and changes nothing. Values are passed as `--flag=value` so that empty and
//! hyphen-leading values reach the parser.

mod common;

use common::{Env, exec, issue_row};

/// (spelling, canonical status it must parse to)
const STATUS_OK: &[(&str, &str)] = &[
    ("Backlog", "Backlog"),
    ("backlog", "Backlog"),
    ("BACKLOG", "Backlog"),
    ("Todo", "Todo"),
    ("TODO", "Todo"),
    ("to-do", "Todo"),
    ("InProgress", "InProgress"),
    ("In Progress", "InProgress"),
    ("in_progress", "InProgress"),
    ("IN-PROGRESS", "InProgress"),
    ("inprogress", "InProgress"),
    ("InReview", "InReview"),
    ("in review", "InReview"),
    ("In-Review", "InReview"),
    ("Done", "Done"),
    ("done", "Done"),
    ("DONE", "Done"),
    ("Canceled", "Canceled"),
    ("canceled", "Canceled"),
    ("cancelled", "Canceled"),
    ("Cancelled", "Canceled"),
    ("Duplicate", "Duplicate"),
    ("duplicate", "Duplicate"),
];
const STATUS_BAD: &[&str] = &[
    "Finished",
    "Finshed",
    "",
    "Started",
    "Closed",
    "Dupe",
    "In Progres",
    "done!",
    "Backlog2",
];

#[test]
fn status_spellings_through_create_update_and_list() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    assert_eq!(env.create("update target"), 1);

    for &(spelling, canonical) in STATUS_OK {
        let flag = format!("--status={spelling}");
        let created = env
            .lcl(&["issue", "create", "--title", "t", &flag, "--json"])
            .assert_ok()
            .json();
        assert_eq!(created["status"], canonical, "create {flag:?}");

        let updated = env
            .lcl(&["issue", "update", "1", &flag, "--json"])
            .assert_ok()
            .json();
        assert_eq!(updated["status"], canonical, "update {flag:?}");

        let listed = env
            .lcl(&["issue", "list", &flag, "--all", "--json"])
            .assert_ok()
            .json();
        let listed = listed["issues"].as_array().expect("array");
        assert!(
            listed.iter().all(|i| i["status"] == canonical),
            "list {flag:?} returned other statuses: {listed:#?}"
        );
        assert!(
            listed.iter().any(|i| i["number"] == created["number"]),
            "list {flag:?} missed the issue just created"
        );
    }

    for &bad in STATUS_BAD {
        let flag = format!("--status={bad}");
        let message = format!(
            "invalid status \"{bad}\"; expected one of: Backlog, Todo, InProgress, InReview, Done, Canceled, Duplicate"
        );
        for args in [
            vec!["issue", "create", "--title", "t", &flag],
            vec!["issue", "update", "1", &flag],
            vec!["issue", "list", &flag],
        ] {
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&message), "{args:?}: {out:#?}");
        }
    }
}

#[test]
fn priority_spellings_through_create_and_update() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    assert_eq!(env.create("update target"), 1);

    let ok = [
        ("0", "None"),
        ("1", "Urgent"),
        ("2", "High"),
        ("3", "Medium"),
        ("4", "Low"),
        ("none", "None"),
        ("NONE", "None"),
        ("urgent", "Urgent"),
        ("Urgent", "Urgent"),
        ("high", "High"),
        ("HIGH", "High"),
        ("Medium", "Medium"),
        ("low", "Low"),
        ("LoW", "Low"),
    ];
    for (spelling, label) in ok {
        let flag = format!("--priority={spelling}");
        let created = env
            .lcl(&["issue", "create", "--title", "t", &flag, "--json"])
            .assert_ok()
            .json();
        assert_eq!(created["priority"], label, "create {flag:?}");
        let updated = env
            .lcl(&["issue", "update", "1", &flag, "--json"])
            .assert_ok()
            .json();
        assert_eq!(updated["priority"], label, "update {flag:?}");
    }

    for bad in [
        "5", "9", "-1", "", "01", "1.0", "high!", " high", "critical",
    ] {
        let flag = format!("--priority={bad}");
        let message = format!(
            "invalid priority \"{bad}\"; expected 0-4 or one of: none, urgent, high, medium, low"
        );
        for args in [
            vec!["issue", "create", "--title", "t", &flag],
            vec!["issue", "update", "1", &flag],
        ] {
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&message), "{args:?}: {out:#?}");
        }
    }
}

#[test]
fn a_rejected_update_changes_nothing() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "keep me",
        "--status",
        "todo",
        "--priority",
        "2",
        "--due",
        "2026-05-01",
        "--estimate",
        "3",
    ])
    .assert_ok();
    let db = env.db_path();
    // A sentinel: any write through update_issue would move updated_at to now.
    exec(&db, "UPDATE issues SET updated_at = 1000 WHERE number = 1");
    let before = issue_row(&db, 1);

    for args in [
        vec!["--status=Finished"],
        vec!["--priority=9"],
        vec!["--due=2026-02-30"],
        vec!["--estimate=-1"],
        // A valid change in the same command must not be applied either.
        vec!["--title", "changed", "--status=nope"],
    ] {
        let mut cmd = vec!["issue", "update", "1"];
        cmd.extend(&args);
        let out = env.lcl(&cmd);
        assert_eq!(out.code, 2, "{cmd:?}: {out:#?}");
        assert_eq!(issue_row(&db, 1), before, "{cmd:?} changed the row");
    }
}

const DATE_BAD: &[&str] = &[
    "2026-02-30",
    "2025-02-29",
    "2026-13-01",
    "2026-00-10",
    "2026-04-31",
    "2026-2-3",
    "2026/02/03",
    "20260203",
    "next tuesday",
    "",
    "2026-02-03T00:00",
    " 2026-02-03",
    "+2026-02-03",
];

#[test]
fn dates_must_be_real_yyyy_mm_dd() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    for good in ["2026-02-28", "2024-02-29", "2026-12-31"] {
        let created = env
            .lcl(&[
                "issue",
                "create",
                "--title",
                "t",
                &format!("--due={good}"),
                "--json",
            ])
            .assert_ok()
            .json();
        assert_eq!(created["due_date"], good);
    }
    let updated = env
        .lcl(&["issue", "update", "1", "--due=2027-01-15", "--json"])
        .assert_ok()
        .json();
    assert_eq!(updated["due_date"], "2027-01-15");

    for &bad in DATE_BAD {
        let message =
            format!("invalid date \"{bad}\"; expected a real calendar date as YYYY-MM-DD");
        for args in [
            vec![
                "issue".to_string(),
                "create".into(),
                "--title".into(),
                "t".into(),
                format!("--due={bad}"),
            ],
            vec![
                "issue".to_string(),
                "update".into(),
                "1".into(),
                format!("--due={bad}"),
            ],
            vec![
                "cycle".to_string(),
                "create".into(),
                "c".into(),
                format!("--start={bad}"),
            ],
            vec![
                "cycle".to_string(),
                "create".into(),
                "c".into(),
                format!("--end={bad}"),
            ],
        ] {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&message), "{args:?}: {out:#?}");
        }
    }
    let cycles = env.lcl(&["cycle", "list", "--json"]).assert_ok().json();
    assert_eq!(
        cycles.as_array().map(Vec::len),
        Some(0),
        "a rejected cycle was stored: {cycles:#}"
    );
}

#[test]
fn a_cycle_cannot_end_before_it_starts() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    let out = env.lcl(&[
        "cycle",
        "create",
        "c",
        "--start",
        "2026-03-01",
        "--end",
        "2026-02-28",
    ]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("cycle end 2026-02-28 is before its start 2026-03-01"),
        "{out:#?}"
    );

    for args in [
        vec![
            "cycle",
            "create",
            "same day",
            "--start",
            "2026-03-01",
            "--end",
            "2026-03-01",
            "--json",
        ],
        vec![
            "cycle",
            "create",
            "start only",
            "--start",
            "2026-03-01",
            "--json",
        ],
        vec![
            "cycle",
            "create",
            "end only",
            "--end",
            "2026-03-01",
            "--json",
        ],
    ] {
        env.lcl(&args).assert_ok();
    }
    let cycles = env.lcl(&["cycle", "list", "--json"]).assert_ok().json();
    assert_eq!(cycles.as_array().map(Vec::len), Some(3), "{cycles:#}");
}

#[test]
fn estimates_cannot_be_negative() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    for good in ["0", "5"] {
        let created = env
            .lcl(&[
                "issue",
                "create",
                "--title",
                "t",
                &format!("--estimate={good}"),
                "--json",
            ])
            .assert_ok()
            .json();
        assert_eq!(created["estimate"].to_string(), good);
    }
    for args in [
        vec!["issue", "create", "--title", "t", "--estimate=-1"],
        vec!["issue", "create", "--title", "t", "--estimate", "-1"],
        vec!["issue", "update", "1", "--estimate=-3"],
        vec!["issue", "create", "--title", "t", "--estimate=lots"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
    }
    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list["issues"].as_array().map(Vec::len), Some(2), "{list:#}");
}

#[test]
fn label_colors_must_be_hex() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    for (name, good) in [("a", "#abc"), ("b", "#A1B2C3"), ("c", "#888888")] {
        let label = env
            .lcl(&[
                "label",
                "create",
                name,
                &format!("--color={good}"),
                "--json",
            ])
            .assert_ok()
            .json();
        assert_eq!(label["color"], good);
    }
    for bad in [
        "abc", "#abcd", "#ggg", "red", "", "#12345", "#1234567", "##abc", "#abc ",
    ] {
        let out = env.lcl(&["label", "create", "l", &format!("--color={bad}")]);
        assert_eq!(out.code, 2, "{bad:?}: {out:#?}");
        assert!(
            out.stderr.contains(&format!(
                "invalid color \"{bad}\"; expected #rgb or #rrggbb"
            )),
            "{bad:?}: {out:#?}"
        );
    }
    let labels = env.lcl(&["label", "list", "--json"]).assert_ok().json();
    assert_eq!(labels.as_array().map(Vec::len), Some(3), "{labels:#}");
}

#[test]
fn an_unknown_stored_status_is_an_error_not_backlog() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    env.create("hand-edited");
    env.create("untouched");
    let db = env.db_path();
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'Triage' WHERE number = 1",
    );
    let before = issue_row(&db, 1);

    for args in [
        vec!["issue", "show", "1"],
        vec!["issue", "list"],
        vec!["issue", "update", "1", "--title", "renamed"],
        vec!["issue", "comment", "1", "hello"],
    ] {
        let out = env.lcl(&args);
        assert_ne!(out.code, 0, "{args:?}: {out:#?}");
        assert!(
            out.stderr.contains("issue 1 has status \"Triage\""),
            "{args:?}: {out:#?}"
        );
    }
    // Nothing read it as Backlog and wrote that back.
    assert_eq!(issue_row(&db, 1), before);
    env.lcl(&["issue", "show", "2"]).assert_ok();
}

#[test]
fn a_stored_status_reads_leniently() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    for t in ["writer one", "writer two", "writer three"] {
        env.create(t);
    }
    let db = env.db_path();
    // Hand edits in non-canonical spellings parse reads the same as input.
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'done' WHERE number = 1; UPDATE issues SET status = 'in progress' WHERE number = 2; UPDATE issues SET status = 'cancelled' WHERE number = 3;",
    );
    let want = [(1, "Done"), (2, "InProgress"), (3, "Canceled")];

    for (n, status) in want {
        let shown = env
            .lcl(&["issue", "show", &n.to_string(), "--json"])
            .assert_ok()
            .json();
        assert_eq!(shown["issue"]["status"], status, "show {n}");
    }
    for args in [
        vec!["issue", "list", "--all", "--json"],
        vec!["search", "writer", "--json"],
    ] {
        let out = env.lcl(&args).assert_ok().json();
        let issues = out["issues"]
            .as_array()
            .unwrap_or_else(|| panic!("{args:?}: {out:#}"))
            .clone();
        assert_eq!(issues.len(), 3, "{args:?}: {out:#}");
        for (n, status) in want {
            let i = issues
                .iter()
                .find(|i| i["number"] == n)
                .unwrap_or_else(|| panic!("{args:?} missing {n}"));
            assert_eq!(i["status"], status, "{args:?} issue {n}");
        }
    }
    // A write stores the canonical spelling. Done -> Done is no transition,
    // so the lifecycle columns stay as they were.
    let updated = env
        .lcl(&["issue", "update", "1", "--title", "renamed", "--json"])
        .assert_ok()
        .json();
    assert_eq!(updated["status"], "Done");
    assert_eq!(
        issue_row(&db, 1)["status"],
        rusqlite::types::Value::Text("Done".into())
    );
    assert!(updated["completed_at"].is_null(), "{updated:#}");
}

#[test]
fn an_unknown_stored_priority_is_an_error_not_none() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    env.create("hand-edited");
    env.create("untouched");
    let db = env.db_path();

    for bad in [9, -1, 5] {
        exec(
            &db,
            &format!("UPDATE issues SET priority = {bad} WHERE number = 1"),
        );
        let before = issue_row(&db, 1);
        for args in [
            vec!["issue", "show", "1"],
            vec!["issue", "list"],
            vec!["search", "hand"],
            vec!["issue", "update", "1", "--title", "renamed"],
            vec!["issue", "comment", "1", "hello"],
        ] {
            let out = env.lcl(&args);
            assert_ne!(out.code, 0, "priority {bad}, {args:?}: {out:#?}");
            assert!(
                out.stderr.contains(&format!(
                    "issue 1 has priority {bad}, which this lcl doesn't know"
                )),
                "{args:?}: {out:#?}"
            );
        }
        // Nothing read it as None and wrote that back.
        assert_eq!(issue_row(&db, 1), before);
    }
    env.lcl(&["issue", "show", "2"]).assert_ok();
}
