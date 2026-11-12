//! Integration-test harness: runs the real `lcl` binary against a throwaway
//! HOME and working directory, so no test can read or write the real `~/.lcl`.
// Each test binary compiles its own copy of this module and uses a subset of it.
#![allow(dead_code)]

use rusqlite::types::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Barrier;
use tempfile::TempDir;

/// Builds an owned argv for `Env::lcl_parallel`.
pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A database written by lcl 0.1.0. See tests/schema.rs for how it was built.
pub const V0_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0.sqlite");

/// What one `lcl` invocation produced.
#[derive(Debug)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Panics with the full output unless the process exited 0.
    #[track_caller]
    pub fn assert_ok(&self) -> &Self {
        assert_eq!(self.code, 0, "lcl exited {}\n{self:#?}", self.code);
        self
    }

    /// Parses stdout as JSON, panicking with the full output if it isn't.
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e})\n{self:#?}"))
    }
}

/// An isolated environment: a fresh HOME and a fresh cwd, both deleted on drop.
pub struct Env {
    pub home: TempDir,
    pub cwd: TempDir,
}

impl Env {
    pub fn new() -> Self {
        let env = Self {
            home: TempDir::new().expect("create temp HOME"),
            cwd: TempDir::new().expect("create temp cwd"),
        };
        // lcl discovers a project by walking up from cwd looking for
        // `.lcl/data.sqlite`. A stray one above the temp dir would leak into
        // every test that runs before `init`, so refuse to run rather than
        // test against it.
        for dir in env.cwd.path().ancestors().skip(1) {
            let stray = dir.join(".lcl").join("data.sqlite");
            assert!(!stray.exists(), "stray lcl database at {} would leak into tests", stray.display());
        }
        env
    }

    /// Runs `lcl <args>` with HOME and cwd pointed at this environment.
    pub fn lcl(&self, args: &[&str]) -> Out {
        let output = Command::new(env!("CARGO_BIN_EXE_lcl"))
            .args(args)
            .current_dir(self.cwd.path())
            .env("HOME", self.home.path())
            // `--project` reads LCL_PROJECT; an exported value must not select
            // a project behind the test's back.
            .env_remove("LCL_PROJECT")
            .output()
            .expect("spawn lcl");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output
            .status
            .code()
            .unwrap_or_else(|| panic!("lcl {args:?} killed by a signal\nstdout: {stdout}\nstderr: {stderr}"));
        Out { code, stdout, stderr }
    }

    /// Runs every command at once: one thread per command, all released
    /// together by a barrier. Outputs come back in the order given.
    pub fn lcl_parallel(&self, cmds: &[Vec<String>]) -> Vec<Out> {
        let barrier = Barrier::new(cmds.len());
        std::thread::scope(|s| {
            let handles: Vec<_> = cmds
                .iter()
                .map(|cmd| {
                    let barrier = &barrier;
                    s.spawn(move || {
                        let args: Vec<&str> = cmd.iter().map(String::as_str).collect();
                        barrier.wait();
                        self.lcl(&args)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread panicked")).collect()
        })
    }

    /// `issue create --title <title> --json`, returning the new issue's number.
    #[track_caller]
    pub fn create(&self, title: &str) -> i64 {
        let out = self.lcl(&["issue", "create", "--title", title, "--json"]);
        out.assert_ok().json()["number"].as_i64().unwrap_or_else(|| panic!("no number in {out:#?}"))
    }

    /// The project database lcl discovers from this env's cwd.
    pub fn db_path(&self) -> PathBuf {
        self.cwd.path().join(".lcl").join("data.sqlite")
    }

    /// Copies `fixture` into place as this env's project database and returns
    /// its path. Always copy: opening a WAL database creates -wal/-shm files
    /// beside it, and a fixture must never be modified.
    pub fn install_db(&self, fixture: &str) -> PathBuf {
        let dst = self.db_path();
        std::fs::create_dir_all(dst.parent().unwrap()).expect("create .lcl");
        std::fs::copy(fixture, &dst).unwrap_or_else(|e| panic!("copy {fixture}: {e}"));
        dst
    }
}

/// Reads `PRAGMA user_version` directly, bypassing lcl.
pub fn user_version(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.pragma_query_value(None, "user_version", |r| r.get(0)).expect("read user_version")
}

/// Writes `PRAGMA user_version` directly, bypassing lcl.
pub fn set_user_version(db: &Path, v: i64) {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.pragma_update(None, "user_version", v).expect("write user_version");
}

/// The schema plus every row of every table, for before/after comparisons.
#[derive(Debug, PartialEq)]
pub struct Dump {
    pub schema: Vec<Vec<Value>>,
    pub tables: Vec<(String, Vec<Vec<Value>>)>,
}

pub fn dump(db: &Path) -> Dump {
    let conn = rusqlite::Connection::open(db).expect("open db");
    let rows = |sql: &str| -> Vec<Vec<Value>> {
        let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let n = stmt.column_count();
        stmt.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())
            .and_then(Iterator::collect)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    };
    let schema = rows("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY name");
    let tables = schema
        .iter()
        .filter(|r| r[0] == Value::Text("table".into()))
        .filter_map(|r| match (&r[1], &r[3]) {
            (Value::Text(name), Value::Text(sql)) if !name.starts_with("sqlite_") => Some((name.clone(), sql)),
            _ => None,
        })
        .map(|(t, sql)| {
            // Include rowid where there is one: the FTS index maps rowids, so
            // a changed rowid is a real change. WITHOUT ROWID tables (FTS5's
            // _config and _idx) are ordered by every column instead.
            let data = if sql.contains("WITHOUT ROWID") {
                let n = conn.prepare(&format!("SELECT * FROM \"{t}\"")).expect("prepare").column_count();
                let order: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
                rows(&format!("SELECT * FROM \"{t}\" ORDER BY {}", order.join(",")))
            } else {
                rows(&format!("SELECT rowid, * FROM \"{t}\" ORDER BY rowid"))
            };
            (t, data)
        })
        .collect();
    Dump { schema, tables }
}
