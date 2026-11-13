//! Comments (step 13b): how people read them, and `comment edit` and
//! `comment delete`, which name a comment by its id.
//!
//! `issue comments` shows each comment as a header, `AUTHOR · DATE · ID`,
//! then its body with every line indented two spaces, and a blank line
//! between comments; `issue show` shows them the same way without the id.
//! DATE is local time (TZ) as YYYY-MM-DD HH:MM, and a time chrono can't
//! represent shows as the number stored. JSON is as it was. Editing or
//! deleting a comment leaves its issue's updated_at alone, as adding one
//! does, and search follows both.

mod common;

use common::{Env, Out, dump, exec, fts_integrity};
use serde_json::{Value, json};

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env
}

fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

fn json_error(kind: &str, message: &str) -> String {
    format!("{}\n", json!({"error": {"kind": kind, "message": message}}))
}

/// 2027-01-15 08:00:00 UTC.
const T1: i64 = 1_800_000_000;
/// 2027-01-15 09:02:03 UTC.
const T2: i64 = 1_800_003_723;

/// W-1 (created by ann) with ann's three-line comment at T1, then one with
/// no author at T2. Returns their ids.
fn two_comments(env: &Env) -> (String, String) {
    env.lcl(&["issue", "create", "--title", "one", "--author", "ann"])
        .assert_ok();
    let add = |args: &[&str]| {
        env.lcl(args).assert_ok().json()["id"]
            .as_str()
            .expect("id")
            .to_string()
    };
    let a = add(&[
        "--json",
        "issue",
        "comment",
        "1",
        "first line\nsecond line\nthird line",
        "--author",
        "ann",
    ]);
    let b = add(&["--json", "issue", "comment", "1", "no author"]);
    exec(
        &env.db_path(),
        &format!(
            "UPDATE comments SET created_at = {T1} WHERE id = '{a}'; UPDATE comments SET created_at = {T2}, author = NULL WHERE id = '{b}';"
        ),
    );
    (a, b)
}

fn issue_id(env: &Env) -> String {
    env.lcl(&["--json", "issue", "show", "1"])
        .assert_ok()
        .json()["issue"]["id"]
        .as_str()
        .expect("id")
        .to_string()
}

fn updated_at(env: &Env) -> Value {
    env.lcl(&["--json", "issue", "show", "1"])
        .assert_ok()
        .json()["issue"]["updated_at"]
        .clone()
}

#[test]
fn comments_read_as_a_header_and_an_indented_body() {
    let env = workspace();
    let (a, b) = two_comments(&env);
    let listed = format!(
        "ann · 2027-01-15 08:00 · {a}\n  first line\n  second line\n  third line\n\nanon · 2027-01-15 09:02 · {b}\n  no author\n"
    );
    assert_eq!(
        trio(&env.lcl(&["issue", "comments", "1"])),
        (0, listed.as_str(), "")
    );
    let shown = "W-1 one\nStatus: Backlog  Priority: None  Assignee: Unassigned\nCreator: ann\n\nComments:\nann · 2027-01-15 08:00\n  first line\n  second line\n  third line\n\nanon · 2027-01-15 09:02\n  no author\n";
    assert_eq!(trio(&env.lcl(&["issue", "show", "1"])), (0, shown, ""));

    // Local time, whatever TZ says.
    let tokyo = env.lcl_env(&[("TZ", "JST-9")], &["issue", "comments", "1"]);
    assert!(
        tokyo
            .stdout
            .starts_with(&format!("ann · 2027-01-15 17:00 · {a}\n")),
        "{tokyo:#?}"
    );
    assert!(
        tokyo
            .stdout
            .contains(&format!("\nanon · 2027-01-15 18:02 · {b}\n")),
        "{tokyo:#?}"
    );

    // JSON is as it was: the stored number, and the body as stored.
    let iid = issue_id(&env);
    let want = json!([
        {"id": a, "issue_id": iid, "body": "first line\nsecond line\nthird line", "author": "ann", "created_at": T1},
        {"id": b, "issue_id": iid, "body": "no author", "author": null, "created_at": T2},
    ]);
    assert_eq!(
        env.lcl(&["--json", "issue", "comments", "1"])
            .assert_ok()
            .json(),
        want
    );
    assert_eq!(
        env.lcl(&["--json", "issue", "show", "1"])
            .assert_ok()
            .json()["comments"],
        want
    );

    // Every line is indented, an empty one too; a final newline adds no
    // line. A time chrono can't represent shows as stored.
    exec(
        &env.db_path(),
        &format!(
            "UPDATE comments SET body = 'x\n\ny\n', created_at = 9223372036854775807 WHERE id = '{b}'"
        ),
    );
    let out = env.lcl(&["issue", "comments", "1"]);
    assert!(
        out.stdout.ends_with(&format!(
            "\n\nanon · 9223372036854775807 · {b}\n  x\n  \n  y\n"
        )),
        "{out:#?}"
    );

    // No comments, no output.
    env.lcl(&["issue", "create", "--title", "two"]).assert_ok();
    assert_eq!(trio(&env.lcl(&["issue", "comments", "2"])), (0, "", ""));
}

