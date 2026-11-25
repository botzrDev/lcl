//! The trash for docs, comments and labels: the rules of `lcl_core::trash`
//! under "Docs, comments and labels", held by the commands.
//!
//! For each: one field (delete then restore leaves every table as it was),
//! frozen, out of sight, not exported, purged only from the trash and only
//! when asked. And what is particular: a live doc's parent is live; a
//! comment is frozen with its issue; a label keeps its name and its links.

mod common;

use common::{Env, FTS_TABLES, dump, error_text, fts_integrity};
use serde_json::{Value, json};

fn j(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    env.lcl(&argv).assert_ok().json()
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().expect("an id").to_string()
}

/// Exit 1, nothing on stdout, and this error under `--json`.
#[track_caller]
fn refused(env: &Env, args: &[&str], kind: &str, message: &str) {
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = env.lcl(&argv);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", error_text(&argv, kind, message).as_str()),
        "{args:?}"
    );
}

fn labels_of(env: &Env, n: &str) -> Value {
    j(env, &["issue", "show", n])["issue"]["labels"].clone()
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .expect("a list")
        .iter()
        .map(|v| v[key].as_str().unwrap().to_string())
        .collect()
}

fn export_records(env: &Env, kind: &str) -> Vec<Value> {
    env.lcl(&["export"])
        .assert_ok()
        .stdout
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["type"] == kind)
        .collect()
}

/// W-1 has bug and ui, W-2 has bug, W-3 has none.
fn labelled() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    for label in ["bug", "ui", "later"] {
        env.lcl(&["label", "create", label]).assert_ok();
    }
    env.lcl(&[
        "issue", "create", "--title", "one", "--label", "bug", "--label", "ui",
    ])
    .assert_ok();
    env.lcl(&["issue", "create", "--title", "two", "--label", "bug"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "three"])
        .assert_ok();
    env
}

const BUG_TRASHED: &str = "label \"bug\" is in the trash; restore it with: lcl label restore bug";

#[test]
fn a_label_s_delete_and_restore_change_one_field() {
    let env = labelled();
    let db = env.db_path();
    let before = dump(&db);
    let shown: Vec<Value> = ["1", "2", "3"]
        .iter()
        .map(|n| j(&env, &["issue", "show", n]))
        .collect();

    assert_eq!(
        j(&env, &["label", "delete", "BUG"]),
        json!({"deleted": "bug", "id": id_of(&j(&env, &["label", "list", "--trashed"])[0]), "removed_from_issues": 2})
    );
    // Out of sight: no list and no issue shows it; the trash does.
    assert_eq!(names(&j(&env, &["label", "list"]), "name"), ["later", "ui"]);
    assert_eq!(
        names(&j(&env, &["label", "list", "--trashed"]), "name"),
        ["bug"]
    );
    assert!(j(&env, &["label", "list", "--trashed"])[0]["deleted_at"].is_i64());
    assert_eq!(
        [labels_of(&env, "1"), labels_of(&env, "2")],
        [json!(["ui"]), json!([])]
    );
    assert_eq!(
        j(&env, &["issue", "list"])["issues"][1]["labels"],
        json!([])
    );
    // Its links are still stored: only its own row changed.
    let mut trashed = dump(&db);
    for row in trashed.tables.get_mut("labels").unwrap() {
        if row["name"] == rusqlite::types::Value::Text("bug".into()) {
            row.insert("deleted_at".into(), rusqlite::types::Value::Null);
        }
    }
    let logged = common::same_records(
        &before,
        &trashed,
        "deleting a label changed more than its deleted_at",
    );
    assert_eq!(common::logged_fields(&logged), ["deleted_at"]);

    assert_eq!(
        j(&env, &["label", "restore", "bug"])["restored_to_issues"],
        2
    );
    let logged = common::same_records(&before, &dump(&db), "the label came back changed");
    assert_eq!(common::logged_fields(&logged), ["deleted_at", "deleted_at"]);
    for (n, was) in ["1", "2", "3"].iter().zip(&shown) {
        assert_eq!(j(&env, &["issue", "show", n]), *was, "W-{n}");
    }
}

