//! The trash: the invariants of `lcl_core::trash`, held by the commands.
//!
//! - One field: delete then restore leaves the workspace, and so `issue show
//!   --json`, exactly as it was.
//! - Frozen: no command reads or changes a trashed issue; each names
//!   `issue restore`.
//! - Out of sight: a trashed issue is in no list, search, count, child
//!   list or relation list, and does not block. `issue list --trashed`
//!   shows the trash and nothing else.
//! - A live issue's parent is live.
//! - Still taken: its number and its ref.
//! - Not exported.
//!
//! And of purging: only from the trash; whole, in one transaction;
//! sub-issues first; the number is spent; only when asked.

mod common;

use common::{Dump, Env, FTS_TABLES, dump, files, fts_integrity};
use serde_json::{Value, json};

fn j(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    env.lcl(&argv).assert_ok().json()
}

/// The identifiers `issue list ARGS` returns, lowest number first.
fn listed(env: &Env, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["issue", "list", "--all"];
    argv.extend(args);
    let mut ids: Vec<(i64, String)> = j(env, &argv)["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| {
            (
                i["number"].as_i64().unwrap(),
                i["identifier"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id).collect()
}

fn found(env: &Env, word: &str) -> Vec<String> {
    let mut ids: Vec<String> = j(env, &["search", word])["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| i["identifier"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// Refused as a conflict, exit 1, naming the restore command for `id`.
#[track_caller]
fn refused_as_trashed(env: &Env, args: &[&str], id: &str) {
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = env.lcl(&argv);
    assert_eq!(out.code, 1, "{args:?}\n{out:#?}");
    assert_eq!(out.stdout, "", "{args:?}");
    assert_eq!(
        out.stderr,
        common::error_text(
            &argv,
            "conflict",
            &format!("{id} is in the trash; restore it with: lcl issue restore {id}")
        ),
        "{args:?}"
    );
}

/// A small deterministic generator, so the property test below needs no
/// dependency and fails the same way every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self, below: u64) -> u64 {
        // Knuth's MMIX constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % below
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.next(one_in) == 0
    }
}

/// A workspace of `n` issues, each with a mix of fields, labels, a cycle, a
/// project and milestone, a ref, a parent, comments and relations, chosen by
/// `rng`. Whatever an issue can have, some issue here has.
fn varied(rng: &mut Rng, n: i64) -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    for label in ["bug", "ui", "later"] {
        env.lcl(&["label", "create", label]).assert_ok();
    }
    env.lcl(&["project", "create", "P"]).assert_ok();
    env.lcl(&["milestone", "create", "M", "--project", "P"])
        .assert_ok();
    env.lcl(&["cycle", "create", "C1"]).assert_ok();
    env.lcl(&["cycle", "create", "C2"]).assert_ok();
    for i in 1..=n {
        let title = format!("issue {i} zebra");
        let mut args: Vec<String> = ["issue", "create", "--title", &title]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut add = |flag: &str, value: &str| {
            args.push(flag.to_string());
            args.push(value.to_string());
        };
        if rng.chance(2) {
            add("--description", "some words\nand a second line");
        }
        add(
            "--status",
            [
                "backlog",
                "todo",
                "in progress",
                "in review",
                "done",
                "canceled",
            ][rng.next(6) as usize],
        );
        add("--priority", &rng.next(5).to_string());
        if rng.chance(2) {
            add("--assignee", "ann");
        }
        for label in ["bug", "ui", "later"] {
            if rng.chance(3) {
                add("--label", label);
            }
        }
        if rng.chance(3) {
            add("--estimate", &rng.next(9).to_string());
        }
        if rng.chance(3) {
            add("--due", "2026-12-24");
        }
        if rng.chance(2) {
            add("--project", "P");
            if rng.chance(2) {
                add("--milestone", "M");
            }
        }
        if rng.chance(2) {
            add("--cycle", ["C1", "C2"][rng.next(2) as usize]);
        }
        if rng.chance(3) {
            add("--ref", &format!("EXT-{i}"));
        }
        if i > 1 && rng.chance(3) {
            add("--parent", &(1 + rng.next(i as u64 - 1)).to_string());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        env.lcl(&args).assert_ok();
        for c in 0..rng.next(3) {
            env.lcl(&[
                "issue",
                "comment",
                &i.to_string(),
                &format!("comment {c} on {i}"),
            ])
            .assert_ok();
        }
    }
    for _ in 0..n {
        let a = (1 + rng.next(n as u64)).to_string();
        let b = (1 + rng.next(n as u64)).to_string();
        let kind = ["blocks", "related", "duplicate-of"][rng.next(3) as usize];
        // Some are refused (itself, a cycle, a second duplicate-of): fine.
        env.lcl(&["issue", "relate", &a, kind, &b]);
    }
    env
}

/// The property: for any issue, delete then restore leaves the whole
/// workspace as it was, table for table and rowid for rowid (timestamps
/// too: neither command touches `updated_at`), and so `issue show --json`
/// byte for byte. In between, the one thing changed is its `deleted_at`.
///
/// Each workspace is gone through twice: with an empty trash, where an
/// issue with sub-issues can't be deleted, and then with about half of the
/// issues left in the trash, so that the rest are round-tripped beside
/// trashed neighbours, relations to trashed issues and trashed sub-issues.
#[test]
fn delete_then_restore_changes_nothing() {
    let mut parents = 0;
    for seed in [1, 2, 3] {
        let mut rng = Rng(seed);
        let n = 14;
        let env = varied(&mut rng, n);
        let db = env.db_path();
        let (first, _) = round_trip_every_issue(&env, n, seed, &[]);
        assert!(
            first >= n / 2,
            "seed {seed}: only {first} issues could be deleted"
        );

        // Fill the trash from the highest number down: sub-issues have
        // higher numbers than their parents, so parents come free.
        let mut trashed = vec![];
        for i in (1..=n).rev() {
            if rng.chance(2) && env.lcl(&["issue", "delete", &i.to_string()]).code == 0 {
                trashed.push(i);
            }
        }
        assert!(!trashed.is_empty(), "seed {seed}");
        let (second, with_trashed_children) = round_trip_every_issue(&env, n, seed, &trashed);
        assert!(
            second > 0,
            "seed {seed}: nothing round-tripped beside the trash"
        );
        parents += with_trashed_children;
        fts_integrity(&db, &FTS_TABLES);
    }
    assert!(
        parents > 0,
        "no issue with sub-issues was ever round-tripped"
    );
}

/// Deletes and restores each of issues 1..=n that isn't in `trashed`,
/// checking the property each time. Returns how many it could delete, and
/// how many of those had sub-issues (in the trash, as they must be).
fn round_trip_every_issue(env: &Env, n: i64, seed: u64, trashed: &[i64]) -> (i64, i64) {
    let db = env.db_path();
    let (mut round_trips, mut parents) = (0, 0);
    for i in (1..=n).filter(|i| !trashed.contains(i)) {
        let id = i.to_string();
        let shown = env
            .lcl(&["issue", "show", &id, "--json"])
            .assert_ok()
            .stdout
            .clone();
        let before = dump(&db);

        let out = env.lcl(&["issue", "delete", &id]);
        if out.code != 0 {
            // It has live sub-issues: refused, and nothing changed.
            assert!(
                out.stderr.contains("has sub-issues"),
                "seed {seed}: {out:#?}"
            );
            assert!(
                dump(&db) == before,
                "seed {seed}: a refused delete changed W-{i}"
            );
            continue;
        }
        let mut in_trash = dump(&db);
        only_deleted_at_differs(&mut in_trash, &before, i, seed);

        env.lcl(&["issue", "restore", &id]).assert_ok();
        let (mut was, mut now) = (before.clone(), dump(&db));
        let logged = common::take_log(&mut was, &mut now);
        assert!(now == was, "seed {seed}: W-{i} came back changed");
        assert_eq!(
            common::logged_fields(&logged),
            ["deleted_at", "deleted_at"],
            "seed {seed}: W-{i}: the log has its delete and restore, and nothing else"
        );
        assert_eq!(
            env.lcl(&["issue", "show", &id, "--json"])
                .assert_ok()
                .stdout,
            shown,
            "seed {seed}: W-{i}"
        );
        round_trips += 1;
        let sub_issues: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM issues c JOIN issues p ON p.id = c.parent_id WHERE p.number = ?1",
                [i],
                |r| r.get(0),
            )
            .unwrap();
        if sub_issues > 0 {
            parents += 1;
        }
    }
    (round_trips, parents)
}

/// Checks that `trashed` is `before` with issue `number`'s `deleted_at` set
/// and nothing else changed, and that the change log has that, and only it.
fn only_deleted_at_differs(trashed: &mut Dump, before: &Dump, number: i64, seed: u64) {
    use rusqlite::types::Value as Sql;
    let mut before = before.clone();
    let logged = common::take_log(&mut before, trashed);
    assert_eq!(
        common::logged_fields(&logged),
        ["deleted_at"],
        "seed {seed}: W-{number}: {logged:?}"
    );
    assert_eq!(logged[0].2.as_deref(), Some("null"), "seed {seed}");
    let rows = trashed.tables.get_mut("issues").expect("issues");
    let mut set = 0;
    for (row, was) in rows.iter_mut().zip(&before.tables["issues"]) {
        if row["number"] == Sql::Integer(number) {
            assert!(
                matches!(row["deleted_at"], Sql::Integer(_)),
                "seed {seed}: {row:?}"
            );
            row.insert("deleted_at".into(), was["deleted_at"].clone());
            set += 1;
        }
    }
    assert_eq!(set, 1);
    assert!(
        *trashed == before,
        "seed {seed}: deleting W-{number} changed more than deleted_at"
    );
}

/// One workspace for the tests below.
///
/// - W-1 "alpha", in project P with milestone M, in cycle C, label bug, ref
///   EXT-1, with a comment. W-2 is under it.
/// - W-3 "gamma" blocks W-4 "delta", and is related to W-1.
/// - W-5 is a duplicate of W-1.
fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.lcl(&["label", "create", "bug"]).assert_ok();
    env.lcl(&["project", "create", "P"]).assert_ok();
    env.lcl(&["milestone", "create", "M", "--project", "P"])
        .assert_ok();
    env.lcl(&["cycle", "create", "C"]).assert_ok();
    env.lcl(&[
        "issue",
        "create",
        "--title",
        "alpha",
        "--description",
        "aardvark",
        "--project",
        "P",
        "--milestone",
        "M",
        "--cycle",
        "C",
        "--label",
        "bug",
        "--ref",
        "EXT-1",
    ])
    .assert_ok();
    env.lcl(&["issue", "comment", "1", "a remark about badgers"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "beta", "--parent", "1"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "gamma"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "delta"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "epsilon"])
        .assert_ok();
    env.lcl(&["issue", "relate", "3", "blocks", "4"])
        .assert_ok();
    env.lcl(&["issue", "relate", "3", "related", "1"])
        .assert_ok();
    env.lcl(&["issue", "relate", "5", "duplicate-of", "1"])
        .assert_ok();
    env
}

