//! The `--json` contract, pinned: `docs/json-contract.md` is the pin.
//!
//! Each command's section in that note holds the shape of its `--json`
//! output. A test per command runs it and checks what it printed against
//! that shape, so a shape can't change without the note changing with it.
//!
//! What holds, and is tested here:
//!
//! - Every command `lcl --help` reaches has a section in the note, and every
//!   section names such a command.
//! - Every section with a shape has a test below, and every test a section.
//! - An object has exactly the keys its shape shows, and each value has the
//!   type its shape names.
//! - A shape claims nothing the tests never saw: for every `X|null` a test
//!   printed both a null and a value, and for every array an element. Inside
//!   a named object that holds for the object's own test (`object_*`), not
//!   for each command that prints one.
//! - stdout of a success is one JSON value. A failure prints nothing on
//!   stdout, exits 1 or 2, and ends stderr with the one error line.
//!
//! A slice that adds or changes a command changes the note, then this file.

mod common;
mod sync_service;

use common::{Env, Out};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use sync_service::{Service, TOKEN};

const NOTE: &str = include_str!("../../../docs/json-contract.md");

/// The note's shapes: a command's by its words after `lcl` (None for a
/// section without one), a named object's by its name.
struct Note {
    commands: BTreeMap<String, Option<Value>>,
    objects: BTreeMap<String, Value>,
}

/// Reads the note. A section starts at a heading `### \`NAME\`` and runs to
/// the next heading; its shape is its first fenced `json` block.
fn note() -> Note {
    let mut note = Note {
        commands: BTreeMap::new(),
        objects: BTreeMap::new(),
    };
    let mut lines = NOTE.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .strip_prefix("### `")
            .and_then(|rest| rest.strip_suffix('`'))
        else {
            continue;
        };
        let mut shape = None;
        while let Some(next) = lines.next_if(|l| !l.starts_with('#')) {
            if next != "```json" || shape.is_some() {
                continue;
            }
            let mut block = String::new();
            for l in lines.by_ref() {
                if l == "```" {
                    break;
                }
                block.push_str(l);
                block.push('\n');
            }
            shape = Some(
                serde_json::from_str::<Value>(&block)
                    .unwrap_or_else(|e| panic!("the shape of `{name}` is not JSON: {e}\n{block}")),
            );
        }
        let fresh = match name.strip_prefix("lcl ") {
            Some(command) => note.commands.insert(command.to_string(), shape).is_none(),
            None => {
                let shape = shape.unwrap_or_else(|| panic!("object `{name}` has no shape"));
                note.objects.insert(name.to_string(), shape).is_none()
            }
        };
        assert!(fresh, "the note has two sections named `{name}`");
    }
    note
}

/// One alternative of a leaf such as `"'cwd'|'global'"` or `"Issue|null"`.
fn is_object_name(alt: &str) -> bool {
    alt.starts_with(|c: char| c.is_ascii_uppercase())
}

/// Checks values against one shape, and records which of the shape's claims
/// the values bore out.
struct Checker<'a> {
    objects: &'a BTreeMap<String, Value>,
    /// What the shape claims can happen: `PATH is null`, `PATH has a value`,
    /// `PATH has an element`. Not collected inside a named object.
    claimed: BTreeSet<String>,
    seen: BTreeSet<String>,
    errors: Vec<String>,
}

impl<'a> Checker<'a> {
    fn object(&self, name: &str) -> &'a Value {
        self.objects
            .get(name)
            .unwrap_or_else(|| panic!("the note has no object `{name}`"))
    }

    /// The keys of an object shape, with `"..."` replaced by the keys of the
    /// object it names. The flag says the key came from a named object.
    fn keys(
        &self,
        shape: &'a serde_json::Map<String, Value>,
    ) -> BTreeMap<&'a str, (&'a Value, bool)> {
        let mut keys = BTreeMap::new();
        for (k, v) in shape {
            if k != "..." {
                assert!(
                    keys.insert(k.as_str(), (v, false)).is_none(),
                    "key {k} twice"
                );
                continue;
            }
            let name = v.as_str().expect("\"...\" names an object");
            let Value::Object(spread) = self.object(name) else {
                panic!("\"...\": \"{name}\" must name an object shape");
            };
            for (k, v) in spread {
                assert!(
                    keys.insert(k.as_str(), (v, true)).is_none(),
                    "key {k} twice"
                );
            }
        }
        keys
    }

    /// Collects what `shape` claims, outside named objects.
    fn claims(&mut self, shape: &'a Value, path: &str) {
        match shape {
            Value::String(leaf) => {
                let alts: Vec<&str> = leaf.split('|').collect();
                if alts.len() > 1 && alts.contains(&"null") {
                    self.claimed.insert(format!("{path} is null"));
                    self.claimed.insert(format!("{path} has a value"));
                }
            }
            Value::Array(elems) => {
                assert_eq!(elems.len(), 1, "{path}: an array shape has one element");
                self.claimed.insert(format!("{path} has an element"));
                self.claims(&elems[0], &format!("{path}[]"));
            }
            Value::Object(map) => {
                for (k, (v, named)) in self.keys(map) {
                    if !named {
                        self.claims(v, &format!("{path}.{k}"));
                    }
                }
            }
            other => panic!("{path}: {other} is not a shape"),
        }
    }

    fn check(&mut self, shape: &'a Value, actual: &Value, path: &str) {
        match shape {
            Value::String(leaf) => self.check_leaf(leaf, actual, path),
            Value::Array(elems) => {
                let Value::Array(items) = actual else {
                    self.errors
                        .push(format!("{path}: expected an array, got {actual}"));
                    return;
                };
                if !items.is_empty() {
                    self.seen.insert(format!("{path} has an element"));
                }
                for item in items {
                    self.check(&elems[0], item, &format!("{path}[]"));
                }
            }
            Value::Object(map) => {
                let Value::Object(fields) = actual else {
                    self.errors
                        .push(format!("{path}: expected an object, got {actual}"));
                    return;
                };
                let keys = self.keys(map);
                for k in fields.keys().filter(|k| !keys.contains_key(k.as_str())) {
                    self.errors
                        .push(format!("{path}: key \"{k}\" is not in the shape"));
                }
                for (k, (v, _)) in keys {
                    match fields.get(k) {
                        Some(field) => self.check(v, field, &format!("{path}.{k}")),
                        None => self.errors.push(format!("{path}: key \"{k}\" is missing")),
                    }
                }
            }
            other => panic!("{path}: {other} is not a shape"),
        }
    }

    fn check_leaf(&mut self, leaf: &'a str, actual: &Value, path: &str) {
        self.seen.insert(if actual.is_null() {
            format!("{path} is null")
        } else {
            format!("{path} has a value")
        });
        // Named alternatives that are themselves leaves (Status) join the
        // alternatives here; one that is an object or array is checked whole.
        let mut alts: Vec<&'a str> = leaf.split('|').collect();
        let mut structure = None;
        let mut i = 0;
        while i < alts.len() {
            if is_object_name(alts[i]) {
                match self.object(alts[i]) {
                    Value::String(inner) => alts.extend(inner.split('|')),
                    other => structure = Some(other),
                }
            }
            i += 1;
        }
        let fits = alts.iter().any(|alt| match *alt {
            "null" => actual.is_null(),
            "string" => actual.is_string(),
            "integer" => actual.is_i64() || actual.is_u64(),
            "boolean" => actual.is_boolean(),
            alt if is_object_name(alt) => false,
            alt => {
                let literal = alt
                    .strip_prefix('\'')
                    .and_then(|a| a.strip_suffix('\''))
                    .unwrap_or_else(|| panic!("{path}: `{alt}` is not a shape"));
                actual.as_str() == Some(literal)
            }
        });
        if fits {
            return;
        }
        match structure {
            Some(shape) if actual.is_object() || actual.is_array() => {
                self.check(shape, actual, path)
            }
            _ => self
                .errors
                .push(format!("{path}: expected {leaf}, got {actual}")),
        }
    }
}

