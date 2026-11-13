//! A doc's parent is checked as an issue's is: it must exist, and a doc
//! can't be moved under itself or a doc below it.

mod common;

use common::{dump, Env};
use serde_json::{json, Value};

fn doc(env: &Env, args: &[&str]) -> Value {
    let mut all = vec!["--json", "doc"];
    all.extend_from_slice(args);
    env.lcl(&all).assert_ok().json()
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_string()
}

fn parent_of(env: &Env, d: &str) -> Value {
    doc(env, &["show", d])["parent_id"].clone()
}

#[test]
fn a_new_doc_needs_a_parent_that_exists() {
    let env = Env::new();
    env.lcl(&["init", "docs", "--key", "DOCS"]).assert_ok();
    let before = dump(&env.db_path());
    let out = env.lcl(&["doc", "create", "--title", "orphan", "--parent", "nope"]);
    assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (1, "", "Error: doc nope not found\n"));
    let missing = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let out = env.lcl(&["doc", "create", "--title", "orphan", "--parent", missing]);
    assert_eq!((out.code, out.stderr.as_str()), (1, format!("Error: doc {missing} not found\n").as_str()));
    assert!(dump(&env.db_path()) == before, "a refused create wrote");
    // An existing parent, given in upper case, is stored as lcl stores ids.
    let top = id(&doc(&env, &["create", "--title", "top"]));
    let child = doc(&env, &["create", "--title", "child", "--parent", &top.to_uppercase()]);
    assert_eq!(child["parent_id"], json!(top));
}

#[test]
fn a_doc_moves_only_where_it_makes_no_cycle() {
    let env = Env::new();
    env.lcl(&["init", "docs", "--key", "DOCS"]).assert_ok();
    let a = id(&doc(&env, &["create", "--title", "a"]));
    let b = id(&doc(&env, &["create", "--title", "b", "--parent", &a]));
    let c = id(&doc(&env, &["create", "--title", "c", "--parent", &b]));
    let other = id(&doc(&env, &["create", "--title", "other"]));
    let db = env.db_path();
    let before = dump(&db);
    for (args, want) in [
        (vec!["doc", "update", &a, "--parent", &a], format!("Error: doc {a} can't be its own parent\n")),
        (vec!["doc", "update", &a, "--parent", &b], format!("Error: making doc {b} the parent of doc {a} would make a cycle: {a} -> {b} -> {a}\n")),
        (vec!["doc", "update", &a, "--parent", &c], format!("Error: making doc {c} the parent of doc {a} would make a cycle: {a} -> {c} -> {b} -> {a}\n")),
        (vec!["doc", "update", &a, "--parent", "nope", "--title", "renamed"], "Error: doc nope not found\n".to_string()),
    ] {
        let out = env.lcl(&args);
        assert_eq!((out.code, out.stdout.as_str(), out.stderr.as_str()), (1, "", want.as_str()), "{args:?}");
        assert!(dump(&db) == before, "{args:?}: a refused update wrote");
    }
    let out = env.lcl(&["doc", "update", &a, "--parent", &other, "--no-parent"]);
    assert_eq!(out.code, 2, "--parent and --no-parent conflict: {out:#?}");

    // Moves that make no cycle are fine, and --no-parent makes a doc top-level.
    assert_eq!(doc(&env, &["update", &c, "--parent", &a])["parent_id"], json!(a));
    assert_eq!(doc(&env, &["update", &a, "--parent", &other])["parent_id"], json!(other));
    assert_eq!(parent_of(&env, &b), json!(a), "moving a doc leaves its children with it");
    assert_eq!(doc(&env, &["update", &a, "--no-parent"])["parent_id"], Value::Null);
    // An update that doesn't name the parent leaves it.
    assert_eq!(doc(&env, &["update", &b, "--title", "b2"])["parent_id"], json!(a));
}
