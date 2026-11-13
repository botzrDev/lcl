//! The contract a program relies on (step 13a): exit codes, `--json`
//! errors, help on every argument, and a closed stdout.
//!
//! Exit 0 is success, 1 a command that failed, 2 a command line that is
//! wrong. With `--json` among the arguments before any `--`, every error,
//! clap's included, is one line on stderr, `{"error":{"kind":K,"message":M}}`,
//! and stdout is empty: M is the text the error prints without --json, less
//! its "Error: " or "error: " prefix and clap's usage and "For more
//! information" lines. Every exit-2 error has kind usage. --help and
//! --version are not errors. A reader that closes stdout early (`lcl ... |
//! head`) ends lcl quietly, exit 0.

mod common;

use common::{Env, Out, error_text, exec, set_user_version};
use lcl_core::SCHEMA_VERSION;
use serde_json::{Value, json};
use std::io::Read;
use std::process::Stdio;

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env
}

fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

/// One JSON error line, as lcl prints it.
fn json_error(kind: &str, message: &str) -> String {
    format!("{}\n", json!({"error": {"kind": kind, "message": message}}))
}

/// Errors lcl finds after parsing: under --json (wherever it is before
/// `--`) one JSON line of the error's kind; without it `Error: M`, as
/// before. stdout is empty either way, and the exit code is the same.
#[test]
fn errors_after_parsing_are_one_json_line_under_json() {
    let env = workspace();
    env.create("one");
    env.lcl(&["label", "create", "bug"]).assert_ok();
    let db = env.db_path();
    let missing = env.cwd.path().join("no").join("such").join("x.jsonl");
    let missing = missing.display().to_string();
    let cases: Vec<(Vec<&str>, i32, &str, String)> = vec![
        (
            vec!["issue", "show", "99"],
            1,
            "not_found",
            "issue W-99 not found".into(),
        ),
        (
            vec!["doc", "show", "nope"],
            1,
            "not_found",
            "doc nope not found".into(),
        ),
        (
            vec!["label", "create", "Bug"],
            1,
            "conflict",
            "label \"Bug\" already exists (as \"bug\")".into(),
        ),
        (
            vec!["issue", "update", "1", "--parent", "1"],
            1,
            "conflict",
            "W-1 can't be its own parent".into(),
        ),
        (
            vec!["issue", "relate", "1", "blocks", "W-1"],
            1,
            "conflict",
            "W-1 can't be related to itself".into(),
        ),
        (
            vec!["issue", "show", "X-1"],
            2,
            "usage",
            "X-1 is not in this workspace (key W)".into(),
        ),
        (
            vec!["export", "--output", &missing],
            1,
            "io",
            format!(
                "can't write {missing}\n\nCaused by:\n    No such file or directory (os error 2)"
            ),
        ),
    ];
    for (args, code, kind, message) in &cases {
        let plain = env.lcl(args);
        assert_eq!(
            trio(&plain),
            (*code, "", format!("Error: {message}\n").as_str()),
            "{args:?}"
        );
        for at in [0, args.len()] {
            let mut with = args.clone();
            with.insert(at, "--json");
            let out = env.lcl(&with);
            assert_eq!(
                trio(&out),
                (*code, "", json_error(kind, message).as_str()),
                "{with:?}"
            );
            assert_eq!(error_text(&with, kind, message), out.stderr);
        }
    }

    // A newer schema, found through the context lcl adds: kind schema.
    set_user_version(&db, 99);
    let message = format!(
        "open workspace {}\n\nCaused by:\n    database schema v99 is newer than this lcl supports (v{SCHEMA_VERSION}); upgrade lcl",
        db.display()
    );
    assert_eq!(
        trio(&env.lcl(&["--json", "issue", "list"])),
        (1, "", json_error("schema", &message).as_str())
    );
    set_user_version(&db, SCHEMA_VERSION);
    // No HOME, where lcl needs it: kind other.
    let out = env.lcl_with(&["--json", "-w", "x", "issue", "list"], |cmd| {
        cmd.env_remove("HOME");
    });
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            json_error(
                "other",
                "HOME is not set; lcl keeps its workspace registry in $HOME/.lcl"
            )
            .as_str()
        )
    );
}

