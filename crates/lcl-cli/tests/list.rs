//! `issue list` (prove-out blocker 7): every filter in SQL before LIMIT, a
//! total that says how many matched, paging, sorting, and a status filter
//! that reads stored values by the same rule as `issue show`.
//!
//! Most tests seed 1,700 issues with SQL. Expected results are computed here
//! from the same rules as the seed (`Seed`), not read back from lcl.

mod common;

use common::{Env, exec};
use serde_json::{Value, json};
use std::time::Instant;

const N: i64 = 1700;

/// What the seed gives issue `x` (1..=1700).
struct Seed;
impl Seed {
    /// 1,440 Done, 160 Canceled, 100 Todo, interleaved.
    fn status(x: i64) -> &'static str {
        match x % 85 {
            0..72 => "Done",
            72..80 => "Canceled",
            _ => "Todo",
        }
    }
    /// 160 for austin, all at or below 1600: older than the newest 100. 170
    /// for cleetus; the other 1,370 unassigned.
    fn assignee(x: i64) -> Option<&'static str> {
        match x % 10 {
            0 if x <= 1600 => Some("austin"),
            5 => Some("cleetus"),
            _ => None,
        }
    }
    /// Stored 0 None, 1 Urgent, 2 High, 3 Medium, 4 Low.
    fn priority(x: i64) -> i64 {
        x % 5
    }
    /// Pairs share a value, and the order is not the numbers' order.
    fn created(x: i64) -> i64 {
        1000 + (x * 7919) % 850
    }
    /// A permutation of 5000..6700.
    fn updated(x: i64) -> i64 {
        5000 + (x * 104_729) % 1700
    }
    fn labels(x: i64) -> Vec<&'static str> {
        [("a", x % 2 == 0), ("b", x % 3 == 0)]
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(l, _)| l)
            .collect()
    }
    /// Issues 2..=5 are sub-issues of issue 1.
    fn parent(x: i64) -> Option<i64> {
        (2..=5).contains(&x).then_some(1)
    }
    /// The numbers matching `keep`, newest first.
    fn newest_first(keep: impl Fn(i64) -> bool) -> Vec<i64> {
        (1..=N).rev().filter(|&x| keep(x)).collect()
    }
}

fn uuid(x: i64) -> String {
    format!("00000000-0000-4000-8000-{x:012}")
}

/// A workspace (key DREA) holding the seeded 1,700 issues, labels a and b.
fn seeded() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    for l in ["a", "b"] {
        env.lcl(&["label", "create", l]).assert_ok();
    }
    let labels = env.lcl(&["label", "list", "--json"]).assert_ok().json();
    let id = |name: &str| {
        labels
            .as_array()
            .expect("labels")
            .iter()
            .find(|l| l["name"] == name)
            .expect("label")["id"]
            .as_str()
            .expect("id")
            .to_string()
    };
    let (a, b) = (id("a"), id("b"));
    let q = |s: Option<&str>| s.map_or("NULL".to_string(), |s| format!("'{s}'"));

    let mut sql = String::from("BEGIN;\n");
    for x in 1..=N {
        sql.push_str(&format!(
            "INSERT INTO issues(id, number, title, description, status, priority, assignee, parent_id, created_at, updated_at) VALUES ('{}', {x}, 'Seeded issue {x}', '', '{}', {}, {}, {}, {}, {});\n",
            uuid(x),
            Seed::status(x),
            Seed::priority(x),
            q(Seed::assignee(x)),
            q(Seed::parent(x).map(uuid).as_deref()),
            Seed::created(x),
            Seed::updated(x),
        ));
        for l in Seed::labels(x) {
            sql.push_str(&format!(
                "INSERT INTO issue_labels(issue_id, label_id) VALUES ('{}', '{}');\n",
                uuid(x),
                if l == "a" { &a } else { &b }
            ));
        }
    }
    sql.push_str("COMMIT;");
    exec(&env.db_path(), &sql);
    env
}

fn list(env: &Env, args: &[&str]) -> Value {
    let mut cmd = vec!["issue", "list", "--json"];
    cmd.extend(args);
    env.lcl(&cmd).assert_ok().json()
}

