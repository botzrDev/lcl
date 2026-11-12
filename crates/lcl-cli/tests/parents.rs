//! Parents. `--parent` takes any issue id and is stored as the parent's UUID.
//! The issues must stay a tree: a missing parent exits 1, and the issue
//! itself or anything below it exits 2. Also `--no-parent`, `list --parent`,
//! sub-issues in `show`, `delete` refusing an issue that has sub-issues, and
//! migration 4's repair of the parent values 0.1.0 stored as typed.
//!
//! tests/fixtures/v0-parents.sqlite was built like v0.sqlite (see
//! tests/schema.rs): lcl 0.1.0 at ~/.local/bin/lcl (sha256 1b42efae...),
//! HOME set to a temp dir, LCL_PROJECT unset, run with `sh -e` from an empty
//! temp cwd:
//!
//!     uuid() { lcl --json issue show "$1" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["issue"]["id"])'; }
//!     lcl init fixture
//!     lcl issue create --title "Root"
//!     U1=$(uuid 1)
//!     lcl issue create --title "By number" --parent 1
//!     lcl issue create --title "By uuid" --parent "$U1"
//!     lcl issue create --title "By key" --parent FIXT-1
//!     lcl issue create --title "Missing number" --parent 99999
//!     lcl issue create --title "Garbage" --parent garbage
//!     lcl issue create --title "By hash" --parent '#2'
//!     lcl issue create --title "Lowercase key" --parent fixt-3
//!     lcl issue create --title "Other key" --parent BZR-1
//!     lcl issue create --title "Self" --parent 10
//!     lcl issue create --title "Cycle A" --parent 12
//!     lcl issue create --title "Cycle B" --parent 11
//!     lcl issue create --title "Under the cycle" --parent 11
//!     U2=$(uuid 2)
//!     lcl issue create --title "Uppercase uuid" --parent "$(printf %s "$U2" | tr a-f A-F)"
//!     lcl issue create --title "Empty" --parent ""
//!     lcl issue create --title "Missing uuid" --parent 00000000-0000-4000-8000-000000000000
//!
//! and then .lcl/data.sqlite was copied before anything else opened it. No
//! -wal or -shm file was left behind. The fixture is at user_version 0 in WAL
//! mode. Tests copy it with `Env::install_db`; never open it in place.

mod common;

use common::{dump, foreign_key_violations, fts_integrity, FTS_TABLES, issue_row, undo_v5_schema, undo_v6_schema, undo_v7_schema, user_version, Env, V0_PARENTS_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Value};
use std::path::Path;

/// A workspace with key DREA.
fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd"]).assert_ok();
    env
}

fn create(env: &Env, title: &str, parent: &str) -> Value {
    env.lcl(&["issue", "create", "--title", title, &format!("--parent={parent}"), "--json"]).assert_ok().json()
}

fn text(v: &SqlValue) -> Option<String> {
    match v {
        SqlValue::Text(s) => Some(s.clone()),
        SqlValue::Null => None,
        other => panic!("expected text or null, got {other:?}"),
    }
}

/// `parent_id` exactly as stored.
fn stored_parent(db: &Path, n: i64) -> Option<String> {
    text(&issue_row(db, n)["parent_id"])
}

fn id(db: &Path, n: i64) -> String {
    text(&issue_row(db, n)["id"]).expect("id")
}

fn children(env: &Env, n: i64) -> Value {
    env.lcl(&["issue", "show", &n.to_string(), "--json"]).assert_ok().json()["children"].clone()
}

