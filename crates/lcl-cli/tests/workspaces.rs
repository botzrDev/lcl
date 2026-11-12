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
//! tests/fixtures/v7-index.sqlite was made the same way by a build of 097af5c
//! (`git archive`, built in its own target dir), with
//! `lcl project create beta --global --key BETA`. That build ran the
//! workspace migrations V1–V7 on the index: user_version 7.
//!
//! No fixture left a -wal or -shm file. Tests copy them; never open one in
//! place.

mod common;

use common::{argv, dump, user_version, Dump, Env, V0_INDEX_ALPHA_FIXTURE, V0_INDEX_FIXTURE, V7_INDEX_FIXTURE};
use lcl_core::SCHEMA_VERSION;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    conn.execute("UPDATE projects SET path = ?1 || substr(path, instr(path, '/.lcl/'))", [&home]).expect("repoint paths");
    home
}

/// Copies `fixture` to `rel` under this env's HOME.
fn install(env: &Env, fixture: &str, rel: &str) -> PathBuf {
    let dst = env.home.path().join(rel);
    std::fs::create_dir_all(dst.parent().unwrap()).expect("create dirs");
    std::fs::copy(fixture, &dst).unwrap_or_else(|e| panic!("copy {fixture}: {e}"));
    dst
}

/// Every file under this env's HOME and cwd, with its bytes.
fn files(env: &Env) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
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

/// The registry's rows, (slug, name, path), read directly.
fn registry_rows(env: &Env) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(index(env)).expect("open index");
    let mut stmt = conn.prepare("SELECT slug, name, path FROM workspaces ORDER BY slug").expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).expect("query").collect::<Result<_, _>>().expect("rows")
}

/// Takes the registry's own tables out of a dump of the index, after
/// checking its version row, so what's left is what older builds wrote.
fn undo_registry(after: &mut Dump) {
    let text = |s: &str| SqlValue::Text(s.into());
    let meta = &after.tables["registry_meta"];
    assert_eq!(meta.len(), 1, "{meta:?}");
    assert_eq!((&meta[0]["key"], &meta[0]["value"]), (&text("schema_version"), &text("1")));
    let ours = ["registry_meta", "workspaces", "sqlite_autoindex_registry_meta_1", "sqlite_autoindex_workspaces_1"];
    let before = after.schema.len();
    after.schema.retain(|r| !ours.iter().any(|n| r["name"] == text(n)));
    assert_eq!(before - after.schema.len(), ours.len(), "the registry adds exactly these schema entries");
    after.tables.remove("registry_meta");
    after.tables.remove("workspaces");
}

fn workspace(env: &Env, name: &str, key: &str) -> Value {
    env.lcl(&["workspace", "create", name, "--key", key, "--json"]).assert_ok().json()
}

