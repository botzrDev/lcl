//! `lcl export` and `lcl import`: a workspace as a text file (format
//! lcl-export/1), and back, exactly. dreamd's Linear tickets come in through
//! the same format from a converter outside lcl, so import is held to every
//! rule the commands apply, and to all-or-nothing.

mod common;

use common::{
    Env, FTS_TABLES, Out, V0_FIXTURE, argv, dump, exec, foreign_key_violations, fts_integrity,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Barrier;
use std::time::{Duration, Instant};

/// An empty workspace with key DRM.
fn drm() -> Env {
    let env = Env::new();
    env.lcl(&["init", "drm", "--key", "DRM"]).assert_ok();
    env
}

fn export(env: &Env) -> String {
    let out = env.lcl(&["export"]);
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    out.stdout.clone()
}

fn import(env: &Env, text: &str) -> Out {
    env.lcl_stdin(&["import", "-"], text.as_bytes())
}

fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

/// The file's lines as JSON.
fn records(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn of_type<'a>(recs: &'a [Value], t: &str) -> Vec<&'a Value> {
    recs.iter().filter(|r| r["type"] == t).collect()
}

/// The keys of a raw JSON line, in the order they are written.
fn key_order(line: &str) -> Vec<String> {
    let mut keys = vec![];
    let mut de = serde_json::Deserializer::from_str(line);
    use serde::de::{Deserializer, MapAccess, Visitor};
    struct Keys<'a>(&'a mut Vec<String>);
    impl<'de> Visitor<'de> for Keys<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while let Some((k, _)) = map.next_entry::<String, serde::de::IgnoredAny>()? {
                self.0.push(k);
            }
            Ok(())
        }
    }
    de.deserialize_map(Keys(&mut keys)).expect("an object");
    keys
}

fn id_of(env: &Env, n: i64) -> String {
    env.lcl(&["issue", "show", &n.to_string(), "--json"])
        .assert_ok()
        .json()["issue"]["id"]
        .as_str()
        .expect("id")
        .to_string()
}

/// A workspace using every feature lcl has.
fn everything() -> Env {
    let env = drm();
    let ok = |args: &[&str]| {
        env.lcl(args).assert_ok();
    };
    ok(&["label", "create", "bug", "--color", "#f00"]);
    ok(&["label", "create", "Feature"]);
    ok(&["label", "create", "ux", "--color", "#00aa11"]);
    ok(&[
        "project",
        "create",
        "dreamd-hardening",
        "--status",
        "in progress",
        "--target-date",
        "2026-12-01",
        "--description",
        "harden it",
    ]);
    ok(&["project", "create", "alpha"]);
    ok(&[
        "milestone",
        "create",
        "1.3.1 patch",
        "--project",
        "dreamd-hardening",
        "--target-date",
        "2026-11-30",
    ]);
    ok(&["milestone", "create", "beta", "--project", "alpha"]);
    ok(&[
        "cycle",
        "create",
        "Sprint 1",
        "--start",
        "2026-11-01",
        "--end",
        "2026-11-14",
    ]);
    let text = "line one\nline two \"quoted\" and 'single'\ncafé ✓ 日本語\ttab";
    ok(&[
        "issue",
        "create",
        "--title",
        "root ✓",
        "--description",
        text,
        "--label",
        "bug",
        "--label",
        "Feature",
        "--project",
        "dreamd-hardening",
        "--milestone",
        "1.3.1 patch",
        "--ref",
        "BZR-1532",
        "--author",
        "Ana María",
    ]);
    ok(&[
        "issue",
        "create",
        "--title",
        "child",
        "--parent",
        "1",
        "--status",
        "In Progress",
        "--priority",
        "high",
        "--assignee",
        "alice",
        "--estimate",
        "3",
        "--due",
        "2026-12-24",
        "--author",
        "bob",
    ]);
    ok(&[
        "issue",
        "create",
        "--title",
        "grandchild",
        "--parent",
        "2",
        "--status",
        "in review",
        "--priority",
        "urgent",
        "--ref",
        "BZR-7",
    ]);
    ok(&[
        "issue", "create", "--title", "todo", "--status", "todo", "--label", "ux",
    ]);
    ok(&[
        "issue",
        "create",
        "--title",
        "done",
        "--status",
        "done",
        "--project",
        "alpha",
        "--milestone",
        "beta",
    ]);
    ok(&[
        "issue",
        "create",
        "--title",
        "canceled",
        "--status",
        "canceled",
        "--priority",
        "low",
    ]);
    ok(&["issue", "create", "--title", "dup", "--priority", "medium"]);
    ok(&["issue", "create", "--title", "plain"]);
    // One issue with no creator.
    env.lcl_with(&["issue", "create", "--title", "nobody"], |c| {
        c.env_remove("USER");
    })
    .assert_ok();
    ok(&["issue", "relate", "1", "blocks", "2"]);
    ok(&["issue", "relate", "2", "blocks", "3"]);
    ok(&["issue", "relate", "7", "duplicate-of", "1"]);
    // related: a pair whose stored order (by UUID) is the reverse of their
    // numbers, so writing it smaller-number-first is a real choice.
    let ids: Vec<String> = (1..=9).map(|n| id_of(&env, n)).collect();
    // (4, 5) is related below already. Nine random UUIDs are all in order
    // once in 9! runs.
    let (a, b) = (1..=9)
        .flat_map(|a| (a + 1..=9).map(move |b| (a, b)))
        .find(|&(a, b)| (a, b) != (4, 5) && ids[a - 1] > ids[b - 1])
        .expect("a pair with UUIDs in reverse order");
    ok(&["issue", "relate", &a.to_string(), "related", &b.to_string()]);
    ok(&["issue", "relate", "4", "related", "5"]);
    // Comments: issue 2's is older than issue 1's three, which share a second.
    for body in ["c1", "c2", "c3"] {
        ok(&["issue", "comment", "1", body, "--author", "carol"]);
    }
    ok(&["issue", "comment", "2", "only a zebra here"]);
    let db = env.db_path();
    exec(
        &db,
        &format!(
            "UPDATE comments SET created_at = 2000 WHERE issue_id = '{}'",
            ids[0]
        ),
    );
    exec(
        &db,
        &format!(
            "UPDATE comments SET created_at = 1000 WHERE issue_id = '{}'",
            ids[1]
        ),
    );
    // Issues in the cycle: no command does this yet.
    let cycle = env.lcl(&["cycle", "list", "--json"]).assert_ok().json()[0]["id"]
        .as_str()
        .expect("cycle id")
        .to_string();
    exec(
        &db,
        &format!(
            "INSERT INTO issue_cycles(issue_id, cycle_id) VALUES ('{}', '{cycle}'), ('{}', '{cycle}')",
            ids[0], ids[3]
        ),
    );
    // A doc tree, three deep.
    let doc = |args: &[&str]| {
        env.lcl(args).assert_ok().json()["id"]
            .as_str()
            .expect("doc id")
            .to_string()
    };
    let top = doc(&[
        "--json",
        "doc",
        "create",
        "--title",
        "Handbook",
        "--content",
        "# Handbook\n\nwelcome",
    ]);
    let chapter = doc(&[
        "--json", "doc", "create", "--title", "Chapter", "--parent", &top,
    ]);
    doc(&[
        "--json",
        "doc",
        "create",
        "--title",
        "Section",
        "--parent",
        &chapter,
        "--content",
        "deep",
    ]);
    env
}

