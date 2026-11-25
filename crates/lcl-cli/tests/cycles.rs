//! Cycles (step 13b).
//!
//! A cycle name is trimmed and can't be blank, and no two cycles have one,
//! comparing both trimmed and ignoring ASCII case. Names stored twice
//! before `cycle create` checked stay as they are, and stored names are
//! never rewritten. A CYCLE argument is a cycle's id (ignoring case) or
//! its name, compared the same way; a name several cycles have is refused
//! with their ids. `--cycle` puts an issue in that one cycle (an import can
//! give it several); `cycle delete` is refused while any issue is in the
//! cycle. Issue JSON lists its cycles by start date (no date last), then
//! name.

mod common;

use common::{Env, Out, V010_DIRTY_FIXTURE, dump, exec};
use serde_json::{Value, json};

fn workspace() -> Env {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env
}

fn trio(out: &Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

fn json_error(kind: &str, message: &str) -> String {
    format!("{}\n", json!({"error": {"kind": kind, "message": message}}))
}

/// `cycle create` with these arguments; the new cycle's id.
fn cycle(env: &Env, args: &[&str]) -> String {
    let mut all = vec!["--json", "cycle", "create"];
    all.extend(args);
    env.lcl(&all).assert_ok().json()["id"]
        .as_str()
        .expect("id")
        .to_string()
}

/// Issue `n`'s cycles, as issue show --json lists them.
fn cycles_of(env: &Env, n: i64) -> Value {
    env.lcl(&["--json", "issue", "show", &n.to_string()])
        .assert_ok()
        .json()["issue"]["cycles"]
        .clone()
}

fn names(cycles: &Value) -> Vec<&str> {
    cycles
        .as_array()
        .expect("cycles")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect()
}

/// Exit 1 with `message`, plain and under --json, and nothing written.
fn refused(env: &Env, args: &[&str], kind: &str, message: &str) {
    let db = env.db_path();
    let before = dump(&db);
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
    assert!(dump(&db) == before, "{args:?} wrote");
}

#[test]
fn cycle_names_are_trimmed_unique_and_not_blank() {
    let env = workspace();
    let out = env
        .lcl(&["--json", "cycle", "create", "  Sprint 1 "])
        .assert_ok()
        .json();
    assert_eq!(out["name"], "Sprint 1");
    for (name, message) in [
        ("Sprint 1", "cycle \"Sprint 1\" already exists"),
        (
            "sprint 1",
            "cycle \"sprint 1\" already exists (as \"Sprint 1\")",
        ),
        (
            " SPRINT 1 ",
            "cycle \"SPRINT 1\" already exists (as \"Sprint 1\")",
        ),
    ] {
        refused(&env, &["cycle", "create", name], "conflict", message);
    }

    // Blank is refused wherever a cycle is named.
    env.create("one");
    for args in [
        vec!["cycle", "create", " "],
        vec!["cycle", "update", "Sprint 1", "--name", ""],
        vec!["cycle", "update", " "],
        vec!["cycle", "delete", ""],
        vec!["issue", "create", "--title", "t", "--cycle", " "],
        vec!["issue", "update", "1", "--cycle", "\t"],
        vec!["issue", "list", "--cycle", ""],
    ] {
        let out = env.lcl(&args);
        assert!(
            out.code == 2
                && out.stdout.is_empty()
                && out.stderr.contains(": a cycle name can't be empty\n"),
            "{args:?}: {out:#?}"
        );
    }

    // Names stored twice before the check stay, and each is still listed.
    exec(
        &env.db_path(),
        "INSERT INTO cycles(id, name, created_at) VALUES ('00000000-0000-4000-8000-00000000000a', 'Old', 1), ('00000000-0000-4000-8000-00000000000b', ' old', 2);",
    );
    let listed = env.lcl(&["--json", "cycle", "list"]).assert_ok().json();
    let mut all: Vec<&str> = listed
        .as_array()
        .expect("list")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    all.sort_unstable();
    assert_eq!(all, [" old", "Old", "Sprint 1"]);
    refused(
        &env,
        &["cycle", "create", "OLD"],
        "conflict",
        "cycle \"OLD\" already exists (as \"Old\")",
    );
}

#[test]
fn a_cycle_is_named_by_its_id_or_its_name() {
    let env = workspace();
    let id = cycle(&env, &["Sprint 1"]);
    env.create("one");
    for arg in [id.as_str(), &id.to_uppercase(), "sprint 1", "  SPRINT 1  "] {
        env.lcl(&["issue", "update", "1", "--cycle", arg])
            .assert_ok();
        assert_eq!(
            cycles_of(&env, 1),
            json!([{"id": id, "name": "Sprint 1"}]),
            "{arg}"
        );
    }

    // A name two cycles have (they were made before names were checked).
    let (a, b) = (
        "00000000-0000-4000-8000-00000000000a",
        "00000000-0000-4000-8000-00000000000b",
    );
    exec(
        &env.db_path(),
        &format!(
            "INSERT INTO cycles(id, name, created_at) VALUES ('{b}', ' twin', 2), ('{a}', 'Twin', 1);"
        ),
    );
    let twice = format!("cycle \"twin\" names 2 cycles: {a}, {b}; give one by its id");
    let unknown = "cycle \"nope\" not found";
    for args in [
        vec!["cycle", "update", "CYCLE", "--name", "x"],
        vec!["cycle", "delete", "CYCLE"],
        vec!["issue", "create", "--title", "t", "--cycle", "CYCLE"],
        vec!["issue", "update", "1", "--cycle", "CYCLE"],
        vec!["issue", "list", "--cycle", "CYCLE"],
    ] {
        let with = |name: &'static str| {
            args.iter()
                .map(|a| if *a == "CYCLE" { name } else { a })
                .collect::<Vec<_>>()
        };
        refused(&env, &with("twin"), "conflict", &twice);
        refused(&env, &with("nope"), "not_found", unknown);
    }
    // By id, each one; once renamed, the name is no longer shared.
    env.lcl(&["cycle", "update", &a.to_uppercase(), "--name", "Twin A"])
        .assert_ok();
    env.lcl(&["issue", "update", "1", "--cycle", "TWIN"])
        .assert_ok();
    assert_eq!(cycles_of(&env, 1), json!([{"id": b, "name": " twin"}]));
}

