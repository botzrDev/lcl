//! Search (prove-out blocker 6, C4–C10). The query is split on whitespace;
//! every word must match, and each is literal text, never FTS5 syntax, so no
//! query reaches the user as an fts5 error. A word ending in `*` is a prefix.
//! Comments are indexed: an issue matches when every word is in its title
//! and description together, or every word is in one of its comments. A
//! query that is exactly this workspace's `KEY-N` or `#N` lists that issue
//! first. Each issue is listed once, with where it matched.

mod common;

use common::{exec, fts_integrity, user_version, Env, FTS_TABLES, V0_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use serde_json::{json, Value};
use std::time::Instant;

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    env
}

/// `issue create` with a title and description; returns the number.
fn issue(env: &Env, title: &str, description: &str) -> i64 {
    let out = env.lcl(&["issue", "create", "--title", title, "--description", description, "--json"]);
    out.assert_ok().json()["number"].as_i64().expect("number")
}

fn comment(env: &Env, n: i64, body: &str) {
    env.lcl(&["issue", "comment", &n.to_string(), body]).assert_ok();
}

fn search(env: &Env, args: &[&str]) -> Value {
    let mut cmd = vec!["search", "--json"];
    cmd.extend(args);
    env.lcl(&cmd).assert_ok().json()
}