/// The lines of clap's error before its usage, which names the subcommand.
fn before_usage(stderr: &str) -> &str {
    stderr.split("\nUsage:").next().unwrap_or(stderr)
}

#[test]
fn comment_edit_replaces_the_text_and_nothing_else() {
    let env = workspace();
    let (a, _) = two_comments(&env);
    let db = env.db_path();
    exec(&db, "UPDATE issues SET updated_at = 1000");
    let iid = issue_id(&env);

    // The id ignoring case; the comment as add prints it.
    let out = env.lcl(&["--json", "comment", "edit", &a.to_uppercase(), "new text"]);
    assert_eq!(
        out.assert_ok().json(),
        json!({"id": a, "issue_id": iid, "body": "new text", "author": "ann", "created_at": T1})
    );
    // Stored, not only printed: read back after the edit by the id in
    // another case.
    let stored = env
        .lcl(&["--json", "issue", "comments", "1"])
        .assert_ok()
        .json();
    assert_eq!(
        (&stored[0]["id"], &stored[0]["body"]),
        (&json!(a), &json!("new text"))
    );
    let out = env.lcl_stdin(
        &["comment", "edit", &a, "--body-file", "-"],
        b"from\nstdin\n",
    );
    assert_eq!(trio(&out), (0, "Comment edited\n", ""));
    let comments = env
        .lcl(&["--json", "issue", "comments", "1"])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &comments[0]["id"],
            &comments[0]["body"],
            &comments[0]["created_at"]
        ),
        (&json!(a), &json!("from\nstdin\n"), &json!(T1))
    );
    // Editing leaves the issue's updated_at alone, as adding does.
    assert_eq!(updated_at(&env), 1000);
    env.lcl(&["issue", "comment", "1", "third"]).assert_ok();
    assert_eq!(updated_at(&env), 1000);

    // The body is checked exactly as add checks it.
    std::fs::write(env.cwd.path().join("blank.txt"), " \n").expect("write");
    let before = dump(&db);
    for bad in [
        vec![" "],
        vec!["-"],
        vec!["--body-file", "blank.txt"],
        vec!["x", "--body-file", "blank.txt"],
        vec![],
    ] {
        let mut add = vec!["issue", "comment", "1"];
        add.extend(&bad);
        let mut edit = vec!["comment", "edit", a.as_str()];
        edit.extend(&bad);
        let (added, edited) = (env.lcl(&add), env.lcl(&edit));
        assert_eq!(
            (added.code, edited.code),
            (2, 2),
            "{bad:?}: {added:#?} {edited:#?}"
        );
        assert_eq!(
            before_usage(&edited.stderr),
            before_usage(&added.stderr),
            "{bad:?}"
        );
    }
    assert!(dump(&db) == before, "a refused edit wrote");

    assert_eq!(
        trio(&env.lcl(&["comment", "edit", "nope", "x"])),
        (1, "", "Error: comment nope not found\n")
    );
    assert_eq!(
        trio(&env.lcl(&["--json", "comment", "edit", "nope", "x"])),
        (
            1,
            "",
            json_error("not_found", "comment nope not found").as_str()
        )
    );
}

