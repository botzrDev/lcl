//! Migration 10: the values lcl 0.1.0 accepted but lcl can't read, made
//! readable, so a 0.1.0 workspace can be exported. Text lcl can't read is
//! kept where it can be seen, or dropped only when it said nothing. The
//! upgrade says what it changed in one line on stderr, once, and says
//! nothing when it changed nothing.
//!
//! tests/fixtures/v010-dirty.sqlite was built like v0.sqlite (see
//! tests/schema.rs): lcl 0.1.0 at ~/.local/bin/lcl (sha256 1b42efae...),
//! copied to $F/bin/lcl, and run as
//! `cd $F/cwd && env -i PATH="$F/bin:/usr/bin:/bin" HOME=$F/home sh -e recipe.sh`
//! from an empty cwd, with this recipe.sh:
//!
//!     docid() { python3 -I -c 'import json,sys; print(json.load(sys.stdin)["id"])'; }
//!     lcl init prob
//!     lcl label create " bug " --color red
//!     lcl label create ""
//!     lcl label create x --color "#12345"
//!     lcl label create padded --color " #fff"
//!     lcl label create nocolor --color ""
//!     lcl label create keep --color "#00aa11"
//!     lcl issue create --title t1 --assignee " alice " --due tomorrow
//!     lcl issue update 1 --due "next week" --assignee "  "
//!     lcl issue create --title "" --assignee ""
//!     lcl issue create --title "   " --estimate=-3 --due 2026-02-30
//!     lcl issue create --title t4 --parent nope
//!     lcl issue create --title t8 --parent 1
//!     lcl issue create --title "with text" --description "has text" --due "end of month"
//!     lcl issue create --title "blank due" --due ""
//!     lcl issue create --title good --due 2026-12-24 --estimate 0 --label keep
//!     lcl issue comment 1 c --author " bob "
//!     lcl issue comment 1 c2 --author ""
//!     lcl issue comment 1 c3 --author "   "
//!     lcl issue comment 1 c4 --author carol
//!     lcl doc create --title d1 --parent nope
//!     lcl doc create --title "" --parent d1
//!     D=$(lcl --json doc create --title top | docid)
//!     lcl doc create --title upper --parent "$(printf %s "$D" | tr a-f A-F)"
//!     lcl doc create --title child --parent "$D"
//!     lcl cycle create " " --start soon --end 2026-13-01
//!     lcl cycle create "Sprint 1" --start soon --end 2026-11-14
//!     lcl cycle create c2 --start "" --end 2026-12-31
//!     lcl cycle create "Sprint 2" --start 2026-11-01 --end 2026-11-14
//!
//! and then .lcl/data.sqlite was copied before anything else opened it. No
//! -wal or -shm file was left behind. The fixture is at user_version 0 in WAL
//! mode (sha256 4c71a50f...). Tests copy it with `Env::install_db`; never
//! open it in place. The variants below are made with SQL on that copy,
//! before lcl first opens it.

mod common;

