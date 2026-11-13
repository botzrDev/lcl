//! Issue refs, creators and comment order. dreamd's Linear tickets are
//! renumbered DRM-1.. on import, and each keeps its Linear id (BZR-1532) as
//! its ref, because commits, AGENTS.md and specs cite those ids. A ref is
//! found by search and named in the wrong-workspace error, but an
//! identifier never resolves through one.
//!
//! tests/fixtures/v8.sqlite was made by a build of 8cffde1 (`git archive`,
//! built in its own target dir: schema v8), with HOME set to a temp dir,
//! LCL_PROJECT, LCL_WORKSPACE and LCL_AUTHOR unset, from an empty temp cwd:
//!
//!     lcl init eight
//!     lcl project create dreamd-hardening --status "in progress"
//!     lcl milestone create "1.3.1 patch" --project dreamd-hardening
//!     lcl issue create --title "first" --project dreamd-hardening --milestone "1.3.1 patch"
//!     lcl issue create --title "second"
//!     lcl issue relate 1 blocks 2
//!     lcl issue comment 1 "a comment" --author alice
//!
//! and then .lcl/data.sqlite was copied. It is at user_version 8 in WAL
//! mode, with no -wal or -shm file. Tests copy it; never open it in place.

mod common;

use common::{argv, dump, exec, files, foreign_key_violations, fts_integrity, undo_v9_schema, user_version, Env, Out, FTS_TABLES, V8_FIXTURE};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

/// A workspace (key DRM) with issues 1..=n, titled "issue N".
fn workspace(n: i64) -> Env {
    let env = Env::new();
    env.lcl(&["init", "drm"]).assert_ok();
    for i in 1..=n {
        env.create(&format!("issue {i}"));
    }
    env
}

/// Exit code, stdout and stderr, for exact comparisons.
fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

/// Issue `n` as `issue show --json` has it.
fn issue(env: &Env, n: i64) -> Value {
    env.lcl(&["issue", "show", &n.to_string(), "--json"]).assert_ok().json()["issue"].clone()
}

