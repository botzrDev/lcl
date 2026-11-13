//! Concurrent writers, as separate lcl processes. Every write runs in one
//! BEGIN IMMEDIATE transaction that also covers the reads it depends on, and
//! writers queue on the lock (busy_timeout) instead of failing.

mod common;

use common::{Env, Out, argv};
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

/// Holds `db`'s write lock (BEGIN IMMEDIATE on a plain connection) for
/// `hold`, and runs `f` once it is taken. Returns what `f` returned and how
/// long after the lock was taken `f` finished.
fn while_locked(db: &Path, hold: Duration, f: impl FnOnce() -> Out) -> (Out, Duration) {
    let (locked, taken) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        let holder = s.spawn(move || {
            let conn = Connection::open(db).expect("open db");
            conn.execute_batch("BEGIN IMMEDIATE")
                .expect("take the write lock");
            locked.send(Instant::now()).expect("signal");
            std::thread::sleep(hold);
            conn.execute_batch("COMMIT")
                .expect("release the write lock");
        });
        let at = taken.recv().expect("lock taken");
        let out = f();
        let took = at.elapsed();
        holder.join().expect("holder thread");
        (out, took)
    })
}

/// A write that meets a lock held for 7 s (a stall of a few seconds is
/// ordinary on a machine busy with builds) waits it out, and lands.
#[test]
fn a_write_waits_out_a_seven_second_lock() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    env.create("one");
    let (out, took) = while_locked(&env.db_path(), Duration::from_secs(7), || {
        env.lcl(&["issue", "comment", "1", "x"])
    });
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "Comment added\n", ""),
        "after {took:?}"
    );
    assert!(took >= Duration::from_secs(7), "{took:?}");
    let comments = env
        .lcl(&["issue", "comments", "1", "--json"])
        .assert_ok()
        .json();
    let bodies: Vec<_> = comments
        .as_array()
        .expect("comments")
        .iter()
        .map(|c| c["body"].clone())
        .collect();
    assert_eq!(bodies, [json!("x")]);
}

/// The same for the registry's index.
#[test]
fn workspace_create_waits_out_a_seven_second_lock_on_the_index() {
    let env = Env::new();
    env.lcl(&["workspace", "create", "first", "--key", "FIR"])
        .assert_ok();
    let index = env.home.path().join(".lcl").join("index.sqlite");
    let (out, took) = while_locked(&index, Duration::from_secs(7), || {
        env.lcl(&["workspace", "create", "second", "--key", "SEC", "--json"])
    });
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (0, ""),
        "after {took:?}: {out:#?}"
    );
    assert!(took >= Duration::from_secs(7), "{took:?}");
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    let names: Vec<_> = list
        .as_array()
        .expect("workspaces")
        .iter()
        .map(|w| (w["name"].clone(), w["missing"].clone()))
        .collect();
    assert_eq!(
        names,
        [
            (json!("first"), json!(false)),
            (json!("second"), json!(false))
        ]
    );
}

#[test]
fn parallel_creates_all_succeed_with_distinct_numbers() {
    const PROCS: i64 = 30;
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();

    let cmds: Vec<_> = (1..=PROCS)
        .map(|i| argv(&["issue", "create", "--title", &format!("par {i}"), "--json"]))
        .collect();
    let outs = env.lcl_parallel(&cmds);
    let mut numbers: Vec<i64> = outs
        .iter()
        .enumerate()
        .map(|(i, out)| {
            assert_eq!(out.code, 0, "process {i} failed: {out:#?}");
            out.json()["number"]
                .as_i64()
                .unwrap_or_else(|| panic!("process {i}: no number in {out:#?}"))
        })
        .collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=PROCS).collect::<Vec<_>>());

    // The database agrees with what the processes reported.
    let list = env
        .lcl(&["issue", "list", "--all", "--json"])
        .assert_ok()
        .json();
    let mut stored: Vec<i64> = list["issues"]
        .as_array()
        .expect("array")
        .iter()
        .map(|i| i["number"].as_i64().expect("number"))
        .collect();
    stored.sort_unstable();
    assert_eq!(stored, (1..=PROCS).collect::<Vec<_>>());
}

/// The prove-out report's blocker 2b: on 0.1.0, three parallel updates to
/// different fields lost at least one of them in 60 of 60 rounds.
#[test]
fn concurrent_updates_lose_nothing() {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    assert_eq!(env.create("t0"), 1);
    let updates = [
        argv(&["issue", "update", "1", "--status", "Done"]),
        argv(&["issue", "update", "1", "--assignee", "cleetus"]),
        argv(&["issue", "update", "1", "--title", "renamed"]),
    ];

    for round in 1..=40 {
        env.lcl(&[
            "issue",
            "update",
            "1",
            "--status",
            "Backlog",
            "--assignee",
            "nobody",
            "--title",
            "t0",
        ])
        .assert_ok();
        for (i, out) in env.lcl_parallel(&updates).iter().enumerate() {
            assert_eq!(out.code, 0, "round {round}, update {i} failed: {out:#?}");
        }
        let issue = env
            .lcl(&["issue", "show", "1", "--json"])
            .assert_ok()
            .json()["issue"]
            .clone();
        assert_eq!(
            (
                issue["status"].as_str(),
                issue["assignee"].as_str(),
                issue["title"].as_str()
            ),
            (Some("Done"), Some("cleetus"), Some("renamed")),
            "round {round} lost a write: {issue:#}"
        );
    }
}

#[test]
fn concurrent_comments_and_updates_all_land() {
    const EACH: usize = 20;
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    assert_eq!(env.create("busy issue"), 1);

    let mut cmds = Vec::new();
    for i in 0..EACH {
        cmds.push(argv(&["issue", "comment", "1", &format!("comment {i}")]));
        cmds.push(argv(&[
            "issue",
            "update",
            "1",
            "--title",
            &format!("title {i}"),
        ]));
    }
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "process {i} ({:?}) failed: {out:#?}", cmds[i]);
    }

    let comments = env
        .lcl(&["issue", "comments", "1", "--json"])
        .assert_ok()
        .json();
    let mut bodies: Vec<&str> = comments
        .as_array()
        .expect("array")
        .iter()
        .map(|c| c["body"].as_str().expect("body"))
        .collect();
    bodies.sort_unstable();
    let mut expected: Vec<String> = (0..EACH).map(|i| format!("comment {i}")).collect();
    expected.sort_unstable();
    assert_eq!(bodies, expected);

    let issue = env
        .lcl(&["issue", "show", "1", "--json"])
        .assert_ok()
        .json();
    let title = issue["issue"]["title"].as_str().expect("title");
    assert!(
        title.starts_with("title "),
        "title is not one of the written ones: {title:?}"
    );
}

/// Label edits read the issue's set inside the write transaction, so 20
/// parallel `--add-label` calls with distinct labels all land.
#[test]
fn parallel_add_labels_all_land() {
    const PROCS: usize = 20;
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    let names: Vec<String> = (0..PROCS).map(|i| format!("l{i:02}")).collect();
    for n in &names {
        env.lcl(&["label", "create", n]).assert_ok();
    }
    assert_eq!(env.create("target"), 1);

    let cmds: Vec<_> = names
        .iter()
        .map(|n| argv(&["issue", "update", "1", "--add-label", n]))
        .collect();
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "process {i} failed: {out:#?}");
    }
    let issue = env
        .lcl(&["issue", "show", "1", "--json"])
        .assert_ok()
        .json()["issue"]
        .clone();
    assert_eq!(
        issue["labels"],
        serde_json::json!(names),
        "lost a label: {issue:#}"
    );
}
