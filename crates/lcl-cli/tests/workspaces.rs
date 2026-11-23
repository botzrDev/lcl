//! Workspaces: the registry at ~/.lcl/index.sqlite, which only
//! lcl_core::registry opens, and the rename of projects to workspaces
//! (--workspace / -w, LCL_WORKSPACE, `lcl workspace`).
//!
//! tests/fixtures/v0-index.sqlite and v0-index-alpha.sqlite were made with
//! lcl 0.1.0 (~/.local/bin/lcl, byte-identical to a build of c699c62), with
//! HOME set to a temp dir, LCL_PROJECT unset, run from an empty temp cwd:
//!
//!     lcl project create alpha --global
//!     lcl project create "R&D" --global --key RD
//!
//! and then ~/.lcl/index.sqlite and ~/.lcl/projects/alpha.sqlite were copied.
//! 0.1.0 opened the index as a workspace, so it holds 0.1.0's workspace
//! tables as well as `projects`, whose paths point into that temp HOME. It is
//! at user_version 0 in WAL mode.
//!
//! tests/fixtures/v0-index-cwd.sqlite was made by the same 0.1.0 (copied to
//! $F/bin/lcl and run as `env -i PATH="$F/bin:/usr/bin:/bin" HOME=$F/home
//! sh -e` from an empty $F/cwd/testlcl) with
//!
//!     lcl init
//!     lcl project create globaldemo --global
//!
//! and then $F/home/.lcl/index.sqlite was copied. Its `projects` table has a
//! cwd row (testlcl, at $F/cwd/testlcl/.lcl/data.sqlite) and a global one
//! (globaldemo, at $F/home/.lcl/projects/globaldemo.sqlite): the shape of a
//! real index. User_version 0, WAL mode.
//!
//! tests/fixtures/v7-index.sqlite was made the same way by a build of 097af5c
//! (`git archive`, built in its own target dir), with
//! `lcl project create beta --global --key BETA`. That build ran the
//! workspace migrations V1–V7 on the index: user_version 7.
//!
//! No fixture left a -wal or -shm file. Tests copy them; never open one in
//! place.

mod common;

use common::{
    Dump, Env, V0_FIXTURE, V0_INDEX_ALPHA_FIXTURE, V0_INDEX_CWD_FIXTURE, V0_INDEX_FIXTURE,
    V7_INDEX_FIXTURE, argv, dump, error_text, exec, files, user_version,
};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// When 0.1.0 made the v0-index fixture's two workspaces.
const V0_CREATED: i64 = 1794522404;

fn index(env: &Env) -> PathBuf {
    env.home.path().join(".lcl").join("index.sqlite")
}

fn projects_dir(env: &Env) -> PathBuf {
    env.home.path().join(".lcl").join("projects")
}

/// Points a fixture index's recorded paths into this env's HOME, as if the
/// old lcl had run here, so whether a file is missing doesn't depend on what
/// exists on the machine that runs the test.
fn repoint(env: &Env, idx: &Path) -> String {
    let home = env.home.path().display().to_string();
    let conn = rusqlite::Connection::open(idx).expect("open index");
    conn.execute(
        "UPDATE projects SET path = ?1 || substr(path, instr(path, '/.lcl/'))",
        [&home],
    )
    .expect("repoint paths");
    home
}

/// The file of global workspace `slug` under `home`, as lcl prints it.
fn global_file(home: &str, slug: &str) -> String {
    Path::new(home)
        .join(".lcl")
        .join("projects")
        .join(format!("{slug}.sqlite"))
        .display()
        .to_string()
}

/// Copies `fixture` to `rel` under this env's HOME.
fn install(env: &Env, fixture: &str, rel: &str) -> PathBuf {
    let dst = env.home.path().join(rel);
    std::fs::create_dir_all(dst.parent().unwrap()).expect("create dirs");
    std::fs::copy(fixture, &dst).unwrap_or_else(|e| panic!("copy {fixture}: {e}"));
    dst
}

/// The registry's rows, (slug, name, path), read directly.
fn registry_rows(env: &Env) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(index(env)).expect("open index");
    let mut stmt = conn
        .prepare("SELECT slug, name, path FROM workspaces ORDER BY slug")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// Takes the registry's own tables out of a dump of the index, after
/// checking its version row, so what's left is what older builds wrote.
fn undo_registry(after: &mut Dump) {
    let text = |s: &str| SqlValue::Text(s.into());
    let meta = &after.tables["registry_meta"];
    assert_eq!(meta.len(), 1, "{meta:?}");
    assert_eq!(
        (&meta[0]["key"], &meta[0]["value"]),
        (&text("schema_version"), &text("2"))
    );
    let ours = [
        "registry_meta",
        "workspaces",
        "sqlite_autoindex_registry_meta_1",
        "sqlite_autoindex_workspaces_1",
        "workspaces_path",
    ];
    let before = after.schema.len();
    after
        .schema
        .retain(|r| !ours.iter().any(|n| r["name"] == text(n)));
    assert_eq!(
        before - after.schema.len(),
        ours.len(),
        "the registry adds exactly these schema entries"
    );
    after.tables.remove("registry_meta");
    after.tables.remove("workspaces");
}