fn numbers(v: &Value) -> Vec<i64> {
    v["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("no issues: {v:#}"))
        .iter()
        .map(|i| i["number"].as_i64().expect("number"))
        .collect()
}

/// total, limit, offset, truncated.
fn shape(v: &Value) -> (Value, Value, Value, Value) {
    (
        v["total"].clone(),
        v["limit"].clone(),
        v["offset"].clone(),
        v["truncated"].clone(),
    )
}

#[test]
fn a_status_filter_counts_everything_and_says_it_truncated() {
    let env = seeded();
    let done = Seed::newest_first(|x| Seed::status(x) == "Done");
    assert_eq!(done.len(), 1440);

    let page = list(&env, &["--status", "Done"]);
    assert_eq!(
        shape(&page),
        (json!(1440), json!(50), json!(0), json!(true))
    );
    assert_eq!(numbers(&page), done[..50]);

    let human = env
        .lcl(&["issue", "list", "--status", "Done"])
        .assert_ok()
        .stdout
        .clone();
    let lines: Vec<&str> = human.lines().collect();
    assert_eq!(lines.len(), 51, "{human}");
    assert!(
        lines[..50]
            .iter()
            .all(|l| l.starts_with("DREA-") && l.contains("\t[Done]\t")),
        "{human}"
    );
    assert_eq!(
        lines[50],
        "showing 1–50 of 1440; use --all, or --limit/--offset"
    );

    let human = env
        .lcl(&["issue", "list", "--status", "Done", "--offset", "50"])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        human.ends_with("\nshowing 51–100 of 1440; use --all, or --limit/--offset\n"),
        "{human}"
    );
    assert_eq!(
        human.lines().next(),
        Some(
            format!(
                "DREA-{}\t[Done]\t{}\tSeeded issue {}",
                done[50],
                ["None", "Urgent", "High", "Medium", "Low"][Seed::priority(done[50]) as usize],
                done[50]
            )
            .as_str()
        )
    );

    // A last page that reaches the end isn't truncated, and says nothing.
    let human = env
        .lcl(&["issue", "list", "--status", "Done", "--offset", "1400"])
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(human.lines().count(), 40, "{human}");
    assert!(!human.contains("showing"), "{human}");
}

/// The assignee filter runs before the limit: all 160 of austin's issues are
/// older than the newest 100, so filtering a page of 50 would find none.
#[test]
fn the_assignee_filter_runs_before_the_limit() {
    let env = seeded();
    let austin = Seed::newest_first(|x| Seed::assignee(x) == Some("austin"));
    assert_eq!(austin.len(), 160);
    assert!(austin.iter().all(|&x| x <= N - 100));

    let page = list(&env, &["--assignee", "austin"]);
    assert_eq!(shape(&page), (json!(160), json!(50), json!(0), json!(true)));
    assert_eq!(numbers(&page), austin[..50]);
    assert!(
        page["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .all(|i| i["assignee"] == "austin"),
        "{page:#}"
    );
    // ASCII case is ignored, and the name is trimmed.
    assert_eq!(list(&env, &["--assignee", "AUSTIN"]), page);
    assert_eq!(list(&env, &["--assignee", " Austin "]), page);

    let unassigned = Seed::newest_first(|x| Seed::assignee(x).is_none());
    assert_eq!(unassigned.len(), 1370);
    for none in ["none", "NONE"] {
        let page = list(&env, &["--assignee", none, "--all"]);
        assert_eq!(
            shape(&page),
            (json!(1370), Value::Null, json!(0), json!(false)),
            "{none}"
        );
        assert_eq!(numbers(&page), unassigned, "{none}");
        assert!(
            page["issues"]
                .as_array()
                .expect("issues")
                .iter()
                .all(|i| i["assignee"].is_null()),
            "{none}"
        );
    }
    let out = env.lcl(&["issue", "list", "--assignee", "  "]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("for unassigned issues, use --assignee none"),
        "{out:#?}"
    );
}

/// Repeated statuses OR; every other filter ANDs with them.
#[test]
fn statuses_or_and_other_filters_and() {
    let env = seeded();
    let done_or_canceled = Seed::newest_first(|x| matches!(Seed::status(x), "Done" | "Canceled"));
    assert_eq!(done_or_canceled.len(), 1600);
    let page = list(&env, &["--status", "Done", "--status", "Canceled", "--all"]);
    assert_eq!(page["total"], 1600);
    assert_eq!(numbers(&page), done_or_canceled);

    let want = Seed::newest_first(|x| {
        matches!(Seed::status(x), "Canceled" | "Todo")
            && Seed::assignee(x) == Some("cleetus")
            && Seed::labels(x).contains(&"b")
    });
    assert!(!want.is_empty());
    let page = list(
        &env,
        &[
            "--status",
            "canceled",
            "--status",
            "TODO",
            "--assignee",
            "cleetus",
            "--label",
            "b",
            "--all",
        ],
    );
    assert_eq!(numbers(&page), want);
    assert_eq!(page["total"], want.len());

    let out = env.lcl(&["issue", "list", "--status", "Finished"]);
    assert_eq!(out.code, 2, "{out:#?}");
}

/// Repeated labels AND: an issue must have every one.
#[test]
fn labels_and() {
    let env = seeded();
    let both = Seed::newest_first(|x| Seed::labels(x) == ["a", "b"]);
    assert_eq!(both.len(), 283);
    let page = list(&env, &["--label", "a", "--label", "b", "--all"]);
    assert_eq!(page["total"], 283);
    assert_eq!(numbers(&page), both);
    assert!(
        page["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .all(|i| i["labels"] == json!(["a", "b"])),
        "{page:#}"
    );

    // Names match ignoring case; one label alone.
    let a = Seed::newest_first(|x| Seed::labels(x).contains(&"a"));
    let page = list(&env, &["--label", "A", "--limit", "100"]);
    assert_eq!(page["total"], a.len());
    assert_eq!(numbers(&page), a[..100]);

    let out = env.lcl(&["issue", "list", "--label", "a", "--label", "nope"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("unknown label \"nope\""), "{out:#?}");
}

#[test]
fn the_parent_filter() {
    let env = seeded();
    assert_eq!(numbers(&list(&env, &["--parent", "DREA-1"])), [5, 4, 3, 2]);
    let want = Seed::newest_first(|x| Seed::parent(x) == Some(1) && Seed::status(x) == "Done");
    assert_eq!(
        numbers(&list(&env, &["--parent", "#1", "--status", "Done"])),
        want
    );
    assert_eq!(list(&env, &["--parent", "DREA-2"])["total"], 0);
    let out = env.lcl(&["issue", "list", "--parent", "DREA-9999"]);
    assert_ne!(out.code, 0, "{out:#?}");
    assert!(out.stderr.contains("issue DREA-9999 not found"), "{out:#?}");
}

/// Each sort, over every issue, against the order computed from the seed.
/// Ties go to the higher number.
#[test]
fn every_sort() {
    let env = seeded();
    let by = |key: &dyn Fn(i64) -> i64| -> Vec<i64> {
        let mut xs: Vec<i64> = (1..=N).collect();
        xs.sort_by(|&a, &b| key(a).cmp(&key(b)).then(b.cmp(&a)));
        xs
    };
    let rank = |x: i64| match Seed::priority(x) {
        0 => 5,
        p => p,
    };
    for (sort, want) in [
        ("number", by(&|x| -x)),
        ("created", by(&|x| -Seed::created(x))),
        ("updated", by(&|x| -Seed::updated(x))),
        ("priority", by(&rank)),
        ("PRIORITY", by(&rank)),
    ] {
        let page = list(&env, &["--sort", sort, "--all"]);
        assert_eq!(numbers(&page), want, "--sort {sort}");
    }
    // The default is number, newest first.
    assert_eq!(
        numbers(&list(&env, &[])),
        (N - 49..=N).rev().collect::<Vec<_>>()
    );

    let page = list(&env, &["--sort", "priority", "--all"]);
    let priorities: Vec<&str> = page["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| i["priority"].as_str().expect("priority"))
        .collect();
    let mut runs = priorities.clone();
    runs.dedup();
    assert_eq!(
        runs,
        ["Urgent", "High", "Medium", "Low", "None"],
        "None sorts last"
    );
    assert_eq!(priorities.iter().filter(|p| **p == "None").count(), 340);

    let out = env.lcl(&["issue", "list", "--sort", "title"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("expected one of: number, created, updated, priority"),
        "{out:#?}"
    );
}

#[test]
fn offset_paging_visits_every_issue_once() {
    let env = seeded();
    // 17 full pages, then an empty one. Bounded: a page that ignored the
    // offset would otherwise never come back empty.
    let mut seen = vec![];
    for offset in (0..=N).step_by(100) {
        let page = list(&env, &["--limit", "100", "--offset", &offset.to_string()]);
        assert_eq!(
            (&page["total"], &page["limit"], &page["offset"]),
            (&json!(N), &json!(100), &json!(offset))
        );
        assert_eq!(
            page["truncated"],
            json!(offset + 100 < N),
            "offset {offset}"
        );
        let got = numbers(&page);
        assert_eq!(
            got.len(),
            if offset < N { 100 } else { 0 },
            "offset {offset}"
        );
        seen.extend(got);
    }
    assert_eq!(
        seen,
        (1..=N).rev().collect::<Vec<_>>(),
        "every issue once, in order"
    );

    // Past the end: an empty page, not an error.
    let page = list(&env, &["--offset", "5000"]);
    assert_eq!(
        (numbers(&page), shape(&page)),
        (vec![], (json!(N), json!(50), json!(5000), json!(false)))
    );
    assert_eq!(
        env.lcl(&["issue", "list", "--offset", "5000"])
            .assert_ok()
            .stdout,
        ""
    );
}

#[test]
fn all_limit_and_offset_rules() {
    let env = seeded();
    let page = list(&env, &["--all"]);
    assert_eq!(
        shape(&page),
        (json!(N), Value::Null, json!(0), json!(false))
    );
    assert_eq!(numbers(&page).len(), N as usize);
    assert!(
        !env.lcl(&["issue", "list", "--all"])
            .assert_ok()
            .stdout
            .contains("showing")
    );

    for args in [
        vec!["issue", "list", "--all", "--limit", "5"],
        vec!["issue", "list", "--all", "--offset", "5"],
        vec!["issue", "list", "--limit", "0"],
        vec!["issue", "list", "--limit=-1"],
        vec!["issue", "list", "--offset=-1"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert_eq!(out.stdout, "", "{args:?}");
    }
}

/// A status written by hand in another spelling still reads as that status
/// in `issue show`. `--status` compares the stored text with the canonical
/// spelling, so the hand edit is not selected until a write stores it.
/// A value that names no status makes the unfiltered list fail.
#[test]
fn a_hand_edited_status_matches_its_filter() {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    for t in [
        "hand done",
        "hand in progress",
        "real done",
        "unknown",
        "todo",
    ] {
        env.lcl(&["issue", "create", "--title", t]).assert_ok();
    }
    env.lcl(&["issue", "update", "3", "--status", "Done"])
        .assert_ok();
    exec(
        &env.db_path(),
        "PRAGMA ignore_check_constraints = ON;
         UPDATE issues SET status = 'done' WHERE number = 1;
         UPDATE issues SET status = 'in_progress ' WHERE number = 2;
         UPDATE issues SET status = 'finished' WHERE number = 4;",
    );
    assert_eq!(
        env.lcl(&["issue", "show", "1", "--json"])
            .assert_ok()
            .json()["issue"]["status"],
        "Done"
    );
    assert_eq!(numbers(&list(&env, &["--status", "Done"])), [3]);
    assert_eq!(
        numbers(&list(&env, &["--status", "in progress"])),
        Vec::<i64>::new()
    );
    assert_eq!(
        numbers(&list(&env, &["--status", "Todo", "--status", "Backlog"])),
        [5]
    );
    // Listing issue 4 itself still reports its status.
    let out = env.lcl(&["issue", "list"]);
    assert_ne!(out.code, 0, "{out:#?}");
    assert!(
        out.stderr
            .contains("issue 4 has status \"finished\", which this lcl doesn't know"),
        "{out:#?}"
    );
}

/// Every issue with its labels. Prints the time; release-build timings go
/// in the step report.
#[test]
fn listing_everything_with_labels() {
    let env = seeded();
    let started = Instant::now();
    let page = list(&env, &["--all"]);
    eprintln!(
        "issue list --all --json over 1,700 issues took {:?} (this build)",
        started.elapsed()
    );
    let issues = page["issues"].as_array().expect("issues");
    assert_eq!(issues.len(), N as usize);
    for i in issues {
        let x = i["number"].as_i64().expect("number");
        assert_eq!(i["labels"], json!(Seed::labels(x)), "issue {x}");
        assert_eq!(i["assignee"], json!(Seed::assignee(x)), "issue {x}");
        assert_eq!(i["status"], Seed::status(x), "issue {x}");
        assert_eq!(
            i["parent_identifier"],
            json!(Seed::parent(x).map(|p| format!("DREA-{p}"))),
            "issue {x}"
        );
    }
}