/// Fails unless every sample has `shape`, and the samples between them show
/// everything the shape claims.
#[track_caller]
fn pin_shape(what: &str, shape: &Value, objects: &BTreeMap<String, Value>, samples: &[Value]) {
    assert!(!samples.is_empty(), "{what}: no samples");
    let mut checker = Checker {
        objects,
        claimed: BTreeSet::new(),
        seen: BTreeSet::new(),
        errors: Vec::new(),
    };
    checker.claims(shape, "$");
    for sample in samples {
        checker.check(shape, sample, "$");
    }
    let mut errors = checker.errors;
    errors.extend(
        checker
            .claimed
            .difference(&checker.seen)
            .map(|claim| format!("no sample where {claim}")),
    );
    errors.sort();
    errors.dedup();
    assert!(
        errors.is_empty(),
        "{what} differs from its shape in docs/json-contract.md:\n  {}\nshape: {shape:#}\nsamples: {samples:#?}",
        errors.join("\n  ")
    );
}

/// Pins a command's output to its section in the note.
#[track_caller]
fn pin(command: &str, samples: &[Value]) {
    let note = note();
    let shape = note
        .commands
        .get(command)
        .unwrap_or_else(|| panic!("the note has no section `lcl {command}`"))
        .as_ref()
        .unwrap_or_else(|| panic!("the note's section `lcl {command}` has no shape"));
    pin_shape(&format!("`lcl {command}`"), shape, &note.objects, samples);
}

/// Pins a named object: here its own nulls and arrays must all be shown.
#[track_caller]
fn pin_object(name: &str, samples: &[Value]) {
    let note = note();
    let shape = note
        .objects
        .get(name)
        .unwrap_or_else(|| panic!("the note has no object `{name}`"));
    pin_shape(&format!("object `{name}`"), shape, &note.objects, samples);
}

/// `lcl ARGS --json`, which must succeed, as its one JSON value.
#[track_caller]
fn j(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    env.lcl(&argv).assert_ok().json()
}

/// As `j`, with USER unset, so nothing names a creator or an author.
#[track_caller]
fn j_anonymous(env: &Env, args: &[&str]) -> Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    env.lcl_with(&argv, |cmd| {
        cmd.env_remove("USER");
    })
    .assert_ok()
    .json()
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("an id").to_string()
}

/// A workspace with one of everything, and of each thing one that is full
/// and one that is bare.
///
/// - Labels `bug`. Projects `Parser` (dated) and `Bare`. Milestones `Beta`
///   (dated) and `Later`, both in Parser. Cycles `Sprint 1` (dated), `Open`.
/// - DEMO-1: every field set, In Progress. A comment by ann, one by nobody.
/// - DEMO-2: under DEMO-1, bare, created by nobody. Related to DEMO-1.
/// - DEMO-3: blocks DEMO-1, duplicate of DEMO-2 (so Duplicate, canceled).
/// - DEMO-4: Done.
/// - Docs `Notes`, and `Child` under it.
struct Rich {
    env: Env,
    comment: String,
    notes: String,
    child: String,
}