fn workspace(env: &Env, name: &str, key: &str) -> Value {
    env.lcl(&["workspace", "create", name, "--key", key, "--json"])
        .assert_ok()
        .json()
}

fn show(env: &Env, name: &str) -> Value {
    env.lcl(&["-w", name, "workspace", "show", "--json"])
        .assert_ok()
        .json()
}

#[test]
fn upgrades_a_0_1_0_index() {
    let env = Env::new();
    let idx = install(&env, V0_INDEX_FIXTURE, ".lcl/index.sqlite");
    install(&env, V0_INDEX_ALPHA_FIXTURE, ".lcl/projects/alpha.sqlite");
    let home = repoint(&env, &idx);
    assert_eq!(user_version(&idx), 0);
    let before = dump(&idx);

    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    assert_eq!(
        list,
        json!([
            {"name": "R&D", "slug": "r-d", "kind": "global", "path": global_file(&home, "r-d"), "created_at": V0_CREATED, "missing": true},
            {"name": "alpha", "slug": "alpha", "kind": "global", "path": global_file(&home, "alpha"), "created_at": V0_CREATED, "missing": false},
        ])
    );
    let human = env.lcl(&["workspace", "list"]).assert_ok().stdout.clone();
    assert_eq!(
        human,
        format!(
            "R&D\tr-d\t{}\t(missing)\nalpha\talpha\t{}\n",
            global_file(&home, "r-d"),
            global_file(&home, "alpha")
        )
    );

    let issues = env
        .lcl(&["-w", "alpha", "issue", "list", "--json"])
        .assert_ok()
        .json();
    assert_eq!(issues["total"], 0, "{issues:#}");
    assert_eq!(
        env.lcl(&["-w", "alpha", "issue", "create", "--title", "first"])
            .assert_ok()
            .stdout,
        "Created ALPH-1\n"
    );
    let alpha = show(&env, "alpha");
    assert_eq!(
        (&alpha["key"], &alpha["created_at"]),
        (&json!("ALPH"), &json!(V0_CREATED))
    );

    workspace(&env, "beta", "BETA");
    let names: Vec<_> = env
        .lcl(&["workspace", "list", "--json"])
        .assert_ok()
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].clone())
        .collect();
    assert_eq!(names, [json!("R&D"), json!("alpha"), json!("beta")]);

    // The registry added its own two tables and changed nothing else.
    let mut after = dump(&idx);
    undo_registry(&mut after);
    assert_eq!(after, before);
    assert_eq!(
        user_version(&idx),
        0,
        "the registry doesn't use user_version"
    );
}

/// Builds from c546b8a to 097af5c opened the index as a workspace and left it
/// at user_version 7. The registry ignores that number, and the tables.
#[test]
fn opens_an_index_a_dev_build_migrated() {
    let env = Env::new();
    let idx = install(&env, V7_INDEX_FIXTURE, ".lcl/index.sqlite");
    let home = repoint(&env, &idx);
    assert_eq!(user_version(&idx), 7);
    let before = dump(&idx);

    // beta's file wasn't installed.
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    assert_eq!(list.as_array().map(Vec::len), Some(1), "{list:#}");
    assert_eq!(
        (&list[0]["name"], &list[0]["slug"], &list[0]["missing"]),
        (&json!("beta"), &json!("beta"), &json!(true))
    );
    assert_eq!(list[0]["path"], json!(global_file(&home, "beta")));

    workspace(&env, "gamma", "GAM");
    assert_eq!(
        env.lcl(&["-w", "gamma", "issue", "create", "--title", "g"])
            .assert_ok()
            .stdout,
        "Created GAM-1\n"
    );
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["name"].clone(), w["missing"].clone()))
        .collect();
    assert_eq!(
        names,
        [(json!("beta"), json!(true)), (json!("gamma"), json!(false))]
    );

    let mut after = dump(&idx);
    undo_registry(&mut after);
    assert_eq!(after, before);
    assert_eq!(user_version(&idx), 7);
}

