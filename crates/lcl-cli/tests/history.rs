//! `lcl issue history`, and the change log under it (docs/sync.md): every
//! write logs its changes in its own transaction, a write that fails or
//! changes nothing logs nothing, and an issue's history replays to the
//! issue as it is.

mod common;

use common::{Env, dump};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// `lcl ARGS --json`, which must succeed, as its one JSON value.
#[track_caller]
fn j(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    env.lcl(&argv).assert_ok().json()
}

fn history(env: &Env, issue: &str) -> Vec<Value> {
    j(env, &["issue", "history", issue])
        .as_array()
        .expect("a list")
        .clone()
}

/// The rows in the change log.
fn log_rows(env: &Env) -> i64 {
    rusqlite::Connection::open(env.db_path())
        .unwrap()
        .query_row("SELECT count(*) FROM changes", [], |r| r.get(0))
        .unwrap()
}

/// Every (field, old, new) of an issue's history after its first entry.
fn later_changes(h: &[Value]) -> Vec<(String, Value, Value)> {
    h[1..]
        .iter()
        .flat_map(|e| e["changes"].as_array().unwrap().clone())
        .map(|c| {
            (
                c["field"].as_str().unwrap().to_string(),
                c["old"].clone(),
                c["new"].clone(),
            )
        })
        .collect()
}

fn workspace() -> Env {
    let env = Env::new();
    j(&env, &["init", "w", "--key", "W"]);
    for label in ["bug", "ui", "later"] {
        j(&env, &["label", "create", label]);
    }
    j(&env, &["cycle", "create", "S1"]);
    j(&env, &["cycle", "create", "S2"]);
    j(&env, &["project", "create", "P1"]);
    j(&env, &["project", "create", "P2"]);
    j(&env, &["milestone", "create", "M1", "--project", "P1"]);
    j(&env, &["milestone", "create", "M2", "--project", "P1"]);
    j(&env, &["milestone", "create", "M3", "--project", "P2"]);
    env
}

#[test]
fn a_history_lists_each_write_with_its_author_oldest_first() {
    let env = workspace();
    env.lcl_with(
        &[
            "issue",
            "create",
            "--title",
            "Fix the parser",
            "--label",
            "bug",
            "--priority",
            "high",
        ],
        |cmd| {
            cmd.env("USER", "ann");
        },
    )
    .assert_ok();
    j(&env, &["issue", "create", "--title", "Other"]);
    env.lcl_with(
        &[
            "issue",
            "update",
            "1",
            "--status",
            "In Progress",
            "--cycle",
            "S1",
        ],
        |cmd| {
            cmd.env("LCL_AUTHOR", "bob").env("USER", "ann");
        },
    )
    .assert_ok();
    env.lcl_with(&["issue", "relate", "2", "blocks", "1"], |cmd| {
        cmd.env_remove("USER");
    })
    .assert_ok();
    let c = j(
        &env,
        &["issue", "comment", "1", "Looks done", "--author", "cy"],
    );
    let c = c["id"].as_str().unwrap().to_string();
    j(&env, &["comment", "edit", &c, "Looks done to me"]);
    j(&env, &["comment", "delete", &c]);

    let h = history(&env, "W-1");
    let authors: Vec<&Value> = h.iter().map(|e| &e["author"]).collect();
    let user = std::env::var("USER").map_or(Value::Null, Value::from);
    assert_eq!(
        authors,
        [
            &json!("ann"),
            &json!("bob"),
            &Value::Null,
            &json!("cy"),
            &user,
            &user
        ],
        "LCL_AUTHOR before USER; --author for the comment"
    );
    let at: Vec<i64> = h.iter().map(|e| e["at"].as_i64().unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] <= w[1]), "{at:?}");

    // The creation: every field that isn't null, from null.
    let first: Vec<(&str, &Value, &Value)> = h[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["field"].as_str().unwrap(), &c["old"], &c["new"]))
        .collect();
    let created = j(&env, &["issue", "show", "1"])["issue"]["created_at"].clone();
    assert_eq!(
        first,
        [
            ("title", &Value::Null, &json!("Fix the parser")),
            ("description", &Value::Null, &json!("")),
            ("status", &Value::Null, &json!("Backlog")),
            ("priority", &Value::Null, &json!("High")),
            ("creator", &Value::Null, &json!("ann")),
            ("created_at", &Value::Null, &created),
            ("labels", &Value::Null, &json!("bug")),
        ]
    );
    let started = j(&env, &["issue", "show", "1"])["issue"]["started_at"].clone();
    assert_eq!(
        h[1]["changes"],
        json!([
            {"field": "status", "comment_id": null, "old": "Backlog", "new": "InProgress"},
            {"field": "started_at", "comment_id": null, "old": null, "new": started},
            {"field": "cycles", "comment_id": null, "old": null, "new": "S1"},
        ])
    );
    // A relation shows on both issues, each from its own side.
    assert_eq!(
        h[2]["changes"],
        json!([{"field": "blocked_by", "comment_id": null, "old": null, "new": "W-2"}])
    );
    assert_eq!(
        later_changes(&history(&env, "2")),
        [("blocks".to_string(), Value::Null, json!("W-1"))]
    );
    let deleted = j(&env, &["issue", "comments", "1", "--trashed"])[0]["deleted_at"].clone();
    assert_eq!(
        [&h[3]["changes"], &h[4]["changes"], &h[5]["changes"]],
        [
            &json!([{"field": "body", "comment_id": c, "old": null, "new": "Looks done"}]),
            &json!([{"field": "body", "comment_id": c, "old": "Looks done", "new": "Looks done to me"}]),
            &json!([{"field": "deleted_at", "comment_id": c, "old": null, "new": deleted}]),
        ]
    );
}