fn rich() -> Rich {
    let env = Env::new();
    j(&env, &["init", "demo", "--key", "DEMO"]);
    j(&env, &["label", "create", "bug"]);
    j(
        &env,
        &["project", "create", "Parser", "--target-date", "2026-12-31"],
    );
    j(&env, &["project", "create", "Bare"]);
    j(
        &env,
        &[
            "milestone",
            "create",
            "Beta",
            "--project",
            "Parser",
            "--target-date",
            "2026-12-15",
        ],
    );
    j(
        &env,
        &["milestone", "create", "Later", "--project", "Parser"],
    );
    j(
        &env,
        &[
            "cycle",
            "create",
            "Sprint 1",
            "--start",
            "2026-11-16",
            "--end",
            "2026-11-29",
        ],
    );
    j(&env, &["cycle", "create", "Open"]);
    j(
        &env,
        &[
            "issue",
            "create",
            "--title",
            "one",
            "--description",
            "words",
            "--status",
            "in progress",
            "--priority",
            "high",
            "--assignee",
            "ann",
            "--label",
            "bug",
            "--project",
            "Parser",
            "--milestone",
            "Beta",
            "--cycle",
            "Sprint 1",
            "--estimate",
            "3",
            "--due",
            "2026-12-01",
            "--ref",
            "BZR-1",
            "--author",
            "ann",
        ],
    );
    j_anonymous(
        &env,
        &["issue", "create", "--title", "two", "--parent", "1"],
    );
    j(
        &env,
        &["issue", "create", "--title", "three", "--author", "ann"],
    );
    j(
        &env,
        &[
            "issue", "create", "--title", "four", "--status", "done", "--author", "ann",
        ],
    );
    j(&env, &["issue", "relate", "3", "blocks", "1"]);
    j(&env, &["issue", "relate", "2", "related", "1"]);
    j(&env, &["issue", "relate", "3", "duplicate-of", "2"]);
    let comment = id(&j(
        &env,
        &["issue", "comment", "1", "hello", "--author", "ann"],
    ));
    j_anonymous(&env, &["issue", "comment", "1", "unsigned"]);
    let notes = id(&j(
        &env,
        &["doc", "create", "--title", "Notes", "--content", "words"],
    ));
    let child = id(&j(
        &env,
        &["doc", "create", "--title", "Child", "--parent", &notes],
    ));
    Rich {
        env,
        comment,
        notes,
        child,
    }
}

/// One test per command: `$test` runs `cases::$test`, and what it returns
/// must have the shape of the note's section `lcl $command`.
macro_rules! pinned {
    ($($test:ident => $command:literal,)*) => {
        /// Every command a test below pins.
        const PINNED: &[&str] = &[$($command),*];
        $(
            #[test]
            fn $test() {
                pin($command, &cases::$test());
            }
        )*
    };
}

pinned! {
    init => "init",
    workspace_create => "workspace create",
    workspace_list => "workspace list",
    workspace_show => "workspace show",
    issue_create => "issue create",
    issue_list => "issue list",
    issue_show => "issue show",
    issue_update => "issue update",
    issue_delete => "issue delete",
    issue_restore => "issue restore",
    issue_purge => "issue purge",
    issue_relate => "issue relate",
    issue_unrelate => "issue unrelate",
    issue_comment => "issue comment",
    issue_comments => "issue comments",
    issue_history => "issue history",
    comment_edit => "comment edit",
    comment_delete => "comment delete",
    comment_restore => "comment restore",
    comment_purge => "comment purge",
    project_create => "project create",
    project_list => "project list",
    project_show => "project show",
    project_update => "project update",
    project_delete => "project delete",
    milestone_create => "milestone create",
    milestone_list => "milestone list",
    milestone_show => "milestone show",
    milestone_update => "milestone update",
    milestone_delete => "milestone delete",
    label_create => "label create",
    label_list => "label list",
    label_delete => "label delete",
    label_restore => "label restore",
    label_purge => "label purge",
    cycle_create => "cycle create",
    cycle_list => "cycle list",
    cycle_update => "cycle update",
    cycle_delete => "cycle delete",
    doc_create => "doc create",
    doc_list => "doc list",
    doc_show => "doc show",
    doc_update => "doc update",
    doc_delete => "doc delete",
    doc_restore => "doc restore",
    doc_purge => "doc purge",
    search => "search",
    backup => "backup",
    export => "export",
    import => "import",
    sync_start => "sync start",
    sync_join => "sync join",
    sync => "sync",
    sync_leave => "sync leave",
    sync_stop => "sync stop",
}

/// What each command prints, for `pinned!`: enough samples to show every
/// null and every array element its shape claims.
mod cases {
    use super::*;

    pub fn init() -> Vec<Value> {
        let env = Env::new();
        let made = j(&env, &["init", "demo", "--key", "DEMO"]);
        let again = j(&env, &["init", "demo", "--key", "DEMO"]);
        assert_eq!(
            (&made["created"], &again["created"]),
            (&true.into(), &false.into())
        );
        vec![made, again]
    }

    pub fn workspace_create() -> Vec<Value> {
        vec![j(
            &Env::new(),
            &["workspace", "create", "glob", "--key", "GLB"],
        )]
    }

    pub fn workspace_list() -> Vec<Value> {
        let env = Env::new();
        j(&env, &["init", "demo", "--key", "DEMO"]);
        j(&env, &["workspace", "create", "glob", "--key", "GLB"]);
        vec![j(&env, &["workspace", "list"])]
    }