/// An index from a newer lcl is refused, and left as it is, by everything
/// that reads it: `workspace list` and `create`, and `--workspace` (or
/// LCL_WORKSPACE), which looks names up there. Opening a guessed file would
/// be worse. A workspace found from the current directory doesn't read the
/// index, so it still works.
/// The registry's schema version, read directly.
fn registry_version(env: &Env) -> String {
    let conn = rusqlite::Connection::open(index(env)).expect("open index");
    conn.query_row(
        "SELECT value FROM registry_meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )
    .expect("schema_version")
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// An index of the shape a real one has, made by 0.1.0: a cwd workspace
/// (its `init`) and a global one. After the upgrade the global row is found
/// under the current HOME, whatever path it recorded, and the cwd row at
/// its recorded path.
#[test]
fn a_0_1_0_index_s_cwd_and_global_rows_are_told_apart() {
    let env = Env::new();
    let idx = install(&env, V0_INDEX_CWD_FIXTURE, ".lcl/index.sqlite");
    // The cwd row is pointed into this test's cwd; the global row keeps the
    // path 0.1.0 recorded under the HOME it was made in.
    let cwd_db = env
        .cwd
        .path()
        .join("testlcl")
        .join(".lcl")
        .join("data.sqlite");
    exec(
        &idx,
        &format!(
            "UPDATE projects SET path = '{}' WHERE name = 'testlcl'",
            cwd_db.display()
        ),
    );
    let global_db = projects_dir(&env).join("globaldemo.sqlite");
    let entry = |name: &str, kind: &str, path: &Path, missing: bool| json!({"name": name, "slug": name, "kind": kind, "path": path.display().to_string(), "created_at": 1794535633, "missing": missing});
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    assert_eq!(
        list,
        json!([
            entry("globaldemo", "global", &global_db, true),
            entry("testlcl", "cwd", &cwd_db, true)
        ])
    );
    assert_eq!(registry_version(&env), "2");

    // Each is opened where it was found.
    for path in [&cwd_db, &global_db] {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::copy(V0_FIXTURE, path).expect("copy workspace");
    }
    assert_eq!(
        show(&env, "testlcl")["path"],
        json!(cwd_db.display().to_string())
    );
    assert_eq!(
        show(&env, "globaldemo")["path"],
        json!(global_db.display().to_string())
    );
}

/// After HOME moves (copied to a new place, HOME pointed at the copy),
/// every global workspace is found under the new HOME: none is missing, and
/// `-w` opens the copy, never the file at the old path.
#[test]
fn a_home_move_keeps_every_global_workspace() {
    let env = Env::new();
    workspace(&env, "alpha", "AL");
    env.lcl(&["-w", "alpha", "issue", "create", "--title", "first"])
        .assert_ok();
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    let moved = common::temp_dir();
    let copy = |from: &Path, to: &Path| {
        std::fs::create_dir_all(to.parent().unwrap()).expect("mkdir");
        std::fs::copy(from, to).expect("copy");
    };
    copy(
        &index(&env),
        &moved.path().join(".lcl").join("index.sqlite"),
    );
    let old_alpha = projects_dir(&env).join("alpha.sqlite");
    let new_alpha = moved
        .path()
        .join(".lcl")
        .join("projects")
        .join("alpha.sqlite");
    copy(&old_alpha, &new_alpha);
    let (old_mtime, old_dump) = (mtime(&old_alpha), dump(&old_alpha));

    let in_moved = |args: &[&str]| {
        env.lcl_with(args, |cmd| {
            cmd.env("HOME", moved.path());
        })
    };
    let list = in_moved(&["workspace", "list", "--json"]);
    list.assert_ok();
    let seen: Vec<_> = list
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["name"].clone(),
                w["kind"].clone(),
                w["path"].clone(),
                w["missing"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                json!("alpha"),
                json!("global"),
                json!(new_alpha.display().to_string()),
                json!(false)
            ),
            (
                json!("here"),
                json!("cwd"),
                json!(env.db_path().display().to_string()),
                json!(false)
            ),
        ]
    );
    assert_eq!(
        in_moved(&["-w", "alpha", "issue", "create", "--title", "moved"]).stdout,
        "Created AL-2\n"
    );
    assert_eq!(
        in_moved(&["-w", "alpha", "issue", "list"]).stdout,
        "AL-2\t[Backlog]\tNone\tmoved\nAL-1\t[Backlog]\tNone\tfirst\n"
    );
    assert_eq!(
        mtime(&old_alpha),
        old_mtime,
        "the file at the old path was opened"
    );
    assert!(dump(&old_alpha) == old_dump);
}