fn show(env: &Env, name: &str) -> Value {
    env.lcl(&["-w", name, "workspace", "show", "--json"]).assert_ok().json()
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
            {"name": "R&D", "slug": "r-d", "path": format!("{home}/.lcl/projects/r-d.sqlite"), "created_at": V0_CREATED, "missing": true},
            {"name": "alpha", "slug": "alpha", "path": format!("{home}/.lcl/projects/alpha.sqlite"), "created_at": V0_CREATED, "missing": false},
        ])
    );
    let human = env.lcl(&["workspace", "list"]).assert_ok().stdout.clone();
    assert_eq!(
        human,
        format!("R&D\tr-d\t{home}/.lcl/projects/r-d.sqlite\t(missing)\nalpha\talpha\t{home}/.lcl/projects/alpha.sqlite\n")
    );

    let issues = env.lcl(&["-w", "alpha", "issue", "list", "--json"]).assert_ok().json();
    assert_eq!(issues["total"], 0, "{issues:#}");
    assert_eq!(env.lcl(&["-w", "alpha", "issue", "create", "--title", "first"]).assert_ok().stdout, "Created ALPH-1\n");
    let alpha = show(&env, "alpha");
    assert_eq!((&alpha["key"], &alpha["created_at"]), (&json!("ALPH"), &json!(V0_CREATED)));

    workspace(&env, "beta", "BETA");
    let names: Vec<_> = env.lcl(&["workspace", "list", "--json"]).assert_ok().json().as_array().unwrap().iter().map(|w| w["name"].clone()).collect();
    assert_eq!(names, [json!("R&D"), json!("alpha"), json!("beta")]);

    // The registry added its own two tables and changed nothing else.
    let mut after = dump(&idx);
    undo_registry(&mut after);
    assert_eq!(after, before);
    assert_eq!(user_version(&idx), 0, "the registry doesn't use user_version");
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
    assert_eq!((&list[0]["name"], &list[0]["slug"], &list[0]["missing"]), (&json!("beta"), &json!("beta"), &json!(true)));
    assert_eq!(list[0]["path"], json!(format!("{home}/.lcl/projects/beta.sqlite")));

    workspace(&env, "gamma", "GAM");
    assert_eq!(env.lcl(&["-w", "gamma", "issue", "create", "--title", "g"]).assert_ok().stdout, "Created GAM-1\n");
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    let names: Vec<_> = list.as_array().unwrap().iter().map(|w| (w["name"].clone(), w["missing"].clone())).collect();
    assert_eq!(names, [(json!("beta"), json!(true)), (json!("gamma"), json!(false))]);

    let mut after = dump(&idx);
    undo_registry(&mut after);
    assert_eq!(after, before);
    assert_eq!(user_version(&idx), 7);
}

#[test]
fn refuses_a_newer_registry_untouched() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    common::exec(&index(&env), "UPDATE registry_meta SET value = '99' WHERE key = 'schema_version'");
    let before = files(&env);

    for args in [vec!["workspace", "list"], vec!["workspace", "create", "beta", "--key", "BETA"]] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 1, "{args:?}: {out:#?}");
        assert!(out.stderr.contains("is v99, newer than this lcl supports (v1)"), "{args:?}: {out:#?}");
    }
    // assert! rather than assert_eq!: a mismatch would print every byte.
    assert!(files(&env) == before, "a refused registry was written to");
    // Workspaces themselves never open the registry.
    env.lcl(&["-w", "alpha", "issue", "list"]).assert_ok();
}