/// One value from SQL run directly on `db`.
fn query<T: rusqlite::types::FromSql>(db: &Path, sql: &str) -> T {
    let conn = Connection::open(db).expect("open db");
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The (number, matched_in) of each issue a `search --json` found, in order.
fn hits(found: &Value) -> Vec<(i64, Value)> {
    found["issues"].as_array().unwrap_or_else(|| panic!("{found}")).iter().map(|h| (h["number"].as_i64().expect("number"), h["matched_in"].clone())).collect()
}

#[test]
fn upgrades_a_v8_workspace() {
    let env = Env::new();
    let db = env.install_db(V8_FIXTURE);
    assert_eq!(user_version(&db), 8);
    let before = dump(&db);

    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(user_version(&db), 9);
    let seen: Vec<_> = list["issues"].as_array().expect("issues").iter().map(|i| (i["identifier"].clone(), i["ref"].clone(), i["creator"].clone())).collect();
    assert_eq!(seen, [(json!("EIGH-2"), Value::Null, Value::Null), (json!("EIGH-1"), Value::Null, Value::Null)]);

    // Undoing V9's expected changes gives back the v8 file exactly, the
    // issues table's CREATE statement included.
    let mut after = dump(&db);
    undo_v9_schema(&mut after);
    assert_eq!(after, before);
    assert_eq!(foreign_key_violations(&db), vec![]);
    fts_integrity(&db, &FTS_TABLES);

    // What v8 had is all there, and refs work in the upgraded file.
    let one = env.lcl(&["issue", "show", "EIGH-1", "--json"]).assert_ok().json();
    assert_eq!(
        (&one["issue"]["project"], &one["issue"]["milestone"], &one["relations"]["blocks"][0]["identifier"], &one["comments"][0]["author"], &one["comments"][0]["body"]),
        (&json!("dreamd-hardening"), &json!("1.3.1 patch"), &json!("EIGH-2"), &json!("alice"), &json!("a comment"))
    );
    let two = env.lcl(&["issue", "update", "2", "--ref", "BZR-9", "--json"]).assert_ok().json();
    assert_eq!((&two["ref"], &two["creator"]), (&json!("BZR-9"), &Value::Null), "an update writes no creator");
}

#[test]
fn refs_are_set_changed_and_removed() {
    let env = workspace(3);
    let db = env.db_path();
    let made = env.lcl(&["issue", "create", "--title", "linear", "--ref", " BZR-1532 ", "--json"]).assert_ok().json();
    assert_eq!((&made["identifier"], &made["ref"]), (&json!("DRM-4"), &json!("BZR-1532")));
    assert_eq!(issue(&env, 4)["ref"], "BZR-1532");
    let listed = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(listed["issues"].as_array().expect("issues").iter().map(|i| i["ref"].clone()).collect::<Vec<_>>(), [json!("BZR-1532"), Value::Null, Value::Null, Value::Null]);
    let shown = env.lcl(&["issue", "show", "4"]).assert_ok().stdout.clone();
    assert!(shown.contains("\nRef: BZR-1532\n"), "{shown}");
    assert!(!env.lcl(&["issue", "show", "1"]).assert_ok().stdout.contains("Ref:"));

    // update: set, change, keep (no ref flag), remove.
    assert_eq!(env.lcl(&["issue", "update", "1", "--ref", "BZR-7", "--json"]).assert_ok().json()["ref"], "BZR-7");
    assert_eq!(env.lcl(&["issue", "update", "1", "--ref", "BZR-8", "--json"]).assert_ok().json()["ref"], "BZR-8");
    assert_eq!(env.lcl(&["issue", "update", "1", "--title", "renamed", "--json"]).assert_ok().json()["ref"], "BZR-8");
    assert_eq!(env.lcl(&["issue", "update", "1", "--no-ref", "--json"]).assert_ok().json()["ref"], Value::Null);
    assert_eq!(query::<Option<String>>(&db, "SELECT ref FROM issues WHERE number = 1"), None);
    assert_eq!(env.lcl(&["issue", "update", "1", "--ref", "BZR-1", "--no-ref"]).code, 2);
    // 100 characters is the most.
    let longest = "x".repeat(100);
    assert_eq!(env.lcl(&["issue", "update", "2", "--ref", &longest, "--json"]).assert_ok().json()["ref"], longest);
    // An issue's own ref in another case: allowed, and the new case is stored.
    assert_eq!(trio(&env.lcl(&["issue", "update", "4", "--ref", "bzr-1532"])), (0, "Updated DRM-4\n", ""));
    assert_eq!(query::<String>(&db, "SELECT ref FROM issues WHERE number = 4"), "bzr-1532");
}

#[test]
fn bad_refs_exit_2_and_taken_refs_exit_1_writing_nothing() {
    let env = workspace(3);
    let db = env.db_path();
    env.lcl(&["issue", "update", "1", "--ref", "BZR-1532"]).assert_ok();
    let before = dump(&db);

    let long = "x".repeat(101);
    for (bad, why) in [
        ("", "a ref can't be empty"),
        ("   ", "a ref can't be empty"),
        (long.as_str(), "a ref can be at most 100 characters; this one has 101"),
        ("BZR 1532", "ref \"BZR 1532\" contains whitespace or a control character"),
        ("BZR\t1532", "ref \"BZR\\t1532\" contains whitespace or a control character"),
        ("BZR\u{1}1532", "ref \"BZR\\u{1}1532\" contains whitespace or a control character"),
    ] {
        for args in [vec!["issue", "create", "--title", "t", "--ref", bad], vec!["issue", "update", "2", "--ref", bad]] {
            let out = env.lcl(&args);
            assert!(out.code == 2 && out.stdout.is_empty() && out.stderr.contains(why), "{args:?}: {out:#?}");
        }
    }
    // Shaped like a reference lcl reads as one of this workspace's issues:
    // its KEY-N in any case, N, #N (leading zeros too), and any UUID, an
    // issue's or not.
    let existing = issue(&env, 3)["id"].as_str().expect("id").to_string();
    let shouting = existing.to_uppercase();
    for own in ["drm-5", "DRM-5", "Drm-05", "7", "#7", "007", "#007", &existing, &shouting, "0f8fad5b-d9cb-469f-a165-70867728950e"] {
        let want = format!("Error: ref \"{own}\" looks like one of this workspace's own issue references\n");
        assert_eq!(trio(&env.lcl(&["issue", "create", "--title", "t", "--ref", own])), (2, "", want.as_str()), "{own}");
        assert_eq!(trio(&env.lcl(&["issue", "update", "2", "--ref", own])), (2, "", want.as_str()), "{own}");
    }
    // On another issue, ignoring case: exit 1, naming the holder.
    for (args, given) in [
        (vec!["issue", "update", "2", "--ref", "bzr-1532"], "bzr-1532"),
        (vec!["--json", "issue", "update", "3", "--ref", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "create", "--title", "t", "--ref", "Bzr-1532"], "Bzr-1532"),
    ] {
        let want = format!("Error: ref \"{given}\" is already on DRM-1\n");
        assert_eq!(trio(&env.lcl(&args)), (1, "", want.as_str()), "{args:?}");
    }
    assert!(dump(&db) == before, "a refused ref changed the database");
    // Other keys, and things that aren't identifiers, are fine.
    for ok in ["DRMX-5", "BZR-5", "DRM5"] {
        env.lcl(&["issue", "create", "--title", "t", "--ref", ok]).assert_ok();
    }
}

/// The unique index holds a writer that skips lcl, as an import would.
#[test]
fn the_schema_keeps_refs_unique_ignoring_case() {
    let env = workspace(2);
    let db = env.db_path();
    env.lcl(&["issue", "update", "1", "--ref", "BZR-1532"]).assert_ok();
    let conn = Connection::open(&db).expect("open db");
    let run = |sql: &str| conn.execute_batch(sql).map_err(|e| e.to_string());
    let insert = |n: i64, r: &str| run(&format!("INSERT INTO issues(id, number, title, created_at, updated_at, ref) VALUES ('hand-{n}', {n}, 't', 1, 1, {r})"));
    let unique = Err("UNIQUE constraint failed: issues.ref".to_string());
    assert_eq!(insert(90, "'bzr-1532'"), unique);
    assert_eq!(run("UPDATE issues SET ref = 'Bzr-1532' WHERE number = 2"), unique);
    // Any number of issues have no ref.
    assert_eq!(insert(91, "NULL"), Ok(()));
    assert_eq!(insert(92, "NULL"), Ok(()));
    assert_eq!(insert(93, "'BZR-1533'"), Ok(()));
}

#[test]
fn search_lists_the_ref_holder_first() {
    let env = workspace(1);
    env.lcl(&["issue", "create", "--title", "holder", "--ref", "BZR-1532"]).assert_ok(); // 2
    env.lcl(&["issue", "create", "--title", "mentions", "--description", "see BZR-1532 for this"]).assert_ok(); // 3
    let search = |q: &str| env.lcl(&["search", q, "--json"]).assert_ok().json();

    for q in ["BZR-1532", "bzr-1532", " Bzr-1532 "] {
        let found = search(q);
        assert_eq!(hits(&found), [(2, json!(["ref"])), (3, json!(["description"]))], "{q:?}");
        assert_eq!((&found["issue_total"], &found["issues"][0]["ref"]), (&json!(2), &json!("BZR-1532")), "{q:?}");
    }
    let human = env.lcl(&["search", "BZR-1532"]).assert_ok().stdout.clone();
    assert_eq!(human, "DRM-2\t[Backlog]\tholder\tref\nDRM-3\t[Backlog]\tmentions\tdescription\n");

    // A text hit on the holder merges into it: listed once.
    env.lcl(&["issue", "update", "2", "--description", "BZR-1532 is this one"]).assert_ok();
    let found = search("bzr-1532");
    assert_eq!(hits(&found), [(2, json!(["ref", "description"])), (3, json!(["description"]))]);
    assert_eq!(found["issue_total"], 2);

    // Anything that isn't exactly a ref is searched as before.
    assert_eq!(hits(&search("BZR-1533")), []);
    let two_words = env.lcl(&["search", "BZR-1532", "this", "--json"]).assert_ok().json();
    let mut numbers = hits(&two_words);
    numbers.sort_by_key(|(n, _)| *n);
    assert_eq!(numbers, [(2, json!(["description"])), (3, json!(["description"]))]);
    assert_eq!(hits(&search("holder")), [(2, json!(["title"]))]);
    // The identifier rule still comes first.
    assert_eq!(hits(&search("DRM-3")), [(3, json!(["identifier"]))]);
}

/// Another workspace's identifier never resolves through a ref, but the
/// error names the issue that has it as its ref.
#[test]
fn a_wrong_key_names_the_issue_with_that_ref() {
    let env = workspace(3);
    env.lcl(&["issue", "update", "3", "--ref", "BZR-1532"]).assert_ok();
    let before = files(&env);

    let hint = |given: &str| format!("Error: {given} is not in this workspace (key DRM); DRM-3 has ref BZR-1532\n");
    for (args, given) in [
        (vec!["issue", "show", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "show", "bzr-1532", "--json"], "bzr-1532"),
        (vec!["issue", "update", "BZR-1532", "--title", "x"], "BZR-1532"),
        (vec!["issue", "delete", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "comment", "BZR-1532", "hi"], "BZR-1532"),
        (vec!["issue", "comments", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "relate", "1", "blocks", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "relate", "BZR-1532", "related", "1"], "BZR-1532"),
        (vec!["issue", "unrelate", "1", "blocks", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "create", "--title", "x", "--parent", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "update", "1", "--parent", "BZR-1532"], "BZR-1532"),
        (vec!["issue", "list", "--parent", "BZR-1532"], "BZR-1532"),
    ] {
        assert_eq!(trio(&env.lcl(&args)), (2, "", hint(given).as_str()), "{args:?}");
        assert!(files(&env) == before, "{args:?} changed a file");
    }
    // No ref matches: today's message alone.
    assert_eq!(trio(&env.lcl(&["issue", "show", "BZR-1"])), (2, "", "Error: BZR-1 is not in this workspace (key DRM)\n"));
    // Numbers and this workspace's KEY-N never consult refs.
    env.lcl(&["issue", "update", "1", "--ref", "BZR-3"]).assert_ok();
    assert_eq!(issue(&env, 3)["identifier"], "DRM-3");
    let three = env.lcl(&["issue", "show", "DRM-3", "--json"]).assert_ok().json();
    assert_eq!((&three["issue"]["number"], &three["issue"]["ref"]), (&json!(3), &json!("BZR-1532")));
}

#[test]
fn creators_and_comment_authors_come_from_the_flag_then_lcl_author_then_user() {
    let env = workspace(0);
    // Runs lcl with USER and LCL_AUTHOR as given (None: unset).
    let as_ = |user: Option<&str>, lcl_author: Option<&str>, args: &[&str]| {
        env.lcl_with(args, |cmd: &mut Command| {
            for (name, value) in [("USER", user), ("LCL_AUTHOR", lcl_author)] {
                match value {
                    Some(v) => cmd.env(name, v),
                    None => cmd.env_remove(name),
                };
            }
        })
    };
    let create = |user, lcl_author, extra: &[&str]| {
        let mut args = vec!["issue", "create", "--title", "t", "--json"];
        args.extend_from_slice(extra);
        let out = as_(user, lcl_author, &args);
        out.assert_ok().json()["creator"].clone()
    };
    assert_eq!(create(Some("carol"), Some("bob"), &["--author", " alice "]), "alice");
    assert_eq!(create(Some("carol"), Some(" bob "), &[]), "bob");
    assert_eq!(create(Some("carol"), None, &[]), "carol");
    assert_eq!(create(Some("carol"), Some(""), &[]), "carol", "an empty LCL_AUTHOR counts as unset");
    assert_eq!(create(Some("carol"), Some("   "), &[]), "carol");
    assert_eq!(create(Some(""), None, &[]), Value::Null, "an empty USER counts as unset");
    assert_eq!(create(None, None, &[]), Value::Null);
    let n = 7;
    assert_eq!(env.lcl(&["issue", "list", "--json"]).assert_ok().json()["total"], n);
    for blank in ["", "  "] {
        let out = as_(Some("carol"), None, &["issue", "create", "--title", "t", "--author", blank]);
        assert!(out.code == 2 && out.stderr.contains("an author can't be empty"), "{out:#?}");
        let out = as_(Some("carol"), None, &["issue", "comment", "1", "hi", "--author", blank]);
        assert!(out.code == 2 && out.stderr.contains("an author can't be empty"), "{out:#?}");
    }
    assert_eq!(env.lcl(&["issue", "list", "--json"]).assert_ok().json()["total"], n);

    // Human show prints the creator when there is one.
    assert!(env.lcl(&["issue", "show", "1"]).assert_ok().stdout.contains("\nCreator: alice\n"));
    assert!(!env.lcl(&["issue", "show", "7"]).assert_ok().stdout.contains("Creator:"));

    // issue update never changes the creator, set or not.
    for (number, creator) in [(1, json!("alice")), (7, Value::Null)] {
        let out = as_(Some("dave"), Some("erin"), &["issue", "update", &number.to_string(), "--title", "renamed", "--json"]);
        assert_eq!(out.assert_ok().json()["creator"], creator, "issue {number}");
    }

    // Comments: the same chain.
    let comment = |user, lcl_author, extra: &[&str]| {
        let mut args = vec!["issue", "comment", "1", "hi", "--json"];
        args.extend_from_slice(extra);
        as_(user, lcl_author, &args).assert_ok().json()["author"].clone()
    };
    assert_eq!(comment(Some("carol"), Some("bob"), &["--author", "alice"]), "alice");
    assert_eq!(comment(Some("carol"), Some("bob"), &[]), "bob");
    assert_eq!(comment(Some("carol"), Some(""), &[]), "carol");
    assert_eq!(comment(None, None, &[]), Value::Null);
    let authors: Vec<Value> = env.lcl(&["issue", "comments", "1", "--json"]).assert_ok().json().as_array().expect("comments").iter().map(|c| c["author"].clone()).collect();
    assert_eq!(authors, [json!("alice"), json!("bob"), json!("carol"), Value::Null]);
}

#[test]
fn comments_read_oldest_first_then_in_the_order_added() {
    let env = workspace(2);
    let db = env.db_path();
    let id = |n: i64| issue(&env, n)["id"].as_str().expect("id").to_string();
    env.lcl(&["issue", "comment", "1", "second"]).assert_ok();
    // Added later (a higher rowid), but written earlier.
    exec(&db, &format!("INSERT INTO comments(id, issue_id, body, created_at) VALUES ('a-early', '{}', 'first', 1000)", id(1)));
    // Three in one second, given ids that sort the other way.
    let two = id(2);
    exec(&db, &format!(
        "INSERT INTO comments(id, issue_id, body, created_at) VALUES ('z', '{two}', 'c1', 5000);
         INSERT INTO comments(id, issue_id, body, created_at) VALUES ('y', '{two}', 'c2', 5000);
         INSERT INTO comments(id, issue_id, body, created_at) VALUES ('x', '{two}', 'c3', 5000);"
    ));
    let bodies = |v: &Value| -> Vec<String> { v.as_array().expect("comments").iter().map(|c| c["body"].as_str().expect("body").to_string()).collect() };
    for (n, want) in [(1, vec!["first", "second"]), (2, vec!["c1", "c2", "c3"])] {
        assert_eq!(bodies(&env.lcl(&["issue", "comments", &n.to_string(), "--json"]).assert_ok().json()), want, "issue comments {n}");
        assert_eq!(bodies(&env.lcl(&["issue", "show", &n.to_string(), "--json"]).assert_ok().json()["comments"]), want, "issue show {n}");
    }
    let human = env.lcl(&["issue", "show", "2"]).assert_ok().stdout.clone();
    let at: Vec<usize> = ["c1", "c2", "c3"].iter().map(|b| human.find(&format!(": {b}\n")).unwrap_or_else(|| panic!("{b}: {human}"))).collect();
    assert!(at[0] < at[1] && at[1] < at[2], "{human}");
}

#[test]
fn parallel_updates_to_one_ref_give_it_to_one_issue() {
    let env = workspace(10);
    let db = env.db_path();
    let cmds: Vec<Vec<String>> = (1..=10).map(|n| argv(&["issue", "update", &n.to_string(), "--ref", "SAME"])).collect();
    let outs = env.lcl_parallel(&cmds);
    let won: Vec<usize> = (0..10).filter(|i| outs[*i].code == 0).collect();
    assert_eq!(won.len(), 1, "{outs:#?}");
    let holder = won[0] + 1;
    let taken = format!("Error: ref \"SAME\" is already on DRM-{holder}\n");
    for (i, out) in outs.iter().enumerate().filter(|(i, _)| *i != won[0]) {
        assert_eq!(trio(out), (1, "", taken.as_str()), "process {i}");
    }
    assert_eq!(query::<i64>(&db, "SELECT count(*) FROM issues WHERE ref IS NOT NULL"), 1);
    assert_eq!(query::<i64>(&db, "SELECT number FROM issues WHERE ref = 'SAME'"), holder as i64);
}

#[test]
fn every_new_flag_has_help() {
    let env = Env::new();
    for (cmd, flags) in [
        ("create", vec!["--ref <REF>", "--author <NAME>"]),
        ("update", vec!["--ref <REF>", "--no-ref"]),
        ("comment", vec!["--author <NAME>"]),
    ] {
        let help = env.lcl(&["issue", cmd, "-h"]).assert_ok().stdout.clone();
        for flag in flags {
            let line = help.lines().find(|l| l.trim_start().starts_with(flag)).unwrap_or_else(|| panic!("issue {cmd}: no {flag}\n{help}"));
            assert!(line.trim_start()[flag.len()..].trim().len() > 3, "issue {cmd}: no help for {flag}: {line:?}");
        }
    }
}
