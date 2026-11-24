//! `lcl sync`: opting in, pushing, numbers from blocks, and stopping,
//! against the reference service in tests/sync_service (docs/sync-protocol.md).

mod common;
mod sync_service;

use common::Env;
use serde_json::{Value, json};
use sync_service::client::*;
use sync_service::{Service, TOKEN};

#[test]
fn start_makes_the_replica_from_this_machines_whole_log() {
    let svc = Service::start();
    let env = demo();
    env.lcl(&["label", "create", "bug"]).assert_ok();
    env.lcl(&["issue", "create", "--title", "two", "--label", "bug"])
        .assert_ok();
    env.lcl(&["issue", "comment", "1", "first"]).assert_ok();
    env.lcl(&["issue", "relate", "1", "blocks", "2"])
        .assert_ok();
    let before: Vec<Value> = local_log(&env.db_path()).into_iter().map(|r| r.0).collect();

    let started = ok(&env, &["sync", "start", &svc.url]);
    let (workspace, machine) = {
        let mut s = svc.state();
        let r = s.replica();
        assert_eq!(
            (r.key.as_str(), r.name.as_str(), r.machines.len()),
            ("DEMO", "demo", 1)
        );
        // The counter started at the file's next number, 3.
        assert_eq!(r.counter, 3 + 100);
        let logged: Vec<Value> = r.log.iter().map(|l| l.change.clone()).collect();
        assert_eq!(logged, before, "the whole log, as it is in the file");
        assert!(
            r.log
                .iter()
                .all(|l| l.applied && l.machine == r.machines[0])
        );
        let workspace = s.replicas.keys().next().unwrap().clone();
        (workspace, s.replica().machines[0].clone())
    };
    assert_eq!(
        started,
        json!({"server": svc.url, "workspace": workspace, "machine": machine, "sent": before.len(), "numbers_left": 100})
    );
    // Each row is marked with its seq.
    let after = local_log(&env.db_path());
    for (i, (change, synced)) in after.iter().enumerate() {
        assert_eq!((change, *synced), (&before[i], Some(i as i64 + 1)));
    }
    // Every request carried the token and named lcl.
    for seen in &svc.state().requests {
        assert_eq!(seen.authorization.as_deref(), Some("Bearer test-token"));
        assert_eq!(
            seen.user_agent.as_deref(),
            Some(concat!("lcl/", env!("CARGO_PKG_VERSION")))
        );
    }

    let shown = ok(&env, &["workspace", "show"]);
    let last = shown["sync"]["last_sync"].as_i64().expect("last_sync");
    assert!((sync_service::now_ms() / 1000 - last).abs() < 60);
    assert_eq!(
        shown["sync"],
        json!({"server": svc.url, "workspace": workspace, "machine": machine, "last_sync": last, "numbers_left": 100})
    );
    let text = env.lcl(&["workspace", "show"]).assert_ok().stdout.clone();
    assert!(
        text.ends_with(&format!(
            "\nSyncs with {} as workspace {workspace}; 100 issue numbers held\n",
            svc.url
        )),
        "{text}"
    );

    // The token is in no file of the workspace.
    for entry in std::fs::read_dir(env.cwd.path().join(".lcl")).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()));
    }
}

#[test]
fn issues_take_numbers_from_the_blocks_once_opted_in() {
    let svc = Service::start();
    {
        let mut s = svc.state();
        s.block_size = 3;
        s.block_gap = 10;
    }
    let env = demo();
    // Fewer than 50 numbers held: it takes a second block, and stops at two.
    let started = ok(&env, &["sync", "start", &svc.url]);
    assert_eq!(started["numbers_left"], 6);
    let numbers: Vec<i64> = (0..6).map(|i| env.create(&format!("n{i}"))).collect();
    assert_eq!(numbers, [2, 3, 4, 15, 16, 17]);
    // The file's own counter is left alone.
    assert_eq!(
        meta(&env.db_path(), "next_issue_number").as_deref(),
        Some("2")
    );
    let (kind, message) = fails(&env, None, &["issue", "create", "--title", "x"], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "conflict",
            "this machine has used the issue numbers it holds; run lcl sync to get more"
        )
    );
    let other_writes = ok(&env, &["issue", "update", "2", "--title", "still fine"]);
    assert_eq!(other_writes["title"], "still fine");

    let synced = ok(&env, &["sync"]);
    assert_eq!(synced["numbers_left"], 6);
    assert_eq!(env.create("after"), 28);
    assert_eq!(ok(&env, &["workspace", "show"])["sync"]["numbers_left"], 5);

    // Stopped, it numbers from past the highest number used.
    ok(&env, &["sync", "stop"]);
    assert_eq!(env.create("local again"), 29);
}

