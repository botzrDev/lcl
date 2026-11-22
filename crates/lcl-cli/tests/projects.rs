//! Projects and milestones inside a workspace: dreamd's Linear projects
//! (dreamd-hardening, dreamd-eng, …) and milestones (1.3.1 patch, …), and
//! each issue's place in them. The schema holds every writer to the rules,
//! not only lcl: an import may insert rows directly.
//!
//! tests/fixtures/v7.sqlite was made by a build of 097af5c (`git archive`,
//! built in its own target dir: schema v7), with HOME set to a temp dir,
//! LCL_PROJECT and LCL_WORKSPACE unset, from an empty temp cwd:
//!
//!     lcl init seven
//!     lcl label create bug
//!     lcl issue create --title "first" --label bug
//!     lcl issue create --title "second" --parent 1 --status Done
//!     lcl issue create --title "third" --status InProgress
//!     lcl issue relate 1 blocks 2
//!     lcl issue relate 3 duplicate-of 1
//!     lcl issue comment 1 "a comment"
//!     lcl doc create --title Notes --content "doc body"
//!
//! and then .lcl/data.sqlite was copied. It is at user_version 7 in WAL
//! mode, with no -wal or -shm file. Tests copy it; never open it in place.

mod common;

use common::{
    Env, FTS_TABLES, Out, V0_INDEX_FIXTURE, V7_FIXTURE, argv, dump, error_text, exec, files,
    foreign_key_violations, fts_integrity, undo_v8_schema, undo_v9_schema, undo_v10_schema,
    undo_v11_schema, user_version,
};
use lcl_core::SCHEMA_VERSION;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

/// A workspace (key DREA) in the env's cwd.
fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    env
}

/// Exit code, stdout and stderr, for exact comparisons.
fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

/// An object's keys, sorted.
fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect();
    k.sort();
    k
}

const PROJECT_KEYS: [&str; 7] = [
    "created_at",
    "description",
    "id",
    "name",
    "status",
    "target_date",
    "updated_at",
];
const MILESTONE_KEYS: [&str; 8] = [
    "created_at",
    "description",
    "id",
    "name",
    "project",
    "sort_order",
    "target_date",
    "updated_at",
];

/// Issue counts with every status zero.
fn no_issues() -> Value {
    json!({"total": 0, "by_status": {"Backlog": 0, "Todo": 0, "InProgress": 0, "InReview": 0, "Done": 0, "Canceled": 0, "Duplicate": 0, "unreadable": 0}})
}

fn project(env: &Env, name: &str) -> Value {
    env.lcl(&["project", "create", name, "--json"])
        .assert_ok()
        .json()
}

fn milestone(env: &Env, name: &str, project: &str) -> Value {
    env.lcl(&["milestone", "create", name, "--project", project, "--json"])
        .assert_ok()
        .json()
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_string()
}

/// Issue `n` as `issue show --json` has it.
fn issue(env: &Env, n: i64) -> Value {
    env.lcl(&["issue", "show", &n.to_string(), "--json"])
        .assert_ok()
        .json()["issue"]
        .clone()
}

/// Issue `n`'s project and milestone, by name.
fn place(env: &Env, n: i64) -> (Value, Value) {
    let i = issue(env, n);
    (i["project"].clone(), i["milestone"].clone())
}

/// One value from SQL run directly on `db`.
fn query<T: rusqlite::types::FromSql>(db: &Path, sql: &str) -> T {
    let conn = Connection::open(db).expect("open db");
    conn.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The numbers of the issues in an `issue list --json` result, in order.
fn numbers(list: &Value) -> Vec<i64> {
    list["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"))
        .iter()
        .map(|i| i["number"].as_i64().expect("number"))
        .collect()
}