/// The v10 repair put unreadable dates on the end of a cycle's name; a
/// blank name became "  (start: soon) (end: 2026-13-01)". It is reached by
/// what it shows, and renamed with cycle update.
#[test]
fn the_v10_repair_s_names_are_reached_by_what_they_show() {
    let env = Env::new();
    env.install_db(V010_DIRTY_FIXTURE);
    env.lcl(&["workspace", "show"]).assert_ok();
    let shown = "(start: soon) (end: 2026-13-01)";
    assert_eq!(
        env.lcl(&["--json", "issue", "list", "--cycle", shown])
            .assert_ok()
            .json()["total"],
        0
    );
    refused(
        &env,
        &["cycle", "create", "(START: soon) (end: 2026-13-01)"],
        "conflict",
        "cycle \"(START: soon) (end: 2026-13-01)\" already exists (as \"  (start: soon) (end: 2026-13-01)\")",
    );
    let fixed = env
        .lcl(&[
            "--json",
            "cycle",
            "update",
            shown,
            "--name",
            "Sprint 0",
            "--start",
            "2026-10-01",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (
            &fixed["id"],
            &fixed["name"],
            &fixed["start_date"],
            &fixed["end_date"]
        ),
        (
            &json!("29ade97e-eeac-4de5-9f98-656f5a5628c2"),
            &json!("Sprint 0"),
            &json!("2026-10-01"),
            &Value::Null
        )
    );
}

#[test]
fn cycle_update_renames_and_redates() {
    let env = workspace();
    let a = cycle(&env, &["A", "--start", "2026-11-01", "--end", "2026-11-14"]);
    cycle(&env, &["B"]);
    let created = env
        .lcl(&["--json", "cycle", "list"])
        .assert_ok()
        .json()
        .as_array()
        .expect("list")
        .iter()
        .find(|c| c["id"] == a.as_str())
        .expect("A")["created_at"]
        .clone();

    // Its own name in another case is fine; another cycle's is not.
    let out = env
        .lcl(&[
            "--json",
            "cycle",
            "update",
            "A",
            "--name",
            "a",
            "--end",
            "2026-11-30",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        out,
        json!({"id": a, "name": "a", "start_date": "2026-11-01", "end_date": "2026-11-30", "created_at": created})
    );
    refused(
        &env,
        &["cycle", "update", "a", "--name", "b"],
        "conflict",
        "cycle \"b\" already exists (as \"B\")",
    );

    // The end can't come before the start it keeps, nor the start after
    // the end it keeps.
    refused(
        &env,
        &["cycle", "update", "a", "--end", "2026-10-31"],
        "conflict",
        "cycle \"a\" would end 2026-10-31, before its start 2026-11-01",
    );
    refused(
        &env,
        &["cycle", "update", "a", "--start", "2026-12-01"],
        "conflict",
        "cycle \"a\" would end 2026-11-30, before its start 2026-12-01",
    );
    // Both typed the wrong way round: a usage error, as on create.
    let out = env.lcl(&[
        "cycle",
        "update",
        "a",
        "--start",
        "2026-12-01",
        "--end",
        "2026-11-01",
    ]);
    assert_eq!(
        trio(&out),
        (
            2,
            "",
            "error: cycle end 2026-11-01 is before its start 2026-12-01\n\nUsage: lcl cycle update [OPTIONS] <CYCLE>\n\nFor more information, try '--help'.\n"
        )
    );
    let out = env.lcl(&[
        "--json",
        "cycle",
        "update",
        "a",
        "--start",
        "2026-12-01",
        "--end",
        "2026-11-01",
    ]);
    assert_eq!(
        trio(&out),
        (
            2,
            "",
            json_error(
                "usage",
                "cycle end 2026-11-01 is before its start 2026-12-01"
            )
            .as_str()
        )
    );
    for args in [
        vec!["--start", "2026-11-01", "--no-start"],
        vec!["--end", "2026-11-01", "--no-end"],
    ] {
        let mut all = vec!["cycle", "update", "a"];
        all.extend(&args);
        assert_eq!(env.lcl(&all).code, 2, "{all:?}");
    }

    // Clearing the start lifts the check; then each date clears.
    let out = env
        .lcl(&[
            "--json",
            "cycle",
            "update",
            "a",
            "--no-start",
            "--end",
            "2026-10-01",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&out["start_date"], &out["end_date"]),
        (&Value::Null, &json!("2026-10-01"))
    );
    let out = env
        .lcl(&[
            "--json",
            "cycle",
            "update",
            "a",
            "--no-end",
            "--start",
            "2027-01-01",
        ])
        .assert_ok()
        .json();
    assert_eq!(
        (&out["start_date"], &out["end_date"]),
        (&json!("2027-01-01"), &Value::Null)
    );
    assert_eq!(
        trio(&env.lcl(&["cycle", "update", "a", "--name", "A"])),
        (0, "Updated cycle \"A\"\n", "")
    );
}

#[test]
fn cycle_delete_is_refused_while_issues_are_in_it() {
    let env = workspace();
    let s = cycle(&env, &["S"]);
    cycle(&env, &["Other"]);
    for title in ["one", "two"] {
        env.lcl(&["issue", "create", "--title", title, "--cycle", "s"])
            .assert_ok();
    }
    refused(
        &env,
        &["cycle", "delete", "s"],
        "conflict",
        "cycle \"S\" has 2 issues; move them to another cycle or none first",
    );
    env.lcl(&["issue", "update", "1", "--no-cycle"]).assert_ok();
    refused(
        &env,
        &["cycle", "delete", "S"],
        "conflict",
        "cycle \"S\" has 1 issue; move it to another cycle or none first",
    );
    env.lcl(&["issue", "update", "2", "--cycle", "other"])
        .assert_ok();
    assert_eq!(
        env.lcl(&["--json", "cycle", "delete", "S"])
            .assert_ok()
            .json(),
        json!({"deleted": "S", "id": s})
    );
    let left = env.lcl(&["--json", "cycle", "list"]).assert_ok().json();
    assert_eq!(left.as_array().map(Vec::len), Some(1));
    cycle(&env, &["T"]);
    assert_eq!(
        trio(&env.lcl(&["cycle", "delete", "t"])),
        (0, "Deleted cycle \"T\"\n", "")
    );
}

#[test]
fn an_issue_is_in_the_one_cycle_it_is_given() {
    let env = workspace();
    let later = cycle(&env, &["Later", "--start", "2026-12-01"]);
    let sooner = cycle(&env, &["Sooner", "--start", "2026-11-01"]);
    let zeta = cycle(&env, &["zeta"]);
    let alpha = cycle(&env, &["Alpha"]);

    let made = env
        .lcl(&[
            "--json", "issue", "create", "--title", "x", "--author", "ann", "--cycle", "sooner",
        ])
        .assert_ok()
        .json();
    assert_eq!(made["cycles"], json!([{"id": sooner, "name": "Sooner"}]));
    let plain = env
        .lcl(&["--json", "issue", "create", "--title", "y"])
        .assert_ok()
        .json();
    assert_eq!(plain["cycles"], json!([]));

    // --cycle replaces whatever cycles the issue is in.
    let updated = env
        .lcl(&["--json", "issue", "update", "1", "--cycle", "LATER"])
        .assert_ok()
        .json();
    assert_eq!(updated["cycles"], json!([{"id": later, "name": "Later"}]));
    assert_eq!(
        trio(&env.lcl(&["issue", "show", "1"])),
        (
            0,
            "W-1 x\nStatus: Backlog  Priority: None  Assignee: Unassigned\nCreator: ann\nCycle: Later\n",
            ""
        )
    );

    // In several, as an import can make it: by start date, no date last,
    // then by name.
    exec(&env.db_path(), &format!(
        "INSERT INTO issue_cycles(issue_id, cycle_id) SELECT id, '{sooner}' FROM issues WHERE number = 1;
         INSERT INTO issue_cycles(issue_id, cycle_id) SELECT id, '{zeta}' FROM issues WHERE number = 1;
         INSERT INTO issue_cycles(issue_id, cycle_id) SELECT id, '{alpha}' FROM issues WHERE number = 1;"
    ));
    assert_eq!(
        names(&cycles_of(&env, 1)),
        ["Sooner", "Later", "Alpha", "zeta"]
    );
    let listed = env.lcl(&["--json", "issue", "list"]).assert_ok().json();
    assert_eq!(
        names(&listed["issues"][1]["cycles"]),
        ["Sooner", "Later", "Alpha", "zeta"]
    );
    let shown = env.lcl(&["issue", "show", "1"]).assert_ok().stdout.clone();
    assert!(
        shown.contains("\nCycles: Sooner, Later, Alpha, zeta\n"),
        "{shown}"
    );
    env.lcl(&["issue", "update", "1", "--cycle", "zeta"])
        .assert_ok();
    assert_eq!(names(&cycles_of(&env, 1)), ["zeta"]);

    // --no-cycle takes it out of all of them.
    env.lcl(&["issue", "update", "1", "--no-cycle"]).assert_ok();
    assert_eq!(cycles_of(&env, 1), json!([]));
    assert!(!env.lcl(&["issue", "show", "1"]).stdout.contains("Cycle"));
    assert_eq!(
        env.lcl(&["issue", "update", "1", "--cycle", "zeta", "--no-cycle"])
            .code,
        2
    );

    // An unknown cycle writes nothing, and uses no number.
    refused(
        &env,
        &["issue", "create", "--title", "z", "--cycle", "nope"],
        "not_found",
        "cycle \"nope\" not found",
    );
    assert_eq!(
        env.lcl(&["--json", "issue", "create", "--title", "z"])
            .assert_ok()
            .json()["identifier"],
        "W-3"
    );
}

#[test]
fn issue_list_filters_by_cycle_before_the_limit() {
    let env = workspace();
    cycle(&env, &["C"]);
    for n in 1..=6 {
        env.create(&format!("issue {n}"));
    }
    for n in ["1", "2", "4", "5"] {
        env.lcl(&["issue", "update", n, "--cycle", "c"]).assert_ok();
    }
    let page = |args: &[&str]| -> (Value, Vec<String>, Value) {
        let mut all = vec!["--json", "issue", "list", "--cycle", "C"];
        all.extend(args);
        let out = env.lcl(&all).assert_ok().json();
        let ids = out["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .map(|i| i["identifier"].as_str().expect("id").to_string())
            .collect();
        (out["total"].clone(), ids, out["truncated"].clone())
    };
    assert_eq!(
        page(&["--limit", "2"]),
        (
            json!(4),
            vec!["W-5".to_string(), "W-4".to_string()],
            json!(true)
        )
    );
    assert_eq!(
        page(&["--limit", "2", "--offset", "2"]),
        (
            json!(4),
            vec!["W-2".to_string(), "W-1".to_string()],
            json!(false)
        )
    );
    let (total, _, _) = page(&["--status", "Backlog", "--all"]);
    assert_eq!(total, 4);
}

#[test]
fn cycle_list_counts_issues_in_json_only() {
    let env = workspace();
    let a = cycle(&env, &["A", "--start", "2026-11-01", "--end", "2026-11-14"]);
    let b = cycle(&env, &["B"]);
    exec(
        &env.db_path(),
        &format!(
            "UPDATE cycles SET created_at = 100 WHERE id = '{a}'; UPDATE cycles SET created_at = 200 WHERE id = '{b}';"
        ),
    );
    for title in ["one", "two"] {
        env.lcl(&["issue", "create", "--title", title, "--cycle", "a"])
            .assert_ok();
    }
    assert_eq!(
        env.lcl(&["--json", "cycle", "list"]).assert_ok().json(),
        json!([
            {"id": b, "name": "B", "start_date": null, "end_date": null, "created_at": 200, "issues": 0},
            {"id": a, "name": "A", "start_date": "2026-11-01", "end_date": "2026-11-14", "created_at": 100, "issues": 2},
        ])
    );
    assert_eq!(
        trio(&env.lcl(&["cycle", "list"])),
        (
            0,
            format!("{b}\tB\t?..?\n{a}\tA\t2026-11-01..2026-11-14\n").as_str(),
            ""
        )
    );
}

#[test]
fn export_and_import_keep_each_issue_s_cycles() {
    let env = workspace();
    cycle(&env, &["A", "--start", "2026-11-01"]);
    let b = cycle(&env, &["B"]);
    env.lcl(&["issue", "create", "--title", "in a", "--cycle", "a"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "in b", "--cycle", "b"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "in none"])
        .assert_ok();
    env.lcl(&["issue", "create", "--title", "in both", "--cycle", "a"])
        .assert_ok();
    exec(
        &env.db_path(),
        &format!(
            "INSERT INTO issue_cycles(issue_id, cycle_id) SELECT id, '{b}' FROM issues WHERE number = 4;"
        ),
    );

    let text = env.lcl(&["export"]).assert_ok().stdout.clone();
    let copy = workspace();
    copy.lcl_stdin(&["import", "-"], text.as_bytes())
        .assert_ok();
    let cycles = |e: &Env| -> Vec<(Value, Value)> {
        let out = e
            .lcl(&["--json", "issue", "list", "--all"])
            .assert_ok()
            .json();
        out["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .map(|i| (i["identifier"].clone(), i["cycles"].clone()))
            .collect()
    };
    let before = cycles(&env);
    assert_eq!(
        before
            .iter()
            .map(|(_, c)| names(c).len())
            .collect::<Vec<_>>(),
        [2, 0, 1, 1]
    );
    assert_eq!(cycles(&copy), before);
    // Same-second cycles have no order between them: compare by id.
    let listed = |e: &Env| -> Vec<Value> {
        let mut all = e
            .lcl(&["--json", "cycle", "list"])
            .assert_ok()
            .json()
            .as_array()
            .expect("list")
            .clone();
        all.sort_by_key(|c| c["id"].as_str().expect("id").to_string());
        all
    };
    assert_eq!(listed(&copy), listed(&env));
}
