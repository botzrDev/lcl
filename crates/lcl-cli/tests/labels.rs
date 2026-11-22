//! Labels as records, clearing fields, and deletes of unknown ids.
//!
//! A label argument is a name, trimmed of whitespace, matched ignoring
//! (ASCII) case, and must name an existing label. Names are unique ignoring case. Deleting a label takes
//! it off its issues through the foreign key's cascade. `issue update` can
//! add, remove and clear labels, and clear the assignee, estimate and due
//! date. Deleting an unknown label or doc exits 1.
//!
//! tests/fixtures/v0-labels.sqlite was built like v0.sqlite (see
//! tests/schema.rs): lcl 0.1.0 at ~/.local/bin/lcl (sha256 1b42efae...),
//! HOME set to a temp dir, LCL_PROJECT unset, run with `sh -e` from an empty
//! temp cwd:
//!
//!     lbl() { lcl --json label create "$@" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["id"])'; }
//!     lcl init fixture
//!     H1=$(lbl hitl --color '#111111')
//!     H2=$(lbl hitl --color '#222222')
//!     B1=$(lbl Bug --color '#333333')
//!     B2=$(lbl bug --color '#444444')
//!     D=$(lbl docs)
//!     T=$(lbl temp)
//!     lcl issue create --title "Raw lower" --label hitl
//!     lcl issue create --title "Raw upper" --label HITL
//!     lcl issue create --title "Raw mixed, two labels" --label Hitl --label bug
//!     lcl issue create --title "Unknown raw" --label needs-triage
//!     lcl issue create --title "Unknown raw, other case" --label Needs-Triage
//!     lcl issue create --title "Empty label" --label ""
//!     lcl issue create --title "Whitespace label" --label "  "
//!     lcl issue create --title "By id of the merged hitl" --label "$H2"
//!     lcl issue create --title "By id" --label "$D"
//!     lcl issue create --title "Same label three ways" --label hitl --label HITL --label "$H1"
//!     lcl issue create --title "Empty assignee" --assignee ""
//!     lcl issue create --title "Whitespace assignee" --assignee "  "
//!     lcl issue create --title "Real assignee" --assignee cleetus --label docs
//!     lcl issue create --title "Deleted label's id" --label "$T"
//!     lcl label delete "$T"
//!
//! and then .lcl/data.sqlite was copied before anything else opened it. No
//! -wal or -shm file was left behind. The fixture is at user_version 0 in WAL
//! mode. Tests copy it with `Env::install_db`; never open it in place.

mod common;

use common::{
    Env, FTS_TABLES, V0_LABELS_FIXTURE, dump, exec, foreign_key_violations, fts_integrity,
    issue_row, undo_v5_schema, undo_v6_schema, undo_v7_schema, undo_v8_schema, undo_v9_schema,
    undo_v10_schema, undo_v11_schema, user_version,
};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::path::Path;

fn workspace(labels: &[&str]) -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    for l in labels {
        env.lcl(&["label", "create", l]).assert_ok();
    }
    env
}

fn labels_of(env: &Env, n: i64) -> Value {
    env.lcl(&["issue", "show", &n.to_string(), "--json"])
        .assert_ok()
        .json()["issue"]["labels"]
        .clone()
}

/// `issue update N <args> --json`, asserting success; returns the labels.
fn update_labels(env: &Env, n: i64, args: &[&str]) -> Value {
    let n = n.to_string();
    let mut cmd = vec!["issue", "update", n.as_str(), "--json"];
    cmd.extend(args);
    env.lcl(&cmd).assert_ok().json()["labels"].clone()
}

fn text(v: &SqlValue) -> Option<String> {
    match v {
        SqlValue::Text(s) => Some(s.clone()),
        SqlValue::Null => None,
        other => panic!("expected text or null, got {other:?}"),
    }
}