/// A registry written before V2 can hold several rows for one file (an
/// init under another name added one). The upgrade keeps one: the row
/// whose slug is the slug of the name stored in the file, else the last
/// row inserted. It reads that name without opening the file as a
/// workspace (which would upgrade it). Then a path is on one row only.
#[test]
fn the_registry_upgrade_keeps_one_row_per_file() {
    let env = Env::new();
    let place = |dir: &str, name: Option<&str>| {
        let db = env.cwd.path().join(dir).join(".lcl").join("data.sqlite");
        if let Some(name) = name {
            std::fs::create_dir_all(db.parent().unwrap()).expect("mkdir");
            std::fs::copy(V0_FIXTURE, &db).expect("copy");
            exec(
                &db,
                &format!("UPDATE meta SET value = '{name}' WHERE key = 'name'"),
            );
        }
        db
    };
    let named = place("p", Some("alpha")); // its name matches a row, not the last
    let gone = place("q", None); // no file: the last row
    let other = place("r", Some("fixture")); // its name matches no row: the last
    let global = projects_dir(&env).join("g.sqlite");
    let idx = index(&env);
    std::fs::create_dir_all(idx.parent().unwrap()).expect("mkdir");
    exec(
        &idx,
        &format!(
            "CREATE TABLE registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO registry_meta VALUES ('schema_version', '1');
             CREATE TABLE workspaces (slug TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO workspaces VALUES ('alpha', 'alpha', '{p}', 1), ('a', 'a', '{p}', 2),
                                           ('x', 'x', '{q}', 3), ('y', 'y', '{q}', 4),
                                           ('m', 'm', '{r}', 5), ('n', 'n', '{r}', 6),
                                           ('g', 'g', '{g}', 7);",
            p = named.display(),
            q = gone.display(),
            r = other.display(),
            g = global.display()
        ),
    );
    let (named_before, other_before) = (dump(&named), dump(&other));

    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    let seen: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["slug"].clone(), w["kind"].clone(), w["path"].clone()))
        .collect();
    let at = |p: &Path| json!(p.display().to_string());
    assert_eq!(
        seen,
        [
            (json!("alpha"), json!("cwd"), at(&named)),
            (json!("g"), json!("global"), at(&global)),
            (json!("n"), json!("cwd"), at(&other)),
            (json!("y"), json!("cwd"), at(&gone))
        ]
    );
    assert_eq!(registry_version(&env), "2");
    assert!(
        dump(&named) == named_before && dump(&other) == other_before,
        "the upgrade wrote to a workspace file"
    );
    assert_eq!(
        (user_version(&named), user_version(&other)),
        (0, 0),
        "the upgrade opened a workspace file as a workspace"
    );
    let conn = rusqlite::Connection::open(&idx).expect("open index");
    let dup = conn.execute(
        "INSERT INTO workspaces(slug, name, path, created_at) VALUES ('z', 'z', ?1, 8)",
        [named.display().to_string()],
    );
    assert!(dup.is_err(), "a second row for one path was accepted");
}

/// `--workspace` reads the index when there is one, so it upgrades an old
/// one, once: the next lookup finds V2 and writes nothing. With no index it
/// creates none, and a global file is found where it always was.
#[test]
fn a_workspace_flag_lookup_upgrades_an_old_index_once() {
    let env = Env::new();
    let idx = install(&env, V0_INDEX_FIXTURE, ".lcl/index.sqlite");
    install(&env, V0_INDEX_ALPHA_FIXTURE, ".lcl/projects/alpha.sqlite");
    repoint(&env, &idx);
    assert_eq!(
        env.lcl(&["-w", "alpha", "issue", "list", "--json"])
            .assert_ok()
            .json()["total"],
        0
    );
    assert_eq!(registry_version(&env), "2");
    let (stamp, before) = (mtime(&idx), dump(&idx));
    env.lcl(&["-w", "alpha", "issue", "list"]).assert_ok();
    assert_eq!(mtime(&idx), stamp, "the second lookup wrote to the index");
    assert!(dump(&idx) == before);

    let bare = Env::new();
    install(&bare, V0_INDEX_ALPHA_FIXTURE, ".lcl/projects/alpha.sqlite");
    assert_eq!(
        bare.lcl(&["-w", "alpha", "issue", "list", "--json"])
            .assert_ok()
            .json()["total"],
        0
    );
    assert!(!index(&bare).exists(), "a lookup created an index");
}

/// With an index, a name it has no row for is still looked for where a
/// global workspace's file is: a ~/.lcl/projects/<slug>.sqlite put there by
/// hand opens with `-w`, which leaves the index as it was.
#[test]
fn the_workspace_flag_finds_an_unregistered_global_file() {
    let env = Env::new();
    workspace(&env, "other", "OT");
    let idx = index(&env);
    let before = dump(&idx);
    install(&env, V0_INDEX_ALPHA_FIXTURE, ".lcl/projects/alpha.sqlite");
    let out = env.lcl(&["-w", "alpha", "issue", "list", "--json"]);
    out.assert_ok();
    assert_eq!(out.json()["total"], 0);
    assert!(dump(&idx) == before, "-w changed the index");
}

