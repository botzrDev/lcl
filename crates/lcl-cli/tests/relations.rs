//! Relations (prove-out blocker 3): blocks / blocked-by, related, and
//! duplicate-of. One row per relation in one stored form, checked inside the
//! write transaction, removed with either issue, and shown from both sides.

mod common;

use common::{Env, argv, dump, exec, foreign_key_violations};
use serde_json::{Value, json};

/// A workspace (key DREA) with issues 1..=n, titled "issue N".
fn workspace(n: i64) -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    for i in 1..=n {
        assert_eq!(env.create(&format!("issue {i}")), i);
    }
    env
}

fn relate(env: &Env, a: i64, kind: &str, b: i64) -> common::Out {
    env.lcl(&["issue", "relate", &a.to_string(), kind, &b.to_string()])
}

fn relations(env: &Env, n: i64) -> Value {
    env.lcl(&["issue", "show", &n.to_string(), "--json"])
        .assert_ok()
        .json()["relations"]
        .clone()
}

/// The identifiers in one list of `relations`.
fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap_or_else(|| panic!("not a list: {list}"))
        .iter()
        .map(|o| o["identifier"].as_str().expect("identifier").to_string())
        .collect()
}

/// Every stored row as (issue number, related number, kind), sorted.
fn rows(env: &Env) -> Vec<(i64, i64, String)> {
    let conn = rusqlite::Connection::open(env.db_path()).expect("open db");
    let mut stmt = conn
        .prepare("SELECT a.number, b.number, r.kind FROM issue_relations r JOIN issues a ON a.id = r.issue_id JOIN issues b ON b.id = r.related_id ORDER BY 1, 2, 3")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn listed(env: &Env, args: &[&str]) -> Value {
    let mut cmd = vec!["issue", "list", "--json"];
    cmd.extend(args);
    env.lcl(&cmd).assert_ok().json()
}

fn numbers(list: &Value) -> Vec<i64> {
    list["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| i["number"].as_i64().expect("number"))
        .collect()
}

/// `--json` spells a relation's kind as `issue show` names the relations,
/// however it was typed; text keeps the typed words.
#[test]
fn relate_prints_the_kind_as_issue_show_names_it() {
    let env = workspace(4);
    for (typed, printed) in [
        ("blocked-by", "blocked_by"),
        ("Blocked_By", "blocked_by"),
        ("duplicate-of", "duplicate_of"),
        ("blocks", "blocks"),
    ] {
        let (a, b) = if printed == "blocks" {
            ("4", "1")
        } else {
            ("2", "3")
        };
        let made = env
            .lcl(&["--json", "issue", "relate", a, typed, b])
            .assert_ok()
            .json();
        assert_eq!(made["kind"], printed, "{typed}: {made}");
        let gone = env
            .lcl(&["--json", "issue", "unrelate", a, typed, b])
            .assert_ok()
            .json();
        assert_eq!(
            (&gone["kind"], &gone["removed"]),
            (&json!(printed), &json!(true)),
            "{typed}"
        );
    }
}

#[test]
fn each_kind_shows_from_both_sides() {
    let env = workspace(7);
    relate(&env, 1, "blocks", 2).assert_ok();
    relate(&env, 3, "blocked-by", 1).assert_ok();
    relate(&env, 1, "related", 4).assert_ok();
    relate(&env, 5, "related", 1).assert_ok();
    relate(&env, 6, "duplicate-of", 1).assert_ok();

    let one = relations(&env, 1);
    assert_eq!(ids(&one["blocks"]), ["DREA-2", "DREA-3"]);
    assert_eq!(ids(&one["blocked_by"]), Vec::<String>::new());
    assert_eq!(ids(&one["related"]), ["DREA-4", "DREA-5"]);
    assert_eq!(one["duplicate_of"], Value::Null);
    assert_eq!(ids(&one["duplicates"]), ["DREA-6"]);
    assert_eq!(
        one["blocks"][0],
        json!({"identifier": "DREA-2", "title": "issue 2", "status": "Backlog"})
    );
    assert_eq!(one["duplicates"][0]["status"], "Duplicate");

    for n in [2, 3] {
        assert_eq!(
            ids(&relations(&env, n)["blocked_by"]),
            ["DREA-1"],
            "DREA-{n}"
        );
        assert_eq!(
            ids(&relations(&env, n)["blocks"]),
            Vec::<String>::new(),
            "DREA-{n}"
        );
    }
    for n in [4, 5] {
        assert_eq!(ids(&relations(&env, n)["related"]), ["DREA-1"], "DREA-{n}");
    }
    assert_eq!(
        relations(&env, 6)["duplicate_of"],
        json!({"identifier": "DREA-1", "title": "issue 1", "status": "Backlog"})
    );
    assert_eq!(
        relations(&env, 7),
        json!({"blocks": [], "blocked_by": [], "related": [], "duplicate_of": null, "duplicates": []})
    );

    let show = |n: i64| {
        env.lcl(&["issue", "show", &n.to_string()])
            .assert_ok()
            .stdout
            .clone()
    };
    let one = show(1);
    for line in [
        "Blocks: DREA-2 [Backlog], DREA-3 [Backlog]\n",
        "Related: DREA-4 [Backlog], DREA-5 [Backlog]\n",
        "Duplicates: DREA-6 [Duplicate]\n",
    ] {
        assert!(one.contains(line), "{line:?} not in:\n{one}");
    }
    assert!(
        !one.contains("Blocked by") && !one.contains("Duplicate of"),
        "{one}"
    );
    assert!(
        show(3).contains("\nBlocked by: DREA-1 [Backlog]\n"),
        "{}",
        show(3)
    );
    assert!(
        show(5).contains("\nRelated: DREA-1 [Backlog]\n"),
        "{}",
        show(5)
    );
    assert!(
        show(6).contains("\nDuplicate of: DREA-1 [Backlog]\n"),
        "{}",
        show(6)
    );
    let seven = show(7);
    for name in ["Blocks", "Blocked by", "Related", "Duplicate"] {
        assert!(!seven.contains(name), "{seven}");
    }
}

/// `A blocked-by B` is the row `B blocks A`; related is stored once whichever
/// way round it is made. Making an existing relation again is a no-op.
#[test]
fn one_row_per_relation() {
    let env = workspace(3);
    let out = relate(&env, 1, "blocks", 2).assert_ok().stdout.clone();
    assert_eq!(out, "DREA-1 blocks DREA-2\n");
    let again = relate(&env, 2, "blocked-by", 1);
    assert_eq!(
        (again.code, again.stdout.as_str()),
        (0, "Already related: DREA-2 blocked-by DREA-1\n")
    );
    let json = env
        .lcl(&["issue", "relate", "1", "blocks", "2", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        json,
        json!({"issue": "DREA-1", "kind": "blocks", "related": "DREA-2", "created": false})
    );
    assert_eq!(rows(&env), [(1, 2, "blocks".to_string())]);

    // A blocked-by B, stored as B blocks A.
    relate(&env, 3, "Blocked_By", 2).assert_ok();
    assert_eq!(
        rows(&env),
        [(1, 2, "blocks".to_string()), (2, 3, "blocks".to_string())]
    );

    // Related both ways: one row, whichever order made it.
    relate(&env, 3, "related", 1).assert_ok();
    let again = env
        .lcl(&["issue", "relate", "1", "related", "3", "--json"])
        .assert_ok()
        .json();
    assert_eq!(again["created"], false);
    let related: Vec<_> = rows(&env)
        .into_iter()
        .filter(|r| r.2 == "related")
        .collect();
    assert_eq!(related.len(), 1, "{related:?}");
    // A different kind between the same two issues is a different relation.
    relate(&env, 2, "related", 1).assert_ok();
    assert_eq!(rows(&env).len(), 4);

    let out = relate(&env, 1, "precedes", 2);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("expected one of: blocks, blocked-by, related, duplicate-of"),
        "{out:#?}"
    );
}

#[test]
fn unrelate_in_either_form() {
    let env = workspace(3);
    relate(&env, 1, "blocks", 2).assert_ok();
    relate(&env, 3, "related", 1).assert_ok();
    let out = env
        .lcl(&["issue", "unrelate", "2", "blocked-by", "1"])
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(out, "Removed: DREA-2 blocked-by DREA-1\n");
    let json = env
        .lcl(&["issue", "unrelate", "1", "related", "3", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        json,
        json!({"issue": "DREA-1", "kind": "related", "related": "DREA-3", "removed": true})
    );
    assert_eq!(rows(&env), Vec::new());
    assert_eq!(
        relations(&env, 1),
        json!({"blocks": [], "blocked_by": [], "related": [], "duplicate_of": null, "duplicates": []})
    );

    // Nothing to remove: exit 1, naming it, and nothing changes.
    relate(&env, 1, "blocks", 2).assert_ok();
    let db = env.db_path();
    let before = dump(&db);
    for (args, named) in [
        (
            vec!["issue", "unrelate", "2", "blocks", "1"],
            "DREA-2 blocks DREA-1",
        ),
        (
            vec!["issue", "unrelate", "1", "related", "2"],
            "DREA-1 related DREA-2",
        ),
        (
            vec!["issue", "unrelate", "1", "duplicate-of", "3"],
            "DREA-1 duplicate-of DREA-3",
        ),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(
            out.stderr
                .contains(&format!("there is no relation {named}")),
            "{args:?}: {out:#?}"
        );
    }
    assert_eq!(dump(&db), before);
}

/// Each refusal depends on what is stored (what the references resolve to,
/// the relations there): it exits 1 and leaves the database as it was.
#[test]
fn refusals_exit_1_and_change_nothing() {
    let env = workspace(9);
    relate(&env, 1, "blocks", 2).assert_ok();
    relate(&env, 2, "blocks", 3).assert_ok();
    relate(&env, 4, "duplicate-of", 5).assert_ok();
    relate(&env, 6, "duplicate-of", 7).assert_ok();
    relate(&env, 7, "duplicate-of", 8).assert_ok();
    let db = env.db_path();
    let before = dump(&db);

    for (args, message) in [
        (
            vec!["1", "blocks", "1"],
            "DREA-1 can't be related to itself",
        ),
        (
            vec!["1", "related", "#1"],
            "DREA-1 can't be related to itself",
        ),
        (
            vec!["DREA-1", "duplicate-of", "1"],
            "DREA-1 can't be related to itself",
        ),
        (
            vec!["2", "blocks", "1"],
            "DREA-2 blocking DREA-1 would make a cycle: DREA-2 blocks DREA-1 blocks DREA-2",
        ),
        (
            vec!["1", "blocked-by", "2"],
            "DREA-2 blocking DREA-1 would make a cycle: DREA-2 blocks DREA-1 blocks DREA-2",
        ),
        (
            vec!["3", "blocks", "1"],
            "DREA-3 blocking DREA-1 would make a cycle: DREA-3 blocks DREA-1 blocks DREA-2 blocks DREA-3",
        ),
        (
            vec!["5", "duplicate-of", "4"],
            "making DREA-5 a duplicate of DREA-4 would make a cycle: DREA-5 -> DREA-4 -> DREA-5",
        ),
        (
            vec!["8", "duplicate-of", "6"],
            "making DREA-8 a duplicate of DREA-6 would make a cycle: DREA-8 -> DREA-6 -> DREA-7 -> DREA-8",
        ),
        (
            vec!["4", "duplicate-of", "9"],
            "DREA-4 is already a duplicate of DREA-5; unrelate it first: lcl issue unrelate DREA-4 duplicate-of DREA-5",
        ),
    ] {
        let mut cmd = vec!["issue", "relate"];
        cmd.extend(&args);
        let out = env.lcl(&cmd);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(message), "{args:?}: {out:#?}");
        assert_eq!(dump(&db), before, "{args:?} changed the database");
    }
    let out = env.lcl(&["issue", "unrelate", "3", "related", "3"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert_eq!(dump(&db), before);
    // A blocks chain that doesn't close a loop is fine: 1 blocks 3.
    relate(&env, 1, "blocks", 3).assert_ok();
}

/// The schema itself holds an issue to one duplicate-of target, for writers
/// that insert rows directly (an import) and skip `lcl issue relate`.
#[test]
fn the_schema_allows_one_duplicate_target() {
    let env = workspace(3);
    relate(&env, 1, "duplicate-of", 2).assert_ok();
    let conn = rusqlite::Connection::open(env.db_path()).expect("open db");
    let insert = |related: i64| {
        conn.execute(
            "INSERT INTO issue_relations(issue_id, related_id, kind, created_at)
             SELECT a.id, b.id, 'duplicate', 0 FROM issues a, issues b WHERE a.number = 1 AND b.number = ?1",
            [related],
        )
    };
    let err = insert(3).expect_err("a second duplicate target was stored");
    assert!(
        matches!(&err, rusqlite::Error::SqliteFailure(e, Some(m)) if e.code == rusqlite::ErrorCode::ConstraintViolation && m.contains("UNIQUE") && m.contains("issue_relations.issue_id")),
        "{err:?}"
    );
    // Other kinds from the same issue are unaffected.
    for kind in ["blocks", "related"] {
        conn.execute(
            "INSERT INTO issue_relations(issue_id, related_id, kind, created_at)
             SELECT a.id, b.id, ?1, 0 FROM issues a, issues b WHERE a.number = 1 AND b.number = 3",
            [kind],
        )
        .unwrap_or_else(|e| panic!("{kind}: {e}"));
    }
    drop(conn);
    assert_eq!(relations(&env, 1)["duplicate_of"]["identifier"], "DREA-2");
}

/// Deleting an issue moves it to the trash with its relations: they stay
/// stored, are hidden from the issues at their other ends, and are there
/// again once it is restored.
#[test]
fn a_trashed_issue_s_relations_are_hidden_until_it_is_restored() {
    let env = workspace(6);
    relate(&env, 1, "blocks", 2).assert_ok();
    relate(&env, 3, "blocks", 1).assert_ok();
    relate(&env, 1, "related", 4).assert_ok();
    relate(&env, 5, "duplicate-of", 1).assert_ok();
    relate(&env, 1, "duplicate-of", 6).assert_ok();
    relate(&env, 2, "related", 3).assert_ok();
    let stored = rows(&env);
    assert_eq!(stored.len(), 6);
    let shown: Vec<Value> = (1..=6).map(|n| relations(&env, n)).collect();

    env.lcl(&["issue", "delete", "1"]).assert_ok();
    assert_eq!(rows(&env), stored, "the relations stay stored");
    for n in [4, 5, 6] {
        assert_eq!(
            relations(&env, n),
            json!({"blocks": [], "blocked_by": [], "related": [], "duplicate_of": null, "duplicates": []}),
            "DREA-{n}"
        );
    }
    assert_eq!(ids(&relations(&env, 2)["related"]), ["DREA-3"]);
    assert_eq!(ids(&relations(&env, 2)["blocked_by"]), Vec::<String>::new());
    assert_eq!(ids(&relations(&env, 3)["blocks"]), Vec::<String>::new());

    env.lcl(&["issue", "restore", "1"]).assert_ok();
    assert_eq!(rows(&env), stored);
    for n in 1..=6 {
        assert_eq!(relations(&env, n), shown[n as usize - 1], "DREA-{n}");
    }
    assert!(foreign_key_violations(&env.db_path()).is_empty());
}

/// duplicate-of closes the issue as Duplicate; unrelating leaves it so.
#[test]
fn duplicate_of_sets_the_status_and_unrelate_keeps_it() {
    let env = workspace(2);
    env.lcl(&["issue", "update", "1", "--status", "InProgress"])
        .assert_ok();
    let out = relate(&env, 1, "duplicate-of", 2)
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(out, "DREA-1 duplicate-of DREA-2; DREA-1 is Duplicate\n");
    let issue = env
        .lcl(&["issue", "show", "1", "--json"])
        .assert_ok()
        .json()["issue"]
        .clone();
    assert_eq!(issue["status"], "Duplicate");
    let canceled_at = issue["canceled_at"].as_i64().expect("canceled_at set");
    assert!(issue["started_at"].is_i64(), "{issue:#}");

    env.lcl(&["issue", "unrelate", "1", "duplicate-of", "2"])
        .assert_ok();
    let after = env
        .lcl(&["issue", "show", "1", "--json"])
        .assert_ok()
        .json()["issue"]
        .clone();
    assert_eq!(
        (&after["status"], after["canceled_at"].as_i64()),
        (&json!("Duplicate"), Some(canceled_at))
    );
    assert_eq!(relations(&env, 1)["duplicate_of"], Value::Null);

    let help = env
        .lcl(&["issue", "unrelate", "--help"])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        help.contains("Statuses stay as they are: unrelating duplicate-of leaves A Duplicate."),
        "{help}"
    );
}

/// Blocked: at least one blocker whose stored status is not Done, Canceled
/// or Duplicate. The comparison is the stored text.
#[test]
fn the_blocked_filter() {
    let env = workspace(4);
    relate(&env, 1, "blocks", 2).assert_ok();
    relate(&env, 3, "blocks", 2).assert_ok();
    assert_eq!(numbers(&listed(&env, &["--blocked"])), [2]);
    assert_eq!(numbers(&listed(&env, &["--unblocked"])), [4, 3, 1]);

    env.lcl(&["issue", "update", "1", "--status", "Done"])
        .assert_ok();
    assert_eq!(
        numbers(&listed(&env, &["--blocked"])),
        [2],
        "DREA-3 still blocks"
    );
    exec(
        &env.db_path(),
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'done' WHERE number = 3",
    );
    assert_eq!(
        numbers(&listed(&env, &["--blocked"])),
        [2],
        "a hand-edited spelling is not the stored canonical Done, so it still blocks"
    );
    assert_eq!(numbers(&listed(&env, &["--unblocked"])), [4, 3, 1]);
    for status in ["Canceled", "Duplicate", "Todo"] {
        env.lcl(&["issue", "update", "3", "--status", status])
            .assert_ok();
        let want: Vec<i64> = if status == "Todo" { vec![2] } else { vec![] };
        assert_eq!(
            numbers(&listed(&env, &["--blocked"])),
            want,
            "blocker {status}"
        );
    }

    // With other filters and a limit: 10 issues behind one open blocker,
    // five of them Todo.
    for i in 5..=15 {
        assert_eq!(env.create(&format!("issue {i}")), i);
    }
    for i in 5..=14 {
        relate(&env, 15, "blocks", i).assert_ok();
        if i <= 9 {
            env.lcl(&["issue", "update", &i.to_string(), "--status", "Todo"])
                .assert_ok();
        }
    }
    let page = listed(&env, &["--blocked", "--status", "Todo", "--limit", "2"]);
    assert_eq!(numbers(&page), [9, 8]);
    assert_eq!(
        (&page["total"], &page["truncated"]),
        (&json!(5), &json!(true))
    );
    // DREA-3 ended the loop above as Todo, with nothing blocking it.
    let page = listed(&env, &["--unblocked", "--status", "Todo"]);
    assert_eq!((numbers(&page), &page["total"]), (vec![3], &json!(1)));
    let page = listed(&env, &["--blocked", "--all"]);
    assert_eq!(
        (numbers(&page), &page["total"]),
        (
            (2..=14)
                .rev()
                .filter(|&n| n != 3 && n != 4)
                .collect::<Vec<_>>(),
            &json!(11)
        )
    );

    let out = env.lcl(&["issue", "list", "--blocked", "--unblocked"]);
    assert_eq!(out.code, 2, "{out:#?}");
}

/// Opposite blocks between the same two issues, raced: the cycle check runs
/// inside the write transaction, so exactly one of each pair is stored.
#[test]
fn racing_opposite_blocks_store_one() {
    const PAIRS: i64 = 30;
    let env = workspace(2 * PAIRS);
    let mut cmds = vec![];
    for p in 0..PAIRS {
        let (a, b) = ((2 * p + 1).to_string(), (2 * p + 2).to_string());
        cmds.push(argv(&["issue", "relate", &a, "blocks", &b]));
        cmds.push(argv(&["issue", "relate", &b, "blocks", &a]));
    }
    let outs = env.lcl_parallel(&cmds);
    for p in 0..PAIRS as usize {
        let (one, two) = (&outs[2 * p], &outs[2 * p + 1]);
        let mut codes = [one.code, two.code];
        codes.sort_unstable();
        assert_eq!(codes, [0, 1], "pair {p}: {one:#?} {two:#?}");
        let refused = if one.code == 1 { one } else { two };
        assert!(
            refused.stderr.contains("would make a cycle"),
            "pair {p}: {refused:#?}"
        );
    }
    let stored = rows(&env);
    for p in 0..PAIRS {
        let (a, b) = (2 * p + 1, 2 * p + 2);
        let between = stored
            .iter()
            .filter(|r| (r.0, r.1) == (a, b) || (r.0, r.1) == (b, a))
            .count();
        assert_eq!(between, 1, "pair {a}/{b}: {stored:?}");
    }
}

/// Twenty different relates at once all land.
#[test]
fn concurrent_relates_all_land() {
    let env = workspace(21);
    let mut cmds = vec![];
    for b in 2..=11 {
        cmds.push(argv(&["issue", "relate", "1", "blocks", &b.to_string()]));
    }
    for a in 12..=16 {
        cmds.push(argv(&["issue", "relate", &a.to_string(), "related", "1"]));
    }
    for a in 17..=20 {
        cmds.push(argv(&[
            "issue",
            "relate",
            &a.to_string(),
            "duplicate-of",
            "1",
        ]));
    }
    cmds.push(argv(&["issue", "relate", "21", "blocked-by", "1"]));
    assert_eq!(cmds.len(), 20);
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "{:?}: {out:#?}", cmds[i]);
    }
    assert_eq!(rows(&env).len(), 20);
    let one = relations(&env, 1);
    let blocked: Vec<String> = (2..=11).chain([21]).map(|n| format!("DREA-{n}")).collect();
    assert_eq!(ids(&one["blocks"]), blocked);
    assert_eq!(
        ids(&one["related"]),
        (12..=16).map(|n| format!("DREA-{n}")).collect::<Vec<_>>()
    );
    assert_eq!(
        ids(&one["duplicates"]),
        (17..=20).map(|n| format!("DREA-{n}")).collect::<Vec<_>>()
    );
}
