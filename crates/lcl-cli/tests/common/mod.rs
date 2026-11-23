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

/// What lcl prints on stderr for an error that isn't clap's: `Error: M`,
/// or, when `args` has `--json` before any `--`, one JSON line,
/// `{"error":{"kind":K,"message":M}}`.
pub fn error_text(args: &[&str], kind: &str, message: &str) -> String {
    if args
        .iter()
        .take_while(|a| **a != "--")
        .any(|a| *a == "--json")
    {
        format!(
            "{}\n",
            serde_json::json!({"error": {"kind": kind, "message": message}})
        )
    } else {
        format!("Error: {message}\n")
    }
}

/// Builds an owned argv for `Env::lcl_parallel`.
pub fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// lcl 0.1.0's workspace index, and the file of one workspace it lists. See
/// tests/workspaces.rs for how they were built.
pub const V0_INDEX_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v0-index.sqlite"
);
pub const V0_INDEX_ALPHA_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v0-index-alpha.sqlite"
);

/// lcl 0.1.0's index holding a cwd workspace (made by its `init`) and a
/// global one, the shape of a real index. See tests/workspaces.rs.
pub const V0_INDEX_CWD_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v0-index-cwd.sqlite"
);

/// A workspace index written by a build of 097af5c, which opened it as a
/// workspace: user_version 7. See tests/workspaces.rs.
pub const V7_INDEX_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v7-index.sqlite"
);

/// A workspace written by a build of 097af5c (schema v7) with a label, a
/// parent, a blocks and a duplicate relation, a comment and a doc. See
/// tests/projects.rs for how it was built.
pub const V7_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v7.sqlite");

/// A workspace written by a build of 8cffde1 (schema v8) with a project, a
/// milestone, an issue in both, a blocks relation and a comment. See
/// tests/refs.rs for how it was built.
pub const V8_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v8.sqlite");

/// A database written by lcl 0.1.0. See tests/schema.rs for how it was built.
pub const V0_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0.sqlite");

/// A 0.1.0 database whose issues have parents typed in every form 0.1.0
/// accepted. See tests/parents.rs for how it was built.
pub const V0_PARENTS_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v0-parents.sqlite"
);

/// A 0.1.0 database with duplicate labels, labels attached by raw name in
/// mixed case and by id, blank labels and blank assignees. See
/// tests/labels.rs for how it was built.
pub const V0_LABELS_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v0-labels.sqlite"
);

/// A 0.1.0 database holding every value 0.1.0 accepted that lcl can't
/// read, for migration 10. See tests/v010_repairs.rs for how it was built.
pub const V010_DIRTY_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/v010-dirty.sqlite"
);

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

/// A fresh temp directory, deleted on drop, named by the path the OS gives
/// back for it. lcl prints the cwd as the OS reports it, and on macOS the
/// temp directory is reached through a symlink (`/var` is
/// `/private/var`), so a path built from the unresolved name would never
/// match. On Windows the name is kept as it is: resolving it adds a `\\?\`
/// prefix that lcl doesn't print.
pub fn temp_dir() -> TempDir {
    let root = std::env::temp_dir();
    let root = if cfg!(windows) {
        root
    } else {
        std::fs::canonicalize(&root).unwrap_or_else(|e| panic!("resolve {}: {e}", root.display()))
    };
    TempDir::new_in(&root)
        .unwrap_or_else(|e| panic!("create a temp dir in {}: {e}", root.display()))
}

/// The null device, to open as a file.
pub const NULL_DEVICE: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// An isolated environment: a fresh HOME and a fresh cwd, both deleted on drop.
pub struct Env {
    pub home: TempDir,
    pub cwd: TempDir,
}

