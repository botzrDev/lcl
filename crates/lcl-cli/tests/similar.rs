//! Similar issues: `lcl issue create` lists the open issues like the one it
//! created, and `lcl issue similar` runs the same check without writing.
//! The matching rules are tested in `lcl_core::similar`; these tests run
//! the commands.

mod common;

use common::{Env, exec};
use serde_json::{Value, json};
use std::time::Instant;

fn demo() -> Env {
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    env
}

fn create(env: &Env, args: &[&str]) -> Value {
    let mut all = vec!["--json", "issue", "create"];
    all.extend(args);
    let out = env.lcl(&all);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    out.json()
}

fn ids(similar: &Value) -> Vec<&str> {
    similar
        .as_array()
        .expect("an array")
        .iter()
        .map(|i| i["identifier"].as_str().unwrap())
        .collect()
}

/// DEMO-1 to DEMO-7 about logging in, in every status, and DEMO-8 in the
/// trash.
fn login_issues() -> Env {
    let env = demo();
    for (title, status) in [
        ("Login redirect loops", "Todo"),
        ("Redirect after login is slow", "Backlog"),
        ("Login redirect breaks on Safari", "In Progress"),
        ("Login redirect drops the query", "In Review"),
        ("Login redirect loop on Firefox", "Done"),
        ("Login redirect is wrong", "Canceled"),
        ("Login redirects twice", "Duplicate"),
        ("Login redirect in the trash", "Todo"),
    ] {
        create(&env, &["--title", title, "--status", status]);
    }
    env.lcl(&["issue", "delete", "DEMO-8"]).assert_ok();
    env
}