#[test]
fn the_parent_is_stored_as_its_uuid() {
    let env = workspace();
    env.create("parent");
    env.create("other");
    let db = env.db_path();
    let (u1, u2) = (id(&db, 1), id(&db, 2));

    for form in ["DREA-1", "drea-1", "#1", "1", u1.as_str(), u1.to_uppercase().as_str()] {
        let child = create(&env, "child", form);
        assert_eq!(child["parent_id"], u1.as_str(), "--parent {form}: {child:#}");
        assert_eq!(child["parent_identifier"], "DREA-1", "--parent {form}");
        let n = child["number"].as_i64().expect("number");
        assert_eq!(stored_parent(&db, n), Some(u1.clone()), "--parent {form}");
    }
    let moved = env.lcl(&["issue", "update", "3", "--parent", "DREA-2", "--json"]).assert_ok().json();
    assert_eq!((moved["parent_id"].as_str(), moved["parent_identifier"].as_str()), (Some(u2.as_str()), Some("DREA-2")));
    assert_eq!(stored_parent(&db, 3), Some(u2));
}

#[test]
fn a_missing_parent_exits_1_and_changes_nothing() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    let before = dump(&db);
    let missing = "00000000-0000-4000-8000-000000000000";
    for (args, named) in [
        (vec!["issue", "create", "--title", "orphan", "--parent", "DREA-9"], "DREA-9"),
        (vec!["issue", "update", "1", "--parent", "#9"], "DREA-9"),
        (vec!["issue", "update", "1", "--parent", missing], missing),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(&format!("issue {named} not found")), "{args:?}: {out:#?}");
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn an_issue_cannot_be_its_own_parent() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    let u1 = id(&db, 1);
    let before = dump(&db);
    for form in ["DREA-1", "1", "#1", u1.as_str()] {
        let out = env.lcl(&["issue", "update", "1", "--parent", form]);
        assert_eq!(out.code, 2, "{form}: {out:#?}");
        assert!(out.stderr.contains("DREA-1 can't be its own parent"), "{form}: {out:#?}");
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn a_parent_cycle_is_refused() {
    let env = workspace();
    env.create("A");
    create(&env, "B", "DREA-1");
    create(&env, "C", "DREA-2");
    let db = env.db_path();
    let before = dump(&db);

    for (issue, parent, path) in [
        ("1", "DREA-3", "DREA-1 -> DREA-3 -> DREA-2 -> DREA-1"),
        ("1", "DREA-2", "DREA-1 -> DREA-2 -> DREA-1"),
        ("2", "DREA-3", "DREA-2 -> DREA-3 -> DREA-2"),
    ] {
        let out = env.lcl(&["issue", "update", issue, "--parent", parent]);
        assert_eq!(out.code, 2, "{issue} under {parent}: {out:#?}");
        let want = format!("making {parent} the parent of DREA-{issue} would make a cycle: {path}");
        assert!(out.stderr.contains(&want), "{issue} under {parent}: {out:#?}");
    }
    assert_eq!(dump(&db), before);

    // Moves that keep it a tree are fine.
    let c = env.lcl(&["issue", "update", "3", "--parent", "DREA-1", "--json"]).assert_ok().json();
    assert_eq!(c["parent_identifier"], "DREA-1");
    env.create("D");
    let a = env.lcl(&["issue", "update", "1", "--parent", "DREA-4", "--json"]).assert_ok().json();
    assert_eq!(a["parent_identifier"], "DREA-4");
}

#[test]
fn no_parent_clears_it_and_conflicts_with_parent() {
    let env = workspace();
    env.create("A");
    create(&env, "B", "DREA-1");
    let db = env.db_path();
    let before = dump(&db);

    let out = env.lcl(&["issue", "update", "2", "--parent", "DREA-1", "--no-parent"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(out.stderr.contains("cannot be used with"), "{out:#?}");
    assert_eq!(dump(&db), before);

    for _ in 0..2 {
        let b = env.lcl(&["issue", "update", "2", "--no-parent", "--json"]).assert_ok().json();
        assert!(b["parent_id"].is_null() && b["parent_identifier"].is_null(), "{b:#}");
        assert_eq!(stored_parent(&db, 2), None);
    }
    assert_eq!(children(&env, 1), json!([]));
}

#[test]
fn show_lists_sub_issues() {
    let env = workspace();
    env.create("A");
    create(&env, "B", "DREA-1");
    create(&env, "C", "DREA-1");
    create(&env, "D", "DREA-2");

    assert_eq!(children(&env, 1), json!(["DREA-2", "DREA-3"]));
    assert_eq!(children(&env, 2), json!(["DREA-4"]));
    assert_eq!(children(&env, 4), json!([]));

    let a = env.lcl(&["issue", "show", "1"]).assert_ok().stdout.clone();
    assert!(a.contains("\nSub-issues:\n  DREA-2 B\n  DREA-3 C\n"), "{a}");
    assert!(!a.contains("Parent:"), "{a}");
    let b = env.lcl(&["issue", "show", "2"]).assert_ok().stdout.clone();
    assert!(b.contains("\nParent: DREA-1\n") && b.contains("\nSub-issues:\n  DREA-4 D\n"), "{b}");
    let d = env.lcl(&["issue", "show", "4"]).assert_ok().stdout.clone();
    assert!(!d.contains("Sub-issues:"), "{d}");
}

#[test]
fn list_filters_by_parent() {
    let env = workspace();
    env.create("A");
    create(&env, "B", "DREA-1");
    env.lcl(&["issue", "create", "--title", "C", "--parent", "DREA-1", "--status", "Done"]).assert_ok();
    create(&env, "D", "DREA-2");
    env.create("E");

    let numbers = |args: &[&str]| -> Vec<i64> {
        let mut cmd = vec!["issue", "list", "--json"];
        cmd.extend(args);
        let list = env.lcl(&cmd).assert_ok().json();
        list["issues"].as_array().expect("array").iter().map(|i| i["number"].as_i64().expect("number")).collect()
    };
    assert_eq!(numbers(&["--parent", "DREA-1"]), [3, 2], "direct sub-issues only, newest first");
    assert_eq!(numbers(&["--parent", "#2"]), [4]);
    assert_eq!(numbers(&["--parent", "DREA-4"]), Vec::<i64>::new());
    assert_eq!(numbers(&["--parent", "1", "--status", "Done"]), [3]);
    assert_eq!(numbers(&["--parent", "1", "--limit", "1"]), [3], "the limit applies after the filter");

    let human = env.lcl(&["issue", "list", "--parent", "DREA-1"]).assert_ok().stdout.clone();
    let firsts: Vec<_> = human.lines().map(|l| l.split('\t').next().unwrap_or_default()).collect();
    assert_eq!(firsts, ["DREA-3", "DREA-2"], "{human}");
}

#[test]
fn delete_refuses_an_issue_with_sub_issues() {
    let env = workspace();
    env.create("A");
    create(&env, "B", "DREA-1");
    create(&env, "C", "DREA-1");
    let db = env.db_path();
    let before = dump(&db);

    let out = env.lcl(&["issue", "delete", "DREA-1"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("DREA-1 has sub-issues DREA-2, DREA-3; reparent or delete them first"), "{out:#?}");
    assert_eq!(dump(&db), before);

    env.lcl(&["issue", "update", "2", "--no-parent"]).assert_ok();
    env.lcl(&["issue", "update", "3", "--parent", "DREA-2"]).assert_ok();
    assert_eq!(env.lcl(&["issue", "delete", "DREA-1"]).assert_ok().stdout, "Deleted DREA-1\n");

    let out = env.lcl(&["issue", "delete", "2"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("DREA-2 has sub-issues DREA-3; reparent or delete them first"), "{out:#?}");
    env.lcl(&["issue", "delete", "3"]).assert_ok();
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list, json!({"issues": [], "total": 0, "limit": 50, "offset": 0, "truncated": false}));
}

#[test]
fn upgrade_repairs_parent_ids() {
    let env = Env::new();
    let db = env.install_db(V0_PARENTS_FIXTURE);
    assert_eq!(user_version(&db), 0, "the fixture should be an unversioned 0.1.0 database");
    let u = |n| id(&db, n);

    // The fixture as 0.1.0 left it: parents as typed.
    let raw: Vec<Option<String>> = (1..=16).map(|n| stored_parent(&db, n)).collect();
    let typed = [
        None, Some("1".into()), Some(u(1)), Some("FIXT-1".into()), Some("99999".into()), Some("garbage".into()),
        Some("#2".into()), Some("fixt-3".into()), Some("BZR-1".into()), Some("10".into()), Some("12".into()),
        Some("11".into()), Some("11".into()), Some(u(2).to_uppercase()), Some(String::new()),
        Some("00000000-0000-4000-8000-000000000000".into()),
    ];
    assert_eq!(raw, typed);
    let mut before = dump(&db);

    env.lcl(&["issue", "list"]).assert_ok();
    assert_eq!(user_version(&db), SCHEMA_VERSION);

    // FIXT-n's parent after the upgrade, as an issue number.
    let want: [(i64, Option<i64>, &str); 16] = [
        (1, None, "had none"),
        (2, Some(1), "\"1\""),
        (3, Some(1), "an existing UUID stays"),
        (4, Some(1), "\"FIXT-1\""),
        (5, None, "\"99999\": no such issue"),
        (6, None, "\"garbage\""),
        (7, Some(2), "\"#2\""),
        (8, Some(3), "\"fixt-3\": the key in any case"),
        (9, None, "\"BZR-1\": another workspace"),
        (10, None, "\"10\": itself, a cycle"),
        (11, None, "\"12\": a cycle with 12"),
        (12, None, "\"11\": a cycle with 11"),
        (13, Some(11), "\"11\": below the cycle, kept"),
        (14, Some(2), "issue 2's UUID in upper case"),
        (15, None, "\"\""),
        (16, None, "a UUID with no issue"),
    ];
    for (n, parent, why) in want {
        assert_eq!(stored_parent(&db, n), parent.map(u), "FIXT-{n} ({why})");
    }

    // What lcl shows for the repaired tree.
    assert_eq!(children(&env, 1), json!(["FIXT-2", "FIXT-3", "FIXT-4"]));
    assert_eq!(children(&env, 11), json!(["FIXT-13"]));
    let thirteen = env.lcl(&["issue", "show", "13", "--json"]).assert_ok().json();
    assert_eq!(thirteen["issue"]["parent_identifier"], "FIXT-11");

    // Nothing else changed beyond migrations 2-5. Undo their expected changes
    // on `after`; what's left must equal `before`. (v3's backfills don't
    // apply: no issue here is Done or Canceled.)
    let t = |s: &str| SqlValue::Text(s.into());
    let mut after = dump(&db);
    let meta = after.tables.get_mut("meta").expect("meta table");
    let mark = meta.iter().position(|r| r["key"] == t("next_issue_number")).expect("mark");
    assert_eq!(meta.remove(mark)["value"], t("17"));
    for row in after.tables.get_mut("issues").expect("issues table") {
        assert_eq!(row.remove("started_at"), Some(SqlValue::Null), "{row:?}");
        assert_eq!(row.remove("canceled_at"), Some(SqlValue::Null), "{row:?}");
        row.remove("parent_id");
    }
    for row in before.tables.get_mut("issues").expect("issues table") {
        row.remove("parent_id");
    }
    // v5: no labels or blank assignees here, so only its schema changes.
    undo_v5_schema(&mut after, &mut before);
    undo_v6_schema(&mut after);
    undo_v7_schema(&mut after);
    assert_eq!(foreign_key_violations(&db), vec![]);
    // v3's ADD COLUMN rewrote the issues CREATE statement, and v4's UPDATE
    // fired the FTS trigger for the rows it changed: check those with FTS5's
    // integrity-check instead of byte for byte.
    for d in [&mut before, &mut after] {
        d.schema.retain(|r| r["name"] != t("issues"));
        d.tables.retain(|t, _| !t.starts_with("issues_fts"));
    }
    assert_eq!(after, before);
    fts_integrity(&db, &FTS_TABLES);
    let found = env.lcl(&["search", "cycle", "--json"]).assert_ok().json();
    let mut found: Vec<_> = found["issues"].as_array().expect("issues").iter().map(|i| i["number"].as_i64().expect("number")).collect();
    found.sort();
    assert_eq!(found, [11, 12, 13]);
}