/// clap's errors, and the checks lcl makes the same way, are kind usage,
/// exit 2: the message is clap's, without its "error: " prefix, usage and
/// --help pointer, and keeps a tip. Without --json they print as before.
#[test]
fn clap_errors_are_one_json_line_under_json() {
    let env = workspace();
    env.create("one");
    let status = "invalid value 'Nope' for '--status <STATUS>': invalid status \"Nope\"; expected one of: Backlog, Todo, InProgress, InReview, Done, Canceled, Duplicate";
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--json", "issue", "list", "--status", "Nope"], status),
        (
            vec!["issue", "list", "--json", "--stauts", "x"],
            "unexpected argument '--stauts' found\n\n  tip: a similar argument exists: '--status'",
        ),
        (
            vec!["--json", "init", "x"],
            "the following required arguments were not provided:\n  --key <KEY>",
        ),
        (
            vec!["--json", "issue", "update", "1", "--estimate=-1"],
            "invalid value '-1' for '--estimate <ESTIMATE>': estimate must be 0 or more; got -1",
        ),
        (
            vec![
                "--json",
                "cycle",
                "create",
                "c",
                "--start",
                "2026-02-02",
                "--end",
                "2026-01-01",
            ],
            "cycle end 2026-01-01 is before its start 2026-02-02",
        ),
        (
            vec![
                "--json",
                "issue",
                "update",
                "1",
                "--add-label",
                "x",
                "--remove-label",
                "X",
            ],
            "label \"x\" is in both --add-label and --remove-label",
        ),
        // A group with no subcommand: clap's MissingSubcommand text, not the
        // group's help.
        (
            vec!["--json", "issue"],
            "'lcl issue' requires a subcommand but one was not provided\n  [subcommands: create, list, show, update, delete, relate, unrelate, comment, comments, help]",
        ),
        (
            vec!["--json", "doc"],
            "'lcl doc' requires a subcommand but one was not provided\n  [subcommands: create, list, show, update, delete, help]",
        ),
        (
            vec!["--json"],
            "'lcl' requires a subcommand but one was not provided\n  [subcommands: init, workspace, project, milestone, issue, comment, doc, label, cycle, search, export, import, tui, help]",
        ),
    ];
    for (args, message) in &cases {
        let out = env.lcl(args);
        assert_eq!(
            trio(&out),
            (2, "", json_error("usage", message).as_str()),
            "{args:?}"
        );
        let plain: Vec<&str> = args.iter().copied().filter(|a| *a != "--json").collect();
        let out = env.lcl(&plain);
        assert_eq!(out.code, 2, "{plain:?}");
        if plain.len() > 1 {
            assert!(
                out.stderr.starts_with(&format!("error: {message}\n")),
                "{plain:?}: {out:#?}"
            );
        }
    }
    // Without --json, exactly as clap prints it.
    assert_eq!(
        trio(&env.lcl(&["issue", "list", "--status", "Nope"])),
        (
            2,
            "",
            format!("error: {status}\n\nFor more information, try '--help'.\n").as_str()
        )
    );
    // A group with no subcommand still prints its help, on stderr.
    let out = env.lcl(&["issue"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.starts_with("Issues in this workspace: create, list, show, update, delete, relate, comment\n\nUsage: lcl issue [OPTIONS] <COMMAND>\n"), "{out:#?}");
}

/// --help and --version are not errors, with --json too: they print on
/// stdout as always, exit 0.
#[test]
fn help_and_version_are_not_errors() {
    let env = Env::new();
    let version = format!("lcl {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        trio(&env.lcl(&["--json", "--version"])),
        (0, version.as_str(), "")
    );
    for args in [
        vec!["--json", "--help"],
        vec!["issue", "--json", "--help"],
        vec!["--json", "help", "issue"],
    ] {
        let out = env.lcl(&args);
        assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{args:?}");
        assert!(out.stdout.contains("Usage: lcl"), "{args:?}: {out:#?}");
    }
}