#[test]
fn sync_sends_what_the_service_lacks_once() {
    let svc = Service::start();
    let env = demo();
    let started = ok(&env, &["sync", "start", &svc.url]);
    let machine = started["machine"].as_str().unwrap().to_string();
    let first = local_log(&env.db_path()).len();

    env.lcl(&["issue", "update", "1", "--status", "Done"])
        .assert_ok();
    env.lcl(&["issue", "comment", "1", "done"]).assert_ok();
    let new = unsynced(&env.db_path());
    assert!(new.len() >= 3, "{new:#?}");
    assert_eq!(
        ok(&env, &["sync"]),
        json!({"sent": new.len(), "received": 0, "numbers_left": 100, "not_applied": [], "overwritten": []})
    );
    assert_eq!(
        ok(&env, &["sync"]),
        json!({"sent": 0, "received": 0, "numbers_left": 100, "not_applied": [], "overwritten": []})
    );
    let text = with_token(&env, &["sync"]).assert_ok().stdout.clone();
    assert_eq!(
        text,
        "Sent 0 changes, received 0 changes; 100 issue numbers held\n"
    );

    // Writes before opting in carry `local`, after it the machine's id.
    let log = local_log(&env.db_path());
    for (i, (change, synced)) in log.iter().enumerate() {
        let clock = change["clock"].as_str().unwrap();
        let ends = if i < first {
            "-local".to_string()
        } else {
            format!("-{machine}")
        };
        assert!(clock.ends_with(&ends), "{clock}");
        assert_eq!(*synced, Some(i as i64 + 1));
    }
    let mut s = svc.state();
    let sent: Vec<Value> = s.replica().log.iter().map(|l| l.change.clone()).collect();
    assert_eq!(sent, log.into_iter().map(|r| r.0).collect::<Vec<_>>());
}

#[test]
fn a_push_whose_answer_was_lost_is_sent_again_and_kept_once() {
    let svc = Service::start();
    let env = demo();
    ok(&env, &["sync", "start", &svc.url]);
    env.lcl(&["issue", "update", "1", "--title", "renamed"])
        .assert_ok();
    let edit = unsynced(&env.db_path());
    svc.state().lose_next_push_answer = true;
    let (kind, message) = fails(&env, Some(TOKEN), &["sync"], 1);
    assert_eq!(kind, "network", "{message}");
    assert!(message.contains("edits stay in this file"), "{message}");
    // The service has it; the file doesn't know yet.
    assert_eq!(unsynced(&env.db_path()), edit);
    let logged = svc.state().replica().log.len();
    assert_eq!(logged, local_log(&env.db_path()).len());

    assert_eq!(ok(&env, &["sync"])["sent"], edit.len());
    let mut s = svc.state();
    let r = s.replica();
    assert_eq!(r.log.len(), logged, "kept once");
    let synced: Vec<Option<i64>> = local_log(&env.db_path()).into_iter().map(|r| r.1).collect();
    let seqs: Vec<Option<i64>> = r.log.iter().map(|l| Some(l.seq)).collect();
    assert_eq!(synced, seqs);
}

#[test]
fn a_start_whose_push_failed_is_finished_by_sync() {
    let svc = Service::start();
    let env = demo();
    svc.state().lose_next_push_answer = true;
    let (kind, _) = fails(&env, Some(TOKEN), &["sync", "start", &svc.url], 1);
    assert_eq!(kind, "network");
    // The replica was made and the file records it.
    let shown = ok(&env, &["workspace", "show"]);
    assert_eq!(shown["sync"]["last_sync"], Value::Null);
    assert_eq!(shown["sync"]["numbers_left"], 100);
    let all = local_log(&env.db_path()).len();
    assert_eq!(ok(&env, &["sync"])["sent"], all);
    let mut s = svc.state();
    assert_eq!(s.replica().log.len(), all, "the lost push kept once");
}

