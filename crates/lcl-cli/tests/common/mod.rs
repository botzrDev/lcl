//! Integration-test harness: runs the real `lcl` binary against a throwaway
//! HOME and working directory, so no test can read or write the real `~/.lcl`.
// Each test binary compiles its own copy of this module and uses a subset of it.
#![allow(dead_code)]

use rusqlite::types::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Barrier;
use tempfile::TempDir;

/// Builds an owned argv for `Env::lcl_parallel`.
pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// A database written by lcl 0.1.0. See tests/schema.rs for how it was built.
pub const V0_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0.sqlite");

/// A 0.1.0 database whose issues have parents typed in every form 0.1.0
/// accepted. See tests/parents.rs for how it was built.
pub const V0_PARENTS_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0-parents.sqlite");

/// A 0.1.0 database with duplicate labels, labels attached by raw name in
/// mixed case and by id, blank labels and blank assignees. See
/// tests/labels.rs for how it was built.
pub const V0_LABELS_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0-labels.sqlite");

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

    /// Runs `lcl <args>` with HOME and cwd pointed at this environment, and
    /// stdin empty (not a terminal).
    pub fn lcl(&self, args: &[&str]) -> Out {
        self.run(args, None, Stdio::null())
    }

    /// As `lcl`, with `input` piped to stdin.
    pub fn lcl_stdin(&self, args: &[&str], input: &[u8]) -> Out {
        self.run(args, Some(input), Stdio::piped())
    }

    /// As `lcl`, with stdin opened on /dev/null.
    pub fn lcl_devnull(&self, args: &[&str]) -> Out {
        self.run(args, None, std::fs::File::open("/dev/null").expect("open /dev/null").into())
    }

    /// The command `lcl` runs, for wrapping in another program.
    pub fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.cwd.path())
            .env("HOME", self.home.path())
            // `--project` reads LCL_PROJECT; an exported value must not select
            // a project behind the test's back. Likewise LCL_STDIN_WAIT.
            .env_remove("LCL_PROJECT")
            .env_remove("LCL_STDIN_WAIT");
        cmd
    }

    fn run(&self, args: &[&str], input: Option<&[u8]>, stdin: Stdio) -> Out {
        let mut child = self
            .command(env!("CARGO_BIN_EXE_lcl"))
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn lcl");
        // Feed stdin from another thread so a large input can't deadlock
        // against output. A write error means lcl exited without reading it
        // all, which the caller sees in the exit code.
        let output = std::thread::scope(|s| {
            if let (Some(input), Some(mut pipe)) = (input, child.stdin.take()) {
                s.spawn(move || {
                    let _ = pipe.write_all(input);
                });
            }
            child.wait_with_output().expect("wait for lcl")
        });
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

/// Runs SQL directly against `db`, bypassing lcl (to fake hand edits or
/// older binaries).
pub fn exec(db: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.execute_batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// One database row, keyed by column name.
pub type Row = BTreeMap<String, Value>;

/// Every column of issue `number`, read directly.
pub fn issue_row(db: &Path, number: i64) -> Row {
    let conn = rusqlite::Connection::open(db).expect("open db");
    rows(&conn, &format!("SELECT * FROM issues WHERE number = {number}")).pop().unwrap_or_else(|| panic!("no issue {number}"))
}

fn rows(conn: &rusqlite::Connection, sql: &str) -> Vec<Row> {
    let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    stmt.query_map([], |r| (0..names.len()).map(|i| Ok((names[i].clone(), r.get::<_, Value>(i)?))).collect())
        .and_then(Iterator::collect)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Rows of `PRAGMA foreign_key_check`: empty when every foreign key holds.
pub fn foreign_key_violations(db: &Path) -> Vec<Row> {
    let conn = rusqlite::Connection::open(db).expect("open db");
    rows(&conn, "PRAGMA foreign_key_check")
}

/// Undoes migration 5's schema changes on an upgraded dump, after checking
/// them: issue_labels rebuilt with cascading foreign keys (its CREATE
/// statement is dropped from the comparison), and the new unique index on
/// label names (removed).
pub fn undo_v5_schema(after: &mut Dump, before: &mut Dump) {
    let t = |s: &str| Value::Text(s.into());
    let index = after.schema.iter().position(|r| r["name"] == t("labels_name")).expect("labels_name index");
    let index = after.schema.remove(index);
    assert_eq!(index["sql"], t("CREATE UNIQUE INDEX labels_name ON labels(name COLLATE NOCASE)"));
    let sql = after.schema.iter().find(|r| r["name"] == t("issue_labels")).map(|r| format!("{:?}", r["sql"])).unwrap_or_default();
    assert!(
        sql.contains("issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE")
            && sql.contains("label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE"),
        "{sql}"
    );
    for d in [after, before] {
        d.schema.retain(|r| r["name"] != t("issue_labels"));
    }
}

/// The schema plus every row of every table, for before/after comparisons.
#[derive(Debug, PartialEq)]
pub struct Dump {
    pub schema: Vec<Row>,
    pub tables: BTreeMap<String, Vec<Row>>,
}

pub fn dump(db: &Path) -> Dump {
    let conn = rusqlite::Connection::open(db).expect("open db");
    let schema = rows(&conn, "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY name");
    let mut tables = BTreeMap::new();
    for entry in &schema {
        let (Value::Text(kind), Value::Text(name), Value::Text(sql)) = (&entry["type"], &entry["name"], &entry["sql"]) else { continue };
        if kind != "table" || name.starts_with("sqlite_") {
            continue;
        }
        // Include rowid where there is one: the FTS index maps rowids, so a
        // changed rowid is a real change. WITHOUT ROWID tables (FTS5's
        // _config and _idx) are ordered by every column instead.
        let data = if sql.contains("WITHOUT ROWID") {
            let n = conn.prepare(&format!("SELECT * FROM \"{name}\"")).expect("prepare").column_count();
            let order: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
            rows(&conn, &format!("SELECT * FROM \"{name}\" ORDER BY {}", order.join(",")))
        } else {
            rows(&conn, &format!("SELECT rowid AS \"(rowid)\", * FROM \"{name}\" ORDER BY rowid"))
        };
        tables.insert(name.clone(), data);
    }
    Dump { schema, tables }
}
