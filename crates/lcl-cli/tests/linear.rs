//! `lcl import --linear TEAM`: a Linear team's issues, fetched from Linear's
//! API, converted and imported in one command. These run against the
//! stand-in in tests/linear_service; the conversion itself is tested in
//! `lcl_core::linear`.

mod common;
mod linear_service;

use common::{Env, error_text};
use linear_service::{Linear, comment, issue};
use serde_json::{Value, json};

/// Team SMM, paged two at a time: five issues, one in Linear's trash and
/// one in a cycle. SMM-1 has five comments and three labels, so both page
/// past the first; its project has three documents.
fn team() -> Linear {
    let linear = Linear::start(
        "SMM",
        vec![
            issue(
                "SMM-1",
                json!({
                    "title": "Batch the posts",
                    "description": "All of them.",
                    "priority": 2,
                    "estimate": 3,
                    "state": {"name": "In Review", "type": "started"},
                    "assignee": {"name": "P. Example", "displayName": "pat"},
                    "project": {"name": "Launch"},
                    "labels": [{"name": "content"}, {"name": "social"}, {"name": "urgent"}],
                    "comments": [
                        comment("five", "2026-11-20T15:00:00.000Z"),
                        comment("one", "2026-11-20T11:00:00.000Z"),
                        comment("two", "2026-11-20T12:00:00.000Z"),
                        comment("three", "2026-11-20T13:00:00.000Z"),
                        comment("four", "2026-11-20T14:00:00.000Z"),
                    ],
                    "relations": [{"type": "blocks", "relatedIssue": {"identifier": "SMM-2"}}],
                }),
            ),
            issue(
                "SMM-2",
                json!({"state": {"name": "Done", "type": "completed"},
                       "completedAt": "2026-11-21T09:00:00.000Z",
                       "parent": {"identifier": "SMM-1"}}),
            ),
            issue("SMM-3", json!({"trashed": true})),
            issue("SMM-4", json!({"cycle": {"id": "cycle-1"}})),
            issue("SMM-5", json!({})),
        ],
        2,
    );
    linear.state().projects = vec![json!({
        "id": "project-1",
        "documents": [{"id": "d1"}, {"id": "d2"}, {"id": "d3"}],
    })];
    linear
}

fn workspace(key: &str) -> Env {
    let env = Env::new();
    env.lcl(&["init", "smm", "--key", key]).assert_ok();
    env
}

const NOTES: &str = "Notes:\n  \
    1 issue in Linear's trash is left out\n  \
    1 issue is in a Linear cycle: cycles are not converted\n  \
    3 documents are not converted\n";

