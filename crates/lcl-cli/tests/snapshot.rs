//! The snapshot lcl takes before it upgrades a file: the rules in
//! `lcl_core::backup`, held against real processes and real old files.
//!
//! - Snapshot first, and exactly the state upgraded: the snapshot equals
//!   the old file table for table, at the old version.
//! - One upgrade, one snapshot, however many processes open the file.
//! - Never overwritten: an existing snapshot keeps its bytes.
//! - No snapshot, no upgrade: when the copy can't be written, the file is
//!   left as it was and the command exits 1.
//! - Kept only if the upgrade commits.
//! - Nothing for nothing: a current file and a new file get none.

mod common;

use common::{
    Dump, Env, V0_FIXTURE, V0_INDEX_FIXTURE, V0_LABELS_FIXTURE, V0_PARENTS_FIXTURE, V7_FIXTURE,
    V7_INDEX_FIXTURE, V8_FIXTURE, V010_DIRTY_FIXTURE, argv, dump, files, snapshot_path,
    upgrade_line, user_version,
};
use lcl_core::SCHEMA_VERSION;
use std::path::{Path, PathBuf};

/// What a fixture holds, read from a copy so the fixture itself is never
/// opened.
fn fixture_dump(fixture: &str) -> Dump {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let copy = dir.path().join("copy.sqlite");
    std::fs::copy(fixture, &copy).expect("copy fixture");
    dump(&copy)
}

/// Every file under the env's HOME and cwd whose name says snapshot.
fn snapshots(env: &Env) -> Vec<PathBuf> {
    files(env)
        .into_keys()
        .filter(|p| p.to_string_lossy().contains(".snapshot"))
        .collect()
}

/// The registry version a file is at: its `registry_meta` row, or 0.
fn registry_version(index: &Path) -> i64 {
    let conn = rusqlite::Connection::open(index).expect("open index");
    conn.query_row(
        "SELECT value FROM registry_meta WHERE key = 'schema_version'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v.parse().expect("a number"))
    .unwrap_or(0)
}

#[test]
fn an_old_workspace_is_copied_beside_itself_before_it_is_upgraded() {
    for fixture in [
        V0_FIXTURE,
        V0_LABELS_FIXTURE,
        V0_PARENTS_FIXTURE,
        V010_DIRTY_FIXTURE,
        V7_FIXTURE,
        V8_FIXTURE,
    ] {
        let env = Env::new();
        let db = env.install_db(fixture);
        let from = user_version(&db);
        let old = fixture_dump(fixture);

        let out = env.lcl(&["workspace", "show", "--json"]);
        let name = out.assert_ok().json()["name"].as_str().unwrap().to_string();
        assert_eq!(user_version(&db), SCHEMA_VERSION, "{fixture}");

        // One snapshot, named for the version the file came from, and it is
        // the old file: every table, every row, every rowid, the old version.
        let snapshot = snapshot_path(&db, from);
        assert_eq!(
            snapshots(&env),
            std::slice::from_ref(&snapshot),
            "{fixture}"
        );
        assert_eq!(user_version(&snapshot), from, "{fixture}");
        assert!(
            dump(&snapshot) == old,
            "{fixture}: the snapshot is not the old file"
        );
        // The upgrade line names it.
        let line = out.stderr.lines().next().unwrap_or_default();
        assert!(
            line.starts_with(upgrade_line(&db, from, &name, "").trim_end()),
            "{fixture}: {line}"
        );

        // The next open upgrades nothing: no line, no second snapshot.
        let again = env.lcl(&["issue", "list", "--json"]);
        assert_eq!(again.assert_ok().stderr, "", "{fixture}");
        assert_eq!(snapshots(&env), [snapshot], "{fixture}");
    }
}

#[test]
fn an_old_registry_is_copied_beside_itself_before_it_is_upgraded() {
    for fixture in [V0_INDEX_FIXTURE, V7_INDEX_FIXTURE] {
        let env = Env::new();
        let index = env.home.path().join(".lcl").join("index.sqlite");
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        std::fs::copy(fixture, &index).unwrap();
        let old = fixture_dump(fixture);
        assert_eq!(registry_version(&index), 0, "{fixture}");

        let out = env.lcl(&["workspace", "list", "--json"]);
        out.assert_ok();
        let now = registry_version(&index);
        assert!(now > 0, "{fixture}");

        let snapshot = snapshot_path(&index, 0);
        assert_eq!(
            snapshots(&env),
            std::slice::from_ref(&snapshot),
            "{fixture}"
        );
        assert_eq!(registry_version(&snapshot), 0, "{fixture}");
        assert!(
            dump(&snapshot) == old,
            "{fixture}: the snapshot is not the old index"
        );
        assert_eq!(
            out.stderr,
            format!(
                "lcl: upgraded the workspace registry to v{now}; the file as it was is {}\n",
                snapshot.display()
            ),
            "{fixture}"
        );

        let again = env.lcl(&["workspace", "list", "--json"]);
        assert_eq!(again.assert_ok().stderr, "", "{fixture}");
        assert_eq!(snapshots(&env), [snapshot], "{fixture}");
    }
}

/// `-w NAME` reads the registry on its way to the workspace; an upgrade it
/// makes there is announced too.
#[test]
fn the_registry_s_upgrade_is_announced_on_the_way_to_a_workspace() {
    let env = Env::new();
    let index = env.home.path().join(".lcl").join("index.sqlite");
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    std::fs::copy(V0_INDEX_FIXTURE, &index).unwrap();
    let out = env.lcl(&["-w", "no-such-workspace", "issue", "list"]);
    assert_eq!(out.code, 1, "{out:#?}");
    let snapshot = snapshot_path(&index, 0);
    assert!(
        out.stderr.starts_with(&format!(
            "lcl: upgraded the workspace registry to v{}; the file as it was is {}\n",
            registry_version(&index),
            snapshot.display()
        )),
        "{out:#?}"
    );
    assert_eq!(snapshots(&env), [snapshot]);
}

#[test]
fn an_existing_snapshot_is_never_overwritten() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    let old = fixture_dump(V0_FIXTURE);
    let first = snapshot_path(&db, 0);
    std::fs::write(&first, b"an earlier snapshot").unwrap();
    let mut second = first.clone().into_os_string();
    second.push(".2");
    let second = PathBuf::from(second);
    std::fs::write(&second, b"and another").unwrap();
    let mut third = first.clone().into_os_string();
    third.push(".3");
    let third = PathBuf::from(third);

    let out = env.lcl(&["workspace", "show"]);
    out.assert_ok();
    assert_eq!(std::fs::read(&first).unwrap(), b"an earlier snapshot");
    assert_eq!(std::fs::read(&second).unwrap(), b"and another");
    // This upgrade's snapshot took the next free name, and the line says so.
    assert_eq!(snapshots(&env), [first, second, third.clone()]);
    assert!(dump(&third) == old);
    assert!(
        out.stderr
            .contains(&format!("the file as it was is {}\n", third.display())),
        "{out:#?}"
    );
}