/// `--json` after `--` is an argument, not the flag: its error prints as
/// text. Before `--` it is the flag, whatever follows.
#[test]
fn json_after_a_double_dash_is_an_argument() {
    let env = workspace();
    let out = env.lcl(&["issue", "show", "--", "--json"]);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr
            .starts_with("error: invalid value '--json' for '<ID>': invalid issue id \"--json\""),
        "{out:#?}"
    );
    assert_eq!(
        trio(&env.lcl(&["--json", "issue", "show", "--", "99"])),
        (
            1,
            "",
            json_error("not_found", "issue W-99 not found").as_str()
        )
    );
    let out = env.lcl(&["search", "--", "--json"]);
    assert_eq!(trio(&out), (0, "no matches\n", ""));
}

/// `lcl --help` ends with the exit codes and the --json error shape.
#[test]
fn help_states_the_exit_codes() {
    let out = Env::new().lcl(&["--help"]);
    out.assert_ok();
    let tail = "\nExit status: 0 success; 1 the command failed (not found, a conflict with what is stored, a newer schema, an I/O error); 2 the command line is wrong (a bad flag, value or combination).\nWith --json, an error is one line on stderr: {\"error\":{\"kind\":\"usage|not_found|conflict|schema|io|other\",\"message\":\"...\"}}\n";
    assert!(out.stdout.ends_with(tail), "{}", out.stdout);
}

/// The checks that span several arguments show the usage of the
/// subcommand they are about, never lcl's own.
#[test]
fn cross_argument_errors_show_their_subcommand_s_usage() {
    let env = workspace();
    env.create("one");
    for (args, message, usage) in [
        (
            vec![
                "cycle",
                "create",
                "c",
                "--start",
                "2026-02-02",
                "--end",
                "2026-01-01",
            ],
            "cycle end 2026-01-01 is before its start 2026-02-02",
            "lcl cycle create [OPTIONS] <NAME>",
        ),
        (
            vec![
                "issue",
                "update",
                "1",
                "--add-label",
                "x",
                "--remove-label",
                "X",
            ],
            "label \"x\" is in both --add-label and --remove-label",
            "lcl issue update [OPTIONS] <ID>",
        ),
        (
            vec!["issue", "list", "--project", "none", "--milestone", "m"],
            "--milestone \"m\" can't be used with --project none: issues in no project have no milestone",
            "lcl issue list [OPTIONS]",
        ),
    ] {
        let want =
            format!("error: {message}\n\nUsage: {usage}\n\nFor more information, try '--help'.\n");
        assert_eq!(trio(&env.lcl(&args)), (2, "", want.as_str()), "{args:?}");
    }
}

/// A negative estimate says what an estimate is, exit 2.
#[test]
fn a_negative_estimate_says_what_an_estimate_is() {
    let env = workspace();
    env.create("one");
    for args in [
        vec!["issue", "create", "--title", "x", "--estimate", "-1"],
        vec!["issue", "update", "1", "--estimate=-1"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}");
        assert!(out.stderr.starts_with("error: invalid value '-1' for '--estimate <ESTIMATE>': estimate must be 0 or more; got -1\n"), "{args:?}: {out:#?}");
    }
    let out = env.lcl(&["issue", "update", "1", "--estimate", "0", "--json"]);
    assert_eq!(out.assert_ok().json()["estimate"], 0);
}

/// stdout closed at once, over more output than a pipe holds (64 KiB), so
/// a write must fail: each command ends quietly, exit 0, nothing on
/// stderr. Text, JSON and export to stdout.
#[test]
fn a_closed_stdout_ends_lcl_quietly() {
    let env = workspace();
    let big = "x".repeat(200_000);
    let file = env.cwd.path().join("big.txt");
    std::fs::write(&file, &big).expect("write description");
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "big",
        "--description-file",
        &file.display().to_string(),
    ])
    .assert_ok();
    let long_title = "t".repeat(30_000);
    for _ in 0..3 {
        env.lcl(&["issue", "create", "--title", &long_title])
            .assert_ok();
    }
    for args in [
        vec!["issue", "show", "1"],
        vec!["--json", "issue", "show", "1"],
        vec!["issue", "list"],
        vec!["--json", "issue", "list"],
        vec!["export"],
    ] {
        let full = env.lcl(&args);
        full.assert_ok();
        assert!(
            full.stdout.len() > 64 * 1024,
            "{args:?} prints only {} bytes",
            full.stdout.len()
        );
        let mut child = env
            .command(env!("CARGO_BIN_EXE_lcl"))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn lcl");
        drop(child.stdout.take());
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("stderr")
            .read_to_string(&mut stderr)
            .expect("read stderr");
        let status = child.wait().expect("wait for lcl");
        assert_eq!((status.code(), stderr.as_str()), (Some(0), ""), "{args:?}");
    }
}