/// lcl 0.1.0 still reads an index this lcl upgraded: its `project list`
/// shows what it showed before. Needs 0.1.0's binary: LCL_010_BIN, or
/// ~/.local/bin/lcl, run from a copy; skipped (saying so) when neither is
/// 0.1.0.
#[test]
fn lcl_0_1_0_still_reads_an_upgraded_index() {
    let candidate = std::env::var_os("LCL_010_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("bin").join("lcl"))
        });
    let version = |bin: &Path| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let Some(original) = candidate.filter(|b| version(b).as_deref() == Some("lcl 0.1.0")) else {
        eprintln!(
            "SKIPPED lcl_0_1_0_still_reads_an_upgraded_index: no lcl 0.1.0 binary (set LCL_010_BIN)"
        );
        return;
    };
    let bin_dir = common::temp_dir();
    let bin = bin_dir.path().join("lcl");
    std::fs::copy(&original, &bin).expect("copy 0.1.0");
    let env = Env::new();
    let idx = install(&env, V0_INDEX_FIXTURE, ".lcl/index.sqlite");
    install(&env, V0_INDEX_ALPHA_FIXTURE, ".lcl/projects/alpha.sqlite");
    repoint(&env, &idx);
    let old_list = || {
        let out = std::process::Command::new(&bin)
            .args(["project", "list"])
            .current_dir(env.cwd.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", env.home.path())
            .output()
            .expect("run 0.1.0");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let before = old_list();
    assert_eq!(
        before,
        (
            Some(0),
            "\"R&D\" (\"r-d\")\n\"alpha\" (\"alpha\")\n".to_string(),
            String::new()
        )
    );
    env.lcl(&["workspace", "list"]).assert_ok();
    assert_eq!(registry_version(&env), "2");
    assert_eq!(
        old_list(),
        before,
        "0.1.0 reads the upgraded index differently"
    );
    env.lcl(&["workspace", "list"]).assert_ok();
}

#[test]
fn refuses_a_newer_registry_untouched() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    let sub = env.cwd.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    env.create("in the cwd workspace");
    common::exec(
        &index(&env),
        "UPDATE registry_meta SET value = '99' WHERE key = 'schema_version'",
    );
    let before = files(&env);

    let too_new = format!(
        "Error: the workspace index {} is v99, newer than this lcl supports (v2); upgrade lcl\n",
        index(&env).display()
    );
    for args in [
        vec!["workspace", "list"],
        vec!["workspace", "create", "beta", "--key", "BETA"],
        vec!["-w", "alpha", "issue", "list"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (1, "", too_new.as_str()),
            "{args:?}"
        );
    }
    let out = env.lcl_env(&[("LCL_WORKSPACE", "alpha")], &["issue", "list"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", too_new.as_str()),
        "LCL_WORKSPACE"
    );
    // The cwd workspace, found by walking up from a directory below it.
    let out = env.lcl_with(&["issue", "list"], |cmd| {
        cmd.current_dir(&sub);
    });
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (0, "HERE-1\t[Backlog]\tNone\tin the cwd workspace\n"),
        "{out:#?}"
    );
    // assert! rather than assert_eq!: a mismatch would print every byte.
    assert!(files(&env) == before, "a refused registry was written to");
}

/// The old flag and subcommand exit 2 with the new form, before anything is
/// opened.
#[test]
fn old_forms_exit_2_with_a_hint_and_touch_nothing() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    let before = files(&env);

    let flag = "the workspace flag is now --workspace: ";
    let sub = "workspaces are now: lcl workspace create NAME --key KEY / list / show";
    for (args, hint) in [
        (
            vec!["--project", "alpha", "issue", "list"],
            format!("{flag}lcl --workspace alpha issue list"),
        ),
        (
            vec![
                "--json",
                "--project=alpha",
                "issue",
                "create",
                "--title",
                "a b",
            ],
            format!("{flag}lcl --json --workspace=alpha issue create --title 'a b'"),
        ),
        (
            vec!["--project", "R&D", "issue", "list"],
            format!("{flag}lcl --workspace 'R&D' issue list"),
        ),
        (
            vec!["--project", "alpha", "tui"],
            format!("{flag}lcl --workspace alpha tui"),
        ),
        (
            vec!["project", "create", "foo", "--global"],
            sub.to_string(),
        ),
        (
            vec!["project", "create", "foo", "--key", "FOO"],
            sub.to_string(),
        ),
        (
            vec!["project", "create", "foo", "--global", "--key", "FOO"],
            sub.to_string(),
        ),
        (
            vec!["--json", "project", "create", "foo", "--key"],
            sub.to_string(),
        ),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        if args.contains(&"--json") {
            assert_eq!(out.stderr, error_text(&args, "usage", &hint), "{args:?}");
        } else {
            assert!(
                out.stderr.starts_with(&format!("error: {hint}\n")),
                "{args:?}: {out:#?}"
            );
        }
        assert_eq!(out.stdout, "", "{args:?}");
        assert!(files(&env) == before, "{args:?} changed a file");
    }
    // After the subcommand, --project is that command's own: a project in
    // the workspace. One named like a workspace gets the --workspace hint
    // (tests/projects.rs has the rest).
    let out = env.lcl(&["issue", "list", "--project", "alpha"]);
    let hint = "Error: project \"alpha\" not found in this workspace; \"alpha\" is a workspace: the workspace flag is now --workspace (-w)\n";
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (1, "", hint),
        "{out:#?}"
    );
    assert!(files(&env) == before);

    let help = env.lcl(&["--help"]).assert_ok().stdout.clone();
    assert!(
        help.contains("-w, --workspace <NAME>") && help.contains("[env: LCL_WORKSPACE=]"),
        "{help}"
    );
    assert!(!help.contains("--project"), "{help}");
    assert!(
        help.lines()
            .any(|l| l.trim_start().starts_with("project ")
                && l.contains("Projects in this workspace")),
        "{help}"
    );
}