impl Env {
    pub fn new() -> Self {
        let env = Self {
            home: temp_dir(),
            cwd: temp_dir(),
        };
        // lcl discovers a project by walking up from cwd looking for
        // `.lcl/data.sqlite`. A stray one above the temp dir would leak into
        // every test that runs before `init`, so refuse to run rather than
        // test against it.
        for dir in env.cwd.path().ancestors().skip(1) {
            let stray = dir.join(".lcl").join("data.sqlite");
            assert!(
                !stray.exists(),
                "stray lcl database at {} would leak into tests",
                stray.display()
            );
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

    /// As `lcl`, with stdin opened on the null device (`/dev/null`, or `NUL`
    /// on Windows).
    pub fn lcl_devnull(&self, args: &[&str]) -> Out {
        self.run(
            args,
            None,
            std::fs::File::open(NULL_DEVICE)
                .unwrap_or_else(|e| panic!("open {NULL_DEVICE}: {e}"))
                .into(),
        )
    }

    /// The command `lcl` runs, for wrapping in another program.
    pub fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.cwd.path())
            .env("HOME", self.home.path())
            // `--workspace` reads LCL_WORKSPACE, or else the deprecated
            // LCL_PROJECT; an exported value must not select a workspace
            // behind the test's back. Likewise LCL_STDIN_WAIT.
            .env_remove("LCL_WORKSPACE")
            .env_remove("LCL_PROJECT")
            .env_remove("LCL_STDIN_WAIT")
            // An exported LCL_AUTHOR must not name the tests' creators and
            // comment authors either. USER is left as it is; a test that
            // checks an author sets or removes it.
            .env_remove("LCL_AUTHOR")
            // Human output shows timestamps in local time; tests read them
            // in UTC unless they set TZ themselves.
            .env("TZ", "UTC");
        cmd
    }

    /// As `lcl`, with these environment variables set.
    pub fn lcl_env(&self, vars: &[(&str, &str)], args: &[&str]) -> Out {
        self.lcl_with(args, |cmd| {
            cmd.envs(vars.iter().copied());
        })
    }

    /// As `lcl`, after `adjust` has changed the command (to unset HOME, say).
    pub fn lcl_with(&self, args: &[&str], adjust: impl FnOnce(&mut Command)) -> Out {
        self.run_with(args, None, Stdio::null(), adjust)
    }

    /// As `lcl_stdin`, after `adjust` has changed the command.
    pub fn lcl_stdin_with(
        &self,
        args: &[&str],
        input: &[u8],
        adjust: impl FnOnce(&mut Command),
    ) -> Out {
        self.run_with(args, Some(input), Stdio::piped(), adjust)
    }

    fn run(&self, args: &[&str], input: Option<&[u8]>, stdin: Stdio) -> Out {
        self.run_with(args, input, stdin, |_| {})
    }

    fn run_with(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        stdin: Stdio,
        adjust: impl FnOnce(&mut Command),
    ) -> Out {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_lcl"));
        adjust(&mut cmd);
        let mut child = cmd
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
        let code = output.status.code().unwrap_or_else(|| {
            panic!("lcl {args:?} killed by a signal\nstdout: {stdout}\nstderr: {stderr}")
        });
        Out {
            code,
            stdout,
            stderr,
        }
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
            handles
                .into_iter()
                .map(|h| h.join().expect("thread panicked"))
                .collect()
        })
    }

    /// `issue create --title <title> --json`, returning the new issue's number.
    #[track_caller]
    pub fn create(&self, title: &str) -> i64 {
        let out = self.lcl(&["issue", "create", "--title", title, "--json"]);
        out.assert_ok().json()["number"]
            .as_i64()
            .unwrap_or_else(|| panic!("no number in {out:#?}"))
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

/// Every file under this env's HOME and cwd, with its bytes.
pub fn files(env: &Env) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), std::fs::read(&path).expect("read file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(env.home.path(), &mut out);
    walk(env.cwd.path(), &mut out);
    out
}

/// Reads `PRAGMA user_version` directly, bypassing lcl.
pub fn user_version(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("read user_version")
}

/// Writes `PRAGMA user_version` directly, bypassing lcl.
pub fn set_user_version(db: &Path, v: i64) {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.pragma_update(None, "user_version", v)
        .expect("write user_version");
}