#[test]
fn a_team_fills_an_empty_workspace_page_by_page() {
    let linear = team();
    let env = workspace("SMM");
    let out = linear.lcl(&env, &["import", "--linear", "smm"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    assert_eq!(
        out.stdout,
        format!(
            "Imported 4 issues, 5 comments, 1 relation, 3 labels, 1 project, 0 milestones, 0 cycles, 0 docs into workspace smm (key SMM); next number SMM-6\n{NOTES}"
        )
    );
    // Every page was fetched: three of issues, the rest of SMM-1's labels
    // and comments, and the rest of the documents.
    assert_eq!(
        linear.state().queries,
        [
            "Team",
            "Issues",
            "Issues",
            "Issues",
            "Rest",
            "Rest",
            "Rest",
            "Projects",
            "Documents"
        ]
    );
    let shown = env
        .lcl(&["--json", "issue", "show", "SMM-1"])
        .assert_ok()
        .json();
    let one = &shown["issue"];
    assert_eq!(
        (
            &one["title"],
            &one["status"],
            &one["priority"],
            &one["assignee"],
            &one["project"],
            &one["labels"]
        ),
        (
            &json!("Batch the posts"),
            &json!("InReview"),
            &json!("High"),
            &json!("P. Example"),
            &json!("Launch"),
            &json!(["content", "social", "urgent"])
        ),
        "{one:#}"
    );
    let bodies: Vec<&Value> = shown["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["body"])
        .collect();
    assert_eq!(bodies, ["one", "two", "three", "four", "five"]);
    let two = &env
        .lcl(&["--json", "issue", "show", "SMM-2"])
        .assert_ok()
        .json()["issue"];
    assert_eq!(
        (&two["status"], &two["parent_identifier"]),
        (&json!("Done"), &json!("SMM-1"))
    );
    // SMM-3 was in the trash: not brought in.
    assert_eq!(env.lcl(&["issue", "show", "SMM-3"]).code, 1);
}

#[test]
fn a_dry_run_fetches_and_checks_but_writes_nothing() {
    let linear = team();
    let env = workspace("SMM");
    let out = linear.lcl(&env, &["import", "--linear", "SMM", "--dry-run"]);
    assert_eq!(
        out.stdout,
        format!(
            "Dry run, nothing written: would import 4 issues, 5 comments, 1 relation, 3 labels, 1 project, 0 milestones, 0 cycles, 0 docs into workspace smm (key SMM); next number SMM-6\n{NOTES}"
        ),
        "{out:#?}"
    );
    assert_eq!(
        env.lcl(&["--json", "issue", "list"]).assert_ok().json()["total"],
        0
    );
    // A real import follows it.
    linear.lcl(&env, &["import", "--linear", "SMM"]).assert_ok();
}

#[test]
fn with_json_the_notes_come_with_the_counts() {
    let linear = team();
    let env = workspace("SMM");
    let out = linear
        .lcl(&env, &["--json", "import", "--linear", "SMM"])
        .assert_ok()
        .json();
    assert_eq!(
        (&out["issues"], &out["dry_run"], &out["next_issue_number"]),
        (&json!(4), &json!(false), &json!(6))
    );
    assert_eq!(
        out["notes"],
        json!([
            "1 issue in Linear's trash is left out",
            "1 issue is in a Linear cycle: cycles are not converted",
            "3 documents are not converted"
        ])
    );
}

#[test]
fn under_another_key_the_linear_identifier_is_the_ref() {
    let linear = team();
    let env = workspace("DEMO");
    linear.lcl(&env, &["import", "--linear", "SMM"]).assert_ok();
    let one = env
        .lcl(&["--json", "issue", "show", "DEMO-1"])
        .assert_ok()
        .json();
    assert_eq!(one["issue"]["ref"], "SMM-1");
    // The old identifier points to it.
    let out = env.lcl(&["issue", "show", "SMM-1"]);
    assert_eq!(
        out.stderr,
        "Error: SMM-1 is not in this workspace (key DEMO); DEMO-1 has ref SMM-1\n"
    );
}

#[test]
fn a_value_lcl_refuses_names_its_linear_issue_and_writes_nothing() {
    let linear = Linear::start(
        "SMM",
        vec![
            issue("SMM-1", json!({})),
            issue("SMM-2", json!({"estimate": 0.5})),
            issue(
                "SMM-3",
                json!({"state": {"name": "Gone", "type": "archived"}}),
            ),
        ],
        50,
    );
    let env = workspace("SMM");
    let args = ["import", "--linear", "SMM"];
    let out = linear.lcl(&env, &args);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            2,
            "",
            error_text(
                &args,
                "usage",
                "can't import from Linear: 2 problems; nothing was imported:\n  \
                 SMM-2: estimate: 0.5 isn't a whole number; lcl estimates are whole points\n  \
                 SMM-3: state: \"Gone\" has type \"archived\", which lcl doesn't know"
            )
            .as_str()
        )
    );
    assert_eq!(
        env.lcl(&["--json", "issue", "list"]).assert_ok().json()["total"],
        0
    );
}

#[test]
fn a_missing_or_refused_key_is_an_auth_error() {
    let linear = team();
    let env = workspace("SMM");
    let args = ["--json", "import", "--linear", "SMM"];
    let out = linear.lcl_with_key(&env, None, &args);
    assert_eq!(
        (out.code, out.stderr),
        (
            1,
            error_text(
                &args,
                "auth",
                "set LINEAR_API_KEY to a personal API key from Linear's settings"
            )
        )
    );
    assert!(linear.state().queries.is_empty());
    let out = linear.lcl_with_key(&env, Some("lin_api_wrong"), &args);
    assert_eq!(
        (out.code, out.stderr),
        (
            1,
            error_text(
                &args,
                "auth",
                "Linear refused the API key in LINEAR_API_KEY: Authentication required, not authenticated"
            )
        )
    );
}

#[test]
fn an_unknown_team_is_not_found() {
    let linear = team();
    let env = workspace("SMM");
    let args = ["--json", "import", "--linear", "ENG"];
    let out = linear.lcl(&env, &args);
    assert_eq!(
        (out.code, out.stderr),
        (
            1,
            error_text(
                &args,
                "not_found",
                "Linear has no team with key \"ENG\" that this API key can see"
            )
        )
    );
}

#[test]
fn a_workspace_that_is_not_empty_is_refused_before_linear_is_asked() {
    let linear = team();
    let env = workspace("SMM");
    env.create("already here");
    let out = linear.lcl(&env, &["--json", "import", "--linear", "SMM"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("\"kind\":\"conflict\""), "{out:#?}");
    assert!(linear.state().queries.is_empty());
}

#[test]
fn a_path_or_linear_but_not_both() {
    let linear = team();
    let env = workspace("SMM");
    std::fs::write(env.cwd.path().join("f.jsonl"), "").unwrap();
    for args in [
        &["import", "f.jsonl", "--linear", "SMM"][..],
        &["import"][..],
    ] {
        let out = linear.lcl(&env, args);
        assert_eq!(out.code, 2, "{args:?}\n{out:#?}");
    }
    assert!(linear.state().queries.is_empty());
}

#[test]
fn an_address_that_is_not_https_is_refused() {
    let env = workspace("SMM");
    let args = ["--json", "import", "--linear", "SMM"];
    let out = env.lcl_env(
        &[
            ("LCL_LINEAR_API", "http://api.example.com/graphql"),
            ("LINEAR_API_KEY", "k"),
        ],
        &args,
    );
    assert_eq!(
        (out.code, out.stderr),
        (
            2,
            error_text(
                &args,
                "usage",
                "\"http://api.example.com/graphql\" is not a Linear API address: give https://HOST/PATH (http:// only for localhost)"
            )
        )
    );
}