    /// Not syncing; syncing; and opted in, but the first push failed, so
    /// not yet synced.
    pub fn workspace_show() -> Vec<Value> {
        let svc = Service::start();
        let synced = Env::new();
        j(&synced, &["init", "demo", "--key", "DEMO"]);
        with_token(&synced, &["sync", "start", &svc.url]);
        let unfinished = Env::new();
        j(&unfinished, &["init", "demo", "--key", "DEMO"]);
        j(&unfinished, &["issue", "create", "--title", "one"]);
        svc.state().lose_next_push_answer = true;
        let out = unfinished.lcl_env(
            &[("LCL_SYNC_TOKEN", TOKEN)],
            &["sync", "start", &svc.url, "--json"],
        );
        assert_eq!(out.code, 1, "{out:#?}");
        vec![
            j(&rich().env, &["workspace", "show"]),
            j(&synced, &["workspace", "show"]),
            j(&unfinished, &["workspace", "show"]),
        ]
    }

    /// `lcl ARGS --json` with the reference service's token.
    fn with_token(env: &Env, args: &[&str]) -> Value {
        let mut argv = args.to_vec();
        argv.push("--json");
        env.lcl_env(&[("LCL_SYNC_TOKEN", TOKEN)], &argv)
            .assert_ok()
            .json()
    }

    pub fn sync_start() -> Vec<Value> {
        let svc = Service::start();
        vec![with_token(&rich().env, &["sync", "start", &svc.url])]
    }

    pub fn sync_join() -> Vec<Value> {
        let svc = Service::start();
        let started = with_token(&rich().env, &["sync", "start", &svc.url]);
        let env = Env::new();
        j(&env, &["init", "demo", "--key", "DEMO"]);
        let ws = started["workspace"].as_str().unwrap();
        vec![with_token(&env, &["sync", "join", &svc.url, ws])]
    }

    /// Nothing to report; then an issue's change, a comment's and a
    /// label's, each on a record the service purged; then a field this
    /// machine changed, overwritten by another's later change.
    pub fn sync() -> Vec<Value> {
        let svc = Service::start();
        let env = rich().env;
        j(&env, &["label", "create", "gone-there"]);
        let started = with_token(&env, &["sync", "start", &svc.url]);
        let ws = started["workspace"].as_str().unwrap().to_string();
        let quiet = with_token(&env, &["sync"]);
        let issue = id(&j(&env, &["issue", "show", "4"])["issue"]);
        let label = j(&env, &["label", "list"])
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["name"] == "gone-there")
            .map(id)
            .unwrap();
        svc.state().purge(&ws, "issue", &issue);
        svc.state().purge(&ws, "label", &label);
        j(&env, &["issue", "update", "4", "--title", "edited"]);
        j(&env, &["issue", "comment", "4", "late"]);
        j(&env, &["label", "delete", "gone-there"]);
        let refused = with_token(&env, &["sync"]);