/// The name stays taken and the label can't be used, each refusal naming
/// the restore command; and none of them changes anything.
#[test]
fn a_trashed_label_keeps_its_name_and_is_frozen() {
    let env = labelled();
    env.lcl(&["label", "delete", "bug"]).assert_ok();
    let before = dump(&env.db_path());
    for args in [
        &["label", "create", "bug"][..],
        &["label", "create", "BUG", "--color", "#fff"],
        &["label", "delete", "bug"],
        &["issue", "create", "--title", "t", "--label", "bug"],
        &["issue", "update", "3", "--add-label", "bug"],
        &["issue", "update", "3", "--label", "Bug"],
        &["issue", "update", "1", "--remove-label", "bug"],
        &["issue", "list", "--label", "bug"],
    ] {
        refused(&env, args, "conflict", BUG_TRASHED);
    }
    refused(
        &env,
        &["label", "restore", "ui"],
        "conflict",
        "label \"ui\" is not in the trash",
    );
    refused(
        &env,
        &["label", "restore", "nope"],
        "not_found",
        "label \"nope\" not found",
    );
    refused(
        &env,
        &["label", "purge", "ui"],
        "conflict",
        "label \"ui\" is not in the trash; only what is in the trash can be purged (lcl label delete moves it there)",
    );
    assert!(
        dump(&env.db_path()) == before,
        "a refused command changed the workspace"
    );
}

/// Restore brings the label back on every issue that had it, whatever was
/// done to those issues' labels while it was in the trash.
#[test]
fn a_restored_label_is_back_on_every_issue_that_had_it() {
    let env = labelled();
    env.lcl(&["label", "delete", "bug"]).assert_ok();
    // W-1: labels replaced, then cleared, then one added. W-2: updated,
    // trashed and restored. W-3: never had it.
    env.lcl(&["issue", "update", "1", "--label", "later"])
        .assert_ok();
    env.lcl(&["issue", "update", "1", "--clear-labels"])
        .assert_ok();
    env.lcl(&["issue", "update", "1", "--add-label", "ui"])
        .assert_ok();
    env.lcl(&["issue", "update", "2", "--title", "two again"])
        .assert_ok();
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    env.lcl(&["issue", "restore", "2"]).assert_ok();
    env.lcl(&["issue", "update", "3", "--add-label", "later"])
        .assert_ok();
    assert_eq!(
        [
            labels_of(&env, "1"),
            labels_of(&env, "2"),
            labels_of(&env, "3")
        ],
        [json!(["ui"]), json!([]), json!(["later"])]
    );

    env.lcl(&["label", "restore", "bug"]).assert_ok();
    assert_eq!(
        [
            labels_of(&env, "1"),
            labels_of(&env, "2"),
            labels_of(&env, "3")
        ],
        [json!(["bug", "ui"]), json!(["bug"]), json!(["later"])]
    );
    assert_eq!(
        names(
            &j(&env, &["issue", "list", "--label", "bug"])["issues"],
            "identifier"
        ),
        ["W-2", "W-1"]
    );
}