/// The kinds a program sees for the exit-1 errors that used to exit 2:
/// each depends on what is stored.
#[test]
fn stored_state_refusals_exit_1_as_conflicts() {
    let env = workspace();
    for title in ["a", "b", "c", "d"] {
        env.create(title);
    }
    env.lcl(&["issue", "update", "2", "--parent", "1"])
        .assert_ok();
    env.lcl(&["issue", "relate", "1", "blocks", "3"])
        .assert_ok();
    env.lcl(&["issue", "relate", "3", "duplicate-of", "4"])
        .assert_ok();
    for (args, message) in [
        (
            vec!["issue", "update", "1", "--parent", "#1"],
            "W-1 can't be its own parent",
        ),
        (
            vec!["issue", "update", "1", "--parent", "2"],
            "making W-2 the parent of W-1 would make a cycle: W-1 -> W-2 -> W-1",
        ),
        (
            vec!["issue", "relate", "#3", "related", "3"],
            "W-3 can't be related to itself",
        ),
        (
            vec!["issue", "relate", "3", "blocks", "1"],
            "W-3 blocking W-1 would make a cycle: W-3 blocks W-1 blocks W-3",
        ),
        (
            vec!["issue", "relate", "3", "duplicate-of", "2"],
            "W-3 is already a duplicate of W-4; unrelate it first: lcl issue unrelate W-3 duplicate-of W-4",
        ),
        (
            vec!["issue", "relate", "4", "duplicate-of", "3"],
            "making W-4 a duplicate of W-3 would make a cycle: W-4 -> W-3 -> W-4",
        ),
    ] {
        let mut with: Vec<&str> = vec!["--json"];
        with.extend(&args);
        let out = env.lcl(&with);
        let err: Value = serde_json::from_str(out.stderr.trim_end())
            .unwrap_or_else(|e| panic!("{with:?}: {e}: {out:#?}"));
        assert_eq!(
            (out.code, err),
            (
                1,
                json!({"error": {"kind": "conflict", "message": message}})
            ),
            "{with:?}"
        );
    }
}

/// Every schema error is kind schema under --json: a newer registry, and a
/// stored status or priority this lcl doesn't know (a newer schema's
/// SchemaTooNew is pinned above).
#[test]
fn every_schema_error_is_kind_schema() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'Weird' WHERE number = 1;",
    );
    let out = env.lcl(&["--json", "issue", "show", "1"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            json_error(
                "schema",
                "issue 1 has status \"Weird\", which this lcl doesn't know"
            )
            .as_str()
        )
    );
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'Backlog', priority = 9 WHERE number = 1;",
    );
    let out = env.lcl(&["--json", "issue", "show", "1"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            json_error(
                "schema",
                "issue 1 has priority 9, which this lcl doesn't know (expected 0-4)"
            )
            .as_str()
        )
    );

    let index = env.home.path().join(".lcl").join("index.sqlite");
    exec(
        &index,
        "UPDATE registry_meta SET value = '99' WHERE key = 'schema_version'",
    );
    let message = format!(
        "the workspace index {} is v99, newer than this lcl supports (v2); upgrade lcl",
        index.display()
    );
    assert_eq!(
        trio(&env.lcl(&["--json", "workspace", "list"])),
        (1, "", json_error("schema", &message).as_str())
    );
}