#[test]
fn pushes_hold_whole_writes_and_at_most_500_changes() {
    let svc = Service::start();
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    // Writes of 700, 200, 200, 200 and 1 rows: one clock each.
    let conn = rusqlite::Connection::open(env.db_path()).unwrap();
    let mut n = 0;
    for (w, size) in [700, 200, 200, 200, 1].into_iter().enumerate() {
        conn.execute(
            "UPDATE meta SET value = printf('%013d-0000-local', 1700000000000 + ?1) WHERE key = 'clock'",
            [w as i64],
        )
        .unwrap();
        conn.execute_batch("BEGIN").unwrap();
        for _ in 0..size {
            n += 1;
            conn.execute(
                "INSERT INTO labels(id, name, color) VALUES (?1, ?2, '#000000')",
                [format!("00000000-0000-4000-8000-{n:012}"), format!("l{n}")],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
    }
    drop(conn);
    assert_eq!(local_log(&env.db_path()).len(), 1301);

    assert_eq!(ok(&env, &["sync", "start", &svc.url])["sent"], 1301);
    let mut s = svc.state();
    let r = s.replica();
    let sizes: Vec<usize> = r.pushes.iter().map(Vec::len).collect();
    assert_eq!(
        sizes,
        [700, 400, 201],
        "a write alone when it has more than 500"
    );
    for (i, clocks) in r.push_clocks.iter().enumerate() {
        for later in &r.push_clocks[i + 1..] {
            assert!(clocks.iter().all(|c| !later.contains(c)), "a write split");
        }
    }
}

#[test]
fn the_token_comes_from_the_environment_and_the_address_must_be_https() {
    let svc = Service::start();
    let env = demo();
    let (kind, message) = fails(&env, None, &["sync", "start", &svc.url], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "auth",
            "set LCL_SYNC_TOKEN to your token for the sync service"
        )
    );
    let (kind, message) = fails(&env, Some("not-it"), &["sync", "start", &svc.url], 1);
    assert_eq!(kind, "auth");
    assert_eq!(
        message,
        format!(
            "the sync service at {} refused the token: not a token this service knows",
            svc.url
        )
    );
    assert!(svc.state().replicas.is_empty());
    for url in [
        "http://sync.example",
        "ftp://sync.example",
        "https://",
        "sync.example",
    ] {
        let (kind, message) = fails(&env, Some(TOKEN), &["sync", "start", url], 2);
        assert_eq!(kind, "usage", "{url}: {message}");
        assert!(
            message.contains("is not a sync service address"),
            "{message}"
        );
    }
    // A bad address is a usage error, token or not.
    assert_eq!(
        fails(&env, None, &["sync", "start", "http://x.example"], 2).0,
        "usage"
    );
    assert_eq!(ok(&env, &["workspace", "show"])["sync"], Value::Null);
}

#[test]
fn with_the_service_out_of_reach_the_file_takes_edits_that_go_later() {
    let svc = Service::start();
    let env = demo();
    ok(&env, &["sync", "start", &svc.url]);
    svc.state().down = true;
    env.lcl(&["issue", "update", "1", "--priority", "high"])
        .assert_ok();
    env.create("offline");
    let waiting = unsynced(&env.db_path());
    let (kind, message) = fails(&env, Some(TOKEN), &["sync"], 1);
    assert_eq!(kind, "network");
    assert_eq!(
        message,
        format!(
            "can't reach the sync service at {}: it answered 503: down for the test; edits stay in this file, and go at the next lcl sync",
            svc.url
        )
    );
    assert_eq!(unsynced(&env.db_path()), waiting);
    svc.state().down = false;
    assert_eq!(ok(&env, &["sync"])["sent"], waiting.len());

    // Nothing listening at all.
    let gone = Service::start();
    let other = demo();
    ok(&other, &["sync", "start", &gone.url]);
    drop(gone);
    other.create("offline");
    let (kind, message) = fails(&other, Some(TOKEN), &["sync"], 1);
    assert_eq!(kind, "network", "{message}");
    assert_eq!(unsynced(&other.db_path()).len(), 1);
}

#[test]
fn a_clock_far_ahead_of_the_services_is_refused_whole() {
    let svc = Service::start();
    let env = demo();
    ok(&env, &["sync", "start", &svc.url]);
    env.lcl(&["issue", "update", "1", "--title", "later"])
        .assert_ok();
    let logged = svc.state().replica().log.len();
    svc.state().skew_ms = -120_000;
    let (kind, message) = fails(&env, Some(TOKEN), &["sync"], 1);
    assert_eq!(
        (kind.as_str(), message.as_str()),
        (
            "conflict",
            "this machine's clock is 120 seconds ahead of the sync service's; set it right, then run lcl sync again"
        )
    );
    assert_eq!(svc.state().replica().log.len(), logged);
    svc.state().skew_ms = 0;
    assert_eq!(ok(&env, &["sync"])["sent"], 1);
}

#[test]
fn changes_the_service_did_not_apply_are_reported_and_its_purges_win() {
    let svc = Service::start();
    let env = demo();
    env.create("two");
    env.lcl(&["label", "create", "bug"]).assert_ok();
    let started = ok(&env, &["sync", "start", &svc.url]);
    let ws = started["workspace"].as_str().unwrap().to_string();
    let issue = issue_id(&env, 1);
    let label = ok(&env, &["label", "list"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Purged there, as from a browser, before this machine heard of it.
    svc.state().purge(&ws, "issue", &issue);
    svc.state().purge(&ws, "label", &label);
    env.lcl(&["issue", "update", "1", "--title", "edited"])
        .assert_ok();
    let comment = ok(&env, &["issue", "comment", "1", "too late"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    env.lcl(&["label", "delete", "bug"]).assert_ok();
    let report = ok(&env, &["sync"]);
    assert_eq!(
        report["not_applied"],
        json!([
            {"kind": "issue", "record": issue, "identifier": "DEMO-1", "field": "title"},
            {"kind": "comment", "record": comment, "identifier": "DEMO-1", "field": "*"},
            {"kind": "label", "record": label, "identifier": null, "field": "deleted_at"},
        ])
    );
    // Then the purges came, and won: the issue went with its comment.
    assert_eq!(report["received"], 2);
    assert_eq!(fails(&env, None, &["issue", "show", "1"], 1).0, "not_found");
    assert_eq!(ok(&env, &["label", "list"]), json!([]));
    assert!(unsynced(&env.db_path()).is_empty(), "nothing to send back");
    let conn = rusqlite::Connection::open(env.db_path()).unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM changes WHERE record IN (?1, ?2, ?3) AND field <> '*'",
            [&issue, &comment, &label],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "a purge takes the history with it");
    drop(conn);

    let two = issue_id(&env, 2);
    svc.state().purge(&ws, "issue", &two);
    env.lcl(&["issue", "update", "2", "--priority", "low"])
        .assert_ok();
    let text = with_token(&env, &["sync"]).assert_ok().stdout.clone();
    assert_eq!(
        text,
        "Sent 1 change, received 1 change; 100 issue numbers held\nNot applied by the service (purged there, or in conflict): DEMO-2 priority\n"
    );
}

#[test]
fn stop_forgets_the_service_and_start_makes_a_new_replica() {
    let svc = Service::start();
    let env = demo();
    let started = ok(&env, &["sync", "start", &svc.url]);
    let (kind, message) = fails(&env, Some(TOKEN), &["sync", "start", &svc.url], 1);
    assert_eq!(
        (kind.as_str(), message),
        (
            "conflict",
            format!(
                "this workspace syncs with {} already; lcl sync sends and receives changes",
                svc.url
            )
        )
    );
    assert_eq!(svc.state().replicas.len(), 1);

    assert_eq!(
        ok(&env, &["sync", "stop"]),
        json!({"server": svc.url, "workspace": started["workspace"]})
    );
    assert_eq!(ok(&env, &["workspace", "show"])["sync"], Value::Null);
    let conn = rusqlite::Connection::open(env.db_path()).unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM meta WHERE key LIKE 'sync%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    drop(conn);
    for args in [&["sync"][..], &["sync", "stop"]] {
        let (kind, message) = fails(&env, Some(TOKEN), args, 1);
        assert_eq!(
            (kind.as_str(), message.as_str()),
            (
                "conflict",
                "this workspace doesn't sync; lcl sync start URL opts it in"
            )
        );
    }
    assert_eq!(fails(&env, None, &["sync", "start", &svc.url], 1).0, "auth");

    // Again: a new replica, from the whole log.
    let again = ok(&env, &["sync", "start", &svc.url]);
    assert_ne!(again["workspace"], started["workspace"]);
    assert_eq!(again["sent"], local_log(&env.db_path()).len());
    let s = svc.state();
    let old = &s.replicas[started["workspace"].as_str().unwrap()];
    assert_eq!(
        old.log.len(),
        started["sent"].as_u64().unwrap() as usize,
        "left as it was"
    );
}

#[test]
fn a_replica_the_service_no_longer_has_is_not_found() {
    let svc = Service::start();
    let env = demo();
    ok(&env, &["sync", "start", &svc.url]);
    svc.state().replicas.clear();
    env.lcl(&["issue", "update", "1", "--title", "t"])
        .assert_ok();
    let (kind, message) = fails(&env, Some(TOKEN), &["sync"], 1);
    assert_eq!(
        (kind.as_str(), message),
        (
            "not_found",
            format!(
                "the sync service at {} has no replica of this workspace",
                svc.url
            )
        )
    );
}

#[test]
fn a_workspace_that_never_opted_in_makes_no_call() {
    let svc = Service::start();
    let env = demo();
    with_token(&env, &["issue", "update", "1", "--title", "t"]).assert_ok();
    with_token(&env, &["issue", "list"]).assert_ok();
    assert_eq!(ok(&env, &["workspace", "show"])["sync"], Value::Null);
    let text = env.lcl(&["workspace", "show"]).assert_ok().stdout.clone();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert_eq!(fails(&env, Some(TOKEN), &["sync"], 1).0, "conflict");
    assert!(svc.state().requests.is_empty());
    // The clocks still end in `local`.
    assert!(
        local_log(&env.db_path()).iter().all(|(c, s)| c["clock"]
            .as_str()
            .unwrap()
            .ends_with("-local")
            && s.is_none())
    );
}