        let other = Env::new();
        j(&other, &["init", "demo", "--key", "DEMO"]);
        with_token(&other, &["sync", "join", &svc.url, &ws]);
        j(&env, &["issue", "update", "2", "--title", "here"]);
        j(&other, &["issue", "update", "2", "--title", "there, later"]);
        with_token(&other, &["sync"]);
        let overwritten = with_token(&env, &["sync"]);
        assert_eq!(overwritten["overwritten"][0]["identifier"], "DEMO-2");
        vec![quiet, refused, overwritten]
    }

    /// Leaving a replica, then again with none.
    pub fn sync_leave() -> Vec<Value> {
        let svc = Service::start();
        let env = rich().env;
        with_token(&env, &["sync", "start", &svc.url]);
        let path = env.cwd.path().join("left.jsonl");
        let p = path.to_str().unwrap();
        let left = with_token(&env, &["sync", "leave", "--output", p]);
        let again = j(&env, &["sync", "leave", "--output", p, "--force"]);
        vec![left, again]
    }

    pub fn sync_stop() -> Vec<Value> {
        let svc = Service::start();
        let env = rich().env;
        with_token(&env, &["sync", "start", &svc.url]);
        vec![j(&env, &["sync", "stop"])]
    }

    pub fn issue_create() -> Vec<Value> {
        let env = rich().env;
        vec![j(&env, &["issue", "create", "--title", "new"])]
    }

    pub fn issue_list() -> Vec<Value> {
        let env = rich().env;
        j(&env, &["issue", "delete", "4"]);
        vec![
            j(&env, &["issue", "list"]),
            j(&env, &["issue", "list", "--trashed"]),
        ]
    }

    pub fn issue_show() -> Vec<Value> {
        let env = rich().env;
        ["1", "2", "3"]
            .iter()
            .map(|n| j(&env, &["issue", "show", n]))
            .collect()
    }

    pub fn issue_update() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["issue", "update", "2", "--status", "todo"],
        )]
    }

    pub fn issue_delete() -> Vec<Value> {
        vec![j(&rich().env, &["issue", "delete", "4"])]
    }

    pub fn issue_restore() -> Vec<Value> {
        let env = rich().env;
        j(&env, &["issue", "delete", "4"]);
        vec![j(&env, &["issue", "restore", "4"])]
    }

    pub fn issue_purge() -> Vec<Value> {
        let env = rich().env;
        j(&env, &["issue", "delete", "4"]);
        j(&env, &["issue", "delete", "3"]);
        vec![
            j(&env, &["issue", "purge", "4"]),
            j(&env, &["issue", "purge", "--all"]),
            j(&env, &["issue", "purge", "--all"]),
        ]
    }

    pub fn issue_relate() -> Vec<Value> {
        let env = rich().env;
        vec![
            j(&env, &["issue", "relate", "4", "blocks", "1"]),
            j(&env, &["issue", "relate", "1", "blocked-by", "4"]),
            j(&env, &["issue", "relate", "4", "related", "3"]),
            j(&env, &["issue", "relate", "4", "duplicate-of", "1"]),
        ]
    }

    pub fn issue_unrelate() -> Vec<Value> {
        let env = rich().env;
        vec![
            j(&env, &["issue", "unrelate", "3", "blocks", "1"]),
            j(&env, &["issue", "unrelate", "3", "duplicate-of", "2"]),
        ]
    }

    pub fn issue_comment() -> Vec<Value> {
        vec![j(&rich().env, &["issue", "comment", "2", "a comment"])]
    }

    /// Issue 1's history after writes with an author and without one: a
    /// field set and cleared, a label removed, and a comment added and
    /// moved to the trash.
    pub fn issue_history() -> Vec<Value> {
        let rich = rich();
        let env = &rich.env;
        j(env, &["issue", "update", "1", "--assignee", "ann"]);
        env.lcl_with(
            &["issue", "update", "1", "--no-assignee", "--clear-labels"],
            |cmd| {
                cmd.env_remove("USER");
            },
        )
        .assert_ok();
        let c = j(env, &["issue", "comment", "1", "a word"]);
        j(env, &["comment", "delete", &id(&c)]);
        vec![j(env, &["issue", "history", "1"])]
    }

    pub fn issue_comments() -> Vec<Value> {
        vec![j(&rich().env, &["issue", "comments", "1"])]
    }

    pub fn comment_edit() -> Vec<Value> {
        let rich = rich();
        vec![j(&rich.env, &["comment", "edit", &rich.comment, "edited"])]
    }

    pub fn comment_delete() -> Vec<Value> {
        let rich = rich();
        vec![j(&rich.env, &["comment", "delete", &rich.comment])]
    }

    pub fn comment_restore() -> Vec<Value> {
        let rich = rich();
        j(&rich.env, &["comment", "delete", &rich.comment]);
        vec![j(&rich.env, &["comment", "restore", &rich.comment])]
    }

    pub fn comment_purge() -> Vec<Value> {
        let rich = rich();
        j(&rich.env, &["comment", "delete", &rich.comment]);
        vec![
            j(&rich.env, &["comment", "purge", &rich.comment]),
            j(&rich.env, &["comment", "purge", "--all"]),
        ]
    }

    pub fn project_create() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["project", "create", "Lexer", "--status", "planned"],
        )]
    }

    pub fn project_list() -> Vec<Value> {
        vec![j(&rich().env, &["project", "list"])]
    }

    pub fn project_show() -> Vec<Value> {
        vec![j(&rich().env, &["project", "show", "Parser"])]
    }

    pub fn project_update() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["project", "update", "Bare", "--status", "started"],
        )]
    }

    pub fn project_delete() -> Vec<Value> {
        vec![j(&rich().env, &["project", "delete", "Bare"])]
    }

    pub fn milestone_create() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["milestone", "create", "GA", "--project", "Bare"],
        )]
    }

    pub fn milestone_list() -> Vec<Value> {
        vec![j(&rich().env, &["milestone", "list"])]
    }

    pub fn milestone_show() -> Vec<Value> {
        vec![j(&rich().env, &["milestone", "show", "Beta"])]
    }

    pub fn milestone_update() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["milestone", "update", "Later", "--sort-order", "2"],
        )]
    }

    pub fn milestone_delete() -> Vec<Value> {
        vec![j(&rich().env, &["milestone", "delete", "Later"])]
    }

    pub fn label_create() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["label", "create", "urgent", "--color", "#d73a4a"],
        )]
    }

    pub fn label_list() -> Vec<Value> {
        vec![j(&rich().env, &["label", "list"])]
    }

    pub fn label_delete() -> Vec<Value> {
        vec![j(&rich().env, &["label", "delete", "bug"])]
    }

    pub fn label_restore() -> Vec<Value> {
        let env = rich().env;
        j(&env, &["label", "delete", "bug"]);
        vec![j(&env, &["label", "restore", "bug"])]
    }

    pub fn label_purge() -> Vec<Value> {
        let env = rich().env;
        j(&env, &["label", "delete", "bug"]);
        vec![
            j(&env, &["label", "purge", "bug"]),
            j(&env, &["label", "purge", "--all"]),
        ]
    }

    pub fn cycle_create() -> Vec<Value> {
        vec![j(&rich().env, &["cycle", "create", "Sprint 2"])]
    }

    pub fn cycle_list() -> Vec<Value> {
        vec![j(&rich().env, &["cycle", "list"])]
    }

    pub fn cycle_update() -> Vec<Value> {
        vec![j(
            &rich().env,
            &["cycle", "update", "Open", "--end", "2026-12-01"],
        )]
    }

    pub fn cycle_delete() -> Vec<Value> {
        vec![j(&rich().env, &["cycle", "delete", "Open"])]
    }

    pub fn doc_create() -> Vec<Value> {
        vec![j(&rich().env, &["doc", "create", "--title", "More"])]
    }

    pub fn doc_list() -> Vec<Value> {
        vec![j(&rich().env, &["doc", "list"])]
    }

    pub fn doc_show() -> Vec<Value> {
        let rich = rich();
        vec![j(&rich.env, &["doc", "show", &rich.notes])]
    }

    pub fn doc_update() -> Vec<Value> {
        let rich = rich();
        vec![j(
            &rich.env,
            &["doc", "update", &rich.notes, "--title", "Renamed"],
        )]
    }

    pub fn doc_delete() -> Vec<Value> {
        let rich = rich();
        vec![j(&rich.env, &["doc", "delete", &rich.child])]
    }

    pub fn doc_restore() -> Vec<Value> {
        let rich = rich();
        j(&rich.env, &["doc", "delete", &rich.child]);
        vec![j(&rich.env, &["doc", "restore", &rich.child])]
    }

    pub fn doc_purge() -> Vec<Value> {
        let rich = rich();
        j(&rich.env, &["doc", "delete", &rich.child]);
        vec![
            j(&rich.env, &["doc", "purge", &rich.child]),
            j(&rich.env, &["doc", "purge", "--all"]),
        ]
    }

    pub fn search() -> Vec<Value> {
        let env = rich().env;
        vec![
            j(&env, &["search", "words"]),
            j(&env, &["search", "nowhere"]),
        ]
    }

    pub fn backup() -> Vec<Value> {
        vec![j(&rich().env, &["backup", "copy.sqlite"])]
    }

    pub fn export() -> Vec<Value> {
        vec![j(&rich().env, &["export", "--output", "out.jsonl"])]
    }

    pub fn import() -> Vec<Value> {
        let rich = rich();
        j(&rich.env, &["export", "--output", "out.jsonl"]);
        let file = rich.env.cwd.path().join("out.jsonl");
        let file = file.to_str().expect("a UTF-8 temp path");
        let empty = Env::new();
        j(&empty, &["init", "copy", "--key", "DEMO"]);
        let mut samples = vec![
            j(&empty, &["import", file, "--dry-run"]),
            j(&empty, &["import", file]),
        ];
        samples.push(import_with_unknown_types());
        samples
    }

    /// An import of a file with records of a type lcl doesn't know.
    pub fn import_with_unknown_types() -> Value {
        let rich = rich();
        j(&rich.env, &["export", "--output", "out.jsonl"]);
        let path = rich.env.cwd.path().join("out.jsonl");
        let mut text = std::fs::read_to_string(&path).expect("the export");
        text.push_str("{\"type\":\"attachment\",\"issue\":1}\n");
        std::fs::write(&path, text).expect("write the file back");
        let empty = Env::new();
        j(&empty, &["init", "copy", "--key", "DEMO"]);
        j(
            &empty,
            &["import", path.to_str().expect("a UTF-8 temp path")],
        )
    }
}