/// An error that other stored data would avoid exits 1: an unknown label
/// (not found), and a milestone in another project, in several, or with
/// no project to be in (conflict). A new issue's --milestone needs
/// --project on the command line: without it the command can never
/// succeed (exit 2).
#[test]
fn refusals_other_stored_data_would_avoid_exit_1() {
    let env = workspace();
    env.create("one");
    for p in ["p", "q"] {
        env.lcl(&["project", "create", p]).assert_ok();
    }
    env.lcl(&["milestone", "create", "m", "--project", "p"])
        .assert_ok();
    let cases: Vec<(Vec<&str>, &str, &str)> = vec![
        (
            vec!["issue", "create", "--title", "x", "--label", "nope"],
            "not_found",
            "unknown label \"nope\"; create it with: lcl label create nope",
        ),
        (
            vec!["issue", "list", "--label", "nope"],
            "not_found",
            "unknown label \"nope\"; create it with: lcl label create nope",
        ),
        (
            vec![
                "issue",
                "create",
                "--title",
                "x",
                "--project",
                "q",
                "--milestone",
                "m",
            ],
            "conflict",
            "milestone \"m\" is not in project \"q\" (it is in project \"p\")",
        ),
        (
            vec!["issue", "update", "1", "--milestone", "m"],
            "conflict",
            "milestone \"m\" needs a project: the issue has none; pass --project too",
        ),
    ];
    let db = env.db_path();
    for (args, kind, message) in &cases {
        let before = common::dump(&db);
        assert_eq!(
            trio(&env.lcl(args)),
            (1, "", format!("Error: {message}\n").as_str()),
            "{args:?}"
        );
        let mut with = vec!["--json"];
        with.extend(args);
        assert_eq!(
            trio(&env.lcl(&with)),
            (1, "", json_error(kind, message).as_str()),
            "{with:?}"
        );
        assert!(common::dump(&db) == before, "{args:?} wrote");
    }
    env.lcl(&["milestone", "create", "m", "--project", "q"])
        .assert_ok();
    let out = env.lcl(&["--json", "milestone", "show", "m"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            json_error(
                "conflict",
                "milestone \"m\" is in projects \"p\" and \"q\"; pass --project to choose"
            )
            .as_str()
        )
    );
    let out = env.lcl(&[
        "--json",
        "issue",
        "create",
        "--title",
        "x",
        "--milestone",
        "m",
    ]);
    assert_eq!(
        trio(&out),
        (
            2,
            "",
            json_error(
                "usage",
                "the following required arguments were not provided:\n  --project <PROJECT>"
            )
            .as_str()
        )
    );
}

/// A workspace file lcl may not write is an io error, not other: SQLite's
/// read-only, can't-open, I/O and disk-full errors are kind io. Skipped
/// where the file stays writable after chmod 0444 (as root).
#[test]
fn a_read_only_workspace_is_kind_io() {
    use std::os::unix::fs::PermissionsExt;
    let env = workspace();
    let db = env.db_path();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).expect("chmod 0444");
    if std::fs::OpenOptions::new().write(true).open(&db).is_ok() {
        eprintln!(
            "SKIPPED a_read_only_workspace_is_kind_io: {} is still writable (running as root?)",
            db.display()
        );
        return;
    }
    let out = env.lcl(&["--json", "issue", "create", "--title", "x"]);
    assert_eq!((out.code, out.stdout.as_str()), (1, ""), "{out:#?}");
    let err: Value =
        serde_json::from_str(out.stderr.strip_suffix('\n').expect("one line")).expect("JSON");
    assert_eq!(err["error"]["kind"], "io", "{err}");
    // The rest is rusqlite's and SQLite's own chain of causes.
    let message = err["error"]["message"].as_str().expect("message");
    assert!(
        message.starts_with("database error: attempt to write a readonly database\n"),
        "{message}"
    );
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).expect("chmod 0644");
}

/// Notices stay plain `lcl:` lines on stderr. Under --json the error is the
/// last line, and the only one that starts with `{`.
#[test]
fn under_json_the_error_is_the_last_stderr_line() {
    let env = workspace();
    let out = env.lcl_env(&[("LCL_PROJECT", "w")], &["--json", "issue", "show", "999"]);
    let want = format!(
        "lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE\n{}",
        json_error("not_found", "issue W-999 not found")
    );
    assert_eq!(trio(&out), (1, "", want.as_str()));
    let lines: Vec<&str> = out.stderr.lines().collect();
    assert_eq!(lines.iter().filter(|l| l.starts_with('{')).count(), 1);
    assert!(lines.last().is_some_and(|l| l.starts_with('{')));
}