/// (identifier, matched_in) of each issue found, in order.
fn hits(found: &Value) -> Vec<(String, Value)> {
    found["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| (i["identifier"].as_str().expect("identifier").to_string(), i["matched_in"].clone()))
        .collect()
}

fn found_ids(found: &Value) -> Vec<String> {
    hits(found).into_iter().map(|(id, _)| id).collect()
}

/// Every query the prove-out saw fail now searches, as literal text.
#[test]
fn prove_out_queries_never_error() {
    let env = workspace();
    issue(&env, "Release BZR-1532 follow-up", "Ship v1.3.1; touches writer_lock.rs and skill_action::rust.");
    issue(&env, "Words", r#"AND OR NEAR NOT say "hi" (x, x) a:b ^x +x -x xylophone"#);

    for (q, want) in [
        ("BZR-1532", vec!["DREA-1"]),
        ("v1.3.1", vec!["DREA-1"]),
        ("writer_lock.rs", vec!["DREA-1"]),
        ("skill_action::rust", vec!["DREA-1"]),
        (r#"say "hi"#, vec!["DREA-2"]),
        ("OR", vec!["DREA-2"]),
        ("NEAR", vec!["DREA-2"]),
        ("NOT", vec!["DREA-2"]),
        ("(x, x)", vec!["DREA-2"]),
        ("a:b", vec!["DREA-2"]),
        ("^x", vec!["DREA-2"]),
        ("+x", vec!["DREA-2"]),
        ("-x", vec!["DREA-2"]),
        ("x*", vec!["DREA-2"]),
        ("xylophone -- *", vec!["DREA-2"]),
        ("NEAR(x y)", vec![]),
        ("BZR-9999", vec![]),
    ] {
        // After --, so that -x is a word too.
        let out = env.lcl(&["search", "--json", "--", q]);
        assert_eq!(out.code, 0, "{q:?}: {out:#?}");
        assert_eq!(out.stderr, "", "{q:?}");
        let found = out.json();
        assert_eq!(found_ids(&found), want, "{q:?}");
        assert_eq!(found["query"], q);
        let human = env.lcl(&["search", "--", q]);
        assert_eq!(human.code, 0, "{q:?}: {human:#?}");
    }
    // "AND" is a word here, not an operator ("and" is also in DREA-1).
    let mut and = found_ids(&search(&env, &["AND"]));
    and.sort();
    assert_eq!(and, ["DREA-1", "DREA-2"]);

    // No words at all: lcl's own message, exit 2, never an fts5 error.
    for args in [vec!["search", "\""], vec!["search", "*"], vec!["search", "--", "--"], vec!["search", "--", "-- * \"\""]] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains("has no words to search for"), "{args:?}: {out:#?}");
    }
}

/// The words needn't be quoted, and flags after them are still flags.
#[test]
fn unquoted_words_and_trailing_flags() {
    let env = workspace();
    issue(&env, "writer lock", "stage_and_commit eddbfe9");
    issue(&env, "writer two", "");

    let found = search(&env, &["stage_and_commit", "eddbfe9"]);
    assert_eq!((&found["query"], found_ids(&found)), (&json!("stage_and_commit eddbfe9"), vec!["DREA-1".to_string()]));
    assert_eq!(env.lcl(&["search", "stage_and_commit", "eddbfe9"]).assert_ok().stdout, "DREA-1\t[Backlog]\twriter lock\tdescription\n");
    // Every word counts, not just the first.
    assert_eq!(found_ids(&search(&env, &["writer", "two"])), ["DREA-2"]);

    // Flags after the words are parsed as flags.
    let out = env.lcl(&["search", "writer", "--json"]).assert_ok().json();
    assert_eq!((&out["query"], &out["issue_total"]), (&json!("writer"), &json!(2)));
    let human = env.lcl(&["search", "writer", "--limit", "1"]).assert_ok().stdout.clone();
    let lines: Vec<&str> = human.lines().collect();
    assert!(lines.len() == 2 && lines[0].starts_with("DREA-") && lines[1] == "showing 1 of 2 issues", "{human}");
}

/// A word that starts with - goes after --. Anywhere else it is a flag, so a
/// made-up flag exits 2 instead of being searched for.
#[test]
fn a_word_starting_with_a_dash_goes_after_double_dash() {
    let env = workspace();
    issue(&env, "status Done", "pass -x or --no-commit");

    // Before --: exit 2 with clap's tip, and nothing is searched.
    for args in [vec!["search", "--status", "Done", "x"], vec!["search", "-x"], vec!["search", "status", "-x"]] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.contains("unexpected argument '-") && out.stderr.contains("tip: to pass '-"), "{args:?}: {out:#?}");
        assert_eq!(out.stdout, "", "{args:?}");
    }
    // After --, it is a word.
    for q in ["-x", "--no-commit"] {
        let found = search(&env, &["--", q]);
        assert_eq!((&found["query"], found_ids(&found)), (&json!(q), vec!["DREA-1".to_string()]), "{q}");
    }
    let out = env.lcl(&["search", "--", "--"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(out.stderr.contains("has no words to search for") && out.stderr.contains("Usage: lcl search"), "{out:#?}");

    let help = env.lcl(&["search", "--help"]).assert_ok().stdout.clone();
    assert!(help.contains("Words that start with - go after --, e.g. lcl search -- --no-commit."), "{help}");
}

#[test]
fn an_empty_query_exits_2() {
    let env = workspace();
    issue(&env, "anything", "");
    for q in ["", "   ", "\t\n"] {
        let out = env.lcl(&["search", q]);
        assert_eq!(out.code, 2, "{q:?}: {out:#?}");
        assert!(out.stderr.contains("the search query is empty"), "{q:?}: {out:#?}");
        assert!(!out.stderr.contains("fts5"), "{q:?}: {out:#?}");
    }
}

#[test]
fn a_word_only_in_a_comment_finds_the_issue() {
    let env = workspace();
    issue(&env, "decoy", "nothing here");
    let n = issue(&env, "release record", "see comments");
    comment(&env, n, "Verified on staging; zanzibar build is green.");

    let found = search(&env, &["zanzibar"]);
    assert_eq!(hits(&found), [("DREA-2".to_string(), json!(["comments"]))]);
    assert_eq!(env.lcl(&["search", "zanzibar"]).assert_ok().stdout, "DREA-2\t[Backlog]\trelease record\tcomments\n");
}

#[test]
fn deleting_an_issue_removes_its_comment_hits() {
    let env = workspace();
    let db = env.db_path();
    let a = issue(&env, "first", "");
    let b = issue(&env, "second", "");
    comment(&env, b, "another quokka");
    comment(&env, b, "unrelated");
    // DREA-1's comment takes the highest rowid, so the next comment reuses
    // it once it is deleted: a stale index entry would then point at that
    // new comment.
    comment(&env, a, "quokka sighting");
    assert_eq!(found_ids(&search(&env, &["quokka"])), ["DREA-2", "DREA-1"]);

    env.lcl(&["issue", "delete", "1"]).assert_ok();
    assert_eq!(found_ids(&search(&env, &["quokka"])), ["DREA-2"]);
    fts_integrity(&db, &["comments_fts"]);
    comment(&env, b, "a fresh note");
    let reused: i64 = rusqlite::Connection::open(&db)
        .and_then(|c| c.query_row("SELECT rowid FROM comments WHERE body = 'a fresh note'", [], |r| r.get(0)))
        .expect("rowid");
    assert_eq!(reused, 3, "the deleted comment's rowid should be reused");
    assert_eq!(found_ids(&search(&env, &["sighting"])), Vec::<String>::new(), "a deleted comment's words still match");
    assert_eq!(found_ids(&search(&env, &["fresh"])), ["DREA-2"]);
    fts_integrity(&db, &["comments_fts"]);

    // A comment edited by hand is reindexed (the update trigger).
    exec(&db, "UPDATE comments SET body = 'wombat now' WHERE body = 'another quokka';");
    assert_eq!(found_ids(&search(&env, &["quokka"])), Vec::<String>::new());
    assert_eq!(found_ids(&search(&env, &["wombat"])), ["DREA-2"]);
    fts_integrity(&db, &FTS_TABLES);
}

#[test]
fn an_identifier_query_lists_that_issue_first_once() {
    let env = workspace();
    let target = issue(&env, "the target", "");
    issue(&env, "mentions DREA-1 in its title", "");
    let c = issue(&env, "mentions it in a comment", "");
    comment(&env, c, "see DREA-1");
    comment(&env, target, "DREA-1 talks about itself");
    issue(&env, "unrelated", "");

    for q in ["DREA-1", "drea-1", "#1"] {
        let found = search(&env, &[q]);
        let h = hits(&found);
        assert_eq!(h[0], ("DREA-1".to_string(), json!(["identifier", "comments"])), "{q}: {found:#}");
        let mut ids = found_ids(&found);
        ids.sort();
        assert_eq!(ids, ["DREA-1", "DREA-2", "DREA-3"], "{q}: each issue once");
        assert_eq!(found["issue_total"], 3, "{q}");
    }
    // Another workspace's key, a missing issue, or a bare number is just
    // text: no error, and nothing matched as an identifier.
    let by_identifier = |found: &Value| hits(found).into_iter().filter(|(_, m)| m.as_array().expect("matched_in").contains(&json!("identifier"))).count();
    for (q, want) in [("BZR-1", vec![]), ("DREA-99", vec![]), ("1", vec!["DREA-1", "DREA-2", "DREA-3"])] {
        let out = env.lcl(&["search", q, "--json"]);
        assert_eq!(out.code, 0, "{q}: {out:#?}");
        let found = out.json();
        assert_eq!(by_identifier(&found), 0, "{q}: {found:#}");
        let mut ids = found_ids(&found);
        ids.sort();
        assert_eq!(ids, want, "{q}");
    }
}

#[test]
fn a_trailing_star_is_a_prefix() {
    let env = workspace();
    issue(&env, "writer lock", "");
    issue(&env, "unwritten", "");
    assert_eq!(found_ids(&search(&env, &["writ*"])), ["DREA-1"]);
    assert_eq!(found_ids(&search(&env, &["writ"])), Vec::<String>::new());
}

#[test]
fn diacritics_are_ignored() {
    let env = workspace();
    issue(&env, "Meet at the café", "");
    issue(&env, "cafe menu", "");
    let mut ids = found_ids(&search(&env, &["cafe"]));
    ids.sort();
    assert_eq!(ids, ["DREA-1", "DREA-2"]);
    let mut ids = found_ids(&search(&env, &["CAFÉ"]));
    ids.sort();
    assert_eq!(ids, ["DREA-1", "DREA-2"]);
}

/// Words may be spread over an issue's title and description, but not
/// between the issue and a comment, or across comments.
#[test]
fn words_must_match_in_the_issue_or_in_one_comment() {
    let env = workspace();
    issue(&env, "alpha", "beta");
    let b = issue(&env, "second", "gamma");
    comment(&env, b, "delta");
    let c = issue(&env, "third", "");
    comment(&env, c, "epsilon");
    comment(&env, c, "zeta");
    let d = issue(&env, "fourth", "");
    comment(&env, d, "eta and theta");

    assert_eq!(hits(&search(&env, &["alpha beta"])), [("DREA-1".to_string(), json!(["title", "description"]))]);
    assert_eq!(found_ids(&search(&env, &["gamma delta"])), Vec::<String>::new());
    assert_eq!(found_ids(&search(&env, &["epsilon zeta"])), Vec::<String>::new());
    assert_eq!(hits(&search(&env, &["eta theta"])), [("DREA-4".to_string(), json!(["comments"]))]);
    // Both words in DREA-2's own text (title and description) do match.
    assert_eq!(hits(&search(&env, &["second gamma"])), [("DREA-2".to_string(), json!(["title", "description"]))]);

    let help = env.lcl(&["search", "--help"]).assert_ok().stdout.clone();
    for rule in [
        "An issue matches when every word is in its title and description together, or every word is in one of its comments.",
        "Words split between the issue and a comment, or across two comments, don't match.",
    ] {
        assert!(help.contains(rule), "{help}");
    }
}

/// An issue that matches by its own text and by comments is listed once,
/// counted once, and says both.
#[test]
fn an_issue_matched_twice_is_listed_once() {
    let env = workspace();
    let n = issue(&env, "omega release", "notes");
    comment(&env, n, "omega shipped");
    comment(&env, n, "omega again");
    issue(&env, "other", "omega in the description");

    let found = search(&env, &["omega"]);
    let mut h = hits(&found);
    h.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(h, [("DREA-1".to_string(), json!(["title", "comments"])), ("DREA-2".to_string(), json!(["description"]))]);
    assert_eq!(found["issue_total"], 2);
}

#[test]
fn limit_applies_to_each_list() {
    let env = workspace();
    for i in 1..=5 {
        issue(&env, &format!("common issue {i}"), "");
    }
    for i in 1..=3 {
        env.lcl(&["doc", "create", "--title", &format!("common doc {i}")]).assert_ok();
    }

    let found = search(&env, &["common", "--limit", "2"]);
    assert_eq!((found["issues"].as_array().map(Vec::len), found["docs"].as_array().map(Vec::len)), (Some(2), Some(2)));
    assert_eq!((&found["issue_total"], &found["doc_total"], &found["limit"], &found["truncated"]), (&json!(5), &json!(3), &json!(2), &json!(true)));
    let human = env.lcl(&["search", "common", "--limit", "2"]).assert_ok().stdout.clone();
    let lines: Vec<&str> = human.lines().collect();
    assert_eq!(lines.len(), 6, "{human}");
    assert!(lines[..2].iter().all(|l| l.starts_with("DREA-") && l.ends_with("\ttitle")), "{human}");
    assert!(lines[2..4].iter().all(|l| l.starts_with("doc\t")), "{human}");
    assert_eq!(lines[4..], ["showing 2 of 5 issues", "showing 2 of 3 docs"], "{human}");

    let found = search(&env, &["common", "--limit", "3"]);
    assert_eq!((&found["truncated"], found["docs"].as_array().map(Vec::len)), (&json!(true), Some(3)));
    let human = env.lcl(&["search", "common", "--limit", "3"]).assert_ok().stdout.clone();
    assert!(human.ends_with("showing 3 of 5 issues\n") && !human.contains("docs\n"), "{human}");

    let found = search(&env, &["common"]);
    assert_eq!((&found["limit"], &found["truncated"], &found["issue_total"]), (&json!(50), &json!(false), &json!(5)));
    assert!(!env.lcl(&["search", "common"]).assert_ok().stdout.contains("showing"));

    assert_eq!(env.lcl(&["search", "nothing-like-this"]).assert_ok().stdout, "no matches\n");
    let out = env.lcl(&["search", "common", "--limit", "0"]);
    assert_eq!(out.code, 2, "{out:#?}");
}

#[test]
fn upgrade_indexes_existing_comments() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    assert_eq!(user_version(&db), 0);
    let conn = rusqlite::Connection::open(&db).expect("open db");
    let (n, body): (i64, String) = conn
        .query_row("SELECT i.number, c.body FROM comments c JOIN issues i ON i.id = c.issue_id", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("the fixture's comment");
    drop(conn);
    assert_eq!(body, "first comment");

    let found = search(&env, &["first comment"]);
    assert_eq!(user_version(&db), SCHEMA_VERSION);
    let hit = hits(&found).into_iter().find(|(id, _)| id.ends_with(&format!("-{n}")) || id == &format!("#{n}"));
    let (_, matched_in) = hit.unwrap_or_else(|| panic!("issue {n} not found: {found:#}"));
    assert!(matched_in.as_array().expect("matched_in").contains(&json!("comments")), "{found:#}");
    fts_integrity(&db, &FTS_TABLES);
}

/// 1,700 issues with 3 comments each, seeded with SQL, and a word in all of
/// them. Prints the time; release-build timings go in the step report.
#[test]
fn a_word_in_every_issue_and_comment() {
    let env = workspace();
    let db = env.db_path();
    exec(
        &db,
        "BEGIN;
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 1700)
         INSERT INTO issues(id, number, title, description, status, priority, created_at, updated_at)
         SELECT printf('00000000-0000-4000-8000-%012d', x), x, 'Seeded issue ' || x,
                'A common thread runs through issue ' || x || '.', 'Backlog', 0, 1, 1 FROM n;
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 5100)
         INSERT INTO comments(id, issue_id, body, author, created_at)
         SELECT printf('00000000-0000-4000-9000-%012d', x), printf('00000000-0000-4000-8000-%012d', (x - 1) / 3 + 1),
                'Comment ' || x || ' on the common thread.', 'seed', 1 FROM n;
         COMMIT;",
    );
    let started = Instant::now();
    let found = search(&env, &["common"]);
    let took = started.elapsed();
    eprintln!("search over 1,700 issues and 5,100 comments took {took:?} (this build)");
    assert_eq!((&found["issue_total"], &found["truncated"]), (&json!(1700), &json!(true)));
    let h = hits(&found);
    assert_eq!(h.len(), 50);
    assert!(h.iter().all(|(_, m)| m == &json!(["description", "comments"])), "{found:#}");
}