#[test]
fn lcl_project_still_selects_a_workspace_with_a_notice() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    workspace(&env, "beta", "BETA");
    let notice = "lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE\n";
    let create = |vars: &[(&str, &str)], extra: &[&str]| {
        let mut args = extra.to_vec();
        args.extend(["issue", "create", "--title", "t"]);
        env.lcl_env(vars, &args)
    };

    let out = create(&[("LCL_PROJECT", "alpha")], &[]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "Created ALPHA-1\n", notice)
    );
    // Both set: LCL_WORKSPACE wins, without the notice.
    let out = create(&[("LCL_PROJECT", "alpha"), ("LCL_WORKSPACE", "beta")], &[]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "Created BETA-1\n", "")
    );
    let out = create(&[("LCL_WORKSPACE", "beta")], &[]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "Created BETA-2\n", "")
    );
    // The flag beats either variable; the notice still says LCL_PROJECT is set.
    let out = create(&[("LCL_PROJECT", "alpha")], &["-w", "beta"]);
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "Created BETA-3\n", notice)
    );
    let out = create(&[("LCL_WORKSPACE", "alpha")], &["--workspace", "beta"]);
    assert_eq!((out.code, out.stdout.as_str()), (0, "Created BETA-4\n"));

    let total = |name: &str| {
        env.lcl(&["-w", name, "issue", "list", "--json"])
            .assert_ok()
            .json()["total"]
            .clone()
    };
    assert_eq!((total("alpha"), total("beta")), (json!(1), json!(4)));
}

/// Bad input is refused by the argument parser (exit 2) before ~/.lcl exists.
#[test]
fn workspace_create_refuses_bad_input_before_touching_anything() {
    let env = Env::new();
    for args in [
        vec!["workspace", "create", "x"],
        vec!["workspace", "create", "x", "--key", "1AB"],
        vec!["workspace", "create", "x", "--key", "AB-C"],
        vec!["workspace", "create", "x", "--key", "ABCDEFGHIJK"],
        vec!["workspace", "create", "x", "--key", ""],
        vec!["workspace", "create", "x", "--key", "ÉCOLE"],
        vec!["workspace", "create", "&&", "--key", "X"],
        vec!["workspace", "create", "  ", "--key", "X"],
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(
            files(&env).is_empty() && !env.home.path().join(".lcl").exists(),
            "{args:?} touched ~/.lcl"
        );
    }
    let out = env.lcl(&["workspace", "create", "x", "--key", "1AB"]);
    assert!(
        out.stderr
            .contains("invalid key \"1AB\": a key is a letter, then up to 9 letters or digits"),
        "{out:#?}"
    );
}

#[test]
fn workspace_create() {
    let env = Env::new();
    let made = workspace(&env, " dreamd ", "drm");
    let path = projects_dir(&env).join("dreamd.sqlite");
    assert_eq!(made["name"], "dreamd");
    assert_eq!(
        (&made["slug"], &made["key"], &made["path"]),
        (
            &json!("dreamd"),
            &json!("DRM"),
            &json!(path.display().to_string())
        )
    );
    assert!(made["created_at"].is_i64(), "{made:#}");
    assert_eq!(show(&env, "dreamd")["key"], "DRM");
    assert_eq!(
        env.lcl(&["-w", "dreamd", "issue", "create", "--title", "t"])
            .assert_ok()
            .stdout,
        "Created DRM-1\n"
    );
    let out = env
        .lcl(&["workspace", "create", "R&D", "--key", "RD"])
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(
        out,
        format!(
            "Created workspace \"R&D\" (key RD) at {}\n",
            projects_dir(&env).join("r-d.sqlite").display()
        )
    );

    // A stray file that nothing registered.
    std::fs::write(projects_dir(&env).join("stray.sqlite"), b"not mine").expect("write stray");

    let before = files(&env);
    let rows = registry_rows(&env);
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["dreamd", "r-d"]
    );
    for (args, message) in [
        (vec!["dreamd", "--key", "OTHER"], "workspace \"dreamd\" already exists".to_string()),
        (vec!["R&D", "--key", "RD"], "workspace \"R&D\" already exists".to_string()),
        (vec!["R-D", "--key", "RD"], "workspace \"R-D\" would use the same file (r-d.sqlite) as the existing workspace \"R&D\"; choose another name".to_string()),
        (vec!["DreamD", "--key", "DRM"], "workspace \"DreamD\" would use the same file (dreamd.sqlite) as the existing workspace \"dreamd\"".to_string()),
        (vec!["stray", "--key", "S"], format!("a workspace file already exists at {}", projects_dir(&env).join("stray.sqlite").display())),
    ] {
        let mut cmd = vec!["workspace", "create"];
        cmd.extend(&args);
        let out = env.lcl(&cmd);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(out.stderr.contains(&message), "{args:?}: {out:#?}");
        assert!(files(&env) == before, "{args:?} changed a file");
        assert_eq!(registry_rows(&env), rows, "{args:?} changed the registry");
    }
    assert_eq!(show(&env, "dreamd")["key"], "DRM");
}

