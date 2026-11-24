//! Driving lcl against the service: the helpers the sync tests share.
#![allow(dead_code)]

use super::TOKEN;
use crate::common::{Env, Out, error_text};
use serde_json::{Value, json};
use std::path::Path;

/// `lcl ARGS` with the service's token in LCL_SYNC_TOKEN.
pub fn with_token(env: &Env, args: &[&str]) -> Out {
    env.lcl_env(&[("LCL_SYNC_TOKEN", TOKEN)], args)
}

#[track_caller]
pub fn ok(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    with_token(env, &argv).assert_ok().json()
}

/// A workspace DEMO with issue DEMO-1.
pub fn demo() -> Env {
    let env = Env::new();
    env.lcl(&["init", "demo", "--key", "DEMO"]).assert_ok();
    env.create("one");
    env
}

/// The file's change log, each row as it travels, with its `synced`.
pub fn local_log(db: &Path) -> Vec<(Value, Option<i64>)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT seq, clock, kind, record, field, old, new, author, synced
             FROM changes ORDER BY seq",
        )
        .unwrap();
    let json = |t: Option<String>| {
        t.map(|t| serde_json::from_str::<Value>(&t).unwrap())
            .unwrap_or(Value::Null)
    };
    stmt.query_map([], |r| {
        Ok((
            json!({
                "id": r.get::<_, i64>(0)?,
                "clock": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "record": r.get::<_, String>(3)?,
                "field": r.get::<_, String>(4)?,
                "old": json(r.get(5)?),
                "new": json(r.get(6)?),
                "author": r.get::<_, Option<String>>(7)?,
            }),
            r.get::<_, Option<i64>>(8)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

pub fn meta(db: &Path, key: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

pub fn unsynced(db: &Path) -> Vec<Value> {
    local_log(db)
        .into_iter()
        .filter(|(_, s)| s.is_none())
        .map(|(c, _)| c)
        .collect()
}

/// The issue's id, from `issue show`.
pub fn issue_id(env: &Env, n: i64) -> String {
    let shown = ok(env, &["issue", "show", &n.to_string()]);
    shown["issue"]["id"].as_str().unwrap().to_string()
}

/// The kind and message of a `--json` failure, after checking it exits
/// `code`. (contract.rs checks that the exit is the same without `--json`.)
#[track_caller]
pub fn fails(env: &Env, token: Option<&str>, args: &[&str], code: i32) -> (String, String) {
    let vars: Vec<(&str, &str)> = token.map(|t| ("LCL_SYNC_TOKEN", t)).into_iter().collect();
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = env.lcl_env(&vars, &argv);
    assert_eq!(out.code, code, "{argv:?}\n{out:#?}");
    assert_eq!(out.stdout, "", "{out:#?}");
    let error: Value = serde_json::from_str(out.stderr.lines().last().unwrap()).unwrap();
    let kind = error["error"]["kind"].as_str().unwrap().to_string();
    let message = error["error"]["message"].as_str().unwrap().to_string();
    assert_eq!(out.stderr, error_text(&argv, &kind, &message));
    (kind, message)
}
