//! Lifecycle timestamps. started_at, completed_at and canceled_at follow
//! status moves, on create and on update alike: both go through the one rule
//! function, `Issue::apply_lifecycle`.
//!
//! Timestamps have one-second resolution, so "kept, not re-stamped" can't be
//! seen by timing. Instead a test plants a sentinel value directly in the row
//! (111, 222, 333) and checks whether it survives the next move.

mod common;

use common::{exec, Env};
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
enum T {
    /// null
    Unset,
    /// equal to the issue's `at` field (created_at or updated_at): stamped now
    Now,
    /// a planted sentinel that must survive
    Kept(i64),
}

#[track_caller]
fn check(issue: &Value, at: &str, step: &str, started: T, completed: T, canceled: T) {
    for (field, want) in [("started_at", started), ("completed_at", completed), ("canceled_at", canceled)] {
        let got = &issue[field];
        let ok = match want {
            T::Unset => got.is_null(),
            T::Now => !got.is_null() && *got == issue[at],
            T::Kept(v) => *got == v,
        };
        assert!(ok, "{step}: {field} should be {want:?}, got {got} ({at} = {})\n{issue:#}", issue[at]);
    }
}

#[test]
fn create_stamps_the_initial_status() {
    use T::*;
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();
    let cases = [
        ("Backlog", Unset, Unset, Unset),
        ("Todo", Unset, Unset, Unset),
        ("InProgress", Now, Unset, Unset),
        ("InReview", Now, Unset, Unset),
        ("Done", Unset, Now, Unset),
        ("Canceled", Unset, Unset, Now),
        ("Duplicate", Unset, Unset, Now),
    ];
    for (status, started, completed, canceled) in cases {
        let issue = env.lcl(&["issue", "create", "--title", status, "--status", status, "--json"]).assert_ok().json();
        check(&issue, "created_at", &format!("create --status {status}"), started, completed, canceled);
        // And what was stored is what was printed.
        let shown = env.lcl(&["issue", "show", &issue["number"].to_string(), "--json"]).assert_ok().json();
        check(&shown["issue"], "created_at", &format!("show after create --status {status}"), started, completed, canceled);
    }
}

#[test]
fn update_follows_every_transition() {
    use T::*;
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();
    assert_eq!(env.create("moving"), 1);
    let db = env.db_path();
    let plant = |field: &str, v: i64| exec(&db, &format!("UPDATE issues SET {field} = {v} WHERE number = 1"));
    let update = |args: &[&str]| {
        let mut cmd = vec!["issue", "update", "1", "--json"];
        cmd.extend(args);
        env.lcl(&cmd).assert_ok().json()
    };

    let i = update(&["--status", "InProgress"]);
    check(&i, "updated_at", "Backlog -> InProgress sets started_at", Now, Unset, Unset);
    plant("started_at", 111);

    let i = update(&["--status", "InReview"]);
    check(&i, "updated_at", "InProgress -> InReview keeps started_at", Kept(111), Unset, Unset);

    let i = update(&["--status", "Done"]);
    check(&i, "updated_at", "InReview -> Done sets completed_at", Kept(111), Now, Unset);
    plant("completed_at", 222);

    let i = update(&["--title", "renamed"]);
    check(&i, "updated_at", "an edit that keeps Done changes nothing", Kept(111), Kept(222), Unset);
    let i = update(&["--status", "Done"]);
    check(&i, "updated_at", "Done -> Done changes nothing", Kept(111), Kept(222), Unset);

    let i = update(&["--status", "InProgress"]);
    check(&i, "updated_at", "Done -> InProgress clears completed_at, keeps started_at", Kept(111), Unset, Unset);

    let i = update(&["--status", "Canceled"]);
    check(&i, "updated_at", "InProgress -> Canceled sets canceled_at", Kept(111), Unset, Now);
    plant("canceled_at", 333);

    let i = update(&["--status", "Duplicate"]);
    check(&i, "updated_at", "Canceled -> Duplicate keeps canceled_at", Kept(111), Unset, Kept(333));

    let i = update(&["--status", "Todo"]);
    check(&i, "updated_at", "Duplicate -> Todo clears canceled_at and started_at", Unset, Unset, Unset);

    let i = update(&["--status", "InReview"]);
    check(&i, "updated_at", "Todo -> InReview sets started_at", Now, Unset, Unset);
    let i = update(&["--status", "Backlog"]);
    check(&i, "updated_at", "InReview -> Backlog clears started_at", Unset, Unset, Unset);

    let i = update(&["--status", "Done"]);
    check(&i, "updated_at", "Backlog -> Done sets completed_at only", Unset, Now, Unset);
    let i = update(&["--status", "Canceled"]);
    check(&i, "updated_at", "Done -> Canceled swaps completed_at for canceled_at", Unset, Unset, Now);
    let i = update(&["--status", "Done"]);
    check(&i, "updated_at", "Canceled -> Done swaps canceled_at for completed_at", Unset, Now, Unset);

    // What was stored is what update printed.
    let shown = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json();
    check(&shown["issue"], "updated_at", "show after the last update", Unset, Now, Unset);
}