#[test]
fn upgrades_a_v7_workspace() {
    let env = Env::new();
    let db = env.install_db(V7_FIXTURE);
    assert_eq!(user_version(&db), 7);
    let before = dump(&db);

    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(user_version(&db), SCHEMA_VERSION);
    let seen: Vec<_> = list["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| {
            (
                i["identifier"].clone(),
                i["project"].clone(),
                i["milestone"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (json!("SEVE-3"), Value::Null, Value::Null),
            (json!("SEVE-2"), Value::Null, Value::Null),
            (json!("SEVE-1"), Value::Null, Value::Null)
        ]
    );

    // Undoing V10's expected changes (none), V9's and V8's gives back the
    // v7 file exactly, the issues table's CREATE statement included.
    let mut after = dump(&db);
    common::undo_v12_schema(&mut after, &before);
    undo_v11_schema(&mut after, &before);
    undo_v10_schema(&mut after);
    undo_v9_schema(&mut after);
    undo_v8_schema(&mut after);
    assert_eq!(after, before);
    assert_eq!(foreign_key_violations(&db), vec![]);
    fts_integrity(&db, &FTS_TABLES);

    // The upgraded file has what v7 had, and takes projects.
    let one = env
        .lcl(&["issue", "show", "SEVE-1", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &one["relations"]["blocks"][0]["identifier"],
            &one["issue"]["labels"],
            &one["comments"][0]["body"]
        ),
        (&json!("SEVE-2"), &json!(["bug"]), &json!("a comment"))
    );
    project(&env, "p");
    milestone(&env, "m", "p");
    let moved = env
        .lcl(&[
            "issue",
            "update",
            "SEVE-2",
            "--project",
            "p",
            "--milestone",
            "m",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &moved["project"],
            &moved["milestone"],
            &moved["parent_identifier"]
        ),
        (&json!("p"), &json!("m"), &json!("SEVE-1"))
    );
    assert_eq!(foreign_key_violations(&db), vec![]);
}

#[test]
fn projects_have_unique_names_ignoring_case_and_canonical_statuses() {
    let env = workspace();
    let db = env.db_path();
    let args = [
        "project",
        "create",
        "  dreamd-hardening ",
        "--status",
        "in progress",
        "--target-date",
        "2026-12-01",
        "--description",
        "Harden dreamd.",
        "--json",
    ];
    let made = env.lcl(&args).assert_ok().json();
    assert_eq!(keys(&made), PROJECT_KEYS);
    assert_eq!(
        (
            &made["name"],
            &made["status"],
            &made["target_date"],
            &made["description"]
        ),
        (
            &json!("dreamd-hardening"),
            &json!("Started"),
            &json!("2026-12-01"),
            &json!("Harden dreamd.")
        )
    );
    assert!(
        made["created_at"].is_i64() && made["created_at"] == made["updated_at"],
        "{made}"
    );
    let pid = id(&made);

    // Case and spaces are ignored, In Progress is Started; stored canonically.
    let spellings = [
        ("backlog", "Backlog"),
        ("PLANNED", "Planned"),
        ("Started", "Started"),
        ("In Progress", "Started"),
        ("inprogress", "Started"),
        ("in_progress", "Started"),
        ("in-progress", "Started"),
        (" paused ", "Paused"),
        ("Completed", "Completed"),
        ("canceled", "Canceled"),
    ];
    for (i, (given, stored)) in spellings.iter().enumerate() {
        let name = format!("s{i}");
        let out = env
            .lcl(&["project", "create", &name, "--status", given, "--json"])
            .assert_ok()
            .json();
        assert_eq!(out["status"], *stored, "{given:?}");
        assert_eq!(
            query::<String>(
                &db,
                &format!("SELECT status FROM projects WHERE name = '{name}'")
            ),
            *stored,
            "{given:?}"
        );
    }
    let count = || query::<i64>(&db, "SELECT count(*) FROM projects");
    assert_eq!(count(), 11);

    // Refused: an unknown status, a blank name, a bad date (exit 2); a name
    // taken in any case (exit 1, naming the project that has it).
    let out = env.lcl(&["project", "create", "z", "--status", "Finished"]);
    let want = "error: invalid value 'Finished' for '--status <STATUS>': invalid project status \"Finished\"; expected one of: Backlog, Planned, Started, Paused, Completed, Canceled\n";
    assert!(out.code == 2 && out.stderr.starts_with(want), "{out:#?}");
    for blank in ["", "   "] {
        let out = env.lcl(&["project", "create", blank]);
        assert!(
            out.code == 2 && out.stderr.contains("a project name can't be empty"),
            "{out:#?}"
        );
    }
    for date in ["2026-02-30", "2026-2-3", "tomorrow"] {
        let out = env.lcl(&["project", "create", "z", "--target-date", date]);
        assert!(
            out.code == 2
                && out.stderr.contains(&format!(
                    "invalid date \"{date}\"; expected a real calendar date as YYYY-MM-DD"
                )),
            "{out:#?}"
        );
    }
    let out = env.lcl(&["project", "create", "DREAMD-Hardening"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"DREAMD-Hardening\" already exists (as \"dreamd-hardening\")\n"
        )
    );
    let out = env.lcl(&["project", "create", "dreamd-hardening"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"dreamd-hardening\" already exists\n"
        )
    );
    assert_eq!(count(), 11, "nothing refused was written");

    // update, by name in any case or by id.
    let out = env
        .lcl(&[
            "project",
            "update",
            "DREAMD-HARDENING",
            "--no-target-date",
            "--description",
            "",
            "--status",
            "paused",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(keys(&out), PROJECT_KEYS);
    assert_eq!(
        (
            &out["id"],
            &out["target_date"],
            &out["description"],
            &out["status"]
        ),
        (&json!(pid), &Value::Null, &json!(""), &json!("Paused"))
    );
    assert!(
        out["updated_at"].as_i64() >= made["created_at"].as_i64(),
        "{out}"
    );
    assert_eq!(
        env.lcl(&[
            "project",
            "update",
            &pid,
            "--target-date",
            "2027-01-31",
            "--no-target-date"
        ])
        .code,
        2
    );
    let out = env
        .lcl(&[
            "project",
            "update",
            "s0",
            "--target-date",
            "2027-01-31",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(out["target_date"], "2027-01-31");
    // Its own name in another case is fine; another project's isn't.
    let renamed = env
        .lcl(&[
            "project",
            "update",
            &pid,
            "--name",
            " Dreamd-Hardening ",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&renamed["id"], &renamed["name"]),
        (&json!(pid), &json!("Dreamd-Hardening"))
    );
    let out = env.lcl(&["project", "update", "s0", "--name", "DREAMD-HARDENING"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"DREAMD-HARDENING\" already exists (as \"Dreamd-Hardening\")\n"
        )
    );
    let out = env.lcl(&["project", "update", "nope", "--status", "planned"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"nope\" not found in this workspace\n"
        )
    );

    // list: by name ignoring case; a repeated --status matches any.
    project(&env, "alpha");
    project(&env, "Beta");
    let list = env.lcl(&["project", "list", "--json"]).assert_ok().json();
    let all = list.as_array().expect("array");
    let names: Vec<&str> = all
        .iter()
        .map(|p| p["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "alpha",
            "Beta",
            "Dreamd-Hardening",
            "s0",
            "s1",
            "s2",
            "s3",
            "s4",
            "s5",
            "s6",
            "s7",
            "s8",
            "s9"
        ]
    );
    for p in all {
        assert_eq!(keys(p), PROJECT_KEYS);
        assert!(p["created_at"].is_i64() && p["updated_at"].is_i64(), "{p}");
    }
    let some = env
        .lcl(&[
            "project", "list", "--status", "STARTED", "--status", "paused", "--json",
        ])
        .assert_ok()
        .json();
    let names: Vec<&str> = some
        .as_array()
        .expect("array")
        .iter()
        .map(|p| p["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        ["Dreamd-Hardening", "s2", "s3", "s4", "s5", "s6", "s7"]
    );

    // show: by id or by name in any case.
    let by_id = env
        .lcl(&["project", "show", &pid, "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        by_id,
        env.lcl(&["project", "show", "dreamd-HARDENING", "--json"])
            .assert_ok()
            .json()
    );
    assert_eq!(
        keys(&by_id),
        [
            "created_at",
            "description",
            "id",
            "issues",
            "milestones",
            "name",
            "status",
            "target_date",
            "updated_at"
        ]
    );
    assert_eq!(
        (&by_id["issues"], &by_id["milestones"]),
        (&no_issues(), &json!([]))
    );
    let out = env.lcl(&["project", "show", "nope"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"nope\" not found in this workspace\n"
        )
    );
}

#[test]
fn milestones_are_unique_in_their_project_and_stay_there() {
    let env = workspace();
    project(&env, "dreamd-hardening");
    project(&env, "dreamd-eng");
    let args = [
        "milestone",
        "create",
        " 1.3.1 patch ",
        "--project",
        "DREAMD-HARDENING",
        "--target-date",
        "2026-11-20",
        "--sort-order",
        "-3",
        "--description",
        "The patch.",
        "--json",
    ];
    let m = env.lcl(&args).assert_ok().json();
    assert_eq!(keys(&m), MILESTONE_KEYS);
    assert_eq!(
        (
            &m["name"],
            &m["project"],
            &m["target_date"],
            &m["sort_order"],
            &m["description"]
        ),
        (
            &json!("1.3.1 patch"),
            &json!("dreamd-hardening"),
            &json!("2026-11-20"),
            &json!(-3),
            &json!("The patch.")
        )
    );
    assert!(
        m["created_at"].is_i64() && m["created_at"] == m["updated_at"],
        "{m}"
    );
    let mid = id(&m);

    // Unique in its project ignoring case; the same name in another project
    // is another milestone.
    let out = env.lcl(&[
        "milestone",
        "create",
        "1.3.1 PATCH",
        "--project",
        "dreamd-hardening",
    ]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: milestone \"1.3.1 PATCH\" already exists in project \"dreamd-hardening\" (as \"1.3.1 patch\")\n"
        )
    );
    let other = milestone(&env, "1.3.1 patch", "dreamd-eng");
    assert_ne!(other["id"], m["id"]);
    let out = env.lcl(&["milestone", "create", "x", "--project", "nope"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"nope\" not found in this workspace\n"
        )
    );
    for args in [
        vec!["milestone", "create", "  ", "--project", "dreamd-eng"],
        vec!["milestone", "create", "x"],
        vec![
            "milestone",
            "create",
            "x",
            "--project",
            "dreamd-eng",
            "--sort-order",
            "1.5",
        ],
        vec![
            "milestone",
            "create",
            "x",
            "--project",
            "dreamd-eng",
            "--target-date",
            "2026-13-01",
        ],
    ] {
        assert_eq!(env.lcl(&args).code, 2, "{args:?}");
    }

    // A name two projects have needs --project; an id never does.
    let out = env.lcl(&["milestone", "show", "1.3.1 patch"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: milestone \"1.3.1 patch\" is in projects \"dreamd-eng\" and \"dreamd-hardening\"; pass --project to choose\n"
        )
    );
    let shown = env
        .lcl(&[
            "milestone",
            "show",
            "1.3.1 Patch",
            "--project",
            "dreamd-hardening",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        keys(&shown),
        [
            "created_at",
            "description",
            "id",
            "issues",
            "name",
            "project",
            "sort_order",
            "target_date",
            "updated_at"
        ]
    );
    assert_eq!(
        (&shown["id"], &shown["issues"]),
        (&json!(mid), &no_issues())
    );
    assert_eq!(
        env.lcl(&["milestone", "show", &mid, "--json"])
            .assert_ok()
            .json(),
        shown
    );
    for (args, want) in [
        (
            vec!["milestone", "show", "nope"],
            "Error: milestone \"nope\" not found\n".to_string(),
        ),
        (
            vec!["milestone", "show", "nope", "--project", "dreamd-eng"],
            "Error: milestone \"nope\" not found in project \"dreamd-eng\"\n".to_string(),
        ),
        (
            vec!["milestone", "show", &mid, "--project", "dreamd-eng"],
            format!("Error: milestone \"{mid}\" not found in project \"dreamd-eng\"\n"),
        ),
        (
            vec!["milestone", "update", "nope", "--sort-order", "1"],
            "Error: milestone \"nope\" not found\n".to_string(),
        ),
        (
            vec!["milestone", "delete", "nope", "--project", "dreamd-eng"],
            "Error: milestone \"nope\" not found in project \"dreamd-eng\"\n".to_string(),
        ),
        (
            vec!["milestone", "show", "x", "--project", "nope"],
            "Error: project \"nope\" not found in this workspace\n".to_string(),
        ),
    ] {
        assert_eq!(trio(&env.lcl(&args)), (1, "", want.as_str()), "{args:?}");
    }

    // update: name, sort order, target date. Nothing moves it to another
    // project: --project only says where to look.
    let out = env
        .lcl(&[
            "milestone",
            "update",
            &mid,
            "--name",
            "1.3.2 patch",
            "--sort-order",
            "7",
            "--no-target-date",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(keys(&out), MILESTONE_KEYS);
    assert_eq!(
        (
            &out["id"],
            &out["name"],
            &out["project"],
            &out["sort_order"],
            &out["target_date"],
            &out["description"]
        ),
        (
            &json!(mid),
            &json!("1.3.2 patch"),
            &json!("dreamd-hardening"),
            &json!(7),
            &Value::Null,
            &json!("The patch.")
        )
    );
    milestone(&env, "1.4", "dreamd-hardening");
    let out = env.lcl(&["milestone", "update", "1.3.2 patch", "--name", "1.4"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: milestone \"1.4\" already exists in project \"dreamd-hardening\"\n"
        )
    );
    env.lcl(&[
        "milestone",
        "update",
        "1.3.2 patch",
        "--name",
        "1.3.2 PATCH",
    ])
    .assert_ok();
    // "1.3.1 patch" is now only dreamd-eng's; "1.4" there is free.
    let out = env
        .lcl(&[
            "milestone",
            "update",
            "1.3.1 patch",
            "--name",
            "1.4",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&out["id"], &out["project"]),
        (&other["id"], &json!("dreamd-eng"))
    );
    assert_eq!(
        env.lcl(&[
            "milestone",
            "update",
            "1.4",
            "--project",
            "dreamd-eng",
            "--target-date",
            "2026-01-01",
            "--no-target-date"
        ])
        .code,
        2
    );
    let help = env
        .lcl(&["milestone", "update", "--help"])
        .assert_ok()
        .stdout
        .clone();
    assert!(help.contains("It can't move to another project"), "{help}");

    // list: by project name, then sort order, then name ignoring case.
    for (name, order) in [("b", "0"), ("A", "0"), ("z", "-1")] {
        env.lcl(&[
            "milestone",
            "create",
            name,
            "--project",
            "dreamd-eng",
            "--sort-order",
            order,
        ])
        .assert_ok();
    }
    let list = env.lcl(&["milestone", "list", "--json"]).assert_ok().json();
    let all = list.as_array().expect("array");
    let seen: Vec<(&str, &str)> = all
        .iter()
        .map(|m| {
            (
                m["project"].as_str().expect("project"),
                m["name"].as_str().expect("name"),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("dreamd-eng", "z"),
            ("dreamd-eng", "1.4"),
            ("dreamd-eng", "A"),
            ("dreamd-eng", "b"),
            ("dreamd-hardening", "1.4"),
            ("dreamd-hardening", "1.3.2 PATCH")
        ]
    );
    for m in all {
        assert_eq!(keys(m), MILESTONE_KEYS);
    }
    let one = env
        .lcl(&[
            "milestone",
            "list",
            "--project",
            "DREAMD-hardening",
            "--json",
        ])
        .assert_ok()
        .json();
    let names: Vec<&str> = one
        .as_array()
        .expect("array")
        .iter()
        .map(|m| m["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["1.4", "1.3.2 PATCH"]);
    let out = env.lcl(&["milestone", "list", "--project", "nope"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"nope\" not found in this workspace\n"
        )
    );
}

#[test]
fn deletes_are_refused_while_anything_is_in_them() {
    let env = workspace();
    let db = env.db_path();
    // a: an issue. b: a milestone. c: two milestones. d: three issues and
    // two milestones, one milestone with an issue.
    for p in ["a", "b", "c", "d"] {
        project(&env, p);
    }
    for (m, p) in [
        ("b1", "b"),
        ("c1", "c"),
        ("c2", "c"),
        ("d1", "d"),
        ("d2", "d"),
    ] {
        milestone(&env, m, p);
    }
    env.lcl(&["issue", "create", "--title", "in a", "--project", "a"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "d one", "--project", "d"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "d two", "--project", "d"])
        .assert_ok();
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "d three",
        "--project",
        "d",
        "--milestone",
        "d1",
    ])
    .assert_ok();

    for (args, refusal) in [
        (
            vec!["project", "delete", "a"],
            "project \"a\" has 1 issue; move or delete it first",
        ),
        (
            vec!["project", "delete", "B"],
            "project \"b\" has 1 milestone; move or delete it first",
        ),
        (
            vec!["project", "delete", "c"],
            "project \"c\" has 2 milestones; move or delete them first",
        ),
        (
            vec!["project", "delete", "d"],
            "project \"d\" has 3 issues and 2 milestones; move or delete them first",
        ),
        (
            vec!["milestone", "delete", "d1"],
            "milestone \"d1\" has 1 issue; move or delete it first",
        ),
        (
            vec!["--json", "milestone", "delete", "D1", "--project", "d"],
            "milestone \"d1\" has 1 issue; move or delete it first",
        ),
    ] {
        let before = dump(&db);
        let out = env.lcl(&args);
        assert_eq!(
            trio(&out),
            (1, "", error_text(&args, "conflict", refusal).as_str()),
            "{args:?}"
        );
        assert!(dump(&db) == before, "{args:?} changed the database");
    }

    // Once nothing is in them, they go.
    let a = query::<String>(&db, "SELECT id FROM projects WHERE name = 'a'");
    let b1 = query::<String>(&db, "SELECT id FROM milestones WHERE name = 'b1'");
    env.lcl(&["issue", "update", "1", "--no-project"])
        .assert_ok();
    assert_eq!(
        env.lcl(&["project", "delete", "A", "--json"])
            .assert_ok()
            .json(),
        json!({"deleted": "a", "id": a})
    );
    assert_eq!(
        env.lcl(&["milestone", "delete", "b1", "--json"])
            .assert_ok()
            .json(),
        json!({"deleted": "b1", "id": b1, "project": "b"})
    );
    assert_eq!(
        trio(&env.lcl(&["project", "delete", "b"])),
        (0, "Deleted project \"b\"\n", "")
    );
    env.lcl(&["issue", "update", "4", "--no-milestone"])
        .assert_ok();
    assert_eq!(
        trio(&env.lcl(&["milestone", "delete", "d1"])),
        (0, "Deleted milestone \"d1\" from project \"d\"\n", "")
    );
    // Deleting its issues doesn't empty a project: they are in the trash,
    // still in it, and the refusal says so. Restored, they can be moved.
    for n in ["2", "3", "4"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }
    assert_eq!(
        trio(&env.lcl(&["project", "delete", "d"])),
        (
            1,
            "",
            "Error: project \"d\" has 3 issues and 1 milestone; move or delete them first (3 are in the trash: lcl issue list --trashed shows them; restore them to move them, or purge them)\n"
        )
    );
    for n in ["2", "3", "4"] {
        env.lcl(&["issue", "restore", n]).assert_ok();
        env.lcl(&["issue", "update", n, "--no-project"]).assert_ok();
    }
    env.lcl(&["milestone", "delete", "d2"]).assert_ok();
    env.lcl(&["project", "delete", "d"]).assert_ok();
    assert_eq!(
        query::<String>(
            &db,
            "SELECT group_concat(name, ',') FROM (SELECT name FROM projects ORDER BY name)"
        ),
        "c"
    );
    assert_eq!(
        query::<String>(
            &db,
            "SELECT group_concat(name, ',') FROM (SELECT name FROM milestones ORDER BY name)"
        ),
        "c1,c2"
    );
    assert_eq!(
        trio(&env.lcl(&["project", "delete", "a"])),
        (1, "", "Error: project \"a\" not found in this workspace\n")
    );
    assert_eq!(
        trio(&env.lcl(&["milestone", "delete", "b1"])),
        (1, "", "Error: milestone \"b1\" not found\n")
    );
    assert_eq!(foreign_key_violations(&db), vec![]);
}

#[test]
fn an_issues_milestone_is_always_in_its_project() {
    let env = workspace();
    let db = env.db_path();
    for p in ["dreamd-hardening", "dreamd-eng", "other"] {
        project(&env, p);
    }
    for (m, p) in [
        ("1.3.1 patch", "dreamd-hardening"),
        ("eng-m", "dreamd-eng"),
        ("x", "dreamd-eng"),
        ("X", "other"),
    ] {
        milestone(&env, m, p);
    }

    // The prove-out's B4: an issue in a project and one of its milestones.
    let made = env
        .lcl(&[
            "issue",
            "create",
            "--title",
            "x",
            "--project",
            "DREAMD-hardening",
            "--milestone",
            "1.3.1 PATCH",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&made["identifier"], &made["project"], &made["milestone"]),
        (
            &json!("DREA-1"),
            &json!("dreamd-hardening"),
            &json!("1.3.1 patch")
        )
    );
    assert!(
        made.get("project_id").is_none() && made.get("milestone_id").is_none(),
        "names, not ids: {made}"
    );
    assert_eq!(
        place(&env, 1),
        (json!("dreamd-hardening"), json!("1.3.1 patch"))
    );
    let listed = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(
        (
            &listed["issues"][0]["project"],
            &listed["issues"][0]["milestone"]
        ),
        (&json!("dreamd-hardening"), &json!("1.3.1 patch"))
    );

    // A milestone must be in the issue's project, and there must be one: a
    // new issue's milestone needs --project (exit 2).
    let needs = "error: the following required arguments were not provided:\n  --project <PROJECT>\n\nUsage: lcl issue create --title <TITLE> --project <PROJECT> --milestone <MILESTONE>\n\nFor more information, try '--help'.\n";
    let elsewhere = |p: &str| {
        format!(
            "Error: milestone \"eng-m\" is not in project \"{p}\" (it is in project \"dreamd-eng\")\n"
        )
    };
    for (args, want) in [
        (vec!["issue", "create", "--title", "y", "--milestone", "eng-m"], (2, needs.to_string())),
        (vec!["issue", "create", "--title", "y", "--project", "dreamd-hardening", "--milestone", "eng-m"], (1, elsewhere("dreamd-hardening"))),
        (
            vec!["issue", "create", "--title", "y", "--project", "dreamd-hardening", "--milestone", "x"],
            (1, "Error: milestone \"x\" is not in project \"dreamd-hardening\" (it is in projects \"dreamd-eng\" and \"other\")\n".to_string()),
        ),
        (vec!["issue", "create", "--title", "y", "--project", "dreamd-hardening", "--milestone", "nowhere"], (1, "Error: milestone \"nowhere\" not found in project \"dreamd-hardening\"\n".to_string())),
        (vec!["issue", "update", "1", "--milestone", "eng-m"], (1, elsewhere("dreamd-hardening"))),
        (vec!["issue", "update", "1", "--project", "other", "--milestone", "eng-m"], (1, elsewhere("other"))),
        (vec!["issue", "update", "1", "--milestone", "nowhere"], (1, "Error: milestone \"nowhere\" not found in project \"dreamd-hardening\"\n".to_string())),
    ] {
        let before = dump(&db);
        assert_eq!(trio(&env.lcl(&args)), (want.0, "", want.1.as_str()), "{args:?}");
        assert!(dump(&db) == before, "{args:?} wrote");
    }
    for args in [
        vec![
            "issue",
            "create",
            "--title",
            "y",
            "--no-project",
            "--milestone",
            "eng-m",
        ],
        vec![
            "issue",
            "update",
            "1",
            "--no-project",
            "--milestone",
            "eng-m",
        ],
        vec!["issue", "update", "1", "--project", "other", "--no-project"],
        vec!["issue", "update", "1", "--milestone", "x", "--no-milestone"],
    ] {
        assert_eq!(env.lcl(&args).code, 2, "{args:?}");
    }
    assert_eq!(query::<i64>(&db, "SELECT count(*) FROM issues"), 1);

    // Changing the project clears a milestone of the old one, with a note...
    let note = |m: &str, p: &str| {
        format!("lcl: milestone \"{m}\" cleared: it belongs to project \"{p}\"\n")
    };
    let updated = "Updated DREA-1\n";
    assert_eq!(
        trio(&env.lcl(&["issue", "update", "1", "--project", "dreamd-eng"])),
        (0, updated, note("1.3.1 patch", "dreamd-hardening").as_str())
    );
    assert_eq!(place(&env, 1), (json!("dreamd-eng"), Value::Null));
    // ...unless --milestone gives the new one.
    assert_eq!(
        trio(&env.lcl(&[
            "issue",
            "update",
            "1",
            "--project",
            "dreamd-hardening",
            "--milestone",
            "1.3.1 patch"
        ])),
        (0, updated, "")
    );
    let out = env.lcl(&[
        "issue",
        "update",
        "1",
        "--project",
        "dreamd-eng",
        "--milestone",
        "eng-m",
        "--json",
    ]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    assert_eq!(
        (&out.json()["project"], &out.json()["milestone"]),
        (&json!("dreamd-eng"), &json!("eng-m"))
    );
    // Its own project again keeps the milestone; --no-milestone keeps the project.
    assert_eq!(
        trio(&env.lcl(&["issue", "update", "1", "--project", "DREAMD-ENG"])),
        (0, updated, "")
    );
    assert_eq!(place(&env, 1), (json!("dreamd-eng"), json!("eng-m")));
    assert_eq!(
        trio(&env.lcl(&["issue", "update", "1", "--no-milestone"])),
        (0, updated, "")
    );
    assert_eq!(place(&env, 1), (json!("dreamd-eng"), Value::Null));
    // --milestone alone looks in the issue's project.
    assert_eq!(
        trio(&env.lcl(&["issue", "update", "1", "--milestone", "ENG-M"])),
        (0, updated, "")
    );
    assert_eq!(place(&env, 1), (json!("dreamd-eng"), json!("eng-m")));
    // --no-project clears both, with the note.
    assert_eq!(
        trio(&env.lcl(&["issue", "update", "1", "--no-project"])),
        (0, updated, note("eng-m", "dreamd-eng").as_str())
    );
    assert_eq!(place(&env, 1), (Value::Null, Value::Null));
    // An issue in no project can't take a milestone alone: with a project
    // stored it could, so this exits 1.
    let out = env.lcl(&["issue", "update", "1", "--milestone", "eng-m"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: milestone \"eng-m\" needs a project: the issue has none; pass --project too\n"
        )
    );
    // A move that also drops the milestone has nothing to report.
    env.lcl(&[
        "issue",
        "update",
        "1",
        "--project",
        "dreamd-eng",
        "--milestone",
        "eng-m",
    ])
    .assert_ok();
    assert_eq!(
        trio(&env.lcl(&[
            "issue",
            "update",
            "1",
            "--project",
            "other",
            "--no-milestone"
        ])),
        (0, updated, "")
    );
    assert_eq!(place(&env, 1), (json!("other"), Value::Null));

    // issue show prints both when set, neither when not.
    env.lcl(&["issue", "update", "1", "--milestone", "x"])
        .assert_ok();
    let shown = env.lcl(&["issue", "show", "1"]).assert_ok().stdout.clone();
    assert!(
        shown.contains("\nProject: other\nMilestone: X\n"),
        "{shown}"
    );
    env.lcl(&["issue", "create", "--title", "plain"])
        .assert_ok();
    let plain = env.lcl(&["issue", "show", "2"]).assert_ok().stdout.clone();
    assert!(
        !plain.contains("Project:") && !plain.contains("Milestone:"),
        "{plain}"
    );
    let two = issue(&env, 2);
    assert_eq!(
        (&two["project"], &two["milestone"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(foreign_key_violations(&db), vec![]);
}

/// The rules hold for a writer that skips lcl, as an import would.
#[test]
fn the_schema_holds_writers_that_skip_lcl() {
    let env = workspace();
    let db = env.db_path();
    let p = id(&project(&env, "p"));
    let q = id(&project(&env, "q"));
    let pm = id(&milestone(&env, "pm", "p"));
    let qm = id(&milestone(&env, "qm", "q"));
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "one",
        "--project",
        "p",
        "--milestone",
        "pm",
    ])
    .assert_ok();

    let conn = Connection::open(&db).expect("open db");
    conn.pragma_update(None, "foreign_keys", true)
        .expect("foreign keys on");
    let run = |sql: &str| conn.execute_batch(sql).map_err(|e| e.to_string());
    let insert = |n: i64, project: &str, milestone: &str| {
        run(&format!(
            "INSERT INTO issues(id, number, title, created_at, updated_at, project_id, milestone_id) VALUES ('hand-{n}', {n}, 't', 1, 1, {project}, {milestone})"
        ))
    };
    let quoted = |s: &str| format!("'{s}'");
    let not_in_project = Err("milestone is not in the issue's project".to_string());

    // Trigger (a), on INSERT and on UPDATE of either column.
    assert_eq!(insert(90, &quoted(&q), &quoted(&pm)), not_in_project);
    assert_eq!(insert(91, "NULL", &quoted(&pm)), not_in_project);
    assert_eq!(
        insert(92, &quoted(&p), &quoted("no-such-milestone")),
        not_in_project
    );
    assert_eq!(insert(93, &quoted(&p), &quoted(&pm)), Ok(()));
    assert_eq!(insert(94, &quoted(&q), "NULL"), Ok(()));
    assert_eq!(insert(95, "NULL", "NULL"), Ok(()));
    let one = "WHERE number = 1";
    assert_eq!(
        run(&format!("UPDATE issues SET project_id = '{q}' {one}")),
        not_in_project
    );
    assert_eq!(
        run(&format!("UPDATE issues SET milestone_id = '{qm}' {one}")),
        not_in_project
    );
    assert_eq!(
        run(&format!("UPDATE issues SET project_id = NULL {one}")),
        not_in_project
    );
    assert_eq!(
        run(&format!(
            "UPDATE issues SET project_id = '{q}', milestone_id = '{qm}' {one}"
        )),
        Ok(())
    );
    assert_eq!(
        run(&format!("UPDATE issues SET title = 'renamed' {one}")),
        Ok(())
    );
    assert_eq!(
        run(&format!("UPDATE issues SET milestone_id = NULL {one}")),
        Ok(())
    );

    // Trigger (b): a milestone never changes project.
    let moved = Err("a milestone cannot move to another project".to_string());
    assert_eq!(
        run(&format!(
            "UPDATE milestones SET project_id = '{q}' WHERE id = '{pm}'"
        )),
        moved
    );
    assert_eq!(
        run(&format!(
            "UPDATE milestones SET project_id = project_id, name = 'pm2' WHERE id = '{pm}'"
        )),
        Ok(())
    );

    // Names unique ignoring case; statuses canonical; nothing deleted out
    // from under what references it.
    let row = |id: &str, name: &str, status: &str| {
        run(&format!(
            "INSERT INTO projects(id, name, status, created_at, updated_at) VALUES ('{id}', '{name}', '{status}', 1, 1)"
        ))
    };
    assert_eq!(
        row("hand-p", "P", "Backlog"),
        Err("UNIQUE constraint failed: projects.name".to_string())
    );
    assert_eq!(row("hand-p", "r", "started"), Err("CHECK constraint failed: status IN ('Backlog', 'Planned', 'Started', 'Paused', 'Completed', 'Canceled')".to_string()));
    assert_eq!(row("hand-p", "r", "Started"), Ok(()));
    let ms = |id: &str, project: &str, name: &str| {
        run(&format!(
            "INSERT INTO milestones(id, project_id, name, created_at, updated_at) VALUES ('{id}', '{project}', '{name}', 1, 1)"
        ))
    };
    assert_eq!(
        ms("hand-m", &q, "QM"),
        Err("UNIQUE constraint failed: milestones.project_id, milestones.name".to_string())
    );
    assert_eq!(
        ms("hand-m", "no-such-project", "x"),
        Err("FOREIGN KEY constraint failed".to_string())
    );
    assert_eq!(ms("hand-m", &p, "QM"), Ok(()));
    let fk = Err("FOREIGN KEY constraint failed".to_string());
    assert_eq!(run(&format!("DELETE FROM projects WHERE id = '{p}'")), fk);
    assert_eq!(
        run(&format!("DELETE FROM milestones WHERE id = '{qm}'")),
        Ok(()),
        "no issue has qm now"
    );
    assert_eq!(
        run(&format!("DELETE FROM milestones WHERE id = '{pm}'")),
        fk,
        "hand-93 has pm"
    );
    assert_eq!(insert(96, &quoted("no-such-project"), "NULL"), fk);
    drop(conn);
    assert_eq!(foreign_key_violations(&db), vec![]);
}

#[test]
fn issue_list_filters_by_project_and_milestone_before_the_limit() {
    let env = workspace();
    let db = env.db_path();
    let p = id(&project(&env, "P"));
    let m = id(&milestone(&env, "M", "P"));
    // 161 issues in P (every 4th in M, every 3rd Done) and 40 in none.
    exec(&db, &format!(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 201)
         INSERT INTO issues(id, number, title, status, created_at, updated_at, project_id, milestone_id)
         SELECT printf('00000000-0000-4000-8000-%012d', i), i, 'issue ' || i, CASE WHEN i % 3 = 0 THEN 'Done' ELSE 'Todo' END, 1000 + i, 1000 + i,
                CASE WHEN i <= 161 THEN '{p}' END, CASE WHEN i <= 161 AND i % 4 = 0 THEN '{m}' END
         FROM n;
         UPDATE meta SET value = '202' WHERE key = 'next_issue_number';"
    ));
    let list = |args: &[&str]| {
        let mut all = vec!["issue", "list", "--json"];
        all.extend_from_slice(args);
        env.lcl(&all).assert_ok().json()
    };

    let page = list(&["--project", "P", "--limit", "50"]);
    assert_eq!(
        (
            &page["total"],
            &page["truncated"],
            &page["limit"],
            &page["offset"]
        ),
        (&json!(161), &json!(true), &json!(50), &json!(0))
    );
    assert_eq!(numbers(&page), (112..=161).rev().collect::<Vec<_>>());
    assert!(
        page["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .all(|i| i["project"] == "P"),
        "{page}"
    );
    for r in ["p", p.as_str()] {
        assert_eq!(list(&["--project", r, "--all"])["total"], 161, "{r}");
    }
    let loose = list(&["--project", "none", "--all"]);
    assert_eq!(numbers(&loose), (162..=201).rev().collect::<Vec<_>>());
    assert!(
        loose["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .all(|i| i["project"].is_null() && i["milestone"].is_null()),
        "{loose}"
    );
    assert_eq!(list(&["--project", " NONE "])["total"], 40);

    // --milestone alone or within --project, combined with --status.
    let done_in_m: Vec<i64> = (1..=161).rev().filter(|i| i % 12 == 0).collect();
    assert_eq!(
        numbers(&list(&["--milestone", "M", "--status", "Done", "--all"])),
        done_in_m
    );
    assert_eq!(
        numbers(&list(&[
            "--project",
            "p",
            "--milestone",
            "m",
            "--status",
            "done",
            "--all"
        ])),
        done_in_m
    );
    assert_eq!(list(&["--milestone", m.as_str()])["total"], 40);
    let first = list(&["--milestone", "M", "--limit", "5"]);
    assert_eq!(
        (numbers(&first), &first["total"]),
        (vec![160, 156, 152, 148, 144], &json!(40))
    );

    // A project named "none" is chosen by its id; --project none still means
    // issues in no project. (Issue create's --project has no `none`.)
    let none = id(&project(&env, "none"));
    env.lcl(&["issue", "create", "--title", "in none", "--project", "none"])
        .assert_ok();
    assert_eq!(numbers(&list(&["--project", none.as_str()])), [202]);
    assert_eq!(list(&["--project", "none"])["total"], 40);
    let help = env
        .lcl(&["issue", "list", "--help"])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        help.contains("A project named \"none\" is selected by its id"),
        "{help}"
    );

    for (args, want) in [
        (
            vec!["issue", "list", "--project", "nope"],
            "Error: project \"nope\" not found in this workspace\n",
        ),
        (
            vec!["issue", "list", "--milestone", "nope"],
            "Error: milestone \"nope\" not found\n",
        ),
        (
            vec!["issue", "list", "--project", "P", "--milestone", "nope"],
            "Error: milestone \"nope\" not found in project \"P\"\n",
        ),
    ] {
        assert_eq!(trio(&env.lcl(&args)), (1, "", want), "{args:?}");
    }
    let out = env.lcl(&["issue", "list", "--project", "none", "--milestone", "M"]);
    let want = "error: --milestone \"M\" can't be used with --project none: issues in no project have no milestone\n";
    assert!(
        out.code == 2 && out.stdout.is_empty() && out.stderr.starts_with(want),
        "{out:#?}"
    );

    // Once two projects have an M, it needs --project.
    milestone(&env, "M", "none");
    let out = env.lcl(&["issue", "list", "--milestone", "m"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: milestone \"m\" is in projects \"none\" and \"P\"; pass --project to choose\n"
        )
    );
    assert_eq!(
        numbers(&list(&[
            "--project",
            "P",
            "--milestone",
            "M",
            "--status",
            "Done",
            "--all"
        ])),
        done_in_m
    );
    assert_eq!(
        list(&["--project", none.as_str(), "--milestone", "M"])["total"],
        0
    );
}

#[test]
fn project_show_counts_issues_by_status_as_reads_do() {
    let env = workspace();
    let db = env.db_path();
    project(&env, "P");
    project(&env, "Q");
    for (name, order) in [
        ("later", "5"),
        ("Beta", "0"),
        ("alpha", "0"),
        ("first", "-2"),
        ("zeta", "5"),
    ] {
        env.lcl(&[
            "milestone",
            "create",
            name,
            "--project",
            "P",
            "--sort-order",
            order,
            "--target-date",
            "2026-12-24",
        ])
        .assert_ok();
    }
    let new = |args: &[&str]| {
        let mut all = vec!["issue", "create", "--title", "t"];
        all.extend_from_slice(args);
        env.lcl(&all).assert_ok();
    };
    new(&["--project", "P", "--milestone", "alpha"]); // 1 Backlog
    new(&["--project", "P", "--milestone", "alpha", "--status", "Todo"]); // 2
    new(&["--project", "P", "--milestone", "Beta", "--status", "Done"]); // 3
    new(&["--project", "P", "--status", "InProgress"]); // 4
    new(&["--project", "P"]); // 5: "done" by hand
    new(&["--project", "P"]); // 6: "Bogus" by hand
    new(&["--status", "Todo"]); // 7: no project
    new(&["--project", "Q", "--status", "Done"]); // 8
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'done' WHERE number = 5; UPDATE issues SET status = 'Bogus' WHERE number = 6;",
    );

    let out = env.lcl(&["project", "show", "p", "--json"]);
    let shown = out.assert_ok().json();
    let by_status = json!({"Backlog": 1, "Todo": 1, "InProgress": 1, "InReview": 0, "Done": 2, "Canceled": 0, "Duplicate": 0, "unreadable": 1});
    assert_eq!(shown["issues"], json!({"total": 6, "by_status": by_status}));
    // In Status order, as printed.
    let at: Vec<usize> = [
        "Backlog",
        "Todo",
        "InProgress",
        "InReview",
        "Done",
        "Canceled",
        "Duplicate",
        "unreadable",
    ]
    .iter()
    .map(|k| {
        out.stdout
            .find(&format!("\"{k}\":"))
            .unwrap_or_else(|| panic!("{k} missing"))
    })
    .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{}", out.stdout);
    let milestones = shown["milestones"].as_array().expect("milestones");
    let seen: Vec<(&str, i64, i64)> = milestones
        .iter()
        .map(|m| {
            (
                m["name"].as_str().expect("name"),
                m["sort_order"].as_i64().expect("order"),
                m["issues"].as_i64().expect("issues"),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("first", -2, 0),
            ("alpha", 0, 2),
            ("Beta", 0, 1),
            ("later", 5, 0),
            ("zeta", 5, 0)
        ]
    );
    for m in milestones {
        assert_eq!(
            keys(m),
            ["id", "issues", "name", "sort_order", "target_date"]
        );
        assert_eq!(m["target_date"], "2026-12-24");
    }
    let human = env
        .lcl(&["project", "show", "P"])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        human.contains("Issues: 6 (Backlog 1, Todo 1, InProgress 1, Done 2, unreadable 1)\n"),
        "{human}"
    );

    let alpha = env
        .lcl(&["milestone", "show", "alpha", "--json"])
        .assert_ok()
        .json();
    let by_status = json!({"Backlog": 1, "Todo": 1, "InProgress": 0, "InReview": 0, "Done": 0, "Canceled": 0, "Duplicate": 0, "unreadable": 0});
    assert_eq!(alpha["issues"], json!({"total": 2, "by_status": by_status}));
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'in review' WHERE number = 2;",
    );
    let alpha = env
        .lcl(&["milestone", "show", "alpha", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &alpha["issues"]["by_status"]["Todo"],
            &alpha["issues"]["by_status"]["InReview"]
        ),
        (&json!(0), &json!(1))
    );
}

#[test]
fn a_workspace_name_given_to_project_gets_the_workspace_hint() {
    let env = Env::new();
    env.lcl(&["workspace", "create", "alpha", "--key", "ALPHA"])
        .assert_ok();
    env.lcl(&["workspace", "create", "R&D", "--key", "RD"])
        .assert_ok();
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    env.lcl(&["issue", "create", "--title", "mine"]).assert_ok();
    let before = files(&env);

    let hint = |args: &[&str], name: &str| {
        error_text(
            args,
            "not_found",
            &format!(
                "project \"{name}\" not found in this workspace; \"{name}\" is a workspace: the workspace flag is now --workspace (-w)"
            ),
        )
    };
    for (args, name) in [
        (
            vec!["issue", "create", "--title", "t", "--project", "alpha"],
            "alpha",
        ),
        (
            vec![
                "--json",
                "issue",
                "create",
                "--title",
                "t",
                "--project",
                "ALPHA",
                "--milestone",
                "m",
            ],
            "ALPHA",
        ),
        (vec!["issue", "update", "1", "--project", "alpha"], "alpha"),
        (vec!["issue", "list", "--project", "alpha"], "alpha"),
        (vec!["issue", "list", "--project", "r-d"], "r-d"),
        (vec!["milestone", "create", "m", "--project", "R&D"], "R&D"),
        (vec!["milestone", "list", "--project", "alpha"], "alpha"),
        (
            vec!["milestone", "show", "m", "--project", "alpha"],
            "alpha",
        ),
        (
            vec![
                "milestone",
                "update",
                "m",
                "--project",
                "alpha",
                "--sort-order",
                "1",
            ],
            "alpha",
        ),
        (
            vec!["milestone", "delete", "m", "--project", "alpha"],
            "alpha",
        ),
    ] {
        assert_eq!(
            trio(&env.lcl(&args)),
            (1, "", hint(&args, name).as_str()),
            "{args:?}"
        );
        assert!(files(&env) == before, "{args:?} changed a file");
    }
    // Neither workspace gained an issue.
    assert_eq!(
        numbers(&env.lcl(&["issue", "list", "--json"]).assert_ok().json()),
        [1]
    );
    assert_eq!(
        env.lcl(&["-w", "alpha", "issue", "list", "--json"])
            .assert_ok()
            .json()["total"],
        0
    );

    // No hint for a name that isn't a workspace's, or without HOME.
    let plain = "Error: project \"beta\" not found in this workspace\n";
    assert_eq!(
        trio(&env.lcl(&["issue", "list", "--project", "beta"])),
        (1, "", plain)
    );
    let out = env.lcl_with(&["issue", "list", "--project", "alpha"], |cmd| {
        cmd.env_remove("HOME");
    });
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: project \"alpha\" not found in this workspace\n"
        )
    );
    // A project named like a workspace is a project.
    project(&env, "alpha");
    let made = env
        .lcl(&[
            "issue",
            "create",
            "--title",
            "t",
            "--project",
            "alpha",
            "--json",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&made["identifier"], &made["project"]),
        (&json!("HERE-2"), &json!("alpha"))
    );
}

/// The hint reads an index lcl 0.1.0 left (its workspaces are in
/// `projects`) without migrating it, or leaving -wal or -shm files.
#[test]
fn the_workspace_hint_reads_an_old_index_without_changing_it() {
    let env = Env::new();
    let index = env.home.path().join(".lcl").join("index.sqlite");
    std::fs::create_dir_all(index.parent().expect("parent")).expect("mkdir");
    std::fs::copy(V0_INDEX_FIXTURE, &index).expect("copy index");
    env.install_db(V7_FIXTURE);
    env.lcl(&["issue", "list"]).assert_ok(); // upgrades the workspace, not the index
    let before = files(&env);

    for name in ["alpha", "R&D", "r-d"] {
        let want = format!(
            "Error: project \"{name}\" not found in this workspace; \"{name}\" is a workspace: the workspace flag is now --workspace (-w)\n"
        );
        assert_eq!(
            trio(&env.lcl(&["issue", "list", "--project", name])),
            (1, "", want.as_str()),
            "{name}"
        );
    }
    assert_eq!(
        trio(&env.lcl(&["issue", "list", "--project", "beta"])),
        (
            1,
            "",
            "Error: project \"beta\" not found in this workspace\n"
        )
    );
    assert!(files(&env) == before, "the index or the workspace changed");
    assert_eq!(user_version(&index), 0);
}

#[test]
fn parallel_issue_updates_all_land() {
    let env = workspace();
    let db = env.db_path();
    project(&env, "P");
    project(&env, "Q");
    milestone(&env, "pm", "P");
    milestone(&env, "qm", "Q");
    for n in 1..=20 {
        env.create(&format!("issue {n}"));
    }
    let to = |n: i64| if n % 2 == 0 { ("P", "pm") } else { ("Q", "qm") };
    let cmds: Vec<Vec<String>> = (1..=20)
        .map(|n| {
            argv(&[
                "issue",
                "update",
                &n.to_string(),
                "--project",
                to(n).0,
                "--milestone",
                to(n).1,
            ])
        })
        .collect();
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "process {i}: {out:#?}");
    }
    for n in 1..=20 {
        assert_eq!(
            place(&env, n),
            (json!(to(n).0), json!(to(n).1)),
            "issue {n}"
        );
    }
    assert_eq!(foreign_key_violations(&db), vec![]);
}

#[test]
fn parallel_creates_of_one_name_make_one() {
    let env = workspace();
    let db = env.db_path();
    let spell = |i: usize| if i.is_multiple_of(2) { "same" } else { "SAME" };
    let cmds: Vec<Vec<String>> = (0..10)
        .map(|i| argv(&["project", "create", spell(i)]))
        .collect();
    let outs = env.lcl_parallel(&cmds);
    let made: Vec<usize> = (0..10).filter(|i| outs[*i].code == 0).collect();
    assert_eq!(made.len(), 1, "{outs:#?}");
    let winner = spell(made[0]);
    for (i, out) in outs.iter().enumerate().filter(|(i, _)| *i != made[0]) {
        let exists = if spell(i) == winner {
            format!("Error: project \"{winner}\" already exists\n")
        } else {
            format!(
                "Error: project \"{}\" already exists (as \"{winner}\")\n",
                spell(i)
            )
        };
        assert_eq!(trio(out), (1, "", exists.as_str()), "process {i}");
    }
    assert_eq!(query::<i64>(&db, "SELECT count(*) FROM projects"), 1);

    let cmds: Vec<Vec<String>> = (0..10usize)
        .map(|i| {
            argv(&[
                "milestone",
                "create",
                if i.is_multiple_of(2) { "m" } else { "M" },
                "--project",
                "same",
            ])
        })
        .collect();
    let outs = env.lcl_parallel(&cmds);
    assert_eq!(outs.iter().filter(|o| o.code == 0).count(), 1, "{outs:#?}");
    // The project is named as the winning create spelled it.
    let exists = format!("already exists in project \"{winner}\"");
    assert!(
        outs.iter()
            .all(|o| o.code == 0 || (o.code == 1 && o.stderr.contains(&exists))),
        "{outs:#?}"
    );
    let cmds: Vec<Vec<String>> = (0..10)
        .map(|i| argv(&["milestone", "create", &format!("m{i}"), "--project", "same"]))
        .collect();
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "process {i}: {out:#?}");
    }
    assert_eq!(query::<i64>(&db, "SELECT count(*) FROM milestones"), 11);
}

/// `project delete P` racing ten `issue update --project P`: either P
/// stays, with every issue that landed, or P is gone and nothing names it.
#[test]
fn a_project_delete_racing_issue_updates_strands_nothing() {
    let env = workspace();
    let db = env.db_path();
    for n in 1..=10 {
        env.create(&format!("issue {n}"));
    }
    let mut cmds: Vec<Vec<String>> = vec![argv(&["project", "delete", "P"])];
    cmds.extend((1..=10).map(|n| argv(&["issue", "update", &n.to_string(), "--project", "P"])));
    let (mut deleted, mut refused) = (0, 0);
    for round in 0..10 {
        exec(
            &db,
            "UPDATE issues SET project_id = NULL, milestone_id = NULL; DELETE FROM projects;",
        );
        project(&env, "P");
        let outs = env.lcl_parallel(&cmds);
        let landed = outs[1..].iter().filter(|o| o.code == 0).count() as i64;
        let in_p = query::<i64>(
            &db,
            "SELECT count(*) FROM issues WHERE project_id IS NOT NULL",
        );
        if outs[0].code == 0 {
            // Deleted first: every update then found no P.
            deleted += 1;
            assert_eq!(
                query::<i64>(&db, "SELECT count(*) FROM projects"),
                0,
                "round {round}"
            );
            assert_eq!((landed, in_p), (0, 0), "round {round}: {outs:#?}");
            for out in &outs[1..] {
                assert_eq!(
                    trio(out),
                    (1, "", "Error: project \"P\" not found in this workspace\n"),
                    "round {round}"
                );
            }
        } else {
            // Refused: some update had landed, and the rest landed after.
            refused += 1;
            let before_delete: i64 = outs[0]
                .stderr
                .strip_prefix("Error: project \"P\" has ")
                .and_then(|s| s.split(' ').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("round {round}: {:#?}", outs[0]));
            assert!(
                (1..=10).contains(&before_delete),
                "round {round}: {:#?}",
                outs[0]
            );
            assert_eq!((landed, in_p), (10, 10), "round {round}: {outs:#?}");
        }
        assert_eq!(foreign_key_violations(&db), vec![], "round {round}");
    }
    eprintln!("10 rounds: P deleted first in {deleted}, delete refused in {refused}");
}

/// Every flag and positional of the project and milestone commands, and
/// the issue commands' new flags, has help text.
#[test]
fn every_new_argument_has_help() {
    let env = Env::new();
    let commands: [&[&str]; 10] = [
        &["project", "create"],
        &["project", "list"],
        &["project", "show"],
        &["project", "update"],
        &["project", "delete"],
        &["milestone", "create"],
        &["milestone", "list"],
        &["milestone", "show"],
        &["milestone", "update"],
        &["milestone", "delete"],
    ];
    for cmd in commands {
        let mut args = cmd.to_vec();
        args.push("-h");
        let help = env.lcl(&args).assert_ok().stdout.clone();
        let mut entries = 0;
        let mut section = false;
        for line in help.lines() {
            if line == "Arguments:" || line == "Options:" {
                section = true;
            } else if line.is_empty() {
                section = false;
            } else if section {
                entries += 1;
                let described = line
                    .trim_start()
                    .split_once("  ")
                    .is_some_and(|(_, text)| !text.trim().is_empty());
                assert!(described, "{cmd:?}: no help for {line:?}\n{help}");
            }
        }
        assert!(entries >= 2, "{cmd:?}\n{help}");
    }
    for cmd in ["create", "update", "list"] {
        let help = env.lcl(&["issue", cmd, "-h"]).assert_ok().stdout.clone();
        for flag in ["--project <PROJECT>", "--milestone <MILESTONE>"]
            .into_iter()
            .chain(if cmd == "list" {
                vec![]
            } else {
                vec!["--no-project", "--no-milestone"]
            })
        {
            let line = help
                .lines()
                .find(|l| l.trim_start().starts_with(flag))
                .unwrap_or_else(|| panic!("issue {cmd}: no {flag}\n{help}"));
            assert!(
                line.trim_start()[flag.len()..].trim().len() > 3,
                "issue {cmd}: no help for {flag}: {line:?}"
            );
        }
    }
}