use common::{exec, foreign_key_violations, fts_integrity, user_version, Env, FTS_TABLES, V010_DIRTY_FIXTURE, V0_FIXTURE, V0_LABELS_FIXTURE, V0_PARENTS_FIXTURE, V7_FIXTURE, V8_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use serde_json::Value;
use std::path::Path;

/// When the fixture was made: every row's created_at and updated_at.
const T: i64 = 1794532403;

const TOP: &str = "c431322e-7403-4022-a7cf-1139dbb49b5e";
const CHILD: &str = "e1bb1a1a-58f1-4512-8f1b-51060c900541";
const UNTITLED: &str = "b22accd5-f92e-4f8d-938a-4e8ee5ae1d96";

/// The line the fixture's upgrade prints, with `docs` in place of its two
/// doc-parent phrases and `cycles` in place of its two cycle phrases.
fn line(cycles: &str, docs: &str) -> String {
    format!(
        "lcl: upgraded workspace \"prob\" to schema 10: named 1 blank label, reset 4 label colours, moved 3 unreadable due dates into descriptions, cleared 1 blank due date, {cycles}, cleared 1 negative estimate, {docs}, cleaned 3 comment authors\n"
    )
}

const CYCLES: &str = "renamed 2 cycles with unreadable dates, cleared 1 blank cycle date";
const DOCS: &str = "cleared 2 doc parents, lowercased 1 doc parent";

/// A copy of the dirty fixture, with `sql` run on it before lcl opens it.
fn dirty(sql: &str) -> Env {
    let env = Env::new();
    let db = env.install_db(V010_DIRTY_FIXTURE);
    assert_eq!(user_version(&db), 0);
    if !sql.is_empty() {
        exec(&db, sql);
    }
    env
}

/// The first command on a 0.1.0 copy, which upgrades it. Returns stderr,
/// after checking that stdout is still clean JSON.
fn upgrade(env: &Env) -> String {
    let out = env.lcl(&["--json", "workspace", "show"]);
    out.assert_ok();
    let shown: Value = out.json();
    assert_eq!(shown["schema_version"], SCHEMA_VERSION, "{out:#?}");
    assert_eq!(user_version(&env.db_path()), 10);
    out.stderr
}

/// Every row `sql` selects, each column as SQL text (NULL as None).
fn table(db: &Path, sql: &str, width: usize) -> Vec<Vec<Option<String>>> {
    let conn = rusqlite::Connection::open(db).expect("open db");
    let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    stmt.query_map([], |r| (0..width).map(|i| r.get(i)).collect())
        .and_then(Iterator::collect)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

/// What each issue's title, description, due date, estimate and times are
/// after the upgrade, by number.
fn issues(db: &Path) -> Vec<Vec<Option<String>>> {
    table(db, "SELECT CAST(number AS TEXT), title, description, due_date, CAST(estimate AS TEXT), CAST(created_at AS TEXT), CAST(updated_at AS TEXT) FROM issues ORDER BY number", 7)
}

fn issue(n: &str, title: &str, description: &str, due: Option<&str>, estimate: Option<&str>) -> Vec<Option<String>> {
    vec![s(n), s(title), s(description), due.map(String::from), estimate.map(String::from), s(&T.to_string()), s(&T.to_string())]
}

fn cycles(db: &Path) -> Vec<Vec<Option<String>>> {
    table(db, "SELECT id, name, starts_at, ends_at, CAST(created_at AS TEXT) FROM cycles ORDER BY id", 5)
}

fn cycle(id: &str, name: &str, starts: Option<&str>, ends: Option<&str>) -> Vec<Option<String>> {
    vec![s(id), s(name), starts.map(String::from), ends.map(String::from), s(&T.to_string())]
}

fn docs(db: &Path) -> Vec<Vec<Option<String>>> {
    table(db, "SELECT id, title, parent_id, CAST(created_at AS TEXT), CAST(updated_at AS TEXT) FROM docs ORDER BY id", 5)
}

fn doc(id: &str, title: &str, parent: Option<&str>) -> Vec<Option<String>> {
    vec![s(id), s(title), parent.map(String::from), s(&T.to_string()), s(&T.to_string())]
}

#[test]
fn the_upgrade_repairs_what_0_1_0_accepted_and_says_so_once() {
    let env = dirty("");
    let db = env.db_path();
    assert_eq!(upgrade(&env), line(CYCLES, DOCS));

    // M1, M2: the blank label is named, and every colour that isn't #rgb or
    // #rrggbb is reset, " #fff" included. (V5 trimmed " bug ".)
    assert_eq!(
        table(&db, "SELECT id, name, color FROM labels ORDER BY id", 3),
        [
            [s("1fcf7d83-73cb-435f-8802-12e1bcba019b"), s("keep"), s("#00aa11")],
            [s("212e23d1-b65c-451e-be7c-19933b97b058"), s("bug"), s("#888888")],
            [s("38ef3607-ec2b-4787-8825-3273b47ae109"), s("unnamed"), s("#888888")],
            [s("405a2096-d7ea-42c0-92b0-9e5a3eb246a3"), s("x"), s("#888888")],
            [s("590e4e13-7a77-416c-9575-c3ccb525462d"), s("padded"), s("#888888")],
            [s("e39e5ec7-109a-4437-a835-0c99d510a26a"), s("nocolor"), s("#888888")],
        ]
    );
    // M3, M4: unreadable due dates move into the description (after a blank
    // line when it has text), 2026-02-30 included; the blank one is just
    // cleared; the negative estimate is cleared. Nothing valid changes, and
    // no updated_at moves.
    assert_eq!(
        issues(&db),
        [
            issue("1", "t1", "Due (from 0.1.0): next week", None, None),
            issue("2", "", "", None, None),
            issue("3", "   ", "Due (from 0.1.0): 2026-02-30", None, None),
            issue("4", "t4", "", None, None),
            issue("5", "t8", "", None, None),
            issue("6", "with text", "has text\n\nDue (from 0.1.0): end of month", None, None),
            issue("7", "blank due", "", None, None),
            issue("8", "good", "", Some("2026-12-24"), Some("0")),
        ]
    );
    // M3: a cycle's unreadable dates go on the end of its name, start first;
    // c2's blank start is just cleared, and its name kept.
    assert_eq!(
        cycles(&db),
        [
            cycle("2934fb1c-735c-4123-bd9e-8922c55c372f", "Sprint 2", Some("2026-11-01"), Some("2026-11-14")),
            cycle("29ade97e-eeac-4de5-9f98-656f5a5628c2", "  (start: soon) (end: 2026-13-01)", None, None),
            cycle("a15539d1-b743-428a-a9df-2cc151a2bd49", "c2", None, Some("2026-12-31")),
            cycle("a88d68db-06a3-41d9-a1cb-794104df41da", "Sprint 1 (start: soon)", None, Some("2026-11-14")),
        ]
    );
    // M5: "nope" and "d1" name no doc and are cleared; upper's parent, top's
    // id in upper case, is lowercased; child's is already right.
    assert_eq!(
        docs(&db),
        [
            doc("5b76accd-2ef2-4a8c-81a5-60dd33101b94", "d1", None),
            doc(UNTITLED, "", None),
            doc(TOP, "top", None),
            doc("c5366a66-3aab-436a-bd54-aa7934c74348", "upper", Some(TOP)),
            doc(CHILD, "child", Some(TOP)),
        ]
    );
    // M6: authors trimmed, and cleared when blank.
    assert_eq!(
        table(&db, "SELECT body, author FROM comments ORDER BY body", 2),
        [[s("c"), s("bob")], [s("c2"), None], [s("c3"), None], [s("c4"), s("carol")]]
    );
    fts_integrity(&db, &FTS_TABLES);
    assert_eq!(foreign_key_violations(&db), vec![]);
    // The moved text is searchable.
    let found = env.lcl(&["--json", "search", "month"]).assert_ok().json();
    assert_eq!(found["issues"][0]["number"], 6, "{found:#}");

    // Once: the upgrade has run, so the next open says nothing.
    let again = env.lcl(&["--json", "workspace", "show"]);
    again.assert_ok();
    assert_eq!(again.stderr, "");
}

#[test]
fn after_the_upgrade_export_import_export_is_byte_identical() {
    let env = dirty("");
    upgrade(&env);
    let first = env.lcl(&["export"]).assert_ok().stdout.clone();
    assert!(first.contains("\"name\":\"Sprint 1 (start: soon)\""), "{first}");
    let new = Env::new();
    new.lcl(&["init", "prob", "--key", "PROB"]).assert_ok();
    let out = new.lcl_stdin(&["import", "-"], first.as_bytes());
    out.assert_ok();
    assert_eq!(out.stderr, "", "a new workspace has nothing to upgrade");
    assert!(new.lcl(&["export"]).assert_ok().stdout == first, "the upgraded fixture didn't round-trip");
}

/// Every other fixture holds only readable values, so its upgrade prints
/// nothing; nor does a new workspace's first open.
#[test]
fn a_clean_workspace_upgrades_without_a_word() {
    for fixture in [V0_FIXTURE, V0_LABELS_FIXTURE, V0_PARENTS_FIXTURE, V7_FIXTURE, V8_FIXTURE] {
        let env = Env::new();
        let db = env.install_db(fixture);
        assert!(user_version(&db) < 10, "{fixture}");
        let out = env.lcl(&["--json", "issue", "list"]);
        out.assert_ok();
        assert_eq!((user_version(&db), out.stderr.as_str()), (10, ""), "{fixture}");
        out.json();
    }
    let env = Env::new();
    let out = env.lcl(&["init", "fresh", "--key", "FRES"]);
    out.assert_ok();
    assert_eq!(out.stderr, "");
    assert_eq!(env.lcl(&["--json", "issue", "list"]).assert_ok().stderr, "");
}

/// A blank date (empty, or only whitespace, Unicode's included) said
/// nothing: it is cleared, and never becomes a "Due (from 0.1.0):" line or a
/// " (start: …)" on a cycle's name. A cycle with a blank start and an
/// unreadable end gets only the end.
#[test]
fn a_blank_date_is_just_cleared() {
    let env = dirty(
        "UPDATE issues SET due_date = ' \t' WHERE number = 7;
         UPDATE issues SET due_date = '', description = 'kept' WHERE number = 2;
         UPDATE cycles SET starts_at = '\u{3000}', ends_at = 'later' WHERE name = 'c2';",
    );
    let db = env.db_path();
    assert_eq!(
        upgrade(&env),
        line("renamed 3 cycles with unreadable dates, cleared 1 blank cycle date", DOCS).replace("cleared 1 blank due date", "cleared 2 blank due dates")
    );
    let rows = issues(&db);
    assert_eq!((&rows[1], &rows[6]), (&issue("2", "", "kept", None, None), &issue("7", "blank due", "", None, None)));
    assert_eq!(cycles(&db)[2], cycle("a15539d1-b743-428a-a9df-2cc151a2bd49", "c2 (end: later)", None, None));
}

/// A doc-parent loop, which only a direct writer can make: every link on it
/// is cleared, as V4 does for issues, counted with the parents that name no
/// doc. A link given in upper case counts once, as cleared. A doc below the
/// loop keeps its parent.
#[test]
fn every_link_on_a_doc_parent_loop_is_cleared() {
    let env = dirty(&format!(
        "UPDATE docs SET parent_id = '{child}' WHERE id = '{TOP}';
         UPDATE docs SET parent_id = '{UNTITLED}' WHERE id = '{UNTITLED}';",
        child = CHILD.to_uppercase()
    ));
    let db = env.db_path();
    assert_eq!(upgrade(&env), line(CYCLES, "cleared 4 doc parents, lowercased 1 doc parent"));
    assert_eq!(
        docs(&db),
        [
            doc("5b76accd-2ef2-4a8c-81a5-60dd33101b94", "d1", None),
            doc(UNTITLED, "", None),
            doc(TOP, "top", None),
            doc("c5366a66-3aab-436a-bd54-aa7934c74348", "upper", Some(TOP)),
            doc(CHILD, "child", None),
        ]
    );
}

/// The blank label takes the first of "unnamed", "unnamed-2", … that no
/// label has, ignoring case.
#[test]
fn a_blank_label_takes_the_first_free_unnamed_name() {
    let env = dirty(
        "INSERT INTO labels(id, name, color) VALUES ('00000000-0000-4000-8000-000000000001', 'Unnamed', '#123'),
                                                    ('00000000-0000-4000-8000-000000000002', 'UNNAMED-2', '#123');",
    );
    assert_eq!(upgrade(&env), line(CYCLES, DOCS));
    assert_eq!(
        table(&env.db_path(), "SELECT name FROM labels WHERE id = '38ef3607-ec2b-4787-8825-3273b47ae109'", 1),
        [[s("unnamed-3")]]
    );
}

/// `init` in a directory whose workspace file is from 0.1.0 opens, and so
/// upgrades, that file: it says what the upgrade changed, once.
#[test]
fn init_on_a_0_1_0_file_says_what_the_upgrade_changed() {
    let env = dirty("");
    let out = env.lcl(&["init", "prob", "--key", "PROB"]);
    out.assert_ok();
    assert_eq!(out.stderr, line(CYCLES, DOCS), "{out:#?}");
    assert_eq!(user_version(&env.db_path()), 10);
    let again = env.lcl(&["--json", "workspace", "show"]);
    again.assert_ok();
    assert_eq!(again.stderr, "");
}

/// A comment author is trimmed of any whitespace, a tab and Unicode's
/// (U+3000 here) as well as spaces.
#[test]
fn a_comment_author_is_trimmed_of_any_whitespace() {
    let env = dirty(
        "UPDATE comments SET author = '\tcarol' WHERE body = 'c4';
         UPDATE comments SET author = '\u{3000}bob\u{3000}' WHERE body = 'c';",
    );
    assert_eq!(upgrade(&env), line(CYCLES, DOCS).replace("cleaned 3 comment authors", "cleaned 4 comment authors"));
    assert_eq!(
        table(&env.db_path(), "SELECT body, author FROM comments ORDER BY body", 2),
        [[s("c"), s("bob")], [s("c2"), None], [s("c3"), None], [s("c4"), s("carol")]]
    );
}
