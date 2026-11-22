//! `lcl backup PATH`: the invariants of `lcl_core::backup`, held by the
//! command against real processes.
//!
//! - Whole or absent: PATH is a complete workspace or is not there, also
//!   after a kill or an unwritable destination.
//! - Never replaced: an existing PATH keeps its bytes.
//! - One committed state, same rows and rowids: the copy equals the source
//!   table for table, and a copy taken while another process writes is a
//!   state some transaction left.

mod common;

use common::{Env, FTS_TABLES, dump, foreign_key_violations, fts_integrity, user_version};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

/// A workspace with issues, labels, comments, relations and docs, and
/// `out/`, an empty directory for copies.
fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.lcl(&["label", "create", "bug"]).assert_ok();
    for n in 1..=6 {
        let title = format!("parser issue {n}");
        env.lcl(&[
            "issue",
            "create",
            "--title",
            &title,
            "--label",
            "bug",
            "--description",
            "tokenizer words",
        ])
        .assert_ok();
        env.lcl(&["issue", "comment", &n.to_string(), "a searchable remark"])
            .assert_ok();
    }
    env.lcl(&["issue", "relate", "1", "blocks", "2"])
        .assert_ok();
    env.lcl(&["issue", "delete", "5"]).assert_ok();
    env.lcl(&[
        "doc",
        "create",
        "--title",
        "Notes",
        "--content",
        "tokenizer design",
    ])
    .assert_ok();
    std::fs::create_dir(env.cwd.path().join("out")).expect("create out/");
    env
}

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect()
}

fn integrity(db: &Path) -> String {
    let conn = rusqlite::Connection::open(db).expect("open copy");
    conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check")
}

fn count(db: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open copy");
    conn.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The copy opened as a workspace of its own, in a fresh environment.
fn restored(copy: &Path) -> Env {
    let env = Env::new();
    env.install_db(copy.to_str().expect("UTF-8 path"));
    env
}

#[test]
fn a_backup_is_the_workspace_table_for_table() {
    let env = workspace();
    let out = env.cwd.path().join("out");
    let copy = out.join("copy.sqlite");

    let printed = env
        .lcl(&["backup", "out/copy.sqlite", "--json"])
        .assert_ok()
        .json();
    assert_eq!(printed["path"], "out/copy.sqlite");
    assert_eq!(printed["schema_version"], user_version(&env.db_path()));
    assert_eq!(printed["bytes"], std::fs::metadata(&copy).unwrap().len());

    // No litter: the directory gained the copy and nothing else.
    assert_eq!(names(&out), BTreeSet::from(["copy.sqlite".to_string()]));
    assert_eq!(user_version(&copy), user_version(&env.db_path()));
    assert_eq!(integrity(&copy), "ok");
    // Same rows, same rowids, search index tables included.
    assert_eq!(dump(&copy), dump(&env.db_path()));
    fts_integrity(&copy, &FTS_TABLES);
    assert!(foreign_key_violations(&copy).is_empty());

    // Opened as a workspace, it lists and finds what the source does.
    let other = restored(&copy);
    for args in [
        &["search", "tokenizer", "--json"][..],
        &["search", "remark", "--json"],
        &["search", "pars*", "--json"],
        &["issue", "list", "--all", "--json"],
        &["doc", "list", "--json"],
    ] {
        let theirs = other.lcl(args).assert_ok().json();
        assert_eq!(theirs, env.lcl(args).assert_ok().json(), "{args:?}");
        assert_ne!(theirs, json!({}), "{args:?}");
    }
    assert_eq!(
        other.lcl(&["search", "tokenizer", "--json"]).json()["issue_total"],
        5
    );
    // A deleted issue's number is not handed out again in the copy either.
    assert_eq!(other.create("next"), 7);

    // Without --json it says where the copy went.
    let plain = env.lcl(&["backup", "out/second.sqlite"]);
    assert!(
        plain
            .assert_ok()
            .stdout
            .starts_with("Backed up to out/second.sqlite ("),
        "{plain:#?}"
    );
}

#[test]
fn an_existing_path_is_refused_with_exit_1_and_left_untouched() {
    let env = workspace();
    let taken = env.cwd.path().join("out").join("taken");
    std::fs::write(&taken, b"mine").unwrap();
    for target in ["out/taken", "out", ".lcl/data.sqlite"] {
        let before = common::files(&env);
        let out = env.lcl(&["backup", target, "--json"]);
        assert_eq!(out.code, 1, "{target}\n{out:#?}");
        assert_eq!(out.stdout, "");
        let error: Value = serde_json::from_str(out.stderr.lines().last().unwrap()).unwrap();
        assert_eq!(error["error"]["kind"], "conflict", "{out:#?}");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{target} already exists")),
            "{out:#?}"
        );
        // Nothing changed anywhere: not the target, not the workspace.
        assert_eq!(common::files(&env), before, "{target}");
    }
}