#[test]
fn workspace_show_json() {
    let env = Env::new();
    let start = lcl_core::models::now();
    workspace(&env, "dreamd", "drm");
    let shown = show(&env, "dreamd");
    let keys: Vec<&String> = shown.as_object().expect("object").keys().collect();
    assert_eq!(
        keys,
        [
            "created_at",
            "description",
            "key",
            "name",
            "path",
            "schema_version"
        ]
    );
    let created = shown["created_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("created_at is not an integer: {shown:#}"));
    assert!(
        (start..=lcl_core::models::now()).contains(&created),
        "{created}"
    );
    // The whole object. A new workspace's description is "": `workspace
    // create` (and `init`) store an empty one.
    let path = projects_dir(&env)
        .join("dreamd.sqlite")
        .display()
        .to_string();
    let exact = |description: &str| json!({"created_at": created, "description": description, "key": "DRM", "name": "dreamd", "path": path, "schema_version": SCHEMA_VERSION});
    assert_eq!(shown, exact(""));
    // A description that is set is shown as it is; here an import's header sets it.
    let header = "{\"type\":\"workspace\",\"format\":\"lcl-export/1\",\"key\":\"DRM\",\"description\":\"dreamd's tracker\"}\n";
    env.lcl_stdin(&["-w", "dreamd", "import", "-"], header.as_bytes())
        .assert_ok();
    assert_eq!(show(&env, "dreamd"), exact("dreamd's tracker"));

    // A workspace in the current directory, found from cwd.
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    let here = env.lcl(&["workspace", "show", "--json"]).assert_ok().json();
    assert_eq!(here["path"], json!(env.db_path().display().to_string()));
    assert!(here["created_at"].is_i64(), "{here:#}");
    assert_eq!(
        here["description"],
        json!(""),
        "init stores an empty description"
    );
    let human = env
        .lcl(&["-w", "dreamd", "workspace", "show"])
        .assert_ok()
        .stdout
        .clone();
    assert_eq!(
        human,
        format!(
            "dreamd [DRM] {}\n",
            projects_dir(&env).join("dreamd.sqlite").display()
        )
    );

    let none = Env::new();
    let out = none.lcl(&["workspace", "show"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(
        out.stderr
            .contains("no workspace found; run lcl init or pass --workspace"),
        "{out:#?}"
    );
    let out = none.lcl(&["-w", "nope", "issue", "list"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(
        out.stderr
            .contains("workspace \"nope\" not found: no file at"),
        "{out:#?}"
    );
    assert!(
        !none.home.path().join(".lcl").exists(),
        "resolving a workspace created ~/.lcl"
    );
}

/// F2: lcl needs HOME only for ~/.lcl: the registry, `-w` and
/// LCL_WORKSPACE, and init (which registers what it makes). Unset or empty
/// is an error there, never a fallback directory.
#[test]
fn no_home_is_an_error_only_where_home_is_needed() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    env.create("in the cwd workspace");
    let message = "Error: HOME is not set; lcl keeps its workspace registry in $HOME/.lcl\n";
    let fresh = common::temp_dir();
    for (label, unset) in [("HOME unset", true), ("HOME empty", false)] {
        let without = |args: &[&str], cwd: &Path| {
            env.lcl_with(args, |cmd| {
                cmd.current_dir(cwd);
                if unset {
                    cmd.env_remove("HOME");
                } else {
                    cmd.env("HOME", "");
                }
            })
        };
        for args in [
            vec!["workspace", "list"],
            vec!["workspace", "create", "x", "--key", "X"],
            vec!["-w", "alpha", "issue", "list"],
            vec!["init", "here", "--key", "HERE"],
        ] {
            let out = without(&args, fresh.path());
            assert_eq!(
                (out.code, out.stdout.as_str(), out.stderr.as_str()),
                (1, "", message),
                "{label}: {args:?}"
            );
            assert_eq!(
                std::fs::read_dir(fresh.path()).unwrap().count(),
                0,
                "{label}: {args:?} left something in the cwd"
            );
        }
        // LCL_WORKSPACE needs HOME as -w does.
        let out = env.lcl_with(&["issue", "list"], |cmd| {
            cmd.env("LCL_WORKSPACE", "alpha");
            if unset {
                cmd.env_remove("HOME");
            } else {
                cmd.env("HOME", "");
            }
        });
        assert_eq!(
            (out.code, out.stderr.as_str()),
            (1, message),
            "{label}: LCL_WORKSPACE"
        );
        // A workspace in the current directory doesn't need HOME.
        let out = without(&["issue", "list"], env.cwd.path());
        assert_eq!(
            (out.code, out.stdout.as_str()),
            (0, "HERE-1\t[Backlog]\tNone\tin the cwd workspace\n"),
            "{label}: {out:#?}"
        );
        let out = without(&["workspace", "show", "--json"], env.cwd.path());
        assert_eq!(
            (out.code, out.json()["key"].clone()),
            (0, json!("HERE")),
            "{label}: {out:#?}"
        );
    }
    assert_eq!(registry_rows(&env).len(), 2, "alpha and here, nothing more");
}

/// F3: an empty LCL_WORKSPACE or LCL_PROJECT counts as unset. An empty
/// name typed on the command line is still refused.
#[test]
fn empty_workspace_variables_count_as_unset() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here", "--key", "HERE"]).assert_ok();
    let notice = "lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE\n";
    let key = |vars: &[(&str, &str)], args: &[&str]| {
        let out = env.lcl_env(vars, args);
        let key = if out.code == 0 {
            out.json()["key"].as_str().unwrap_or_default().to_string()
        } else {
            String::new()
        };
        (out.code, key, out.stderr)
    };
    let show = ["workspace", "show", "--json"];
    assert_eq!(
        key(&[("LCL_WORKSPACE", ""), ("LCL_PROJECT", "alpha")], &show),
        (0, "ALPHA".into(), notice.into())
    );
    assert_eq!(
        key(&[("LCL_WORKSPACE", "")], &show),
        (0, "HERE".into(), String::new())
    );
    assert_eq!(
        key(&[("LCL_PROJECT", "")], &show),
        (0, "HERE".into(), String::new())
    );
    assert_eq!(
        key(&[("LCL_WORKSPACE", ""), ("LCL_PROJECT", "")], &show),
        (0, "HERE".into(), String::new())
    );
    let refused = "Error: workspace name \"\" has no letters or digits\n";
    for args in [
        vec!["-w", "", "workspace", "show"],
        vec!["--workspace=", "workspace", "show"],
    ] {
        for vars in [
            vec![],
            vec![("LCL_WORKSPACE", "")],
            vec![("LCL_WORKSPACE", "alpha")],
        ] {
            let out = env.lcl_env(&vars, &args);
            assert_eq!(
                (out.code, out.stdout.as_str(), out.stderr.as_str()),
                (2, "", refused),
                "{args:?} {vars:?}"
            );
        }
    }
    let help = env.lcl(&["--help"]).assert_ok().stdout.clone();
    assert!(help.contains("[env: LCL_WORKSPACE=]"), "{help}");
}

/// F1: ten `lcl init race` at once in a new directory, with the index
/// already made, so only the workspace file is new: its switch to WAL is
/// the race. Every process succeeds, every round.
#[test]
fn concurrent_inits_of_a_new_workspace_all_succeed() {
    const ROUNDS: usize = 30;
    const PROCS: usize = 10;
    for round in 0..ROUNDS {
        let env = Env::new();
        env.lcl(&["workspace", "list"]).assert_ok();
        let cmds: Vec<Vec<String>> = (0..PROCS)
            .map(|_| argv(&["init", "race", "--key", "RACE"]))
            .collect();
        for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
            assert_eq!(out.code, 0, "round {round}, process {i}: {out:#?}");
        }
        let conn = rusqlite::Connection::open(env.db_path()).expect("open db");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .expect("journal_mode");
        assert_eq!(mode, "wal", "round {round}");
        drop(conn);
        assert_eq!(
            registry_rows(&env),
            [(
                "race".to_string(),
                "race".to_string(),
                env.db_path().display().to_string()
            )],
            "round {round}"
        );
    }
}

/// Twenty creates at once, on a HOME with no index yet: all land.
#[test]
fn concurrent_creates_of_different_names_all_land() {
    const N: usize = 20;
    let env = Env::new();
    let cmds: Vec<Vec<String>> = (0..N)
        .map(|i| {
            argv(&[
                "workspace",
                "create",
                &format!("w{i}"),
                "--key",
                &format!("W{i}"),
            ])
        })
        .collect();
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "w{i}: {out:#?}");
    }
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    assert_eq!(list.as_array().map(Vec::len), Some(N), "{list:#}");
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|w| w["missing"] == false),
        "{list:#}"
    );
    for i in 0..N {
        assert_eq!(show(&env, &format!("w{i}"))["key"], format!("W{i}"));
    }
    let mut left: Vec<String> = std::fs::read_dir(projects_dir(&env))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut want: Vec<String> = (0..N).map(|i| format!("w{i}.sqlite")).collect();
    want.sort();
    assert_eq!(left, want, "temporary files left behind");
}

