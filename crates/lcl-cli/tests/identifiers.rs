//! Naming issues. One resolver turns what a user typed into an issue:
//! `KEY-N` (the key in any case, but it must be this workspace's), `#N`, `N`,
//! or a full UUID in either case. A malformed id, or another workspace's key,
//! exits 2. A well-formed id with no issue behind it exits 1 and names the
//! issue as `KEY-N`, whatever form was typed. Issues print as `KEY-N`.

mod common;

use common::{dump, Env};

/// A workspace with key DREA.
fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "dreamd"]).assert_ok();
    env
}

fn uuid(env: &Env, n: i64) -> String {
    let shown = env.lcl(&["issue", "show", &n.to_string(), "--json"]).assert_ok().json();
    shown["issue"]["id"].as_str().expect("issue id").to_string()
}

/// Every accepted spelling of issue `n`.
fn forms(n: i64, uuid: &str) -> Vec<String> {
    vec![format!("DREA-{n}"), format!("drea-{n}"), format!("Drea-{n}"), format!("#{n}"), n.to_string(), uuid.to_string(), uuid.to_uppercase()]
}

/// Every command that takes an issue, with `{}` where the issue goes. The
/// `--flag=value` form lets empty and hyphen-leading values through.
const COMMANDS: &[&[&str]] = &[
    &["issue", "show", "{}"],
    &["issue", "update", "{}", "--title", "renamed"],
    &["issue", "delete", "{}"],
    &["issue", "comment", "{}", "hello"],
    &["issue", "comments", "{}"],
    &["issue", "create", "--title", "child", "--parent={}"],
    &["issue", "update", "1", "--parent={}"],
    &["issue", "list", "--parent={}"],
];

fn with(cmd: &[&str], id: &str) -> Vec<String> {
    cmd.iter().map(|a| a.replace("{}", id)).collect()
}

fn run(env: &Env, args: &[String]) -> common::Out {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    env.lcl(&args)
}

#[test]
fn every_accepted_form_names_the_same_issue() {
    let env = workspace();
    assert_eq!(env.create("target"), 1);
    let u = uuid(&env, 1);

    for (i, f) in forms(1, &u).iter().enumerate() {
        let shown = env.lcl(&["issue", "show", f, "--json"]).assert_ok().json();
        assert_eq!((shown["issue"]["number"].as_i64(), shown["issue"]["identifier"].as_str()), (Some(1), Some("DREA-1")), "show {f}");

        let title = format!("via {f}");
        let updated = env.lcl(&["issue", "update", f, "--title", &title, "--json"]).assert_ok().json();
        assert_eq!((updated["number"].as_i64(), updated["title"].as_str()), (Some(1), Some(title.as_str())), "update {f}");

        let comment = env.lcl(&["issue", "comment", f, &title, "--json"]).assert_ok().json();
        assert_eq!(comment["issue_id"], u.as_str(), "comment {f}");
        let comments = env.lcl(&["issue", "comments", f, "--json"]).assert_ok().json();
        assert_eq!(comments.as_array().map(Vec::len), Some(i + 1), "comments {f}");

        let child = env.lcl(&["issue", "create", "--title", "child", &format!("--parent={f}"), "--json"]).assert_ok().json();
        assert_eq!((child["parent_id"].as_str(), child["parent_identifier"].as_str()), (Some(u.as_str()), Some("DREA-1")), "create --parent {f}");
        let listed = env.lcl(&["issue", "list", &format!("--parent={f}"), "--json"]).assert_ok().json();
        let listed: Vec<_> = listed.as_array().expect("array").iter().map(|i| i["number"].clone()).collect();
        assert!(listed.contains(&child["number"]), "list --parent {f}: {listed:?}");

        let other = env.create("other");
        let moved = env.lcl(&["issue", "update", &other.to_string(), &format!("--parent={f}"), "--json"]).assert_ok().json();
        assert_eq!(moved["parent_identifier"], "DREA-1", "update --parent {f}");
    }

    // delete, each form on an issue of its own
    for i in 0..forms(0, "").len() {
        let n = env.create("doomed");
        let f = &forms(n, &uuid(&env, n))[i];
        let out = env.lcl(&["issue", "delete", f]).assert_ok().stdout.clone();
        assert_eq!(out, format!("Deleted DREA-{n}\n"), "delete {f}");
        let gone = env.lcl(&["issue", "show", &n.to_string()]);
        assert_eq!(gone.code, 1, "{gone:#?}");
        assert!(gone.stderr.contains(&format!("issue DREA-{n} not found")), "{gone:#?}");
    }
}