#[test]
fn an_unwritable_destination_exits_1_names_the_path_and_leaves_nothing() {
    let env = workspace();
    let out_dir = env.cwd.path().join("out");

    let out = env.lcl(&["backup", "out/no-such-dir/copy.sqlite", "--json"]);
    assert_eq!(out.code, 1, "{out:#?}");
    let error: Value = serde_json::from_str(out.stderr.lines().last().unwrap()).unwrap();
    assert_eq!(error["error"]["kind"], "io", "{out:#?}");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("can't write out/no-such-dir/copy.sqlite: "),
        "{out:#?}"
    );
    assert_eq!(names(&out_dir), BTreeSet::new());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&out_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root writes where the mode says no; then there is nothing to test.
        if std::fs::write(out_dir.join("probe"), b"").is_ok() {
            eprintln!("SKIPPED: this user can write to a read-only directory");
            return;
        }
        let out = env.lcl(&["backup", "out/copy.sqlite"]);
        std::fs::set_permissions(&out_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(out.code, 1, "{out:#?}");
        assert!(
            out.stderr
                .starts_with("Error: can't write out/copy.sqlite: "),
            "{out:#?}"
        );
        assert_eq!(names(&out_dir), BTreeSet::new());
    }
}

/// While one process creates issues, each with a label (one transaction:
/// the issue, its label link and its search row) and then a comment (a
/// second), another takes backups. Every copy is a state a transaction
/// left: nothing half written, and never ahead of or behind itself.
#[test]
fn a_backup_taken_while_another_process_writes_is_one_committed_state() {
    const WRITES: i64 = 40;
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.lcl(&["label", "create", "bug"]).assert_ok();
    std::fs::create_dir(env.cwd.path().join("out")).unwrap();

    let done = AtomicBool::new(false);
    let copies = std::thread::scope(|s| {
        s.spawn(|| {
            for n in 1..=WRITES {
                let title = format!("written {n}");
                env.lcl(&["issue", "create", "--title", &title, "--label", "bug"])
                    .assert_ok();
                env.lcl(&["issue", "comment", &n.to_string(), "remark"])
                    .assert_ok();
            }
            done.store(true, Ordering::SeqCst);
        });
        let mut copies = Vec::new();
        loop {
            // Read before the backup: the last copy is then taken after
            // every write, the ones before it while writes were under way.
            let last = done.load(Ordering::SeqCst);
            let name = format!("out/copy-{}.sqlite", copies.len());
            env.lcl(&["backup", &name]).assert_ok();
            copies.push(env.cwd.path().join(name));
            if last {
                break copies;
            }
        }
    });
    assert!(copies.len() >= 2, "no backup ran beside the writer");

    let mut earlier = 0;
    let mut mid_stream = 0;
    for copy in &copies {
        assert_eq!(integrity(copy), "ok", "{copy:?}");
        fts_integrity(copy, &FTS_TABLES);
        assert!(foreign_key_violations(copy).is_empty(), "{copy:?}");
        let issues = count(copy, "SELECT count(*) FROM issues");
        // Issue numbers are 1..=issues, and the next number follows them.
        assert_eq!(
            count(copy, "SELECT coalesce(max(number), 0) FROM issues"),
            issues
        );
        assert_eq!(
            count(
                copy,
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'next_issue_number'"
            ),
            issues + 1
        );
        // What one transaction wrote is all there or not at all.
        assert_eq!(count(copy, "SELECT count(*) FROM issue_labels"), issues);
        assert_eq!(count(copy, "SELECT count(*) FROM issues_fts"), issues);
        // The comment comes in a later transaction: at most one behind.
        let comments = count(copy, "SELECT count(*) FROM comments");
        assert!(
            comments == issues || comments == issues - 1,
            "{issues} issues, {comments} comments"
        );
        // Time doesn't run backwards from one copy to the next.
        assert!(issues >= earlier, "{copy:?}");
        earlier = issues;
        if issues > 0 && issues < WRITES {
            mid_stream += 1;
        }
    }
    // Without a copy from the middle, this tested nothing about writers.
    assert!(
        mid_stream > 0,
        "no backup was taken while writes were under way"
    );
    assert_eq!(earlier, WRITES, "the last copy has every write");
    // And the search in a copy finds exactly its own issues.
    let last = restored(copies.last().unwrap());
    assert_eq!(
        last.lcl(&["search", "written", "--json"])
            .assert_ok()
            .json()["issue_total"],
        WRITES
    );
}