#[test]
fn the_text_history_reads_as_changes() {
    let env = workspace();
    env.lcl_with(
        &["issue", "create", "--title", "Parser", "--label", "bug"],
        |cmd| {
            cmd.env("USER", "ann");
        },
    )
    .assert_ok();
    j(&env, &["issue", "create", "--title", "Second"]);
    env.lcl_with(
        &[
            "issue",
            "update",
            "1",
            "--title",
            "Parser, fixed",
            "--priority",
            "urgent",
            "--parent",
            "2",
            "--remove-label",
            "bug",
            "--add-label",
            "ui",
        ],
        |cmd| {
            cmd.env("USER", "bob");
        },
    )
    .assert_ok();
    env.lcl_with(
        &["issue", "comment", "1", "first line\nsecond line"],
        |cmd| {
            cmd.env_remove("USER");
        },
    )
    .assert_ok();
    let out = env.lcl(&["issue", "history", "1"]);
    let text = out.assert_ok().stdout.clone();
    let lines: Vec<&str> = text.lines().collect();
    // Times are local, so only their shape is checked.
    let shape: Vec<String> = lines
        .iter()
        .map(|l| {
            let l = l.to_string();
            match l.find(" · ") {
                Some(i) if !l.starts_with(' ') => format!("DATE{}", &l[i..]),
                _ if l.starts_with("  created_at: ") => "  created_at: DATE".into(),
                _ if l.starts_with("  comment ") => {
                    let (_, rest) = l.split_once(": ").unwrap();
                    format!("  comment ID: {rest}")
                }
                _ => l,
            }
        })
        .collect();
    assert_eq!(
        shape,
        [
            "DATE · ann · created",
            "  title: Parser",
            "  status: Backlog",
            "  priority: None",
            "  creator: ann",
            "  created_at: DATE",
            "  labels: + bug",
            "DATE · bob",
            "  title: Parser → Parser, fixed",
            "  priority: None → Urgent",
            "  parent: none → W-2",
            "  labels: - bug",
            "  labels: + ui",
            "DATE · anon",
            "  comment ID: + first line…",
        ],
        "{text}"
    );
    // Nothing to list for nothing: an issue has a history from its start.
    assert_eq!(
        env.lcl(&["issue", "history", "9"]).code,
        1,
        "an issue that doesn't exist"
    );
}