/// The old flag and subcommand exit 2 with the new form, before anything is
/// opened.
#[test]
fn old_forms_exit_2_with_a_hint_and_touch_nothing() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here"]).assert_ok();
    let before = files(&env);

    let flag = "the workspace flag is now --workspace: ";
    let sub = "workspaces are now: lcl workspace create NAME --key KEY / list / show";
    for (args, hint) in [
        (vec!["--project", "alpha", "issue", "list"], format!("{flag}lcl --workspace alpha issue list")),
        (vec!["--json", "--project=alpha", "issue", "create", "--title", "a b"], format!("{flag}lcl --json --workspace=alpha issue create --title 'a b'")),
        (vec!["--project", "R&D", "issue", "list"], format!("{flag}lcl --workspace 'R&D' issue list")),
        (vec!["--project", "alpha", "tui"], format!("{flag}lcl --workspace alpha tui")),
        (vec!["project", "create", "foo", "--global"], sub.to_string()),
        (vec!["project", "create", "foo", "--key", "FOO"], sub.to_string()),
        (vec!["project", "create", "foo", "--global", "--key", "FOO"], sub.to_string()),
        (vec!["project", "list"], sub.to_string()),
        (vec!["--json", "project", "show"], sub.to_string()),
        (vec!["project"], sub.to_string()),
        (vec!["project", "--help"], sub.to_string()),
    ] {
        let out = env.lcl(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:#?}");
        assert!(out.stderr.starts_with(&format!("error: {hint}\n")), "{args:?}: {out:#?}");
        assert_eq!(out.stdout, "", "{args:?}");
        assert!(files(&env) == before, "{args:?} changed a file");
    }
    // After the subcommand, --project isn't the old global flag; clap
    // refuses it as unknown (step 10b may give a subcommand its own).
    let out = env.lcl(&["issue", "list", "--project", "alpha"]);
    assert_eq!(out.code, 2, "{out:#?}");
    assert!(files(&env) == before);

    let help = env.lcl(&["--help"]).assert_ok().stdout.clone();
    assert!(help.contains("-w, --workspace <NAME>") && help.contains("[env: LCL_WORKSPACE=]"), "{help}");
    assert!(!help.contains("--project") && !help.contains("  project "), "{help}");
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
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (0, "Created ALPHA-1\n", notice));
    // Both set: LCL_WORKSPACE wins, without the notice.
    let out = create(&[("LCL_PROJECT", "alpha"), ("LCL_WORKSPACE", "beta")], &[]);
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (0, "Created BETA-1\n", ""));
    let out = create(&[("LCL_WORKSPACE", "beta")], &[]);
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (0, "Created BETA-2\n", ""));
    // The flag beats either variable; the notice still says LCL_PROJECT is set.
    let out = create(&[("LCL_PROJECT", "alpha")], &["-w", "beta"]);
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (0, "Created BETA-3\n", notice));
    let out = create(&[("LCL_WORKSPACE", "alpha")], &["--workspace", "beta"]);
    assert_eq!((out.code, out.stdout.as_str()), (0, "Created BETA-4\n"));

    let total = |name: &str| env.lcl(&["-w", name, "issue", "list", "--json"]).assert_ok().json()["total"].clone();
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
        assert!(files(&env).is_empty() && !env.home.path().join(".lcl").exists(), "{args:?} touched ~/.lcl");
    }
    let out = env.lcl(&["workspace", "create", "x", "--key", "1AB"]);
    assert!(out.stderr.contains("invalid key \"1AB\": a key is a letter, then up to 9 letters or digits"), "{out:#?}");
}

