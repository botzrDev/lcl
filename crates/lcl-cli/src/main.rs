use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{CommandFactory, Parser, Subcommand};
use lcl_core::db::{IssueFilter, Store};
use lcl_core::error::LclError;
use lcl_core::models::*;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "lcl", version, about = "Local Linear-like tracker (SQLite per project)")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, env = "LCL_PROJECT")]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init { name: Option<String> },
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    Issue {
        #[command(subcommand)]
        cmd: IssueCmd,
    },
    Doc {
        #[command(subcommand)]
        cmd: DocCmd,
    },
    Label {
        #[command(subcommand)]
        cmd: LabelCmd,
    },
    Cycle {
        #[command(subcommand)]
        cmd: CycleCmd,
    },
    Search { query: String },
    Tui,
}

#[derive(Subcommand)]
enum ProjectCmd {
    Create { name: String, #[arg(long)] key: Option<String>, #[arg(long)] global: bool },
    List,
    Show,
}

#[derive(Subcommand)]
enum IssueCmd {
    // Status, Priority and IssueRef parse through their FromStr impls, and
    // dates, estimates and colors through the parsers below. A bad value is a
    // clap error: exit 2, before any database is opened. Issue arguments allow
    // a leading '-' so that "-3" reaches the IssueRef parser and is rejected
    // there, rather than being taken for a flag.
    Create {
        #[arg(long)] title: String,
        #[arg(long, default_value = "")] description: String,
        #[arg(long, default_value = "Backlog")] status: Status,
        #[arg(long, default_value = "0")] priority: Priority,
        #[arg(long, value_parser = parse_assignee)] assignee: Option<String>,
        #[arg(long, allow_negative_numbers = true)] parent: Option<IssueRef>,
        /// A label name (ignoring case); repeat for more.
        #[arg(long, value_parser = parse_label_name)] label: Vec<String>,
        #[arg(long, allow_negative_numbers = true, value_parser = clap::value_parser!(i64).range(0..))] estimate: Option<i64>,
        #[arg(long, value_parser = parse_date)] due: Option<NaiveDate>,
    },
    List {
        #[arg(long)] status: Option<Status>,
        #[arg(long)] assignee: Option<String>,
        /// Only the direct sub-issues of this issue.
        #[arg(long, allow_negative_numbers = true)] parent: Option<IssueRef>,
        #[arg(long, default_value_t = 50)] limit: i64,
    },
    Show { #[arg(allow_negative_numbers = true)] id: IssueRef },
    Update {
        #[arg(allow_negative_numbers = true)] id: IssueRef,
        #[arg(long)] title: Option<String>,
        #[arg(long)] description: Option<String>,
        #[arg(long)] status: Option<Status>,
        #[arg(long)] priority: Option<Priority>,
        #[arg(long, value_parser = parse_assignee, conflicts_with = "no_assignee")] assignee: Option<String>,
        /// Unassign the issue.
        #[arg(long)] no_assignee: bool,
        #[arg(long, allow_negative_numbers = true, value_parser = clap::value_parser!(i64).range(0..), conflicts_with = "no_estimate")] estimate: Option<i64>,
        /// Clear the estimate.
        #[arg(long)] no_estimate: bool,
        #[arg(long, value_parser = parse_date, conflicts_with = "no_due")] due: Option<NaiveDate>,
        /// Clear the due date.
        #[arg(long)] no_due: bool,
        /// Replace all labels with these (names, ignoring case); repeat for more.
        #[arg(long, value_parser = parse_label_name, conflicts_with_all = ["add_label", "remove_label", "clear_labels"])] label: Vec<String>,
        /// Add a label, keeping the others; repeatable.
        #[arg(long, value_parser = parse_label_name)] add_label: Vec<String>,
        /// Remove a label (a no-op if the issue doesn't have it); repeatable.
        #[arg(long, value_parser = parse_label_name)] remove_label: Vec<String>,
        /// Remove every label.
        #[arg(long, conflicts_with_all = ["add_label", "remove_label"])] clear_labels: bool,
        /// Move the issue under this one.
        #[arg(long, allow_negative_numbers = true, conflicts_with = "no_parent")] parent: Option<IssueRef>,
        /// Make the issue top-level.
        #[arg(long)] no_parent: bool,
    },
    Delete { #[arg(allow_negative_numbers = true)] id: IssueRef },
    Comment { #[arg(allow_negative_numbers = true)] id: IssueRef, body: String, #[arg(long)] author: Option<String> },
    Comments { #[arg(allow_negative_numbers = true)] id: IssueRef },
}

#[derive(Subcommand)]
enum DocCmd {
    Create { #[arg(long)] title: String, #[arg(long, default_value = "")] content: String, #[arg(long)] parent: Option<String> },
    List,
    Show { id: String },
    Update { id: String, #[arg(long)] title: Option<String>, #[arg(long)] content: Option<String> },
    Delete { id: String },
}

#[derive(Subcommand)]
enum LabelCmd {
    Create { #[arg(value_parser = parse_label_name)] name: String, #[arg(long, default_value = "#888888", value_parser = parse_color)] color: String },
    List,
    /// Delete a label by name (ignoring case) or id, removing it from every issue.
    Delete { #[arg(value_parser = parse_label_name)] name_or_id: String },
}

#[derive(Subcommand)]
enum CycleCmd {
    Create { name: String, #[arg(long, value_parser = parse_date)] start: Option<NaiveDate>, #[arg(long, value_parser = parse_date)] end: Option<NaiveDate> },
    List,
}

/// A real calendar date written exactly as YYYY-MM-DD: 2026-02-30 and
/// 2026-2-3 are both rejected.
fn parse_date(s: &str) -> std::result::Result<NaiveDate, String> {
    let b = s.as_bytes();
    let shaped = b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    shaped
        .then(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .flatten()
        .ok_or_else(|| format!("invalid date \"{s}\"; expected a real calendar date as YYYY-MM-DD"))
}

/// `#rgb` or `#rrggbb`.
fn parse_color(s: &str) -> std::result::Result<String, String> {
    let hex = s.strip_prefix('#').unwrap_or("");
    if s.starts_with('#') && matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(s.to_string())
    } else {
        Err(format!("invalid color \"{s}\"; expected #rgb or #rrggbb"))
    }
}

/// A label name, trimmed; blank is refused.
fn parse_label_name(s: &str) -> std::result::Result<String, String> {
    let s = s.trim();
    if s.is_empty() { Err("a label name can't be empty".into()) } else { Ok(s.to_string()) }
}

/// An assignee, trimmed; blank is refused. Unassigning is its own flag.
fn parse_assignee(s: &str) -> std::result::Result<String, String> {
    let s = s.trim();
    if s.is_empty() { Err("an assignee can't be empty; to unassign, use `issue update --no-assignee`".into()) } else { Ok(s.to_string()) }
}

/// Checks that span more than one argument. clap's value parsers have
/// already checked each value on its own; this exits 2 the same way.
fn validate(cli: &Cli) {
    let fail = |msg: String| -> ! { Cli::command().error(clap::error::ErrorKind::ValueValidation, msg).exit() };
    if let Commands::Cycle { cmd: CycleCmd::Create { start: Some(start), end: Some(end), .. } } = &cli.cmd
        && end < start
    {
        fail(format!("cycle end {end} is before its start {start}"));
    }
    // Names match ignoring ASCII case, as label lookups do.
    if let Commands::Issue { cmd: IssueCmd::Update { add_label, remove_label, .. } } = &cli.cmd
        && let Some(both) = add_label.iter().find(|a| remove_label.iter().any(|r| r.eq_ignore_ascii_case(a)))
    {
        fail(format!("label {both:?} is in both --add-label and --remove-label"));
    }
}

fn global_dir() -> PathBuf { dirs_home().join(".lcl") }
fn dirs_home() -> PathBuf { std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp")) }
fn projects_dir() -> PathBuf { global_dir().join("projects") }
fn index_db_path() -> PathBuf { global_dir().join("index.sqlite") }

fn open_index() -> Result<Store> {
    let p = index_db_path();
    let s = Store::open(&p)?;
    // ensure projects table exists (index-specific)
    s.conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (name TEXT PRIMARY KEY, slug TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL);"
    )?;
    Ok(s)
}

fn slugify(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect::<String>().trim_matches('-').to_string()
}

fn find_local_db() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?;
    loop {
        let cand = cur.join(".lcl").join("data.sqlite");
        if cand.exists() { return Some(cand); }
        if !cur.pop() { break; }
    }
    None
}

fn resolve_store(cli_project: &Option<String>) -> Result<Store> {
    if let Some(name) = cli_project {
        let slug = slugify(name);
        let p = projects_dir().join(format!("{}.sqlite", slug));
        if !p.exists() {
            return Err(anyhow::anyhow!("project '{}' not found at {}", name, p.display()));
        }
        return Store::open(&p).context("open project db");
    }
    if let Some(p) = find_local_db() {
        return Store::open(&p).context("open local db");
    }
    // global fallback: if index has projects, try default? require explicit
    Err(anyhow::anyhow!("no project found. Run `lcl init` or pass --project <name>"))
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// Exit 2 for a usage error, as clap does for a bad argument; 1 for anything
/// else. Errors print as `fn main() -> Result` would print them.
fn main() -> ExitCode {
    let cli = Cli::parse();
    validate(&cli);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            let usage = e.downcast_ref::<LclError>().is_some_and(LclError::is_usage);
            ExitCode::from(if usage { 2 } else { 1 })
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Commands::Init { name } => {
            let pname = name.unwrap_or_else(|| {
                std::env::current_dir().ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string())).unwrap_or_else(|| "project".into())
            });
            let key = pname.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase();
            let key = if key.is_empty() { "PRJ".to_string() } else { key.chars().take(4).collect() };
            let dir = std::env::current_dir()?.join(".lcl");
            std::fs::create_dir_all(&dir)?;
            let dbpath = dir.join("data.sqlite");
            let mut store = Store::open(&dbpath)?;
            let created_at = now().to_string();
            store.set_meta(&[("name", pname.as_str()), ("key", key.as_str()), ("description", ""), ("created_at", created_at.as_str())])?;
            // register in global index
            let idx = open_index()?;
            idx.conn.execute("INSERT OR REPLACE INTO projects(name,slug,path,created_at) VALUES(?1,?2,?3,?4)", rusqlite::params![pname, slugify(&pname), dbpath.to_string_lossy(), now()])?;
            if cli.json { print_json(&serde_json::json!({"path": dbpath, "name": pname, "key": key})); }
            else { println!("Initialized project '{}' at {}", pname, dbpath.display()); }
        }
        Commands::Project { cmd } => match cmd {
            ProjectCmd::Create { name, key, global } => {
                let slug = slugify(&name);
                let k = key.unwrap_or_else(|| name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase().chars().take(4).collect());
                let k = if k.is_empty() { "PRJ".to_string() } else { k };
                let path = if global {
                    std::fs::create_dir_all(projects_dir())?;
                    projects_dir().join(format!("{}.sqlite", slug))
                } else {
                    let dir = std::env::current_dir()?.join(".lcl");
                    std::fs::create_dir_all(&dir)?;
                    dir.join(format!("{}.sqlite", slug))
                };
                let mut store = Store::open(&path)?;
                let created_at = now().to_string();
                store.set_meta(&[("name", name.as_str()), ("key", k.as_str()), ("description", ""), ("created_at", created_at.as_str())])?;
                let idx = open_index()?;
                idx.conn.execute("INSERT OR REPLACE INTO projects(name,slug,path,created_at) VALUES(?1,?2,?3,?4)", rusqlite::params![name, slug, path.to_string_lossy(), now()])?;
                if cli.json { print_json(&serde_json::json!({"path": path, "name": name, "key": k})); }
                else { println!("Created project '{}' at {}", name, path.display()); }
            }
            ProjectCmd::List => {
                let idx = open_index()?;
                let mut stmt = idx.conn.prepare("SELECT name,slug,path,created_at FROM projects ORDER BY name")?;
                let rows = stmt.query_map([], |r| Ok(serde_json::json!({"name": r.get::<_,String>(0)?, "slug": r.get::<_,String>(1)?, "path": r.get::<_,String>(2)?, "created_at": r.get::<_,i64>(3)?})))?;
                let mut v = vec![];
                for r in rows { v.push(r?); }
                if cli.json { print_json(&v); }
                else { for e in &v { println!("{} ({})", e["name"], e["slug"]); } }
            }
            ProjectCmd::Show => {
                let store = resolve_store(&cli.project)?;
                let name = store.get_meta("name")?.unwrap_or_default();
                let key = store.get_meta("key")?.unwrap_or_default();
                let desc = store.get_meta("description")?.unwrap_or_default();
                let created = store.get_meta("created_at")?.unwrap_or_default();
                let schema_version = store.schema_version()?;
                let v = serde_json::json!({"name": name, "key": key, "description": desc, "created_at": created, "schema_version": schema_version});
                if cli.json { print_json(&v); } else { println!("{} [{}] - {}", name, key, desc); }
            }
        },
        Commands::Issue { cmd } => {
            let mut store = resolve_store(&cli.project)?;
            match cmd {
                IssueCmd::Create { title, description, status, priority, assignee, parent, label, estimate, due } => {
                    let parent_id = parent.map(|p| store.resolve(&p)).transpose()?.map(|p| p.id);
                    let issue = store.create_issue(Issue {
                        id: String::new(), number: 0, identifier: String::new(), title, description, status, priority,
                        assignee, parent_id, parent_identifier: None, estimate, due_date: due.map(|d| d.to_string()), labels: label,
                        created_at: 0, updated_at: 0, completed_at: None, started_at: None, canceled_at: None,
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Created {}", issue.identifier); }
                }
                IssueCmd::List { status, assignee, parent, limit } => {
                    let parent_id = parent.map(|p| store.resolve(&p)).transpose()?.map(|p| p.id);
                    let issues = store.list_issues(&IssueFilter { status, assignee, parent_id, limit })?;
                    if cli.json { print_json(&issues); }
                    else {
                        for i in &issues { println!("{}\t[{}]\t{}\t{}", i.identifier, i.status, i.priority.label(), i.title); }
                    }
                }
                IssueCmd::Show { id } => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(&issue.id)?;
                    let children = store.children(&issue.id)?;
                    if cli.json {
                        let children: Vec<&str> = children.iter().map(|c| c.identifier.as_str()).collect();
                        print_json(&serde_json::json!({"issue": issue, "comments": comments, "children": children}));
                    }
                    else {
                        println!("{} {}", issue.identifier, issue.title);
                        println!("Status: {}  Priority: {}  Assignee: {}", issue.status, issue.priority.label(), issue.assignee.as_deref().unwrap_or("Unassigned"));
                        if !issue.labels.is_empty() { println!("Labels: {}", issue.labels.join(", ")); }
                        if let Some(p) = &issue.parent_identifier { println!("Parent: {p}"); }
                        if !issue.description.is_empty() { println!("\n{}", issue.description); }
                        if !children.is_empty() {
                            println!("\nSub-issues:");
                            for c in &children { println!("  {} {}", c.identifier, c.title); }
                        }
                        if !comments.is_empty() {
                            println!("\nComments:");
                            for c in comments { println!("  - {}: {}", c.author.as_deref().unwrap_or("anon"), c.body); }
                        }
                    }
                }
                IssueCmd::Update {
                    id, title, description, status, priority, assignee, no_assignee, estimate, no_estimate, due, no_due,
                    label, add_label, remove_label, clear_labels, parent, no_parent,
                } => {
                    // An unknown name is an error even when removing; a known
                    // label the issue doesn't have is a no-op.
                    store.require_labels(&remove_label)?;
                    // Some(new parent UUID or None), or None to leave it alone.
                    let new_parent = match (parent, no_parent) {
                        (_, true) => Some(None),
                        (Some(p), false) => Some(Some(store.resolve(&p)?.id)),
                        (None, false) => None,
                    };
                    let issue = store.update_issue(&id, |i| {
                        if let Some(t) = title { i.title = t; }
                        if let Some(d) = description { i.description = d; }
                        if let Some(st) = status { i.status = st; }
                        if let Some(p) = priority { i.priority = p; }
                        if assignee.is_some() { i.assignee = assignee; }
                        if no_assignee { i.assignee = None; }
                        if let Some(e) = estimate { i.estimate = Some(e); }
                        if no_estimate { i.estimate = None; }
                        if let Some(d) = due { i.due_date = Some(d.to_string()); }
                        if no_due { i.due_date = None; }
                        // Label edits apply to the set as read inside the write
                        // transaction, so concurrent edits all land.
                        if !label.is_empty() { i.labels = label; }
                        if clear_labels { i.labels.clear(); }
                        i.labels.retain(|n| !remove_label.iter().any(|r| r.eq_ignore_ascii_case(n)));
                        i.labels.extend(add_label);
                        if let Some(p) = new_parent { i.parent_id = p; }
                    })?;
                    if cli.json { print_json(&issue); } else { println!("Updated {}", issue.identifier); }
                }
                IssueCmd::Delete { id } => {
                    let issue = store.delete_issue(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": issue.identifier, "id": issue.id})); } else { println!("Deleted {}", issue.identifier); }
                }
                IssueCmd::Comment { id, body, author } => {
                    let c = store.add_comment(&id, body, author)?;
                    if cli.json { print_json(&c); } else { println!("Comment added"); }
                }
                IssueCmd::Comments { id } => {
                    let issue = store.resolve(&id)?;
                    let comments = store.list_comments(&issue.id)?;
                    if cli.json { print_json(&comments); }
                    else { for c in comments { println!("[{}] {}: {}", c.created_at, c.author.as_deref().unwrap_or("anon"), c.body); } }
                }
            }
        },
        Commands::Doc { cmd } => {
            let mut store = resolve_store(&cli.project)?;
            match cmd {
                DocCmd::Create { title, content, parent } => {
                    let d = store.create_doc(Doc { id: String::new(), title, content, parent_id: parent, created_at: 0, updated_at: 0 })?;
                    if cli.json { print_json(&d); } else { println!("Created doc {}", d.id); }
                }
                DocCmd::List => {
                    let docs = store.list_docs()?;
                    if cli.json { print_json(&docs); }
                    else { for d in docs { println!("{}\t{}", d.id, d.title); } }
                }
                DocCmd::Show { id } => {
                    let d = store.get_doc(&id)?.ok_or_else(|| anyhow::anyhow!("doc not found"))?;
                    if cli.json { print_json(&d); } else { println!("# {}\n\n{}", d.title, d.content); }
                }
                DocCmd::Update { id, title, content } => {
                    let d = store.update_doc(&id, title, content)?;
                    if cli.json { print_json(&d); } else { println!("Updated {}", id); }
                }
                DocCmd::Delete { id } => {
                    store.delete_doc(&id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": id})); } else { println!("Deleted {}", id); }
                }
            }
        },
        Commands::Label { cmd } => {
            let mut store = resolve_store(&cli.project)?;
            match cmd {
                LabelCmd::Create { name, color } => {
                    let l = Label { id: uuid::Uuid::new_v4().to_string(), name, color };
                    store.create_label(&l)?;
                    if cli.json { print_json(&l); } else { println!("Created label {}", l.name); }
                }
                LabelCmd::List => {
                    let ls = store.list_labels()?;
                    if cli.json { print_json(&ls); } else { for l in ls { println!("{} {} ({})", l.id, l.name, l.color); } }
                }
                LabelCmd::Delete { name_or_id } => {
                    let (label, issues) = store.delete_label(&name_or_id)?;
                    if cli.json { print_json(&serde_json::json!({"deleted": label.name, "id": label.id, "removed_from_issues": issues})); }
                    else { println!("Deleted label {} (removed from {} issue{})", label.name, issues, if issues == 1 { "" } else { "s" }); }
                }
            }
        },
        Commands::Cycle { cmd } => {
            let mut store = resolve_store(&cli.project)?;
            match cmd {
                CycleCmd::Create { name, start, end } => {
                    let c = store.create_cycle(Cycle { id: String::new(), name, starts_at: start.map(|d| d.to_string()), ends_at: end.map(|d| d.to_string()), created_at: 0 })?;
                    if cli.json { print_json(&c); } else { println!("Created cycle {}", c.id); }
                }
                CycleCmd::List => {
                    let cs = store.list_cycles()?;
                    if cli.json { print_json(&cs); }
                    else {
                        for c in cs {
                            let (start, end) = (c.starts_at.as_deref().unwrap_or("?"), c.ends_at.as_deref().unwrap_or("?"));
                            println!("{}\t{}\t{start}..{end}", c.id, c.name);
                        }
                    }
                }
            }
        },
        Commands::Search { query } => {
            let store = resolve_store(&cli.project)?;
            let issues = store.search_issues(&query)?;
            let docs = store.search_docs(&query)?;
            if cli.json { print_json(&serde_json::json!({"issues": issues, "docs": docs})); }
            else {
                println!("Issues:");
                for i in &issues { println!("  {} {}", i.identifier, i.title); }
                println!("Docs:");
                for d in &docs { println!("  {} {}", d.id, d.title); }
            }
        },
        Commands::Tui => {
            // launch tui crate
            lcl_tui::run(cli.project.clone())?;
        }
    }
    Ok(())
}

fn now() -> i64 { lcl_core::models::now() }

// helper to avoid import name clash