/// Each named object, with every null and every array element it claims.
/// (`Status`, `Priority` and `IssueCounts` claim none; the commands that
/// print them check them.)
mod objects {
    use super::*;

    #[test]
    fn object_issue() {
        let env = rich().env;
        let issues = j(&env, &["issue", "list"])["issues"]
            .as_array()
            .expect("issues")
            .clone();
        assert_eq!(issues.len(), 4);
        // And one in the trash, for `deleted_at`.
        j(&env, &["issue", "delete", "4"]);
        let mut issues = issues;
        issues.extend(
            j(&env, &["issue", "list", "--trashed"])["issues"]
                .as_array()
                .expect("issues")
                .clone(),
        );
        assert_eq!(issues.len(), 5);
        pin_object("Issue", &issues);
    }

    #[test]
    fn object_related_issue() {
        let shown = j(&rich().env, &["issue", "show", "1"]);
        pin_object(
            "RelatedIssue",
            &[shown["relations"]["blocked_by"][0].clone()],
        );
    }

    #[test]
    fn object_comment() {
        let rich = rich();
        let mut comments = j(&rich.env, &["issue", "comments", "1"])
            .as_array()
            .expect("comments")
            .clone();
        // And one in the trash, for `deleted_at`.
        j(&rich.env, &["comment", "delete", &rich.comment]);
        comments.extend(
            j(&rich.env, &["issue", "comments", "1", "--trashed"])
                .as_array()
                .expect("comments")
                .clone(),
        );
        assert_eq!(comments.len(), 3);
        pin_object("Comment", &comments);
    }

    #[test]
    fn object_project() {
        let projects = j(&rich().env, &["project", "list"]);
        pin_object("Project", projects.as_array().expect("projects"));
    }

    #[test]
    fn object_milestone() {
        let milestones = j(&rich().env, &["milestone", "list"]);
        pin_object("Milestone", milestones.as_array().expect("milestones"));
    }

    #[test]
    fn object_cycle() {
        let env = rich().env;
        pin_object(
            "Cycle",
            &[
                j(&env, &["cycle", "update", "Open", "--name", "Still open"]),
                j(
                    &env,
                    &["cycle", "update", "Sprint 1", "--name", "Sprint one"],
                ),
            ],
        );
    }

    #[test]
    fn object_label() {
        let env = rich().env;
        let mut labels = j(&env, &["label", "list"])
            .as_array()
            .expect("labels")
            .clone();
        j(&env, &["label", "delete", "bug"]);
        labels.extend(
            j(&env, &["label", "list", "--trashed"])
                .as_array()
                .expect("labels")
                .clone(),
        );
        assert_eq!(labels.len(), 2);
        pin_object("Label", &labels);
    }

    #[test]
    fn object_doc() {
        let rich = rich();
        let mut docs = j(&rich.env, &["doc", "list"])
            .as_array()
            .expect("docs")
            .clone();
        j(&rich.env, &["doc", "delete", &rich.child]);
        docs.extend(
            j(&rich.env, &["doc", "list", "--trashed"])
                .as_array()
                .expect("docs")
                .clone(),
        );
        assert_eq!(docs.len(), 3);
        pin_object("Doc", &docs);
    }

    #[test]
    fn object_history_entry_and_field_change() {
        let history = super::cases::issue_history();
        let entries: Vec<Value> = history[0].as_array().expect("entries").clone();
        pin_object("HistoryEntry", &entries);
        let changes: Vec<Value> = entries
            .iter()
            .flat_map(|e| e["changes"].as_array().expect("changes").clone())
            .collect();
        pin_object("FieldChange", &changes);
    }

    #[test]
    fn object_sync_status() {
        let shown = super::cases::workspace_show();
        let statuses: Vec<Value> = shown.iter().skip(1).map(|s| s["sync"].clone()).collect();
        pin_object("SyncStatus", &statuses);
    }

