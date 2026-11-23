//! `lcl init NAME --key KEY`: a workspace in the current directory, at
//! .lcl/data.sqlite, registered as a cwd workspace so `--workspace NAME`
//! finds it from anywhere. The user types the key; nothing is derived. Run
//! again with the same name and key it does nothing; it never renames or
//! re-keys a workspace; and a name another workspace has (by slug, whatever
//! its kind) is refused. A refused init creates nothing.

mod common;

use common::{Env, V0_FIXTURE, dump, exec, user_version};
use lcl_core::SCHEMA_VERSION;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn index(env: &Env) -> PathBuf {
    env.home.path().join(".lcl").join("index.sqlite")
}

/// The registry's rows, (slug, name, kind, path), read directly.
fn rows(env: &Env) -> Vec<(String, String, String, String)> {
    let conn = rusqlite::Connection::open(index(env)).expect("open index");
    let mut stmt = conn
        .prepare("SELECT slug, name, kind, path FROM workspaces ORDER BY slug")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn row(slug: &str, name: &str, kind: &str, path: &Path) -> (String, String, String, String) {
    (
        slug.into(),
        name.into(),
        kind.into(),
        path.display().to_string(),
    )
}

fn trio(out: &common::Out) -> (i32, &str, &str) {
    (out.code, out.stdout.as_str(), out.stderr.as_str())
}

/// `lcl <args>` run from `dir` instead of the env's cwd.
fn lcl_in(env: &Env, dir: &Path, args: &[&str]) -> common::Out {
    env.lcl_with(args, |cmd| {
        cmd.current_dir(dir);
    })
}

fn subdir(env: &Env, name: &str) -> PathBuf {
    let dir = env.cwd.path().join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn db_in(dir: &Path) -> PathBuf {
    dir.join(".lcl").join("data.sqlite")
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The key is required and checked, and the name, given or taken from the
/// directory, must have a slug, before anything is made: not .lcl/, not
/// ~/.lcl.
#[test]
fn init_needs_a_key_and_a_name_before_it_makes_anything() {
    let env = Env::new();
    let out = env.lcl(&["init", "alpha"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("the following required arguments were not provided")
            && out.stderr.contains("--key <KEY>"),
        "{out:#?}"
    );
    let out = env.lcl(&["init", "alpha", "--key", "1AB"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("invalid key \"1AB\": a key is a letter, then up to 9 letters or digits"),
        "{out:#?}"
    );
    let out = env.lcl(&["init", "&&", "--key", "X"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(
        out.stderr
            .contains("workspace name \"&&\" has no letters or digits"),
        "{out:#?}"
    );
    // A directory whose name has no slug, and the root, which has no name.
    let amp = subdir(&env, "&&");
    assert_eq!(
        trio(&lcl_in(&env, &amp, &["init", "--key", "X"])),
        (
            2,
            "",
            "Error: workspace name \"&&\" has no letters or digits\n"
        )
    );
    let root_had = Path::new("/.lcl").exists();
    assert_eq!(
        trio(&lcl_in(&env, Path::new("/"), &["init", "--key", "X"])),
        (
            2,
            "",
            "Error: this directory has no name for the workspace; give it one: lcl init NAME --key KEY\n"
        )
    );
    assert_eq!(
        Path::new("/.lcl").exists(),
        root_had,
        "init at / touched /.lcl"
    );
    assert_eq!(entries(env.cwd.path()), ["&&"]);
    assert_eq!(entries(&amp), Vec::<String>::new());
    assert_eq!(
        entries(env.home.path()),
        Vec::<String>::new(),
        "a refused init made ~/.lcl"
    );
}

/// Left out, the name is the directory's, and the new workspace is
/// registered as a cwd workspace.
#[test]
fn init_takes_its_name_from_the_directory() {
    let env = Env::new();
    let dir = subdir(&env, "my proj");
    let made = lcl_in(&env, &dir, &["--json", "init", "--key", "mp"]);
    made.assert_ok();
    let db = db_in(&dir);
    assert_eq!(
        made.json(),
        json!({"path": db.display().to_string(), "name": "my proj", "key": "MP", "created": true})
    );
    assert_eq!(rows(&env), [row("my-proj", "my proj", "cwd", &db)]);
    let shown = lcl_in(&env, &dir, &["--json", "workspace", "show"]);
    shown.assert_ok();
    assert_eq!(
        (&shown.json()["name"], &shown.json()["key"]),
        (&json!("my proj"), &json!("MP"))
    );
}

/// A directory name is trimmed, as a typed name is: from " padded " the
/// workspace is "padded", slug padded. Windows drops a trailing space from
/// a file name, so there the directory is " padded".
#[test]
fn init_trims_the_directory_s_name() {
    let env = Env::new();
    let dir = subdir(&env, if cfg!(windows) { " padded" } else { " padded " });
    let made = lcl_in(&env, &dir, &["--json", "init", "--key", "PD"]);
    made.assert_ok();
    let db = db_in(&dir);
    assert_eq!(
        made.json(),
        json!({"path": db.display().to_string(), "name": "padded", "key": "PD", "created": true})
    );
    assert_eq!(rows(&env), [row("padded", "padded", "cwd", &db)]);
    assert_eq!(
        lcl_in(&env, &dir, &["--json", "workspace", "show"])
            .assert_ok()
            .json()["name"],
        "padded"
    );
}

/// Run again with the same name and key (in any case), init says the
/// workspace exists and writes nothing: the description and created_at it
/// had are kept.
#[test]
fn init_again_with_the_same_name_and_key_writes_nothing() {
    let env = Env::new();
    let first = env.lcl(&["--json", "init", "alpha", "--key", "AL"]);
    first.assert_ok();
    let db = env.db_path();
    assert_eq!(
        first.json(),
        json!({"path": db.display().to_string(), "name": "alpha", "key": "AL", "created": true})
    );
    exec(
        &db,
        "UPDATE meta SET value = 'my notes' WHERE key = 'description'; UPDATE meta SET value = '1700000000' WHERE key = 'created_at';",
    );
    let (before, registered) = (dump(&db), rows(&env));

    let again = env.lcl(&["init", "alpha", "--key", "al"]);
    assert_eq!(
        trio(&again),
        (
            0,
            format!(
                "Workspace \"alpha\" (key AL) already exists at {}\n",
                db.display()
            )
            .as_str(),
            ""
        )
    );
    let json_again = env.lcl(&["--json", "init", "alpha", "--key", "AL"]);
    json_again.assert_ok();
    assert_eq!(
        json_again.json(),
        json!({"path": db.display().to_string(), "name": "alpha", "key": "AL", "created": false})
    );
    assert!(
        dump(&db) == before,
        "a repeated init wrote to the workspace"
    );
    assert_eq!(rows(&env), registered);
    let shown = env.lcl(&["--json", "workspace", "show"]);
    shown.assert_ok();
    let shown: Value = shown.json();
    assert_eq!(
        (&shown["description"], &shown["created_at"]),
        (&json!("my notes"), &json!(1700000000))
    );
}

/// Another name or another key: exit 1, naming the workspace that is there,
/// which keeps its name, key, issues and registry row.
#[test]
fn init_never_renames_or_rekeys_a_workspace() {
    let env = Env::new();
    env.lcl(&["init", "dreamd", "--key", "DREA"]).assert_ok();
    env.create("x");
    let db = env.db_path();
    let (before, registered) = (dump(&db), rows(&env));
    let refused = format!(
        "Error: this directory already has workspace \"dreamd\" (key DREA) at {}; init doesn't rename or re-key a workspace\n",
        db.display()
    );
    for args in [
        ["init", "botzr-research", "--key", "BOTZ"],
        ["init", "dreamd", "--key", "OTHER"],
        ["init", "botzr-research", "--key", "DREA"],
    ] {
        assert_eq!(trio(&env.lcl(&args)), (1, "", refused.as_str()), "{args:?}");
    }
    assert!(dump(&db) == before, "a refused init wrote to the workspace");
    assert_eq!(rows(&env), registered);
    assert_eq!(
        env.lcl(&["issue", "list"]).assert_ok().stdout,
        "DREA-1\t[Backlog]\tNone\tx\n"
    );
}

/// A 0.1.0 workspace keeps the key 0.1.0 derived for it: init with that
/// key does nothing but upgrade the file and register it; any other key is
/// refused.
#[test]
fn a_0_1_0_workspace_keeps_the_key_it_was_given() {
    let env = Env::new();
    let db = env.install_db(V0_FIXTURE);
    let out = env.lcl(&["init", "fixture", "--key", "FIXT"]);
    assert_eq!(
        trio(&out),
        (
            0,
            format!(
                "Workspace \"fixture\" (key FIXT) already exists at {}\n",
                db.display()
            )
            .as_str(),
            ""
        )
    );
    assert_eq!(user_version(&db), SCHEMA_VERSION);
    assert_eq!(rows(&env), [row("fixture", "fixture", "cwd", &db)]);
    let before = dump(&db);
    let out = env.lcl(&["init", "fixture", "--key", "FIX"]);
    assert_eq!(
        trio(&out),
        (1, "", format!("Error: this directory already has workspace \"fixture\" (key FIXT) at {}; init doesn't rename or re-key a workspace\n", db.display()).as_str())
    );
    assert!(dump(&db) == before);
}

/// Names are unique across the registry, by slug, whatever the kind: init
/// refuses a name registered to another place, naming it, and makes
/// nothing, not even .lcl/.
#[test]
fn init_refuses_a_name_another_workspace_has() {
    let env = Env::new();
    env.lcl(&["workspace", "create", "gamma", "--key", "GAM"])
        .assert_ok();
    env.lcl(&["workspace", "create", "R&D", "--key", "RD"])
        .assert_ok();
    let one = subdir(&env, "one");
    lcl_in(&env, &one, &["init", "shared", "--key", "SH"]).assert_ok();
    let registered = rows(&env);
    let global = |slug: &str| {
        env.home
            .path()
            .join(".lcl")
            .join("projects")
            .join(format!("{slug}.sqlite"))
            .display()
            .to_string()
    };
    let two = subdir(&env, "two");
    for (args, want) in [
        (
            ["init", "gamma", "--key", "G"],
            format!(
                "Error: workspace \"gamma\" is already registered at {}\n",
                global("gamma")
            ),
        ),
        (
            ["init", "R-D", "--key", "RD"],
            format!(
                "Error: the name \"R-D\" is taken: workspace \"R&D\" is registered at {}\n",
                global("r-d")
            ),
        ),
        (
            ["init", "shared", "--key", "SH"],
            format!(
                "Error: workspace \"shared\" is already registered at {}\n",
                db_in(&one).display()
            ),
        ),
        (
            ["init", "Shared", "--key", "SH"],
            format!(
                "Error: the name \"Shared\" is taken: workspace \"shared\" is registered at {}\n",
                db_in(&one).display()
            ),
        ),
    ] {
        assert_eq!(
            trio(&lcl_in(&env, &two, &args)),
            (1, "", want.as_str()),
            "{args:?}"
        );
        assert_eq!(
            entries(&two),
            Vec::<String>::new(),
            "{args:?} made something"
        );
    }
    assert_eq!(rows(&env), registered);
}

/// A row whose file is gone (its .lcl/ deleted) is replaced by the next
/// init there, whatever its name: one row per path.
#[test]
fn init_replaces_the_row_of_a_workspace_whose_file_is_gone() {
    let env = Env::new();
    env.lcl(&["init", "a", "--key", "A"]).assert_ok();
    let db = env.db_path();
    assert_eq!(rows(&env), [row("a", "a", "cwd", &db)]);
    std::fs::remove_dir_all(env.cwd.path().join(".lcl")).expect("rm .lcl");
    let made = env.lcl(&["--json", "init", "b", "--key", "B"]);
    made.assert_ok();
    assert_eq!(made.json()["created"], true);
    assert_eq!(rows(&env), [row("b", "b", "cwd", &db)]);
    std::fs::remove_dir_all(env.cwd.path().join(".lcl")).expect("rm .lcl");
    env.lcl(&["init", "b", "--key", "B"]).assert_ok();
    assert_eq!(rows(&env), [row("b", "b", "cwd", &db)]);
}

/// A workspace file no row has (its row lost) is registered by an init with
/// its name and key, which writes only the index; unless its slug is
/// registered to another place, which is refused.
#[test]
fn init_registers_a_file_nobody_registered() {
    let env = Env::new();
    env.lcl(&["init", "solo", "--key", "SO"]).assert_ok();
    let db = env.db_path();
    exec(&index(&env), "DELETE FROM workspaces");
    let before = dump(&db);
    let out = env.lcl(&["init", "solo", "--key", "SO"]);
    assert_eq!(
        trio(&out),
        (
            0,
            format!(
                "Workspace \"solo\" (key SO) already exists at {}\n",
                db.display()
            )
            .as_str(),
            ""
        )
    );
    assert_eq!(rows(&env), [row("solo", "solo", "cwd", &db)]);
    assert!(dump(&db) == before, "registering it wrote to the workspace");

    exec(&index(&env), "DELETE FROM workspaces");
    env.lcl(&["workspace", "create", "solo", "--key", "S2"])
        .assert_ok();
    let registered = rows(&env);
    let global = env
        .home
        .path()
        .join(".lcl")
        .join("projects")
        .join("solo.sqlite");
    let out = env.lcl(&["init", "solo", "--key", "SO"]);
    assert_eq!(
        trio(&out),
        (
            1,
            "",
            format!(
                "Error: workspace \"solo\" is already registered at {}\n",
                global.display()
            )
            .as_str()
        )
    );
    assert_eq!(rows(&env), registered);
    assert!(dump(&db) == before);
}

/// `--workspace NAME` (and LCL_WORKSPACE) finds a workspace init made, from
/// any directory, at its recorded path; one whose file is gone is not
/// found, and `workspace list` marks it missing.
#[test]
fn the_workspace_flag_finds_a_workspace_init_made() {
    let env = Env::new();
    let one = subdir(&env, "one");
    lcl_in(&env, &one, &["init", "cwdws", "--key", "CW"]).assert_ok();
    lcl_in(&env, &one, &["issue", "create", "--title", "x"]).assert_ok();
    let elsewhere = subdir(&env, "elsewhere");
    assert_eq!(
        trio(&lcl_in(&env, &elsewhere, &["-w", "cwdws", "issue", "list"])),
        (0, "CW-1\t[Backlog]\tNone\tx\n", "")
    );
    let out = env.lcl_with(&["issue", "list"], |cmd| {
        cmd.current_dir(&elsewhere).env("LCL_WORKSPACE", "cwdws");
    });
    assert_eq!(trio(&out), (0, "CW-1\t[Backlog]\tNone\tx\n", ""));

    std::fs::remove_dir_all(one.join(".lcl")).expect("rm .lcl");
    let gone = format!(
        "Error: workspace \"cwdws\" not found: no file at {}; see lcl workspace list\n",
        db_in(&one).display()
    );
    assert_eq!(
        trio(&lcl_in(&env, &elsewhere, &["-w", "cwdws", "issue", "list"])),
        (1, "", gone.as_str())
    );
    let list = env.lcl(&["--json", "workspace", "list"]);
    list.assert_ok();
    let list = list.json();
    assert_eq!(
        (&list[0]["kind"], &list[0]["path"], &list[0]["missing"]),
        (
            &json!("cwd"),
            &json!(db_in(&one).display().to_string()),
            &json!(true)
        ),
        "{list:#}"
    );
}
