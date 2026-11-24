//! `lcl sync join` and the pulling half of `lcl sync`: what one machine
//! does reaches the others, once, and they converge (LOCL-30). Against the
//! reference service in tests/sync_service (docs/sync-protocol.md).

mod common;
mod sync_service;

use common::Env;
use serde_json::{Value, json};
use sync_service::client::*;
use sync_service::{Service, now_ms};

/// The workspace's records, as `lcl export` writes them, without what each
/// file keeps for itself: the header (its counters) and `updated_at`.
fn records(env: &Env) -> Vec<Value> {
    let out = env.lcl(&["export"]).assert_ok().stdout.clone();
    out.lines()
        .skip(1)
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            v.as_object_mut().unwrap().remove("updated_at");
            v
        })
        .collect()
}

/// An empty workspace DEMO on a second machine, joined to `ws`.
fn join(svc: &Service, ws: &str) -> (Env, Value) {
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    let joined = ok(&env, &["sync", "join", &svc.url, ws]);
    (env, joined)
}

/// Starts `env` on `svc`, returning the replica's id.
fn start(env: &Env, svc: &Service) -> String {
    ok(env, &["sync", "start", &svc.url])["workspace"]
        .as_str()
        .unwrap()
        .to_string()
}

#[track_caller]
fn same(a: &Env, b: &Env) {
    let (ra, rb) = (records(a), records(b));
    assert_eq!(ra, rb, "the two machines differ");
}