    #[test]
    fn object_sync_conflict() {
        let synced = super::cases::sync();
        let mut conflicts = synced[1]["not_applied"].as_array().unwrap().clone();
        assert_eq!(conflicts.len(), 3, "{conflicts:#?}");
        conflicts.extend(synced[2]["overwritten"].as_array().unwrap().clone());
        pin_object("SyncConflict", &conflicts);
    }

    #[test]
    fn object_skipped_records() {
        let out = super::cases::import_with_unknown_types();
        pin_object("SkippedRecords", &[out["skipped"][0].clone()]);
    }

    /// Every object in the note is pinned above or is one of the three that
    /// claim nothing, so a new object can't arrive without a test.
    #[test]
    fn every_object_is_pinned() {
        let named: Vec<String> = note().objects.into_keys().collect();
        assert_eq!(
            named,
            [
                "Comment",
                "Cycle",
                "Doc",
                "Error",
                "FieldChange",
                "HistoryEntry",
                "Issue",
                "IssueCounts",
                "Label",
                "Milestone",
                "Priority",
                "Project",
                "RelatedIssue",
                "SkippedRecords",
                "Status",
                "SyncConflict",
                "SyncStatus",
            ]
        );
    }
}

/// The subcommands `lcl PATH --help` lists, without `help`.
fn subcommands(env: &Env, path: &[&str]) -> Vec<String> {
    let mut args = path.to_vec();
    args.push("--help");
    let help = env.lcl(&args).assert_ok().stdout.clone();
    help.lines()
        .skip_while(|l| *l != "Commands:")
        .skip(1)
        .take_while(|l| !l.is_empty())
        // A wrapped description continues on a deeper indent.
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_string)
        .collect()
}

/// Every command that runs something: the leaves under `lcl --help`, and
/// a command whose subcommand is optional (`lcl sync [COMMAND]`).
fn leaf_commands(env: &Env, path: &[&str], into: &mut BTreeSet<String>) {
    let subs = subcommands(env, path);
    let mut args = path.to_vec();
    args.push("--help");
    let optional = env
        .lcl(&args)
        .stdout
        .lines()
        .any(|l| l.starts_with("Usage:") && l.ends_with("[COMMAND]"));
    if subs.is_empty() || optional && !path.is_empty() {
        into.insert(path.join(" "));
    }
    for sub in subs {
        let mut deeper = path.to_vec();
        deeper.push(&sub);
        leaf_commands(env, &deeper, into);
    }
}

#[test]
fn every_command_has_a_section_and_every_section_a_command() {
    let mut commands = BTreeSet::new();
    leaf_commands(&Env::new(), &[], &mut commands);
    assert!(
        commands.contains("issue show"),
        "help not understood: {commands:?}"
    );
    let sections: BTreeSet<String> = note().commands.into_keys().collect();
    assert_eq!(
        commands, sections,
        "the commands under `lcl --help` (left) and the `lcl ...` sections of docs/json-contract.md (right) differ"
    );
}

#[test]
fn every_shape_has_a_test_and_every_test_a_shape() {
    let shaped: BTreeSet<String> = note()
        .commands
        .into_iter()
        .filter_map(|(command, shape)| shape.map(|_| command))
        .collect();
    let pinned: BTreeSet<String> = PINNED.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        shaped, pinned,
        "the sections with a shape (left) and the commands pinned! lists (right) differ"
    );
    // The one section without a shape, and why.
    let unshaped: Vec<String> = note()
        .commands
        .into_iter()
        .filter_map(|(command, shape)| shape.is_none().then_some(command))
        .collect();
    assert_eq!(unshaped, ["tui"]);
}

/// A failure under `--json`: nothing on stdout, and stderr ends with the one
/// line that starts with `{`, an `Error`. Returns its kind.
#[track_caller]
fn error_kind(out: &Out) -> String {
    assert_eq!(
        out.stdout, "",
        "a failure prints nothing on stdout\n{out:#?}"
    );
    let json_lines: Vec<&str> = out.stderr.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(json_lines.len(), 1, "one error line\n{out:#?}");
    assert_eq!(
        out.stderr.lines().last(),
        Some(json_lines[0]),
        "it is the last line\n{out:#?}"
    );
    let error: Value = serde_json::from_str(json_lines[0]).expect("the error line is JSON");
    pin_object("Error", std::slice::from_ref(&error));
    error["error"]["kind"].as_str().expect("a kind").to_string()
}