#[test]
fn a_write_that_fails_or_changes_nothing_logs_nothing() {
    let env = workspace();
    j(
        &env,
        &[
            "issue", "create", "--title", "One", "--label", "bug", "--cycle", "S1",
        ],
    );
    j(&env, &["issue", "create", "--title", "Two"]);
    j(&env, &["issue", "relate", "1", "blocks", "2"]);
    let rows = log_rows(&env);
    let before = dump(&env.db_path());
    // Refused: nothing written, the log included.
    for args in [
        &["issue", "update", "1", "--parent", "1"][..],
        &["issue", "update", "1", "--label", "nope"],
        &["issue", "relate", "2", "blocks", "1"],
        &["issue", "update", "1", "--milestone", "M1"],
        &["issue", "purge", "1"],
    ] {
        let out = env.lcl(args);
        assert_ne!(out.code, 0, "{args:?}: {out:#?}");
        assert_eq!(log_rows(&env), rows, "{args:?}");
    }
    // Done already: nothing changes, so nothing is logged, and the clock
    // is as it was. (`issue update` still sets `updated_at` and re-indexes
    // the issue for search, as it did before.)
    for args in [
        &["issue", "update", "1", "--title", "One"][..],
        &["issue", "update", "1", "--label", "bug"],
        &[
            "issue",
            "update",
            "1",
            "--add-label",
            "BUG",
            "--cycle",
            "s1",
        ],
        &[
            "issue",
            "update",
            "2",
            "--remove-label",
            "bug",
            "--no-cycle",
            "--no-parent",
        ],
        &["issue", "relate", "2", "blocked-by", "1"],
    ] {
        let out = env.lcl(args);
        assert_eq!(out.code, 0, "{args:?}: {out:#?}");
        let now = dump(&env.db_path());
        assert_eq!(
            now.tables["changes"], before.tables["changes"],
            "{args:?} logged"
        );
        assert_eq!(
            now.tables["meta"], before.tables["meta"],
            "{args:?} moved the clock"
        );
    }
    // A write with nothing to do at all leaves the file as it was.
    env.lcl(&["issue", "relate", "1", "blocks", "2"])
        .assert_ok();
    let again = dump(&env.db_path());
    env.lcl(&["issue", "relate", "1", "blocks", "2"])
        .assert_ok();
    assert!(dump(&env.db_path()) == again);
    // A write that changes something always logs it.
    j(
        &env,
        &["issue", "update", "1", "--label", "ui", "--cycle", "S2"],
    );
    assert_eq!(
        later_changes(&history(&env, "1")),
        [
            ("blocks".into(), Value::Null, json!("W-2")),
            ("labels".into(), json!("bug"), Value::Null),
            ("labels".into(), Value::Null, json!("ui")),
            ("cycles".into(), json!("S1"), Value::Null),
            ("cycles".into(), Value::Null, json!("S2")),
        ]
    );
}

#[test]
fn trash_restore_and_purge() {
    let env = workspace();
    j(&env, &["issue", "create", "--title", "One"]);
    j(&env, &["issue", "create", "--title", "Two"]);
    j(&env, &["issue", "relate", "1", "related", "2"]);
    let c = j(&env, &["issue", "comment", "1", "gone soon"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    j(&env, &["comment", "delete", &c]);
    j(&env, &["comment", "purge", &c]);
    j(&env, &["issue", "delete", "1"]);
    // An issue in the trash has a history; its comment purged has none.
    let h = history(&env, "1");
    let deleted = j(&env, &["issue", "list", "--trashed"])["issues"][0]["deleted_at"].clone();
    assert_eq!(
        later_changes(&h),
        [
            ("related".into(), Value::Null, json!("W-2")),
            ("deleted_at".into(), Value::Null, deleted.clone()),
        ]
    );
    j(&env, &["issue", "restore", "1"]);
    assert_eq!(
        later_changes(&history(&env, "1")).last().unwrap(),
        &("deleted_at".to_string(), deleted, Value::Null)
    );
    // Purged: its history goes, on the other issue too, and its number is
    // remembered (tombstones hold no content).
    j(&env, &["issue", "delete", "1"]);
    j(&env, &["issue", "purge", "1"]);
    assert_eq!(env.lcl(&["issue", "history", "1"]).code, 1);
    assert_eq!(later_changes(&history(&env, "2")), []);
    let db = rusqlite::Connection::open(env.db_path()).unwrap();
    let left: i64 = db
        .query_row(
            "SELECT count(*) FROM changes WHERE new IS NOT NULL AND (instr(new, 'gone soon') > 0 OR instr(new, '\"One\"') > 0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "a purged record's values outlived it");
    let tombstones: Vec<(String, Option<i64>)> = db
        .prepare("SELECT kind, number FROM purged ORDER BY kind")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        tombstones,
        [("comment".into(), None), ("issue".into(), Some(1))]
    );
}

#[test]
fn an_import_begins_each_history_with_the_issue_as_it_was() {
    let env = workspace();
    j(
        &env,
        &["issue", "create", "--title", "Old", "--label", "bug"],
    );
    j(&env, &["issue", "update", "1", "--status", "Done"]);
    let file = env.lcl(&["export"]).assert_ok().stdout.clone();
    // The created_at an export carries, a day back.
    let file = file.replace(
        &format!(
            "\"created_at\":{}",
            j(&env, &["issue", "show", "1"])["issue"]["created_at"]
        ),
        "\"created_at\":1700000000",
    );
    let other = Env::new();
    j(&other, &["init", "w", "--key", "W"]);
    other
        .lcl_stdin(&["import", "-"], file.as_bytes())
        .assert_ok();
    let h = history(&other, "1");
    assert_eq!(h.len(), 1, "{h:#?}");
    let fields: BTreeMap<&str, &Value> = h[0]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["field"].as_str().unwrap(), &c["new"]))
        .collect();
    assert_eq!(fields["status"], &json!("Done"));
    assert_eq!(fields["created_at"], &json!(1700000000));
    assert_eq!(fields["labels"], &json!("bug"));
    let text = other
        .lcl(&["issue", "history", "1"])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        text.lines().next().unwrap().ends_with(" · first recorded"),
        "{text}"
    );
}

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

    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.next(of.len() as u64) as usize]
    }
}