#[test]
fn issue_create_lists_similar_open_issues_best_first() {
    let env = login_issues();
    let made = create(&env, &["--title", "The login redirect loops again"]);
    // Done, Canceled, Duplicate and trashed issues are not suggested. DEMO-3
    // and DEMO-4 are as good as each other: the newer first.
    assert_eq!(
        ids(&made["similar"]),
        ["DEMO-1", "DEMO-4", "DEMO-3", "DEMO-2"]
    );
    assert_eq!(
        made["similar"][0],
        json!({"identifier": "DEMO-1", "status": "Todo", "title": "Login redirect loops"})
    );
    // As text: after the line it printed before, one issue a line.
    let out = env.lcl(&["issue", "create", "--title", "Safari login redirect"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    assert_eq!(
        out.stdout,
        "Created DEMO-10\n\
         Similar open issues:\n\
         DEMO-3\t[InProgress]\tLogin redirect breaks on Safari\n\
         DEMO-1\t[Todo]\tLogin redirect loops\n\
         DEMO-9\t[Backlog]\tThe login redirect loops again\n\
         DEMO-4\t[InReview]\tLogin redirect drops the query\n\
         DEMO-2\t[Backlog]\tRedirect after login is slow\n"
    );
    // Nothing similar: the line it printed before, alone.
    let out = env.lcl(&["issue", "create", "--title", "Dark mode"]);
    assert_eq!(out.stdout, "Created DEMO-11\n");
}

#[test]
fn the_check_never_changes_the_create() {
    let fields = [
        "--description",
        "Seen twice",
        "--priority",
        "high",
        "--assignee",
        "ann",
    ];
    let mut with = vec!["--title", "Login redirect loops"];
    with.extend(fields);
    // The same create where it finds issues and where it finds none.
    let busy = login_issues();
    let quiet = demo();
    for _ in 0..8 {
        create(&quiet, &["--title", "Chore"]);
    }
    let a = create(&busy, &with);
    let b = create(&quiet, &with);
    assert_eq!((ids(&a["similar"]).len(), ids(&b["similar"]).len()), (4, 0));
    let shown = |env: &Env| env.lcl(&["--json", "issue", "show", "DEMO-9"]).json()["issue"].clone();
    for (made, env) in [(&a, &busy), (&b, &quiet)] {
        let mut issue = made.clone();
        issue.as_object_mut().unwrap().remove("similar");
        // What create printed, less `similar`, is the issue as stored.
        assert_eq!(issue, shown(env));
        assert_eq!(
            (
                &issue["identifier"],
                &issue["priority"],
                &issue["assignee"],
                &issue["description"]
            ),
            (
                &json!("DEMO-9"),
                &json!("High"),
                &json!("ann"),
                &json!("Seen twice")
            )
        );
    }
}

#[test]
fn issue_similar_checks_a_title_and_writes_nothing() {
    let env = login_issues();
    let before = env.lcl(&["export"]).stdout;
    let out = env.lcl(&[
        "--json",
        "issue",
        "similar",
        "--title",
        "Login redirect loops",
    ]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    let found = out.json();
    assert_eq!(found["words"], json!(["login", "redirect", "loops"]));
    assert_eq!(
        ids(&found["issues"]),
        ["DEMO-1", "DEMO-4", "DEMO-3", "DEMO-2"]
    );
    // A description ranks what the title finds; from a file or stdin too.
    let out = env.lcl_stdin(
        &[
            "--json",
            "issue",
            "similar",
            "--title",
            "Login redirect",
            "--description-file",
            "-",
        ],
        b"Only on Firefox and Safari",
    );
    assert_eq!(ids(&out.json()["issues"])[0], "DEMO-3");
    // Nothing was written.
    assert_eq!(env.lcl(&["export"]).stdout, before);

    let out = env.lcl(&["issue", "similar", "--title", "Login redirect loops"]);
    assert_eq!(
        out.stdout,
        "DEMO-1\t[Todo]\tLogin redirect loops\n\
         DEMO-4\t[InReview]\tLogin redirect drops the query\n\
         DEMO-3\t[InProgress]\tLogin redirect breaks on Safari\n\
         DEMO-2\t[Backlog]\tRedirect after login is slow\n"
    );
    let out = env.lcl(&["issue", "similar", "--title", "Dark mode"]);
    assert_eq!(out.stdout, "No similar open issues.\n");
    let out = env.lcl(&["issue", "similar", "--title", "Fix the bug"]);
    assert_eq!(
        out.stdout,
        "No similar open issues: every word of the title is too common to compare.\n"
    );
    let out = env.lcl(&["issue", "similar", "--title", "  "]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(out.stderr.contains("a title can't be blank"), "{out:#?}");
}

#[test]
fn a_title_of_common_words_finds_nothing() {
    let env = demo();
    for title in ["Fix the bug", "Update it", "Add a new one"] {
        create(&env, &["--title", title]);
    }
    let made = create(&env, &["--title", "Fix the bug in the update"]);
    assert_eq!(made["similar"], json!([]));
    let found = env
        .lcl(&["--json", "issue", "similar", "--title", "Fix the bug"])
        .json();
    assert_eq!(found, json!({"words": [], "issues": []}));
}

/// 20,000 issues, seeded with SQL: each title three words, and each
/// description five, from about a thousand per place, and "Seeded" in
/// every title, too common to compare. Each description also has kappaN,
/// lambdaN and muN, each in 1,500 to 1,820 issues: as common as a word can
/// be and still be compared, so a title of them is the slowest to check.
fn twenty_thousand() -> Env {
    let env = demo();
    exec(
        &env.db_path(),
        "BEGIN;
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 20000)
         INSERT INTO issues(id, number, title, description, status, priority, created_at, updated_at)
         SELECT printf('00000000-0000-4000-8000-%012d', x), x,
                printf('Seeded alpha%d beta%d gamma%d', x * 7 % 997, x * 13 % 1009, x * 17 % 1013),
                printf('delta%d epsilon%d zeta%d eta%d theta%d kappa%d lambda%d mu%d', x % 991, x * 3 % 983, x * 5 % 977, x * 11 % 971, x * 19 % 967, x % 11, x % 12, x % 13),
                CASE x % 4 WHEN 0 THEN 'Done' ELSE 'Todo' END, 0, 1, 1 FROM n;
         UPDATE meta SET value = '20001' WHERE key = 'next_issue_number';
         COMMIT;",
    );
    env
}

/// The budget recorded in LOCL-33: a create over 20,000 issues, with the
/// check, within 100 ms in a release build (measured: 43 to 52 ms, the
/// check at most 8 ms of it). Here, in a debug build on any CI runner, it
/// fails only past 2 s: a runaway query, not a slow runner. Prints the
/// times.
#[test]
fn a_create_in_a_workspace_of_20000_issues() {
    let env = twenty_thousand();
    // Warm the file, as a second command in a row would find it.
    env.lcl(&["issue", "list", "--limit", "1"]).assert_ok();
    let started = Instant::now();
    let made = create(
        &env,
        &[
            "--title",
            "Seeded alpha7 beta13 crash",
            "--description",
            "delta1 epsilon3",
        ],
    );
    let with_matches = started.elapsed();
    let started = Instant::now();
    let none = create(&env, &["--title", "Nothing like the others"]);
    let without = started.elapsed();
    let started = Instant::now();
    let checked = env.lcl(&[
        "--json",
        "issue",
        "similar",
        "--title",
        "Seeded alpha7 beta13 crash",
    ]);
    let check = started.elapsed();
    let started = Instant::now();
    let slowest = create(&env, &["--title", "kappa1 lambda1 mu1"]);
    let worst = started.elapsed();
    let started = Instant::now();
    env.lcl(&["issue", "show", "DEMO-1"]).assert_ok();
    let floor = started.elapsed();
    eprintln!(
        "issue create over 20,000 issues: {with_matches:?} with similar issues, {without:?} without, {worst:?} with common words; issue similar alone: {check:?}; issue show: {floor:?} (this build)"
    );
    assert_eq!(ids(&slowest["similar"]).len(), 5);
    for took in [with_matches, without, worst] {
        assert!(took.as_secs_f64() < 2.0, "a create took {took:?}");
    }
    assert_eq!(made["number"], 20001);
    assert_eq!(none["number"], 20002);
    assert_eq!(none["similar"], json!([]));
    let found = ids(&made["similar"]);
    assert!(!found.is_empty() && found.len() <= 5, "{made:#}");
    assert_eq!(
        checked.json()["words"],
        json!(["alpha7", "beta13", "crash"])
    );
}