#[test]
fn a_trashed_issue_is_out_of_sight_until_restored() {
    let env = workspace();
    // What every view shows before, to compare with after the restore.
    let views: &[&[&str]] = &[
        &["issue", "list", "--all"],
        &["issue", "list", "--blocked"],
        &["issue", "list", "--unblocked"],
        &["issue", "list", "--parent", "1"],
        &["issue", "list", "--project", "P"],
        &["issue", "list", "--cycle", "C"],
        &["issue", "list", "--label", "bug"],
        &["search", "alpha"],
        &["search", "badgers"],
        &["search", "EXT-1"],
        &["search", "W-1"],
        &["search", "beta"],
        &["project", "show", "P"],
        &["milestone", "show", "M"],
        &["cycle", "list"],
        &["issue", "show", "1"],
        &["issue", "show", "4"],
        &["issue", "show", "5"],
    ];
    let before: Vec<Value> = views.iter().map(|v| j(&env, v)).collect();
    assert_eq!(listed(&env, &["--blocked"]), ["W-4"]);

    // W-2, the sub-issue, then W-1 and W-3.
    for n in ["2", "1", "3"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }

    assert_eq!(listed(&env, &[]), ["W-4", "W-5"]);
    assert_eq!(j(&env, &["issue", "list"])["total"], 2);
    for args in [
        &["--project", "P"][..],
        &["--cycle", "C"],
        &["--label", "bug"],
        &["--milestone", "M"],
    ] {
        assert_eq!(listed(&env, args), Vec::<String>::new(), "{args:?}");
    }
    // Search: by title, description, comment, ref and identifier.
    for word in [
        "alpha", "aardvark", "badgers", "EXT-1", "W-1", "#1", "beta", "gamma",
    ] {
        let hit = j(&env, &["search", word]);
        assert_eq!(found(&env, word), Vec::<String>::new(), "{word}");
        assert_eq!(hit["issue_total"], 0, "{word}");
    }
    // Counts.
    let project = j(&env, &["project", "show", "P"]);
    assert_eq!(project["issues"]["total"], 0);
    assert_eq!(project["issues"]["by_status"]["Backlog"], 0);
    assert_eq!(project["milestones"][0]["issues"], 0);
    assert_eq!(j(&env, &["milestone", "show", "M"])["issues"]["total"], 0);
    assert_eq!(j(&env, &["cycle", "list"])[0]["issues"], 0);
    // A trashed blocker does not block.
    assert_eq!(listed(&env, &["--blocked"]), Vec::<String>::new());
    assert_eq!(listed(&env, &["--unblocked"]), ["W-4", "W-5"]);
    // Relations to trashed issues are hidden from the other issue.
    let empty = json!({"blocks": [], "blocked_by": [], "related": [], "duplicate_of": null, "duplicates": []});
    assert_eq!(j(&env, &["issue", "show", "4"])["relations"], empty);
    assert_eq!(j(&env, &["issue", "show", "5"])["relations"], empty);

    // The trash is listed, and only the trash, with when each went in.
    assert_eq!(listed(&env, &["--trashed"]), ["W-1", "W-2", "W-3"]);
    let trash = j(&env, &["issue", "list", "--trashed"]);
    assert_eq!(trash["total"], 3);
    assert!(
        trash["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["deleted_at"].is_i64())
    );
    assert_eq!(listed(&env, &["--trashed", "--project", "P"]), ["W-1"]);
    assert!(
        j(&env, &["issue", "list", "--all"])["issues"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["deleted_at"].is_null())
    );
    assert_eq!(
        env.lcl(&["issue", "list", "--trashed"]).assert_ok().stdout,
        "W-3\t[Backlog]\tNone\tgamma\nW-2\t[Backlog]\tNone\tbeta\nW-1\t[Backlog]\tNone\talpha\n"
    );

    // Restored (the parent before its sub-issue), every view is as before.
    for n in ["3", "1", "2"] {
        env.lcl(&["issue", "restore", n]).assert_ok();
    }
    for (view, was) in views.iter().zip(&before) {
        assert_eq!(j(&env, view), *was, "{view:?}");
    }
    assert_eq!(listed(&env, &["--trashed"]), Vec::<String>::new());
}

#[test]
fn a_trashed_issue_is_frozen_and_every_command_names_restore() {
    let env = workspace();
    let comment = j(&env, &["issue", "comments", "3"]);
    assert_eq!(comment, json!([]));
    let on_three = j(&env, &["issue", "comment", "3", "a note"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    env.lcl(&["issue", "delete", "3"]).assert_ok();
    let before = files(&env);

    let refusals: &[&[&str]] = &[
        &["issue", "show", "3"],
        &["issue", "show", "W-3"],
        &["issue", "update", "3", "--title", "changed"],
        &["issue", "update", "3", "--status", "done"],
        &["issue", "delete", "3"],
        &["issue", "comment", "3", "more"],
        &["issue", "comments", "3"],
        &["issue", "relate", "3", "blocks", "5"],
        &["issue", "relate", "5", "blocked-by", "3"],
        &["issue", "unrelate", "3", "blocks", "4"],
        &["issue", "create", "--title", "under it", "--parent", "3"],
        &["issue", "update", "4", "--parent", "3"],
        &["issue", "list", "--parent", "3"],
        &["comment", "edit", &on_three, "rewritten"],
        &["comment", "delete", &on_three],
    ];
    for args in refusals {
        refused_as_trashed(&env, args, "W-3");
    }
    // Without --json the same, as text.
    let out = env.lcl(&["issue", "show", "3"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            1,
            "",
            "Error: W-3 is in the trash; restore it with: lcl issue restore W-3\n"
        )
    );
    assert_eq!(
        files(&env).get(&env.db_path()),
        before.get(&env.db_path()),
        "a refused command wrote to the workspace"
    );
    assert!(
        dump(&env.db_path()) == {
            env.lcl(&["issue", "list"]).assert_ok();
            dump(&env.db_path())
        }
    );

    // Restore itself: only for an issue that is in the trash and exists.
    for (args, kind, message) in [
        (
            &["issue", "restore", "4"][..],
            "conflict",
            "W-4 is not in the trash",
        ),
        (
            &["issue", "restore", "99"],
            "not_found",
            "issue W-99 not found",
        ),
    ] {
        let argv = [args, &["--json"]].concat();
        let out = env.lcl(&argv);
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (1, "", common::error_text(&argv, kind, message).as_str()),
            "{args:?}"
        );
    }
    assert_eq!(
        env.lcl(&["issue", "restore", "3"]).assert_ok().stdout,
        "Restored W-3\n"
    );
    assert_eq!(j(&env, &["issue", "comments", "3"])[0]["body"], "a note");
}

#[test]
fn a_live_issue_s_parent_is_live() {
    let env = workspace();
    let db = env.db_path();
    // Delete: refused while a sub-issue is live.
    let before = dump(&db);
    let out = env.lcl(&["issue", "delete", "1"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert_eq!(
        out.stderr,
        "Error: W-1 has sub-issues W-2; reparent or delete them first\n"
    );
    assert!(dump(&db) == before);
    // With the sub-issue in the trash, the parent can follow it.
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();

    // Restore: refused while the parent is in the trash, naming it.
    let before = dump(&db);
    let argv = ["issue", "restore", "2", "--json"];
    let out = env.lcl(&argv);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            1,
            "",
            common::error_text(
                &argv,
                "conflict",
                "W-2 can't be restored while its parent W-1 is in the trash; restore W-1 first"
            )
            .as_str()
        )
    );
    assert!(dump(&db) == before);
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    // The parent alone is back; its sub-issue is still in the trash.
    assert_eq!(j(&env, &["issue", "show", "1"])["children"], json!([]));
    env.lcl(&["issue", "restore", "2"]).assert_ok();
    assert_eq!(j(&env, &["issue", "show", "1"])["children"], json!(["W-2"]));
    assert_eq!(
        j(&env, &["issue", "show", "2"])["issue"]["parent_identifier"],
        "W-1"
    );

    // At no point was a live issue under a trashed one.
    let orphans: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM issues c JOIN issues p ON p.id = c.parent_id WHERE c.deleted_at IS NULL AND p.deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
}

#[test]
fn a_trashed_issue_keeps_its_number_and_its_ref() {
    let env = workspace();
    env.lcl(&["issue", "delete", "5"]).assert_ok();
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    // The next issue gets the next number, not a trashed one's.
    assert_eq!(env.create("zeta"), 6);
    // The ref is still on W-1: no other issue can take it, in any case.
    for args in [
        &["issue", "create", "--title", "thief", "--ref", "ext-1"][..],
        &["issue", "update", "6", "--ref", "EXT-1"],
    ] {
        let out = env.lcl(args);
        assert_eq!(out.code, 1, "{out:#?}");
        assert_eq!(
            out.stderr,
            "Error: ref \"EXT-1\" is already on W-1\n"
                .replace("\"EXT-1\"", &format!("{:?}", args[args.len() - 1])),
            "{args:?}"
        );
    }
    assert_eq!(env.create("eta"), 7);
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    assert_eq!(j(&env, &["issue", "show", "1"])["issue"]["ref"], "EXT-1");
}

/// What uses a trashed issue's project, milestone or cycle can't be
/// deleted from under it, and the refusal says the issue is in the trash.
#[test]
fn what_a_trashed_issue_is_in_stays() {
    let env = workspace();
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    let note = " (1 is in the trash: lcl issue list --trashed shows it; restore it to move it, or purge it)";
    for (args, message) in [
        (
            &["cycle", "delete", "C"][..],
            format!("cycle \"C\" has 1 issue; move it to another cycle or none first{note}"),
        ),
        (
            &["milestone", "delete", "M"],
            format!("milestone \"M\" has 1 issue; move or delete it first{note}"),
        ),
        (
            &["project", "delete", "P"],
            format!("project \"P\" has 1 issue and 1 milestone; move or delete them first{note}"),
        ),
    ] {
        let out = env.lcl(args);
        assert_eq!(
            (out.code, out.stderr.as_str()),
            (1, format!("Error: {message}\n").as_str()),
            "{args:?}"
        );
    }
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    let shown = j(&env, &["issue", "show", "1"]);
    assert_eq!(
        (
            &shown["issue"]["project"],
            &shown["issue"]["milestone"],
            &shown["issue"]["cycles"][0]["name"]
        ),
        (&json!("P"), &json!("M"), &json!("C"))
    );
}

#[test]
fn export_leaves_the_trash_out() {
    let env = workspace();
    env.lcl(&["issue", "comment", "3", "a comment that goes with it"])
        .assert_ok();
    // W-3 has a comment, blocks W-4 and is related to W-1.
    env.lcl(&["issue", "delete", "3"]).assert_ok();
    let export = env.lcl(&["export"]).assert_ok().stdout.clone();
    let records: Vec<Value> = export
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let of = |kind: &str| -> Vec<&Value> { records.iter().filter(|r| r["type"] == kind).collect() };
    assert_eq!(
        of("issue")
            .iter()
            .map(|i| i["number"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 4, 5]
    );
    assert_eq!(
        of("comment")
            .iter()
            .map(|c| c["issue"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        of("relation")
            .iter()
            .map(|r| (
                r["from"].as_i64().unwrap(),
                r["kind"].as_str().unwrap(),
                r["to"].as_i64().unwrap()
            ))
            .collect::<Vec<_>>(),
        [(5, "duplicate", 1)]
    );
    // The format is the one it was: no record says anything about a trash.
    assert!(!export.contains("deleted_at"), "{export}");
    // The counter still covers the trashed number.
    assert_eq!(records[0]["next_issue_number"], 6);

    // Imported, it is the same live issues, numbers and comments, with an
    // empty trash: it exports to the same bytes.
    let copy = Env::new();
    copy.lcl(&["init", "copy", "--key", "W"]).assert_ok();
    copy.lcl_stdin(&["import", "-"], export.as_bytes())
        .assert_ok();
    let again = copy
        .lcl(&["export"])
        .assert_ok()
        .stdout
        .replace("\"name\":\"copy\"", "\"name\":\"w\"");
    assert_eq!(again, export);
    assert_eq!(listed(&copy, &[]), ["W-1", "W-2", "W-4", "W-5"]);
    assert_eq!(listed(&copy, &["--trashed"]), Vec::<String>::new());
    assert_eq!(copy.create("next"), 6);

    // A workspace with nothing but a trashed issue is not empty: its number
    // and ref are taken.
    let trashed_only = Env::new();
    trashed_only.lcl(&["init", "t", "--key", "W"]).assert_ok();
    trashed_only.create("gone");
    trashed_only.lcl(&["issue", "delete", "1"]).assert_ok();
    let out = trashed_only.lcl_stdin(&["import", "-"], export.as_bytes());
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(
        out.stderr
            .contains("is not empty: it has 1 issue (1 in the trash)"),
        "{out:#?}"
    );
}

/// The rows of every table that can name an issue, for issue `number`.
fn rows_of(env: &Env, number: i64) -> Vec<i64> {
    let conn = rusqlite::Connection::open(env.db_path()).unwrap();
    let id: Option<String> = conn
        .query_row("SELECT id FROM issues WHERE number = ?1", [number], |r| {
            r.get(0)
        })
        .ok();
    let Some(id) = id else {
        return vec![0; 5];
    };
    [
        "SELECT count(*) FROM issues WHERE id = ?1",
        "SELECT count(*) FROM comments WHERE issue_id = ?1",
        "SELECT count(*) FROM issue_relations WHERE issue_id = ?1 OR related_id = ?1",
        "SELECT count(*) FROM issue_labels WHERE issue_id = ?1",
        "SELECT count(*) FROM issue_cycles WHERE issue_id = ?1",
    ]
    .iter()
    .map(|sql| conn.query_row(sql, [&id], |r| r.get(0)).unwrap())
    .collect()
}

/// Rows that name an issue that isn't there: none, ever.
fn dangling(env: &Env) -> i64 {
    let conn = rusqlite::Connection::open(env.db_path()).unwrap();
    conn.query_row(
        "SELECT (SELECT count(*) FROM comments WHERE issue_id NOT IN (SELECT id FROM issues))
              + (SELECT count(*) FROM issue_labels WHERE issue_id NOT IN (SELECT id FROM issues))
              + (SELECT count(*) FROM issue_cycles WHERE issue_id NOT IN (SELECT id FROM issues))
              + (SELECT count(*) FROM issue_relations WHERE issue_id NOT IN (SELECT id FROM issues) OR related_id NOT IN (SELECT id FROM issues))
              + (SELECT count(*) FROM issues WHERE parent_id IS NOT NULL AND parent_id NOT IN (SELECT id FROM issues))",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Only from the trash: a live issue can't be purged, by any spelling, and
/// the refusal changes nothing.
#[test]
fn only_an_issue_in_the_trash_can_be_purged() {
    let env = workspace();
    let db = env.db_path();
    let before = dump(&db);
    for id in ["1", "W-3", "#5"] {
        let argv = ["issue", "purge", id, "--json"];
        let out = env.lcl(&argv);
        let n = id.trim_start_matches(['W', '-', '#']);
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (
                1,
                "",
                common::error_text(
                    &argv,
                    "conflict",
                    &format!("W-{n} is not in the trash; only an issue in the trash can be purged (lcl issue delete W-{n} moves it there)")
                )
                .as_str()
            ),
            "{id}"
        );
    }
    // With nothing in the trash, emptying it removes nothing.
    assert_eq!(
        j(&env, &["issue", "purge", "--all"]),
        json!({"purged": [], "issues": 0, "comments": 0, "relations": 0})
    );
    assert_eq!(
        env.lcl(&["issue", "purge", "--all"]).assert_ok().stdout,
        "The trash is empty\n"
    );
    assert!(
        dump(&db) == before,
        "a purge of nothing in the trash changed the workspace"
    );

    // One or the other, not both and not neither: the command line is wrong.
    for args in [&["issue", "purge"][..], &["issue", "purge", "1", "--all"]] {
        assert_eq!(env.lcl(args).code, 2, "{args:?}");
    }
    assert!(dump(&db) == before);
    // An issue that isn't there at all.
    let out = env.lcl(&["issue", "purge", "99"]);
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (1, "Error: issue W-99 not found\n")
    );
}

/// Whole: a purge removes the issue with its comments, relations, label
/// links and cycle links, and nothing of any other issue but the relations
/// that named it.
#[test]
fn a_purge_removes_the_issue_and_everything_on_it_and_nothing_else() {
    let env = workspace();
    let db = env.db_path();
    env.lcl(&["issue", "comment", "3", "one"]).assert_ok();
    env.lcl(&["issue", "comment", "3", "two"]).assert_ok();
    env.lcl(&["issue", "update", "3", "--add-label", "bug", "--cycle", "C"])
        .assert_ok();
    // W-3: 2 comments, blocks W-4, related to W-1, a label, a cycle.
    assert_eq!(rows_of(&env, 3), [1, 2, 2, 1, 1]);
    let others_before: Vec<Value> = ["1", "2", "5"]
        .iter()
        .map(|n| j(&env, &["issue", "show", n]))
        .collect();
    env.lcl(&["issue", "delete", "3"]).assert_ok();
    let trashed = dump(&db);

    assert_eq!(
        j(&env, &["issue", "purge", "3"]),
        json!({"purged": ["W-3"], "issues": 1, "comments": 2, "relations": 2})
    );
    assert_eq!(dangling(&env), 0);
    assert!(common::foreign_key_violations(&db).is_empty());
    fts_integrity(&db, &FTS_TABLES);

    // Exactly W-3's rows are gone: every other row of every table that
    // holds the workspace's own data is as it was.
    let after = dump(&db);
    let gone = |table: &str| trashed.tables[table].len() - after.tables[table].len();
    assert_eq!(
        (
            gone("issues"),
            gone("comments"),
            gone("issue_relations"),
            gone("issue_labels"),
            gone("issue_cycles")
        ),
        (1, 2, 2, 1, 1)
    );
    for table in [
        "issues",
        "comments",
        "issue_relations",
        "issue_labels",
        "issue_cycles",
    ] {
        assert!(
            after.tables[table]
                .iter()
                .all(|row| trashed.tables[table].contains(row)),
            "{table}: a row changed that the purge should have left"
        );
    }
    let (mut was, mut now) = (trashed.clone(), after.clone());
    let logged = common::take_log(&mut was, &mut now);
    for table in ["labels", "projects", "milestones", "cycles", "docs", "meta"] {
        assert!(now.tables[table] == was.tables[table], "{table} changed");
    }
    // The log lost every row about W-3 and its comments, and gained a
    // purge row for each: W-3 and its two comments, with tombstones. What
    // it logged for W-3's relations, labels and cycle went with W-3.
    assert_eq!(
        logged
            .iter()
            .map(|(kind, field, old, new)| (
                kind.as_str(),
                field.as_str(),
                old.is_none(),
                new.is_none()
            ))
            .collect::<Vec<_>>(),
        [
            ("comment", "*", true, true),
            ("comment", "*", true, true),
            ("issue", "*", true, true)
        ]
    );
    let purged: Vec<(String, Option<i64>)> = rusqlite::Connection::open(&db)
        .unwrap()
        .prepare("SELECT kind, number FROM purged ORDER BY kind")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        purged,
        [
            ("comment".into(), None),
            ("comment".into(), None),
            ("issue".into(), Some(3))
        ]
    );
    let w3_rows: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM changes c JOIN purged p ON c.record = p.record OR c.field LIKE '%:' || p.record WHERE c.new IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        w3_rows, 0,
        "a row about W-3 or its comments outlived the purge"
    );
    // The other issues read as they did while W-3 was in the trash, which
    // hid its relations from them already.
    for (n, was) in ["1", "2", "5"].iter().zip(&others_before) {
        let now = j(&env, &["issue", "show", n]);
        assert_eq!(now["issue"], was["issue"], "W-{n}");
        assert_eq!(now["comments"], was["comments"], "W-{n}");
    }
    assert_eq!(found(&env, "gamma"), Vec::<String>::new());

    // Purged is for good: not in the trash, not restorable, not found.
    assert_eq!(listed(&env, &["--trashed"]), Vec::<String>::new());
    for args in [
        &["issue", "restore", "3"][..],
        &["issue", "show", "3"],
        &["issue", "purge", "3"],
    ] {
        let out = env.lcl(args);
        assert_eq!(
            (out.code, out.stderr.as_str()),
            (1, "Error: issue W-3 not found\n"),
            "{args:?}"
        );
    }
    assert_eq!(
        {
            env.lcl(&["issue", "delete", "5"]).assert_ok();
            env.lcl(&["issue", "purge", "5"]).assert_ok().stdout.clone()
        },
        "Purged W-5 with 0 comments and 1 relation\n"
    );
}

/// The number is spent, and the ref is free.
#[test]
fn a_purged_issue_s_number_is_never_reused() {
    let env = workspace();
    for n in ["5", "2", "1"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }
    env.lcl(&["issue", "purge", "5"]).assert_ok();
    // W-5 was the highest number: the next issue still gets 6.
    assert_eq!(env.create("zeta"), 6);
    env.lcl(&["issue", "purge", "--all"]).assert_ok();
    assert_eq!(env.create("eta"), 7);
    // W-1's ref went with it.
    env.lcl(&["issue", "update", "7", "--ref", "EXT-1"])
        .assert_ok();
    assert_eq!(listed(&env, &[]), ["W-3", "W-4", "W-6", "W-7"]);
    // An export and import keeps the counter above the purged numbers.
    let export = env.lcl(&["export"]).assert_ok().stdout.clone();
    let copy = Env::new();
    copy.lcl(&["init", "copy", "--key", "W"]).assert_ok();
    copy.lcl_stdin(&["import", "-"], export.as_bytes())
        .assert_ok();
    assert_eq!(copy.create("theta"), 8);
}

/// Sub-issues first: one issue is not purged from over its sub-issues, and
/// emptying the trash takes a parent and its sub-issues together.
#[test]
fn a_parent_is_purged_after_or_with_its_sub_issues() {
    let env = workspace();
    let db = env.db_path();
    env.lcl(&["issue", "create", "--title", "under beta", "--parent", "2"])
        .assert_ok();
    for n in ["6", "2", "1"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }
    let before = dump(&db);
    let argv = ["issue", "purge", "1", "--json"];
    let out = env.lcl(&argv);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (
            1,
            "",
            common::error_text(
                &argv,
                "conflict",
                "W-1 has sub-issues W-2 in the trash; purge them first, or empty the trash with lcl issue purge --all"
            )
            .as_str()
        )
    );
    assert!(dump(&db) == before, "a refused purge changed the workspace");

    // Bottom up, one at a time.
    env.lcl(&["issue", "purge", "6"]).assert_ok();
    env.lcl(&["issue", "purge", "2"]).assert_ok();
    assert_eq!(dangling(&env), 0);

    // Or all together: W-1 and a new chain under it, plus W-3.
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    env.lcl(&["issue", "create", "--title", "a", "--parent", "1"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "b", "--parent", "7"])
        .assert_ok();
    for n in ["8", "7", "1", "3"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }
    let purged = j(&env, &["issue", "purge", "--all"]);
    assert_eq!(purged["purged"], json!(["W-1", "W-3", "W-7", "W-8"]));
    assert_eq!(purged["issues"], 4);
    // W-1's comment; W-3 blocks W-4, W-3 related W-1, W-5 duplicate-of W-1.
    assert_eq!(
        (&purged["comments"], &purged["relations"]),
        (&json!(1), &json!(3))
    );
    assert_eq!(listed(&env, &[]), ["W-4", "W-5"]);
    assert_eq!(listed(&env, &["--trashed"]), Vec::<String>::new());
    assert_eq!(dangling(&env), 0);
    assert!(common::foreign_key_violations(&db).is_empty());
    fts_integrity(&db, &FTS_TABLES);
    // What the trash held their places in can go now.
    env.lcl(&["milestone", "delete", "M"]).assert_ok();
    env.lcl(&["project", "delete", "P"]).assert_ok();
    env.lcl(&["cycle", "delete", "C"]).assert_ok();
}

/// Only when asked: however long an issue has been in the trash, and
/// whatever else is run, it is there until `issue purge`.
#[test]
fn nothing_is_purged_by_itself() {
    let env = workspace();
    let db = env.db_path();
    env.lcl(&["issue", "delete", "5"]).assert_ok();
    // In the trash since 1970.
    common::exec(&db, "UPDATE issues SET deleted_at = 0 WHERE number = 5");
    let trashed = dump(&db);
    for args in [
        &["issue", "list"][..],
        &["issue", "list", "--trashed"],
        &["issue", "create", "--title", "new"],
        &["issue", "delete", "6"],
        &["issue", "restore", "6"],
        &["search", "epsilon"],
        &["export"],
        &["backup", "copy.sqlite"],
        &["workspace", "show"],
        &["workspace", "list"],
    ] {
        env.lcl(args).assert_ok();
    }
    assert_eq!(listed(&env, &["--trashed"]), ["W-5"]);
    assert_eq!(rows_of(&env, 5), [1, 0, 1, 0, 0]);
    // Its row is as it was put in the trash.
    let now = dump(&db);
    let row = |d: &Dump| {
        d.tables["issues"]
            .iter()
            .find(|r| r["number"] == rusqlite::types::Value::Integer(5))
            .cloned()
    };
    assert_eq!(row(&now), row(&trashed));
    env.lcl(&["issue", "restore", "5"]).assert_ok();
}

/// An issue that is a duplicate of a trashed issue: the refusal to make it
/// a duplicate of another names the two ways out that work, not unrelate,
/// which can't name a trashed issue.
#[test]
fn a_duplicate_of_a_trashed_issue_is_told_how_to_get_free() {
    let env = workspace();
    // W-5 is a duplicate of W-1; W-2 is under W-1.
    env.lcl(&["issue", "delete", "2"]).assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    let argv = ["issue", "relate", "5", "duplicate-of", "4", "--json"];
    let out = env.lcl(&argv);
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (
            1,
            common::error_text(
                &argv,
                "conflict",
                "W-5 is already a duplicate of W-1, which is in the trash; restore it (lcl issue restore W-1) and unrelate it, or purge it (lcl issue purge W-1)"
            )
            .as_str()
        )
    );
    // Both ways out work.
    env.lcl(&["issue", "restore", "1"]).assert_ok();
    env.lcl(&["issue", "unrelate", "5", "duplicate-of", "1"])
        .assert_ok();
    env.lcl(&["issue", "relate", "5", "duplicate-of", "1"])
        .assert_ok();
    env.lcl(&["issue", "delete", "1"]).assert_ok();
    env.lcl(&["issue", "purge", "--all"]).assert_ok();
    env.lcl(&["issue", "relate", "5", "duplicate-of", "4"])
        .assert_ok();
    // A live target still names unrelate.
    let out = env.lcl(&["issue", "relate", "5", "duplicate-of", "3"]);
    assert_eq!(
        out.stderr,
        "Error: W-5 is already a duplicate of W-4; unrelate it first: lcl issue unrelate W-5 duplicate-of W-4\n"
    );
}
