//! Issue numbers come from a high-water mark in `meta` and are never reused.

mod common;

use common::{user_version, Env, V0_FIXTURE};
use lcl_core::SCHEMA_VERSION;

fn next_issue_number(db: &std::path::Path) -> String {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.query_row("SELECT value FROM meta WHERE key='next_issue_number'", [], |r| r.get(0)).expect("read mark")
}

#[test]
fn deleted_numbers_are_never_reused() {
    let env = Env::new();
    env.lcl(&["init"]).assert_ok();
    assert_eq!([env.create("a"), env.create("b"), env.create("c")], [1, 2, 3]);

    env.lcl(&["issue", "delete", "3"]).assert_ok();
    assert_eq!(env.create("d"), 4, "the deleted newest number was reused");

    for n in ["1", "2", "4"] {
        env.lcl(&["issue", "delete", n]).assert_ok();
    }
    let list = env.lcl(&["issue", "list", "--json"]).assert_ok().json();
    assert_eq!(list["issues"].as_array().map(Vec::len), Some(0), "{list:#}");
    assert_eq!(env.create("e"), 5, "numbering restarted after every issue was deleted");
}

#[test]
fn upgrade_seeds_the_high_water_mark() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);

    assert_eq!(env.create("first after upgrade"), 4);
    assert_eq!(user_version(&db), SCHEMA_VERSION);
    assert_eq!(next_issue_number(&db), "5");
}

/// 0.1.0 can still write to an upgraded file, numbering by MAX+1 and leaving
/// the mark behind. Allocation must still step past what it wrote.
#[test]
fn allocation_steps_past_a_0_1_0_write_after_upgrade() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    env.lcl(&["issue", "list"]).assert_ok(); // upgrade: the mark is 4
    assert_eq!(next_issue_number(&db), "4");
    {
        // What 0.1.0's `issue create` does: MAX(number)+1, mark untouched.
        let conn = rusqlite::Connection::open(&db).expect("open db");
        conn.execute(
            "INSERT INTO issues(id,number,title,created_at,updated_at) VALUES('written-by-0.1.0',4,'from 0.1.0',0,0)",
            [],
        )
        .expect("insert");
    }
    assert_eq!(env.create("after the old binary"), 5);
    assert_eq!(next_issue_number(&db), "6");
}