/// One random write to issue `n`, as `lcl` arguments, and its kind.
fn random_write(
    rng: &mut Rng,
    n: &str,
    other: &str,
    comments: &[String],
) -> (&'static str, Vec<String>) {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let words = ["alpha", "beta", "gamma", "delta"];
    let w = rng.pick(&words);
    match rng.next(17) {
        0 => ("title", s(&["issue", "update", n, "--title", w])),
        1 => (
            "description",
            s(&[
                "issue",
                "update",
                n,
                "--description",
                &format!("{w}\nmore {w}"),
            ]),
        ),
        2 => (
            "status",
            s(&[
                "issue",
                "update",
                n,
                "--status",
                rng.pick(&["Backlog", "Todo", "In Progress", "Done", "Canceled"]),
            ]),
        ),
        3 => (
            "priority",
            s(&[
                "issue",
                "update",
                n,
                "--priority",
                rng.pick(&["0", "1", "2", "3", "4"]),
            ]),
        ),
        4 => match rng.next(2) {
            0 => ("assignee", s(&["issue", "update", n, "--assignee", w])),
            _ => ("assignee", s(&["issue", "update", n, "--no-assignee"])),
        },
        5 => match rng.next(3) {
            0 => (
                "estimate",
                s(&["issue", "update", n, "--estimate", &rng.next(8).to_string()]),
            ),
            1 => (
                "due",
                s(&[
                    "issue",
                    "update",
                    n,
                    "--due",
                    rng.pick(&["2026-12-01", "2027-01-15"]),
                ]),
            ),
            _ => (
                "clear",
                s(&[
                    "issue",
                    "update",
                    n,
                    "--no-estimate",
                    "--no-due",
                    "--no-ref",
                ]),
            ),
        },
        6 => match rng.next(2) {
            0 => ("parent", s(&["issue", "update", n, "--parent", other])),
            _ => ("parent", s(&["issue", "update", n, "--no-parent"])),
        },
        7 => match rng.next(3) {
            0 => (
                "project",
                s(&[
                    "issue",
                    "update",
                    n,
                    "--project",
                    "P1",
                    "--milestone",
                    rng.pick(&["M1", "M2"]),
                ]),
            ),
            1 => ("project", s(&["issue", "update", n, "--project", "P2"])),
            _ => ("project", s(&["issue", "update", n, "--no-project"])),
        },
        8 => match rng.next(3) {
            0 => (
                "cycle",
                s(&["issue", "update", n, "--cycle", rng.pick(&["S1", "S2"])]),
            ),
            _ => ("cycle", s(&["issue", "update", n, "--no-cycle"])),
        },
        9 => {
            let l = rng.pick(&["bug", "ui", "later"]);
            match rng.next(4) {
                0 => ("labels", s(&["issue", "update", n, "--add-label", l])),
                1 => ("labels", s(&["issue", "update", n, "--remove-label", l])),
                2 => (
                    "labels",
                    s(&["issue", "update", n, "--label", l, "--label", "later"]),
                ),
                _ => ("labels", s(&["issue", "update", n, "--clear-labels"])),
            }
        }
        10 => {
            let kind = rng.pick(&["blocks", "blocked-by", "related"]);
            match rng.next(2) {
                0 => ("relate", s(&["issue", "relate", n, kind, other])),
                _ => ("relate", s(&["issue", "unrelate", n, kind, other])),
            }
        }
        11 => (
            "duplicate",
            s(&["issue", "relate", n, "duplicate-of", other]),
        ),
        12 => match rng.next(2) {
            0 => ("trash", s(&["issue", "delete", n])),
            _ => ("trash", s(&["issue", "restore", n])),
        },
        13 => (
            "ref",
            s(&["issue", "update", n, "--ref", &format!("X-{}", rng.next(5))]),
        ),
        _ if comments.is_empty() || rng.next(3) == 0 => ("comment", s(&["issue", "comment", n, w])),
        _ => {
            let c = comments[rng.next(comments.len() as u64) as usize].clone();
            match rng.next(3) {
                0 => ("comment", s(&["comment", "edit", &c, w])),
                1 => ("comment", s(&["comment", "delete", &c])),
                _ => ("comment", s(&["comment", "restore", &c])),
            }
        }
    }
}