/// When the copy can't be written, no migration step runs: the file is the
/// old file still, the command exits 1 and names both files.
#[cfg(unix)]
#[test]
fn without_a_snapshot_there_is_no_upgrade() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    let dir = db.parent().unwrap().to_path_buf();
    // A directory nothing can be created in, with the workspace itself
    // still writable: its WAL files are there already, held open.
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder
        .pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
        .unwrap();
    holder.execute_batch("BEGIN IMMEDIATE; COMMIT;").unwrap();
    let before = dump(&db);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join("probe"), b"").is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("SKIPPED: this user can write to a read-only directory");
        return;
    }

    let out = env.lcl(&["issue", "list", "--json"]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.code, 1, "{out:#?}");
    assert_eq!(out.stdout, "");
    let error: serde_json::Value =
        serde_json::from_str(out.stderr.lines().last().unwrap()).unwrap();
    assert_eq!(error["error"]["kind"], "io", "{out:#?}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("can't upgrade {}", db.display()))
            && message.contains(&snapshot_path(&db, 0).display().to_string())
            && message.contains("nothing was changed"),
        "{message}"
    );
    assert_eq!(user_version(&db), 0);
    assert!(dump(&db) == before, "a refused upgrade changed the file");
    assert_eq!(snapshots(&env), Vec::<PathBuf>::new());
    drop(holder);

    // Once the directory can be written, the same command upgrades.
    env.lcl(&["issue", "list", "--json"]).assert_ok();
    assert_eq!(user_version(&db), SCHEMA_VERSION);
    assert_eq!(snapshots(&env), [snapshot_path(&db, 0)]);
}

/// An upgrade that fails rolls back, and takes its snapshot with it: the
/// file is unchanged, so there is nothing the copy would be needed for.
#[test]
fn a_failed_upgrade_keeps_no_snapshot() {
    let env = Env::new();
    let db = env.db_path();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    // Not a workspace: at schema 9 by its own account, with none of the
    // tables step 10 reads.
    common::exec(
        &db,
        "CREATE TABLE junk (x); INSERT INTO junk VALUES (1); PRAGMA user_version = 9;",
    );
    let before = dump(&db);

    for _ in 0..2 {
        let out = env.lcl(&["issue", "list"]);
        assert_eq!(out.code, 1, "{out:#?}");
        assert_eq!(user_version(&db), 9);
        assert!(dump(&db) == before);
        assert_eq!(snapshots(&env), Vec::<PathBuf>::new(), "{out:#?}");
    }
}