/// Ten creates of one name at once: one wins, the rest are told it exists,
/// and the file holds the winner's key.
#[test]
fn concurrent_creates_of_one_name_make_one() {
    const N: usize = 10;
    let env = Env::new();
    let cmds: Vec<Vec<String>> = (0..N)
        .map(|i| {
            argv(&[
                "workspace",
                "create",
                "same",
                "--key",
                &format!("S{i}"),
                "--json",
            ])
        })
        .collect();
    let outs = env.lcl_parallel(&cmds);
    let winners: Vec<usize> = (0..N).filter(|&i| outs[i].code == 0).collect();
    assert_eq!(winners.len(), 1, "{outs:#?}");
    for (i, out) in outs.iter().enumerate() {
        if i != winners[0] {
            assert_eq!(out.code, 1, "{i}: {out:#?}");
            let args: Vec<&str> = cmds[i].iter().map(String::as_str).collect();
            assert_eq!(
                out.stderr,
                error_text(&args, "conflict", "workspace \"same\" already exists"),
                "{i}"
            );
        }
    }
    let winner = outs[winners[0]].json();
    assert_eq!(registry_rows(&env).len(), 1);
    assert_eq!(show(&env, "same")["key"], winner["key"]);
    let left: Vec<String> = std::fs::read_dir(projects_dir(&env))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["same.sqlite"], "temporary files left behind");
}
