//! Schema versioning: upgrading a 0.1.0 database, fresh databases, refusing
//! a newer schema, and concurrent first opens.
//!
//! tests/fixtures/v0.sqlite was built with lcl 0.1.0 (~/.local/bin/lcl,
//! byte-identical to a build of commit c699c62), with HOME set to a temp dir,
//! LCL_PROJECT unset, run from an empty temp cwd:
//!
//!     lcl init fixture
//!     lcl issue create --title "Fish tank" --description "The zebrafish need a bigger tank"
//!     lcl issue create --title "Shipped thing" --status Done
//!     lcl issue create --title "Needs a human" --label hitl
//!     lcl issue comment 1 "first comment"
//!     lcl label create hitl
//!     lcl doc create --title Handbook --content "doc body"
//!
//! and then .lcl/data.sqlite was copied to tests/fixtures/v0.sqlite. No -wal or
//! -shm file was left behind. The fixture is at user_version 0 in WAL mode.
//! Tests copy it with `Env::install_db`; never open it in place.

mod common;

use common::{dump, set_user_version, user_version, Env, Out, V0_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Value};
use std::sync::Barrier;

fn issue(list: &Value, number: i64) -> &Value {
    list.as_array()
        .and_then(|a| a.iter().find(|i| i["number"] == number))
        .unwrap_or_else(|| panic!("issue {number} missing from {list:#}"))
}

#[test]
fn upgrades_a_0_1_0_database() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    assert_eq!(user_version(&db), 0, "the fixture should be an unversioned 0.1.0 database");
    let before = dump(&db);

    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list.as_array().map(Vec::len), Some(3), "{list:#}");
    let one = issue(&list, 1);
    assert_eq!(one["title"], "Fish tank");
    assert_eq!(one["description"], "The zebrafish need a bigger tank");
    assert_eq!(one["status"], "Backlog");
    let two = issue(&list, 2);
    assert_eq!(two["title"], "Shipped thing");
    assert_eq!(two["status"], "Done");
    let three = issue(&list, 3);
    assert_eq!(three["title"], "Needs a human");
    assert_eq!(three["labels"], json!(["hitl"]));

    let show = env.lcl(&["issue", "show", "1", "--json"]).assert_ok().json();
    let comments = show["comments"].as_array().expect("comments array");
    assert_eq!(comments.len(), 1, "{show:#}");
    assert_eq!(comments[0]["body"], "first comment");

    let labels = env.lcl(&["label", "list", "--json"]).assert_ok().json();
    assert_eq!(labels.as_array().map(Vec::len), Some(1), "{labels:#}");
    assert_eq!(labels[0]["name"], "hitl");

    let docs = env.lcl(&["doc", "list", "--json"]).assert_ok().json();
    assert_eq!(docs.as_array().map(Vec::len), Some(1), "{docs:#}");
    assert_eq!(docs[0]["title"], "Handbook");
    assert_eq!(docs[0]["content"], "doc body");

    let found = env.lcl(&["search", "zebrafish", "--json"]).assert_ok().json();
    let numbers: Vec<_> = found["issues"].as_array().expect("issues array").iter().map(|i| &i["number"]).collect();
    assert_eq!(numbers, [1], "{found:#}");

    let project = env.lcl(&["project", "show", "--json"]).assert_ok().json();
    assert_eq!(project["schema_version"], SCHEMA_VERSION, "{project:#}");
    assert_eq!(project["key"], "FIXT");

    assert_eq!(user_version(&db), SCHEMA_VERSION);
    // The only change to a 0.1.0 database's data is migration 2's seeded
    // number mark. Every other object and row, FTS index included, is exactly
    // as 0.1.0 left it.
    let mut after = dump(&db);
    let meta = &mut after.tables.iter_mut().find(|(t, _)| t == "meta").expect("meta table").1;
    let seeded = meta
        .iter()
        .position(|row| row[1] == SqlValue::Text("next_issue_number".into()))
        .expect("next_issue_number seeded");
    assert_eq!(meta.remove(seeded)[2], SqlValue::Text("4".into()), "seeded one past the highest number");
    assert_eq!(after, before);
}

#[test]
fn fresh_database_is_at_latest_version() {
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();

    assert_eq!(user_version(&env.db_path()), SCHEMA_VERSION);
    let project = env.lcl(&["project", "show", "--json"]).assert_ok().json();
    assert_eq!(project["schema_version"], SCHEMA_VERSION, "{project:#}");
}

#[test]
fn refuses_a_newer_schema() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    set_user_version(&db, 99);
    let bytes = std::fs::read(&db).expect("read db");

    let out = env.lcl(&["issue", "list"]);
    assert_ne!(out.code, 0, "{out:#?}");
    assert!(out.stderr.contains("v99"), "{out:#?}");
    assert!(out.stderr.contains(&format!("v{SCHEMA_VERSION}")), "{out:#?}");

    // assert! rather than assert_eq!: a mismatch would print both files byte by byte.
    assert!(std::fs::read(&db).expect("read db") == bytes, "lcl wrote to a newer-schema database");
    assert_eq!(user_version(&db), 99);
}

#[test]
fn refuses_a_newer_schema_without_touching_a_rollback_journal_db() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    {
        let conn = rusqlite::Connection::open(&db).expect("open db");
        let mode: String = conn
            .pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))
            .expect("set journal_mode");
        assert_eq!(mode, "delete");
        conn.pragma_update(None, "user_version", 99).expect("set user_version");
    }
    let sidecar = |ext: &str| db.with_file_name(format!("data.sqlite-{ext}"));
    assert!(!sidecar("wal").exists() && !sidecar("shm").exists(), "setup left a -wal or -shm file");
    let bytes = std::fs::read(&db).expect("read db");

    let out = env.lcl(&["issue", "list"]);
    assert_ne!(out.code, 0, "{out:#?}");
    assert!(out.stderr.contains("v99"), "{out:#?}");

    // assert! rather than assert_eq!: a mismatch would print both files byte by byte.
    assert!(std::fs::read(&db).expect("read db") == bytes, "lcl wrote to a newer-schema database");
    assert!(!sidecar("wal").exists(), "lcl left a -wal file");
    assert!(!sidecar("shm").exists(), "lcl left a -shm file");
}

#[test]
fn concurrent_first_open_migrates_once() {
    const PROCS: usize = 12;
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    let barrier = Barrier::new(PROCS);

    let outs: Vec<Out> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..PROCS)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    env.lcl(&["issue", "list", "--json"])
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("thread panicked")).collect()
    });

    for (i, out) in outs.iter().enumerate() {
        assert_eq!(out.code, 0, "process {i} failed: {out:#?}");
        assert_eq!(out.json().as_array().map(Vec::len), Some(3), "process {i}: {out:#?}");
    }
    assert_eq!(user_version(&db), SCHEMA_VERSION);
}