/// Many processes open the same old workspace at once: all succeed, one
/// of them upgrades it, and there is one snapshot, of the old file.
#[test]
fn processes_that_open_an_old_workspace_at_once_leave_one_snapshot() {
    let env = Env::new();
    let db = env.install_db(V010_DIRTY_FIXTURE);
    let old = fixture_dump(V010_DIRTY_FIXTURE);
    let cmds: Vec<Vec<String>> = (0..8).map(|_| argv(&["issue", "list", "--json"])).collect();
    let outs = env.lcl_parallel(&cmds);
    let mut lists = Vec::new();
    for out in &outs {
        lists.push(out.assert_ok().json());
    }
    assert!(
        lists.iter().all(|l| *l == lists[0]),
        "the processes saw different workspaces"
    );
    assert_eq!(user_version(&db), SCHEMA_VERSION);

    let snapshot = snapshot_path(&db, 0);
    assert_eq!(snapshots(&env), std::slice::from_ref(&snapshot));
    assert_eq!(user_version(&snapshot), 0);
    assert!(dump(&snapshot) == old, "the snapshot is not the old file");
    let announced = outs
        .iter()
        .filter(|o| o.stderr.contains("upgraded workspace"))
        .count();
    assert_eq!(announced, 1, "{outs:#?}");
}

/// A current workspace takes no snapshot, whatever is run on it, and a new
/// workspace has nothing to copy.
///
/// (That the open of a current workspace takes no write lock is not tested
/// here: the upgrade path takes none, but the search-index check on every
/// open does. That is LOCL-42.)
#[test]
fn a_current_or_new_workspace_takes_no_snapshot() {
    let env = Env::new();
    env.lcl(&["init", "w", "--key", "W"]).assert_ok();
    env.lcl(&["workspace", "create", "global", "--key", "GLB"])
        .assert_ok();
    env.create("one");
    env.lcl(&["-w", "global", "issue", "create", "--title", "two"])
        .assert_ok();
    env.lcl(&["issue", "list"]).assert_ok();
    env.lcl(&["workspace", "list"]).assert_ok();
    env.lcl(&["backup", "copy.sqlite"]).assert_ok();
    assert_eq!(snapshots(&env), Vec::<PathBuf>::new());
    let before = files(&env);
    assert_eq!(env.lcl(&["issue", "list"]).assert_ok().stderr, "");
    assert_eq!(
        files(&env).keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>(),
        "a read of a current workspace made a file"
    );
}

/// The registry's snapshot keeps the workspace's rules: an existing one is
/// never overwritten, and processes that open an old registry at once
/// leave one snapshot, of the old file.
#[test]
fn the_registry_s_snapshot_is_never_overwritten_and_taken_once() {
    let old = fixture_dump(V0_INDEX_FIXTURE);
    let install = || {
        let env = Env::new();
        let index = env.home.path().join(".lcl").join("index.sqlite");
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        std::fs::copy(V0_INDEX_FIXTURE, &index).unwrap();
        (env, index)
    };

    let (env, index) = install();
    let first = snapshot_path(&index, 0);
    std::fs::write(&first, b"an earlier snapshot").unwrap();
    let mut second = first.clone().into_os_string();
    second.push(".2");
    let second = PathBuf::from(second);
    let out = env.lcl(&["workspace", "list"]);
    out.assert_ok();
    assert_eq!(std::fs::read(&first).unwrap(), b"an earlier snapshot");
    assert_eq!(snapshots(&env), [first, second.clone()]);
    assert!(dump(&second) == old);
    assert!(
        out.stderr
            .contains(&format!("the file as it was is {}\n", second.display())),
        "{out:#?}"
    );

    let (env, index) = install();
    let cmds: Vec<Vec<String>> = (0..8)
        .map(|_| argv(&["workspace", "list", "--json"]))
        .collect();
    let outs = env.lcl_parallel(&cmds);
    let mut lists = Vec::new();
    for out in &outs {
        lists.push(out.assert_ok().json());
    }
    assert!(lists.iter().all(|l| *l == lists[0]));
    let snapshot = snapshot_path(&index, 0);
    assert_eq!(snapshots(&env), std::slice::from_ref(&snapshot));
    assert!(dump(&snapshot) == old, "the snapshot is not the old index");
    let announced = outs
        .iter()
        .filter(|o| o.stderr.contains("upgraded the workspace registry"))
        .count();
    assert_eq!(announced, 1, "{outs:#?}");
}