#[test]
fn a_trashed_label_is_not_exported_and_is_purged_only_when_asked() {
    let env = labelled();
    let db = env.db_path();
    env.lcl(&["label", "delete", "bug"]).assert_ok();
    // In the trash since 1970, and still there after other commands.
    common::exec(&db, "UPDATE labels SET deleted_at = 0 WHERE name = 'bug'");
    for args in [
        &["label", "list"][..],
        &["issue", "list"],
        &["label", "create", "new"],
        &["workspace", "show"],
    ] {
        env.lcl(args).assert_ok();
    }
    assert_eq!(
        names(&j(&env, &["label", "list", "--trashed"]), "name"),
        ["bug"]
    );

    assert_eq!(
        names(&Value::Array(export_records(&env, "label")), "name"),
        ["later", "new", "ui"]
    );
    let issues = export_records(&env, "issue");
    assert_eq!(
        [&issues[0]["labels"], &issues[1]["labels"]],
        [&json!(["ui"]), &json!([])]
    );
    let export = env.lcl(&["export"]).assert_ok().stdout.clone();
    assert!(!export.contains("deleted_at"));
    let copy = Env::new();
    copy.lcl(&["init", "copy", "--key", "W"]).assert_ok();
    copy.lcl_stdin(&["import", "-"], export.as_bytes())
        .assert_ok();
    assert_eq!(
        copy.lcl(&["export"])
            .assert_ok()
            .stdout
            .replace("\"name\":\"copy\"", "\"name\":\"w\""),
        export
    );
    // There the name is free: nothing of the trash came along.
    copy.lcl(&["label", "create", "bug"]).assert_ok();
    // A workspace holding nothing but a trashed label is not empty for an
    // import, and the refusal says where the label is.
    let only_trash = Env::new();
    only_trash.lcl(&["init", "t", "--key", "W"]).assert_ok();
    only_trash.lcl(&["label", "create", "gone"]).assert_ok();
    only_trash.lcl(&["label", "delete", "gone"]).assert_ok();
    let out = only_trash.lcl_stdin(&["import", "-"], export.as_bytes());
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(
        out.stderr.contains("it has 1 label (1 in the trash)"),
        "{out:#?}"
    );

    // Purged, it goes with its two links, and its name is free here too.
    assert_eq!(
        j(&env, &["label", "purge", "--all"]),
        json!({"purged": ["bug"], "labels": 1, "removed_from_issues": 2})
    );
    assert_eq!(j(&env, &["label", "purge", "--all"])["labels"], 0);
    assert!(common::foreign_key_violations(&db).is_empty());
    env.lcl(&["label", "create", "bug"]).assert_ok();
    assert_eq!(
        [labels_of(&env, "1"), labels_of(&env, "2")],
        [json!(["ui"]), json!([])]
    );
}

/// W-1 with three comments, the second by ann; W-2 with one.
fn commented() -> (Env, Vec<String>) {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.create("one");
    env.create("two");
    let mut ids = vec![];
    for (n, body) in [
        ("1", "apple pie"),
        ("1", "banana bread"),
        ("1", "cherry tart"),
        ("2", "damson jam"),
    ] {
        ids.push(id_of(&j(
            &env,
            &["issue", "comment", n, body, "--author", "ann"],
        )));
    }
    (env, ids)
}

#[test]
fn a_comment_s_delete_and_restore_change_one_field() {
    let (env, ids) = commented();
    let db = env.db_path();
    let before = dump(&db);
    let shown = env
        .lcl(&["issue", "show", "1", "--json"])
        .assert_ok()
        .stdout
        .clone();
    let banana = &ids[1];

    assert_eq!(
        j(&env, &["comment", "delete", banana]),
        json!({"deleted": banana, "id": banana, "issue": "W-1"})
    );
    // Out of sight: issue show, issue comments, search.
    assert_eq!(
        names(&j(&env, &["issue", "show", "1"])["comments"], "body"),
        ["apple pie", "cherry tart"]
    );
    assert_eq!(
        names(&j(&env, &["issue", "comments", "1"]), "body"),
        ["apple pie", "cherry tart"]
    );
    assert_eq!(j(&env, &["search", "banana"])["issue_total"], 0);
    assert_eq!(j(&env, &["search", "cherry"])["issue_total"], 1);
    assert_eq!(
        env.lcl(&["issue", "comments", "1"])
            .assert_ok()
            .stdout
            .matches(" · ")
            .count(),
        4,
        "two comments, two separators each"
    );
    // The trash of that issue, and of no other.
    let trash = j(&env, &["issue", "comments", "1", "--trashed"]);
    assert_eq!(names(&trash, "id"), [banana.as_str()]);
    assert!(trash[0]["deleted_at"].is_i64());
    assert_eq!(j(&env, &["issue", "comments", "2", "--trashed"]), json!([]));
    // Only its own field changed, the search index included.
    let mut trashed = dump(&db);
    for row in trashed.tables.get_mut("comments").unwrap() {
        if row["id"] == rusqlite::types::Value::Text(banana.clone()) {
            row.insert("deleted_at".into(), rusqlite::types::Value::Null);
        }
    }
    let logged = common::same_records(
        &before,
        &trashed,
        "deleting a comment changed more than its deleted_at",
    );
    assert_eq!(common::logged_fields(&logged), ["deleted_at"]);

    // Frozen.
    let in_trash =
        format!("comment {banana} is in the trash; restore it with: lcl comment restore {banana}");
    refused(
        &env,
        &["comment", "edit", banana, "rewritten"],
        "conflict",
        &in_trash,
    );
    refused(&env, &["comment", "delete", banana], "conflict", &in_trash);
    refused(
        &env,
        &["comment", "restore", &ids[0]],
        "conflict",
        &format!("comment {} is not in the trash", ids[0]),
    );

    assert_eq!(
        j(&env, &["comment", "restore", &banana.to_uppercase()])["body"],
        "banana bread"
    );
    let logged = common::same_records(&before, &dump(&db), "the comment came back changed");
    assert_eq!(common::logged_fields(&logged), ["deleted_at", "deleted_at"]);
    assert_eq!(
        env.lcl(&["issue", "show", "1", "--json"])
            .assert_ok()
            .stdout,
        shown
    );
    fts_integrity(&db, &FTS_TABLES);
}

