//! Concurrent writers, as separate lcl processes. Every write runs in one
//! BEGIN IMMEDIATE transaction that also covers the reads it depends on, and
//! writers queue on the lock (busy_timeout) instead of failing.

mod common;

use common::{argv, Env};

#[test]
fn parallel_creates_all_succeed_with_distinct_numbers() {
    const PROCS: i64 = 30;
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();

    let cmds: Vec<_> = (1..=PROCS).map(|i| argv(&["issue", "create", "--title", &format!("par {i}"), "--json"])).collect();
    let outs = env.lcl_parallel(&cmds);
    let mut numbers: Vec<i64> = outs
        .iter()
        .enumerate()
        .map(|(i, out)| {
            assert_eq!(out.code, 0, "process {i} failed: {out:#?}");
            out.json()["number"].as_i64().unwrap_or_else(|| panic!("process {i}: no number in {out:#?}"))
        })
        .collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=PROCS).collect::<Vec<_>>());

    // The database agrees with what the processes reported.
    let list = env.lcl(&["issue", "list", "--limit", "1000", "--json"]).assert_ok().json();
    let mut stored: Vec<i64> = list.as_array().expect("array").iter().map(|i| i["number"].as_i64().expect("number")).collect();
    stored.sort_unstable();
    assert_eq!(stored, (1..=PROCS).collect::<Vec<_>>());
}

/// The prove-out report's blocker 2b: on 0.1.0, three parallel updates to
/// different fields lost at least one of them in 60 of 60 rounds.
#[test]
fn concurrent_updates_lose_nothing() {
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();
    assert_eq!(env.create("t0"), 1);
    let updates = [
        argv(&["issue", "update", "1", "--status", "Done"]),
        argv(&["issue", "update", "1", "--assignee", "cleetus"]),
        argv(&["issue", "update", "1", "--title", "renamed"]),
    ];

    for round in 1..=40 {
        env.lcl(&["issue", "update", "1", "--status", "Backlog", "--assignee", "nobody", "--title", "t0"]).assert_ok();
        for (i, out) in env.lcl_parallel(&updates).iter().enumerate() {
            assert_eq!(out.code, 0, "round {round}, update {i} failed: {out:#?}");
        }
        let issue = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json()["issue"].clone();
        assert_eq!(
            (issue["status"].as_str(), issue["assignee"].as_str(), issue["title"].as_str()),
            (Some("Done"), Some("cleetus"), Some("renamed")),
            "round {round} lost a write: {issue:#}"
        );
    }
}

#[test]
fn concurrent_comments_and_updates_all_land() {
    const EACH: usize = 20;
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();
    assert_eq!(env.create("busy issue"), 1);

    let mut cmds = Vec::new();
    for i in 0..EACH {
        cmds.push(argv(&["issue", "comment", "1", &format!("comment {i}")]));
        cmds.push(argv(&["issue", "update", "1", "--title", &format!("title {i}")]));
    }
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "process {i} ({:?}) failed: {out:#?}", cmds[i]);
    }

    let comments = env.lcl(&["issue", "comments", "1", "--json"]).assert_ok().json();
    let mut bodies: Vec<&str> = comments.as_array().expect("array").iter().map(|c| c["body"].as_str().expect("body")).collect();
    bodies.sort_unstable();
    let mut expected: Vec<String> = (0..EACH).map(|i| format!("comment {i}")).collect();
    expected.sort_unstable();
    assert_eq!(bodies, expected);

    let issue = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json();
    let title = issue["issue"]["title"].as_str().expect("title");
    assert!(title.starts_with("title "), "title is not one of the written ones: {title:?}");
}