/// An issue rebuilt from its history alone.
#[derive(Default, Debug)]
struct Replayed {
    fields: BTreeMap<String, Value>,
    sets: BTreeMap<String, BTreeSet<String>>,
    comments: BTreeMap<String, (Value, Value)>,
}

fn replay(h: &[Value], who: &str) -> Replayed {
    let mut r = Replayed::default();
    for entry in h {
        for c in entry["changes"].as_array().unwrap() {
            let (field, old, new) = (c["field"].as_str().unwrap(), &c["old"], &c["new"]);
            if let Some(id) = c["comment_id"].as_str() {
                let comment = r
                    .comments
                    .entry(id.into())
                    .or_insert((Value::Null, Value::Null));
                let slot = if field == "body" {
                    &mut comment.0
                } else {
                    &mut comment.1
                };
                assert_eq!(
                    &*slot, old,
                    "{who}: comment {id} {field} was not what the change says"
                );
                *slot = new.clone();
                continue;
            }
            if [
                "labels",
                "cycles",
                "blocks",
                "blocked_by",
                "related",
                "duplicate_of",
                "duplicates",
            ]
            .contains(&field)
            {
                let set = r.sets.entry(field.into()).or_default();
                match (old.as_str(), new.as_str()) {
                    (None, Some(added)) => {
                        assert!(set.insert(added.into()), "{who}: {field} had {added}")
                    }
                    (Some(removed), None) => {
                        assert!(set.remove(removed), "{who}: {field} lacked {removed}")
                    }
                    other => panic!("{who}: {field} changed as {other:?}"),
                }
                continue;
            }
            let slot = r.fields.entry(field.into()).or_insert(Value::Null);
            assert_eq!(&*slot, old, "{who}: {field} was not what the change says");
            *slot = new.clone();
        }
    }
    r
}