#[test]
fn join_receives_the_whole_workspace_with_its_history() {
    let svc = Service::start();
    let a = Env::new();
    for args in [
        &["init", "demo", "--key", "DEMO"][..],
        &["label", "create", "bug"],
        &["project", "create", "Parser"],
        &["milestone", "create", "Beta", "--project", "Parser"],
        &["cycle", "create", "Sprint 1"],
        &[
            "issue",
            "create",
            "--title",
            "one",
            "--label",
            "bug",
            "--project",
            "Parser",
            "--milestone",
            "Beta",
            "--cycle",
            "Sprint 1",
            "--priority",
            "high",
            "--author",
            "ann",
        ],
        &["issue", "create", "--title", "two", "--parent", "1"],
        &["issue", "create", "--title", "three"],
        &["issue", "relate", "3", "blocks", "1"],
        &["issue", "relate", "2", "related", "1"],
        &["issue", "comment", "1", "hello", "--author", "ann"],
        &[
            "issue",
            "update",
            "1",
            "--status",
            "in progress",
            "--title",
            "one!",
        ],
        &["doc", "create", "--title", "Notes", "--content", "words"],
    ] {
        a.lcl(args).assert_ok();
    }
    let notes = ok(&a, &["doc", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.lcl(&["doc", "create", "--title", "Child", "--parent", &notes])
        .assert_ok();
    let ws = start(&a, &svc);
    let a_machine = ok(&a, &["workspace", "show"])["sync"]["machine"].clone();

    let (b, joined) = join(&svc, &ws);
    assert_eq!(joined["server"], svc.url);
    assert_eq!(joined["workspace"], ws);
    assert_ne!(joined["machine"], a_machine);
    assert_eq!(joined["received"], local_log(&a.db_path()).len());
    assert_eq!(joined["numbers_left"], 100);
    same(&a, &b);
    for n in ["1", "2"] {
        assert_eq!(
            ok(&a, &["issue", "history", n]),
            ok(&b, &["issue", "history", n]),
            "the history of {n}"
        );
    }
    let child = ok(&b, &["doc", "list"])
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["title"] == "Child")
        .map(|d| d["parent_id"].clone());
    assert_eq!(child, Some(json!(notes)));
    // Nothing received goes back as B's own.
    assert!(unsynced(&b.db_path()).is_empty());
    assert_eq!(ok(&b, &["sync"])["sent"], 0);
    // B numbers from its own block, past A's.
    assert_eq!(b.create("from b"), 104);
    assert_eq!(a.create("from a"), 4);
    let text = with_token(&a, &["workspace", "show"])
        .assert_ok()
        .stdout
        .clone();
    assert!(text.contains(&format!("Syncs with {} as workspace {ws}", svc.url)));
}

#[test]
fn two_machines_converge_with_no_number_used_twice() {
    let svc = Service::start();
    let a = demo();
    a.create("two");
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);

    // Apart: each creates, and edits issues they share.
    assert_eq!(a.create("from a"), 3);
    a.lcl(&["issue", "update", "1", "--title", "A's title"])
        .assert_ok();
    a.lcl(&["issue", "update", "2", "--priority", "low"])
        .assert_ok();
    assert_eq!(b.create("from b"), 103);
    b.lcl(&["label", "create", "b-label"]).assert_ok();
    b.lcl(&[
        "issue",
        "update",
        "1",
        "--status",
        "done",
        "--add-label",
        "b-label",
    ])
    .assert_ok();
    b.lcl(&["issue", "comment", "1", "from b"]).assert_ok();
    // Later than A's.
    b.lcl(&["issue", "update", "2", "--priority", "high"])
        .assert_ok();

    let from_a = ok(&a, &["sync"]);
    assert_eq!(from_a["received"], 0);
    let from_b = ok(&b, &["sync"]);
    assert_eq!(from_b["received"], from_a["sent"]);
    assert_eq!(from_b["overwritten"], json!([]), "B's change was the later");
    let text = with_token(&a, &["sync"]).assert_ok().stdout.clone();
    assert_eq!(
        text,
        format!(
            "Sent 0 changes, received {} changes; 99 issue numbers held\nOverwritten by a later change from elsewhere: DEMO-2 priority\n",
            from_b["sent"]
        )
    );
    same(&a, &b);
    let issues = ok(&a, &["issue", "list"]);
    let mut numbers: Vec<i64> = issues["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_i64().unwrap())
        .collect();
    numbers.sort();
    assert_eq!(numbers, [1, 2, 3, 103]);
    let one = ok(&a, &["issue", "show", "1"]);
    assert_eq!(
        (
            &one["issue"]["title"],
            &one["issue"]["status"],
            &one["issue"]["labels"]
        ),
        (&json!("A's title"), &json!("Done"), &json!(["b-label"]))
    );
    assert_eq!(one["comments"][0]["body"], "from b");
    assert_eq!(ok(&b, &["issue", "show", "2"])["issue"]["priority"], "High");
    assert_eq!(
        ok(&a, &["issue", "history", "2"]),
        ok(&b, &["issue", "history", "2"])
    );
    for env in [&a, &b] {
        assert!(unsynced(&env.db_path()).is_empty());
        assert_eq!(ok(env, &["sync"])["received"], 0, "nothing echoes back");
    }
}

#[test]
fn a_delete_a_restore_and_a_purge_reach_the_other_machine_once() {
    let svc = Service::start();
    let a = demo();
    a.create("two");
    a.create("three");
    let kept = ok(&a, &["issue", "comment", "1", "kept"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let gone = ok(&a, &["issue", "comment", "1", "gone"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.lcl(&["issue", "comment", "3", "goes with three"])
        .assert_ok();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    let three = issue_id(&b, 3);
    let both = |a: &Env, b: &Env| {
        ok(a, &["sync"]);
        ok(b, &["sync"]);
    };

    a.lcl(&["comment", "delete", &kept]).assert_ok();
    a.lcl(&["issue", "delete", "1"]).assert_ok();
    both(&a, &b);
    let (kind, message) = fails(&b, None, &["issue", "show", "1"], 1);
    assert_eq!(kind, "conflict", "{message}");
    same(&a, &b);

    a.lcl(&["issue", "restore", "1"]).assert_ok();
    a.lcl(&["comment", "restore", &kept]).assert_ok();
    a.lcl(&["comment", "delete", &gone]).assert_ok();
    a.lcl(&["comment", "purge", &gone]).assert_ok();
    a.lcl(&["issue", "delete", "3"]).assert_ok();
    a.lcl(&["issue", "purge", "3"]).assert_ok();
    both(&a, &b);
    let comments = ok(&b, &["issue", "comments", "1"]);
    let bodies: Vec<&str> = comments
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["kept"]);
    assert_eq!(fails(&b, None, &["issue", "show", "3"], 1).0, "not_found");
    same(&a, &b);
    let conn = rusqlite::Connection::open(b.db_path()).unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comments WHERE issue_id = ?1",
            [&three],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "three's comment went with it");
    let tombstone: i64 = conn
        .query_row(
            "SELECT number FROM purged WHERE record = ?1",
            [&three],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstone, 3);
    drop(conn);
    // Once: B sends nothing back, and the replica has one purge of three.
    assert!(unsynced(&b.db_path()).is_empty());
    let mut s = svc.state();
    let purges = s
        .replica()
        .log
        .iter()
        .filter(|l| l.change["record"] == three.as_str() && l.change["field"] == "*")
        .filter(|l| l.change["new"].is_null())
        .count();
    assert_eq!(purges, 1);
}

#[test]
fn an_issue_moved_to_another_projects_milestone_arrives_whole() {
    let svc = Service::start();
    let a = Env::new();
    for args in [
        &["init", "demo", "--key", "DEMO"][..],
        &["project", "create", "P1"],
        &["project", "create", "P2"],
        &["milestone", "create", "M1", "--project", "P1"],
        &["milestone", "create", "M2", "--project", "P2"],
        &[
            "issue",
            "create",
            "--title",
            "one",
            "--project",
            "P1",
            "--milestone",
            "M1",
        ],
    ] {
        a.lcl(args).assert_ok();
    }
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    // One write sets both; either alone would put the milestone outside
    // the project.
    a.lcl(&[
        "issue",
        "update",
        "1",
        "--project",
        "P2",
        "--milestone",
        "M2",
    ])
    .assert_ok();
    ok(&a, &["sync"]);
    ok(&b, &["sync"]);
    let one = ok(&b, &["issue", "show", "1"]);
    assert_eq!(
        (&one["issue"]["project"], &one["issue"]["milestone"]),
        (&json!("P2"), &json!("M2"))
    );
    same(&a, &b);
}

#[test]
fn what_a_rule_of_the_file_refuses_is_tried_again_after_the_rest() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    // Apart, both make a label "bug".
    a.lcl(&["label", "create", "bug"]).assert_ok();
    ok(&a, &["sync"]);
    b.lcl(&["label", "create", "bug", "--color", "#ff0000"])
        .assert_ok();
    let b_label = ok(&b, &["label", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The service's correction: B's goes, A's stays.
    svc.state().purge(&ws, "label", &b_label);

    // A's label can't come in while B's has the name; after the purge, it
    // does.
    let report = ok(&b, &["sync"]);
    assert_eq!(
        report["not_applied"],
        json!([{"kind": "label", "record": b_label, "identifier": null, "field": "*"}])
    );
    assert_eq!(report["received"], 2);
    let labels = ok(&b, &["label", "list"]);
    assert_eq!(labels, ok(&a, &["label", "list"]));
    assert_eq!(labels[0]["color"], "#888888");
    ok(&a, &["sync"]);
    same(&a, &b);
}

#[test]
fn a_pull_that_cant_be_applied_is_rolled_back_whole() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    let one = issue_id(&b, 1);
    let before = records(&b);
    let seq = meta(&b.db_path(), "sync_seq");
    // A good change, then one for an issue that isn't anywhere.
    {
        let mut s = svc.state();
        s.change(
            &ws,
            "issue",
            &one,
            "title",
            json!("one"),
            json!("renamed"),
            now_ms(),
        );
        s.change(
            &ws,
            "issue",
            "no-such-issue",
            "title",
            json!("x"),
            json!("y"),
            now_ms(),
        );
    }
    let (kind, message) = fails(&b, Some(sync_service::TOKEN), &["sync"], 1);
    assert_eq!(kind, "other");
    let n = svc.state().replica().log.len();
    assert_eq!(
        message,
        format!(
            "can't apply change {n} from the sync service (issue no-such-issue title): a rule of this file refuses it, or its record isn't here; nothing was applied"
        )
    );
    assert_eq!(records(&b), before);
    assert_eq!(meta(&b.db_path(), "sync_seq"), seq);

    // A field that names a record nowhere, found at the commit.
    let svc = Service::start();
    let c = demo();
    let ws = start(&c, &svc);
    let (d, _) = join(&svc, &ws);
    let before = records(&d);
    svc.state().change(
        &ws,
        "issue",
        &issue_id(&d, 1),
        "project_id",
        Value::Null,
        json!("no-such-project"),
        now_ms(),
    );
    let (kind, message) = fails(&d, Some(sync_service::TOKEN), &["sync"], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "other",
            "the changes from the sync service would leave a record naming one that isn't here (a parent, a project, a milestone, a cycle); nothing was applied"
        )
    );
    assert_eq!(records(&d), before);
}

#[test]
fn a_change_this_lcl_does_not_know_stops_the_sync() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    let before = records(&b);
    {
        let mut s = svc.state();
        s.change(
            &ws,
            "issue",
            &issue_id(&b, 1),
            "title",
            json!("one"),
            json!("renamed"),
            now_ms(),
        );
        s.change(&ws, "widget", "w1", "size", json!(1), json!(2), now_ms());
    }
    let (kind, message) = fails(&b, Some(sync_service::TOKEN), &["sync"], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "schema",
            "the sync service sent a change this lcl doesn't know (widget size); upgrade lcl, then run lcl sync again"
        )
    );
    assert_eq!(records(&b), before);
}