/// label name -> id, read directly.
fn label_ids(db: &Path) -> std::collections::BTreeMap<String, String> {
    let conn = rusqlite::Connection::open(db).expect("open db");
    let mut stmt = conn
        .prepare("SELECT name, id FROM labels")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

#[test]
fn label_names_resolve_ignoring_case() {
    let env = workspace(&["hitl", "Bug"]);
    let db = env.db_path();
    let created = env
        .lcl(&[
            "issue", "create", "--title", "t", "--label", "HITL", "--label", "bug", "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(created["labels"], json!(["Bug", "hitl"]));
    assert_eq!(
        update_labels(&env, 1, &["--label", "Hitl"]),
        json!(["hitl"])
    );
    assert_eq!(
        update_labels(&env, 1, &["--label", " HITL "]),
        json!(["hitl"]),
        "names are trimmed"
    );
    assert_eq!(
        update_labels(&env, 1, &["--add-label", "\tBUG\n"]),
        json!(["Bug", "hitl"])
    );
    assert!(
        env.lcl(&["issue", "show", "1"])
            .assert_ok()
            .stdout
            .contains("\nLabels: Bug, hitl\n")
    );
    assert_eq!(
        update_labels(&env, 1, &["--remove-label", " HiTl  "]),
        json!(["Bug"])
    );
    assert_eq!(
        update_labels(&env, 1, &["--add-label", "bug"]),
        json!(["Bug"]),
        "the same label in another case is not added twice"
    );

    // What's stored is the label's id.
    let ids = label_ids(&db);
    let conn = rusqlite::Connection::open(&db).expect("open db");
    let stored: Vec<String> = conn
        .prepare("SELECT label_id FROM issue_labels")
        .and_then(|mut s| s.query_map([], |r| r.get(0)).and_then(Iterator::collect))
        .expect("issue_labels");
    assert_eq!(stored, [ids["Bug"].clone()]);
}

#[test]
fn issue_labels_are_names_sorted_ignoring_case() {
    let env = workspace(&["gamma", "Alpha", "beta"]);
    let created = env
        .lcl(&[
            "issue", "create", "--title", "t", "--label", "gamma", "--label", "beta", "--label",
            "alpha", "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(created["labels"], json!(["Alpha", "beta", "gamma"]));
    let listed = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(
        listed["issues"][0]["labels"],
        json!(["Alpha", "beta", "gamma"])
    );
}

/// An unknown label is not found (exit 1): with the label there, the
/// command would succeed. The error says how to make it.
#[test]
fn an_unknown_label_exits_1_and_changes_nothing() {
    let env = workspace(&["hitl"]);
    env.lcl(&["issue", "create", "--title", "t", "--label", "hitl"])
        .assert_ok();
    let db = env.db_path();
    let before = dump(&db);

    for (args, name, hint) in [
        (
            vec!["issue", "create", "--title", "u", "--label", "nope"],
            "nope",
            "lcl label create nope",
        ),
        (
            vec![
                "issue",
                "create",
                "--title",
                "u",
                "--label",
                "hitl",
                "--label",
                "needs triage",
            ],
            "needs triage",
            "lcl label create 'needs triage'",
        ),
        (
            vec!["issue", "update", "1", "--label", "nope"],
            "nope",
            "lcl label create nope",
        ),
        (
            vec!["issue", "update", "1", "--add-label", "nope"],
            "nope",
            "lcl label create nope",
        ),
        (
            vec!["issue", "update", "1", "--remove-label", "nope"],
            "nope",
            "lcl label create nope",
        ),
        (
            vec!["issue", "update", "1", "--add-label", "it's"],
            "it's",
            r"lcl label create 'it'\''s'",
        ),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(
            out.stderr
                .contains(&format!("unknown label {name:?}; create it with: {hint}")),
            "{args:?}: {out:#?}"
        );
    }
    for blank in ["", "  "] {
        for args in [
            vec!["issue", "create", "--title", "u", "--label", blank],
            vec!["issue", "update", "1", "--add-label", blank],
            vec!["label", "create", blank],
        ] {
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(
                out.stderr.contains("a label name can't be empty"),
                "{args:?}: {out:#?}"
            );
        }
    }
    assert_eq!(
        dump(&db),
        before,
        "a refused create used up a number or a refused update wrote"
    );
    assert_eq!(env.create("next"), 2);
}

#[test]
fn add_remove_and_clear() {
    let env = workspace(&["a", "b", "c"]);
    env.lcl(&["issue", "create", "--title", "t", "--label", "a"])
        .assert_ok();

    assert_eq!(
        update_labels(&env, 1, &["--add-label", "b", "--add-label", "c"]),
        json!(["a", "b", "c"])
    );
    assert_eq!(
        update_labels(&env, 1, &["--add-label", "a"]),
        json!(["a", "b", "c"]),
        "adding a label it has is a no-op"
    );
    assert_eq!(
        update_labels(&env, 1, &["--remove-label", "b"]),
        json!(["a", "c"])
    );
    assert_eq!(
        update_labels(&env, 1, &["--remove-label", "b"]),
        json!(["a", "c"]),
        "removing a label it doesn't have is a no-op"
    );
    assert_eq!(
        update_labels(&env, 1, &["--add-label", "b", "--remove-label", "a"]),
        json!(["b", "c"])
    );
    assert!(
        env.lcl(&["issue", "show", "1"])
            .assert_ok()
            .stdout
            .contains("\nLabels: b, c\n")
    );
    assert_eq!(update_labels(&env, 1, &["--clear-labels"]), json!([]));
    assert!(
        !env.lcl(&["issue", "show", "1"])
            .assert_ok()
            .stdout
            .contains("Labels"),
        "no Labels line when there are none"
    );
    assert_eq!(update_labels(&env, 1, &["--clear-labels"]), json!([]));
    assert_eq!(
        update_labels(&env, 1, &["--label", "c", "--label", "a"]),
        json!(["a", "c"]),
        "--label replaces the set"
    );
    assert_eq!(
        update_labels(&env, 1, &["--title", "renamed"]),
        json!(["a", "c"]),
        "no label flag keeps the set"
    );
}

#[test]
fn label_flags_conflict() {
    let env = workspace(&["a", "b"]);
    env.lcl(&["issue", "create", "--title", "t", "--label", "a"])
        .assert_ok();
    let db = env.db_path();
    let before = dump(&db);
    for (args, why) in [
        (
            vec!["--label", "a", "--add-label", "b"],
            "cannot be used with",
        ),
        (
            vec!["--label", "a", "--remove-label", "b"],
            "cannot be used with",
        ),
        (
            vec!["--label", "a", "--clear-labels"],
            "cannot be used with",
        ),
        (
            vec!["--clear-labels", "--add-label", "b"],
            "cannot be used with",
        ),
        (
            vec!["--clear-labels", "--remove-label", "a"],
            "cannot be used with",
        ),
        (
            vec!["--add-label", "b", "--remove-label", "B"],
            "label \"b\" is in both --add-label and --remove-label",
        ),
    ] {
        let mut cmd = vec!["issue", "update", "1"];
        cmd.extend(&args);
        let out = env.lcl(&cmd);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(why), "{args:?}: {out:#?}");
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn label_create_refuses_a_duplicate_name() {
    let env = workspace(&["hitl"]);
    for (name, msg) in [
        ("hitl", "label \"hitl\" already exists"),
        ("HITL", "label \"HITL\" already exists (as \"hitl\")"),
        (" hitl", "label \"hitl\" already exists"),
        ("HITL\t", "label \"HITL\" already exists (as \"hitl\")"),
    ] {
        let out = env.lcl(&["label", "create", name]);
        assert_eq!(out.code, 1, "{name}: {out:#?}");
        assert!(out.stderr.contains(msg), "{name}: {out:#?}");
    }
    let labels = env.lcl(&["label", "list", "--json"]).assert_ok().json();
    assert_eq!(labels.as_array().map(Vec::len), Some(1), "{labels:#}");
    // After a delete the name is free again; a padded name is stored trimmed.
    env.lcl(&["label", "delete", "hitl"]).assert_ok();
    let created = env
        .lcl(&["label", "create", "  HITL ", "--json"])
        .assert_ok()
        .json();
    assert_eq!(created["name"], "HITL");
}

#[test]
fn label_delete_cascades_and_counts() {
    let env = workspace(&["hitl", "bug", "lonely", "unused"]);
    let db = env.db_path();
    let ids = label_ids(&db);
    for (title, labels) in [
        ("one", &["hitl", "bug"][..]),
        ("two", &["hitl"]),
        ("three", &["bug", "lonely"]),
        ("four", &[]),
    ] {
        let mut cmd = vec!["issue", "create", "--title", title];
        for l in labels {
            cmd.extend(["--label", l]);
        }
        env.lcl(&cmd).assert_ok();
    }

    let out = env
        .lcl(&["label", "delete", "HITL"])
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(out, "Deleted label hitl (removed from 2 issues)\n");
    assert_eq!(
        [labels_of(&env, 1), labels_of(&env, 2)],
        [json!(["bug"]), json!([])]
    );

    let out = env
        .lcl(&["label", "delete", &ids["bug"], "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        out,
        json!({"deleted": "bug", "id": ids["bug"], "removed_from_issues": 2})
    );
    assert_eq!(
        [labels_of(&env, 1), labels_of(&env, 3)],
        [json!([]), json!(["lonely"])]
    );

    assert_eq!(
        env.lcl(&["label", "delete", " lonely "]).assert_ok().stdout,
        "Deleted label lonely (removed from 1 issue)\n"
    );
    assert_eq!(
        env.lcl(&["label", "delete", "unused"]).assert_ok().stdout,
        "Deleted label unused (removed from 0 issues)\n"
    );

    let conn = rusqlite::Connection::open(&db).expect("open db");
    let left: i64 = conn
        .query_row("SELECT count(*) FROM issue_labels", [], |r| r.get(0))
        .expect("count");
    assert_eq!(left, 0);
    assert_eq!(foreign_key_violations(&db), vec![]);
}

#[test]
fn unknown_label_and_doc_deletes_exit_1() {
    let env = workspace(&["hitl"]);
    env.lcl(&["doc", "create", "--title", "Handbook"])
        .assert_ok();
    let db = env.db_path();
    let before = dump(&db);
    let missing = "00000000-0000-4000-8000-000000000000";
    for (args, msg) in [
        (
            vec!["label", "delete", "nope"],
            "label \"nope\" not found".to_string(),
        ),
        (
            vec!["label", "delete", missing],
            format!("label {missing:?} not found"),
        ),
        (
            vec!["doc", "delete", missing],
            format!("doc {missing} not found"),
        ),
        (
            vec!["doc", "delete", "Handbook"],
            "doc Handbook not found".to_string(),
        ),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(&msg), "{args:?}: {out:#?}");
        assert!(!out.stdout.contains("Deleted"), "{args:?}: {out:#?}");
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn clear_flags_and_blank_assignees() {
    let env = workspace(&[]);
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "t",
        "--assignee",
        "cleetus",
        "--estimate",
        "3",
        "--due",
        "2026-05-01",
    ])
    .assert_ok();
    let db = env.db_path();
    assert!(
        env.lcl(&["issue", "show", "1"])
            .assert_ok()
            .stdout
            .contains("Assignee: cleetus\n")
    );

    let before = dump(&db);
    for (args, why) in [
        (
            vec!["issue", "update", "1", "--assignee", "x", "--no-assignee"],
            "cannot be used with".to_string(),
        ),
        (
            vec!["issue", "update", "1", "--estimate", "1", "--no-estimate"],
            "cannot be used with".to_string(),
        ),
        (
            vec!["issue", "update", "1", "--due", "2026-06-01", "--no-due"],
            "cannot be used with".to_string(),
        ),
        (
            vec!["issue", "update", "1", "--assignee", ""],
            "use `issue update --no-assignee`".to_string(),
        ),
        (
            vec!["issue", "update", "1", "--assignee", " \t "],
            "use `issue update --no-assignee`".to_string(),
        ),
        (
            vec!["issue", "create", "--title", "u", "--assignee", ""],
            "use `issue update --no-assignee`".to_string(),
        ),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(&why), "{args:?}: {out:#?}");
    }
    assert_eq!(dump(&db), before);

    let field = |args: &[&str], key: &str| -> Value {
        let mut cmd = vec!["issue", "update", "1", "--json"];
        cmd.extend(args);
        env.lcl(&cmd).assert_ok().json()[key].clone()
    };
    assert_eq!(field(&["--no-assignee"], "assignee"), Value::Null);
    assert_eq!(field(&["--no-estimate"], "estimate"), Value::Null);
    assert_eq!(field(&["--no-due"], "due_date"), Value::Null);
    let row = issue_row(&db, 1);
    assert_eq!(
        (&row["assignee"], &row["estimate"], &row["due_date"]),
        (&SqlValue::Null, &SqlValue::Null, &SqlValue::Null)
    );
    assert!(
        env.lcl(&["issue", "show", "1"])
            .assert_ok()
            .stdout
            .contains("Assignee: Unassigned\n")
    );
    // Clearing what is already clear is fine.
    env.lcl(&[
        "issue",
        "update",
        "1",
        "--no-assignee",
        "--no-estimate",
        "--no-due",
    ])
    .assert_ok();
    // Assignees are trimmed.
    env.lcl(&["issue", "update", "1", "--assignee", " cleetus "])
        .assert_ok();
    assert_eq!(
        issue_row(&db, 1)["assignee"],
        SqlValue::Text("cleetus".into())
    );
    let created = env
        .lcl(&[
            "issue",
            "create",
            "--title",
            "w",
            "--assignee",
            "\tbrutus\n",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(created["assignee"], "brutus");
}

#[test]
fn cycle_list_prints_plain_dates() {
    let env = workspace(&[]);
    env.lcl(&[
        "cycle",
        "create",
        "both",
        "--start",
        "2026-03-01",
        "--end",
        "2026-03-14",
    ])
    .assert_ok();
    env.lcl(&["cycle", "create", "open", "--start", "2026-04-01"])
        .assert_ok();
    let out = env.lcl(&["cycle", "list"]).assert_ok().stdout.clone();
    let tails: Vec<_> = out
        .lines()
        .map(|l| l.split('\t').skip(1).collect::<Vec<_>>().join("\t"))
        .collect();
    let mut tails = tails;
    tails.sort();
    assert_eq!(
        tails,
        ["both\t2026-03-01..2026-03-14", "open\t2026-04-01..?"],
        "{out}"
    );
    assert!(!out.contains("Some("), "{out}");
}

#[test]
fn upgrade_maps_0_1_0_labels() {
    let env = Env::new();
    let db = env.install_db(V0_LABELS_FIXTURE);
    assert_eq!(
        user_version(&db),
        0,
        "the fixture should be an unversioned 0.1.0 database"
    );

    // The labels as 0.1.0 left them, by rowid.
    let conn = rusqlite::Connection::open(&db).expect("open db");
    let old: Vec<(String, String, String)> = conn
        .prepare("SELECT id, name, color FROM labels ORDER BY rowid")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .and_then(Iterator::collect)
        })
        .expect("labels");
    drop(conn);
    let names: Vec<_> = old
        .iter()
        .map(|(_, n, c)| (n.as_str(), c.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("hitl", "#111111"),
            ("hitl", "#222222"),
            ("Bug", "#333333"),
            ("bug", "#444444"),
            ("docs", "#888888")
        ]
    );
    let (h1, h2, b1, b2, docs) = (&old[0].0, &old[1].0, &old[2].0, &old[3].0, &old[4].0);
    let deleted_id = text(&issue_label_raw(&db, 14)[0]).expect("issue 14's raw label");
    assert!(
        ![h1, h2, b1, b2, docs].contains(&&deleted_id),
        "issue 14 should carry a deleted label's id"
    );
    // Added by hand: a padded raw name on FIXT-12, and on FIXT-11 a raw UUID
    // (upper case) that is a label's *name*. That one is not a leftover of a
    // deleted label, so it is kept.
    let uuid_named = String::from("0a1b2c3d-0000-4000-8000-00000000c0de");
    let uuid_named_id = String::from("5ca1ab1e-0000-4000-8000-000000000001");
    exec(&db, &format!(
        "INSERT INTO issue_labels(issue_id, label_id) SELECT id, ' Hitl ' FROM issues WHERE number = 12;
         INSERT INTO labels(id, name, color) VALUES('{uuid_named_id}', '{uuid_named}', '#555555');
         INSERT INTO issue_labels(issue_id, label_id) SELECT id, upper('{uuid_named}') FROM issues WHERE number = 11;"
    ));
    // Also by hand: padded label names. " Hitl " merges into hitl. "  ops "
    // and a later "OPS" merge into "  ops ", which keeps the trimmed
    // spelling "ops". Plus a padded assignee on FIXT-9.
    let (padded_hitl, ops, ops2) = (
        String::from("5ca1ab1e-0000-4000-8000-000000000006"),
        String::from("5ca1ab1e-0000-4000-8000-000000000007"),
        String::from("5ca1ab1e-0000-4000-8000-000000000008"),
    );
    exec(&db, &format!(
        "INSERT INTO labels(id, name, color) VALUES('{padded_hitl}', ' Hitl ', '#666666'), ('{ops}', '  ops ', '#777777'), ('{ops2}', 'OPS', '#7777aa');
         INSERT INTO issue_labels(issue_id, label_id) SELECT id, '{padded_hitl}' FROM issues WHERE number = 13;
         INSERT INTO issue_labels(issue_id, label_id) SELECT id, 'Ops' FROM issues WHERE number = 6;
         INSERT INTO issue_labels(issue_id, label_id) SELECT id, '{ops2}' FROM issues WHERE number = 7;
         UPDATE issues SET assignee = ' cleetus ' WHERE number = 9;"
    ));
    let mut before = dump(&db);

    env.lcl(&["issue", "list"]).assert_ok();
    assert_eq!(user_version(&db), SCHEMA_VERSION);

    // Labels: duplicates merged into the lowest rowid (keeping its id and
    // colour), and one new label per unknown name ignoring case.
    let after = label_rows(&db);
    let new_triage = after
        .iter()
        .find(|(_, n, _)| n == "needs-triage")
        .expect("needs-triage made")
        .0
        .clone();
    let mut want = vec![
        (h1.clone(), "hitl".to_string(), "#111111".to_string()),
        (b1.clone(), "Bug".into(), "#333333".into()),
        (docs.clone(), "docs".into(), "#888888".into()),
        (new_triage.clone(), "needs-triage".into(), "#888888".into()),
        (uuid_named_id.clone(), uuid_named.clone(), "#555555".into()),
        (ops.clone(), "ops".into(), "#777777".into()),
    ];
    want.sort();
    let mut got = after.clone();
    got.sort();
    assert_eq!(got, want, "no label may be named by the deleted label's id");
    let r = new_triage.parse::<lcl_core::models::IssueRef>();
    assert!(
        r.is_ok_and(|r| matches!(r, lcl_core::models::IssueRef::Id(_))),
        "{new_triage} is not a UUID"
    );

    // Each issue's labels, as stored (ids) and as shown (sorted names).
    let expect: [(i64, &[&String], &[&str], &str); 14] = [
        (1, &[h1], &["hitl"], "raw lower"),
        (2, &[h1], &["hitl"], "raw upper"),
        (
            3,
            &[b1, h1],
            &["Bug", "hitl"],
            "raw mixed case, and \"bug\" merged into \"Bug\"",
        ),
        (4, &[&new_triage], &["needs-triage"], "unknown name"),
        (
            5,
            &[&new_triage],
            &["needs-triage"],
            "unknown name, other case: the same new label",
        ),
        (
            6,
            &[&ops],
            &["ops"],
            "\"\" dropped; raw \"Ops\" -> the label \"  ops \", trimmed",
        ),
        (
            7,
            &[&ops],
            &["ops"],
            "\"  \" dropped; the merged \"OPS\" label's id -> its survivor",
        ),
        (8, &[h1], &["hitl"], "id of the merged hitl -> its survivor"),
        (9, &[docs], &["docs"], "by id"),
        (10, &[h1], &["hitl"], "three ways, one row"),
        (
            11,
            &[&uuid_named_id],
            &[uuid_named.as_str()],
            "a UUID that is a label's name: kept",
        ),
        (12, &[h1], &["hitl"], "raw \" Hitl \" trimmed"),
        (
            13,
            &[docs, h1],
            &["docs", "hitl"],
            "raw name; and the id of \" Hitl \", merged into hitl",
        ),
        (14, &[], &[], "a deleted label's id: dropped"),
    ];
    for (n, ids, names, why) in expect {
        let mut stored: Vec<String> = issue_label_raw(&db, n)
            .iter()
            .map(|v| text(v).expect("label id"))
            .collect();
        stored.sort();
        let mut want: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(stored, want, "FIXT-{n} stored ({why})");
        assert_eq!(labels_of(&env, n), json!(names), "FIXT-{n} shown ({why})");
    }

    // Assignees trimmed; blank ones became NULL.
    for (n, assignee) in [
        (9, Some("cleetus")),
        (11, None),
        (12, None),
        (13, Some("cleetus")),
    ] {
        assert_eq!(
            text(&issue_row(&db, n)["assignee"]).as_deref(),
            assignee,
            "FIXT-{n}"
        );
    }
    assert_eq!(foreign_key_violations(&db), vec![]);
    let dup = env.lcl(&["label", "create", "HITL"]);
    assert_eq!(
        dup.code, 1,
        "the unique index's rule holds after the upgrade: {dup:#?}"
    );

    // Nothing else changed beyond migrations 2-5.
    let t = |s: &str| SqlValue::Text(s.into());
    let mut after = dump(&db);
    let meta = after.tables.get_mut("meta").expect("meta table");
    let mark = meta
        .iter()
        .position(|r| r["key"] == t("next_issue_number"))
        .expect("mark");
    assert_eq!(meta.remove(mark)["value"], t("15"));
    for row in after.tables.get_mut("issues").expect("issues table") {
        assert_eq!(row.remove("started_at"), Some(SqlValue::Null), "{row:?}");
        assert_eq!(row.remove("canceled_at"), Some(SqlValue::Null), "{row:?}");
        row.remove("assignee");
    }
    for row in before.tables.get_mut("issues").expect("issues table") {
        row.remove("assignee");
    }
    undo_v5_schema(&mut after, &mut before);
    undo_v6_schema(&mut after);
    undo_v7_schema(&mut after);
    common::undo_v12_schema(&mut after, &before);
    undo_v11_schema(&mut after, &before);
    undo_v10_schema(&mut after);
    undo_v9_schema(&mut after);
    undo_v8_schema(&mut after);
    for d in [&mut before, &mut after] {
        d.schema.retain(|r| r["name"] != t("issues"));
        d.tables
            .retain(|t, _| !t.starts_with("issues_fts") && t != "labels" && t != "issue_labels");
    }
    assert_eq!(after, before);
    fts_integrity(&db, &FTS_TABLES);
}

fn label_rows(db: &Path) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.prepare("SELECT id, name, color FROM labels ORDER BY rowid")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .and_then(Iterator::collect)
        })
        .expect("labels")
}

/// issue_labels.label_id values for issue `n`, as stored.
fn issue_label_raw(db: &Path, n: i64) -> Vec<SqlValue> {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.prepare("SELECT il.label_id FROM issue_labels il JOIN issues i ON i.id = il.issue_id WHERE i.number = ?1 ORDER BY il.rowid")
        .and_then(|mut s| s.query_map([n], |r| r.get(0)).and_then(Iterator::collect))
        .expect("issue_labels")
}