fn names(v: &Value, key: &str) -> BTreeSet<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| match key {
                    "" => x.as_str().unwrap().to_string(),
                    k => x[k].as_str().unwrap().to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The property: replaying an issue's history from its first entry gives
/// the issue as `lcl` shows it, every field, label, cycle, relation and
/// comment, after any mix of writes. Each change's `old` is checked
/// against the replay too, so no change can go missing between two.
#[test]
fn replaying_a_history_reproduces_the_issue() {
    let mut kinds = BTreeSet::new();
    for seed in 1..=6u64 {
        let mut rng = Rng(seed);
        let env = workspace();
        for t in ["One", "Two", "Three"] {
            j(&env, &["issue", "create", "--title", t]);
        }
        let mut comments: Vec<String> = Vec::new();
        for _ in 0..40 {
            let n = rng.next(3) + 1;
            let other = (n % 3) + 1;
            let (kind, args) =
                random_write(&mut rng, &n.to_string(), &other.to_string(), &comments);
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let rows = log_rows(&env);
            let out = env.lcl(&[&argv[..], &["--json"]].concat());
            if out.code != 0 {
                assert_eq!(log_rows(&env), rows, "seed {seed}: refused {argv:?} logged");
                continue;
            }
            kinds.insert(kind);
            if argv[..2] == ["issue", "comment"] {
                comments.push(out.json()["id"].as_str().unwrap().to_string());
            }
        }
        let trashed = j(&env, &["issue", "list", "--trashed"])["issues"].clone();
        let gone: BTreeSet<String> = names(&trashed, "identifier");
        for n in 1..=3 {
            let who = format!("seed {seed}, W-{n}");
            let r = replay(&history(&env, &n.to_string()), &who);
            let id = format!("W-{n}");
            let issue = if gone.contains(&id) {
                trashed
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["identifier"] == id)
                    .unwrap()
                    .clone()
            } else {
                j(&env, &["issue", "show", &id])["issue"].clone()
            };
            let field = |k: &str| r.fields.get(k).cloned().unwrap_or(Value::Null);
            for k in [
                "title",
                "description",
                "status",
                "priority",
                "assignee",
                "project",
                "milestone",
                "ref",
                "creator",
                "estimate",
                "due_date",
                "created_at",
                "started_at",
                "completed_at",
                "canceled_at",
                "deleted_at",
            ] {
                assert_eq!(field(k), issue[k], "{who}: {k}");
            }
            assert_eq!(field("parent"), issue["parent_identifier"], "{who}: parent");
            let set = |k: &str| r.sets.get(k).cloned().unwrap_or_default();
            assert_eq!(set("labels"), names(&issue["labels"], ""), "{who}: labels");
            assert_eq!(
                set("cycles"),
                names(&issue["cycles"], "name"),
                "{who}: cycles"
            );
            if gone.contains(&id) {
                continue; // issue show refuses it; its relations and comments are hidden
            }
            let shown = j(&env, &["issue", "show", &id]);
            for (k, shown_as) in [
                ("blocks", "blocks"),
                ("blocked_by", "blocked_by"),
                ("related", "related"),
                ("duplicates", "duplicates"),
            ] {
                // issue show leaves out what is in the trash.
                let live: BTreeSet<String> =
                    set(k).into_iter().filter(|i| !gone.contains(i)).collect();
                assert_eq!(
                    live,
                    names(&shown["relations"][shown_as], "identifier"),
                    "{who}: {k}"
                );
            }
            let dup: BTreeSet<String> = set("duplicate_of")
                .into_iter()
                .filter(|i| !gone.contains(i))
                .collect();
            let shown_dup: BTreeSet<String> = shown["relations"]["duplicate_of"]["identifier"]
                .as_str()
                .map(|s| BTreeSet::from([s.to_string()]))
                .unwrap_or_default();
            assert_eq!(dup, shown_dup, "{who}: duplicate_of");
            let mut live_comments = BTreeMap::new();
            let mut trashed_comments = BTreeMap::new();
            for (cid, (body, deleted)) in &r.comments {
                if deleted.is_null() {
                    live_comments.insert(cid.clone(), body.clone());
                } else {
                    trashed_comments.insert(cid.clone(), body.clone());
                }
            }
            let listed = |trashed: bool| -> BTreeMap<String, Value> {
                let mut args = vec!["issue", "comments", id.as_str()];
                if trashed {
                    args.push("--trashed");
                }
                j(&env, &args)
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| (c["id"].as_str().unwrap().to_string(), c["body"].clone()))
                    .collect()
            };
            assert_eq!(live_comments, listed(false), "{who}: comments");
            assert_eq!(trashed_comments, listed(true), "{who}: trashed comments");
        }
    }
    assert_eq!(
        kinds,
        BTreeSet::from([
            "assignee",
            "clear",
            "comment",
            "cycle",
            "description",
            "due",
            "duplicate",
            "estimate",
            "labels",
            "parent",
            "priority",
            "project",
            "ref",
            "relate",
            "status",
            "title",
            "trash",
        ]),
        "a kind of write the seeds never made"
    );
}
