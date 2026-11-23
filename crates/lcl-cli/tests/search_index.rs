//! The search indexes' checks (LOCL-42). Every open checks, read-only, that
//! each index lists exactly its table's rowids, so a read runs while another
//! process holds the write lock. A process's first write also compares the
//! words, under the lock that write holds; so does an upgrade, before it
//! commits. A mismatch is kind schema; nothing else is.

mod common;

use common::{Env, V0_FIXTURE, error_text, exec, snapshot_path, user_version};
use rusqlite::Connection;
use std::sync::mpsc;
use std::time::Duration;

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "--key", "TEST"]).assert_ok();
    env.create("one");
    env.lcl(&["issue", "comment", "1", "a remark"]).assert_ok();
    env.lcl(&["doc", "create", "--title", "notes"]).assert_ok();
    env
}

/// `args` exits 1 with kind schema and `message`, printing nothing on stdout.
#[track_caller]
fn assert_schema_error(env: &Env, args: &[&str], message: &str) {
    let out = env.lcl(args);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", error_text(args, "schema", message).as_str()),
    );
}

/// The error an open of `env`'s workspace fails with, for `cause`.
fn open_error(env: &Env, cause: &str) -> String {
    format!(
        "open workspace {}\n\nCaused by:\n    {cause}",
        env.db_path().display()
    )
}

/// Read commands complete while a second connection holds the write lock.
/// It is held until they are all done: one that waited on it would give up
/// after lcl's 30 s busy timeout and fail.
#[test]
fn reads_run_while_another_connection_holds_the_write_lock() {
    let env = workspace();
    let db = env.db_path();
    let copy = env.cwd.path().join("copy.sqlite");
    let copy_arg = copy.to_str().expect("UTF-8 path");
    let reads: [&[&str]; 7] = [
        &["issue", "list", "--json"],
        &["issue", "show", "1", "--json"],
        &["issue", "comments", "1", "--json"],
        &["search", "one", "--json"],
        &["doc", "list", "--json"],
        &["export"],
        &["backup", copy_arg],
    ];
    let (locked_tx, locked) = mpsc::channel();
    let (done_tx, done) = mpsc::channel::<()>();
    let outs = std::thread::scope(|s| {
        let db = &db;
        let holder = s.spawn(move || {
            let conn = Connection::open(db).expect("open db");
            conn.execute_batch("BEGIN IMMEDIATE")
                .expect("take the write lock");
            locked_tx.send(()).expect("signal");
            let held_to_the_end = done.recv_timeout(Duration::from_secs(120)).is_ok();
            conn.execute_batch("COMMIT")
                .expect("release the write lock");
            held_to_the_end
        });
        locked.recv().expect("lock taken");
        let outs: Vec<_> = reads.iter().map(|args| env.lcl(args)).collect();
        done_tx.send(()).expect("signal");
        assert!(
            holder.join().expect("holder thread"),
            "the lock was let go early"
        );
        outs
    });
    for out in &outs {
        out.assert_ok();
    }
    assert_eq!(outs[0].json()["total"], 1);
    assert_eq!(outs[3].json()["issues"][0]["identifier"], "TEST-1");
    assert!(copy.exists());
}

/// A row the index has under another rowid fails the open, in each index:
/// the read exits 1, kind schema, and names both sides of the difference.
/// The rowid moves behind the index's back: the update triggers watch the
/// indexed columns only.
#[test]
fn an_index_naming_other_rowids_fails_the_open() {
    for (index, renumber) in [
        ("issues_fts", "UPDATE issues SET rowid = rowid + 1000"),
        ("docs_fts", "UPDATE docs SET rowid = rowid + 1000"),
        ("comments_fts", "UPDATE comments SET rowid = rowid + 1000"),
    ] {
        let env = workspace();
        exec(&env.db_path(), renumber);
        assert_schema_error(
            &env,
            &["--json", "issue", "list"],
            &open_error(
                &env,
                &format!(
                    "the search index {index} does not match its rows: \
                     1 row it does not list and 1 rowid it lists that no row has"
                ),
            ),
        );
    }
}

/// Words that differ under the same rowids pass the read-only check, so a
/// read still runs. The first write compares the words, finds them changed,
/// and fails before it writes anything.
#[test]
fn changed_words_under_the_same_rowids_fail_the_first_write() {
    let env = workspace();
    let db = env.db_path();
    let conn = Connection::open(&db).expect("open db");
    let trigger: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'issues_au'",
            [],
            |r| r.get(0),
        )
        .expect("the update trigger");
    conn.execute_batch(&format!(
        "DROP TRIGGER issues_au; UPDATE issues SET title = 'changed'; {trigger};"
    ))
    .expect("change a title behind the index's back");
    drop(conn);

    env.lcl(&["issue", "list", "--json"]).assert_ok();
    assert_schema_error(
        &env,
        &["--json", "issue", "create", "--title", "two"],
        "the search index issues_fts does not match its rows: \
         the words it holds are not the words of its rows",
    );
    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list["total"], 1, "the write landed: {list}");
}

/// An upgrade checks the indexes before it commits. One that would leave an
/// index not matching its rows is not committed: the file stays at its old
/// schema and the snapshot taken for the upgrade goes.
#[test]
fn an_upgrade_that_leaves_an_index_not_matching_is_not_committed() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    exec(
        &db,
        "DELETE FROM issues_fts_docsize WHERE id = (SELECT min(rowid) FROM issues)",
    );
    assert_schema_error(
        &env,
        &["--json", "issue", "list"],
        &open_error(
            &env,
            "the search index issues_fts does not match its rows: 1 row it does not list",
        ),
    );
    assert_eq!(user_version(&db), 0);
    assert!(!snapshot_path(&db, 0).exists());
}