#[test]
fn workspace_create() {
    let env = Env::new();
    let made = workspace(&env, " dreamd ", "drm");
    let path = projects_dir(&env).join("dreamd.sqlite");
    assert_eq!(made["name"], "dreamd");
    assert_eq!((&made["slug"], &made["key"], &made["path"]), (&json!("dreamd"), &json!("DRM"), &json!(path.display().to_string())));
    assert!(made["created_at"].is_i64(), "{made:#}");
    assert_eq!(show(&env, "dreamd")["key"], "DRM");
    assert_eq!(env.lcl(&["-w", "dreamd", "issue", "create", "--title", "t"]).assert_ok().stdout, "Created DRM-1\n");
    let out = env.lcl(&["workspace", "create", "R&D", "--key", "RD"]).assert_ok().stdout.clone();
    assert_eq!(out, format!("Created workspace \"R&D\" (key RD) at {}\n", projects_dir(&env).join("r-d.sqlite").display()));

    // A stray file that nothing registered.
    std::fs::write(projects_dir(&env).join("stray.sqlite"), b"not mine").expect("write stray");

    let before = files(&env);
    let rows = registry_rows(&env);
    assert_eq!(rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["dreamd", "r-d"]);
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
    assert_eq!(keys, ["created_at", "key", "name", "path", "schema_version"]);
    assert_eq!((&shown["name"], &shown["key"]), (&json!("dreamd"), &json!("DRM")));
    assert_eq!(shown["path"], json!(projects_dir(&env).join("dreamd.sqlite").display().to_string()));
    assert_eq!(shown["schema_version"], json!(SCHEMA_VERSION));
    let created = shown["created_at"].as_i64().unwrap_or_else(|| panic!("created_at is not an integer: {shown:#}"));
    assert!((start..=lcl_core::models::now()).contains(&created), "{created}");

    // A workspace in the current directory, found from cwd.
    env.lcl(&["init", "here"]).assert_ok();
    let here = env.lcl(&["workspace", "show", "--json"]).assert_ok().json();
    assert_eq!(here["path"], json!(env.db_path().display().to_string()));
    assert!(here["created_at"].is_i64(), "{here:#}");
    let human = env.lcl(&["-w", "dreamd", "workspace", "show"]).assert_ok().stdout.clone();
    assert_eq!(human, format!("dreamd [DRM] {}\n", projects_dir(&env).join("dreamd.sqlite").display()));

    let none = Env::new();
    let out = none.lcl(&["workspace", "show"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("no workspace found; run lcl init or pass --workspace"), "{out:#?}");
    let out = none.lcl(&["-w", "nope", "issue", "list"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert!(out.stderr.contains("workspace \"nope\" not found: no file at"), "{out:#?}");
    assert!(!none.home.path().join(".lcl").exists(), "resolving a workspace created ~/.lcl");
}

/// `init` registers its workspace unless the slug is taken, and never
/// replaces another workspace's row.
#[test]
fn init_registers_without_replacing() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    let global = registry_rows(&env);
    env.lcl(&["init", "alpha"]).assert_ok();
    assert_eq!(registry_rows(&env), global);
    let other = Env::new();
    other.lcl(&["init", "here"]).assert_ok();
    assert_eq!(registry_rows(&other), [("here".to_string(), "here".to_string(), other.db_path().display().to_string())]);
}

/// F2: lcl needs HOME only for ~/.lcl: the registry, `-w` and
/// LCL_WORKSPACE, and init (which registers what it makes). Unset or empty
/// is an error there, never a fallback directory.
#[test]
fn no_home_is_an_error_only_where_home_is_needed() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here"]).assert_ok();
    env.create("in the cwd workspace");
    let message = "Error: HOME is not set; lcl keeps its workspace registry in $HOME/.lcl\n";
    let fresh = tempfile::TempDir::new().expect("temp cwd");
    for (label, unset) in [("HOME unset", true), ("HOME empty", false)] {
        let without = |args: &[&str], cwd: &Path| {
            env.lcl_with(args, |cmd| {
                cmd.current_dir(cwd);
                if unset { cmd.env_remove("HOME"); } else { cmd.env("HOME", ""); }
            })
        };
        for args in [
            vec!["workspace", "list"],
            vec!["workspace", "create", "x", "--key", "X"],
            vec!["-w", "alpha", "issue", "list"],
            vec!["init", "here"],
        ] {
            let out = without(&args, fresh.path());
            assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (1, "", message), "{label}: {args:?}");
            assert_eq!(std::fs::read_dir(fresh.path()).unwrap().count(), 0, "{label}: {args:?} left something in the cwd");
        }
        // LCL_WORKSPACE needs HOME as -w does.
        let out = env.lcl_with(&["issue", "list"], |cmd| {
            cmd.env("LCL_WORKSPACE", "alpha");
            if unset { cmd.env_remove("HOME"); } else { cmd.env("HOME", ""); }
        });
        assert_eq!((out.code, out.stderr.as_str()), (1, message), "{label}: LCL_WORKSPACE");
        // A workspace in the current directory doesn't need HOME.
        let out = without(&["issue", "list"], env.cwd.path());
        assert_eq!((out.code, out.stdout.as_str()), (0, "HERE-1\t[Backlog]\tNone\tin the cwd workspace\n"), "{label}: {out:#?}");
        let out = without(&["workspace", "show", "--json"], env.cwd.path());
        assert_eq!((out.code, out.json()["key"].clone()), (0, json!("HERE")), "{label}: {out:#?}");
    }
    assert_eq!(registry_rows(&env).len(), 2, "alpha and here, nothing more");
}