/// A comment is frozen with its issue, and goes with it when it is purged.
#[test]
fn a_comment_follows_its_issue() {
    let (env, ids) = commented();
    env.lcl(&["comment", "delete", &ids[1]]).assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    let before = dump(&env.db_path());
    let issue_trashed = "W-1 is in the trash; restore it with: lcl issue restore W-1";
    for args in [
        &["comment", "restore", &ids[1]][..],
        &["comment", "delete", &ids[0]],
        &["comment", "edit", &ids[0], "x"],
        &["issue", "comments", "1", "--trashed"],
    ] {
        refused(&env, args, "conflict", issue_trashed);
    }
    assert!(dump(&env.db_path()) == before);
    // Not exported: neither the trashed issue's comments nor a trashed
    // comment of a live issue.
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    assert_eq!(
        names(&Value::Array(export_records(&env, "comment")), "body"),
        ["apple pie", "cherry tart", "damson jam"]
    );

    // Purging the issue takes all its comments, the trashed one too.
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    assert_eq!(j(&env, &["issue", "purge", "1"])["comments"], 3);
    refused(
        &env,
        &["comment", "restore", &ids[1]],
        "not_found",
        &format!("comment {} not found", ids[1]),
    );
    assert!(common::foreign_key_violations(&env.db_path()).is_empty());
    fts_integrity(&env.db_path(), &FTS_TABLES);
}

#[test]
fn a_comment_is_purged_only_from_the_trash_and_only_when_asked() {
    let (env, ids) = commented();
    let db = env.db_path();
    refused(
        &env,
        &["comment", "purge", &ids[0]],
        "conflict",
        &format!(
            "comment {} is not in the trash; only what is in the trash can be purged (lcl comment delete moves it there)",
            ids[0]
        ),
    );
    assert_eq!(
        j(&env, &["comment", "purge", "--all"]),
        json!({"purged": [], "comments": 0})
    );
    env.lcl(&["comment", "delete", &ids[0]]).assert_ok();
    env.lcl(&["comment", "delete", &ids[3]]).assert_ok();
    common::exec(
        &db,
        "UPDATE comments SET deleted_at = 0 WHERE deleted_at IS NOT NULL",
    );
    for args in [
        &["issue", "show", "1"][..],
        &["issue", "comment", "1", "more"],
        &["search", "apple"],
        &["export"],
    ] {
        env.lcl(args).assert_ok();
    }
    assert_eq!(
        names(&j(&env, &["issue", "comments", "1", "--trashed"]), "id"),
        [ids[0].as_str()]
    );

    assert_eq!(
        j(&env, &["comment", "purge", &ids[0]]),
        json!({"purged": [ids[0]], "comments": 1})
    );
    assert_eq!(
        j(&env, &["comment", "purge", "--all"]),
        json!({"purged": [ids[3]], "comments": 1})
    );
    assert_eq!(
        names(&j(&env, &["issue", "comments", "1"]), "body"),
        ["banana bread", "cherry tart", "more"]
    );
    assert_eq!(j(&env, &["search", "apple"])["issue_total"], 0);
    fts_integrity(&db, &FTS_TABLES);
    for args in [
        &["comment", "purge"][..],
        &["comment", "purge", &ids[1], "--all"],
    ] {
        assert_eq!(env.lcl(args).code, 2, "{args:?}");
    }
}