#[test]
fn a_workspace_round_trips_byte_for_byte() {
    let a = everything();
    let first = export(&a);
    assert_eq!(
        first,
        export(&a),
        "two exports with no change between them differ"
    );
    let recs = records(&first);

    // The order and shape of the file.
    let types: Vec<&str> = recs
        .iter()
        .map(|r| r["type"].as_str().expect("type"))
        .collect();
    let mut sorted = types.clone();
    let rank = |t: &str| {
        [
            "workspace",
            "label",
            "project",
            "milestone",
            "cycle",
            "issue",
            "comment",
            "relation",
            "doc",
        ]
        .iter()
        .position(|x| *x == t)
        .expect("known type")
    };
    sorted.sort_by_key(|t| rank(t));
    assert_eq!(types, sorted, "records are grouped in the documented order");
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(
        key_order(lines[0]),
        [
            "type",
            "format",
            "name",
            "key",
            "description",
            "created_at",
            "next_issue_number"
        ]
    );
    let issue_line = lines
        .iter()
        .find(|l| l.contains(r#""type":"issue""#))
        .expect("an issue");
    assert_eq!(
        key_order(issue_line),
        [
            "type",
            "number",
            "title",
            "description",
            "status",
            "priority",
            "assignee",
            "creator",
            "ref",
            "parent",
            "project",
            "milestone",
            "labels",
            "cycles",
            "estimate",
            "due_date",
            "created_at",
            "updated_at",
            "started_at",
            "completed_at",
            "canceled_at"
        ]
    );
    assert_eq!(
        of_type(&recs, "label")
            .iter()
            .map(|l| l["name"].clone())
            .collect::<Vec<_>>(),
        [json!("bug"), json!("Feature"), json!("ux")]
    );
    assert_eq!(
        of_type(&recs, "project")
            .iter()
            .map(|p| p["name"].clone())
            .collect::<Vec<_>>(),
        [json!("alpha"), json!("dreamd-hardening")]
    );
    let numbers: Vec<i64> = of_type(&recs, "issue")
        .iter()
        .map(|i| i["number"].as_i64().expect("number"))
        .collect();
    assert_eq!(numbers, (1..=9).collect::<Vec<_>>(), "issues by number");
    let bodies: Vec<&str> = of_type(&recs, "comment")
        .iter()
        .map(|c| c["body"].as_str().expect("body"))
        .collect();
    assert_eq!(
        bodies,
        ["c1", "c2", "c3", "only a zebra here"],
        "comments by issue number, then time, then the order added"
    );
    for r in of_type(&recs, "relation") {
        if r["kind"] == "related" {
            assert!(
                r["from"].as_i64() < r["to"].as_i64(),
                "related is written smaller number first: {r}"
            );
        }
    }
    let one = &of_type(&recs, "issue")[0];
    assert_eq!(
        (
            &one["labels"],
            &one["cycles"].as_array().map(Vec::len),
            &one["ref"],
            &one["creator"]
        ),
        (
            &json!(["bug", "Feature"]),
            &Some(1),
            &json!("BZR-1532"),
            &json!("Ana María")
        )
    );
    let statuses: Vec<&str> = of_type(&recs, "issue")
        .iter()
        .map(|i| i["status"].as_str().expect("status"))
        .collect();
    assert_eq!(
        statuses,
        [
            "Backlog",
            "InProgress",
            "InReview",
            "Todo",
            "Done",
            "Canceled",
            "Duplicate",
            "Backlog",
            "Backlog"
        ]
    );

    // Import into a new workspace with the same key, and export again.
    let b = drm();
    let out = import(&b, &first);
    assert_eq!(
        trio(&out),
        (
            0,
            "Imported 9 issues, 4 comments, 5 relations, 3 labels, 2 projects, 2 milestones, 1 cycle, 3 docs into workspace drm (key DRM); next number DRM-10\n",
            ""
        ),
        "{out:#?}"
    );
    let second = export(&b);
    assert!(
        first == second,
        "the round trip changed the file:\n--- first\n{first}\n--- second\n{second}"
    );
    let db = b.db_path();
    assert_eq!(foreign_key_violations(&db), vec![]);
    fts_integrity(&db, &FTS_TABLES);
    let found = b.lcl(&["search", "zebra", "--json"]).assert_ok().json();
    let hits: Vec<_> = found["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|h| (h["identifier"].clone(), h["matched_in"].clone()))
        .collect();
    assert_eq!(hits, [(json!("DRM-2"), json!(["comments"]))]);
    // What the commands show agrees too.
    for n in 1..=9 {
        let show = |env: &Env| {
            let mut v = env
                .lcl(&["issue", "show", &n.to_string(), "--json"])
                .assert_ok()
                .json();
            for key in ["id", "parent_id"] {
                v["issue"][key] = Value::Null;
            }
            for c in v["comments"].as_array_mut().expect("comments") {
                c["id"] = Value::Null;
                c["issue_id"] = Value::Null;
            }
            v
        };
        assert_eq!(show(&b), show(&a), "issue {n}");
    }
    assert_eq!(
        b.lcl(&["doc", "list", "--json"])
            .assert_ok()
            .json()
            .as_array()
            .map(Vec::len),
        Some(3)
    );
}

#[test]
fn import_needs_an_empty_workspace_with_the_files_key() {
    let file = export(&everything());
    for (setup, has) in [
        (&["issue", "create", "--title", "x"][..], "1 issue"),
        (&["label", "create", "only"][..], "1 label"),
    ] {
        let env = drm();
        env.lcl(setup).assert_ok();
        let before = dump(&env.db_path());
        let want = format!(
            "Error: workspace \"drm\" is not empty: it has {has}; import only into an empty workspace (make one with lcl init or lcl workspace create)\n"
        );
        assert_eq!(trio(&import(&env, &file)), (1, "", want.as_str()));
        assert_eq!(
            trio(&env.lcl_stdin(&["import", "-", "--dry-run"], file.as_bytes())),
            (1, "", want.as_str())
        );
        assert!(
            dump(&env.db_path()) == before,
            "a refused import changed the workspace"
        );
    }
    let other = Env::new();
    other.lcl(&["init", "bzr", "--key", "BZR"]).assert_ok();
    let before = dump(&other.db_path());
    let want = "Error: the file is for key DRM but this workspace's key is BZR; import it into a workspace with key DRM\n";
    assert_eq!(trio(&import(&other, &file)), (1, "", want));
    assert!(dump(&other.db_path()) == before);
    // The key matches ignoring ASCII case.
    let lower = file.replacen(r#""key":"DRM""#, r#""key":"drm""#, 1);
    import(&drm(), &lower).assert_ok();
}

/// A valid file (it imports) to break one line of at a time. Line numbers
/// are in the comments.
fn base() -> Vec<String> {
    [
        r#"{"type":"workspace","format":"lcl-export/1","key":"DRM"}"#, // 1
        r#"{"type":"label","name":"bug"}"#,                            // 2
        r##"{"type":"label","name":"feature","color":"#0a0"}"##,       // 3
        r#"{"type":"project","name":"alpha"}"#,                        // 4
        r#"{"type":"project","name":"beta","status":"Planned"}"#,      // 5
        r#"{"type":"milestone","project":"alpha","name":"m1"}"#,       // 6
        r#"{"type":"milestone","project":"beta","name":"m2"}"#,        // 7
        r#"{"type":"cycle","id":"0f8fad5b-d9cb-469f-a165-70867728950e","name":"Sprint"}"#, // 8
        r#"{"type":"doc","id":"11111111-1111-4111-8111-111111111111","title":"top"}"#, // 9
        r#"{"type":"doc","id":"22222222-2222-4222-8222-222222222222","title":"child","parent":"11111111-1111-4111-8111-111111111111"}"#, // 10
        r#"{"type":"issue","number":1,"title":"one","labels":["bug"],"project":"alpha","milestone":"m1","cycles":["0f8fad5b-d9cb-469f-a165-70867728950e"],"ref":"BZR-1"}"#, // 11
        r#"{"type":"issue","number":2,"title":"two","parent":1,"status":"Todo","ref":"BZR-2"}"#, // 12
        r#"{"type":"issue","number":3,"title":"three","parent":2}"#, // 13
        r#"{"type":"comment","issue":1,"body":"hi"}"#,               // 14
        r#"{"type":"relation","kind":"blocks","from":1,"to":2}"#,    // 15
        r#"{"type":"relation","kind":"blocks","from":2,"to":3}"#,    // 16
        r#"{"type":"relation","kind":"related","from":1,"to":3}"#,   // 17
        r#"{"type":"relation","kind":"duplicate","from":3,"to":1}"#, // 18
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn join(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

#[test]
fn a_bad_line_exits_2_naming_it_and_writes_nothing() {
    let env = drm();
    let db = env.db_path();
    let before = dump(&db);
    let set = |n: usize, line: &str| -> Vec<String> {
        let mut l = base();
        l[n - 1] = line.to_string();
        l
    };
    let add = |line: &str| -> Vec<String> {
        let mut l = base();
        l.push(line.to_string());
        l
    };
    let doc1 = "11111111-1111-4111-8111-111111111111";
    let doc2 = "22222222-2222-4222-8222-222222222222";
    let cases: Vec<(&str, Vec<String>, String)> = vec![
        ("an unknown status, mid-file", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"status":"Finished","ref":"BZR-2"}"#),
            r#"line 12: status: invalid status "Finished"; expected one of: Backlog, Todo, InProgress, InReview, Done, Canceled, Duplicate"#.into()),
        ("an unknown priority", set(13, r#"{"type":"issue","number":3,"title":"three","parent":2,"priority":"Highest"}"#),
            r#"line 13: priority: invalid priority "Highest"; expected 0-4 or one of: none, urgent, high, medium, low"#.into()),
        ("an unknown parent number", set(13, r#"{"type":"issue","number":3,"title":"three","parent":9}"#),
            "line 13: parent: there is no issue DRM-9 in the file".into()),
        ("an issue its own parent", set(13, r#"{"type":"issue","number":3,"title":"three","parent":3}"#),
            "line 13: parent: DRM-3 can't be its own parent".into()),
        ("a parent cycle", set(11, r#"{"type":"issue","number":1,"title":"one","parent":3}"#),
            "line 11: parent: making DRM-3 the parent of DRM-1 would make a cycle: DRM-1 -> DRM-3 -> DRM-2 -> DRM-1".into()),
        ("a comment on a number not in the file", set(14, r#"{"type":"comment","issue":9,"body":"hi"}"#),
            "line 14: issue: there is no issue DRM-9 in the file".into()),
        ("a relation to a number not in the file", set(15, r#"{"type":"relation","kind":"blocks","from":1,"to":9}"#),
            "line 15: to: there is no issue DRM-9 in the file".into()),
        ("a self relation", set(17, r#"{"type":"relation","kind":"related","from":1,"to":1}"#),
            "line 17: relation: DRM-1 can't be related to itself".into()),
        ("a blocks cycle", add(r#"{"type":"relation","kind":"blocks","from":3,"to":1}"#),
            "line 19: relation: DRM-3 blocking DRM-1 would make a cycle: DRM-3 blocks DRM-1 blocks DRM-2 blocks DRM-3".into()),
        ("a second duplicate target", add(r#"{"type":"relation","kind":"duplicate","from":3,"to":2}"#),
            "line 19: relation: DRM-3 is already a duplicate of DRM-1 (line 18)".into()),
        ("a duplicate cycle", add(r#"{"type":"relation","kind":"duplicate","from":1,"to":3}"#),
            "line 19: relation: making DRM-1 a duplicate of DRM-3 would make a cycle: DRM-1 -> DRM-3 -> DRM-1".into()),
        ("the same relation twice", add(r#"{"type":"relation","kind":"blocks","from":1,"to":2}"#),
            "line 19: relation: DRM-1 blocks DRM-2 is already on line 15".into()),
        ("related twice, the other way round", add(r#"{"type":"relation","kind":"related","from":3,"to":1}"#),
            "line 19: relation: DRM-3 related DRM-1 is already on line 17".into()),
        ("a relation kind lcl doesn't store", set(15, r#"{"type":"relation","kind":"blocked-by","from":1,"to":2}"#),
            r#"line 15: kind: invalid relation kind "blocked-by"; expected one of: blocks, related, duplicate"#.into()),
        ("a milestone outside the issue's project", set(11, r#"{"type":"issue","number":1,"title":"one","project":"alpha","milestone":"m2"}"#),
            r#"line 11: milestone: milestone "m2" is not in project "alpha" (it is in "beta")"#.into()),
        ("a milestone with no project", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"milestone":"m1"}"#),
            r#"line 12: milestone: milestone "m1" needs a project: the issue has none"#.into()),
        ("an unknown label", set(11, r#"{"type":"issue","number":1,"title":"one","labels":["nope"]}"#),
            r#"line 11: labels: no label named "nope" in the file"#.into()),
        ("a label listed twice on an issue", set(11, r#"{"type":"issue","number":1,"title":"one","labels":["bug","BUG"]}"#),
            r#"line 11: labels: label "BUG" is listed twice (ignoring case)"#.into()),
        ("an unknown project", set(11, r#"{"type":"issue","number":1,"title":"one","project":"gamma"}"#),
            r#"line 11: project: no project named "gamma" in the file"#.into()),
        ("an unknown milestone", set(11, r#"{"type":"issue","number":1,"title":"one","project":"alpha","milestone":"m9"}"#),
            r#"line 11: milestone: no milestone named "m9" in project "alpha" in the file"#.into()),
        ("a milestone in an unknown project", set(7, r#"{"type":"milestone","project":"gamma","name":"m2"}"#),
            r#"line 7: project: no project named "gamma" in the file"#.into()),
        ("an unknown cycle", set(11, r#"{"type":"issue","number":1,"title":"one","cycles":["33333333-3333-4333-8333-333333333333"]}"#),
            "line 11: cycles: no cycle with id 33333333-3333-4333-8333-333333333333 in the file".into()),
        ("two refs equal ignoring case", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"ref":"bzr-1"}"#),
            r#"line 12: ref: ref "bzr-1" is already on DRM-1"#.into()),
        ("a ref shaped like this workspace's own", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"ref":"drm-9"}"#),
            r#"line 12: ref: ref "drm-9" looks like one of this workspace's own issue references"#.into()),
        ("a bare-number ref", set(12, r##"{"type":"issue","number":2,"title":"two","parent":1,"ref":"#12"}"##),
            r##"line 12: ref: ref "#12" looks like one of this workspace's own issue references"##.into()),
        ("two equal numbers", set(13, r#"{"type":"issue","number":2,"title":"three"}"#),
            "line 13: number: DRM-2 is already on line 12".into()),
        ("two labels with one name, ignoring case", set(3, r#"{"type":"label","name":"BUG"}"#),
            r#"line 3: name: label "BUG" is already on line 2 as "bug"; label names are unique, ignoring case"#.into()),
        ("two projects with one name, ignoring case", set(5, r#"{"type":"project","name":"Alpha"}"#),
            r#"line 5: name: project "Alpha" is already on line 4 as "alpha"; project names are unique, ignoring case"#.into()),
        ("two milestones with one name in a project", set(7, r#"{"type":"milestone","project":"alpha","name":"M1"}"#),
            r#"line 7: name: milestone "M1" is already on line 6 as "m1"; milestone names are unique in a project, ignoring case"#.into()),
        ("a doc parent cycle", set(9, &format!(r#"{{"type":"doc","id":"{doc1}","title":"top","parent":"{doc2}"}}"#)),
            format!("line 9: parent: doc parents make a cycle: {doc1} -> {doc2} -> {doc1}")),
        ("a doc parent not in the file", set(10, &format!(r#"{{"type":"doc","id":"{doc2}","title":"child","parent":"33333333-3333-4333-8333-333333333333"}}"#)),
            "line 10: parent: no doc with id 33333333-3333-4333-8333-333333333333 in the file".into()),
        ("a doc id that isn't a UUID", set(9, r#"{"type":"doc","id":"top","title":"top"}"#),
            r#"line 9: id: invalid id "top"; expected a UUID"#.into()),
        ("an unknown key", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"titel":"x"}"#),
            r#"line 12: issue: unknown key "titel"; an issue record has: type, number, title, description, status, priority, assignee, creator, ref, parent, project, milestone, labels, cycles, estimate, due_date, created_at, updated_at, started_at, completed_at, canceled_at"#.into()),
        ("an unknown type", set(14, r#"{"type":"attachment","issue":1}"#),
            r#"line 14: type: unknown record type "attachment"; expected one of: workspace, label, project, milestone, cycle, issue, comment, relation, doc"#.into()),
        ("no header", base()[1..].to_vec(),
            r#"line 1: type: the first line must be the workspace header (type "workspace"), not "label""#.into()),
        ("the header second", { let mut l = base(); l.swap(0, 1); l },
            r#"line 1: type: the first line must be the workspace header (type "workspace"), not "label""#.into()),
        ("a second header", add(r#"{"type":"workspace","format":"lcl-export/1","key":"DRM"}"#),
            "line 19: type: the workspace header can only be the first line".into()),
        ("a line that isn't JSON", set(14, r#"{"type":"comment","#),
            "line 14: line: not JSON at column 18: EOF while parsing a value".into()),
        ("a line that isn't an object", set(14, r#"["comment"]"#),
            "line 14: line: expected a JSON object, got a list".into()),
        ("a blank line", set(14, ""),
            "line 14: line: not JSON: the line is blank".into()),
        ("format lcl-export/2", set(1, r#"{"type":"workspace","format":"lcl-export/2","key":"DRM"}"#),
            r#"line 1: format: unsupported format "lcl-export/2"; this lcl reads lcl-export/1"#.into()),
        ("no format", set(1, r#"{"type":"workspace","key":"DRM"}"#),
            r#"line 1: format: missing; expected "lcl-export/1""#.into()),
        ("a null where only the default is allowed", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"status":null}"#),
            "line 12: status: can't be null; leave the key out for the default (Backlog)".into()),
        ("a missing title", set(13, r#"{"type":"issue","number":3,"parent":2}"#),
            "line 13: title: missing; every issue record needs one".into()),
        ("a name with padding", set(2, r#"{"type":"label","name":" bug"}"#),
            r#"line 2: name: " bug" has leading or trailing whitespace; lcl stores it as "bug""#.into()),
        ("an assignee with padding", set(13, r#"{"type":"issue","number":3,"title":"three","parent":2,"assignee":"alice "}"#),
            r#"line 13: assignee: "alice " has leading or trailing whitespace; lcl stores it as "alice""#.into()),
        ("a ref with padding", set(12, r#"{"type":"issue","number":2,"title":"two","parent":1,"ref":" BZR-2"}"#),
            r#"line 12: ref: " BZR-2" has leading or trailing whitespace; lcl stores it as "BZR-2""#.into()),
        ("a creator with padding", set(13, r#"{"type":"issue","number":3,"title":"three","parent":2,"creator":"bob "}"#),
            r#"line 13: creator: "bob " has leading or trailing whitespace; lcl stores it as "bob""#.into()),
        ("a blank milestone name", set(6, r#"{"type":"milestone","project":"alpha","name":""}"#),
            "line 6: name: a milestone name can't be empty".into()),
        ("a blank author", set(14, r#"{"type":"comment","issue":1,"body":"hi","author":""}"#),
            "line 14: author: an author can't be empty; leave --author out to use LCL_AUTHOR or USER".into()),
        ("a bad colour", set(3, r##"{"type":"label","name":"feature","color":"red"}"##),
            r#"line 3: color: invalid color "red"; expected #rgb or #rrggbb"#.into()),
        ("an impossible date", set(13, r#"{"type":"issue","number":3,"title":"three","parent":2,"due_date":"2026-02-30"}"#),
            r#"line 13: due_date: invalid date "2026-02-30"; expected a real calendar date as YYYY-MM-DD"#.into()),
        ("a negative estimate", set(13, r#"{"type":"issue","number":3,"title":"three","parent":2,"estimate":-3}"#),
            "line 13: estimate: an estimate can't be negative; got -3".into()),
        ("a number that isn't whole", set(13, r#"{"type":"issue","number":3.5,"title":"three"}"#),
            "line 13: number: expected a whole number that fits in 64 bits, got 3.5".into()),
        ("a string where a number goes", set(14, r#"{"type":"comment","issue":"1","body":"hi"}"#),
            "line 14: issue: expected a whole number, got a string".into()),
    ];
    for (name, lines, want) in &cases {
        let text = join(lines);
        let real = import(&env, &text);
        assert_eq!(
            trio(&real),
            (2, "", format!("Error: {want}\n").as_str()),
            "{name}"
        );
        let dry = env.lcl_stdin(&["import", "-", "--dry-run"], text.as_bytes());
        assert_eq!(
            trio(&dry),
            trio(&real),
            "{name}: --dry-run gives the real run's error"
        );
        assert!(
            dump(&db) == before,
            "{name}: a refused file changed the workspace"
        );
    }
    // Bytes that aren't UTF-8, and an empty file.
    let mut bytes = join(&base()).into_bytes();
    let at = bytes.len() - 3;
    bytes[at] = 0xff;
    let out = env.lcl_stdin(&["import", "-"], &bytes);
    assert_eq!(
        trio(&out),
        (
            2,
            "",
            "Error: line 18: line: not UTF-8 (invalid byte at column 53)\n"
        )
    );
    assert_eq!(
        trio(&env.lcl_stdin(&["import", "-"], b"")),
        (
            2,
            "",
            "Error: line 1: workspace: the file is empty; the first line must be the workspace header\n"
        )
    );
    assert!(dump(&db) == before);
    // The base file itself imports, without its last newline too.
    let text = join(&base());
    let out = import(&env, text.trim_end_matches('\n'));
    assert_eq!(
        trio(&out),
        (
            0,
            "Imported 3 issues, 1 comment, 4 relations, 2 labels, 2 projects, 2 milestones, 1 cycle, 2 docs into workspace drm (key DRM); next number DRM-4\n",
            ""
        )
    );
}

#[test]
fn numbers_are_kept_and_the_counter_never_goes_down() {
    let file = |numbers: &[i64], next: Option<i64>| {
        let mut text = match next {
            Some(n) => format!(
                r#"{{"type":"workspace","format":"lcl-export/1","key":"DRM","next_issue_number":{n}}}"#
            ),
            None => r#"{"type":"workspace","format":"lcl-export/1","key":"DRM"}"#.to_string(),
        };
        text.push('\n');
        for n in numbers {
            text.push_str(&format!(
                "{{\"type\":\"issue\",\"number\":{n},\"title\":\"t{n}\"}}\n"
            ));
        }
        text
    };
    let next_after = |env: &Env| env.create("next");

    let gaps = drm();
    import(&gaps, &file(&[1, 2, 7, 1532], None)).assert_ok();
    let listed: Vec<i64> = gaps.lcl(&["issue", "list", "--json"]).assert_ok().json()["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .map(|i| i["number"].as_i64().expect("n"))
        .collect();
    assert_eq!(listed, [1532, 7, 2, 1]);
    assert_eq!(next_after(&gaps), 1533);

    let header_wins = drm();
    let out = import(&header_wins, &file(&[1, 2, 7, 1532], Some(2000)));
    assert!(out.stdout.ends_with("next number DRM-2000\n"), "{out:#?}");
    assert_eq!(next_after(&header_wins), 2000);

    // A counter of 3000, as 2999 deleted issues would leave it: an import
    // never lowers it.
    let high = drm();
    high.create("gone");
    // Gone for good, as a purge leaves it: an issue in the trash would
    // still make the workspace not empty.
    exec(
        &high.db_path(),
        "DELETE FROM issues; UPDATE meta SET value = '3000' WHERE key = 'next_issue_number'",
    );
    let out = import(&high, &file(&[1, 2, 3, 4], Some(5)));
    assert_eq!(
        out.assert_ok()
            .stdout
            .lines()
            .last()
            .map(|l| l.ends_with("next number DRM-3000")),
        Some(true),
        "{out:#?}"
    );
    assert_eq!(next_after(&high), 3000);
}

/// The harness's blocker B1, exactly: a renumbered Linear dump, each issue
/// keeping its Linear id as its ref.
#[test]
fn the_converter_shape_imports_and_finds_issues_by_ref() {
    let env = Env::new();
    env.lcl(&["workspace", "create", "imp", "--key", "DRM"])
        .assert_ok();
    let mut lines = vec![
        json!({"type": "workspace", "format": "lcl-export/1", "name": "imp-src", "key": "DRM", "created_at": 1700000000, "next_issue_number": 5}),
    ];
    for (n, r) in [
        (1, "BZR-146"),
        (2, "BZR-1230"),
        (3, "BZR-1532"),
        (4, "BZR-1535"),
    ] {
        lines.push(json!({"type": "issue", "number": n, "ref": r, "title": format!("t{n}"), "description": "", "status": "Done", "priority": "None",
            "created_at": 1700000000 + n, "updated_at": 1700000100 + n, "completed_at": 1700000200 + n}));
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    // LCL_AUTHOR and USER never fill in a creator the file leaves out.
    let out = env.lcl_stdin_with(&["-w", "imp", "import", "-"], text.as_bytes(), |c| {
        c.env("LCL_AUTHOR", "bob").env("USER", "carol");
    });
    assert_eq!(
        trio(&out),
        (
            0,
            "Imported 4 issues, 0 comments, 0 relations, 0 labels, 0 projects, 0 milestones, 0 cycles, 0 docs into workspace imp (key DRM); next number DRM-5\n",
            ""
        )
    );
    let all = env
        .lcl(&["-w", "imp", "issue", "list", "--all", "--json"])
        .assert_ok()
        .json();
    let got: Vec<_> = all["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .rev()
        .map(|i| {
            (
                i["number"].clone(),
                i["ref"].clone(),
                i["status"].clone(),
                i["created_at"].clone(),
                i["updated_at"].clone(),
                i["completed_at"].clone(),
                i["started_at"].clone(),
                i["canceled_at"].clone(),
                i["creator"].clone(),
            )
        })
        .collect();
    let want: Vec<_> = [
        (1, "BZR-146"),
        (2, "BZR-1230"),
        (3, "BZR-1532"),
        (4, "BZR-1535"),
    ]
    .iter()
    .map(|(n, r)| {
        (
            json!(n),
            json!(r),
            json!("Done"),
            json!(1700000000 + n),
            json!(1700000100 + n),
            json!(1700000200 + n),
            Value::Null,
            Value::Null,
            Value::Null,
        )
    })
    .collect();
    assert_eq!(got, want);
    let hits = env
        .lcl(&["-w", "imp", "search", "BZR-1532", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &hits["issues"][0]["identifier"],
            &hits["issues"][0]["matched_in"]
        ),
        (&json!("DRM-3"), &json!(["ref"]))
    );
    let shown = env.lcl(&["-w", "imp", "issue", "show", "BZR-1532"]);
    assert_eq!(
        trio(&shown),
        (
            2,
            "",
            "Error: BZR-1532 is not in this workspace (key DRM); DRM-3 has ref BZR-1532\n"
        )
    );
    assert_eq!(
        env.lcl(&["-w", "imp", "issue", "create", "--title", "next", "--json"])
            .assert_ok()
            .json()["identifier"],
        "DRM-5"
    );
    // The workspace keeps its name; created_at comes from the file.
    let ws = env
        .lcl(&["-w", "imp", "workspace", "show", "--json"])
        .assert_ok()
        .json();
    assert_eq!(
        (&ws["name"], &ws["created_at"]),
        (&json!("imp"), &json!(1700000000))
    );
}

/// No lifecycle rule runs, and nothing comes from the environment.
#[test]
fn import_stores_what_the_file_says() {
    let env = drm();
    let text = [
        r#"{"type":"workspace","format":"lcl-export/1","key":"DRM","description":"from the file"}"#,
        r#"{"type":"issue","number":1,"title":"done, never completed","status":"Done"}"#,
        r#"{"type":"issue","number":2,"title":"backlog with stale times","status":"Backlog","completed_at":5,"started_at":4,"canceled_at":6}"#,
        r#"{"type":"issue","number":3,"title":"in progress, never started","status":"InProgress"}"#,
        r#"{"type":"issue","number":4,"title":"a duplicate that isn't marked one","status":"Todo"}"#,
        r#"{"type":"relation","kind":"duplicate","from":4,"to":1}"#,
        r#"{"type":"comment","issue":1,"body":"no author"}"#,
        r#"{"type":"comment","issue":1,"body":"same second, second","created_at":100}"#,
    ]
    .map(|l| format!("{l}\n"))
    .concat();
    let out = env.lcl_stdin_with(&["import", "-"], text.as_bytes(), |c| {
        c.env("LCL_AUTHOR", "bob").env("USER", "carol");
    });
    assert_eq!((out.code, out.stderr.as_str()), (0, ""), "{out:#?}");
    let row = |n: i64| {
        let i = env
            .lcl(&["issue", "show", &n.to_string(), "--json"])
            .assert_ok()
            .json()["issue"]
            .clone();
        (
            i["status"].clone(),
            i["started_at"].clone(),
            i["completed_at"].clone(),
            i["canceled_at"].clone(),
            i["creator"].clone(),
        )
    };
    assert_eq!(
        row(1),
        (
            json!("Done"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null
        )
    );
    assert_eq!(
        row(2),
        (json!("Backlog"), json!(4), json!(5), json!(6), Value::Null)
    );
    assert_eq!(
        row(3),
        (
            json!("InProgress"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null
        )
    );
    assert_eq!(
        row(4).0,
        json!("Todo"),
        "a duplicate relation changes no status"
    );
    let comments = env
        .lcl(&["issue", "comments", "1", "--json"])
        .assert_ok()
        .json();
    let authors: Vec<_> = comments
        .as_array()
        .expect("comments")
        .iter()
        .map(|c| c["author"].clone())
        .collect();
    assert_eq!(
        authors,
        [Value::Null, Value::Null],
        "LCL_AUTHOR and USER never fill in an author"
    );
    assert_eq!(
        env.lcl(&["workspace", "show", "--json"]).assert_ok().json()["name"],
        "drm"
    );
}

#[test]
fn dry_run_counts_and_writes_nothing() {
    let file = export(&everything());
    let env = drm();
    let before = dump(&env.db_path());
    let out = env.lcl_stdin(&["import", "-", "--dry-run"], file.as_bytes());
    assert_eq!(
        trio(&out),
        (
            0,
            "Dry run, nothing written: would import 9 issues, 4 comments, 5 relations, 3 labels, 2 projects, 2 milestones, 1 cycle, 3 docs into workspace drm (key DRM); next number DRM-10\n",
            ""
        )
    );
    let out = env.lcl_stdin(&["--json", "import", "-", "--dry-run"], file.as_bytes());
    assert_eq!(
        out.assert_ok().json(),
        json!({"issues": 9, "comments": 4, "relations": 5, "labels": 3, "projects": 2, "milestones": 2, "cycles": 1, "docs": 3, "next_issue_number": 10, "dry_run": true})
    );
    assert!(dump(&env.db_path()) == before, "--dry-run wrote");
    let real = env
        .lcl_stdin(&["--json", "import", "-"], file.as_bytes())
        .assert_ok()
        .json();
    assert_eq!(real["dry_run"], false);
}

#[test]
fn export_refuses_what_import_would_refuse_and_writes_nothing() {
    let env = drm();
    for t in ["one", "two", "three"] {
        env.create(t);
    }
    let doc = env
        .lcl(&["--json", "doc", "create", "--title", "d"])
        .assert_ok()
        .json()["id"]
        .as_str()
        .expect("id")
        .to_string();
    let db = env.db_path();
    exec(
        &db,
        "PRAGMA ignore_check_constraints = ON; UPDATE issues SET status = 'Bogus' WHERE number = 2",
    );
    let one = "Error: can't export: 1 problem:\n  issue DRM-2: status \"Bogus\" isn't one lcl knows; no lcl command can fix it (column issues.status)\n";
    assert_eq!(trio(&env.lcl(&["export"])), (1, "", one));
    let dir = env.cwd.path();
    assert_eq!(
        trio(&env.lcl(&["export", "--output", "out.jsonl"])),
        (1, "", one)
    );
    assert!(!dir.join("out.jsonl").exists());

    // Every problem is listed, each with its fix.
    exec(
        &db,
        "UPDATE issues SET estimate = -3, due_date = 'tomorrow' WHERE number = 3",
    );
    exec(
        &db,
        &format!(
            "PRAGMA foreign_keys = OFF; UPDATE docs SET parent_id = 'nope' WHERE id = '{doc}'"
        ),
    );
    let out = env.lcl(&["export"]);
    let want = format!(
        "Error: can't export: 4 problems:\n  issue DRM-2: status \"Bogus\" isn't one lcl knows; no lcl command can fix it (column issues.status)\n  \
         issue DRM-3: estimate: an estimate can't be negative; got -3; fix it with lcl issue update DRM-3 --estimate N (or --no-estimate)\n  \
         issue DRM-3: due_date: invalid date \"tomorrow\"; expected a real calendar date as YYYY-MM-DD; fix it with lcl issue update DRM-3 --due YYYY-MM-DD (or --no-due)\n  \
         doc {doc}: parent nope is not a doc; fix it with lcl doc update {doc} --no-parent\n"
    );
    assert_eq!(trio(&out), (1, "", want.as_str()));
    // The fixes it names work.
    exec(&db, "UPDATE issues SET status = 'Todo' WHERE number = 2");
    env.lcl(&["issue", "update", "DRM-3", "--no-estimate", "--no-due"])
        .assert_ok();
    env.lcl(&["doc", "update", &doc, "--no-parent"]).assert_ok();
    let text = export(&env);

    // --output: refuses an existing file unless --force, and leaves no
    // temporary file either way.
    std::fs::write(dir.join("existing.jsonl"), "keep").expect("write");
    let out = env.lcl(&["export", "--output", "existing.jsonl"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            "Error: existing.jsonl already exists; pass --force to replace it\n"
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("existing.jsonl")).expect("read"),
        "keep"
    );
    let out = env.lcl(&["export", "--output", "existing.jsonl", "--force"]);
    assert_eq!(
        trio(&out),
        (
            0,
            "Exported 3 issues, 0 comments, 0 relations, 0 labels, 0 projects, 0 milestones, 0 cycles, 1 doc to existing.jsonl\n",
            ""
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("existing.jsonl")).expect("read"),
        text
    );
    let out = env
        .lcl(&["--json", "export", "--output", "new.jsonl"])
        .assert_ok()
        .json();
    assert_eq!(
        out,
        json!({"issues": 3, "comments": 0, "relations": 0, "labels": 0, "projects": 0, "milestones": 0, "cycles": 0, "docs": 1, "path": "new.jsonl"})
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("new.jsonl")).expect("read"),
        text
    );
    let left: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert_eq!(left, Vec::<String>::new(), "temporary files left behind");
    assert_eq!(
        env.lcl(&["export", "--force"]).code,
        2,
        "--force needs --output"
    );
    assert_eq!(
        env.lcl(&["export", "--output", "-"]).assert_ok().stdout,
        text
    );
}

#[test]
fn the_0_1_0_fixture_exports_and_round_trips() {
    let old = Env::new();
    old.install_db(V0_FIXTURE);
    // The first open upgrades the file and says so; the export comes after.
    old.lcl(&["workspace", "show"]).assert_ok();
    let first = export(&old);
    let recs = records(&first);
    assert_eq!(
        (
            &recs[0]["key"],
            of_type(&recs, "issue").len(),
            of_type(&recs, "comment").len(),
            of_type(&recs, "doc").len()
        ),
        (&json!("FIXT"), 3, 1, 1)
    );
    let new = Env::new();
    // The same name: import keeps the target's own.
    new.lcl(&["init", "fixture", "--key", "FIXT"]).assert_ok();
    import(&new, &first).assert_ok();
    assert!(export(&new) == first, "the 0.1.0 fixture didn't round-trip");
}

/// Runs every command at once, each after its own delay, and returns the
/// outputs in the order given.
fn staggered(env: &Env, cmds: &[(Duration, Vec<String>)]) -> Vec<Out> {
    let barrier = Barrier::new(cmds.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = cmds
            .iter()
            .map(|(delay, cmd)| {
                let barrier = &barrier;
                s.spawn(move || {
                    let args: Vec<&str> = cmd.iter().map(String::as_str).collect();
                    barrier.wait();
                    std::thread::sleep(*delay);
                    env.lcl(&args)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect()
    })
}

/// One import racing ten creates on an empty workspace: the import lands
/// whole before every create, or a create lands first and the import
/// writes nothing. Never a mix. Started together, a create nearly always
/// wins, so in half the rounds the creates start 100 ms late.
#[test]
fn an_import_racing_creates_is_all_or_nothing() {
    let mut file = String::from(
        "{\"type\":\"workspace\",\"format\":\"lcl-export/1\",\"key\":\"DRM\",\"next_issue_number\":150}\n{\"type\":\"label\",\"name\":\"imported\"}\n",
    );
    for n in 100..150 {
        file.push_str(&format!("{{\"type\":\"issue\",\"number\":{n},\"title\":\"imported {n}\",\"labels\":[\"imported\"]}}\n{{\"type\":\"comment\",\"issue\":{n},\"body\":\"c{n}\"}}\n"));
    }
    let (mut import_first, mut create_first) = (0, 0);
    for round in 0..10 {
        let env = drm();
        std::fs::write(env.cwd.path().join("race.jsonl"), &file).expect("write");
        let late = if round % 2 == 0 {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        };
        let mut cmds = vec![(Duration::ZERO, argv(&["import", "race.jsonl"]))];
        cmds.extend((0..10).map(|i| {
            (
                late,
                argv(&["issue", "create", "--title", &format!("c{i}"), "--json"]),
            )
        }));
        let outs = staggered(&env, &cmds);
        let mut created: Vec<i64> = outs[1..]
            .iter()
            .map(|o| o.assert_ok().json()["number"].as_i64().expect("number"))
            .collect();
        created.sort_unstable();
        let all = env
            .lcl(&["issue", "list", "--all", "--json"])
            .assert_ok()
            .json();
        let total = all["total"].as_i64().expect("total");
        let labels = env.lcl(&["label", "list", "--json"]).assert_ok().json();
        if outs[0].code == 0 {
            import_first += 1;
            assert_eq!(
                created,
                (150..160).collect::<Vec<_>>(),
                "round {round}: every create is numbered past the import"
            );
            assert_eq!(
                (total, labels.as_array().map(Vec::len)),
                (60, Some(1)),
                "round {round}"
            );
        } else {
            create_first += 1;
            assert_eq!(outs[0].code, 1, "round {round}: {:#?}", outs[0]);
            assert!(
                outs[0].stderr.contains("is not empty"),
                "round {round}: {:#?}",
                outs[0]
            );
            assert_eq!(created, (1..=10).collect::<Vec<_>>(), "round {round}");
            assert_eq!(
                (total, labels.as_array().map(Vec::len)),
                (10, Some(0)),
                "round {round}: the refused import wrote something"
            );
        }
        assert_eq!(
            foreign_key_violations(&env.db_path()),
            vec![],
            "round {round}"
        );
    }
    eprintln!(
        "race: the import landed first in {import_first} of 10 rounds, a create in {create_first}"
    );
}

/// The emptiness check is inside the import's transaction: a write that
/// lands while the import waits for the lock is seen, and the import
/// refuses. (Checked before the lock, the workspace looked empty.)
#[test]
fn a_write_while_the_import_waits_is_seen() {
    let env = drm();
    let file = join(&base());
    let db = env.db_path();
    let (locked, taken) = std::sync::mpsc::channel();
    let out = std::thread::scope(|s| {
        let db = &db;
        let holder = s.spawn(move || {
            let conn = rusqlite::Connection::open(db).expect("open db");
            conn.execute_batch("BEGIN IMMEDIATE")
                .expect("take the write lock");
            locked.send(()).expect("signal");
            // Time for the import to start, read its file and wait.
            std::thread::sleep(Duration::from_millis(1500));
            conn.execute_batch(
                "INSERT INTO labels(id, name, color) VALUES ('held', 'held', '#888888'); COMMIT",
            )
            .expect("write and release");
        });
        taken.recv().expect("lock taken");
        let out = import(&env, &file);
        holder.join().expect("holder thread");
        out
    });
    let want = "Error: workspace \"drm\" is not empty: it has 1 label; import only into an empty workspace (make one with lcl init or lcl workspace create)\n";
    assert_eq!(trio(&out), (1, "", want));
    let total = env
        .lcl(&["issue", "list", "--all", "--json"])
        .assert_ok()
        .json()["total"]
        .clone();
    assert_eq!(
        total, 0,
        "the import wrote after a write it should have seen"
    );
}

/// The size of dreamd's Linear dump, three times over. Run against the
/// release build: cargo test --release --test export_import -- --ignored
#[test]
#[ignore = "timing; run with --release"]
fn import_and_export_are_fast() {
    let mut file =
        String::from("{\"type\":\"workspace\",\"format\":\"lcl-export/1\",\"key\":\"DRM\"}\n");
    for n in 1..=1700 {
        file.push_str(&format!("{{\"type\":\"issue\",\"number\":{n},\"title\":\"issue {n}\",\"description\":\"the description of issue {n}, with some words in it\",\"ref\":\"BZR-{n}\",\"created_at\":{},\"status\":\"Done\"}}\n", 1700000000 + n));
    }
    for n in 1..=1700 {
        for c in 0..3 {
            file.push_str(&format!("{{\"type\":\"comment\",\"issue\":{n},\"body\":\"comment {c} on issue {n}\",\"author\":\"someone\",\"created_at\":{}}}\n", 1700000000 + n));
        }
    }
    let mut relations = 0;
    for n in 1..=600 {
        file.push_str(&format!(
            "{{\"type\":\"relation\",\"kind\":\"blocks\",\"from\":{n},\"to\":{}}}\n",
            n + 1
        ));
        relations += 1;
    }
    for n in 1..=300 {
        file.push_str(&format!(
            "{{\"type\":\"relation\",\"kind\":\"related\",\"from\":{n},\"to\":{}}}\n",
            n + 900
        ));
        relations += 1;
    }
    for n in 1..=100 {
        file.push_str(&format!(
            "{{\"type\":\"relation\",\"kind\":\"duplicate\",\"from\":{},\"to\":{n}}}\n",
            n + 1500
        ));
        relations += 1;
    }
    assert_eq!(relations, 1000);
    let env = drm();
    let path = env.cwd.path().join("big.jsonl");
    std::fs::write(&path, &file).expect("write");
    let start = Instant::now();
    let out = env.lcl(&["import", "big.jsonl"]);
    let import_time = start.elapsed();
    assert_eq!(out.code, 0, "{out:#?}");
    let start = Instant::now();
    let exported = env.lcl(&["export", "--output", "out.jsonl"]);
    let export_time = start.elapsed();
    exported.assert_ok();
    let text = std::fs::read_to_string(Path::new(env.cwd.path()).join("out.jsonl")).expect("read");
    eprintln!(
        "import of {} bytes: {import_time:?}; export of {} bytes: {export_time:?}; {}",
        file.len(),
        text.len(),
        out.stdout.trim()
    );
    assert!(
        import_time.as_secs_f64() < 5.0,
        "import took {import_time:?}"
    );
    assert_eq!(text.lines().count(), 1 + 1700 + 5100 + 1000);
}

#[test]
fn the_new_commands_have_help() {
    let env = Env::new();
    for (args, flags) in [
        (vec!["export", "-h"], vec!["--output <PATH>", "--force"]),
        (vec!["import", "-h"], vec!["--dry-run"]),
        (vec!["doc", "create", "-h"], vec!["--parent <ID>"]),
        (
            vec!["doc", "update", "-h"],
            vec!["--parent <ID>", "--no-parent"],
        ),
    ] {
        let help = env.lcl(&args).assert_ok().stdout.clone();
        for flag in flags {
            let line = help
                .lines()
                .find(|l| l.trim_start().starts_with(flag))
                .unwrap_or_or_else(&help, flag);
            assert!(
                line.trim_start()[flag.len()..].trim().len() > 3,
                "{args:?}: no help for {flag}: {line:?}"
            );
        }
    }
    let top = env.lcl(&["-h"]).assert_ok().stdout.clone();
    for cmd in ["export", "import"] {
        let line = top
            .lines()
            .find(|l| l.trim_start().starts_with(cmd))
            .unwrap_or_or_else(&top, cmd);
        assert!(
            line.trim_start()[cmd.len()..].trim().len() > 3,
            "no help for {cmd}: {line:?}"
        );
    }
}

trait Found<'a> {
    fn unwrap_or_or_else(self, all: &str, what: &str) -> &'a str;
}
impl<'a> Found<'a> for Option<&'a str> {
    fn unwrap_or_or_else(self, all: &str, what: &str) -> &'a str {
        self.unwrap_or_else(|| panic!("no {what} in\n{all}"))
    }
}

/// The not-empty refusal for a workspace holding `has`.
fn not_empty(has: &str) -> String {
    format!(
        "Error: workspace \"drm\" is not empty: it has {has}; import only into an empty workspace (make one with lcl init or lcl workspace create)\n"
    )
}

/// A DRM workspace after `setup`, refusing the base file with `has` and
/// left unchanged.
fn refuses_after(setup: &[&[&str]], has: &str) {
    let env = drm();
    for args in setup {
        env.lcl(args).assert_ok();
    }
    let before = dump(&env.db_path());
    let file = join(&base());
    assert_eq!(trio(&import(&env, &file)), (1, "", not_empty(has).as_str()));
    assert_eq!(
        trio(&env.lcl_stdin(&["import", "-", "--dry-run"], file.as_bytes())),
        (1, "", not_empty(has).as_str())
    );
    assert!(
        dump(&env.db_path()) == before,
        "a refused import changed a workspace holding {has}"
    );
}

#[test]
fn a_workspace_holding_only_a_project_is_not_empty() {
    refuses_after(&[&["project", "create", "p"]], "1 project");
}

#[test]
fn a_workspace_holding_only_a_project_and_a_milestone_is_not_empty() {
    refuses_after(
        &[
            &["project", "create", "p"],
            &["milestone", "create", "m", "--project", "p"],
        ],
        "1 project and 1 milestone",
    );
}

#[test]
fn a_workspace_holding_only_a_cycle_is_not_empty() {
    refuses_after(&[&["cycle", "create", "c"]], "1 cycle");
}

#[test]
fn a_workspace_holding_only_a_doc_is_not_empty() {
    refuses_after(&[&["doc", "create", "--title", "d"]], "1 doc");
}

#[test]
fn the_not_empty_message_names_all_eight_kinds() {
    let env = everything();
    let before = dump(&env.db_path());
    let out = import(&env, &join(&base()));
    let want = not_empty(
        "9 issues, 3 labels, 2 projects, 2 milestones, 1 cycle, 3 docs, 4 comments and 5 relations",
    );
    assert_eq!(trio(&out), (1, "", want.as_str()));
    assert!(dump(&env.db_path()) == before);
}

fn clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

/// A left-out updated_at is the record's created_at; a left-out created_at
/// is the import time.
#[test]
fn left_out_times_default_to_created_at_then_the_import_time() {
    let env = drm();
    let file = [
        r#"{"type":"workspace","format":"lcl-export/1","key":"DRM"}"#,
        r#"{"type":"project","name":"dated","created_at":100}"#,
        r#"{"type":"project","name":"undated"}"#,
        r#"{"type":"milestone","project":"dated","name":"m-dated","created_at":200}"#,
        r#"{"type":"milestone","project":"dated","name":"m-undated"}"#,
        r#"{"type":"cycle","name":"c-undated"}"#,
        r#"{"type":"doc","title":"dated","created_at":300}"#,
        r#"{"type":"doc","title":"undated"}"#,
        r#"{"type":"issue","number":1,"title":"dated","created_at":400}"#,
        r#"{"type":"issue","number":2,"title":"undated"}"#,
        r#"{"type":"comment","issue":1,"body":"undated"}"#,
    ]
    .map(|l| format!("{l}\n"))
    .concat();
    let before = clock();
    import(&env, &file).assert_ok();
    let after = clock();
    let recs = records(&export(&env));
    let find = |t: &str, key: &str, name: &str| -> Value {
        recs.iter()
            .find(|r| r["type"] == t && r[key] == name)
            .unwrap_or_else(|| panic!("no {t} {name}"))
            .clone()
    };
    let times = |r: &Value| {
        (
            r["created_at"].as_i64().expect("created_at"),
            r["updated_at"].as_i64().expect("updated_at"),
        )
    };
    for (t, key, created) in [
        ("project", "name", 100),
        ("milestone", "name", 200),
        ("doc", "title", 300),
        ("issue", "title", 400),
    ] {
        let name = if t == "milestone" { "m-dated" } else { "dated" };
        assert_eq!(
            times(&find(t, key, name)),
            (created, created),
            "{t} with created_at and no updated_at"
        );
        let undated = if t == "milestone" {
            "m-undated"
        } else {
            "undated"
        };
        let (c, u) = times(&find(t, key, undated));
        assert!(
            before <= c && c <= after && u == c,
            "{t} with neither: ({c}, {u}), import between {before} and {after}"
        );
    }
    for (t, key, name) in [
        ("cycle", "name", "c-undated"),
        ("comment", "body", "undated"),
    ] {
        let c = find(t, key, name)["created_at"]
            .as_i64()
            .expect("created_at");
        assert!(
            before <= c && c <= after,
            "{t} with no created_at: {c}, import between {before} and {after}"
        );
    }
}

#[test]
fn the_header_sets_description_and_created_at_only_when_it_has_them() {
    let show = |env: &Env| {
        let ws = env.lcl(&["workspace", "show", "--json"]).assert_ok().json();
        (ws["description"].clone(), ws["created_at"].clone())
    };
    let given = drm();
    import(&given, "{\"type\":\"workspace\",\"format\":\"lcl-export/1\",\"key\":\"DRM\",\"description\":\"dreamd's tracker\",\"created_at\":1700000000}\n").assert_ok();
    assert_eq!(show(&given), (json!("dreamd's tracker"), json!(1700000000)));

    let kept = drm();
    exec(
        &kept.db_path(),
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('description', 'its own')",
    );
    let own = show(&kept);
    assert_eq!(own.0, json!("its own"));
    import(
        &kept,
        "{\"type\":\"workspace\",\"format\":\"lcl-export/1\",\"key\":\"DRM\"}\n",
    )
    .assert_ok();
    assert_eq!(
        show(&kept),
        own,
        "a header without them keeps the workspace's own"
    );
}

#[test]
fn a_byte_order_mark_is_named_as_the_problem() {
    let env = drm();
    let before = dump(&env.db_path());
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend_from_slice(join(&base()).as_bytes());
    let want =
        "Error: line 1: line: the file starts with a UTF-8 byte order mark; save it without one\n";
    assert_eq!(
        trio(&env.lcl_stdin(&["import", "-"], &bytes)),
        (2, "", want)
    );
    assert!(dump(&env.db_path()) == before);
}

/// TEXT with each UUID replaced by `<uuid>`.
fn mask_uuids(text: &str) -> String {
    let is_uuid = |s: &[u8]| {
        s.len() >= 36
            && s[..36].iter().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    *c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
    };
    let (b, mut out, mut i) = (text.as_bytes(), Vec::new(), 0);
    while i < b.len() {
        if is_uuid(&b[i..]) {
            out.extend_from_slice(b"<uuid>");
            i += 36;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("a UUID is ASCII, so the rest stays UTF-8")
}

/// A second person, on a machine of their own (another HOME), is handed the
/// file one person exported. They make a workspace with its key, under a
/// name of their own, and import it. They then see what the first person
/// sees: every issue with its number, fields, relations and comments, in
/// the text output and the JSON. Only the UUIDs differ: the file names
/// issues by number, and an import gives each record it makes a new id.
/// The next issue either of them creates gets the same number.
#[test]
fn a_second_person_importing_an_export_sees_the_same_issues() {
    let a = everything();
    a.lcl(&["export", "--output", "issues.jsonl"]).assert_ok();
    let file = std::fs::read(a.cwd.path().join("issues.jsonl")).expect("the export");

    let b = Env::new();
    std::fs::write(b.cwd.path().join("issues.jsonl"), &file).expect("hand the file over");
    b.lcl(&["init", "theirs", "--key", "DRM"]).assert_ok();
    b.lcl(&["import", "issues.jsonl"]).assert_ok();

    let both = |args: &[&str]| -> (Out, Out) {
        let (x, y) = (a.lcl(args), b.lcl(args));
        x.assert_ok();
        y.assert_ok();
        (x, y)
    };
    let (seen, theirs) = both(&["--json", "issue", "list", "--all"]);
    assert_eq!(
        mask_uuids(&theirs.stdout),
        mask_uuids(&seen.stdout),
        "issue list --json differs"
    );
    let (seen_text, theirs_text) = both(&["issue", "list", "--all"]);
    assert_eq!(theirs_text.stdout, seen_text.stdout, "issue list differs");

    let issues = seen.json()["issues"].as_array().expect("issues").clone();
    assert!(issues.len() >= 5, "everything() has issues: {issues:?}");
    let mut comments = 0;
    for issue in &issues {
        let id = issue["identifier"].as_str().expect("an identifier");
        for args in [
            vec!["--json", "issue", "show", id],
            vec!["issue", "show", id],
            vec!["--json", "issue", "comments", id],
        ] {
            let (x, y) = both(&args);
            assert_eq!(
                mask_uuids(&y.stdout),
                mask_uuids(&x.stdout),
                "lcl {} differs",
                args.join(" ")
            );
        }
        comments += a
            .lcl(&["--json", "issue", "comments", id])
            .json()
            .as_array()
            .expect("comments")
            .len();
    }
    assert!(comments >= 3, "everything() has comments: {comments}");

    let next = |env: &Env| {
        env.lcl(&[
            "--json",
            "issue",
            "create",
            "--title",
            "after the hand-over",
        ])
        .assert_ok()
        .json()["identifier"]
            .clone()
    };
    assert_eq!(next(&b), next(&a));
}
