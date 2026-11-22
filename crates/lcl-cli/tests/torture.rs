//! What rough handling found: long chains, blank and hostile values, files
//! that aren't workspaces, and numbers at the edge of what fits.
//!
//! Each test here failed, or took minutes, before its fix.

mod common;

use common::{Env, exec};
use serde_json::json;
use std::fmt::Write;
use std::time::{Duration, Instant};

/// A chain this long took minutes to check when every link was walked once
/// for every issue on it. Checked in one pass, it takes well under a second.
const LONG: i64 = 20_000;
/// Far more than a one-pass check needs, far less than the old one took.
const PROMPT: Duration = Duration::from_secs(60);

fn header(key: &str, next: i64) -> String {
    format!(
        "{}\n",
        json!({"type": "workspace", "format": "lcl-export/1", "name": "w", "key": key, "next_issue_number": next})
    )
}

fn issue(number: i64, parent: Option<i64>) -> String {
    format!(
        "{}\n",
        json!({"type": "issue", "number": number, "title": "t", "parent": parent})
    )
}

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env
}

#[test]
fn a_long_parent_chain_imports_and_exports_promptly() {
    let env = workspace();
    let mut file = header("W", LONG + 1);
    for n in 1..=LONG {
        file.push_str(&issue(n, (n < LONG).then_some(n + 1)));
    }
    let started = Instant::now();
    env.lcl_stdin(&["import", "-"], file.as_bytes()).assert_ok();
    env.lcl(&["export", "--output", "out.jsonl"]).assert_ok();
    assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
    // The chain is all there: the top issue can't go under the bottom one.
    let out = env.lcl(&["issue", "update", &LONG.to_string(), "--parent", "1"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("would make a cycle"), "{out:#?}");
}

/// Each issue blocks the one before it, listed from the far end: the order
/// in which every row added used to walk the whole chain behind it.
#[test]
fn a_long_blocks_chain_imports_and_exports_promptly() {
    let env = workspace();
    let mut file = header("W", LONG + 1);
    for n in 1..=LONG {
        file.push_str(&issue(n, None));
    }
    for n in 2..=LONG {
        writeln!(
            file,
            "{}",
            json!({"type": "relation", "kind": "blocks", "from": n, "to": n - 1})
        )
        .unwrap();
    }
    for n in 2..=LONG {
        writeln!(
            file,
            "{}",
            json!({"type": "relation", "kind": "duplicate", "from": n - 1, "to": n})
        )
        .unwrap();
    }
    let started = Instant::now();
    env.lcl_stdin(&["import", "-"], file.as_bytes()).assert_ok();
    env.lcl(&["export", "--output", "out.jsonl"]).assert_ok();
    assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
}

/// A loop in a file is still found, on the line that closes it, and a long
/// one is named by its ends.
#[test]
fn a_long_loop_is_refused_and_named_by_its_ends() {
    let env = workspace();
    let mut file = header("W", 101);
    for n in 1..=100 {
        file.push_str(&issue(n, Some(if n < 100 { n + 1 } else { 1 })));
    }
    let out = env.lcl_stdin(&["import", "-"], file.as_bytes());
    assert_eq!(out.code, 2, "{out:#?}");
    assert_eq!(
        out.stderr,
        "Error: line 2: parent: making W-2 the parent of W-1 would make a cycle: \
         W-1 -> W-2 -> W-3 -> W-4 -> W-5 -> … 91 more … -> W-97 -> W-98 -> W-99 -> W-100 -> W-1\n"
    );

    let mut file = header("W", 4);
    for n in 1..=3 {
        file.push_str(&issue(n, None));
    }
    for (from, to) in [(1, 2), (2, 3), (3, 1)] {
        writeln!(
            file,
            "{}",
            json!({"type": "relation", "kind": "blocks", "from": from, "to": to})
        )
        .unwrap();
    }
    let out = env.lcl_stdin(&["import", "-"], file.as_bytes());
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("W-3 blocking W-1 would make a cycle: W-3 blocks W-1 blocks W-2 blocks W-3"),
        "{out:#?}"
    );
}