#[test]
fn another_workspaces_key_exits_2_and_changes_nothing() {
    let env = workspace();
    env.create("one");
    env.lcl(&["issue", "comment", "1", "a comment"]).assert_ok();
    let db = env.db_path();
    let before = dump(&db);

    for given in ["BZR-1", "bzr-1", "DREAM-1", "DRE-1", "MY-DREA-1"] {
        for cmd in COMMANDS {
            let args = with(cmd, given);
            let out = run(&env, &args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&format!("{given} is not in this workspace (key DREA)")), "{args:?}: {out:#?}");
        }
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn malformed_ids_exit_2() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    let before = dump(&db);

    let bad = [
        "DREA-", "DREA-x", "0", "-3", "abc", "", "#", "#0", "#-1", "#x", "+1", "1.5", "DREA-1.5", "DREA-0", "DREA_1", "DREA 1",
        " 1", "1 ", "99999999999999999999", "0f8fad5bd9cb469fa16570867728950e", "{0f8fad5b-d9cb-469f-a165-70867728950e}",
        "0f8fad5b-d9cb-469f-a165-70867728950g",
    ];
    for b in bad {
        for cmd in COMMANDS {
            let args = with(cmd, b);
            let out = run(&env, &args);
            assert_eq!(out.code, 2, "{args:?}: {out:#?}");
            assert!(
                out.stderr.contains(&format!("invalid issue id \"{b}\"; expected KEY-N, #N, N or an issue UUID")),
                "{args:?}: {out:#?}"
            );
        }
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn a_missing_issue_is_named_as_key_n() {
    let env = workspace();
    env.create("one");
    let db = env.db_path();
    let before = dump(&db);

    let missing_uuid = "00000000-0000-4000-8000-000000000000";
    for (given, named) in [
        ("DREA-7", "DREA-7"),
        ("drea-7", "DREA-7"),
        ("#7", "DREA-7"),
        ("7", "DREA-7"),
        (missing_uuid, missing_uuid),
        ("00000000-0000-4000-8000-00000000000A", "00000000-0000-4000-8000-00000000000a"),
    ] {
        for cmd in COMMANDS {
            let args = with(cmd, given);
            let out = run(&env, &args);
            assert_eq!(out.code, 1, "{args:?}: {out:#?}");
            assert!(out.stderr.contains(&format!("issue {named} not found")), "{args:?}: {out:#?}");
        }
    }
    assert_eq!(dump(&db), before);
}

#[test]
fn issues_print_as_key_n() {
    let env = workspace();
    assert_eq!(env.lcl(&["issue", "create", "--title", "first"]).assert_ok().stdout, "Created DREA-1\n");
    let created = env.lcl(&["issue", "create", "--title", "second", "--json"]).assert_ok().json();
    assert_eq!(created["identifier"], "DREA-2");
    assert!(created["parent_id"].is_null() && created["parent_identifier"].is_null(), "{created:#}");

    let shown = env.lcl(&["issue", "show", "2", "--json"]).assert_ok().json();
    assert_eq!(shown["issue"]["identifier"], "DREA-2");
    assert_eq!(shown["children"], serde_json::json!([]));
    assert_eq!(env.lcl(&["issue", "show", "1"]).assert_ok().stdout.lines().next(), Some("DREA-1 first"));

    assert_eq!(env.lcl(&["issue", "update", "#2", "--title", "renamed"]).assert_ok().stdout, "Updated DREA-2\n");
    let updated = env.lcl(&["issue", "update", "drea-2", "--title", "zebra", "--json"]).assert_ok().json();
    assert_eq!(updated["identifier"], "DREA-2");

    let list = env.lcl(&["issue", "list"]).assert_ok().stdout.clone();
    let firsts: Vec<_> = list.lines().map(|l| l.split('\t').next().unwrap_or_default()).collect();
    assert_eq!(firsts, ["DREA-2", "DREA-1"], "{list}");
    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    let ids: Vec<_> = list.as_array().expect("array").iter().map(|i| i["identifier"].clone()).collect();
    assert_eq!(ids, ["DREA-2", "DREA-1"]);

    let found = env.lcl(&["search", "zebra", "--json"]).assert_ok().json();
    assert_eq!(found["issues"][0]["identifier"], "DREA-2", "{found:#}");
    assert_eq!(env.lcl(&["search", "zebra"]).assert_ok().stdout, "DREA-2\t[Backlog]\tzebra\ttitle\n");

    assert_eq!(env.lcl(&["issue", "delete", "2"]).assert_ok().stdout, "Deleted DREA-2\n");
    let u = uuid(&env, 1);
    let deleted = env.lcl(&["issue", "delete", "DREA-1", "--json"]).assert_ok().json();
    assert_eq!(deleted, serde_json::json!({"deleted": "DREA-1", "id": u}));
}