/// Handbook, with Chapter under it and Section under Chapter; and Notes.
fn documented() -> (Env, [String; 4]) {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    let handbook = id_of(&j(
        &env,
        &[
            "doc",
            "create",
            "--title",
            "Handbook",
            "--content",
            "walrus",
        ],
    ));
    let chapter = id_of(&j(
        &env,
        &["doc", "create", "--title", "Chapter", "--parent", &handbook],
    ));
    let section = id_of(&j(
        &env,
        &[
            "doc",
            "create",
            "--title",
            "Section",
            "--content",
            "narwhal",
            "--parent",
            &chapter,
        ],
    ));
    let notes = id_of(&j(
        &env,
        &[
            "doc",
            "create",
            "--title",
            "Notes",
            "--content",
            "walrus too",
        ],
    ));
    (env, [handbook, chapter, section, notes])
}

fn doc_trashed(id: &str) -> String {
    format!("doc {id} is in the trash; restore it with: lcl doc restore {id}")
}

#[test]
fn a_doc_s_delete_and_restore_change_one_field() {
    let (env, [_, _, section, notes]) = documented();
    let db = env.db_path();
    let before = dump(&db);
    let listed = env
        .lcl(&["doc", "list", "--json"])
        .assert_ok()
        .stdout
        .clone();

    assert_eq!(
        j(&env, &["doc", "delete", &section]),
        json!({"deleted": section, "id": section})
    );
    assert_eq!(names(&j(&env, &["doc", "list"]), "title").len(), 3);
    assert!(!names(&j(&env, &["doc", "list"]), "title").contains(&"Section".to_string()));
    assert_eq!(j(&env, &["search", "narwhal"])["doc_total"], 0);
    assert_eq!(j(&env, &["search", "walrus"])["doc_total"], 2);
    let trash = j(&env, &["doc", "list", "--trashed"]);
    assert_eq!(names(&trash, "title"), ["Section"]);
    assert!(trash[0]["deleted_at"].is_i64());
    assert_eq!(
        env.lcl(&["doc", "list", "--trashed"]).assert_ok().stdout,
        format!("{section}\tSection\n")
    );
    let mut trashed = dump(&db);
    for row in trashed.tables.get_mut("docs").unwrap() {
        if row["id"] == rusqlite::types::Value::Text(section.clone()) {
            row.insert("deleted_at".into(), rusqlite::types::Value::Null);
        }
    }
    let logged = common::same_records(
        &before,
        &trashed,
        "deleting a doc changed more than its deleted_at",
    );
    assert_eq!(common::logged_fields(&logged), ["deleted_at"]);

    // Frozen: no command reads or changes it, or puts a doc under it.
    for args in [
        &["doc", "show", &section][..],
        &["doc", "update", &section, "--title", "x"],
        &["doc", "delete", &section],
        &["doc", "create", "--title", "under", "--parent", &section],
        &["doc", "update", &notes, "--parent", &section],
    ] {
        refused(&env, args, "conflict", &doc_trashed(&section));
    }
    refused(
        &env,
        &["doc", "restore", &notes],
        "conflict",
        &format!("doc {notes} is not in the trash"),
    );
    refused(
        &env,
        &["doc", "restore", "nope"],
        "not_found",
        "doc nope not found",
    );
    let mut still = dump(&db);
    for row in still.tables.get_mut("docs").unwrap() {
        if row["id"] == rusqlite::types::Value::Text(section.clone()) {
            row.insert("deleted_at".into(), rusqlite::types::Value::Null);
        }
    }
    common::same_records(&before, &still, "a refused command changed the workspace");

    assert_eq!(j(&env, &["doc", "restore", &section])["title"], "Section");
    let logged = common::same_records(&before, &dump(&db), "the doc came back changed");
    assert_eq!(common::logged_fields(&logged), ["deleted_at", "deleted_at"]);
    assert_eq!(
        env.lcl(&["doc", "list", "--json"]).assert_ok().stdout,
        listed
    );
    fts_integrity(&db, &FTS_TABLES);
}