#[test]
fn this_machines_clock_moves_past_the_latest_it_received() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    let one = issue_id(&b, 1);
    // A browser edit stamped 30 seconds ahead (the service takes up to 60).
    let ahead = now_ms() + 30_000;
    svc.state().change(
        &ws,
        "issue",
        &one,
        "title",
        json!("one"),
        json!("from ahead"),
        ahead,
    );
    ok(&b, &["sync"]);
    assert_eq!(
        ok(&b, &["issue", "show", "1"])["issue"]["title"],
        "from ahead"
    );
    // B's edit right after is later than it, though B's clock is behind.
    b.lcl(&["issue", "update", "1", "--title", "B's"])
        .assert_ok();
    let last = unsynced(&b.db_path()).pop().unwrap();
    assert!(
        last["clock"].as_str().unwrap() > format!("{ahead:013}").as_str(),
        "{last}"
    );
    ok(&b, &["sync"]);
    ok(&a, &["sync"]);
    assert_eq!(ok(&a, &["issue", "show", "1"])["issue"]["title"], "B's");
    same(&a, &b);
}

#[test]
fn join_wants_an_empty_workspace_with_the_replicas_key() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let token = Some(sync_service::TOKEN);

    let full = demo();
    let (kind, message) = fails(&full, token, &["sync", "join", &svc.url, &ws], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "conflict",
            "workspace \"demo\" is not empty: it has 1 issue; join into an empty workspace (make one with lcl workspace create NAME --key KEY)"
        )
    );
    let other = Env::new();
    other.lcl(&["init", "other", "--key", "OTH"]).assert_ok();
    let (kind, message) = fails(&other, token, &["sync", "join", &svc.url, &ws], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "conflict",
            "the replica is for key DEMO but this workspace's key is OTH; join from a workspace with key DEMO"
        )
    );
    assert_eq!(ok(&other, &["workspace", "show"])["sync"], Value::Null);
    let empty = Env::new();
    empty.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    let (kind, _) = fails(&empty, token, &["sync", "join", &svc.url, "a/b"], 2);
    assert_eq!(kind, "usage");
    let (kind, _) = fails(&empty, token, &["sync", "join", &svc.url, "no-such"], 1);
    assert_eq!(kind, "not_found");
    let (kind, _) = fails(&empty, None, &["sync", "join", &svc.url, &ws], 1);
    assert_eq!(kind, "auth");
    let (kind, _) = fails(&a, token, &["sync", "join", &svc.url, &ws], 1);
    assert_eq!(kind, "conflict", "A syncs already");
    // And it works.
    ok(&empty, &["sync", "join", &svc.url, &ws]);
    same(&a, &empty);
}

