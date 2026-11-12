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

use common::{dump, foreign_key_violations, fts_integrity, FTS_TABLES, set_user_version, undo_v5_schema, undo_v6_schema, undo_v7_schema, user_version, Env, Out, V0_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Value};
use std::sync::Barrier;

fn issue(list: &Value, number: i64) -> &Value {
    list["issues"].as_array()
        .and_then(|a| a.iter().find(|i| i["number"] == number))
        .unwrap_or_else(|| panic!("issue {number} missing from {list:#}"))
}

#[test]
fn upgrades_a_0_1_0_database() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    assert_eq!(user_version(&db), 0, "the fixture should be an unversioned 0.1.0 database");
    let mut before = dump(&db);

    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list["issues"].as_array().map(Vec::len), Some(3), "{list:#}");
    let one = issue(&list, 1);
    assert_eq!(one["title"], "Fish tank");
    assert_eq!(one["description"], "The zebrafish need a bigger tank");
    assert_eq!(one["status"], "Backlog");
    let two = issue(&list, 2);
    assert_eq!(two["title"], "Shipped thing");
    assert_eq!(two["status"], "Done");
    assert_eq!(two["completed_at"], two["updated_at"], "v3 backfills completed_at from updated_at: {two:#}");
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

    // Each migration's change to a 0.1.0 database, and nothing else. Undo the
    // expected changes on `after`; what's left must equal `before` exactly.
    let text = |s: &str| SqlValue::Text(s.into());
    let mut after = dump(&db);
    // v2: the number mark, seeded one past the highest number.
    let meta = after.tables.get_mut("meta").expect("meta table");
    let seeded = meta.iter().position(|r| r["key"] == text("next_issue_number")).expect("next_issue_number seeded");
    assert_eq!(meta.remove(seeded)["value"], text("4"), "seeded one past the highest number");
    // v3: two new columns, unset (the fixture has no Canceled issue), and the
    // Done issue's completed_at backfilled from its updated_at.
    for row in after.tables.get_mut("issues").expect("issues table") {
        assert_eq!(row.remove("started_at"), Some(SqlValue::Null), "{row:?}");
        assert_eq!(row.remove("canceled_at"), Some(SqlValue::Null), "{row:?}");
        if row["status"] == text("Done") {
            assert_eq!(row["completed_at"], row["updated_at"], "Done issue not backfilled: {row:?}");
            row.insert("completed_at".into(), SqlValue::Null); // as 0.1.0 left it
        }
    }
    // v5: issue_labels rebuilt with foreign keys, and 0.1.0's raw name "hitl"
    // mapped to the hitl label's id. No assignee was blank.
    let hitl = after.tables["labels"].iter().find(|r| r["name"] == text("hitl")).expect("hitl label")["id"].clone();
    for row in after.tables.get_mut("issue_labels").expect("issue_labels table") {
        assert_eq!(row["label_id"], hitl, "{row:?}");
        row.insert("label_id".into(), text("hitl")); // as 0.1.0 left it
    }
    undo_v5_schema(&mut after, &mut before);
    undo_v6_schema(&mut after);
    undo_v7_schema(&mut after);
    assert_eq!(foreign_key_violations(&db), vec![]);
    // v3's ADD COLUMN rewrote the issues table's CREATE statement, and its
    // backfill UPDATE fired the FTS trigger, which rewrote that row's index
    // entries. Check those with FTS5's own integrity-check instead.
    let issues_sql = after.schema.iter().find(|r| r["name"] == text("issues")).map(|r| format!("{:?}", r["sql"]));
    assert!(issues_sql.as_deref().is_some_and(|s| s.contains("started_at") && s.contains("canceled_at")), "{issues_sql:?}");
    for d in [&mut before, &mut after] {
        d.schema.retain(|r| r["name"] != text("issues"));
        d.tables.retain(|t, _| !t.starts_with("issues_fts"));
    }
    assert_eq!(after, before);
    fts_integrity(&db, &FTS_TABLES);
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
        assert_eq!(out.json()["issues"].as_array().map(Vec::len), Some(3), "process {i}: {out:#?}");
    }
    assert_eq!(user_version(&db), SCHEMA_VERSION);
}