#[test]
fn a_live_doc_s_parent_is_live() {
    let (env, [handbook, chapter, section, _]) = documented();
    let db = env.db_path();
    let before = dump(&db);
    // Delete: refused while a doc under it is live.
    refused(
        &env,
        &["doc", "delete", &chapter],
        "conflict",
        &j_error(&env, &["doc", "delete", &chapter]),
    );
    assert!(j_error(&env, &["doc", "delete", &chapter]).contains(&section));
    assert!(dump(&db) == before);
    // Bottom up it goes.
    for id in [&section, &chapter, &handbook] {
        env.lcl(&["doc", "delete", id]).assert_ok();
    }
    // Restore: refused under a trashed parent, naming it.
    refused(
        &env,
        &["doc", "restore", &section],
        "conflict",
        &format!(
            "doc {section} can't be restored while its parent {chapter} is in the trash; restore {chapter} first"
        ),
    );
    for id in [&handbook, &chapter, &section] {
        env.lcl(&["doc", "restore", id]).assert_ok();
    }
    let logged = common::same_records(&before, &dump(&db), "a doc came back changed");
    assert_eq!(common::logged_fields(&logged), ["deleted_at"; 6]);
    let orphans: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM docs c JOIN docs p ON p.id = c.parent_id WHERE c.deleted_at IS NULL AND p.deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
}

/// The message of the error `args` fails with under --json.
fn j_error(env: &Env, args: &[&str]) -> String {
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = env.lcl(&argv);
    assert_eq!(out.code, 1, "{out:#?}");
    let v: Value = serde_json::from_str(out.stderr.lines().last().unwrap()).unwrap();
    v["error"]["message"].as_str().unwrap().to_string()
}

#[test]
fn a_doc_is_not_exported_and_is_purged_only_from_the_trash() {
    let (env, [handbook, chapter, section, notes]) = documented();
    let db = env.db_path();
    refused(
        &env,
        &["doc", "purge", &notes],
        "conflict",
        &format!(
            "doc {notes} is not in the trash; only what is in the trash can be purged (lcl doc delete moves it there)"
        ),
    );
    env.lcl(&["doc", "delete", &section]).assert_ok();
    env.lcl(&["doc", "delete", &chapter]).assert_ok();
    common::exec(
        &db,
        "UPDATE docs SET deleted_at = 0 WHERE deleted_at IS NOT NULL",
    );
    for args in [
        &["doc", "list"][..],
        &["search", "walrus"],
        &["doc", "create", "--title", "More"],
        &["workspace", "show"],
    ] {
        env.lcl(args).assert_ok();
    }
    assert_eq!(
        j(&env, &["doc", "list", "--trashed"])
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Not exported; what is exported names no parent that was left out.
    let docs = export_records(&env, "doc");
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().all(|d| d["parent"].is_null()), "{docs:?}");
    let export = env.lcl(&["export"]).assert_ok().stdout.clone();
    let copy = Env::new();
    copy.lcl(&["init", "copy", "--key", "W"]).assert_ok();
    copy.lcl_stdin(&["import", "-"], export.as_bytes())
        .assert_ok();
    assert_eq!(j(&copy, &["doc", "list", "--trashed"]), json!([]));

    // One doc is not purged from over the doc under it; --all takes both.
    refused(
        &env,
        &["doc", "purge", &chapter],
        "conflict",
        &format!(
            "doc {chapter} has docs under it in the trash: {section}; purge them first, or empty the trash with lcl doc purge --all"
        ),
    );
    let purged = j(&env, &["doc", "purge", "--all"]);
    assert_eq!(purged["docs"], 2);
    let mut ids = vec![chapter.clone(), section.clone()];
    ids.sort();
    assert_eq!(purged["purged"], json!(ids));
    assert_eq!(
        j(&env, &["doc", "purge", "--all"]),
        json!({"purged": [], "docs": 0})
    );
    refused(
        &env,
        &["doc", "restore", &section],
        "not_found",
        &format!("doc {section} not found"),
    );
    assert_eq!(j(&env, &["search", "narwhal"])["doc_total"], 0);
    assert_eq!(j(&env, &["doc", "show", &handbook])["title"], "Handbook");
    assert!(common::foreign_key_violations(&db).is_empty());
    fts_integrity(&db, &FTS_TABLES);
}