#[test]
fn after_start_the_first_sync_pulls_only_what_is_new() {
    let svc = Service::start();
    let a = demo();
    let sent = ok(&a, &["sync", "start", &svc.url])["sent"]
        .as_i64()
        .unwrap();
    ok(&a, &["sync"]);
    let s = svc.state();
    let pulls: Vec<&str> = s
        .requests
        .iter()
        .filter(|r| r.method == "GET")
        .map(|r| r.url.as_str())
        .collect();
    let expected = format!("/v1/workspaces/{}/changes?after={sent}&limit=500", "ws-1");
    assert_eq!(pulls, [expected.as_str()]);
}

#[test]
fn leave_writes_the_export_then_deletes_the_replica() {
    let svc = Service::start();
    let a = demo();
    a.lcl(&["label", "create", "bug"]).assert_ok();
    let ws = start(&a, &svc);
    let (b, _) = join(&svc, &ws);
    let before = records(&a);
    let path = a.cwd.path().join("leaving.jsonl");
    let p = path.to_str().unwrap();
    let left = ok(&a, &["sync", "leave", "--output", p]);
    assert_eq!(
        (&left["path"], &left["issues"], &left["labels"]),
        (&json!(p), &json!(1), &json!(1))
    );
    assert_eq!(
        (&left["server"], &left["workspace"]),
        (&json!(svc.url), &json!(ws))
    );
    let export = a.lcl(&["export"]).assert_ok().stdout.clone();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), export);
    assert!(svc.state().replicas.is_empty(), "the replica is gone");
    // The local file keeps everything, and no longer syncs.
    assert_eq!(records(&a), before);
    assert_eq!(ok(&a, &["workspace", "show"])["sync"], Value::Null);
    // The other machine finds no replica.
    let token = Some(sync_service::TOKEN);
    assert_eq!(fails(&b, token, &["sync"], 1).0, "not_found");

    // Run again, with no replica: the export, and nothing else.
    let asked = svc.state().requests.len();
    let again = a.cwd.path().join("again.jsonl");
    let out = ok(&a, &["sync", "leave", "--output", again.to_str().unwrap()]);
    assert_eq!(
        (&out["server"], &out["workspace"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(std::fs::read_to_string(&again).unwrap(), export);
    assert_eq!(svc.state().requests.len(), asked);
    let text = a
        .lcl(&[
            "sync",
            "leave",
            "--output",
            again.to_str().unwrap(),
            "--force",
        ])
        .assert_ok()
        .stdout
        .clone();
    assert!(
        text.starts_with("Exported ") && text.lines().count() == 1,
        "{text}"
    );

    // The other machine leaves too: a replica already gone counts as
    // deleted.
    let theirs = b.cwd.path().join("theirs.jsonl");
    let out = ok(&b, &["sync", "leave", "--output", theirs.to_str().unwrap()]);
    assert_eq!(out["workspace"], json!(ws));
    assert_eq!(ok(&b, &["workspace", "show"])["sync"], Value::Null);
}

#[test]
fn leave_deletes_nothing_until_the_export_is_written() {
    let svc = Service::start();
    let a = demo();
    let ws = start(&a, &svc);
    let token = Some(sync_service::TOKEN);
    let still_there = |svc: &Service| svc.state().replicas.contains_key(&ws);

    let taken = a.cwd.path().join("taken.jsonl");
    std::fs::write(&taken, "keep").unwrap();
    let (kind, _) = fails(
        &a,
        token,
        &["sync", "leave", "--output", taken.to_str().unwrap()],
        1,
    );
    assert_eq!(kind, "conflict");
    assert_eq!(std::fs::read_to_string(&taken).unwrap(), "keep");
    assert!(still_there(&svc));

    let fresh = a.cwd.path().join("fresh.jsonl");
    let (kind, _) = fails(
        &a,
        None,
        &["sync", "leave", "--output", fresh.to_str().unwrap()],
        1,
    );
    assert_eq!(kind, "auth");
    assert!(!fresh.exists(), "no token: nothing written");
    assert!(still_there(&svc));

    svc.state().down = true;
    let (kind, _) = fails(
        &a,
        token,
        &["sync", "leave", "--output", fresh.to_str().unwrap()],
        1,
    );
    assert_eq!(kind, "network");
    assert!(fresh.exists(), "the export is written first");
    assert!(still_there(&svc));
    assert!(
        ok(&a, &["workspace", "show"])["sync"].is_object(),
        "still syncing"
    );

    svc.state().down = false;
    let text = with_token(
        &a,
        &[
            "sync",
            "leave",
            "--output",
            fresh.to_str().unwrap(),
            "--force",
        ],
    )
    .assert_ok()
    .stdout
    .clone();
    assert!(
        text.ends_with(&format!(
            "\nDeleted the replica {ws} at {}; this workspace no longer syncs\n",
            svc.url
        )),
        "{text}"
    );
    assert!(!still_there(&svc));
}