#[test]
fn a_blank_title_is_refused_on_the_command_line() {
    let env = workspace();
    env.create("kept");
    env.lcl(&["doc", "create", "--title", "kept"]).assert_ok();
    let doc = env.lcl(&["doc", "list", "--json"]).json()[0]["id"]
        .as_str()
        .expect("doc id")
        .to_string();
    for blank in ["", " ", "\t\n"] {
        for args in [
            vec!["issue", "create", "--title", blank],
            vec!["issue", "update", "1", "--title", blank],
            vec!["doc", "create", "--title", blank],
            vec!["doc", "update", &doc, "--title", blank],
        ] {
            let out = env.lcl(&args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains("a title can't be blank"), "{out:#?}");
        }
    }
    let show = env.lcl(&["issue", "show", "1", "--json"]).json();
    assert_eq!(show["issue"]["title"], "kept");
    // A title is otherwise stored as given.
    env.lcl(&["issue", "update", "1", "--title", " padded "])
        .assert_ok();
    let show = env.lcl(&["issue", "show", "1", "--json"]).json();
    assert_eq!(show["issue"]["title"], " padded ");
}

/// An empty `.lcl/data.sqlite` used to be opened as a workspace with no name
/// and no key, whose first issue was `#1`.
#[test]
fn an_empty_file_is_not_a_workspace() {
    let env = Env::new();
    let db = env.db_path();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"").unwrap();
    for args in [
        vec!["issue", "create", "--title", "x"],
        vec!["issue", "list"],
        vec!["workspace", "show"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert_eq!(
            out.stderr,
            format!(
                "Error: {} is an empty file, not an lcl workspace; delete it, or run lcl init there to replace it\n",
                db.display()
            )
        );
    }
    assert_eq!(std::fs::read(&db).unwrap(), b"", "the file was written to");

    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    let show = env.lcl(&["workspace", "show", "--json"]).assert_ok().json();
    assert_eq!((&show["name"], &show["key"]), (&json!("w"), &json!("W")));
    assert_eq!(env.create("first"), 1);
}

#[test]
fn the_last_issue_number_is_handed_out_once_and_then_refused() {
    let env = workspace();
    env.lcl_stdin(&["import", "-"], header("W", i64::MAX).as_bytes())
        .assert_ok();
    assert_eq!(env.create("last"), i64::MAX);
    let out = env.lcl(&["issue", "create", "--title", "one too many"]);
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (1, "Error: this workspace has run out of issue numbers\n")
    );
    env.lcl(&["issue", "list"]).assert_ok();
}

#[test]
fn the_tui_says_it_needs_a_terminal() {
    let env = workspace();
    let out = env.lcl(&["tui"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            1,
            "",
            "Error: lcl tui needs a terminal: stdin and stdout must both be one\n"
        )
    );
}

#[test]
fn a_doc_that_is_not_there_is_named_the_same_way_by_every_command() {
    let env = workspace();
    for args in [
        vec!["doc", "show", "nope"],
        vec!["doc", "update", "nope", "--title", "t"],
        vec!["doc", "delete", "nope"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(
            (out.code, out.stderr.as_str()),
            (1, "Error: doc nope not found\n"),
            "{args:?}"
        );
    }
}

/// A database error is said once, and one about the registry names its file.
#[test]
fn a_file_that_is_not_a_database_is_said_once_with_its_path() {
    let env = workspace();
    let index = env.home.path().join(".lcl").join("index.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", index.display()));
    }
    std::fs::write(&index, [b'x'; 4096]).unwrap();
    let out = env.lcl(&["workspace", "list"]);
    assert_eq!(
        (out.code, out.stderr),
        (
            1,
            format!(
                "Error: open the workspace registry {}\n\nCaused by:\n    database error: file is not a database\n",
                index.display()
            )
        )
    );

    let db = env.db_path();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    std::fs::write(&db, [b'x'; 4096]).unwrap();
    let out = env.lcl(&["issue", "list"]);
    assert_eq!(
        (out.code, out.stderr),
        (
            1,
            format!(
                "Error: open workspace {}\n\nCaused by:\n    database error: file is not a database\n",
                db.display()
            )
        )
    );
}

/// Stored text reaches a pipe or a file byte for byte, control characters
/// and all: only a terminal is shown their pictures (see `for_terminal`).
#[test]
fn piped_output_keeps_control_characters() {
    let env = workspace();
    let title = "red \u{1b}[31m bell \u{7}";
    env.create(title);
    let out = env.lcl(&["issue", "list"]);
    assert_eq!(out.stdout, format!("W-1\t[Backlog]\tNone\t{title}\n"));
    // Not a way in for a hand edit either: the row is still read.
    exec(
        &env.db_path(),
        "UPDATE issues SET title = title || char(27) WHERE number = 1",
    );
    env.lcl(&["issue", "show", "1"]).assert_ok();
}