/// Runs SQL directly against `db`, bypassing lcl (to fake hand edits or
/// older binaries).
pub fn exec(db: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(db).expect("open db");
    conn.execute_batch(sql)
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// One database row, keyed by column name.
pub type Row = BTreeMap<String, Value>;

/// Every column of issue `number`, read directly.
pub fn issue_row(db: &Path, number: i64) -> Row {
    let conn = rusqlite::Connection::open(db).expect("open db");
    rows(
        &conn,
        &format!("SELECT * FROM issues WHERE number = {number}"),
    )
    .pop()
    .unwrap_or_else(|| panic!("no issue {number}"))
}

fn rows(conn: &rusqlite::Connection, sql: &str) -> Vec<Row> {
    let mut stmt = conn.prepare(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    let names: Vec<String> = stmt.column_names().map(String::from).collect();
    stmt.query_map([], |r| {
        (0..names.len())
            .map(|i| Ok((names[i].clone(), r.get::<_, Value>(i)?)))
            .collect()
    })
    .and_then(Iterator::collect)
    .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Every FTS index lcl keeps.
pub const FTS_TABLES: [&str; 3] = ["issues_fts", "docs_fts", "comments_fts"];

/// Runs FTS5's integrity-check on each table, panicking on a mismatch. The
/// `1` in the rank column matters: for an external-content table, only
/// then is the index compared with the content table's rows. Without it
/// the check covers the index's internal structure alone.
pub fn fts_integrity(db: &Path, tables: &[&str]) {
    for t in tables {
        exec(
            db,
            &format!("INSERT INTO {t}({t}, rank) VALUES('integrity-check', 1);"),
        );
    }
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
    let index = after
        .schema
        .iter()
        .position(|r| r["name"] == t("labels_name"))
        .expect("labels_name index");
    let index = after.schema.remove(index);
    assert_eq!(
        index["sql"],
        t("CREATE UNIQUE INDEX labels_name ON labels(name COLLATE NOCASE)")
    );
    let sql = after
        .schema
        .iter()
        .find(|r| r["name"] == t("issue_labels"))
        .map(|r| format!("{:?}", r["sql"]))
        .unwrap_or_default();
    assert!(
        sql.contains("issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE")
            && sql.contains("label_id TEXT NOT NULL REFERENCES labels(id) ON DELETE CASCADE"),
        "{sql}"
    );
    for d in [after, before] {
        d.schema.retain(|r| r["name"] != t("issue_labels"));
    }
}

/// Undoes migration 6's schema changes on an upgraded dump, after checking
/// them: the comments_fts table (with its shadow tables, rows dropped too)
/// and its three triggers. Check the index itself with FTS5's
/// integrity-check.
pub fn undo_v6_schema(after: &mut Dump) {
    let t = |s: &str| Value::Text(s.into());
    let fts = after
        .schema
        .iter()
        .find(|r| r["name"] == t("comments_fts"))
        .expect("comments_fts");
    assert_eq!(
        fts["sql"],
        t(
            "CREATE VIRTUAL TABLE comments_fts USING fts5(body, content='comments', content_rowid='rowid')"
        )
    );
    for trigger in ["comments_ai", "comments_ad", "comments_au"] {
        let row = after
            .schema
            .iter()
            .find(|r| r["name"] == t(trigger))
            .unwrap_or_else(|| panic!("trigger {trigger}"));
        assert_eq!(
            (&row["type"], &row["tbl_name"]),
            (&t("trigger"), &t("comments")),
            "{trigger}"
        );
    }
    let ours = |name: &Value| matches!(name, Value::Text(n) if n.starts_with("comments_fts") || ["comments_ai", "comments_ad", "comments_au"].contains(&n.as_str()));
    after.schema.retain(|r| !ours(&r["name"]));
    after
        .tables
        .retain(|name, _| !name.starts_with("comments_fts"));
}

/// Undoes migration 7's schema changes on an upgraded dump, after checking
/// them: the issue_relations table (empty after an upgrade), its primary-key
/// index, its reverse index, and the one-duplicate-target index.
pub fn undo_v7_schema(after: &mut Dump) {
    let t = |s: &str| Value::Text(s.into());
    let table = after
        .schema
        .iter()
        .find(|r| r["name"] == t("issue_relations"))
        .expect("issue_relations");
    let Value::Text(sql) = &table["sql"] else {
        panic!("{table:?}")
    };
    for part in [
        "issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE",
        "related_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE",
        "kind TEXT NOT NULL CHECK (kind IN ('blocks', 'related', 'duplicate'))",
        "PRIMARY KEY (issue_id, related_id, kind)",
        "CHECK (issue_id <> related_id)",
    ] {
        assert!(sql.contains(part), "{part} missing from {sql}");
    }
    let index = after
        .schema
        .iter()
        .find(|r| r["name"] == t("issue_relations_related"))
        .expect("issue_relations_related");
    assert_eq!(
        index["sql"],
        t("CREATE INDEX issue_relations_related ON issue_relations(related_id, kind)")
    );
    let one = after
        .schema
        .iter()
        .find(|r| r["name"] == t("issue_relations_one_duplicate"))
        .expect("issue_relations_one_duplicate");
    assert_eq!(
        one["sql"],
        t(
            "CREATE UNIQUE INDEX issue_relations_one_duplicate ON issue_relations(issue_id) WHERE kind = 'duplicate'"
        )
    );
    assert_eq!(
        after.tables.get("issue_relations").map(Vec::len),
        Some(0),
        "an upgrade adds no relations"
    );
    let ours = |name: &Value| matches!(name, Value::Text(n) if n.starts_with("issue_relations") || n.starts_with("sqlite_autoindex_issue_relations"));
    after.schema.retain(|r| !ours(&r["name"]));
    after.tables.remove("issue_relations");
}

/// Migration 8's SQL, as sqlite_master keeps it: (name, CREATE statement).
/// Written out here, not taken from the migration, so that a change to
/// either fails the upgrade tests.
pub const V8_SCHEMA: [(&str, &str); 8] = [
    ("projects", "CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'Backlog' CHECK (status IN ('Backlog', 'Planned', 'Started', 'Paused', 'Completed', 'Canceled')),
    target_date TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
)"),
    ("projects_name", "CREATE UNIQUE INDEX projects_name ON projects(name COLLATE NOCASE)"),
    ("milestones", "CREATE TABLE milestones (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    target_date TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (project_id, name COLLATE NOCASE)
)"),
    ("issues_project", "CREATE INDEX issues_project ON issues(project_id)"),
    ("issues_milestone", "CREATE INDEX issues_milestone ON issues(milestone_id)"),
    ("issues_milestone_in_project_insert", "CREATE TRIGGER issues_milestone_in_project_insert BEFORE INSERT ON issues
WHEN NEW.milestone_id IS NOT NULL AND (SELECT project_id FROM milestones WHERE id = NEW.milestone_id) IS NOT NEW.project_id
BEGIN
    SELECT RAISE(ABORT, 'milestone is not in the issue''s project');
END"),
    ("issues_milestone_in_project_update", "CREATE TRIGGER issues_milestone_in_project_update BEFORE UPDATE OF project_id, milestone_id ON issues
WHEN NEW.milestone_id IS NOT NULL AND (SELECT project_id FROM milestones WHERE id = NEW.milestone_id) IS NOT NEW.project_id
BEGIN
    SELECT RAISE(ABORT, 'milestone is not in the issue''s project');
END"),
    ("milestones_stay_in_project", "CREATE TRIGGER milestones_stay_in_project BEFORE UPDATE OF project_id ON milestones
WHEN NEW.project_id IS NOT OLD.project_id
BEGIN
    SELECT RAISE(ABORT, 'a milestone cannot move to another project');
END"),
];

/// What migration 8's two ADD COLUMNs append to the issues table's CREATE
/// statement, before its closing parenthesis.
pub const V8_ISSUE_COLUMNS: &str =
    ", project_id TEXT REFERENCES projects(id), milestone_id TEXT REFERENCES milestones(id))";

/// Undoes migration 8's schema changes on an upgraded dump, after checking
/// them: the exact SQL of every table, index and trigger it adds (and the
/// autoindexes behind their keys), both new tables empty, the two new issue
/// columns null on every issue and appended to the issues CREATE statement,
/// which gets its v7 text back.
pub fn undo_v8_schema(after: &mut Dump) {
    let t = |s: &str| Value::Text(s.into());
    for (name, sql) in V8_SCHEMA {
        let row = after
            .schema
            .iter()
            .find(|r| r["name"] == t(name))
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(row["sql"], t(sql), "{name}");
    }
    for (name, table) in [
        ("sqlite_autoindex_projects_1", "projects"),
        ("sqlite_autoindex_milestones_1", "milestones"),
        ("sqlite_autoindex_milestones_2", "milestones"),
    ] {
        let row = after
            .schema
            .iter()
            .find(|r| r["name"] == t(name))
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(
            (&row["type"], &row["tbl_name"], &row["sql"]),
            (&t("index"), &t(table), &Value::Null),
            "{name}"
        );
    }
    let ours = |name: &Value| matches!(name, Value::Text(n) if V8_SCHEMA.iter().any(|(v8, _)| v8 == n) || n.starts_with("sqlite_autoindex_projects") || n.starts_with("sqlite_autoindex_milestones"));
    after.schema.retain(|r| !ours(&r["name"]));
    for table in ["projects", "milestones"] {
        assert_eq!(
            after.tables.remove(table).map(|rows| rows.len()),
            Some(0),
            "an upgrade adds no {table}"
        );
    }
    for row in after.tables.get_mut("issues").expect("issues table") {
        assert_eq!(row.remove("project_id"), Some(Value::Null), "{row:?}");
        assert_eq!(row.remove("milestone_id"), Some(Value::Null), "{row:?}");
    }
    let issues = after
        .schema
        .iter_mut()
        .find(|r| r["name"] == t("issues"))
        .expect("issues");
    let Value::Text(sql) = &issues["sql"] else {
        panic!("{issues:?}")
    };
    let v7 = sql
        .strip_suffix(V8_ISSUE_COLUMNS)
        .unwrap_or_else(|| panic!("the v8 columns aren't at the end of {sql}"));
    issues.insert("sql".into(), Value::Text(format!("{v7})")));
}

/// Migration 9's index, as sqlite_master keeps it.
pub const V9_REF_INDEX: &str =
    "CREATE UNIQUE INDEX issues_ref ON issues(ref COLLATE NOCASE) WHERE ref IS NOT NULL";

/// What migration 9's two ADD COLUMNs append to the issues table's CREATE
/// statement, before its closing parenthesis.
pub const V9_ISSUE_COLUMNS: &str = ", ref TEXT, creator TEXT)";

/// Undoes migration 9's schema changes on an upgraded dump, after checking
/// them: the ref index's exact SQL, the two new issue columns null on every
/// issue and appended to the issues CREATE statement, which gets its v8
/// text back. Call it before `undo_v8_schema`, whose columns come before
/// these.
pub fn undo_v9_schema(after: &mut Dump) {
    let t = |s: &str| Value::Text(s.into());
    let index = after
        .schema
        .iter()
        .find(|r| r["name"] == t("issues_ref"))
        .expect("issues_ref missing");
    assert_eq!(
        (&index["type"], &index["tbl_name"], &index["sql"]),
        (&t("index"), &t("issues"), &t(V9_REF_INDEX))
    );
    after.schema.retain(|r| r["name"] != t("issues_ref"));
    for row in after.tables.get_mut("issues").expect("issues table") {
        assert_eq!(row.remove("ref"), Some(Value::Null), "{row:?}");
        assert_eq!(row.remove("creator"), Some(Value::Null), "{row:?}");
    }
    let issues = after
        .schema
        .iter_mut()
        .find(|r| r["name"] == t("issues"))
        .expect("issues");
    let Value::Text(sql) = &issues["sql"] else {
        panic!("{issues:?}")
    };
    let v8 = sql
        .strip_suffix(V9_ISSUE_COLUMNS)
        .unwrap_or_else(|| panic!("the v9 columns aren't at the end of {sql}"));
    issues.insert("sql".into(), Value::Text(format!("{v8})")));
}

/// Undoes migration 10's changes on an upgraded dump: there is nothing to
/// undo. V10 changes no schema, and it only rewrites values 0.1.0 accepted
/// that lcl can't read; the fixtures the upgrade tests use hold none (each
/// upgrades without printing a line, which tests/v010_repairs.rs checks), so
/// their rows are as V9 left them. Kept so each upgrade test names every
/// migration it undoes. Call it before `undo_v9_schema`.
pub fn undo_v10_schema(_after: &mut Dump) {}

/// Undoes migration 11's schema changes on an upgraded dump, after checking
/// them. v11 rebuilds `issues`, `comments`, `docs` and `issue_cycles` and
/// keeps their rowids, indexes and triggers. This puts the four CREATE
/// statements back: comments, docs and issue_cycles as `before` has them,
/// and issues as `before` has them plus the ADD COLUMN tails v3, v8 and v9
/// would have left, which `undo_v9_schema` and `undo_v8_schema` then strip.
/// Call it before `undo_v10_schema`.
pub fn undo_v11_schema(after: &mut Dump, before: &Dump) {
    let t = |s: &str| Value::Text(s.into());
    let sql_of = |dump: &Dump, name: &str| -> String {
        let row = dump
            .schema
            .iter()
            .find(|r| r["name"] == t(name))
            .unwrap_or_else(|| panic!("{name} missing"));
        match &row["sql"] {
            Value::Text(s) => s.clone(),
            other => panic!("{name} sql is {other:?}"),
        }
    };
    let issues = sql_of(after, "issues");
    for part in [
        "CHECK (status IN ('Backlog', 'Todo', 'InProgress', 'InReview', 'Done', 'Canceled', 'Duplicate'))",
        "parent_id TEXT REFERENCES issues(id) ON DELETE RESTRICT",
    ] {
        assert!(issues.contains(part), "{part} missing from {issues}");
    }
    let comments = sql_of(after, "comments");
    assert!(
        comments.contains("issue_id TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE"),
        "{comments}"
    );
    let docs = sql_of(after, "docs");
    assert!(
        docs.contains("parent_id TEXT REFERENCES docs(id) ON DELETE RESTRICT"),
        "{docs}"
    );
    let cycles = sql_of(after, "issue_cycles");
    assert!(
        cycles.contains("REFERENCES issues(id) ON DELETE CASCADE")
            && cycles.contains("REFERENCES cycles(id) ON DELETE RESTRICT"),
        "{cycles}"
    );

    let insert_before_paren = |sql: &str, addition: &str| {
        let end = sql
            .rfind(')')
            .unwrap_or_else(|| panic!("no closing parenthesis in {sql}"));
        format!("{}{addition}{}", &sql[..end], &sql[end..])
    };
    let mut restored = sql_of(before, "issues");
    if !restored.contains("started_at") {
        restored = insert_before_paren(&restored, ", started_at INTEGER, canceled_at INTEGER");
    }
    if !restored.contains("project_id") {
        restored = insert_before_paren(
            &restored,
            ", project_id TEXT REFERENCES projects(id), milestone_id TEXT REFERENCES milestones(id)",
        );
    }
    if !restored.contains(", ref TEXT") {
        restored = insert_before_paren(&restored, ", ref TEXT, creator TEXT");
    }
    let set_sql = |dump: &mut Dump, name: &str, sql: String| {
        let row = dump
            .schema
            .iter_mut()
            .find(|r| r["name"] == t(name))
            .unwrap_or_else(|| panic!("{name} missing"));
        row.insert("sql".into(), Value::Text(sql));
    };
    set_sql(after, "issues", restored);
    for name in ["comments", "docs", "issue_cycles"] {
        set_sql(after, name, sql_of(before, name));
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
    let schema = rows(
        &conn,
        "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY name",
    );
    let mut tables = BTreeMap::new();
    for entry in &schema {
        let (Value::Text(kind), Value::Text(name), Value::Text(sql)) =
            (&entry["type"], &entry["name"], &entry["sql"])
        else {
            continue;
        };
        if kind != "table" || name.starts_with("sqlite_") {
            continue;
        }
        // Include rowid where there is one: the FTS index maps rowids, so a
        // changed rowid is a real change. WITHOUT ROWID tables (FTS5's
        // _config and _idx) are ordered by every column instead.
        let data = if sql.contains("WITHOUT ROWID") {
            let n = conn
                .prepare(&format!("SELECT * FROM \"{name}\""))
                .expect("prepare")
                .column_count();
            let order: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
            rows(
                &conn,
                &format!("SELECT * FROM \"{name}\" ORDER BY {}", order.join(",")),
            )
        } else {
            rows(
                &conn,
                &format!("SELECT rowid AS \"(rowid)\", * FROM \"{name}\" ORDER BY rowid"),
            )
        };
        tables.insert(name.clone(), data);
    }
    Dump { schema, tables }
}