#[test]
fn comment_delete_removes_one_comment() {
    let env = workspace();
    let (a, b) = two_comments(&env);
    exec(&env.db_path(), "UPDATE issues SET updated_at = 1000");

    let out = env.lcl(&["--json", "comment", "delete", &b.to_uppercase()]);
    assert_eq!(
        out.assert_ok().json(),
        json!({"deleted": b, "issue": "W-1"})
    );
    let left = env
        .lcl(&["--json", "issue", "comments", "1"])
        .assert_ok()
        .json();
    assert_eq!(
        left.as_array()
            .map(|l| l.iter().map(|c| c["id"].clone()).collect::<Vec<_>>()),
        Some(vec![json!(a)])
    );
    assert_eq!(
        trio(&env.lcl(&["comment", "delete", &a])),
        (0, format!("Deleted comment {a}\n").as_str(), "")
    );
    assert_eq!(trio(&env.lcl(&["issue", "comments", "1"])), (0, "", ""));
    assert_eq!(updated_at(&env), 1000);

    assert_eq!(
        trio(&env.lcl(&["comment", "delete", &a])),
        (1, "", format!("Error: comment {a} not found\n").as_str())
    );
    assert_eq!(
        trio(&env.lcl(&["--json", "comment", "delete", &a])),
        (
            1,
            "",
            json_error("not_found", &format!("comment {a} not found")).as_str()
        )
    );
}

#[test]
fn search_follows_comment_edits_and_deletes() {
    let env = workspace();
    env.lcl(&["issue", "create", "--title", "plain"])
        .assert_ok();
    let db = env.db_path();
    let found = |q: &str| -> Vec<String> {
        let out = env.lcl(&["--json", "search", q]).assert_ok().json();
        out["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .map(|i| i["identifier"].as_str().expect("identifier").to_string())
            .collect()
    };
    let id = env
        .lcl(&["--json", "issue", "comment", "1", "alpha bravo"])
        .assert_ok()
        .json()["id"]
        .as_str()
        .expect("id")
        .to_string();
    assert_eq!(found("alpha"), ["W-1"]);

    env.lcl(&["comment", "edit", &id, "charlie delta"])
        .assert_ok();
    assert_eq!(
        (found("alpha"), found("charlie")),
        (vec![], vec!["W-1".to_string()])
    );
    fts_integrity(&db, &["comments_fts"]);

    let rowid = |id: &str| -> i64 {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.query_row("SELECT rowid FROM comments WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .expect("rowid")
    };
    let deleted = rowid(&id);
    env.lcl(&["comment", "delete", &id]).assert_ok();
    assert_eq!(found("charlie"), Vec::<String>::new());
    fts_integrity(&db, &["comments_fts"]);
    // The next comment takes the deleted one's rowid, so words of the
    // deleted comment still in the index would now match it.
    let next = env
        .lcl(&["--json", "issue", "comment", "1", "echo"])
        .assert_ok()
        .json()["id"]
        .as_str()
        .expect("id")
        .to_string();
    assert_eq!(rowid(&next), deleted);
    assert_eq!(
        (found("charlie"), found("delta"), found("echo")),
        (vec![], vec![], vec!["W-1".to_string()])
    );
    fts_integrity(&db, &["comments_fts"]);
}

/// The usage shows ID first, as the arguments parse. clap's own line put
/// the body first, and following it, `issue comment 123 1` comments "1" on
/// issue 123.
#[test]
fn comment_usage_puts_the_id_first() {
    let env = Env::new();
    for (sub, usage) in [
        (
            ["issue", "comment"],
            "Usage: lcl issue comment [OPTIONS] <ID> <BODY|--body-file <PATH>>",
        ),
        (
            ["comment", "edit"],
            "Usage: lcl comment edit [OPTIONS] <ID> <BODY|--body-file <PATH>>",
        ),
    ] {
        let missing = format!(
            "error: the following required arguments were not provided:\n  <BODY|--body-file <PATH>>\n  <ID>\n\n{usage}\n\nFor more information, try '--help'.\n"
        );
        assert_eq!(trio(&env.lcl(&sub)), (2, "", missing.as_str()), "{sub:?}");
        let help = env.lcl(&[sub[0], sub[1], "--help"]);
        assert_eq!(
            (
                help.code,
                help.stdout.lines().find(|l| l.starts_with("Usage:"))
            ),
            (0, Some(usage)),
            "{sub:?}: {help:#?}"
        );
    }
}