/// F3: an empty LCL_WORKSPACE or LCL_PROJECT counts as unset. An empty
/// name typed on the command line is still refused.
#[test]
fn empty_workspace_variables_count_as_unset() {
    let env = Env::new();
    workspace(&env, "alpha", "ALPHA");
    env.lcl(&["init", "here"]).assert_ok();
    let notice = "lcl: LCL_PROJECT is deprecated; use LCL_WORKSPACE\n";
    let key = |vars: &[(&str, &str)], args: &[&str]| {
        let out = env.lcl_env(vars, args);
        let key = if out.code == 0 { out.json()["key"].as_str().unwrap_or_default().to_string() } else { String::new() };
        (out.code, key, out.stderr)
    };
    let show = ["workspace", "show", "--json"];
    assert_eq!(key(&[("LCL_WORKSPACE", ""), ("LCL_PROJECT", "alpha")], &show), (0, "ALPHA".into(), notice.into()));
    assert_eq!(key(&[("LCL_WORKSPACE", "")], &show), (0, "HERE".into(), String::new()));
    assert_eq!(key(&[("LCL_PROJECT", "")], &show), (0, "HERE".into(), String::new()));
    assert_eq!(key(&[("LCL_WORKSPACE", ""), ("LCL_PROJECT", "")], &show), (0, "HERE".into(), String::new()));
    let refused = "Error: workspace name \"\" has no letters or digits\n";
    for args in [vec!["-w", "", "workspace", "show"], vec!["--workspace=", "workspace", "show"]] {
        for vars in [vec![], vec![("LCL_WORKSPACE", "")], vec![("LCL_WORKSPACE", "alpha")]] {
            let out = env.lcl_env(&vars, &args);
            assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (2, "", refused), "{args:?} {vars:?}");
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
        let cmds: Vec<Vec<String>> = (0..PROCS).map(|_| argv(&["init", "race"])).collect();
        for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
            assert_eq!(out.code, 0, "round {round}, process {i}: {out:#?}");
        }
        let conn = rusqlite::Connection::open(env.db_path()).expect("open db");
        let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0)).expect("journal_mode");
        assert_eq!(mode, "wal", "round {round}");
        drop(conn);
        assert_eq!(registry_rows(&env), [("race".to_string(), "race".to_string(), env.db_path().display().to_string())], "round {round}");
    }
}

/// Twenty creates at once, on a HOME with no index yet: all land.
#[test]
fn concurrent_creates_of_different_names_all_land() {
    const N: usize = 20;
    let env = Env::new();
    let cmds: Vec<Vec<String>> = (0..N).map(|i| argv(&["workspace", "create", &format!("w{i}"), "--key", &format!("W{i}")])).collect();
    for (i, out) in env.lcl_parallel(&cmds).iter().enumerate() {
        assert_eq!(out.code, 0, "w{i}: {out:#?}");
    }
    let list = env.lcl(&["workspace", "list", "--json"]).assert_ok().json();
    assert_eq!(list.as_array().map(Vec::len), Some(N), "{list:#}");
    assert!(list.as_array().unwrap().iter().all(|w| w["missing"] == false), "{list:#}");
    for i in 0..N {
        assert_eq!(show(&env, &format!("w{i}"))["key"], format!("W{i}"));
    }
    let mut left: Vec<String> = std::fs::read_dir(projects_dir(&env)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
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
    let cmds: Vec<Vec<String>> = (0..N).map(|i| argv(&["workspace", "create", "same", "--key", &format!("S{i}"), "--json"])).collect();
    let outs = env.lcl_parallel(&cmds);
    let winners: Vec<usize> = (0..N).filter(|&i| outs[i].code == 0).collect();
    assert_eq!(winners.len(), 1, "{outs:#?}");
    for (i, out) in outs.iter().enumerate() {
        if i != winners[0] {
            assert_eq!(out.code, 1, "{i}: {out:#?}");
            assert!(out.stderr.contains("workspace \"same\" already exists"), "{i}: {out:#?}");
        }
    }
    let winner = outs[winners[0]].json();
    assert_eq!(registry_rows(&env).len(), 1);
    assert_eq!(show(&env, "same")["key"], winner["key"]);
    let left: Vec<String> = std::fs::read_dir(projects_dir(&env)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["same.sqlite"], "temporary files left behind");
}