/// A backup killed at any moment leaves PATH absent or whole, never part
/// of a file, and what it leaves beside PATH has only the temporary name.
#[test]
fn a_killed_backup_leaves_nothing_at_the_path() {
    const ISSUES: i64 = 3000;
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    let mut file = format!(
        "{}\n",
        json!({"type": "workspace", "format": "lcl-export/1", "name": "w", "key": "W", "next_issue_number": ISSUES + 1})
    );
    let description = "many words to fill pages ".repeat(60);
    for n in 1..=ISSUES {
        file.push_str(&format!(
            "{}\n",
            json!({"type": "issue", "number": n, "title": "t", "description": description})
        ));
    }
    env.lcl_stdin(&["import", "-"], file.as_bytes()).assert_ok();
    let out_dir = env.cwd.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let source = dump(&env.db_path());

    // How long a whole backup takes here, to aim the kills inside one.
    let started = std::time::Instant::now();
    env.lcl(&["backup", "out/whole.sqlite"]).assert_ok();
    let whole = started.elapsed();
    std::fs::remove_file(out_dir.join("whole.sqlite")).unwrap();

    let temporary_of = |name: &str| -> bool {
        names(&out_dir)
            .iter()
            .any(|n| n.starts_with(&format!(".{name}.lcl-backup-")))
    };
    let mut killed_mid_copy = 0;
    // From before it starts to after it ends, in twentieths of a backup.
    for i in 0..=24u32 {
        let name = format!("k{i}.sqlite");
        let mut child = env
            .command(env!("CARGO_BIN_EXE_lcl"))
            .args(["backup", &format!("out/{name}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lcl backup");
        std::thread::sleep(whole * i / 20);
        let _ = child.kill();
        child.wait().expect("wait for lcl backup");
        let path = out_dir.join(&name);
        if path.exists() {
            assert_eq!(integrity(&path), "ok", "kill {i}");
            assert_eq!(count(&path, "SELECT count(*) FROM issues"), ISSUES);
        } else if temporary_of(&name) {
            // Killed with the copy under way: its temporary file is there,
            // and nothing is at the path.
            killed_mid_copy += 1;
        }
    }
    assert!(
        killed_mid_copy > 0,
        "no backup was killed while it was copying (a whole one takes {whole:?})"
    );
    for name in names(&out_dir) {
        let whole = name.starts_with('k') && name.ends_with(".sqlite");
        let temporary = name.starts_with(".k") && name.contains(".sqlite.lcl-backup-");
        assert!(whole || temporary, "unexpected file {name}");
    }
    // The workspace is as it was, and a backup still works.
    assert_eq!(dump(&env.db_path()), source);
    env.lcl(&["backup", "out/after.sqlite"]).assert_ok();
    assert_eq!(dump(&out_dir.join("after.sqlite")), source);
}