/// The exit status and the error kind of each kind of failure, and that
/// the status is the same without `--json`.
#[test]
fn each_kind_of_error_has_its_exit_status() {
    let rich = rich();
    let env = &rich.env;
    // DEMO-5, in the trash.
    j(env, &["issue", "create", "--title", "five"]);
    j(env, &["issue", "delete", "5"]);
    let missing_dir = env.cwd.path().join("no-such-dir").join("out.jsonl");
    let cases: &[(&[&str], i32, &str)] = &[
        // The command line is wrong: 2, usage.
        (&["issue", "show"], 2, "usage"),
        (&["issue", "create", "--title", " "], 2, "usage"),
        (&["issue", "list", "--blocked", "--unblocked"], 2, "usage"),
        (&["no-such-command"], 2, "usage"),
        // A named row doesn't exist: 1, not_found.
        (&["issue", "show", "99"], 1, "not_found"),
        (
            &["issue", "update", "1", "--add-label", "nope"],
            1,
            "not_found",
        ),
        (&["project", "show", "nope"], 1, "not_found"),
        (&["-w", "nope", "issue", "list"], 1, "not_found"),
        // What is stored refuses the change: 1, conflict.
        (&["label", "create", "BUG"], 1, "conflict"),
        (&["issue", "delete", "1"], 1, "conflict"),
        (&["project", "delete", "Parser"], 1, "conflict"),
        (&["issue", "relate", "1", "blocks", "3"], 1, "conflict"),
        (&["backup", ".lcl/data.sqlite"], 1, "conflict"),
        // An issue in the trash is there until restored: 1, conflict.
        (&["issue", "show", "5"], 1, "conflict"),
        (&["issue", "update", "5", "--title", "t"], 1, "conflict"),
        (&["issue", "restore", "1"], 1, "conflict"),
        // The same for a doc, a comment and a label: live ones aren't purged.
        (&["doc", "purge", &rich.notes], 1, "conflict"),
        (&["comment", "purge", &rich.comment], 1, "conflict"),
        (&["label", "purge", "bug"], 1, "conflict"),
        (&["label", "restore", "bug"], 1, "conflict"),
        (&["label", "purge"], 2, "usage"),
        // Only the trash is purged; one of an issue and --all is needed.
        (&["issue", "purge", "1"], 1, "conflict"),
        (&["issue", "purge"], 2, "usage"),
        (&["issue", "purge", "5", "--all"], 2, "usage"),
        // A file can't be written: 1, io.
        (
            &["export", "--output", missing_dir.to_str().unwrap()],
            1,
            "io",
        ),
        (&["backup", missing_dir.to_str().unwrap()], 1, "io"),
        // Sync, which this workspace doesn't: the address is checked
        // first, then the token (LCL_SYNC_TOKEN, unset here).
        (&["sync"], 1, "conflict"),
        (&["sync", "start", "http://sync.example"], 2, "usage"),
        (&["sync", "start", "https://sync.example"], 1, "auth"),
    ];
    for (args, code, kind) in cases {
        let mut argv = args.to_vec();
        argv.push("--json");
        let out = env.lcl(&argv);
        assert_eq!(out.code, *code, "exit of {args:?}\n{out:#?}");
        assert_eq!(error_kind(&out), *kind, "kind of {args:?}\n{out:#?}");

        let plain = env.lcl(args);
        assert_eq!(
            plain.code, *code,
            "exit of {args:?} without --json\n{plain:#?}"
        );
        assert_eq!(plain.stdout, "", "{plain:#?}");
        assert!(
            !plain.stderr.lines().any(|l| l.starts_with('{')),
            "no JSON without --json\n{plain:#?}"
        );
    }

    // A file from a newer lcl: 1, schema. Last, as it spoils the workspace.
    common::set_user_version(&env.db_path(), 9999);
    let out = env.lcl(&["issue", "list", "--json"]);
    assert_eq!(out.code, 1, "{out:#?}");
    assert_eq!(error_kind(&out), "schema");
}

/// `lcl export` without `--output` prints the export, not one JSON value:
/// one JSON object per line, the first naming the format.
#[test]
fn export_to_stdout_is_the_export_itself() {
    let out = rich().env.lcl(&["export", "--json"]);
    let lines: Vec<Value> = out
        .assert_ok()
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect();
    assert!(lines.len() > 1);
    assert_eq!(lines[0]["format"], "lcl-export/1");
    assert!(lines.iter().all(|l| l["type"].is_string()));
}

/// The checker itself: it must refuse what it exists to catch.
mod checker {
    use super::*;
    use serde_json::json;

    fn errors(shape: Value, samples: &[Value]) -> String {
        let objects = BTreeMap::from([("Thing".to_string(), json!({"id": "string"}))]);
        let caught = std::panic::catch_unwind(|| pin_shape("test", &shape, &objects, samples));
        match caught {
            Ok(()) => String::new(),
            Err(e) => e.downcast_ref::<String>().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn it_accepts_a_value_with_its_shape() {
        let shape = json!({"a": "integer", "b": "string|null", "c": ["Thing"], "d": "'x'|'y'"});
        let samples = [
            json!({"a": 1, "b": null, "c": [], "d": "x"}),
            json!({"a": 2, "b": "s", "c": [{"id": "i"}], "d": "y"}),
        ];
        assert_eq!(errors(shape, &samples), "");
    }

    #[test]
    fn it_refuses_a_new_key_a_missing_key_and_a_changed_type() {
        let shape = json!({"a": "integer", "b": "string"});
        assert!(
            errors(shape.clone(), &[json!({"a": 1, "b": "s", "c": 1})])
                .contains("\"c\" is not in the shape")
        );
        assert!(errors(shape.clone(), &[json!({"a": 1})]).contains("\"b\" is missing"));
        assert!(
            errors(shape.clone(), &[json!({"a": "1", "b": "s"})]).contains("$.a: expected integer")
        );
        assert!(errors(shape, &[json!({"a": 1.5, "b": "s"})]).contains("$.a: expected integer"));
    }

    #[test]
    fn it_refuses_a_value_outside_the_listed_ones_and_inside_named_objects() {
        assert!(errors(json!("'x'|'y'"), &[json!("z")]).contains("expected 'x'|'y'"));
        assert!(
            errors(json!(["Thing"]), &[json!([{"id": 1}])]).contains("$[].id: expected string")
        );
        assert!(
            errors(json!({"...": "Thing", "n": "integer"}), &[json!({"n": 1})])
                .contains("\"id\" is missing")
        );
    }

    #[test]
    fn it_refuses_a_claim_no_sample_shows() {
        assert!(
            errors(json!({"b": "string|null"}), &[json!({"b": "s"})])
                .contains("no sample where $.b is null")
        );
        assert!(
            errors(json!({"b": "string|null"}), &[json!({"b": null})])
                .contains("no sample where $.b has a value")
        );
        assert!(
            errors(json!({"c": ["string"]}), &[json!({"c": []})])
                .contains("no sample where $.c has an element")
        );
    }
}
